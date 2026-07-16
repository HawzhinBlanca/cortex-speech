//! Mid-export kill drill writer — driven by `scripts/export_kill_drill.py`.
//!
//! Seeds a DISPOSABLE profile (CORTEX_APP_DATA_DIR) with real segments once, then loops the REAL
//! production export path (`export::export_dataset`, JSON) to numbered files as fast as possible,
//! journaling each path only AFTER the export returned (single flushed write, the durability_writer
//! protocol). The drill hard-kills this process mid-export and asserts the atomic-write design holds:
//! every journaled export is complete and parseable, and NO final `.json` file — journaled or not —
//! is ever torn (a kill may leave `.tmp` staging debris; manifests already exclude it by design).

use cortex_speech_app_lib::db::{Database, SpeechSegment};
use cortex_speech_app_lib::export;
use cortex_speech_app_lib::flock::InstanceLock;
use cortex_speech_app_lib::settings::ExportFormat;
use std::io::Write;
use std::path::PathBuf;

const SEED_ROWS: usize = 400;

fn main() -> Result<(), String> {
    let dir = std::env::var_os("CORTEX_APP_DATA_DIR")
        .map(PathBuf::from)
        .ok_or("CORTEX_APP_DATA_DIR must point at the DISPOSABLE drill profile (never the live one)")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let _lock = InstanceLock::try_lock(&dir)?;
    let db = Database::open_with_retry(dir.join("cortex-speech.db").to_string_lossy().as_ref())
        .map_err(|e| e.to_string())?;
    db.initialize().map_err(|e| e.to_string())?;

    // Seed once: enough rows that a JSON export takes real time (a kill can land mid-write).
    let existing = db.get_segments(None).map_err(|e| e.to_string())?.len();
    for i in existing..SEED_ROWS {
        db.insert_segment(&SpeechSegment {
            id: format!("exp-{i:05}"),
            audio_path: "drill://synthetic.wav".into(),
            raw_transcript: format!("ڕیزبەندی {i} — ").repeat(40), // ~1.5 KB per row
            duration_ms: 1000,
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
    }

    let out_dir = dir.join("exports");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let mut journal = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("export_journal.txt"))
        .map_err(|e| e.to_string())?;

    // Resume above any prior exports so paths never collide across restarts.
    let start = std::fs::read_dir(&out_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_prefix("export_")
                .and_then(|s| s.strip_suffix(".json"))
                .and_then(|n| n.parse::<u64>().ok())
        })
        .max()
        .map_or(0, |m| m + 1);

    for seq in start.. {
        let path = out_dir.join(format!("export_{seq:06}.json"));
        export::export_dataset(&db, &path, &ExportFormat::Json).map_err(|e| e.to_string())?;
        journal.write_all(format!("{}\n", path.to_string_lossy()).as_bytes()).map_err(|e| e.to_string())?;
        journal.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}
