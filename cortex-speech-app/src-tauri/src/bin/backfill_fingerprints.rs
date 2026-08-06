//! Backfill `audio_fingerprint` for recordings imported before migration v50, and REPORT any
//! duplicate content already sitting in the library.
//!
//! Why this is a separate pass rather than part of the migration: computing a fingerprint requires
//! DECODING the audio. A schema migration may not do that, and inventing a value would be worse than
//! the honest NULL v50 leaves behind. Until this runs, pre-v50 rows simply do not participate in
//! duplicate detection — exactly as protected as they were before, uncovered rather than regressed.
//!
//! IT NEVER DELETES ANYTHING. Not a row, not a file, not in `--apply` mode. A duplicate found here is
//! a fact about a library the owner already curated — possibly two legitimate copies, possibly a real
//! double-import, and only the owner knows which. The tool's job is to make it visible; deciding is
//! theirs. `--apply` writes fingerprints and nothing else.
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

    let db_path = data_dir.join("cortex-speech.db");
    println!("db    : {}", db_path.display());
    println!("mode  : {}\n", if apply { "APPLY (writes fingerprints only)" } else { "DRY RUN (writes nothing)" });

    let db = Database::open(db_path.to_string_lossy().as_ref()).map_err(|e| format!("open db: {e}"))?;

    // Already-stored fingerprints participate in collision detection from the start, so a backfilled
    // recording that matches one imported after v50 is reported too — not just backfill-vs-backfill.
    let mut seen: BTreeMap<u64, String> = BTreeMap::new();
    for (fp, path) in db.load_audio_fingerprints().map_err(|e| e.to_string())? {
        seen.insert(fp, path);
    }
    println!("already fingerprinted : {} recording(s)", seen.len());

    // One entry per RECORDING. All VAD chunks of a source share its audio_path and its fingerprint, so
    // decoding once per path instead of once per row is the difference between 144 decodes and 1.
    let pending: Vec<String> = {
        let conn = db.connection();
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT audio_path FROM speech_segments
                 WHERE audio_fingerprint IS NULL AND TRIM(COALESCE(audio_path,'')) <> ''
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
        let fp = AudioFingerprint::fingerprint(&pcm, sample_rate);
        if fp == 0 {
            // Digital silence or a sub-16ms clip. `register` refuses to store this as a content key
            // because every later silent window would collide with it; so does this.
            degenerate += 1;
            continue;
        }
        if let Some(other) = seen.get(&fp) {
            if other != path {
                duplicates.push((other.clone(), path.clone()));
            }
        } else {
            seen.insert(fp, path.clone());
        }
        if apply {
            match db.set_audio_fingerprint(path, fp) {
                Ok(rows) => done += rows.min(1),
                Err(e) => eprintln!("  write failed: {path} ({e})"),
            }
        } else {
            done += 1;
        }
    }

    println!("\n─── summary ───");
    println!("fingerprinted : {done}{}", if apply { "" } else { " (dry run — nothing written)" });
    println!("source missing: {missing}");
    println!("undecodable   : {undecodable}");
    println!("silent/too short (no content key): {degenerate}");

    if duplicates.is_empty() {
        println!("\nNo duplicate audio content found.");
    } else {
        println!("\n⚠ {} recording(s) share content with another recording:", duplicates.len());
        for (first, dup) in &duplicates {
            println!("    {dup}\n      matches {first}");
        }
        println!(
            "\nNOTHING WAS DELETED, and this tool never will. These may be two legitimate copies or a\n\
             real double-import — only you can tell. Review them in the app and mark bad or delete\n\
             there if you decide they are redundant."
        );
    }
    if !apply {
        println!("\nRe-run with --apply to write the fingerprints.");
    }
    Ok(())
}
