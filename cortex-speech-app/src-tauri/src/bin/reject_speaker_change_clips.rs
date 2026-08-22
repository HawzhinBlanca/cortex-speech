//! Inspect clips measured to hold a speaker change, with legacy pre-v60 bulk rejection support.
//!
//! WHY THIS EXISTS. `speaker_change_probe` measures, per clip, whether its two halves are different
//! people, calibrated against the owner's own blind listening pass (0/15 misclassified at 0.59).
//! Migration v47 stores that score and the phone shows a badge, but a flag is not a decision: 13 of
//! the 17 flagged clips are still pending, and the owner asked for them to be rejected rather than
//! listened to one at a time. This utility originally applied exactly that, once.
//!
//! WHY REJECT AND NOT DELETE. A rejected clip stays in the library — `is_human_rejected` keeps it out
//! of every export and every "verified" count, and the decision is reversible by re-reviewing. Nothing
//! here removes audio or transcripts.
//!
//! APPLY IS LEGACY ONLY. Schema v60 makes playback evidence plus an immutable review effect the
//! authority for human decisions. This offline classifier has no per-clip listening evidence, so v60+
//! refuses `--apply` before opening a writable database. The dry run remains useful for locating clips
//! that must be listened to and decided through Cortex Review. Pre-v60 apply keeps its historical
//! atomic finalizer solely for legacy maintenance.
//!
//! ONLY PENDING CLIPS. A flagged clip the owner already decided is left exactly as it is — overwriting
//! a human judgement with a bulk rule is precisely what this must not do.
//!
//! DRY RUN BY DEFAULT. Without `--apply` it lists what WOULD change and writes nothing.
//!
//! Usage: reject_speaker_change_clips [--apply] [--data-dir <dir>]

use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::diarization::SPEAKER_CHANGE_THRESHOLD;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

const EFFECT_BOUND_REVIEW_SCHEMA: i64 = 60;

fn read_schema_version_read_only(db_path: &Path) -> Result<i64, String> {
    // This connection is deliberately read-only. A v60+ refusal must happen before `Database::open`
    // enables WAL or any legacy finalizer can obtain a writable handle.
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|error| format!("open {} read-only for schema check: {error}", db_path.display()))?;
    let result =
        conn.query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |row| row.get::<_, i64>(0));
    match result {
        Ok(version) => Ok(version),
        Err(rusqlite::Error::SqliteFailure(_, Some(message))) if message.contains("no such table") => Ok(0),
        Err(error) => Err(format!("read schema version from {}: {error}", db_path.display())),
    }
}

fn apply_before_effect_bound_review<T>(
    schema_version: i64,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if schema_version >= EFFECT_BOUND_REVIEW_SCHEMA {
        return Err(format!(
            "Schema v{schema_version} refuses --apply before opening a writable database: bulk speaker-change rejection has no per-clip playback evidence and cannot create the immutable review effect required by schema v{EFFECT_BOUND_REVIEW_SCHEMA}+. Run without --apply to inspect candidates, then listen and reject each clip through Cortex Review so the evidence-backed review flow records its decision."
        ));
    }
    operation()
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let apply = args.iter().any(|a| a == "--apply");
    let data_dir: PathBuf = args
        .iter()
        .position(|a| a == "--data-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("cortex-speech"));

    run(&data_dir, apply)
}

fn run(data_dir: &Path, apply: bool) -> Result<(), String> {
    // Selection and finalization must observe one generation. `--apply` therefore shares the
    // desktop's exclusive instance lock instead of writing beside a live app/restore.
    let _instance_lock = if apply {
        Some(cortex_speech_app_lib::flock::InstanceLock::try_lock(&data_dir).map_err(|error| {
            format!(
                "Cannot apply bulk rejection while Cortex is running: {error}. Stop review and close the app first."
            )
        })?)
    } else {
        None
    };

    let db_path = data_dir.join("cortex-speech.db");
    if apply {
        let schema_version = read_schema_version_read_only(&db_path)?;
        return apply_before_effect_bound_review(schema_version, || process_database(&db_path, true));
    }
    process_database(&db_path, false)
}

fn process_database(db_path: &Path, apply: bool) -> Result<(), String> {
    let db_display = db_path.display().to_string();
    let db = Database::open(&db_display).map_err(|e| format!("open {db_display}: {e}"))?;
    println!("db    : {db_display}");
    println!("mode  : {}\n", if apply { "APPLY (writes)" } else { "DRY RUN (writes nothing)" });

    let segments = db.get_segments(None).map_err(|e| e.to_string())?;
    let flagged: Vec<_> = segments
        .iter()
        .filter(|s| s.speaker_change_score.is_some_and(|v| (v as f32) < SPEAKER_CHANGE_THRESHOLD))
        .collect();
    // PENDING only. `verified` is the app's own "this clip has left the review queue" flag, so an
    // already-decided clip is one a human has ruled on — bulk rules do not get to overrule that.
    let (pending, decided): (Vec<&&_>, Vec<&&_>) = flagged.iter().partition(|s| !s.verified);

    println!("flagged (score < {SPEAKER_CHANGE_THRESHOLD}): {}", flagged.len());
    println!("  already decided, LEFT ALONE          : {}", decided.len());
    println!("  pending, to reject                   : {}\n", pending.len());

    let mut done = 0usize;
    for s in &pending {
        let score = s.speaker_change_score.unwrap_or_default();
        println!("  {}  score {score:.4}  speaker {}", &s.id[..8], s.speaker_id.as_deref().unwrap_or("-"));
        if apply {
            // ONE commit (2026-08-20 hunt). The previous two-write version — decision first, then a
            // whole-row upsert to set `verified` — left a kill window in which a clip came back
            // correctly rejected AND still pending, exactly the half-written state the finalize
            // transaction exists to make unrepresentable. `finalize_human_review` records the
            // decision identity, verdict and `verified` atomically; a reject stays in the library,
            // out of exports, and out of every queue.
            db.finalize_human_review(&s.id, "reject", None, None, None).map_err(|e| format!("{}: {e}", s.id))?;
            done += 1;
        }
    }

    if apply {
        println!("\nrejected {done} clip(s).");
        println!("UNDO: re-review any of them in the app or on the phone — the decision is reversible,");
        println!("      the audio and transcripts were never touched.");
    } else {
        println!("\nDRY RUN — nothing was written. Re-run with --apply.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn schema_v60_apply_refuses_without_touching_the_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("cortex-speech.db");
        {
            let conn = Connection::open(&db_path).expect("create fixture database");
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER NOT NULL);\n                 INSERT INTO schema_migrations(version) VALUES (60);\n                 CREATE TABLE sentinel (value TEXT NOT NULL);\n                 INSERT INTO sentinel(value) VALUES ('untouched');",
            )
            .expect("create v60 marker and sentinel");
        }
        let before = std::fs::read(&db_path).expect("read fixture before refusal");

        let error = run(dir.path(), true).expect_err("schema v60 apply must fail closed");

        assert!(error.contains("before opening a writable database"), "{error}");
        assert!(error.contains("playback evidence"), "{error}");
        assert!(error.contains("Cortex Review"), "{error}");
        assert_eq!(
            std::fs::read(&db_path).expect("read fixture after refusal"),
            before,
            "the database file must be byte-for-byte unchanged"
        );
    }

    #[test]
    fn schema_v60_dry_run_remains_available() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("cortex-speech.db");
        {
            let db = Database::open(db_path.to_str().expect("UTF-8 test path")).expect("open fixture database");
            db.initialize().expect("initialize current schema");
            assert!(
                cortex_speech_app_lib::migrations::get_current_version(&db).expect("read fixture schema")
                    >= EFFECT_BOUND_REVIEW_SCHEMA
            );
        }

        run(dir.path(), false).expect("schema v60 dry-run must remain available");
    }

    #[test]
    fn pre_v60_apply_still_reaches_the_legacy_operation() {
        let called = Cell::new(false);
        apply_before_effect_bound_review(59, || {
            called.set(true);
            Ok(())
        })
        .expect("schema v59 remains eligible for the legacy maintenance apply");
        assert!(called.get());
    }

    #[test]
    fn schema_v60_guard_never_calls_the_writable_operation() {
        let called = Cell::new(false);
        let error = apply_before_effect_bound_review(60, || {
            called.set(true);
            Ok(())
        })
        .expect_err("schema v60 must be refused");
        assert!(!called.get(), "no writable operation may run after the v60 boundary");
        assert!(error.contains("evidence-backed review flow"), "{error}");
    }
}
