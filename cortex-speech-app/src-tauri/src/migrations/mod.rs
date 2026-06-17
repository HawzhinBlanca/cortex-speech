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
            db.connection().execute_batch(migration.up_sql)?;
            record_migration(db, migration.version, migration.description)?;
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

fn record_migration(db: &Database, version: i64, description: &str) -> AppResult<()> {
    db.connection().execute(
        "INSERT INTO schema_migrations (version, description) VALUES (?1, ?2)",
        rusqlite::params![version, description],
    )?;
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
}
