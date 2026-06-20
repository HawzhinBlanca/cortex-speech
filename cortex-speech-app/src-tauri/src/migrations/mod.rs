use crate::db::Database;
use crate::error::AppResult;
use serde::{Deserialize, Serialize};

/// Schema migration for the database.
/// Each migration has a version number and an up/down script.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Migration {
    pub version: i64,
    pub description: &'static str,
    pub up_sql: &'static str,
    pub down_sql: Option<&'static str>,
}

/// Run all pending migrations on the database.
pub fn run_migrations(db: &Database) -> AppResult<Vec<i64>> {
    ensure_migrations_table(db)?;
    let current_version = get_current_version(db)?;
    let mut applied = Vec::new();

    for migration in MIGRATIONS {
        if migration.version > current_version {
            tracing::info!("Applying migration v{}: {}", migration.version, migration.description);
            apply_migration(db, migration)?;
            applied.push(migration.version);
        }
    }

    Ok(applied)
}

/// Get the current schema version.
pub fn get_current_version(db: &Database) -> AppResult<i64> {
    let result: Result<i64, _> =
        db.connection().query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |row| row.get(0));
    Ok(result.unwrap_or(0))
}

fn ensure_migrations_table(db: &Database) -> AppResult<()> {
    db.connection().execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            description TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )?;
    Ok(())
}

/// Apply one migration atomically: its DDL and the schema_migrations version row commit
/// together or not at all. Without this, a crash/failure between the schema change and
/// the version INSERT leaves a half-applied schema that `open_with_retry` quarantines as
/// corrupt — silently starting the user from an empty database. SQLite DDL is
/// transactional, so any error rolls the whole migration back.
fn apply_migration(db: &Database, migration: &Migration) -> AppResult<()> {
    let conn = db.connection();
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(migration.up_sql)?;
    tx.execute(
        "INSERT INTO schema_migrations (version, description) VALUES (?1, ?2)",
        rusqlite::params![migration.version, migration.description],
    )?;
    tx.commit()?;
    Ok(())
}

/// Rollback the last N migrations.
pub fn rollback(db: &Database, count: usize) -> AppResult<Vec<i64>> {
    let current = get_current_version(db)?;
    let mut reverted = Vec::new();

    for migration in MIGRATIONS.iter().rev() {
        if migration.version <= current && reverted.len() < count {
            if let Some(down_sql) = migration.down_sql {
                tracing::info!("Rolling back v{}: {}", migration.version, migration.description);
                db.connection().execute_batch(down_sql)?;
                db.connection().execute(
                    "DELETE FROM schema_migrations WHERE version = ?1",
                    rusqlite::params![migration.version],
                )?;
                reverted.push(migration.version);
            }
        }
    }

    Ok(reverted)
}

/// List all migrations and their status.
pub fn list_migrations(db: &Database) -> AppResult<Vec<MigrationStatus>> {
    let current = get_current_version(db)?;
    Ok(MIGRATIONS
        .iter()
        .map(|m| MigrationStatus {
            version: m.version,
            description: m.description.to_string(),
            applied: m.version <= current,
        })
        .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStatus {
    pub version: i64,
    pub description: String,
    pub applied: bool,
}

/// All defined migrations.
pub static MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        description: "Initial schema — speech_segments, settings, FTS",
        up_sql: include_str!("../../migrations/001_initial.sql"),
        down_sql: Some("DROP TABLE IF EXISTS segments_fts;"),
    },
    Migration {
        version: 2,
        description: "Add session state tracking columns",
        up_sql: "ALTER TABLE speech_segments ADD COLUMN session_id TEXT;",
        down_sql: Some("ALTER TABLE speech_segments DROP COLUMN session_id;"),
    },
    Migration {
        version: 3,
        description: "Add confidence score column",
        up_sql: "ALTER TABLE speech_segments ADD COLUMN confidence REAL;",
        down_sql: Some("ALTER TABLE speech_segments DROP COLUMN confidence;"),
    },
    Migration {
        version: 4,
        description: "Add segment_hypotheses table for multi-hypothesis support",
        up_sql: "CREATE TABLE IF NOT EXISTS segment_hypotheses (
            segment_id TEXT NOT NULL,
            model_id TEXT NOT NULL,
            transcript TEXT NOT NULL,
            confidence REAL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (segment_id, model_id),
            FOREIGN KEY (segment_id) REFERENCES speech_segments(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_hypotheses_segment ON segment_hypotheses(segment_id);",
        down_sql: Some("DROP TABLE IF EXISTS segment_hypotheses;"),
    },
    Migration {
        version: 5,
        description: "Add ctc_score column for acoustic consistency caching",
        up_sql: "ALTER TABLE speech_segments ADD COLUMN ctc_score REAL;",
        down_sql: Some("ALTER TABLE speech_segments DROP COLUMN ctc_score;"),
    },
    Migration {
        version: 6,
        description: "Add clipping_ratio, rms_db, and snr_db columns for audio quality gating",
        up_sql: "ALTER TABLE speech_segments ADD COLUMN clipping_ratio REAL;
                 ALTER TABLE speech_segments ADD COLUMN rms_db REAL;
                 ALTER TABLE speech_segments ADD COLUMN snr_db REAL;",
        down_sql: Some(
            "ALTER TABLE speech_segments DROP COLUMN clipping_ratio;
                       ALTER TABLE speech_segments DROP COLUMN rms_db;
                       ALTER TABLE speech_segments DROP COLUMN snr_db;",
        ),
    },
    Migration {
        version: 7,
        description: "Add split column for Hugging Face split assignment",
        up_sql: "ALTER TABLE speech_segments ADD COLUMN split TEXT;",
        down_sql: Some("ALTER TABLE speech_segments DROP COLUMN split;"),
    },
    Migration {
        version: 8,
        description: "Add ood_score column for out-of-distribution tracking",
        up_sql: "ALTER TABLE speech_segments ADD COLUMN ood_score REAL;",
        down_sql: Some("ALTER TABLE speech_segments DROP COLUMN ood_score;"),
    },
    Migration {
        version: 9,
        description: "Add dataset run, job history, and model manifest tables",
        up_sql: "CREATE TABLE IF NOT EXISTS dataset_runs (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            status TEXT NOT NULL,
            config_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            completed_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_dataset_runs_created ON dataset_runs(created_at);

        CREATE TABLE IF NOT EXISTS job_history (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            status TEXT NOT NULL,
            progress REAL NOT NULL DEFAULT 0,
            cancellable INTEGER NOT NULL DEFAULT 0,
            summary TEXT,
            error TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_job_history_status ON job_history(status);
        CREATE INDEX IF NOT EXISTS idx_job_history_created ON job_history(created_at);

        CREATE TABLE IF NOT EXISTS model_manifest (
            filename TEXT PRIMARY KEY,
            size_bytes INTEGER NOT NULL,
            sha256 TEXT NOT NULL,
            source_url TEXT,
            version TEXT,
            installed_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
        down_sql: Some(
            "DROP TABLE IF EXISTS model_manifest;
                       DROP TABLE IF EXISTS job_history;
                       DROP TABLE IF EXISTS dataset_runs;",
        ),
    },
    Migration {
        version: 10,
        description: "Add gold_segments holdout and eval_runs tables for the gold-set eval harness",
        up_sql: "CREATE TABLE IF NOT EXISTS gold_segments (
            id         TEXT PRIMARY KEY,
            audio_path TEXT NOT NULL,
            reference  TEXT NOT NULL,
            is_holdout INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_gold_created ON gold_segments(created_at);
        CREATE TABLE IF NOT EXISTS eval_runs (
            id         TEXT PRIMARY KEY,
            model_id   TEXT NOT NULL,
            run_at     TEXT NOT NULL DEFAULT (datetime('now')),
            num_segs   INTEGER NOT NULL,
            wer        REAL NOT NULL,
            cer        REAL NOT NULL,
            meta_json  TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_eval_runs_model ON eval_runs(model_id);",
        down_sql: Some("DROP TABLE IF EXISTS eval_runs; DROP TABLE IF EXISTS gold_segments;"),
    },
    Migration {
        version: 11,
        description: "Add verdict/rationale/evidence columns + agent_examples few-shot memory table",
        up_sql: "ALTER TABLE speech_segments ADD COLUMN verdict TEXT;
                 ALTER TABLE speech_segments ADD COLUMN verdict_transcript TEXT;
                 ALTER TABLE speech_segments ADD COLUMN rationale TEXT;
                 ALTER TABLE speech_segments ADD COLUMN evidence_json TEXT;
                 ALTER TABLE speech_segments ADD COLUMN agent_confidence REAL;
                 ALTER TABLE speech_segments ADD COLUMN escalated INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE speech_segments ADD COLUMN human_decision TEXT;
                 ALTER TABLE speech_segments ADD COLUMN corrected_at TEXT;
                 ALTER TABLE speech_segments ADD COLUMN is_gold INTEGER NOT NULL DEFAULT 0;
                 CREATE INDEX IF NOT EXISTS idx_segments_verdict ON speech_segments(verdict);
                 CREATE INDEX IF NOT EXISTS idx_segments_escalated ON speech_segments(escalated);
                 CREATE TABLE IF NOT EXISTS agent_examples (
                     id               TEXT PRIMARY KEY,
                     segment_id       TEXT NOT NULL REFERENCES speech_segments(id) ON DELETE CASCADE,
                     audio_features   TEXT,
                     wrong_transcript TEXT NOT NULL,
                     human_fix        TEXT NOT NULL,
                     created_at       TEXT NOT NULL DEFAULT (datetime('now'))
                 );
                 CREATE INDEX IF NOT EXISTS idx_examples_segment ON agent_examples(segment_id);
                 CREATE INDEX IF NOT EXISTS idx_examples_created ON agent_examples(created_at);",
        down_sql: Some("DROP TABLE IF EXISTS agent_examples;"),
    },
    Migration {
        version: 12,
        description: "Add alignment_quality column to track timestamp precision per segment",
        up_sql: "ALTER TABLE speech_segments ADD COLUMN alignment_quality TEXT;",
        down_sql: Some("ALTER TABLE speech_segments DROP COLUMN alignment_quality;"),
    },
    Migration {
        version: 13,
        description: "Add audio_path index to enable O(log N) lookup by file path (media security check)",
        up_sql: "CREATE INDEX IF NOT EXISTS idx_segments_audio_path ON speech_segments(audio_path);",
        down_sql: Some("DROP INDEX IF EXISTS idx_segments_audio_path;"),
    },
    Migration {
        version: 14,
        description: "Add source_transcripts table for agentic whole-file reference transcripts",
        up_sql: "CREATE TABLE IF NOT EXISTS source_transcripts (
            audio_path TEXT NOT NULL,
            model_id TEXT NOT NULL,
            transcript_path TEXT NOT NULL,
            transcript_text TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (audio_path, model_id)
        );
        CREATE INDEX IF NOT EXISTS idx_source_transcripts_audio ON source_transcripts(audio_path);",
        down_sql: Some("DROP TABLE IF EXISTS source_transcripts;"),
    },
    Migration {
        version: 15,
        description: "Add agent_import_reports table for auditable multi-agent import runs",
        up_sql: "CREATE TABLE IF NOT EXISTS agent_import_reports (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            status TEXT NOT NULL,
            audio_paths_json TEXT NOT NULL,
            segment_ids_json TEXT NOT NULL,
            report_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_agent_import_reports_created ON agent_import_reports(created_at);
        CREATE INDEX IF NOT EXISTS idx_agent_import_reports_status ON agent_import_reports(status);",
        down_sql: Some("DROP TABLE IF EXISTS agent_import_reports;"),
    },
    Migration {
        version: 16,
        description: "Add agent_stage_events table for durable multi-agent import timelines",
        up_sql: "CREATE TABLE IF NOT EXISTS agent_stage_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL,
            source TEXT NOT NULL,
            stage TEXT NOT NULL,
            status TEXT NOT NULL,
            file TEXT NOT NULL,
            detail TEXT NOT NULL,
            current INTEGER NOT NULL DEFAULT 0,
            total INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_agent_stage_events_run ON agent_stage_events(run_id, id);
        CREATE INDEX IF NOT EXISTS idx_agent_stage_events_created ON agent_stage_events(created_at);",
        down_sql: Some("DROP TABLE IF EXISTS agent_stage_events;"),
    },
    Migration {
        version: 17,
        description: "Add audio identity fields to whole-file source transcripts",
        up_sql: "ALTER TABLE source_transcripts ADD COLUMN audio_content_hash TEXT;
                 ALTER TABLE source_transcripts ADD COLUMN audio_size_bytes INTEGER;",
        down_sql: Some(
            "ALTER TABLE source_transcripts DROP COLUMN audio_content_hash;
                        ALTER TABLE source_transcripts DROP COLUMN audio_size_bytes;",
        ),
    },
    Migration {
        version: 18,
        description: "Add eval_segment_results table for detailed evaluation records",
        up_sql: "CREATE TABLE IF NOT EXISTS eval_segment_results (
            id            TEXT PRIMARY KEY,
            eval_run_id   TEXT NOT NULL,
            gold_id       TEXT NOT NULL,
            audio_path    TEXT NOT NULL,
            reference     TEXT NOT NULL,
            hypothesis    TEXT NOT NULL,
            wer           REAL NOT NULL,
            cer           REAL NOT NULL,
            word_distance INTEGER NOT NULL,
            word_ref_len  INTEGER NOT NULL,
            char_distance INTEGER NOT NULL,
            char_ref_len  INTEGER NOT NULL,
            FOREIGN KEY(eval_run_id) REFERENCES eval_runs(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_eval_seg_run ON eval_segment_results(eval_run_id);",
        down_sql: Some("DROP TABLE IF EXISTS eval_segment_results;"),
    },
    Migration {
        version: 19,
        description: "Composite index for the verified-filtered, created_at-ordered segment list",
        // The main segment-list query (`WHERE verified=? ORDER BY created_at DESC`, run on
        // every load and after every mutation) was served by separate single-column indexes
        // — good for the filter OR the sort, not both. This composite covers both in one
        // index scan, the difference between demo-scale and 100k-segment-instant.
        up_sql: "CREATE INDEX IF NOT EXISTS idx_segments_verified_created
                 ON speech_segments(verified, created_at);",
        down_sql: Some("DROP INDEX IF EXISTS idx_segments_verified_created;"),
    },
    Migration {
        version: 20,
        description: "Add correction_memory table — the LOOP 0 instant error-memory store (P0)",
        // The continual-learning flywheel's fastest loop: when a curator fixes a token, we
        // persist a normalized slot key (the ±1 neighbor context) + a phonetic key
        // (g2p(normalize(wrong_token))) so the *same* confusion is corrected on the next
        // decode with NO retraining — engine-agnostic, fully auditable. The firing logic
        // (a weighted vote into the ROVER confusion network) lands in a later phase; this is
        // only the provenance-stamped store it reads from.
        //
        // `source_segment` is intentionally NULLABLE with ON DELETE SET NULL (not NOT NULL):
        // a learned correction is a generalization that must OUTLIVE the clip that spawned it
        // ("fix once -> right forever"), and a NOT NULL + RESTRICT FK would also block ordinary
        // segment deletion once any memory exists. Provenance is best-effort; the memory is not.
        up_sql: "CREATE TABLE IF NOT EXISTS correction_memory (
            id               TEXT PRIMARY KEY,
            wrong_token      TEXT NOT NULL,
            human_token      TEXT NOT NULL,
            slot_key         TEXT NOT NULL,
            phonetic_key     TEXT NOT NULL,
            source_segment   TEXT REFERENCES speech_segments(id) ON DELETE SET NULL,
            model_version_id TEXT,
            confidence       REAL NOT NULL DEFAULT 1.0,
            hit_count        INTEGER NOT NULL DEFAULT 0,
            last_fired_at    TEXT,
            created_at       TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_corrmem_slot ON correction_memory(slot_key);
        CREATE INDEX IF NOT EXISTS idx_corrmem_phon ON correction_memory(phonetic_key);",
        down_sql: Some("DROP TABLE IF EXISTS correction_memory;"),
    },
    Migration {
        version: 21,
        description: "Add corrections provenance ledger — reconstructable, attributable training set (P0)",
        // The append-only audit trail for the flywheel: every human fix, the raw hypothesis,
        // each engine's transcript, the cross-architecture agreement (a LOOP-1 signal), which
        // model/adapter produced the corrected label, and which loop changed the output. This
        // is what makes the training set reproducible and every published label attributable.
        //
        // `audio_content_hash` is NOT NULL — it is the DURABLE identity (the single source of
        // truth for holdout exclusion, matching jury/learning.rs::build_dpo_dataset). The live
        // `segment_id` pointer is NULLABLE with ON DELETE SET NULL so the audit row OUTLIVES a
        // deleted segment (erasing audit history on delete would defeat the ledger's purpose),
        // and so segment deletion is never blocked. The two indexes serve the load-bearing
        // queries: holdout exclusion (by hash) and per-segment history (by segment_id).
        up_sql: "CREATE TABLE IF NOT EXISTS corrections (
            id                  TEXT PRIMARY KEY,
            segment_id          TEXT REFERENCES speech_segments(id) ON DELETE SET NULL,
            audio_content_hash  TEXT NOT NULL,
            raw_hypothesis      TEXT NOT NULL,
            ensemble_hyps_json  TEXT,
            agreement_score     REAL,
            jury_verdict        TEXT,
            human_fix           TEXT NOT NULL,
            model_version_id    TEXT,
            adapter_id          TEXT,
            reviewer_id         TEXT,
            loop_applied        TEXT,
            decided_at          TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_corrections_hash ON corrections(audio_content_hash);
        CREATE INDEX IF NOT EXISTS idx_corrections_segment ON corrections(segment_id);",
        down_sql: Some("DROP TABLE IF EXISTS corrections;"),
    },
    Migration {
        version: 22,
        description: "Stamp model_version_id on every hypothesis + verdict (P0 provenance gate)",
        // The P0 attribution gate: no hypothesis or verdict may exist without naming WHICH model
        // produced it. Rather than edit every INSERT path, the column is NOT NULL DEFAULT
        // 'unknown@pre-registry' — SQLite back-fills every existing row to that sentinel, and any
        // future INSERT that omits the column still receives it, so the gate ("no row lacks
        // model_version_id") holds at the schema level. The registry (model_versions/adapters,
        // P1) will turn this free-text id into a foreign key once those tables land.
        up_sql: "ALTER TABLE segment_hypotheses ADD COLUMN model_version_id TEXT NOT NULL DEFAULT 'unknown@pre-registry';
                 ALTER TABLE speech_segments ADD COLUMN model_version_id TEXT NOT NULL DEFAULT 'unknown@pre-registry';",
        down_sql: Some(
            "ALTER TABLE speech_segments DROP COLUMN model_version_id;
             ALTER TABLE segment_hypotheses DROP COLUMN model_version_id;",
        ),
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn test_migrations_run() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let _applied = run_migrations(&db).unwrap();
        assert!(get_current_version(&db).unwrap() >= 1);
    }

    #[test]
    fn test_migration_idempotent() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let _first = run_migrations(&db).unwrap();
        let second = run_migrations(&db).unwrap();
        // Second run should not re-apply
        assert!(second.is_empty());
    }

    #[test]
    fn test_list_migrations() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        run_migrations(&db).unwrap();
        let list = list_migrations(&db).unwrap();
        assert!(!list.is_empty());
    }

    #[test]
    fn migration_versions_are_strictly_ascending_and_unique() {
        // run_migrations applies anything with version > current and tracks MAX(version);
        // a duplicate or out-of-order version would silently skip a migration or hit a
        // PRIMARY KEY conflict. Catch that developer mistake here, at the source.
        for pair in MIGRATIONS.windows(2) {
            assert!(
                pair[0].version < pair[1].version,
                "migrations must be strictly ascending and unique: v{} is not < v{}",
                pair[0].version,
                pair[1].version,
            );
        }
        assert_eq!(MIGRATIONS.first().map(|m| m.version), Some(1), "migrations should start at v1");
    }

    #[test]
    fn initialize_applies_every_migration_and_is_idempotent() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let max_version = MIGRATIONS.iter().map(|m| m.version).max().unwrap();

        // Every migration was applied and the schema is at the latest version.
        assert_eq!(get_current_version(&db).unwrap(), max_version);
        let recorded: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(recorded as usize, MIGRATIONS.len(), "every migration must be recorded exactly once");

        // Re-running migrates nothing and leaves the version untouched.
        let again = run_migrations(&db).unwrap();
        assert!(again.is_empty());
        assert_eq!(get_current_version(&db).unwrap(), max_version);
    }

    #[test]
    fn migration_v19_creates_verified_created_index() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let exists: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_segments_verified_created'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "the v19 composite index must exist after initialize()");
    }

    #[test]
    fn migration_v20_creates_correction_memory() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();

        // The table and both lookup indexes exist after initialize().
        let table: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='correction_memory'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(table, 1, "correction_memory table must exist after initialize()");
        let indexes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index'
                 AND name IN ('idx_corrmem_slot', 'idx_corrmem_phon')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexes, 2, "both correction_memory lookup indexes must exist");

        // A row inserts with only the required columns; defaults fill confidence/hit_count/created_at.
        conn.execute(
            "INSERT INTO correction_memory (id, wrong_token, human_token, slot_key, phonetic_key)
             VALUES ('m1', 'wrong', 'right', 'L|R', 'phon')",
            [],
        )
        .unwrap();
        let (conf, hits, created_set): (f64, i64, i64) = conn
            .query_row(
                "SELECT confidence, hit_count, created_at IS NOT NULL FROM correction_memory WHERE id='m1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(conf, 1.0, "confidence must default to 1.0");
        assert_eq!(hits, 0, "hit_count must default to 0");
        assert_eq!(created_set, 1, "created_at must be populated by its default");
    }

    #[test]
    fn correction_memory_survives_source_segment_deletion() {
        // A learned correction must OUTLIVE the clip that spawned it. With ON DELETE SET NULL
        // (not CASCADE / not RESTRICT), deleting the source segment nulls the provenance but
        // keeps the memory — and crucially does NOT block the segment deletion itself.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();
        // FK enforcement must be on for the SET NULL action to fire (it is, per Database::open).
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        conn.execute("INSERT INTO speech_segments (id, audio_path) VALUES ('seg-x', '/tmp/a.wav')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO correction_memory (id, wrong_token, human_token, slot_key, phonetic_key, source_segment)
             VALUES ('m1', 'wrong', 'right', 'L|R', 'phon', 'seg-x')",
            [],
        )
        .unwrap();

        // Deleting the source segment must succeed (not be blocked by the FK)...
        conn.execute("DELETE FROM speech_segments WHERE id='seg-x'", []).unwrap();
        // ...and the memory survives with its provenance nulled out.
        let (count, src_is_null): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), MAX(source_segment IS NULL) FROM correction_memory WHERE id='m1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1, "the learned correction must survive its source segment's deletion");
        assert_eq!(src_is_null, 1, "source_segment provenance must be SET NULL on delete");
    }

    #[test]
    fn migration_v21_creates_corrections_ledger() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();

        let table: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='corrections'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(table, 1, "corrections ledger table must exist after initialize()");
        let indexes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index'
                 AND name IN ('idx_corrections_hash', 'idx_corrections_segment')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexes, 2, "both corrections lookup indexes must exist");

        // A row inserts with only the required columns; decided_at fills from its default.
        conn.execute(
            "INSERT INTO corrections (id, audio_content_hash, raw_hypothesis, human_fix)
             VALUES ('c1', 'blake3hash', 'wrong text', 'right text')",
            [],
        )
        .unwrap();
        let decided_set: i64 = conn
            .query_row("SELECT decided_at IS NOT NULL FROM corrections WHERE id='c1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(decided_set, 1, "decided_at must be populated by its default");
    }

    #[test]
    fn corrections_ledger_survives_source_segment_deletion() {
        // The audit ledger must OUTLIVE a deleted segment — its durable identity is the
        // audio_content_hash, not the live segment_id pointer. Deleting the source segment
        // must succeed (FK does not block it) and the audit row must remain, hash intact,
        // with segment_id SET NULL.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        conn.execute("INSERT INTO speech_segments (id, audio_path) VALUES ('seg-y', '/tmp/b.wav')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO corrections (id, segment_id, audio_content_hash, raw_hypothesis, human_fix)
             VALUES ('c1', 'seg-y', 'blake3hash', 'wrong', 'right')",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM speech_segments WHERE id='seg-y'", []).unwrap();
        let (count, seg_is_null, hash): (i64, i64, String) = conn
            .query_row(
                "SELECT COUNT(*), MAX(segment_id IS NULL), MAX(audio_content_hash)
                 FROM corrections WHERE id='c1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(count, 1, "the audit row must survive its source segment's deletion");
        assert_eq!(seg_is_null, 1, "segment_id must be SET NULL on delete");
        assert_eq!(hash, "blake3hash", "the durable audio_content_hash must remain intact");
    }

    #[test]
    fn migration_v22_stamps_model_version_id_with_sentinel_default() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();

        // Both tables carry the column.
        for table in ["speech_segments", "segment_hypotheses"] {
            let has_col: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name='model_version_id'",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(has_col, 1, "{table} must have a model_version_id column after v22");
        }

        // The gate: an INSERT that OMITS model_version_id still gets the sentinel — so no
        // hypothesis or verdict row can ever lack attribution, without touching INSERT paths.
        conn.execute("INSERT INTO speech_segments (id, audio_path) VALUES ('s1', '/a.wav')", []).unwrap();
        conn.execute(
            "INSERT INTO segment_hypotheses (segment_id, model_id, transcript)
             VALUES ('s1', 'omniasr-ctc-300m', 'hi')",
            [],
        )
        .unwrap();
        let seg_mv: String = conn
            .query_row("SELECT model_version_id FROM speech_segments WHERE id='s1'", [], |r| r.get(0))
            .unwrap();
        let hyp_mv: String = conn
            .query_row("SELECT model_version_id FROM segment_hypotheses WHERE segment_id='s1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seg_mv, "unknown@pre-registry", "verdict rows default to the pre-registry sentinel");
        assert_eq!(hyp_mv, "unknown@pre-registry", "hypothesis rows default to the pre-registry sentinel");

        // No NULLs exist anywhere in either column.
        let nulls: i64 = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM speech_segments WHERE model_version_id IS NULL)
                      + (SELECT COUNT(*) FROM segment_hypotheses WHERE model_version_id IS NULL)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(nulls, 0, "model_version_id must never be NULL (NOT NULL DEFAULT enforces the gate)");
    }

    #[test]
    fn failed_migration_is_all_or_nothing() {
        // A migration whose DDL partly succeeds then hits invalid SQL must roll back
        // COMPLETELY — no partial table, no version row, version unchanged. Otherwise a
        // half-applied schema gets quarantined as corrupt → user starts from an empty DB.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let before = get_current_version(&db).unwrap();

        let bad = Migration {
            version: 99_999,
            description: "intentionally broken migration",
            up_sql: "CREATE TABLE should_not_persist (id INTEGER); THIS IS NOT VALID SQL;",
            down_sql: None,
        };
        assert!(apply_migration(&db, &bad).is_err(), "a broken migration must fail");

        assert_eq!(get_current_version(&db).unwrap(), before, "version must be unchanged");
        let version_rows: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM schema_migrations WHERE version = 99999", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version_rows, 0, "no version row may be left behind");
        let leaked_table: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='should_not_persist'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leaked_table, 0, "the partial table must have been rolled back");
    }

    #[test]
    fn rollback_then_reapply_restores_schema() {
        // The whole migration set must be round-trip safe: rolling back the latest
        // migration (running its down_sql) and re-applying it (its up_sql) returns to the
        // same version with no error. This also exercises that down_sql actually runs in
        // the bundled SQLite build.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let max_version = MIGRATIONS.iter().map(|m| m.version).max().unwrap();
        let prev_version = MIGRATIONS[MIGRATIONS.len() - 2].version;

        let reverted = rollback(&db, 1).unwrap();
        assert_eq!(reverted, vec![max_version], "rollback(1) must revert exactly the latest migration");
        assert_eq!(get_current_version(&db).unwrap(), prev_version);

        let reapplied = run_migrations(&db).unwrap();
        assert_eq!(reapplied, vec![max_version], "the rolled-back migration must re-apply");
        assert_eq!(get_current_version(&db).unwrap(), max_version);
    }
}
