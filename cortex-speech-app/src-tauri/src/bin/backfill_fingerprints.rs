//! Backfill recording identity for recordings imported before migration v50/v51, and REPORT any
//! duplicate content already sitting in the library.
//!
//! Two columns are filled, and the distinction matters (external review 2026-08-06, P1.1):
//!   - `audio_fingerprint`  (v50) — a 64-bit spectral bucket key. A CANDIDATE signal only.
//!   - `audio_content_hash` (v51) — blake3 over canonical decoded PCM. The DEFINITIVE key.
//!
//! A recording with a bucket but no content hash — anything imported between v50 and v51 — can never
//! prove a duplicate, so it never rejects an import. That is deliberate: the spectral value was
//! demoted precisely because a collision on it used to REFUSE legitimate audio. This pass is what
//! closes that coverage gap, by decoding the audio a schema migration is not allowed to touch.
//!
//! IT NEVER DELETES ANYTHING. Not a row, not a file, not in `--apply` mode. A duplicate found here is
//! a fact about a library the owner already curated — possibly two legitimate copies, possibly a real
//! double-import, and only the owner knows which. The tool's job is to make it visible; deciding is
//! theirs. `--apply` writes identities and nothing else.
//!
//! Dry run by default, mirroring realign_segments.rs:
//!   cargo run --release --bin backfill_fingerprints
//!   cargo run --release --bin backfill_fingerprints -- --apply

use cortex_speech_app_lib::audio;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let apply = args.iter().any(|a| a == "--apply");
    let data_dir: PathBuf = args
        .iter()
        .position(|a| a == "--data-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("cortex-speech"));

    // The apply pass decodes after its initial database read, then writes through the same external
    // connection. Keep the complete pass on one generation by excluding the desktop process.
    let _instance_lock = if apply {
        Some(cortex_speech_app_lib::flock::InstanceLock::try_lock(&data_dir).map_err(|error| {
            format!("Cannot apply fingerprint backfill while Cortex is running: {error}. Stop review and close the app first.")
        })?)
    } else {
        None
    };

    let db_path = data_dir.join("cortex-speech.db");
    println!("db    : {}", db_path.display());
    println!("mode  : {}\n", if apply { "APPLY (writes identities only)" } else { "DRY RUN (writes nothing)" });

    let db = Database::open(db_path.to_string_lossy().as_ref()).map_err(|e| format!("open db: {e}"))?;

    // Keyed on the CONTENT hash, not the spectral bucket: two recordings are the same recording when
    // their audio is byte-identical, and never merely because their loudness envelopes collided. A
    // stored row with no content hash yet cannot key anything, so it is counted and then backfilled
    // below like any other pending row.
    let stored = db.load_audio_identities().map_err(|e| e.to_string())?;
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut legacy_unhashed = 0usize;
    for row in &stored {
        match row.content.as_deref() {
            Some(hash) => {
                seen.insert(hash.to_string(), row.audio_path.clone());
            }
            None => legacy_unhashed += 1,
        }
    }
    println!("already hashed        : {} recording(s)", seen.len());
    if legacy_unhashed > 0 {
        println!("v50-era (bucket only) : {legacy_unhashed} recording(s) — cannot prove a duplicate until hashed");
    }

    // One entry per RECORDING. All VAD chunks of a source share its audio_path and its identity, so
    // decoding once per path instead of once per row is the difference between 144 decodes and 1.
    //
    // A row is pending when EITHER column is missing: a v50-era row already has its bucket but is still
    // unable to reject anything until its content hash exists.
    let pending: Vec<String> = {
        let conn = db.connection();
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT audio_path FROM speech_segments
                 WHERE (audio_fingerprint IS NULL OR audio_content_hash IS NULL)
                   AND TRIM(COALESCE(audio_path,'')) <> ''
                 ORDER BY audio_path",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    println!("to backfill           : {} recording(s)\n", pending.len());
    if pending.is_empty() {
        println!("Nothing to do.");
        return Ok(());
    }

    let (mut done, mut missing, mut undecodable, mut degenerate) = (0usize, 0usize, 0usize, 0usize);
    let mut duplicates: Vec<(String, String)> = Vec::new();

    for path in &pending {
        if !std::path::Path::new(path).exists() {
            // A relinked or moved source. Not an error worth stopping for, but it must be COUNTED —
            // silently skipping would make the summary claim coverage this run did not achieve.
            missing += 1;
            continue;
        }
        let (sample_rate, pcm) = match audio::decode_to_pcm(path) {
            Ok(v) => v,
            Err(e) => {
                undecodable += 1;
                eprintln!("  decode failed: {path} ({e})");
                continue;
            }
        };
        let identity = AudioFingerprint::identify(&pcm, sample_rate);
        if identity.spectral == 0 {
            // Digital silence or a sub-16ms clip. `register` refuses to store this bucket because every
            // later silent window would land in it; so does this.
            degenerate += 1;
            continue;
        }
        match seen.get(&identity.content) {
            Some(other) if other != path => duplicates.push((other.clone(), path.clone())),
            Some(_) => {}
            None => {
                seen.insert(identity.content.clone(), path.clone());
            }
        }
        if apply {
            match db.set_audio_identity(path, &identity) {
                Ok(rows) => done += rows.min(1),
                Err(e) => eprintln!("  write failed: {path} ({e})"),
            }
        } else {
            done += 1;
        }
    }

    println!("\n─── summary ───");
    println!("identified    : {done}{}", if apply { "" } else { " (dry run — nothing written)" });
    println!("source missing: {missing}");
    println!("undecodable   : {undecodable}");
    println!("silent/too short (no content key): {degenerate}");

    if duplicates.is_empty() {
        println!("\nNo duplicate audio content found.");
    } else {
        println!("\n⚠ {} recording(s) are byte-identical to another recording:", duplicates.len());
        for (first, dup) in &duplicates {
            println!("    {dup}\n      matches {first}");
        }
        println!(
            "\nNOTHING WAS DELETED, and this tool never will. These are cryptographically identical —\n\
             not a spectral near-match — but they may still be two legitimate copies rather than a\n\
             double-import. Only you can tell. Review them in the app and mark bad or delete there if\n\
             you decide they are redundant."
        );
    }
    if !apply {
        println!("\nRe-run with --apply to write the identities.");
    }
    Ok(())
}
