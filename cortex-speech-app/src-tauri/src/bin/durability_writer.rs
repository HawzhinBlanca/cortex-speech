//! Kill/restart durability drill writer — driven by `scripts/durability_drill.py`.
//!
//! Opens the REAL production Database (`open_with_retry`: the boot path with WAL recovery + the
//! integrity gates) at `CORTEX_APP_DATA_DIR` (a DISPOSABLE drill profile — never the live one), then
//! inserts sequentially-numbered segments through the production `insert_segment` path as fast as
//! possible. Each id is appended to `journal.txt` and FLUSHED only after the DB commit returned — the
//! journal is therefore the exact set of writes the DB must never lose. The drill script hard-kills
//! this process at random points mid-write and asserts after every kill: `journal ⊆ db` (zero lost
//! committed edits) and no id twice (zero duplicates). Taking the same `InstanceLock` as the app and
//! batch tools also proves a hard-killed process never leaves a stale lock that blocks the next start.

use cortex_speech_app_lib::db::{Database, SpeechSegment};
use cortex_speech_app_lib::flock::InstanceLock;
use std::io::Write;
use std::path::PathBuf;

fn main() -> Result<(), String> {
    let dir = std::env::var_os("CORTEX_APP_DATA_DIR")
        .map(PathBuf::from)
        .ok_or("CORTEX_APP_DATA_DIR must point at the DISPOSABLE drill profile (never the live one)")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let _lock = InstanceLock::try_lock(&dir)?;
    let db_path = dir.join("cortex-speech.db");
    // The exact app boot sequence (lib.rs setup): open_with_retry (WAL recovery + integrity gates)
    // then initialize() (schema + FTS + migrations) — so every drill restart exercises the real boot.
    let db = Database::open_with_retry(db_path.to_string_lossy().as_ref()).map_err(|e| e.to_string())?;
    db.initialize().map_err(|e| e.to_string())?;

    // Resume monotonically after a kill: continue above the highest drill id already committed. (The
    // journal alone is not enough — a kill can land between the commit and the journal append. NOTE:
    // insert_segment is an UPSERT (ON CONFLICT(id) DO UPDATE), so a reused id would silently overwrite
    // the earlier row, NOT error — this max-of-DB scan is the only thing preventing that, which is why
    // the drill also asserts the id space is CONTIGUOUS: a resume regression would surface as a hole.)
    let start = db
        .get_segments(None)
        .map_err(|e| e.to_string())?
        .iter()
        .filter_map(|s| s.id.strip_prefix("drill-").and_then(|n| n.parse::<u64>().ok()))
        .max()
        .map_or(0, |m| m + 1);

    let mut journal = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("journal.txt"))
        .map_err(|e| e.to_string())?;

    for seq in start.. {
        let id = format!("drill-{seq:08}");
        let seg = SpeechSegment {
            id: id.clone(),
            audio_path: "drill://synthetic.wav".into(),
            raw_transcript: format!("drill row {seq}"),
            duration_ms: 1000,
            ..Default::default()
        };
        db.insert_segment(&seg).map_err(|e| e.to_string())?;
        // Journal AFTER the commit returned. ONE write_all syscall for the whole line (writeln! issues
        // two writes — id then newline — and a kill in that gap would concatenate two ids into a torn
        // token); flush so a kill cannot lose the line in userspace buffers.
        journal.write_all(format!("{id}\n").as_bytes()).map_err(|e| e.to_string())?;
        journal.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}
