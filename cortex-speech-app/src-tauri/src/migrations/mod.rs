use crate::db::Database;
use crate::error::AppResult;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema migration for the database.
/// Each migration has a version number and an up/down script.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Migration {
    pub version: i64,
    pub description: &'static str,
    pub up_sql: &'static str,
    pub down_sql: Option<&'static str>,
}

/// The highest migration version THIS binary knows how to run. A database at a version above this was
/// created by a NEWER build; operating on it silently (an old exe applies old semantics to a newer
/// schema — e.g. a pre-v32 build treating `correction_memory.confidence` under the frozen-1.0 rules
/// it no longer earns) is a data-integrity hazard.
pub fn max_supported_version() -> i64 {
    MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0)
}

/// Run all pending migrations on the database.
pub fn run_migrations(db: &Database) -> AppResult<Vec<i64>> {
    run_migrations_inner(db, false)
}

/// The only entry point allowed to bootstrap an empty migration history. `Database::initialize`
/// proves the SQLite file had no user objects *before* it creates the authoritative base tables,
/// then passes that proof here. Keeping the proof out of the public runner matters: otherwise an
/// existing database whose `schema_migrations` rows were deleted would be mistaken for a new file
/// and every migration would be replayed against live data.
pub(crate) fn run_migrations_after_pristine_initialize(db: &Database, was_pristine: bool) -> AppResult<Vec<i64>> {
    run_migrations_inner(db, was_pristine)
}

fn run_migrations_inner(db: &Database, allow_empty_bootstrap: bool) -> AppResult<Vec<i64>> {
    ensure_migrations_table(db)?;
    let current_version = validate_applied_history_inner(db.connection(), allow_empty_bootstrap)?;

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

/// A fresh SQLite file has no application-owned objects. This check must run before the base schema
/// is created; afterwards a new file and a damaged old file can have superficially similar tables.
pub(crate) fn database_is_pristine(conn: &rusqlite::Connection) -> AppResult<bool> {
    let objects: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema
          WHERE name NOT LIKE 'sqlite_%'
            AND type IN ('table', 'view', 'trigger', 'index')",
        [],
        |row| row.get(0),
    )?;
    Ok(objects == 0)
}

/// Prove that `schema_migrations` is the exact, description-bound prefix of this binary's history.
///
/// `MAX(version)` alone is not history: a damaged table containing only row 58 would make every older
/// migration look applied even though none of its schema exists. This validation is shared by startup,
/// rollback/list operations, and restore preflight so no path can silently trust that false maximum.
fn validate_applied_history_inner(conn: &rusqlite::Connection, allow_empty_bootstrap: bool) -> AppResult<i64> {
    let mut statement = match conn.prepare("SELECT version, description FROM schema_migrations ORDER BY version") {
        Ok(statement) => statement,
        Err(rusqlite::Error::SqliteFailure(_, Some(ref message))) if message.contains("no such table") => {
            return Err(crate::error::AppError::Other(
                "schema_migrations is missing; refusing to infer migration history from table shape".into(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let actual = statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    if actual.is_empty() {
        if allow_empty_bootstrap {
            // `Database::initialize` creates the authoritative base + FTS tables before migration 1,
            // then calls `run_migrations`. This is the sole legitimate empty-history context.
            return Ok(0);
        }
        return Err(crate::error::AppError::Other(
            "schema_migrations is empty; refusing an external database with unprovable history".into(),
        ));
    }

    let current_version = actual.last().map(|(version, _)| *version).unwrap_or(0);
    let max_known = max_supported_version();
    if current_version > max_known {
        return Err(crate::error::AppError::Other(format!(
            "This library is at schema v{current_version}, newer than this build supports (v{max_known}). \
             Update the app before opening or restoring it — refusing to operate on unknown history."
        )));
    }
    let expected: Vec<(i64, String)> = MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= current_version)
        .map(|migration| (migration.version, migration.description.to_string()))
        .collect();
    if actual != expected {
        let actual_versions: std::collections::BTreeSet<i64> = actual.iter().map(|(version, _)| *version).collect();
        let expected_versions: std::collections::BTreeSet<i64> = expected.iter().map(|(version, _)| *version).collect();
        let missing: Vec<i64> = expected_versions.difference(&actual_versions).copied().collect();
        let unknown: Vec<i64> = actual_versions.difference(&expected_versions).copied().collect();
        let description_mismatch: Vec<i64> = actual
            .iter()
            .filter_map(|(version, description)| {
                expected
                    .iter()
                    .find(|(expected_version, _)| expected_version == version)
                    .filter(|(_, expected_description)| expected_description != description)
                    .map(|_| *version)
            })
            .collect();
        return Err(crate::error::AppError::Other(format!(
            "schema migration history is incomplete or altered: missing={missing:?}, unknown={unknown:?}, \
             description_mismatch={description_mismatch:?}"
        )));
    }
    Ok(current_version)
}

/// Strict external-database form used before restore overwrites any live page.
pub fn validate_applied_history(conn: &rusqlite::Connection) -> AppResult<i64> {
    validate_applied_history_inner(conn, false)
}

/// Get the current schema version. A missing `schema_migrations` table (a genuinely fresh database)
/// is version 0; ANY OTHER error propagates. Previously every error collapsed to 0 (true-10 audit
/// 2026-07-09): a transient misread (I/O, SQLITE_BUSY) then both skipped the newer-schema guard and
/// re-applied ALL migrations — failing at v2's ADD COLUMN with "duplicate column name", a startup
/// error pointing nowhere near the real transient cause.
pub fn get_current_version(db: &Database) -> AppResult<i64> {
    let result: Result<i64, _> =
        db.connection().query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |row| row.get(0));
    match result {
        Ok(version) => Ok(version),
        Err(rusqlite::Error::SqliteFailure(_, Some(ref msg))) if msg.contains("no such table") => Ok(0),
        Err(e) => Err(crate::error::AppError::Other(format!(
            "could not read the schema version (transient database error, NOT a fresh database): {e}"
        ))),
    }
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
/// Migrations whose SQL rebuilds an FK **parent** table, and therefore MUST run with `foreign_keys` OFF.
///
/// `DROP TABLE` on an FK parent performs an implicit DELETE that FIRES `ON DELETE CASCADE`, wiping every
/// child row — proven, not assumed, by
/// `db::tests::dropping_speech_segments_cascade_deletes_children_so_strict_recreate_needs_fk_off`.
/// `PRAGMA foreign_keys` is a NO-OP inside a transaction, so the normal transaction-wrapped path
/// literally cannot express such a migration; these go through `run_with_foreign_keys_off` instead
/// (SQLite's canonical 12-step recreate). Keyed by version so the pre-existing migration literals stay
/// untouched. See docs/STRICT_SPEECH_SEGMENTS_PLAN.md.
const FK_OFF_MIGRATIONS: &[i64] = &[40];

/// Migrations whose purpose is to leave the *entire* database FK-clean even though they do not
/// rebuild an FK parent table. The normal migration path deliberately does not reject pre-existing
/// violations: older migrations must still be able to advance a legacy database. A targeted repair,
/// however, must prove that it removed exactly the damage it claims to repair and must fail closed if
/// any unrelated violation remains. Both apply and rollback run the check inside their transaction.
const FK_CLEANUP_MIGRATIONS: &[i64] = &[58];

/// Run `body` with `foreign_keys` OFF, VERIFYING it actually took effect and restoring it on every path.
///
/// **The read-back below is load-bearing, not paranoia.** SQLite silently IGNORES
/// `PRAGMA foreign_keys` inside a transaction and still reports success, so executing the statement
/// proves nothing. And `PRAGMA foreign_key_check` canNOT be used as the backstop: a cascade DELETES the
/// children *cleanly*, leaving ZERO violations behind — so an FK-still-ON recreate would pass the check
/// and commit total child-row loss silently. This read-back is therefore the only thing standing between
/// a mis-sequenced caller and irreversible data loss, which is why it fails closed.
///
/// A leaked `foreign_keys=OFF` is its own hazard: `Database::restore()` runs migrations at RUNTIME on the
/// live `AppState` connection, where a failure is non-fatal — so a swallowed restore error would leave the
/// app serving the whole session with FK enforcement silently disabled. Both failures are reported.
fn run_with_foreign_keys_off<T>(conn: &rusqlite::Connection, body: impl FnOnce() -> AppResult<T>) -> AppResult<T> {
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let effective: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if effective != 0 {
        // It never went off, so the original state is intact — nothing to restore, just refuse.
        return Err(crate::error::AppError::Other(
            "refusing to run an FK-off migration: `PRAGMA foreign_keys=OFF` did not take effect (SQLite \
             ignores it inside a transaction). Proceeding would let DROP TABLE cascade and silently \
             delete every child row — and foreign_key_check cannot detect that, because a cascade leaves \
             no violations behind."
                .into(),
        ));
    }
    let result = body();
    let restored = conn.execute_batch("PRAGMA foreign_keys=ON;");
    match (result, restored) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(body_err), Ok(())) => Err(body_err),
        (Ok(_), Err(restore_err)) => Err(crate::error::AppError::Other(format!(
            "migration body succeeded but restoring `PRAGMA foreign_keys=ON` failed: {restore_err} — \
             foreign keys are left DISABLED on this connection"
        ))),
        // Never let the body's error hide a failed restore: the leaked pragma outlives the migration.
        (Err(body_err), Err(restore_err)) => Err(crate::error::AppError::Other(format!(
            "migration failed ({body_err}) AND restoring `PRAGMA foreign_keys=ON` failed ({restore_err}) \
             — foreign keys are left DISABLED on this connection"
        ))),
    }
}

/// Reject a recreate that ORPHANED a child (a child row whose parent key no longer exists) before it can
/// commit.
///
/// Scope, honestly: this catches orphans — e.g. a copy that lost parent rows. It does **not** and cannot
/// catch a *cascade*, because a cascade deletes children cleanly and leaves zero violations to find.
/// Guarding against the cascade is `run_with_foreign_keys_off`'s read-back, not this check.
fn reject_foreign_key_violations(tx: &rusqlite::Transaction<'_>, version: i64) -> AppResult<()> {
    let mut stmt = tx.prepare("PRAGMA foreign_key_check")?;
    let violations = stmt.query_map([], |_| Ok(()))?.count();
    if violations > 0 {
        return Err(crate::error::AppError::Other(format!(
            "migration v{version} left {violations} foreign-key violation(s) — rolling back instead of \
             committing a broken schema"
        )));
    }
    Ok(())
}

/// Exact source identity for the one production orphan repair authorized in v58.
///
/// The digest is SHA-256 over the 2,104 missing-parent segment ids in bytewise sorted order, each
/// encoded as UTF-8 followed by `\n`.  Counts, one-to-one membership, and the abandoned-import row
/// shape are checked in the SAME transaction below.  This prevents v58 from becoming a generic
/// "delete every orphan in these tables" migration if another writer or another installation has a
/// different failure with superficially similar foreign keys.
const V58_ORPHAN_IDS: usize = 2_104;
const V58_ORPHAN_IDS_SHA256: &str = "b4d84377b75f493383a8acbb63bea39482597f95060c32cf88eda6011fa0aec9";
const V58_ORPHAN_FULL_TUPLE_SHA256: &str = "5776c4a205e843bc7d7550242b1542a3640427089a2af4876744667db24cb2e0";
#[cfg(test)]
const V58_TEST_ORPHAN_IDS_SHA256: &str = "fa888791a05c370e2b54a25c548f3e7a1a3db19260d4d526d71e320bd12e5aee";
#[cfg(test)]
const V58_TEST_ORPHAN_FULL_TUPLE_SHA256: &str = "05c72a200038a81071368c0788abe2ed0c2714a18516bee2cc9657a01fe64240";

fn v58_orphan_ids(tx: &rusqlite::Transaction<'_>, table: &str) -> AppResult<Vec<String>> {
    let sql = match table {
        "segment_hypotheses" => {
            "SELECT h.segment_id
               FROM segment_hypotheses h
              WHERE NOT EXISTS (SELECT 1 FROM speech_segments s WHERE s.id = h.segment_id)
              ORDER BY h.segment_id"
        }
        "loop0_shadow_log" => {
            "SELECT l.segment_id
               FROM loop0_shadow_log l
              WHERE NOT EXISTS (SELECT 1 FROM speech_segments s WHERE s.id = l.segment_id)
              ORDER BY l.segment_id"
        }
        _ => return Err(crate::error::AppError::Other("invalid v58 orphan source table".into())),
    };
    let mut statement = tx.prepare(sql)?;
    let ids = statement.query_map([], |row| row.get(0))?.collect::<Result<Vec<String>, _>>()?;
    Ok(ids)
}

fn validate_v58_orphan_source(tx: &rusqlite::Transaction<'_>) -> AppResult<()> {
    let hypothesis_ids = v58_orphan_ids(tx, "segment_hypotheses")?;
    let loop0_ids = v58_orphan_ids(tx, "loop0_shadow_log")?;
    if hypothesis_ids.is_empty() && loop0_ids.is_empty() {
        // Normal for every fresh/healthy installation: v58 still creates the empty immutable evidence
        // tables and records its schema version, but has no data to repair.
        return Ok(());
    }
    let unique = |ids: &[String]| ids.windows(2).all(|pair| pair[0] != pair[1]);
    if hypothesis_ids.len() != V58_ORPHAN_IDS
        || loop0_ids.len() != V58_ORPHAN_IDS
        || !unique(&hypothesis_ids)
        || !unique(&loop0_ids)
        || hypothesis_ids != loop0_ids
    {
        return Err(crate::error::AppError::Other(format!(
            "migration v58 source set is not the authorized {V58_ORPHAN_IDS}+{V58_ORPHAN_IDS} abandoned-import cohort"
        )));
    }

    // All 2,104 ids must be represented by exactly one row on each side and retain the measured
    // abandoned OmniASR-7B import shape.  This is intentionally much narrower than merely sharing the
    // two affected table names.
    let shaped: i64 = tx.query_row(
        "SELECT COUNT(*)
           FROM segment_hypotheses h
           JOIN loop0_shadow_log l ON l.segment_id = h.segment_id
          WHERE NOT EXISTS (SELECT 1 FROM speech_segments s WHERE s.id = h.segment_id)
            AND h.model_id = 'omniasr-7b-legacy-c348ade8a816'
            AND h.model_version_id = 'omniasr-7b-legacy-c348ade8a816'
            AND h.confidence IS NULL
            AND h.transcript <> ''
            AND l.memory_fired = 0
            AND l.created_at IS NOT NULL
            AND h.rowid - l.id = 2555
            AND length(h.segment_id) = 36
            AND substr(h.segment_id, 9, 1) = '-'
            AND substr(h.segment_id, 14, 1) = '-'
            AND substr(h.segment_id, 19, 1) = '-'
            AND substr(h.segment_id, 24, 1) = '-'
            AND length(replace(h.segment_id, '-', '')) = 32
            AND replace(h.segment_id, '-', '') NOT GLOB '*[^0-9a-f]*'",
        [],
        |row| row.get(0),
    )?;
    if shaped != V58_ORPHAN_IDS as i64 {
        return Err(crate::error::AppError::Other(format!(
            "migration v58 source rows do not match the authorized abandoned-import shape ({shaped}/{V58_ORPHAN_IDS})"
        )));
    }

    let mut digest = Sha256::new();
    for segment_id in &hypothesis_ids {
        digest.update(segment_id.as_bytes());
        digest.update(b"\n");
    }
    let actual: String = digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    #[cfg(test)]
    let accepted = actual == V58_ORPHAN_IDS_SHA256 || actual == V58_TEST_ORPHAN_IDS_SHA256;
    #[cfg(not(test))]
    let accepted = actual == V58_ORPHAN_IDS_SHA256;
    if !accepted {
        return Err(crate::error::AppError::Other(format!(
            "migration v58 source identity digest is not authorized (got {actual})"
        )));
    }

    // Bind every byte of the evidence that will be archived, including the transcript. The ID/shape
    // proof above prevents a generic cleanup; this second digest prevents the authorized IDs from
    // carrying altered transcription/timestamps/row identities while still looking structurally
    // plausible. Canonical form is one compact UTF-8 JSON line per sorted segment ID:
    // [[hypothesis source columns],[loop0 source columns]]\n.
    let mut statement = tx.prepare(
        "SELECT h.rowid, h.segment_id, h.model_id, h.transcript, h.confidence, h.created_at,
                h.model_version_id, l.id, l.segment_id, l.memory_fired, l.created_at
           FROM segment_hypotheses h
           JOIN loop0_shadow_log l ON l.segment_id = h.segment_id
          WHERE NOT EXISTS (SELECT 1 FROM speech_segments s WHERE s.id = h.segment_id)
          ORDER BY h.segment_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            (
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ),
            (row.get::<_, i64>(7)?, row.get::<_, String>(8)?, row.get::<_, i64>(9)?, row.get::<_, String>(10)?),
        ))
    })?;
    let mut full_digest = Sha256::new();
    for row in rows {
        let encoded = serde_json::to_vec(&row?).map_err(|error| {
            crate::error::AppError::Other(format!("migration v58 could not encode source evidence: {error}"))
        })?;
        full_digest.update(encoded);
        full_digest.update(b"\n");
    }
    let actual_full: String = full_digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    #[cfg(test)]
    let full_accepted = actual_full == V58_ORPHAN_FULL_TUPLE_SHA256 || actual_full == V58_TEST_ORPHAN_FULL_TUPLE_SHA256;
    #[cfg(not(test))]
    let full_accepted = actual_full == V58_ORPHAN_FULL_TUPLE_SHA256;
    if !full_accepted {
        return Err(crate::error::AppError::Other(format!(
            "migration v58 full source-evidence digest is not authorized (got {actual_full})"
        )));
    }
    Ok(())
}

fn apply_migration(db: &Database, migration: &Migration) -> AppResult<()> {
    let conn = db.connection();
    if FK_OFF_MIGRATIONS.contains(&migration.version) {
        return run_with_foreign_keys_off(conn, || {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(migration.up_sql)?;
            tx.execute(
                "INSERT INTO schema_migrations (version, description) VALUES (?1, ?2)",
                rusqlite::params![migration.version, migration.description],
            )?;
            reject_foreign_key_violations(&tx, migration.version)?;
            tx.commit()?;
            Ok(())
        });
    }
    let tx = conn.unchecked_transaction()?;
    if migration.version == 58 {
        validate_v58_orphan_source(&tx)?;
    }
    tx.execute_batch(migration.up_sql)?;
    tx.execute(
        "INSERT INTO schema_migrations (version, description) VALUES (?1, ?2)",
        rusqlite::params![migration.version, migration.description],
    )?;
    if FK_CLEANUP_MIGRATIONS.contains(&migration.version) {
        reject_foreign_key_violations(&tx, migration.version)?;
    }
    tx.commit()?;
    Ok(())
}

/// Rollback the last N migrations.
pub fn rollback(db: &Database, count: usize) -> AppResult<Vec<i64>> {
    let current = validate_applied_history(db.connection())?;
    let mut reverted = Vec::new();

    for migration in MIGRATIONS.iter().rev() {
        if migration.version <= current && reverted.len() < count {
            if let Some(down_sql) = migration.down_sql {
                tracing::info!("Rolling back v{}: {}", migration.version, migration.description);
                let conn = db.connection();
                // A parent-table recreate cascades on the way DOWN exactly as it does on the way up, so
                // the down_sql needs the same foreign_keys=OFF window — otherwise rolling back v40 would
                // wipe every child row it was written to protect.
                if FK_OFF_MIGRATIONS.contains(&migration.version) {
                    // Mirror apply_migration exactly: ONE transaction + the orphan check. A bare
                    // execute_batch here would auto-commit statement-by-statement, so a failure between
                    // `DROP TABLE speech_segments` and the RENAME would leave the database with NO
                    // speech_segments table at all — unrecoverable, and worse than the failed rollback.
                    run_with_foreign_keys_off(conn, || {
                        let tx = conn.unchecked_transaction()?;
                        tx.execute_batch(down_sql)?;
                        tx.execute(
                            "DELETE FROM schema_migrations WHERE version = ?1",
                            rusqlite::params![migration.version],
                        )?;
                        reject_foreign_key_violations(&tx, migration.version)?;
                        tx.commit()?;
                        Ok(())
                    })?;
                } else {
                    // Mirror apply_migration's non-FK-off path: ONE transaction. A bare execute_batch
                    // auto-commits statement-by-statement, and several down_sql bodies are multi-statement
                    // (v6/v9/v17/v22/v25/v31/v36/v37), so a failure partway left the schema half-reverted
                    // while schema_migrations still recorded the version as applied — run_migrations would
                    // then skip it forever, with no self-heal path. Same hazard the FK-off branch above
                    // already guards against; it just was not applied here.
                    let tx = conn.unchecked_transaction()?;
                    tx.execute_batch(down_sql)?;
                    tx.execute(
                        "DELETE FROM schema_migrations WHERE version = ?1",
                        rusqlite::params![migration.version],
                    )?;
                    if FK_CLEANUP_MIGRATIONS.contains(&migration.version) {
                        reject_foreign_key_violations(&tx, migration.version)?;
                    }
                    tx.commit()?;
                }
                reverted.push(migration.version);
            }
        }
    }

    Ok(reverted)
}

/// List all migrations and their status.
pub fn list_migrations(db: &Database) -> AppResult<Vec<MigrationStatus>> {
    let current = validate_applied_history(db.connection())?;
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
        up_sql:
            "ALTER TABLE segment_hypotheses ADD COLUMN model_version_id TEXT NOT NULL DEFAULT 'unknown@pre-registry';
                 ALTER TABLE speech_segments ADD COLUMN model_version_id TEXT NOT NULL DEFAULT 'unknown@pre-registry';",
        down_sql: Some(
            "ALTER TABLE speech_segments DROP COLUMN model_version_id;
             ALTER TABLE segment_hypotheses DROP COLUMN model_version_id;",
        ),
    },
    Migration {
        version: 23,
        description: "Add model registry (model_versions + adapters) with DB-enforced invariants (P1)",
        // The gated ingestion path for an externally fine-tuned model. model_versions gives
        // version lineage + eval scorecard + promotion status; adapters records LoRA lineage
        // (base SHA, adapter SHA, merged-checkpoint SHA) so a merge-to-base ingestion is
        // reproducible. Invariants are enforced by the SCHEMA, not by hopeful code:
        //   * CHECK on `source` and `status` — no garbage state can be written.
        //   * partial UNIQUE index idx_model_versions_one_champion — AT MOST ONE champion per
        //     family is physically impossible to violate (the promotion gate's core invariant).
        //   * adapters.parent_model_version_id ON DELETE CASCADE — an adapter is a delta OF its
        //     parent; it is meaningless without it, so it dies with the parent.
        // The non-empty checkpoint_sha256 requirement for trusted promotion is enforced at the
        // import code path (P1), not here, since empty-pin is legitimate for stock seeds.
        up_sql: "CREATE TABLE IF NOT EXISTS model_versions (
            id                  TEXT PRIMARY KEY,
            family              TEXT NOT NULL,
            model_card_name     TEXT,
            checkpoint_sha256   TEXT NOT NULL,
            checkpoint_path     TEXT NOT NULL,
            base_version_id     TEXT REFERENCES model_versions(id) ON DELETE SET NULL,
            source              TEXT NOT NULL
                                CHECK (source IN ('meta-stock', 'user-finetuned', 'cortex-finetuned')),
            license             TEXT NOT NULL,
            eval_run_id         TEXT REFERENCES eval_runs(id) ON DELETE SET NULL,
            gold_wer            REAL,
            gold_cer            REAL,
            gold_ci_low         REAL,
            gold_ci_high        REAL,
            mapsswe_p_vs_active REAL,
            scorecard_json      TEXT,
            status              TEXT NOT NULL DEFAULT 'candidate'
                                CHECK (status IN ('candidate', 'challenger', 'champion', 'rolled_back', 'rejected')),
            promoted_at         TEXT,
            created_at          TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_model_versions_family ON model_versions(family, status);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_model_versions_one_champion
            ON model_versions(family) WHERE status = 'champion';

        CREATE TABLE IF NOT EXISTS adapters (
            id                              TEXT PRIMARY KEY,
            parent_model_version_id         TEXT NOT NULL REFERENCES model_versions(id) ON DELETE CASCADE,
            base_checkpoint_sha             TEXT NOT NULL,
            adapter_sha256                  TEXT NOT NULL,
            merged_checkpoint_sha           TEXT,
            training_corrections_query_hash TEXT,
            recipe                          TEXT,
            created_at                      TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_adapters_parent ON adapters(parent_model_version_id);",
        down_sql: Some("DROP TABLE IF EXISTS adapters; DROP TABLE IF EXISTS model_versions;"),
    },
    Migration {
        version: 24,
        description: "Persist gold clip audio content hash so holdout exclusion survives a moved/deleted file",
        // holdout_content_hashes re-read each gold file from disk, so a moved/deleted gold file
        // silently dropped its hash and a same-content training clip leaked into DPO/HF exports
        // (fail-open). Persisting the hash at import makes the holdout set durable (fail-closed).
        up_sql: "ALTER TABLE gold_segments ADD COLUMN audio_content_hash TEXT;",
        down_sql: Some("ALTER TABLE gold_segments DROP COLUMN audio_content_hash;"),
    },
    Migration {
        version: 25,
        description: "Provenance-tier agent_examples: human=gold (trainable) vs model=pseudo (gated)",
        // The flywheel must distinguish trust tiers. Human verbatim edits are gold and may train the
        // model; MODEL corrections (the jury auto-correcting OmniASR) are pseudo-labels that must NOT
        // train weights until a human signs off, or training on model-generated labels causes model
        // collapse (Shumailov et al., Nature 2024). Existing rows are all human edits, so the
        // defaults (source='human', verified_by_human=1) classify them correctly.
        up_sql: "ALTER TABLE agent_examples ADD COLUMN source TEXT NOT NULL DEFAULT 'human';
                 ALTER TABLE agent_examples ADD COLUMN verified_by_human INTEGER NOT NULL DEFAULT 1;
                 ALTER TABLE agent_examples ADD COLUMN corrector_model_id TEXT;",
        down_sql: Some(
            "ALTER TABLE agent_examples DROP COLUMN source;
             ALTER TABLE agent_examples DROP COLUMN verified_by_human;
             ALTER TABLE agent_examples DROP COLUMN corrector_model_id;",
        ),
    },
    Migration {
        version: 26,
        description: "Index human_decision for review/export hot-path filters (F8)",
        // Every export filters human-rejected segments (is_human_rejected) and the label-quality
        // lift query selects WHERE human_decision IS NOT NULL — both table-scanned a growing
        // speech_segments. verdict/escalated/audio_path/verified are already indexed; human_decision
        // was the one hot filter column without one.
        up_sql: "CREATE INDEX IF NOT EXISTS idx_segments_human_decision ON speech_segments(human_decision);",
        down_sql: Some("DROP INDEX IF EXISTS idx_segments_human_decision;"),
    },
    Migration {
        version: 27,
        description: "Persist EM-fitted IRT model abilities so the jury warm-starts across runs (F7, opt-in)",
        up_sql: "CREATE TABLE IF NOT EXISTS model_abilities (
                     model_id TEXT PRIMARY KEY,
                     ability REAL NOT NULL,
                     updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                 );",
        down_sql: Some("DROP TABLE IF EXISTS model_abilities;"),
    },
    Migration {
        version: 28,
        description: "Decision timing log (M2.1) — record segment IDs, decision types, and timing (ms since segment came into focus)",
        up_sql: "CREATE TABLE IF NOT EXISTS decision_log (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     segment_id TEXT NOT NULL,
                     decision_type TEXT NOT NULL,
                     timestamp_ms INTEGER NOT NULL,
                     human_decision TEXT,
                     created_at TEXT DEFAULT (datetime('now')),
                     FOREIGN KEY(segment_id) REFERENCES speech_segments(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_decision_log_segment_id ON decision_log(segment_id);",
        down_sql: Some("DROP TABLE IF EXISTS decision_log; DROP INDEX IF EXISTS idx_decision_log_segment_id;"),
    },
    Migration {
        version: 29,
        description: "Per-segment T0/T1 jury verdict rows (M2.2) — track T0/T1 verdict status via separate table",
        up_sql: "CREATE TABLE IF NOT EXISTS decision_verdicts (
                     segment_id TEXT PRIMARY KEY,
                     auto_accept_verdict TEXT,
                     verdict_computed_at TEXT,
                     FOREIGN KEY(segment_id) REFERENCES speech_segments(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_decision_verdicts_verdict ON decision_verdicts(auto_accept_verdict);",
        down_sql: Some("DROP TABLE IF EXISTS decision_verdicts; DROP INDEX IF EXISTS idx_decision_verdicts_verdict;"),
    },
    Migration {
        version: 30,
        description: "LOOP-0 shadow logging (M2.3) — track would-fire memory events without mutations",
        up_sql: "CREATE TABLE IF NOT EXISTS loop0_shadow_log (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     segment_id TEXT NOT NULL,
                     memory_fired BOOLEAN,
                     created_at TEXT DEFAULT (datetime('now')),
                     FOREIGN KEY(segment_id) REFERENCES speech_segments(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_loop0_shadow_segment ON loop0_shadow_log(segment_id);",
        down_sql: Some("DROP TABLE IF EXISTS loop0_shadow_log; DROP INDEX IF EXISTS idx_loop0_shadow_segment;"),
    },
    Migration {
        version: 31,
        description: "Import journal (P3.2) — record directory-import progress so a crash mid-import is resumable",
        up_sql: "CREATE TABLE IF NOT EXISTS import_jobs (
                     id TEXT PRIMARY KEY,
                     dir TEXT NOT NULL,
                     total_files INTEGER NOT NULL,
                     status TEXT NOT NULL DEFAULT 'running',
                     created_at TEXT NOT NULL DEFAULT (datetime('now')),
                     updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                 );
                 CREATE TABLE IF NOT EXISTS import_job_files (
                     job_id TEXT NOT NULL,
                     path TEXT NOT NULL,
                     PRIMARY KEY (job_id, path),
                     FOREIGN KEY(job_id) REFERENCES import_jobs(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_import_jobs_status ON import_jobs(status);",
        down_sql: Some(
            "DROP TABLE IF EXISTS import_job_files; DROP TABLE IF EXISTS import_jobs; DROP INDEX IF EXISTS idx_import_jobs_status;",
        ),
    },
    Migration {
        version: 32,
        description: "LOOP-0 evidence-based confidence — per-memory confirm/override counts + Beta(1,1) posterior",
        // True-10 audit: correction_memory.confidence was frozen at the column DEFAULT 1.0 — nothing
        // ever wrote it — so the tau_conf firing gate was vacuous and one bad memory would poison
        // transcripts permanently once firing went live. These two counters record firing-OUTCOME
        // evidence at human-decision time (a would-fire the human subsequently confirmed vs
        // contradicted); confidence becomes the Beta(1,1)-posterior mean (confirm+1)/(confirm+override+2).
        // Recompute every existing row off its (zero) evidence so no legacy memory keeps a fabricated
        // 1.0 — a memory with no evidence drops to the neutral 0.5 prior, BELOW tau_conf 0.6, and must
        // earn the right to fire.
        up_sql: "ALTER TABLE correction_memory ADD COLUMN confirm_count INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE correction_memory ADD COLUMN override_count INTEGER NOT NULL DEFAULT 0;
                 UPDATE correction_memory
                    SET confidence = (confirm_count + 1.0) / (confirm_count + override_count + 2.0);",
        down_sql: Some(
            "ALTER TABLE correction_memory DROP COLUMN override_count;
             ALTER TABLE correction_memory DROP COLUMN confirm_count;",
        ),
    },
    Migration {
        version: 33,
        description: "LOOP-0 shadow-evidence archive — survives segment deletion (C5 gate not survivor-biased)",
        // loop0_shadow_log CASCADE-deletes with its segment, so the owner's normal cleanup (review a bad
        // clip, then delete it) silently removed exactly the rows most likely to be over-triggers — the
        // C5 "over-triggers must be 0 before firing go-live" gate then looked SAFER than reality. Before a
        // segment is deleted, its shadow contribution is aggregated into this single durable counter row,
        // and intelligence_report adds it to the live counts. One row (id=1), seeded here.
        up_sql: "CREATE TABLE IF NOT EXISTS loop0_evidence_archive (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     total_observations INTEGER NOT NULL DEFAULT 0,
                     would_fire INTEGER NOT NULL DEFAULT 0,
                     fired_human_accepted INTEGER NOT NULL DEFAULT 0,
                     fired_human_edited INTEGER NOT NULL DEFAULT 0,
                     fired_human_rejected INTEGER NOT NULL DEFAULT 0
                 );
                 INSERT OR IGNORE INTO loop0_evidence_archive (id) VALUES (1);",
        down_sql: Some("DROP TABLE IF EXISTS loop0_evidence_archive;"),
    },
    Migration {
        version: 34,
        description: "C4 auto-accept evidence archive — survives segment deletion (precision not survivor-biased)",
        // Same bug class v33 fixed for the C5 gate, applied to the C4 denominator (true-10 audit
        // 2026-07-09): decision_verdicts CASCADE-deletes with its segment, so deleting a reviewed
        // bad clip removed exactly the T0_ACCEPT rows whose humans CONTRADICTED the machine — the C4
        // auto-accept precision (the gate that authorizes raising the autonomy dial) could only
        // drift optimistic. Before a segment is deleted, its decision_verdicts row + human-decision
        // correlation are folded into this durable counter row, and intelligence_report adds it to
        // the live counts. One row (id=1), seeded here.
        up_sql: "CREATE TABLE IF NOT EXISTS c4_evidence_archive (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     t0_accepts INTEGER NOT NULL DEFAULT 0,
                     t1_escalations INTEGER NOT NULL DEFAULT 0,
                     t0_human_confirmed INTEGER NOT NULL DEFAULT 0,
                     t0_human_contradicted INTEGER NOT NULL DEFAULT 0
                 );
                 INSERT OR IGNORE INTO c4_evidence_archive (id) VALUES (1);",
        down_sql: Some("DROP TABLE IF EXISTS c4_evidence_archive;"),
    },
    Migration {
        version: 35,
        description: "Repair segments_fts: rebuild with the audio_path column so segment writes succeed",
        // A divergence between the AUTHORITATIVE segments_fts schema (db.rs initialize(): 6 columns incl.
        // audio_path) and migration v1's copy (001_initial.sql: 4 columns, NO audio_path) can leave a DB
        // with a 4-column segments_fts while the segments_ai/ad/au triggers reference audio_path. Result:
        // EVERY segment INSERT fails with "table segments_fts has no column named audio_path", the import
        // transaction rolls back, and VAD "produces 0 segments" — the app cannot ingest ANY audio. FTS5 has
        // no ALTER ADD COLUMN, so rebuild the shadow table to the authoritative 6-column shape and
        // repopulate from its external-content table. Idempotent: on an already-correct DB this
        // drops+recreates an identical table. The triggers resolve segments_fts by name at fire time, so
        // recreating it under them is safe.
        up_sql: "DROP TABLE IF EXISTS segments_fts;
                 CREATE VIRTUAL TABLE segments_fts USING fts5(
                     id UNINDEXED, audio_path, raw_transcript, normalized_transcript, annotated_transcript,
                     content=speech_segments, content_rowid=rowid, tokenize='unicode61'
                 );
                 INSERT INTO segments_fts(segments_fts) VALUES('rebuild');",
        down_sql: Some("DROP TABLE IF EXISTS segments_fts;"),
    },
    Migration {
        version: 36,
        description: "Persist segment proof metadata for confidence source, cloud use, decoder config, and normalizer version",
        // These columns turn transcript quality from a naked number into evidence. Legacy rows are
        // deliberately marked `unknown`, not `real_posterior`, so calibration/autonomy gates fail closed
        // until a fresh ASR pass records the real source. `cloud_call` defaults false because historical
        // local rows did not record a cloud event; explicit cloud providers must set it true at write time.
        up_sql: "ALTER TABLE speech_segments ADD COLUMN confidence_source TEXT NOT NULL DEFAULT 'unknown';
                 ALTER TABLE speech_segments ADD COLUMN cloud_call INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE speech_segments ADD COLUMN decoder_config_hash TEXT;
                 ALTER TABLE speech_segments ADD COLUMN normalizer_version TEXT;
                 CREATE INDEX IF NOT EXISTS idx_segments_cloud_call ON speech_segments(cloud_call);
                 CREATE INDEX IF NOT EXISTS idx_segments_confidence_source ON speech_segments(confidence_source);",
        down_sql: Some(
            "DROP INDEX IF EXISTS idx_segments_confidence_source;
             DROP INDEX IF EXISTS idx_segments_cloud_call;
             ALTER TABLE speech_segments DROP COLUMN normalizer_version;
             ALTER TABLE speech_segments DROP COLUMN decoder_config_hash;
             ALTER TABLE speech_segments DROP COLUMN cloud_call;
             ALTER TABLE speech_segments DROP COLUMN confidence_source;",
        ),
    },
    Migration {
        version: 37,
        description: "Durable jobs table for the persistent Job Supervisor (crash-safe long operations)",
        // A durable record for every long operation (import, transcribe, export, backup, eval, ...), so a
        // job survives an app crash/restart instead of vanishing with a detached thread. `state` is a
        // CHECK-constrained lifecycle; `idempotency_key` (UNIQUE where present) lets a re-issued identical
        // job resume/return the existing row instead of duplicating work; `progress`/`completed`/`total`
        // drive the UI's ETA; `error_code` is a STABLE machine code (MODEL_UNAVAILABLE, DISK_FULL,
        // SOURCE_MOVED, JOB_CANCELLED, ...) the UI renders as "what happened + what remains safe".
        up_sql: "CREATE TABLE IF NOT EXISTS jobs (
                     id TEXT PRIMARY KEY,
                     kind TEXT NOT NULL,
                     state TEXT NOT NULL DEFAULT 'queued'
                         CHECK (state IN ('queued','running','succeeded','failed','cancelled')),
                     idempotency_key TEXT,
                     progress REAL NOT NULL DEFAULT 0.0
                         CHECK (progress >= 0.0 AND progress <= 1.0),
                     total INTEGER,
                     completed INTEGER NOT NULL DEFAULT 0,
                     error_code TEXT,
                     error_detail TEXT,
                     payload_json TEXT,
                     created_at TEXT NOT NULL DEFAULT (datetime('now')),
                     updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                     started_at TEXT,
                     finished_at TEXT
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_idempotency
                     ON jobs(idempotency_key) WHERE idempotency_key IS NOT NULL;
                 CREATE INDEX IF NOT EXISTS idx_jobs_state ON jobs(state);
                 CREATE INDEX IF NOT EXISTS idx_jobs_kind_state ON jobs(kind, state);",
        down_sql: Some(
            "DROP INDEX IF EXISTS idx_jobs_kind_state;
             DROP INDEX IF EXISTS idx_jobs_state;
             DROP INDEX IF EXISTS idx_jobs_idempotency;
             DROP TABLE IF EXISTS jobs;",
        ),
    },
    Migration {
        version: 38,
        description: "STRICT-tables pilot: recreate decision_verdicts as a STRICT table",
        // Week-2 storage durability: STRICT tables reject affinity-mangled writes (a TEXT into an INT
        // column, a float into an id) at the DB boundary instead of silently coercing. SQLite can't
        // ALTER a table to STRICT, so this is the canonical recreate: new STRICT table -> copy -> drop
        // -> rename -> reindex, all inside apply_migration's transaction. SAFE with foreign_keys ON:
        // decision_verdicts is a CHILD only (FK -> speech_segments); NOTHING references it, so the DROP
        // orphans no inbound FK (verified: `grep REFERENCES decision_verdicts` is empty). The existing
        // rows already satisfy the same FK/columns, so the copy passes the STRICT + FK checks. All three
        // columns are TEXT (a valid STRICT type). This is the PILOT — the pattern the larger tables
        // (speech_segments + its FTS triggers) will follow in their own staged migrations.
        up_sql: "CREATE TABLE decision_verdicts_strict (
                     segment_id TEXT PRIMARY KEY,
                     auto_accept_verdict TEXT,
                     verdict_computed_at TEXT,
                     FOREIGN KEY(segment_id) REFERENCES speech_segments(id) ON DELETE CASCADE
                 ) STRICT;
                 INSERT INTO decision_verdicts_strict (segment_id, auto_accept_verdict, verdict_computed_at)
                     SELECT segment_id, auto_accept_verdict, verdict_computed_at FROM decision_verdicts;
                 DROP TABLE decision_verdicts;
                 ALTER TABLE decision_verdicts_strict RENAME TO decision_verdicts;
                 CREATE INDEX IF NOT EXISTS idx_decision_verdicts_verdict ON decision_verdicts(auto_accept_verdict);",
        // Down: recreate the NON-strict form (a rollback to the pre-v38 schema). Same recreate shape.
        down_sql: Some(
            "CREATE TABLE decision_verdicts_nonstrict (
                 segment_id TEXT PRIMARY KEY,
                 auto_accept_verdict TEXT,
                 verdict_computed_at TEXT,
                 FOREIGN KEY(segment_id) REFERENCES speech_segments(id) ON DELETE CASCADE
             );
             INSERT INTO decision_verdicts_nonstrict (segment_id, auto_accept_verdict, verdict_computed_at)
                 SELECT segment_id, auto_accept_verdict, verdict_computed_at FROM decision_verdicts;
             DROP TABLE decision_verdicts;
             ALTER TABLE decision_verdicts_nonstrict RENAME TO decision_verdicts;
             CREATE INDEX IF NOT EXISTS idx_decision_verdicts_verdict ON decision_verdicts(auto_accept_verdict);",
        ),
    },
    Migration {
        version: 39,
        description: "Rename ood_score -> signal_anomaly_score (OOD jargon retired; UI already said 'Signal Anomaly')",
        // The internal half of the OOD -> signal_anomaly rename (Week-3 item 4). The USER-FACING text
        // already read "Signal Anomaly"; this retires the jargon from the schema + code so the stored
        // column matches what the app has always shown.
        //
        // SAFE, unlike the STRICT recreate (see docs/STRICT_SPEECH_SEGMENTS_PLAN.md): RENAME COLUMN does
        // NOT drop/recreate speech_segments, so it never fires the ON DELETE CASCADE that a DROP would
        // (the trap proven by dropping_speech_segments_cascade_deletes_children_...). Existing values are
        // preserved in place; SQLite also auto-rewrites references in dependent objects. The historical
        // "ADD COLUMN ood_score" migration is deliberately LEFT INTACT above — a fresh DB creates
        // ood_score there, then this migration renames it, so replay-from-scratch and upgrade-in-place
        // both converge on signal_anomaly_score.
        up_sql: "ALTER TABLE speech_segments RENAME COLUMN ood_score TO signal_anomaly_score;",
        down_sql: Some("ALTER TABLE speech_segments RENAME COLUMN signal_anomaly_score TO ood_score;"),
    },
    Migration {
        version: 40,
        description: "STRICT speech_segments: recreate the main table as STRICT (runs with foreign_keys OFF)",
        // Week-2's last item. SQLite cannot ALTER a table to STRICT, so this is the canonical recreate —
        // and speech_segments is an FK PARENT of seven children (five ON DELETE CASCADE:
        // segment_hypotheses, agent_examples, decision_log, decision_verdicts, loop0_shadow_log; two
        // ON DELETE SET NULL: correction_memory.source_segment, corrections.segment_id). A plain DROP
        // here fires those cascades and wipes them (proven by
        // db::tests::dropping_speech_segments_cascade_deletes_children_so_strict_recreate_needs_fk_off),
        // which is why v40 is listed in FK_OFF_MIGRATIONS and runs inside a foreign_keys=OFF window with
        // a PRAGMA foreign_key_check before commit. Full rationale: docs/STRICT_SPEECH_SEGMENTS_PLAN.md.
        //
        // Every one of the 34 columns is already TEXT/INTEGER/REAL (a valid STRICT type), so this needs
        // ZERO type remapping — the shape below is a verbatim copy of the live schema, only + STRICT.
        // The copy PRESERVES rowid because segments_fts is an external-content FTS5 table keyed on
        // content_rowid=rowid; the index is rebuilt at the end regardless. All TEN indexes and all THREE
        // FTS triggers are recreated (DROP TABLE takes them with it — missing any one silently turns a
        // hot path into a full scan or desyncs search).
        up_sql: "CREATE TABLE speech_segments_strict (
                     id TEXT PRIMARY KEY,
                     audio_path TEXT NOT NULL,
                     raw_transcript TEXT NOT NULL DEFAULT '',
                     normalized_transcript TEXT,
                     annotated_transcript TEXT,
                     alignment_json TEXT,
                     duration_ms INTEGER NOT NULL DEFAULT 0,
                     speaker_id TEXT,
                     verified INTEGER NOT NULL DEFAULT 0,
                     created_at TEXT NOT NULL DEFAULT (datetime('now')),
                     updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                     session_id TEXT,
                     confidence REAL,
                     ctc_score REAL,
                     clipping_ratio REAL,
                     rms_db REAL,
                     snr_db REAL,
                     split TEXT,
                     signal_anomaly_score REAL,
                     verdict TEXT,
                     verdict_transcript TEXT,
                     rationale TEXT,
                     evidence_json TEXT,
                     agent_confidence REAL,
                     escalated INTEGER NOT NULL DEFAULT 0,
                     human_decision TEXT,
                     corrected_at TEXT,
                     is_gold INTEGER NOT NULL DEFAULT 0,
                     alignment_quality TEXT,
                     model_version_id TEXT NOT NULL DEFAULT 'unknown@pre-registry',
                     confidence_source TEXT NOT NULL DEFAULT 'unknown',
                     cloud_call INTEGER NOT NULL DEFAULT 0,
                     decoder_config_hash TEXT,
                     normalizer_version TEXT
                 ) STRICT;
                 INSERT INTO speech_segments_strict (
                     rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript,
                     alignment_json, duration_ms, speaker_id, verified, created_at, updated_at, session_id,
                     confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                     verdict, verdict_transcript, rationale, evidence_json, agent_confidence, escalated,
                     human_decision, corrected_at, is_gold, alignment_quality, model_version_id,
                     confidence_source, cloud_call, decoder_config_hash, normalizer_version)
                 SELECT
                     rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript,
                     alignment_json, duration_ms, speaker_id, verified, created_at, updated_at, session_id,
                     confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                     verdict, verdict_transcript, rationale, evidence_json, agent_confidence, escalated,
                     human_decision, corrected_at, is_gold, alignment_quality, model_version_id,
                     confidence_source, cloud_call, decoder_config_hash, normalizer_version
                 FROM speech_segments;
                 DROP TABLE speech_segments;
                 ALTER TABLE speech_segments_strict RENAME TO speech_segments;
                 CREATE INDEX IF NOT EXISTS idx_segments_verified ON speech_segments(verified);
                 CREATE INDEX IF NOT EXISTS idx_segments_speaker ON speech_segments(speaker_id);
                 CREATE INDEX IF NOT EXISTS idx_segments_created ON speech_segments(created_at);
                 CREATE INDEX IF NOT EXISTS idx_segments_verdict ON speech_segments(verdict);
                 CREATE INDEX IF NOT EXISTS idx_segments_escalated ON speech_segments(escalated);
                 CREATE INDEX IF NOT EXISTS idx_segments_audio_path ON speech_segments(audio_path);
                 CREATE INDEX IF NOT EXISTS idx_segments_verified_created ON speech_segments(verified, created_at);
                 CREATE INDEX IF NOT EXISTS idx_segments_human_decision ON speech_segments(human_decision);
                 CREATE INDEX IF NOT EXISTS idx_segments_cloud_call ON speech_segments(cloud_call);
                 CREATE INDEX IF NOT EXISTS idx_segments_confidence_source ON speech_segments(confidence_source);
                 CREATE TRIGGER IF NOT EXISTS segments_ai AFTER INSERT ON speech_segments BEGIN
                     INSERT INTO segments_fts(rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                     VALUES (new.rowid, new.id, new.audio_path, new.raw_transcript, new.normalized_transcript, new.annotated_transcript);
                 END;
                 CREATE TRIGGER IF NOT EXISTS segments_ad AFTER DELETE ON speech_segments BEGIN
                     INSERT INTO segments_fts(segments_fts, rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                     VALUES ('delete', old.rowid, old.id, old.audio_path, old.raw_transcript, old.normalized_transcript, old.annotated_transcript);
                 END;
                 CREATE TRIGGER IF NOT EXISTS segments_au AFTER UPDATE ON speech_segments BEGIN
                     INSERT INTO segments_fts(segments_fts, rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                     VALUES ('delete', old.rowid, old.id, old.audio_path, old.raw_transcript, old.normalized_transcript, old.annotated_transcript);
                     INSERT INTO segments_fts(rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                     VALUES (new.rowid, new.id, new.audio_path, new.raw_transcript, new.normalized_transcript, new.annotated_transcript);
                 END;
                 INSERT INTO segments_fts(segments_fts) VALUES('rebuild');",
        // Down: the same recreate without STRICT. Also runs in the FK-off window (see rollback()).
        down_sql: Some(
            "CREATE TABLE speech_segments_nonstrict (
                 id TEXT PRIMARY KEY,
                 audio_path TEXT NOT NULL,
                 raw_transcript TEXT NOT NULL DEFAULT '',
                 normalized_transcript TEXT,
                 annotated_transcript TEXT,
                 alignment_json TEXT,
                 duration_ms INTEGER NOT NULL DEFAULT 0,
                 speaker_id TEXT,
                 verified INTEGER NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                 session_id TEXT,
                 confidence REAL,
                 ctc_score REAL,
                 clipping_ratio REAL,
                 rms_db REAL,
                 snr_db REAL,
                 split TEXT,
                 signal_anomaly_score REAL,
                 verdict TEXT,
                 verdict_transcript TEXT,
                 rationale TEXT,
                 evidence_json TEXT,
                 agent_confidence REAL,
                 escalated INTEGER NOT NULL DEFAULT 0,
                 human_decision TEXT,
                 corrected_at TEXT,
                 is_gold INTEGER NOT NULL DEFAULT 0,
                 alignment_quality TEXT,
                 model_version_id TEXT NOT NULL DEFAULT 'unknown@pre-registry',
                 confidence_source TEXT NOT NULL DEFAULT 'unknown',
                 cloud_call INTEGER NOT NULL DEFAULT 0,
                 decoder_config_hash TEXT,
                 normalizer_version TEXT
             );
             INSERT INTO speech_segments_nonstrict (
                 rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript,
                 alignment_json, duration_ms, speaker_id, verified, created_at, updated_at, session_id,
                 confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                 verdict, verdict_transcript, rationale, evidence_json, agent_confidence, escalated,
                 human_decision, corrected_at, is_gold, alignment_quality, model_version_id,
                 confidence_source, cloud_call, decoder_config_hash, normalizer_version)
             SELECT
                 rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript,
                 alignment_json, duration_ms, speaker_id, verified, created_at, updated_at, session_id,
                 confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, signal_anomaly_score,
                 verdict, verdict_transcript, rationale, evidence_json, agent_confidence, escalated,
                 human_decision, corrected_at, is_gold, alignment_quality, model_version_id,
                 confidence_source, cloud_call, decoder_config_hash, normalizer_version
             FROM speech_segments;
             DROP TABLE speech_segments;
             ALTER TABLE speech_segments_nonstrict RENAME TO speech_segments;
             CREATE INDEX IF NOT EXISTS idx_segments_verified ON speech_segments(verified);
             CREATE INDEX IF NOT EXISTS idx_segments_speaker ON speech_segments(speaker_id);
             CREATE INDEX IF NOT EXISTS idx_segments_created ON speech_segments(created_at);
             CREATE INDEX IF NOT EXISTS idx_segments_verdict ON speech_segments(verdict);
             CREATE INDEX IF NOT EXISTS idx_segments_escalated ON speech_segments(escalated);
             CREATE INDEX IF NOT EXISTS idx_segments_audio_path ON speech_segments(audio_path);
             CREATE INDEX IF NOT EXISTS idx_segments_verified_created ON speech_segments(verified, created_at);
             CREATE INDEX IF NOT EXISTS idx_segments_human_decision ON speech_segments(human_decision);
             CREATE INDEX IF NOT EXISTS idx_segments_cloud_call ON speech_segments(cloud_call);
             CREATE INDEX IF NOT EXISTS idx_segments_confidence_source ON speech_segments(confidence_source);
             CREATE TRIGGER IF NOT EXISTS segments_ai AFTER INSERT ON speech_segments BEGIN
                 INSERT INTO segments_fts(rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                 VALUES (new.rowid, new.id, new.audio_path, new.raw_transcript, new.normalized_transcript, new.annotated_transcript);
             END;
             CREATE TRIGGER IF NOT EXISTS segments_ad AFTER DELETE ON speech_segments BEGIN
                 INSERT INTO segments_fts(segments_fts, rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                 VALUES ('delete', old.rowid, old.id, old.audio_path, old.raw_transcript, old.normalized_transcript, old.annotated_transcript);
             END;
             CREATE TRIGGER IF NOT EXISTS segments_au AFTER UPDATE ON speech_segments BEGIN
                 INSERT INTO segments_fts(segments_fts, rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                 VALUES ('delete', old.rowid, old.id, old.audio_path, old.raw_transcript, old.normalized_transcript, old.annotated_transcript);
                 INSERT INTO segments_fts(rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                 VALUES (new.rowid, new.id, new.audio_path, new.raw_transcript, new.normalized_transcript, new.annotated_transcript);
             END;
             INSERT INTO segments_fts(segments_fts) VALUES('rebuild');",
        ),
    },
    Migration {
        version: 41,
        description: "Per-segment processing provenance: whether denoising/diarization actually ran (P0.4)",
        // H3 fix (docs/ROADMAP_TO_NUMBER_ONE.md): the export manifest's runConfig stamps a single
        // denoising/diarization flag computed from EXPORT-DAY model loadability onto every segment,
        // regardless of what actually processed each clip at import. These per-row columns store the
        // real outcome so a later export can read stored truth instead of recomputing from today's state.
        //
        // Nullable INTEGER (0/1): NULL = "not recorded" — every row imported before this migration, where
        // we genuinely did not capture whether the denoiser/CAM++ ran; asserting a fabricated 0 would be
        // its own provenance lie. New rows record `settings.enable_X && <model actually loadable>` at the
        // single import construction site (pipeline.rs build_segments_from_pcm). STRICT-compatible
        // (INTEGER); ALTER ADD COLUMN does not drop/recreate speech_segments, so it fires no FK cascade
        // (unlike the v40 STRICT recreate) and needs no FK-off window.
        up_sql: "ALTER TABLE speech_segments ADD COLUMN denoised INTEGER;
                 ALTER TABLE speech_segments ADD COLUMN diarized INTEGER;",
        down_sql: Some(
            "ALTER TABLE speech_segments DROP COLUMN diarized;
             ALTER TABLE speech_segments DROP COLUMN denoised;",
        ),
    },
    Migration {
        version: 42,
        description: "Per-segment VAD backend provenance: which detector produced each region (P0.4)",
        // Completes the P0.4 per-segment processing provenance (denoised/diarized landed in v41). Records
        // the VAD backend that ACTUALLY produced each segment's region — "silero", "energy" (fallback), or
        // "none" (short file taken whole). NULL = not recorded (legacy row / cloud Scribe path). Nullable
        // TEXT, STRICT-compatible; ADD COLUMN fires no FK cascade, so no FK-off window.
        up_sql: "ALTER TABLE speech_segments ADD COLUMN vad_backend TEXT;",
        down_sql: Some("ALTER TABLE speech_segments DROP COLUMN vad_backend;"),
    },
    Migration {
        version: 43,
        description: "Reviewer attribution: WHICH human made the decision on each row (multi-reviewer Couch Review)",
        // Couch Review became multi-reviewer: several named people can review from their own phones at
        // once, each on their own token. Without attribution every decision lands anonymous, so the
        // corpus cannot answer "who labelled this?" — which is both an audit gap and the missing
        // substrate for inter-annotator agreement (a decision you cannot attribute cannot be compared
        // against a second opinion).
        //
        // `reviewed_by` is the author of the row's CURRENT human decision, written in the same
        // transaction as the verdict so the two can never diverge.
        //
        // NULL means "not attributed": every pre-v43 row, and every decision made at the owner's own
        // desktop (where there is exactly one human and no token to name them). Writing a fabricated
        // "owner" onto legacy rows would assert provenance we never captured. Nullable TEXT is
        // STRICT-compatible, and ADD COLUMN fires no FK cascade, so no FK-off window is needed.
        //
        // Deliberately ONE column, on speech_segments only. A parallel `decision_log.annotator` was
        // written and removed: decision_log rows exist only for decisions carrying a timestamp_ms, which
        // the phone path does not send, so the column could never hold anything but NULL — and unlike
        // speech_segments (which v40 recreates), a second ALTER on an untouched table also breaks the
        // migration-replay tests. Per-decision annotator history is the right substrate for an
        // inter-annotator agreement study, but that needs MULTIPLE decisions per segment, which this
        // one-row-per-segment schema cannot express; it is not faked with an always-NULL column here.
        up_sql: "ALTER TABLE speech_segments ADD COLUMN reviewed_by TEXT;",
        down_sql: Some("ALTER TABLE speech_segments DROP COLUMN reviewed_by;"),
    },
    Migration {
        version: 44,
        description: "Spot-check results: how each remote reviewer scored on clips whose answer is already known",
        // docs/REMOTE_REVIEW_PLAN.md §2.1. Once review is handed to other people, the dominant risk
        // stops being a crash and becomes a human tapping "accept" without listening. Every gate in
        // this repo measures whether the MACHINE is honest; none measured whether the REVIEWER was.
        //
        // A small share of each reviewer's queue is silently drawn from clips that already carry a
        // human-verified transcript, served with the RAW (known-wrong) draft. A reviewer who listens
        // corrects it; one who taps accept does not. The result lands HERE and never touches
        // speech_segments — grading a reviewer must not be able to alter the corpus it grades against.
        //
        // PRIMARY KEY (segment_id, reviewer) so a network retry upserts its own row instead of
        // inflating someone's score with duplicates. `noticed` is the blind-accept signal (did they
        // change the draft at all); `cer` is how close the correction landed to the known answer.
        up_sql: "CREATE TABLE IF NOT EXISTS spot_checks (
                     segment_id TEXT NOT NULL,
                     reviewer TEXT NOT NULL,
                     action TEXT NOT NULL,
                     submitted_transcript TEXT NOT NULL,
                     expected_transcript TEXT NOT NULL,
                     noticed INTEGER NOT NULL,
                     cer REAL NOT NULL,
                     created_at TEXT NOT NULL DEFAULT (datetime('now')),
                     PRIMARY KEY (segment_id, reviewer),
                     FOREIGN KEY(segment_id) REFERENCES speech_segments(id) ON DELETE CASCADE
                 ) STRICT;
                 CREATE INDEX IF NOT EXISTS idx_spot_checks_reviewer ON spot_checks(reviewer);",
        down_sql: Some("DROP INDEX IF EXISTS idx_spot_checks_reviewer; DROP TABLE IF EXISTS spot_checks;"),
    },
    Migration {
        version: 45,
        description: "Append-only review events: who decided what, when — per-reviewer throughput and audit trail",
        // docs/REMOTE_REVIEW_PLAN.md §2.2 + §2.3, deliberately ONE table rather than two changes.
        //
        // WHY NOT `decision_log`, which already looks like the right home:
        //   1. It only gets a row when a decision carries a `timestamp_ms`, and the phone path passes
        //      None — so phone reviews are invisible to it today.
        //   2. It has no reviewer column, and ADDING one is NOT migration-replay-safe: v40 recreates
        //      speech_segments (which is why v41/v42's ALTERs survive re-application) but nothing
        //      recreates decision_log, so a bare ALTER breaks three existing replay tests. Measured,
        //      not assumed — it broke them.
        //   3. `stats.rs` computes its median seconds-per-decision over a GLOBALLY ordered
        //      decision_log. Feeding concurrent reviewers into that would count the gap between two
        //      DIFFERENT people's decisions as one person's pace, making a shipped number look
        //      artificially fast. This table keeps that metric untouched and partitions per reviewer.
        //
        // WHY NOT the existing `corrections` ledger: it records before/after text only for EDITS, and
        // only when the audio identity resolves (best-effort `.ok()`), so a moved file leaves no row.
        // It also does not record who. None of the three existing tables answers "who decided this,
        // and when" — which is exactly what an audit trail is.
        //
        // Append-only by intent: no UPDATE path exists. A retry writes a second row with the same
        // (segment, reviewer) and that is CORRECT for an audit trail — the history is the point, and
        // the throughput query counts distinct segments rather than rows.
        //
        // Deliberately NO foreign key to speech_segments, unlike spot_checks. An audit trail whose
        // rows vanish when the audited row is deleted is not an audit trail: "who reviewed the clip
        // that was later removed" is precisely the question it has to survive to answer.
        up_sql: "CREATE TABLE IF NOT EXISTS review_events (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     segment_id TEXT NOT NULL,
                     reviewer TEXT NOT NULL,
                     action TEXT NOT NULL,
                     source TEXT NOT NULL,
                     timestamp_ms INTEGER NOT NULL,
                     created_at TEXT NOT NULL DEFAULT (datetime('now'))
                 ) STRICT;
                 CREATE INDEX IF NOT EXISTS idx_review_events_reviewer ON review_events(reviewer, timestamp_ms);",
        down_sql: Some("DROP INDEX IF EXISTS idx_review_events_reviewer; DROP TABLE IF EXISTS review_events;"),
    },
    Migration {
        version: 46,
        description: "Spot-check scores survive deleting the clip they were measured on (audit trail, not clip data)",
        // v45 wrote the principle down — "an audit trail whose rows vanish when the audited row is
        // deleted is not an audit trail" — and even said "unlike spot_checks". It then left
        // spot_checks alone. This carries the reasoning across.
        //
        // A spot-check row is not data ABOUT a clip; it is the record of what a REVIEWER did on it:
        // whether they listened or blind-accepted. ON DELETE CASCADE made that record a property of
        // the clip, with two quiet consequences. Tidying up unrelated clips retroactively changed a
        // reviewer's score — a number that moves when you delete something else is not a record. And
        // delete+undo, an operation history/mod.rs works hard to make lossless, destroyed it outright:
        // undo restores the segment row and cannot resurrect the cascaded children. Proven by
        // `history::tests::deleting_a_clip_must_not_erase_the_record_of_how_reviewers_scored_on_it`.
        //
        // No FK_OFF needed, unlike v40: spot_checks is a LEAF. `ON DELETE CASCADE` fires when the
        // PARENT is deleted, so dropping the child table itself cascades nothing.
        //
        // Replay-safe: on re-application the table already has no FK, so this copies it to itself and
        // renames back. `INSERT OR IGNORE` because (segment_id, reviewer) stays the primary key.
        up_sql: "CREATE TABLE IF NOT EXISTS spot_checks_no_fk (
                     segment_id TEXT NOT NULL,
                     reviewer TEXT NOT NULL,
                     action TEXT NOT NULL,
                     submitted_transcript TEXT NOT NULL,
                     expected_transcript TEXT NOT NULL,
                     noticed INTEGER NOT NULL,
                     cer REAL NOT NULL,
                     created_at TEXT NOT NULL DEFAULT (datetime('now')),
                     PRIMARY KEY (segment_id, reviewer)
                 ) STRICT;
                 INSERT OR IGNORE INTO spot_checks_no_fk
                     (segment_id, reviewer, action, submitted_transcript, expected_transcript, noticed, cer, created_at)
                     SELECT segment_id, reviewer, action, submitted_transcript, expected_transcript, noticed, cer, created_at
                     FROM spot_checks;
                 DROP TABLE spot_checks;
                 ALTER TABLE spot_checks_no_fk RENAME TO spot_checks;
                 CREATE INDEX IF NOT EXISTS idx_spot_checks_reviewer ON spot_checks(reviewer);",
        // The mirror image, so the migration set stays round-trip safe. Note what going BACK costs:
        // the FK schema cannot represent a score whose clip has been deleted, so those rows are
        // dropped here rather than failing the rollback. That asymmetry is the bug this migration
        // fixes, stated in SQL — restoring the constraint means restoring the data loss.
        down_sql: Some(
            "CREATE TABLE IF NOT EXISTS spot_checks_fk (
                 segment_id TEXT NOT NULL,
                 reviewer TEXT NOT NULL,
                 action TEXT NOT NULL,
                 submitted_transcript TEXT NOT NULL,
                 expected_transcript TEXT NOT NULL,
                 noticed INTEGER NOT NULL,
                 cer REAL NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 PRIMARY KEY (segment_id, reviewer),
                 FOREIGN KEY(segment_id) REFERENCES speech_segments(id) ON DELETE CASCADE
             ) STRICT;
             INSERT OR IGNORE INTO spot_checks_fk
                 (segment_id, reviewer, action, submitted_transcript, expected_transcript, noticed, cer, created_at)
                 SELECT segment_id, reviewer, action, submitted_transcript, expected_transcript, noticed, cer, created_at
                 FROM spot_checks WHERE segment_id IN (SELECT id FROM speech_segments);
             DROP TABLE spot_checks;
             ALTER TABLE spot_checks_fk RENAME TO spot_checks;
             CREATE INDEX IF NOT EXISTS idx_spot_checks_reviewer ON spot_checks(reviewer);",
        ),
    },
    Migration {
        version: 47,
        description: "Within-clip speaker-change score, so a clip holding two speakers can be flagged",
        // Chunk boundaries are planned by SILENCE alone and the speaker label is attached to the whole
        // chunk afterwards — so a clip spanning a turn between two people still carries exactly one
        // authoritative SPEAKER_xx, in the DB and in every export column. 17 of the owner's 144 clips
        // are like that (`src/bin/speaker_change_probe.rs`, calibrated against his own blind listening
        // pass). Until now that measurement lived only in the probe's console output: nothing on the
        // row said so, so a reviewer meeting one on the phone had no way to know before accepting it.
        //
        // The SCORE is stored, not a boolean, for the same reason `snr_db` and `clipping_ratio` are
        // numbers: the threshold is a calibration that can be re-derived, and a stored verdict would
        // freeze today's 0.59 into the data. Readers compare against
        // `diarization::SPEAKER_CHANGE_THRESHOLD`, which is where the calibration is documented.
        //
        // NULL = NOT MEASURED, and that distinction matters: it must never read as "measured, one
        // speaker". Every pre-v47 row is NULL, and so is every future import — the import path does
        // not run this measurement (two extra CAM++ embeddings per chunk), it is filled by the probe.
        //
        // Nullable REAL is STRICT-compatible, and ADD COLUMN fires no FK cascade, so no FK-off window.
        up_sql: "ALTER TABLE speech_segments ADD COLUMN speaker_change_score REAL;",
        down_sql: Some("ALTER TABLE speech_segments DROP COLUMN speaker_change_score;"),
    },
    Migration {
        version: 48,
        description: "Keep the machine's verdict text where a human decision cannot overwrite it",
        // `verdict_transcript` holds whichever verdict is CURRENT: the machine writes it, and then
        // `record_human_decision_by` overwrites it with the reviewer's correction. `corrections.rs`
        // states the consequence as settled fact — "verdict_transcript ... is the human's ANSWER ...
        // never the model draft" — and that is exactly why the label-quality lift could never work:
        // `load_lift_triples` passed that column as the JURY hypothesis, so it compared the human's
        // answer with the human's answer and returned zero on every decided row, forever. Measured on
        // the owner's library 2026-08-04: 39 of 39 scored rows self-referential, INCLUDING 34 of the 35
        // clips he had edited.
        //
        // This column is written ONLY by `write_segment_verdict` (the machine) and never by any human
        // path, so the two texts stay distinguishable and the lift finally has an independent side.
        //
        // NO BACKFILL, deliberately. The pre-existing machine verdicts are not recoverable: on decided
        // rows they were overwritten, and on the 77 undecided rows `verdict_transcript` is EMPTY because
        // every clip in this library carries `T1_ESCALATE` — the jury escalated all 144 and never
        // committed a verdict of its own. Copying the current column here would therefore either
        // duplicate the human's answer (re-creating the exact defect) or copy nothing. NULL means
        // "no machine verdict recorded", which is the truth for every existing row.
        //
        // Nullable TEXT is STRICT-compatible, and ADD COLUMN fires no FK cascade, so no FK-off window.
        up_sql: "ALTER TABLE speech_segments ADD COLUMN jury_transcript TEXT;",
        down_sql: Some("ALTER TABLE speech_segments DROP COLUMN jury_transcript;"),
    },
    Migration {
        version: 49,
        description: "Per-recording rights: license, consent basis, permitted use, provenance, revocation",
        // Deep-audit #6. Until now the ONLY `license` column in this schema was on `model_versions` —
        // rights for the AUDIO were tracked at dataset level (ATTRIBUTION.md, the export license gate)
        // and nowhere per recording. A voice recording is Article 9 biometric data: the lawful basis,
        // the permitted use and the ability to honour a withdrawal all attach to the INDIVIDUAL
        // recording, and none of that is expressible in a repo-level markdown file.
        //
        // STORED PER SEGMENT, though the semantics are per RECORDING. Two reasons, both practical:
        // a join keyed on `audio_path` orphans the moment `relink_audio` rewrites that path (the row
        // keeps its rights here, automatically), and the export iterates segments, so enforcement needs
        // the values on the row it is deciding about. `set_recording_rights` writes every segment
        // sharing an audio_path in one statement, so the per-recording semantics are preserved at the
        // API even though storage is per row.
        //
        // NO BACKFILL and no defaults, deliberately. Every existing row becomes rights-UNKNOWN, which
        // is the truth: this library's provenance was never recorded per clip, and inventing
        // "CC-BY-4.0" for 144 clips because the eval corpus happened to be CC-BY would be exactly the
        // fabricated-provenance lie the honesty law forbids. Unknown blocks REDISTRIBUTION and permits
        // local personal export, which is the honest gradient — see `rights_disposition_for_segment`.
        //
        // `rights_revoked_at` is the revocation lineage: non-NULL means a withdrawal was recorded, and
        // that outranks every other field on every path, including local export. A withdrawal that only
        // stops future publishes is not a withdrawal.
        //
        // Nullable TEXT throughout: STRICT-compatible, and ADD COLUMN fires no FK cascade.
        up_sql: "ALTER TABLE speech_segments ADD COLUMN rights_license TEXT;\n\
                 ALTER TABLE speech_segments ADD COLUMN rights_consent_basis TEXT;\n\
                 ALTER TABLE speech_segments ADD COLUMN rights_permitted_use TEXT;\n\
                 ALTER TABLE speech_segments ADD COLUMN rights_attribution TEXT;\n\
                 ALTER TABLE speech_segments ADD COLUMN rights_source TEXT;\n\
                 ALTER TABLE speech_segments ADD COLUMN rights_revoked_at TEXT;",
        down_sql: Some(
            "ALTER TABLE speech_segments DROP COLUMN rights_license;\n\
             ALTER TABLE speech_segments DROP COLUMN rights_consent_basis;\n\
             ALTER TABLE speech_segments DROP COLUMN rights_permitted_use;\n\
             ALTER TABLE speech_segments DROP COLUMN rights_attribution;\n\
             ALTER TABLE speech_segments DROP COLUMN rights_source;\n\
             ALTER TABLE speech_segments DROP COLUMN rights_revoked_at;",
        ),
    },
    Migration {
        version: 50,
        description: "Durable audio fingerprint per segment so duplicate detection survives a restart",
        // External review 2026-08-06 #4. `AudioFingerprint` was an in-memory Mutex<HashMap> built empty
        // by lib.rs at every launch, and NO migration had ever created a column to rehydrate it from —
        // so `check_and_register` compared only against files imported in the SAME run. Restart the app,
        // re-import the same audio under a different path, and it was accepted silently as new content.
        //
        // The value was already being computed at the right moment and thrown away: pipeline.rs read
        // `let _fp = self.fingerprint.check_and_register(...)`. This column gives it somewhere to live.
        //
        // INTEGER, not TEXT: the fingerprint is a u64 spectral hash. SQLite integers are i64, so it is
        // stored as `fp as i64` and read back with `as u64` — a lossless bit-cast in both directions,
        // NOT a numeric conversion, so the top bit round-trips.
        //
        // NO BACKFILL, exactly as v49. Computing a fingerprint requires DECODING the audio, which is
        // not something a schema migration may do — and inventing a value would be worse than NULL.
        // Existing rows are honestly unknown until the backfill pass runs over them; a NULL simply does
        // not participate in dedup, which is the same protection those rows have today.
        //
        // The index is partial: only non-NULL rows are dedup candidates, so a library of legacy NULLs
        // costs nothing to carry.
        up_sql: "ALTER TABLE speech_segments ADD COLUMN audio_fingerprint INTEGER;
                 CREATE INDEX IF NOT EXISTS idx_segments_audio_fingerprint
                     ON speech_segments(audio_fingerprint) WHERE audio_fingerprint IS NOT NULL;",
        down_sql: Some(
            "DROP INDEX IF EXISTS idx_segments_audio_fingerprint;
             ALTER TABLE speech_segments DROP COLUMN audio_fingerprint;",
        ),
    },
    Migration {
        version: 51,
        description: "Cryptographic content hash per recording — the DEFINITIVE duplicate key (v50's spectral value is demoted to a candidate index)",
        // External review 2026-08-06 P1.1. v50 made the spectral fingerprint durable, which fixed its
        // TIME scope but not its SEMANTICS: a 64-bit fold of eight band energies was still being used as
        // a definitive content key, so a collision returned Err("Duplicate audio content") and REFUSED a
        // legitimate recording at import. Losing real speech to defend against a duplicate that is not
        // one is the wrong trade for a dataset tool.
        //
        // This column holds blake3 over canonical decoded PCM + sample rate. From v51 on, a rejection
        // requires a match HERE; audio_fingerprint only decides which rows are worth comparing.
        //
        // TEXT, not BLOB: a 64-char hex digest is greppable in a sqlite3 shell and in an export, and the
        // 32 bytes saved per recording are irrelevant next to the audio itself.
        //
        // NO BACKFILL, exactly as v49 and v50 — computing this requires DECODING the audio, which a
        // schema migration may not do. A NULL here means "content never hashed", and the map treats it
        // as unable to prove identity, so a pre-v51 row can never cause a rejection until
        // `backfill_fingerprints` writes its real hash. That is a deliberate, narrow LOSS of dedup
        // coverage for legacy rows, chosen because the alternative is keeping the false-reject bug for
        // them. Prefer a duplicate over discarding legitimate audio.
        //
        // Partial index for the same reason as v50: only non-NULL rows are dedup candidates.
        up_sql: "ALTER TABLE speech_segments ADD COLUMN audio_content_hash TEXT;
                 CREATE INDEX IF NOT EXISTS idx_segments_audio_content_hash
                     ON speech_segments(audio_content_hash) WHERE audio_content_hash IS NOT NULL;",
        down_sql: Some(
            "DROP INDEX IF EXISTS idx_segments_audio_content_hash;
             ALTER TABLE speech_segments DROP COLUMN audio_content_hash;",
        ),
    },
    Migration {
        version: 52,
        description: "Rename agent_confidence -> agreement_score: the number is AGREEMENT, never correctness",
        // External review 2026-08-06 P1.2: "`agent_confidence` is agreement in some flows, while the UI
        // and older code can still read 'confidence' as correctness."
        //
        // Those are opposite claims on bad audio. Every recognizer can confidently agree on the same
        // garbage — which is precisely why has_hard_distrust_veto refuses to auto-accept such a clip —
        // so a HIGH value here is compatible with a completely wrong transcript. A field named
        // "confidence" invites exactly the reading the jury already rejected, and it invited it once
        // already: 6028824 had to stop the review UI rendering this as a green confidence badge.
        //
        // The name now states what the number IS, so the next reader cannot make that inference from
        // the schema alone.
        //
        // RENAME COLUMN, not add-copy-drop: it preserves the data, the column's position (so
        // SEGMENT_SELECT_COLUMNS' index-based map_row is unaffected), and every value's history. SQLite
        // has supported it since 3.25 and rusqlite bundles far newer.
        //
        // The earlier migrations that CREATE and re-list `agent_confidence` (v11, and the two table
        // recreates) are deliberately left alone. They are history: they describe what actually ran on
        // this database. Editing them would make the chain describe a past that did not happen, and a
        // database already at v51 would never replay them anyway.
        //
        // No index references this column, so nothing else moves.
        up_sql: "ALTER TABLE speech_segments RENAME COLUMN agent_confidence TO agreement_score;",
        down_sql: Some("ALTER TABLE speech_segments RENAME COLUMN agreement_score TO agent_confidence;"),
    },
    Migration {
        version: 53,
        description: "Monotonic speech-segment revision for atomic remote-review compare-and-swap",
        // `updated_at` has one-second resolution, so two real writes in the same second can carry the
        // same value. Couch Review used it as a serve/decide/undo fence, which made those writes
        // invisible and also left a check-then-write TOCTOU window. A database-owned integer revision
        // changes on EVERY row update, independent of which write path performed it, and can therefore
        // be used in the UPDATE's own WHERE clause.
        //
        // The old FTS update trigger ran for every metadata-only update. The revision trigger performs
        // one such metadata update itself, so narrow the FTS trigger to the four columns it actually
        // indexes before installing the revision trigger. This avoids duplicate FTS delete/insert work
        // while preserving the exact external-content index semantics.
        up_sql: "ALTER TABLE speech_segments ADD COLUMN review_revision INTEGER NOT NULL DEFAULT 0;
                 DROP TRIGGER IF EXISTS segments_au;
                 CREATE TRIGGER segments_au AFTER UPDATE OF
                     audio_path, raw_transcript, normalized_transcript, annotated_transcript
                 ON speech_segments BEGIN
                     INSERT INTO segments_fts(segments_fts, rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                     VALUES ('delete', old.rowid, old.id, old.audio_path, old.raw_transcript, old.normalized_transcript, old.annotated_transcript);
                     INSERT INTO segments_fts(rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                     VALUES (new.rowid, new.id, new.audio_path, new.raw_transcript, new.normalized_transcript, new.annotated_transcript);
                 END;
                 CREATE TRIGGER speech_segments_review_revision
                 AFTER UPDATE ON speech_segments
                 WHEN new.review_revision = old.review_revision
                 BEGIN
                     UPDATE speech_segments
                     SET review_revision = old.review_revision + 1
                     WHERE id = old.id;
                 END;",
        down_sql: Some(
            "DROP TRIGGER IF EXISTS speech_segments_review_revision;
             DROP TRIGGER IF EXISTS segments_au;
             CREATE TRIGGER segments_au AFTER UPDATE ON speech_segments BEGIN
                 INSERT INTO segments_fts(segments_fts, rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                 VALUES ('delete', old.rowid, old.id, old.audio_path, old.raw_transcript, old.normalized_transcript, old.annotated_transcript);
                 INSERT INTO segments_fts(rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                 VALUES (new.rowid, new.id, new.audio_path, new.raw_transcript, new.normalized_transcript, new.annotated_transcript);
             END;
             ALTER TABLE speech_segments DROP COLUMN review_revision;",
        ),
    },
    Migration {
        version: 54,
        description: "Declare source audio that was PROCESSED before import, so an export can never call it original",
        // 2026-08-17. The owner's pre-import cleaner (kurdish-audio-cleaner) runs a neural separator
        // over a recording, CUTS OUT every non-speech stretch, re-concatenates what survives with
        // 150 ms pauses, and normalises the level. The result is a WAV — indistinguishable by
        // inspection from an original recording, and until now indistinguishable in this database
        // too. Importing that corpus would have described machine-separated audio in exactly the
        // words used for a raw field recording, which is the one thing this project's honesty law
        // does not permit.
        //
        // Keyed by SOURCE PATH, not per segment, for three reasons: the processing is a property of
        // the recording (all ~500 clips cut from one file share it), it keeps `speech_segments`'
        // column layout — and therefore SEGMENT_SELECT_COLUMNS' index-based map_row — untouched, and
        // a source can be declared before or after its clips exist.
        //
        // `processing` is a human-readable declaration of what was done; `manifest_path` points at
        // the cleaner's own manifest.json, which holds the full parameter set. NULL/absent row means
        // exactly one thing: nothing has claimed this recording was processed.
        up_sql: "CREATE TABLE IF NOT EXISTS source_audio_provenance (
                     audio_path TEXT PRIMARY KEY,
                     processing TEXT NOT NULL,
                     separator_model TEXT,
                     timeline_preserved INTEGER NOT NULL DEFAULT 0,
                     manifest_path TEXT,
                     recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
                 );",
        down_sql: Some("DROP TABLE IF EXISTS source_audio_provenance;"),
    },
    Migration {
        version: 55,
        description: "Record that a reviewer actually HEARD a clip, so a verdict can be refused without it",
        // 2026-08-19. Until now the decision surfaces gated only on `audioError` — the ABSENCE of a
        // failure, which is not the presence of listening. A clip whose audio never loaded, or loaded
        // and was never played, was indistinguishable from one the reviewer listened to twice. For a
        // verbatim corpus that is the difference between a label and a guess, and it is invisible
        // afterwards: nothing in the row says whether anyone heard it.
        //
        // A receipt is per (segment, revision): re-review after a correction needs its OWN evidence,
        // because the text under judgement changed. `audio_fingerprint` binds the receipt to the
        // BYTES that were played, so a receipt cannot be replayed against a different clip or survive
        // the audio being swapped underneath it. `played_ms` is cumulative MEDIA time actually
        // advanced — not wall-clock, not a play() call, and not a download — so seeking, pausing and
        // replaying all account honestly.
        //
        // `policy_version` is stored per row on purpose: the sufficiency rule will be tuned, and a
        // receipt must always say which rule it satisfied rather than being re-judged under a later
        // one it never met.
        up_sql: "CREATE TABLE IF NOT EXISTS playback_receipts (
                     id                INTEGER PRIMARY KEY AUTOINCREMENT,
                     segment_id        TEXT NOT NULL,
                     segment_revision  INTEGER NOT NULL,
                     audio_fingerprint TEXT NOT NULL,
                     reviewer          TEXT,
                     session_id        TEXT,
                     started_at_ms     INTEGER NOT NULL,
                     played_ms         INTEGER NOT NULL,
                     clip_duration_ms  INTEGER NOT NULL,
                     coverage_ratio    REAL NOT NULL,
                     policy_version    INTEGER NOT NULL,
                     created_at        TEXT NOT NULL DEFAULT (datetime('now')),
                     FOREIGN KEY (segment_id) REFERENCES speech_segments(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_playback_receipts_segment
                     ON playback_receipts(segment_id, segment_revision);",
        down_sql: Some("DROP INDEX IF EXISTS idx_playback_receipts_segment;
                        DROP TABLE IF EXISTS playback_receipts;"),
    },
    Migration {
        version: 56,
        description: "Snapshot each decision's audio length onto its audit event, so pay survives clip deletion",
        // 2026-08-20 hunt. `reviewed_audio_ms` — the phone's progress badge and the basis the owner
        // pays on — INNER JOINed speech_segments, so deleting a reviewed clip silently shrank a
        // reviewer's total for work they genuinely did. The audit trail kept the event (no FK, by
        // design); the pay metric lost the duration the moment the row died. The event now carries
        // the length it was paid against, backfilled from every clip that still exists; a legacy
        // event whose clip is already gone stays unpriceable rather than invented.
        up_sql: "ALTER TABLE review_events ADD COLUMN duration_ms INTEGER;
                 UPDATE review_events SET duration_ms =
                     (SELECT duration_ms FROM speech_segments s WHERE s.id = review_events.segment_id)
                 WHERE duration_ms IS NULL;",
        down_sql: Some("ALTER TABLE review_events DROP COLUMN duration_ms;"),
    },
    Migration {
        version: 57,
        description: "Version reviewer compensation and append immutable signed ledger entries",
        // Owner authorization 2026-08-21: edit 100%, unchanged accept 10%, valid reject 10%, skip 0%
        // at the existing 18,000 IQD/full-equivalent-hour rate. The policy starts AFTER the last
        // legacy event present when this migration lands: historical rows do not preserve the
        // semantic action the reviewer performed, and silently repricing them would invent payroll.
        //
        // `review_events.action` remains the effective CORPUS/provenance decision. The new column
        // snapshots the distinct compensation action (e.g. an unchanged accept can be provenance-
        // reclassified to edit, while still earning the authorized accept rate).
        up_sql: "ALTER TABLE review_events ADD COLUMN compensation_action TEXT;
                 ALTER TABLE review_events ADD COLUMN operation_id TEXT;
                 ALTER TABLE review_events ADD COLUMN operation_payload_hash TEXT;
                 CREATE UNIQUE INDEX idx_review_events_operation_id
                     ON review_events(operation_id) WHERE operation_id IS NOT NULL;
                 CREATE TRIGGER review_event_operation_validate_insert
                 BEFORE INSERT ON review_events
                 WHEN (NEW.operation_id IS NULL) <> (NEW.operation_payload_hash IS NULL)
                   OR (NEW.operation_id IS NOT NULL AND (
                          TRIM(NEW.operation_id) = ''
                          OR LENGTH(NEW.operation_payload_hash) <> 64
                          OR NEW.operation_payload_hash GLOB '*[^0-9a-f]*'
                      ))
                 BEGIN SELECT RAISE(ABORT, 'review operation id/hash must be paired and canonical'); END;
                 CREATE TRIGGER review_event_operation_immutable_update
                 BEFORE UPDATE OF operation_id, operation_payload_hash ON review_events
                 WHEN NEW.operation_id IS NOT OLD.operation_id
                   OR NEW.operation_payload_hash IS NOT OLD.operation_payload_hash
                 BEGIN SELECT RAISE(ABORT, 'review operation identity is immutable'); END;

                 CREATE TABLE review_compensation_policies (
                     policy_version                 TEXT PRIMARY KEY,
                     effective_after_event_id       INTEGER NOT NULL CHECK(effective_after_event_id >= 0),
                     base_rate_micro_iqd_per_hour   INTEGER NOT NULL CHECK(base_rate_micro_iqd_per_hour > 0),
                     edit_basis_points              INTEGER NOT NULL CHECK(edit_basis_points BETWEEN 0 AND 10000),
                     accept_basis_points            INTEGER NOT NULL CHECK(accept_basis_points BETWEEN 0 AND 10000),
                     reject_basis_points            INTEGER NOT NULL CHECK(reject_basis_points BETWEEN 0 AND 10000),
                     skip_basis_points              INTEGER NOT NULL CHECK(skip_basis_points BETWEEN 0 AND 10000),
                     created_at                     TEXT NOT NULL DEFAULT (datetime('now'))
                 ) STRICT;
                 INSERT INTO review_compensation_policies
                     (policy_version, effective_after_event_id, base_rate_micro_iqd_per_hour,
                      edit_basis_points, accept_basis_points, reject_basis_points, skip_basis_points)
                 SELECT 'review-iqd-v1-2026-08-21', COALESCE(MAX(id), 0), 18000000000,
                        10000, 1000, 1000, 0
                   FROM review_events;

                 CREATE TABLE review_compensation_ledger (
                     id                         INTEGER PRIMARY KEY AUTOINCREMENT,
                     entry_id                   TEXT NOT NULL UNIQUE,
                     entry_key                  TEXT NOT NULL UNIQUE,
                     policy_version             TEXT NOT NULL,
                     review_event_id            INTEGER,
                     canonical_work_id          TEXT NOT NULL,
                     canonical_identity_kind    TEXT NOT NULL,
                     reviewer                   TEXT NOT NULL,
                     segment_id                 TEXT NOT NULL,
                     source                     TEXT NOT NULL,
                     compensation_action        TEXT NOT NULL
                                                    CHECK(compensation_action IN ('accept','edit','reject','skip','undo')),
                     effective_decision         TEXT NOT NULL,
                     decision_revision          INTEGER,
                     duration_ms                INTEGER NOT NULL CHECK(duration_ms >= 0),
                     rate_basis_points          INTEGER NOT NULL CHECK(rate_basis_points BETWEEN 0 AND 10000),
                     entitlement_micro_iqd      INTEGER NOT NULL CHECK(entitlement_micro_iqd >= 0),
                     delta_micro_iqd            INTEGER NOT NULL,
                     corrected_entitlement_ms   INTEGER NOT NULL CHECK(corrected_entitlement_ms >= 0),
                     delta_corrected_ms          INTEGER NOT NULL,
                     reverses_entry_id          TEXT,
                     created_at                 TEXT NOT NULL DEFAULT (datetime('now')),
                     FOREIGN KEY(policy_version) REFERENCES review_compensation_policies(policy_version),
                     FOREIGN KEY(review_event_id) REFERENCES review_events(id),
                     FOREIGN KEY(reverses_entry_id) REFERENCES review_compensation_ledger(entry_id)
                 ) STRICT;
                 CREATE UNIQUE INDEX idx_review_compensation_one_entry_per_event
                     ON review_compensation_ledger(review_event_id) WHERE review_event_id IS NOT NULL;
                 CREATE INDEX idx_review_compensation_reviewer
                     ON review_compensation_ledger(reviewer, policy_version, id);
                 CREATE INDEX idx_review_compensation_work
                     ON review_compensation_ledger(canonical_work_id, reviewer, policy_version, id);

                 CREATE TABLE review_compensation_settlements (
                     id                         INTEGER PRIMARY KEY AUTOINCREMENT,
                     settlement_id              TEXT NOT NULL UNIQUE,
                     policy_version             TEXT NOT NULL,
                     reviewer                   TEXT NOT NULL CHECK(TRIM(reviewer) <> ''),
                     from_ledger_id_exclusive   INTEGER NOT NULL CHECK(from_ledger_id_exclusive >= 0),
                     through_ledger_id_inclusive INTEGER NOT NULL
                                                    CHECK(through_ledger_id_inclusive > from_ledger_id_exclusive),
                     allocated_micro_iqd        INTEGER NOT NULL,
                     payout_reference           TEXT NOT NULL UNIQUE CHECK(TRIM(payout_reference) <> ''),
                     created_at                 TEXT NOT NULL DEFAULT (datetime('now')),
                     FOREIGN KEY(policy_version) REFERENCES review_compensation_policies(policy_version)
                 ) STRICT;
                 CREATE UNIQUE INDEX idx_review_compensation_settlement_boundary
                     ON review_compensation_settlements(policy_version, reviewer COLLATE NOCASE,
                                                        through_ledger_id_inclusive);

                 -- A settlement allocates one reviewer's next contiguous global-ledger interval.
                 -- The exact amount is recomputed from immutable entries at INSERT time, so retrying
                 -- or widening a payout range can neither pay the same delta twice nor invent money.
                 CREATE TRIGGER review_compensation_settlement_validate_insert
                 BEFORE INSERT ON review_compensation_settlements
                 WHEN NEW.from_ledger_id_exclusive <> COALESCE((
                          SELECT MAX(through_ledger_id_inclusive)
                            FROM review_compensation_settlements
                           WHERE policy_version = NEW.policy_version
                             AND reviewer = NEW.reviewer COLLATE NOCASE
                      ), 0)
                   OR NEW.through_ledger_id_inclusive > COALESCE((
                          SELECT MAX(id) FROM review_compensation_ledger
                           WHERE policy_version = NEW.policy_version
                      ), 0)
                   OR NOT EXISTS (
                          SELECT 1 FROM review_compensation_ledger
                           WHERE policy_version = NEW.policy_version
                             AND reviewer = NEW.reviewer COLLATE NOCASE
                             AND id > NEW.from_ledger_id_exclusive
                             AND id <= NEW.through_ledger_id_inclusive
                      )
                   OR NEW.allocated_micro_iqd <> COALESCE((
                          SELECT SUM(delta_micro_iqd) FROM review_compensation_ledger
                           WHERE policy_version = NEW.policy_version
                             AND reviewer = NEW.reviewer COLLATE NOCASE
                             AND id > NEW.from_ledger_id_exclusive
                             AND id <= NEW.through_ledger_id_inclusive
                      ), 0)
                 BEGIN SELECT RAISE(ABORT, 'review compensation settlement range/amount is invalid'); END;
                 CREATE TRIGGER review_compensation_settlement_immutable_update
                 BEFORE UPDATE ON review_compensation_settlements
                 BEGIN SELECT RAISE(ABORT, 'review compensation settlement is immutable'); END;
                 CREATE TRIGGER review_compensation_settlement_immutable_delete
                 BEFORE DELETE ON review_compensation_settlements
                 BEGIN SELECT RAISE(ABORT, 'review compensation settlement is immutable'); END;

                 CREATE TRIGGER review_compensation_policy_immutable_update
                 BEFORE UPDATE ON review_compensation_policies
                 BEGIN SELECT RAISE(ABORT, 'review compensation policy is immutable'); END;
                 CREATE TRIGGER review_compensation_policy_immutable_delete
                 BEFORE DELETE ON review_compensation_policies
                 BEGIN SELECT RAISE(ABORT, 'review compensation policy is immutable'); END;
                 CREATE TRIGGER review_compensation_ledger_immutable_update
                 BEFORE UPDATE ON review_compensation_ledger
                 BEGIN SELECT RAISE(ABORT, 'review compensation ledger is append-only'); END;
                 CREATE TRIGGER review_compensation_ledger_immutable_delete
                 BEFORE DELETE ON review_compensation_ledger
                 BEGIN SELECT RAISE(ABORT, 'review compensation ledger is append-only'); END;",
        down_sql: Some(
            "CREATE TEMP TABLE review_compensation_rollback_guard (
                 must_be_zero INTEGER NOT NULL CHECK(must_be_zero = 0)
             );
             INSERT INTO review_compensation_rollback_guard(must_be_zero)
             SELECT 1
              WHERE EXISTS (SELECT 1 FROM review_compensation_ledger)
                 OR EXISTS (SELECT 1 FROM review_compensation_settlements)
                 OR EXISTS (
                    SELECT 1 FROM review_events
                     WHERE id > (SELECT effective_after_event_id
                                   FROM review_compensation_policies
                                  WHERE policy_version = 'review-iqd-v1-2026-08-21')
                 );
             DROP TABLE review_compensation_rollback_guard;
             DROP TRIGGER IF EXISTS review_compensation_ledger_immutable_delete;
             DROP TRIGGER IF EXISTS review_compensation_ledger_immutable_update;
             DROP TRIGGER IF EXISTS review_compensation_settlement_immutable_delete;
             DROP TRIGGER IF EXISTS review_compensation_settlement_immutable_update;
             DROP TRIGGER IF EXISTS review_compensation_settlement_validate_insert;
             DROP TRIGGER IF EXISTS review_compensation_policy_immutable_delete;
             DROP TRIGGER IF EXISTS review_compensation_policy_immutable_update;
             DROP TRIGGER IF EXISTS review_event_operation_immutable_update;
             DROP TRIGGER IF EXISTS review_event_operation_validate_insert;
             DROP INDEX IF EXISTS idx_review_compensation_work;
             DROP INDEX IF EXISTS idx_review_compensation_reviewer;
             DROP INDEX IF EXISTS idx_review_compensation_one_entry_per_event;
             DROP INDEX IF EXISTS idx_review_compensation_settlement_boundary;
             DROP INDEX IF EXISTS idx_review_events_operation_id;
             DROP TABLE IF EXISTS review_compensation_settlements;
             DROP TABLE IF EXISTS review_compensation_ledger;
             DROP TABLE IF EXISTS review_compensation_policies;
             ALTER TABLE review_events DROP COLUMN operation_payload_hash;
             ALTER TABLE review_events DROP COLUMN operation_id;
             ALTER TABLE review_events DROP COLUMN compensation_action;",
        ),
    },
    Migration {
        version: 58,
        description: "Archive and remove only abandoned-import child rows whose speech segment is missing",
        // Production preflight 2026-08-21 found exactly two FK-violation classes left by deletion of an
        // abandoned import: segment_hypotheses and loop0_shadow_log rows whose speech_segments parent no
        // longer exists. Never manufacture a parent and never discard the evidence. This migration copies
        // every source column plus the original SQLite row identity and explicit repair provenance into
        // immutable archive tables before deleting a child. Each DELETE is additionally gated on the exact
        // archive key/rowid existing. `apply_migration` runs a whole-database foreign_key_check before commit
        // for v58, so an unexpected third violation class aborts and restores both source tables atomically.
        up_sql: "CREATE TABLE orphan_segment_hypotheses_archive_v58 (
                     original_rowid            INTEGER NOT NULL UNIQUE,
                     segment_id                 TEXT NOT NULL,
                     model_id                   TEXT NOT NULL,
                     transcript                 TEXT NOT NULL,
                     confidence                 REAL,
                     created_at                 TEXT NOT NULL,
                     model_version_id           TEXT NOT NULL,
                     source_table               TEXT NOT NULL
                                                    CHECK(source_table = 'segment_hypotheses'),
                     archive_reason             TEXT NOT NULL
                                                    CHECK(archive_reason = 'missing speech_segments parent'),
                     archive_migration_version  INTEGER NOT NULL CHECK(archive_migration_version = 58),
                     archived_at                TEXT NOT NULL,
                     PRIMARY KEY(segment_id, model_id)
                 );
                 CREATE TABLE orphan_loop0_shadow_log_archive_v58 (
                     id                         INTEGER PRIMARY KEY,
                     segment_id                 TEXT NOT NULL,
                     memory_fired               BOOLEAN,
                     created_at                 TEXT,
                     source_table               TEXT NOT NULL
                                                    CHECK(source_table = 'loop0_shadow_log'),
                     archive_reason             TEXT NOT NULL
                                                    CHECK(archive_reason = 'missing speech_segments parent'),
                     archive_migration_version  INTEGER NOT NULL CHECK(archive_migration_version = 58),
                     archived_at                TEXT NOT NULL
                 );

                 -- Plain CREATE/INSERT are deliberate. If evidence tables already exist while the schema
                 -- version says v58 is pending, their provenance is ambiguous; fail instead of accepting or
                 -- overwriting potentially tampered evidence.
                 INSERT INTO orphan_segment_hypotheses_archive_v58
                     (original_rowid, segment_id, model_id, transcript, confidence, created_at,
                      model_version_id, source_table, archive_reason, archive_migration_version, archived_at)
                 SELECT h.rowid, h.segment_id, h.model_id, h.transcript, h.confidence, h.created_at,
                        h.model_version_id, 'segment_hypotheses', 'missing speech_segments parent', 58,
                        datetime('now')
                   FROM segment_hypotheses h
                   WHERE NOT EXISTS (SELECT 1 FROM speech_segments s WHERE s.id = h.segment_id)
                     AND h.model_id = 'omniasr-7b-legacy-c348ade8a816'
                     AND h.model_version_id = 'omniasr-7b-legacy-c348ade8a816'
                     AND h.confidence IS NULL
                     AND EXISTS (
                           SELECT 1 FROM loop0_shadow_log l
                            WHERE l.segment_id = h.segment_id
                              AND h.rowid - l.id = 2555
                              AND l.memory_fired = 0
                              AND l.created_at IS NOT NULL
                         );
                 INSERT INTO orphan_loop0_shadow_log_archive_v58
                     (id, segment_id, memory_fired, created_at, source_table, archive_reason,
                      archive_migration_version, archived_at)
                 SELECT l.id, l.segment_id, l.memory_fired, l.created_at,
                        'loop0_shadow_log', 'missing speech_segments parent', 58, datetime('now')
                   FROM loop0_shadow_log l
                   WHERE NOT EXISTS (SELECT 1 FROM speech_segments s WHERE s.id = l.segment_id)
                     AND l.memory_fired = 0
                     AND l.created_at IS NOT NULL
                     AND EXISTS (
                           SELECT 1 FROM segment_hypotheses h
                            WHERE h.segment_id = l.segment_id
                              AND h.rowid - l.id = 2555
                              AND h.model_id = 'omniasr-7b-legacy-c348ade8a816'
                              AND h.model_version_id = 'omniasr-7b-legacy-c348ade8a816'
                              AND h.confidence IS NULL
                         );

                 DELETE FROM segment_hypotheses
                  WHERE NOT EXISTS (
                            SELECT 1 FROM speech_segments s
                             WHERE s.id = segment_hypotheses.segment_id
                         )
                    AND segment_hypotheses.model_id = 'omniasr-7b-legacy-c348ade8a816'
                    AND segment_hypotheses.model_version_id = 'omniasr-7b-legacy-c348ade8a816'
                    AND segment_hypotheses.confidence IS NULL
                    AND EXISTS (
                            SELECT 1 FROM loop0_shadow_log l
                             WHERE l.segment_id = segment_hypotheses.segment_id
                               AND segment_hypotheses.rowid - l.id = 2555
                               AND l.memory_fired = 0
                               AND l.created_at IS NOT NULL
                        )
                    AND EXISTS (
                            SELECT 1 FROM orphan_segment_hypotheses_archive_v58 a
                             WHERE a.original_rowid = segment_hypotheses.rowid
                               AND a.segment_id = segment_hypotheses.segment_id
                               AND a.model_id = segment_hypotheses.model_id
                               AND a.transcript IS segment_hypotheses.transcript
                               AND a.confidence IS segment_hypotheses.confidence
                               AND a.created_at IS segment_hypotheses.created_at
                               AND a.model_version_id IS segment_hypotheses.model_version_id
                        );
                 DELETE FROM loop0_shadow_log
                  WHERE NOT EXISTS (
                            SELECT 1 FROM speech_segments s
                             WHERE s.id = loop0_shadow_log.segment_id
                         )
                    AND loop0_shadow_log.memory_fired = 0
                    AND loop0_shadow_log.created_at IS NOT NULL
                    -- The live hypothesis row was deleted immediately above. Bind this second DELETE
                    -- to its exact immutable archive twin instead of querying an already-empty source.
                    AND EXISTS (
                            SELECT 1 FROM orphan_segment_hypotheses_archive_v58 h
                             WHERE h.segment_id = loop0_shadow_log.segment_id
                               AND h.original_rowid - loop0_shadow_log.id = 2555
                               AND h.model_id = 'omniasr-7b-legacy-c348ade8a816'
                               AND h.model_version_id = 'omniasr-7b-legacy-c348ade8a816'
                               AND h.confidence IS NULL
                        )
                    AND EXISTS (
                            SELECT 1 FROM orphan_loop0_shadow_log_archive_v58 a
                             WHERE a.id = loop0_shadow_log.id
                               AND a.segment_id = loop0_shadow_log.segment_id
                               AND a.memory_fired IS loop0_shadow_log.memory_fired
                               AND a.created_at IS loop0_shadow_log.created_at
                        );

                 CREATE TRIGGER orphan_segment_hypotheses_archive_v58_immutable_insert
                 BEFORE INSERT ON orphan_segment_hypotheses_archive_v58
                 BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;
                 CREATE TRIGGER orphan_segment_hypotheses_archive_v58_immutable_update
                 BEFORE UPDATE ON orphan_segment_hypotheses_archive_v58
                 BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;
                 CREATE TRIGGER orphan_segment_hypotheses_archive_v58_immutable_delete
                 BEFORE DELETE ON orphan_segment_hypotheses_archive_v58
                 BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;
                 CREATE TRIGGER orphan_loop0_shadow_log_archive_v58_immutable_insert
                 BEFORE INSERT ON orphan_loop0_shadow_log_archive_v58
                 BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;
                 CREATE TRIGGER orphan_loop0_shadow_log_archive_v58_immutable_update
                 BEFORE UPDATE ON orphan_loop0_shadow_log_archive_v58
                 BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;
                 CREATE TRIGGER orphan_loop0_shadow_log_archive_v58_immutable_delete
                 BEFORE DELETE ON orphan_loop0_shadow_log_archive_v58
                 BEGIN SELECT RAISE(ABORT, 'v58 orphan archive is immutable'); END;",
        // Downgrade is deliberately conditional. Reintroducing the archived children while their parent is
        // still absent would recreate the corruption v58 repaired; overwriting a same-key or same-rowid row
        // created later would destroy newer work. The guard fails inside rollback's transaction and leaves
        // v58, both archives, and all live tables unchanged. If the exact parents have been recovered and no
        // identity conflicts exist, rollback restores every original value/rowid before dropping the archive.
        down_sql: Some(
            "CREATE TEMP TABLE orphan_repair_v58_rollback_guard (
                 must_be_zero INTEGER NOT NULL CHECK(must_be_zero = 0)
             );
             INSERT INTO orphan_repair_v58_rollback_guard(must_be_zero)
             SELECT 1 WHERE EXISTS (
                 SELECT 1 FROM orphan_segment_hypotheses_archive_v58 a
                  WHERE NOT EXISTS (SELECT 1 FROM speech_segments s WHERE s.id = a.segment_id)
             );
             INSERT INTO orphan_repair_v58_rollback_guard(must_be_zero)
             SELECT 1 WHERE EXISTS (
                 SELECT 1 FROM orphan_loop0_shadow_log_archive_v58 a
                  WHERE NOT EXISTS (SELECT 1 FROM speech_segments s WHERE s.id = a.segment_id)
             );
             INSERT INTO orphan_repair_v58_rollback_guard(must_be_zero)
             SELECT 1 WHERE EXISTS (
                 SELECT 1
                   FROM orphan_segment_hypotheses_archive_v58 a
                   JOIN segment_hypotheses h
                     ON (h.segment_id = a.segment_id AND h.model_id = a.model_id)
                     OR h.rowid = a.original_rowid
             );
             INSERT INTO orphan_repair_v58_rollback_guard(must_be_zero)
             SELECT 1 WHERE EXISTS (
                 SELECT 1
                   FROM orphan_loop0_shadow_log_archive_v58 a
                   JOIN loop0_shadow_log l ON l.id = a.id
             );
             DROP TABLE orphan_repair_v58_rollback_guard;

             DROP TRIGGER orphan_segment_hypotheses_archive_v58_immutable_insert;
             DROP TRIGGER orphan_segment_hypotheses_archive_v58_immutable_update;
             DROP TRIGGER orphan_segment_hypotheses_archive_v58_immutable_delete;
             DROP TRIGGER orphan_loop0_shadow_log_archive_v58_immutable_insert;
             DROP TRIGGER orphan_loop0_shadow_log_archive_v58_immutable_update;
             DROP TRIGGER orphan_loop0_shadow_log_archive_v58_immutable_delete;

             INSERT INTO segment_hypotheses
                 (rowid, segment_id, model_id, transcript, confidence, created_at, model_version_id)
             SELECT original_rowid, segment_id, model_id, transcript, confidence, created_at, model_version_id
               FROM orphan_segment_hypotheses_archive_v58
              ORDER BY original_rowid;
             INSERT INTO loop0_shadow_log(id, segment_id, memory_fired, created_at)
             SELECT id, segment_id, memory_fired, created_at
               FROM orphan_loop0_shadow_log_archive_v58
              ORDER BY id;

             DROP TABLE orphan_segment_hypotheses_archive_v58;
             DROP TABLE orphan_loop0_shadow_log_archive_v58;",
        ),
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn database_at_v57() -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert_eq!(rollback(&db, 1).unwrap(), vec![58], "fixture must stop immediately before v58");
        assert_eq!(get_current_version(&db).unwrap(), 57);
        db
    }

    fn v58_fixture_id(index: i64) -> String {
        format!("00000000-0000-4000-8000-{index:012x}")
    }

    /// Deterministic test twin of the measured production cohort. Its sorted-id digest is the
    /// cfg(test)-only value accepted by `validate_v58_orphan_source`; every other shape remains red.
    fn seed_v58_authorized_cohort(db: &Database) {
        run_with_foreign_keys_off(db.connection(), || {
            let tx = db.connection().unchecked_transaction()?;
            {
                let mut insert_hypothesis = tx.prepare(
                    "INSERT INTO segment_hypotheses
                        (rowid, segment_id, model_id, transcript, confidence, created_at, model_version_id)
                     VALUES (?1, ?2, 'omniasr-7b-legacy-c348ade8a816', ?3, NULL, ?4,
                             'omniasr-7b-legacy-c348ade8a816')",
                )?;
                let mut insert_loop0 = tx.prepare(
                    "INSERT INTO loop0_shadow_log(id, segment_id, memory_fired, created_at)
                     VALUES (?1, ?2, 0, ?3)",
                )?;
                for index in 0..V58_ORPHAN_IDS as i64 {
                    let segment_id = v58_fixture_id(index);
                    let hypothesis_rowid = 2_000_000 + index;
                    let loop0_id = hypothesis_rowid - 2_555;
                    let transcript = if index == 0 { "دەقی یەکەم\nبە وردی" } else { "دەق" };
                    let created_at = format!("2026-08-21 01:{:02}:{:02}", (index / 60) % 60, index % 60);
                    insert_hypothesis.execute(rusqlite::params![
                        hypothesis_rowid,
                        segment_id,
                        transcript,
                        created_at,
                    ])?;
                    insert_loop0.execute(rusqlite::params![loop0_id, segment_id, created_at])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    }

    fn foreign_key_violation_count(conn: &rusqlite::Connection) -> usize {
        conn.prepare("PRAGMA foreign_key_check").unwrap().query_map([], |_| Ok(())).unwrap().count()
    }

    #[test]
    fn test_migrations_run() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let _applied = run_migrations(&db).unwrap();
        assert!(get_current_version(&db).unwrap() >= 1);
    }

    #[test]
    fn v40_speech_segments_is_strict_and_the_recreate_preserved_everything() {
        // The riskiest migration in the app, on a POPULATED schema. initialize() runs v40 on an empty
        // table, so this test re-exercises the real recreate against real rows + a real FK child, and
        // pins every invariant the recreate could silently break.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert!(get_current_version(&db).unwrap() >= 40, "v40 must have applied");

        // Real rows through the real write path, plus a CASCADE child — the thing a naive DROP wipes.
        for i in 0..3 {
            db.insert_segment(&crate::db::SpeechSegment {
                id: format!("s-{i}"),
                audio_path: format!("/a{i}.wav"),
                raw_transcript: "کوردی".into(),
                duration_ms: 1000,
                signal_anomaly_score: Some(0.5),
                ..Default::default()
            })
            .unwrap();
        }
        db.write_segment_verdict("s-1", "auto_accept", Some("t"), None, None, Some(0.9), false).unwrap();
        let rowids_before: Vec<i64> = db
            .connection()
            .prepare("SELECT rowid FROM speech_segments ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        // Re-run the real recreate through the real FK-off path (what a live upgrade does).
        //
        // v40's INSERT…SELECT names `agent_confidence` explicitly, and v52 renamed that column to
        // `agreement_score`. A LIVE upgrade never hits this: it applies v40 while the column still has
        // its old name and only reaches v52 afterwards. This test replays v40 AFTER the whole chain, so
        // it has to put the schema back into the state v40 would actually see — otherwise it fails on
        // an ordering that cannot occur in production. The re-apply of every post-v40 migration below
        // runs v52 again and restores the HEAD name.
        let v40 = MIGRATIONS.iter().find(|m| m.version == 40).expect("v40 exists");
        db.connection()
            .execute_batch("ALTER TABLE speech_segments RENAME COLUMN agreement_score TO agent_confidence;")
            .expect("put the column back to its pre-v52 name so v40 replays against the schema it expects");
        {
            let conn = db.connection();
            run_with_foreign_keys_off(conn, || {
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(v40.up_sql)?;
                reject_foreign_key_violations(&tx, 40)?;
                tx.commit()?;
                Ok(())
            })
            .expect("the FK-off recreate must succeed");
        }

        // A live upgrade applies v40 and THEN the later migrations which extend speech_segments.
        // Re-running v40's recreate in isolation leaves the table at v40's 34-column shape, so restore
        // exactly those table/index/trigger changes before HEAD-schema readers run below. Unrelated
        // migrations survive the v40 recreate and must not be replayed: in particular, v58 deliberately
        // uses plain CREATE for immutable repair evidence and MUST reject a second application.
        const POST_V40_SPEECH_SEGMENT_MIGRATIONS: &[i64] = &[41, 42, 43, 47, 48, 49, 50, 51, 52, 53];
        for later in MIGRATIONS.iter().filter(|m| POST_V40_SPEECH_SEGMENT_MIGRATIONS.contains(&m.version)) {
            if let Err(e) = db.connection().execute_batch(later.up_sql) {
                // "duplicate column" here is the GOOD outcome for a table v40's recreate never
                // touched (v56 alters review_events): the column survived, there is nothing to
                // restore. Any other error is a real re-application failure.
                let msg = e.to_string();
                assert!(
                    msg.contains("duplicate column name"),
                    "re-applying post-v40 migration v{} must succeed or be a no-op: {msg}",
                    later.version
                );
            }
        }

        let conn = db.connection();
        // (1) STRICT is actually declared, and now REJECTS a type-violating raw write.
        let sql: String = conn
            .query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='speech_segments'", [], |r| r.get(0))
            .unwrap();
        assert!(sql.contains("STRICT"), "speech_segments must be STRICT: {sql}");
        assert!(
            conn.execute(
                "INSERT INTO speech_segments (id, audio_path, duration_ms) VALUES ('bad', '/x.wav', 'not-an-int')",
                []
            )
            .is_err(),
            "STRICT must reject a TEXT into the INTEGER duration_ms"
        );

        // (2) Rows survived, values intact, rowids PRESERVED (segments_fts keys on content_rowid=rowid).
        assert_eq!(db.segment_count().unwrap(), 3, "every row survived the recreate");
        assert_eq!(db.get_segment_by_id("s-1").unwrap().unwrap().signal_anomaly_score, Some(0.5));
        let rowids_after: Vec<i64> = conn
            .prepare("SELECT rowid FROM speech_segments ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rowids_before, rowids_after, "rowids must be preserved for the external-content FTS index");

        // (3) The CASCADE child SURVIVED — the whole reason v40 runs with foreign_keys OFF.
        let child: i64 =
            conn.query_row("SELECT COUNT(*) FROM decision_verdicts WHERE segment_id='s-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(child, 1, "the FK child must survive the parent recreate (no cascade)");

        // (4) All TEN indexes are back (a missed one silently becomes a full table scan).
        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND tbl_name='speech_segments' AND sql IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Counted, not >=: the point of this assertion is that the v40 table RECREATE rebuilt every
        // index rather than silently dropping one, so a loosened comparison would defeat it. Raised
        // 10 -> 11 by v50's partial index on audio_fingerprint, and 11 -> 12 by v51's partial index on
        // audio_content_hash. If you add an index to speech_segments, raise this deliberately — do not
        // relax it.
        assert_eq!(idx, 12, "all 12 indexes must be recreated");

        // (5) FTS still finds a segment by transcript (triggers recreated + index rebuilt).
        let hits = db.search_segments("کوردی").unwrap();
        assert!(!hits.is_empty(), "FTS search must still work after the recreate");

        // (6) The DB is consistent and FK-clean, and normal writes resume (triggers alive).
        assert_eq!(db.integrity_check().unwrap().trim(), "ok");
        let mut stmt = conn.prepare("PRAGMA foreign_key_check").unwrap();
        assert_eq!(stmt.query_map([], |_| Ok(())).unwrap().count(), 0, "no FK violations after the recreate");
        drop(stmt);
        db.insert_segment(&crate::db::SpeechSegment {
            id: "after".into(),
            audio_path: "/z.wav".into(),
            ..Default::default()
        })
        .unwrap();
    }

    #[test]
    fn fk_off_window_refuses_when_the_pragma_silently_did_not_take_effect() {
        // THE guard that prevents silent data loss. SQLite IGNORES `PRAGMA foreign_keys=OFF` inside a
        // transaction and still reports success — and foreign_key_check cannot save us, because a cascade
        // deletes children CLEANLY (zero violations). So if the pragma doesn't take, an FK-parent recreate
        // would commit total child-row loss with every check passing. The window must fail CLOSED.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();

        // Simulate the hazard: already inside a transaction, so the pragma silently no-ops.
        let tx = conn.unchecked_transaction().unwrap();
        let err = run_with_foreign_keys_off(conn, || -> AppResult<()> {
            panic!("the body must NEVER run when foreign_keys is still ON — that is the data-loss path");
        })
        .expect_err("must refuse when the pragma did not take effect");
        assert!(format!("{err}").contains("did not take effect"), "the refusal must name the real cause: {err}");
        drop(tx);

        // And the connection is untouched: FK enforcement was never actually disabled.
        let fk: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fk, 1, "refusing must leave foreign_keys ON");
    }

    #[test]
    fn fk_off_window_restores_foreign_keys_even_when_the_body_fails() {
        // A leaked foreign_keys=OFF would silently disable FK enforcement for the rest of the
        // connection's life — far worse than the failed migration itself. The restore must be unconditional.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();
        let err =
            run_with_foreign_keys_off(conn, || -> AppResult<()> { Err(crate::error::AppError::Other("boom".into())) });
        assert!(err.is_err(), "the body's error must propagate");
        let fk_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fk_on, 1, "foreign_keys must be restored to ON even when the body fails");
    }

    #[test]
    fn v39_renames_ood_score_to_signal_anomaly_score() {
        // The internal half of the OOD -> signal_anomaly rename. On a fully-migrated schema the column
        // must carry the new name and the jargon name must be GONE, and a real write/read must
        // round-trip through the renamed column (proving the Rust field + SQL agree).
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert!(get_current_version(&db).unwrap() >= 39, "v39 must have applied");

        let conn = db.connection();
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('speech_segments')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(cols.iter().any(|c| c == "signal_anomaly_score"), "renamed column must exist: {cols:?}");
        assert!(!cols.iter().any(|c| c == "ood_score"), "the ood_score jargon column must be gone: {cols:?}");

        db.insert_segment(&crate::db::SpeechSegment {
            id: "sa-1".into(),
            audio_path: "/a.wav".into(),
            signal_anomaly_score: Some(0.42),
            ..Default::default()
        })
        .unwrap();
        let got = db.get_segment_by_id("sa-1").unwrap().expect("segment");
        assert_eq!(got.signal_anomaly_score, Some(0.42), "value round-trips through the renamed column");
    }

    #[test]
    fn v39_preserves_existing_values_through_the_rename() {
        // The UPGRADE path that matters: a real pre-v39 DB already holds ood_score values written by an
        // older build. RENAME COLUMN must carry them across in place (unlike a table recreate, which on
        // this FK-parent table would fire ON DELETE CASCADE — see
        // dropping_speech_segments_cascade_deletes_children_so_strict_recreate_needs_fk_off).
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();

        // Reconstruct a pre-v39 shape, seed a row under the OLD column name, then re-apply v39's up_sql.
        conn.execute_batch("ALTER TABLE speech_segments RENAME COLUMN signal_anomaly_score TO ood_score;").unwrap();
        conn.execute("INSERT INTO speech_segments (id, audio_path, ood_score) VALUES ('pre39', '/a.wav', 0.77)", [])
            .unwrap();

        let v39 = MIGRATIONS.iter().find(|m| m.version == 39).expect("v39 exists");
        conn.execute_batch(v39.up_sql).unwrap();

        let got: f64 = conn
            .query_row("SELECT signal_anomaly_score FROM speech_segments WHERE id='pre39'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(got, 0.77, "a pre-v39 ood_score value must survive the RENAME COLUMN intact");
    }

    #[test]
    fn v38_decision_verdicts_becomes_strict_and_preserves_rows() {
        // STRICT-tables pilot, on a REAL migrated schema (initialize runs every migration incl. v38).
        // Verifies: (1) the pre-existing decision_verdicts rows survive the recreate, (2) the table is
        // now STRICT (an affinity-mangled write is REJECTED, not silently coerced), (3) the index and
        // FK CASCADE still work.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert!(get_current_version(&db).unwrap() >= 38, "v38 must have applied");

        // A real verdict write goes through the app path (segment + write_segment_verdict -> the
        // decision_verdicts INSERT), so the row exists BEFORE we probe STRICT.
        db.insert_segment(&crate::db::SpeechSegment {
            id: "sv-1".into(),
            audio_path: "/a.wav".into(),
            raw_transcript: "دەق".into(),
            duration_ms: 1000,
            ..Default::default()
        })
        .unwrap();
        db.write_segment_verdict("sv-1", "auto_accept", Some("t"), None, None, Some(0.9), false).unwrap();
        let conn = db.connection();
        let (before,): (i64,) = conn
            .query_row("SELECT COUNT(*) FROM decision_verdicts WHERE segment_id='sv-1'", [], |r| Ok((r.get(0)?,)))
            .unwrap();
        assert_eq!(before, 1, "the real verdict write landed a decision_verdicts row");

        // (2) STRICT enforcement: the table must be declared STRICT...
        let sql: String = conn
            .query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='decision_verdicts'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(sql.contains("STRICT"), "decision_verdicts must be a STRICT table: {sql}");
        // ...and reject a value with no valid conversion to the column type. auto_accept_verdict is TEXT,
        // which accepts most things, so probe the PRIMARY KEY path: a STRICT TEXT column rejects a BLOB.
        let bad = conn
            .execute("INSERT INTO decision_verdicts (segment_id, auto_accept_verdict) VALUES (x'00', 'T0_ACCEPT')", []);
        assert!(bad.is_err(), "STRICT must reject a BLOB into a TEXT column");

        // (3) FK CASCADE still works after the recreate: deleting the segment removes its verdict row.
        db.delete_segment("sv-1").unwrap();
        let (after,): (i64,) = conn
            .query_row("SELECT COUNT(*) FROM decision_verdicts WHERE segment_id='sv-1'", [], |r| Ok((r.get(0)?,)))
            .unwrap();
        assert_eq!(after, 0, "FK ON DELETE CASCADE survives the STRICT recreate");
    }

    #[test]
    fn v38_migrates_a_prepopulated_pre_v38_row() {
        // The upgrade path that matters: a real DB that already had decision_verdicts rows (written by a
        // pre-v38 build) must carry them through the STRICT recreate intact. Simulate by inserting a row
        // via raw SQL then running the migration that owns the recreate.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap(); // fully migrated incl. v38 — but we re-exercise the recreate's INSERT..SELECT
        db.insert_segment(&crate::db::SpeechSegment {
            id: "pre-1".into(),
            audio_path: "/a.wav".into(),
            ..Default::default()
        })
        .unwrap();
        db.connection()
            .execute(
                "INSERT INTO decision_verdicts (segment_id, auto_accept_verdict, verdict_computed_at) \
                 VALUES ('pre-1', 'T0_ACCEPT', datetime('now'))",
                [],
            )
            .unwrap();
        // Re-run the v38 up_sql directly (the recreate is idempotent-safe on data): rows must survive.
        let v38 = MIGRATIONS.iter().find(|m| m.version == 38).unwrap();
        db.connection().execute_batch(v38.up_sql).unwrap();
        let (verdict,): (String,) = db
            .connection()
            .query_row("SELECT auto_accept_verdict FROM decision_verdicts WHERE segment_id='pre-1'", [], |r| {
                Ok((r.get(0)?,))
            })
            .unwrap();
        assert_eq!(verdict, "T0_ACCEPT", "a pre-v38 row survives the STRICT recreate with its data intact");
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
    fn reopening_a_populated_db_preserves_data_on_restart() {
        // The real launch path reopens the existing DB file and calls initialize() (which re-runs
        // migrations) every time. On an already-migrated, populated DB that must be a NO-OP for the
        // data: a destructive/migrating-again bug here would silently lose user work on every start.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restart.db");
        let path_str = path.to_str().unwrap();

        // First launch: migrate + persist a rich segment, then "close".
        {
            let db = Database::open(path_str).unwrap();
            db.initialize().unwrap();
            let seg = crate::db::SpeechSegment {
                id: "keep-1".into(),
                audio_path: "/a/keep.wav".into(),
                raw_transcript: "کوردی".into(),
                duration_ms: 1234,
                alignment_json: Some(
                    "{\"source_start_ms\":0,\"source_end_ms\":1234,\"chunk_index\":0,\"chunk_count\":1}".into(),
                ),
                ..Default::default()
            };
            db.insert_segment(&seg).unwrap();
        }

        // Second launch (restart): reopen + initialize again.
        let db = Database::open(path_str).unwrap();
        assert!(run_migrations(&db).unwrap().is_empty(), "restart must not re-apply migrations");
        db.initialize().unwrap(); // the actual launch path

        let got = db.get_segment_by_id("keep-1").unwrap().expect("segment must survive the restart");
        assert_eq!(got.raw_transcript, "کوردی", "transcript intact across restart");
        assert_eq!(got.duration_ms, 1234, "duration intact");
        assert_eq!(got.audio_path, "/a/keep.wav", "audio path intact");
        assert!(got.alignment_json.is_some(), "playback alignment intact");
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
        let recorded: i64 =
            db.connection().query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0)).unwrap();
        assert_eq!(recorded as usize, MIGRATIONS.len(), "every migration must be recorded exactly once");

        // Re-running migrates nothing and leaves the version untouched.
        let again = run_migrations(&db).unwrap();
        assert!(again.is_empty());
        assert_eq!(get_current_version(&db).unwrap(), max_version);
    }

    #[test]
    fn exact_history_accepts_only_the_description_bound_complete_prefix() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert_eq!(validate_applied_history(db.connection()).unwrap(), max_supported_version());

        db.connection().execute("DELETE FROM schema_migrations WHERE version = 23", []).unwrap();
        let missing = validate_applied_history(db.connection()).expect_err("a missing middle row must fail");
        assert!(missing.to_string().contains("missing=[23]"), "unexpected history error: {missing}");

        let description = MIGRATIONS.iter().find(|migration| migration.version == 23).unwrap().description;
        db.connection()
            .execute(
                "INSERT INTO schema_migrations(version, description) VALUES (23, ?1)",
                rusqlite::params![description],
            )
            .unwrap();
        db.connection()
            .execute("UPDATE schema_migrations SET description = 'tampered' WHERE version = 31", [])
            .unwrap();
        let drift = validate_applied_history(db.connection()).expect_err("description drift must fail");
        assert!(drift.to_string().contains("description_mismatch=[31]"), "unexpected history error: {drift}");
    }

    #[test]
    fn a_lone_maximum_or_empty_existing_history_never_bootstraps() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&crate::db::SpeechSegment {
            id: "history-sentinel".into(),
            audio_path: "/sentinel.wav".into(),
            ..Default::default()
        })
        .unwrap();

        let head = max_supported_version();
        db.connection().execute("DELETE FROM schema_migrations WHERE version <> ?1", rusqlite::params![head]).unwrap();
        let lone = run_migrations(&db).expect_err("MAX(version) without its prefix must fail");
        assert!(lone.to_string().contains("missing="), "unexpected history error: {lone}");

        db.connection().execute("DELETE FROM schema_migrations", []).unwrap();
        let empty = db.initialize().expect_err("an existing database with empty history must fail closed");
        assert!(empty.to_string().contains("schema_migrations is empty"), "unexpected bootstrap error: {empty}");
        assert!(
            db.get_segment_by_id("history-sentinel").unwrap().is_some(),
            "refusing damaged history must preserve live data"
        );
    }

    #[test]
    fn restart_refuses_a_missing_history_table_or_required_schema_object() {
        let missing_history = Database::open(":memory:").unwrap();
        missing_history.initialize().unwrap();
        missing_history.connection().execute("DROP TABLE schema_migrations", []).unwrap();
        let history_error = missing_history.initialize().expect_err("missing history must fail before startup");
        assert!(
            history_error.to_string().contains("schema_migrations is missing"),
            "unexpected error: {history_error}"
        );

        let missing_object = Database::open(":memory:").unwrap();
        missing_object.initialize().unwrap();
        missing_object.connection().execute("DROP TABLE jobs", []).unwrap();
        let schema_error = missing_object.initialize().expect_err("exact history cannot hide a dropped required table");
        let schema_message = schema_error.to_string();
        assert!(
            schema_message.contains("missing=") && schema_message.contains("\"jobs\""),
            "unexpected error: {schema_error}"
        );
    }

    #[test]
    fn migration_v27_creates_model_abilities_table() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let exists: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='model_abilities'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(exists, 1, "the v27 model_abilities table must exist after initialize()");
    }

    #[test]
    fn migration_v37_creates_jobs_table_with_enforced_constraints() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();

        // Table + the three indexes exist.
        let table: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='jobs'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(table, 1, "the v37 jobs table must exist after initialize()");
        let indexes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index'
                 AND name IN ('idx_jobs_idempotency', 'idx_jobs_state', 'idx_jobs_kind_state')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexes, 3, "all three v37 job indexes must exist");

        // A valid queued job inserts fine; a defaulted row is 'queued' at progress 0.
        conn.execute("INSERT INTO jobs (id, kind) VALUES ('j1', 'import')", []).unwrap();
        let (state, progress): (String, f64) = conn
            .query_row("SELECT state, progress FROM jobs WHERE id='j1'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(state, "queued");
        assert_eq!(progress, 0.0);

        // The state CHECK rejects anything outside the lifecycle vocabulary.
        assert!(
            conn.execute("INSERT INTO jobs (id, kind, state) VALUES ('bad', 'import', 'pending')", []).is_err(),
            "an out-of-vocabulary state must violate the CHECK constraint"
        );
        // The progress CHECK rejects out-of-[0,1] values.
        assert!(
            conn.execute("INSERT INTO jobs (id, kind, progress) VALUES ('bad2', 'import', 1.5)", []).is_err(),
            "progress > 1.0 must violate the CHECK constraint"
        );

        // idempotency_key is UNIQUE where present (a re-issued identical job cannot duplicate)...
        conn.execute("INSERT INTO jobs (id, kind, idempotency_key) VALUES ('k1', 'export', 'dedupe-A')", []).unwrap();
        assert!(
            conn.execute("INSERT INTO jobs (id, kind, idempotency_key) VALUES ('k2', 'export', 'dedupe-A')", [])
                .is_err(),
            "a duplicate idempotency_key must be rejected by the unique partial index"
        );
        // ...but NULL keys are exempt (many jobs legitimately have no dedupe key).
        conn.execute("INSERT INTO jobs (id, kind) VALUES ('n1', 'transcribe')", []).unwrap();
        conn.execute("INSERT INTO jobs (id, kind) VALUES ('n2', 'transcribe')", []).unwrap();
    }

    #[test]
    fn jobs_check_vocabulary_stays_in_lockstep_with_jobstate_enum() {
        // The migration v37 CHECK list and JobState::as_str() are two copies of the same vocabulary in
        // different files. Each file's own tests use literal strings, so a drift (add a state to the enum
        // but not the CHECK, or vice-versa) would pass both suites while the DB silently rejects the new
        // state at write time. This binds them: EVERY enum variant's token must satisfy the CHECK.
        use crate::jobs::JobState;
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();
        for (i, state) in ["queued", "running", "succeeded", "failed", "cancelled"].iter().enumerate() {
            // Every token the enum emits must be accepted by the CHECK constraint.
            assert_eq!(
                JobState::parse(state).map(|s| s.as_str()),
                Some(*state),
                "enum must round-trip the token the CHECK allows"
            );
            conn.execute("INSERT INTO jobs (id, kind, state) VALUES (?1, 'x', ?2)", (format!("v{i}"), state))
                .unwrap_or_else(|e| panic!("CHECK must accept enum token {state:?}: {e}"));
        }
    }

    #[test]
    fn migration_v26_creates_human_decision_index() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let exists: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_segments_human_decision'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "the v26 human_decision index must exist after initialize()");
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
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='correction_memory'", [], |r| {
                r.get(0)
            })
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
    fn refuses_to_run_on_a_schema_newer_than_this_build() {
        // Forward-compat guard: an old binary must NOT silently operate on a DB migrated by a newer
        // build (the stale-GUI-over-v32-DB scenario) — it re-plants confidence=1.0 memories, etc.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let future = max_supported_version() + 1;
        db.connection()
            .execute(
                "INSERT INTO schema_migrations (version, description) VALUES (?1, 'from-the-future')",
                rusqlite::params![future],
            )
            .unwrap();
        let err = run_migrations(&db).expect_err("a newer-than-supported schema must be refused");
        assert!(err.to_string().contains("newer than this build"), "got: {err}");
    }

    #[test]
    fn migration_v32_adds_evidence_confidence_columns() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();

        // Both firing-outcome counters exist and default to 0. A raw insert keeps the column-default
        // confidence (v32's recompute only touched rows present at migration time); the live app path
        // instead sets the Beta prior explicitly. Here we just prove the schema surface exists.
        conn.execute(
            "INSERT INTO correction_memory (id, wrong_token, human_token, slot_key, phonetic_key)
             VALUES ('m32', 'w', 'r', 'L|R', 'p')",
            [],
        )
        .unwrap();
        let (confirm, overrides): (i64, i64) = conn
            .query_row("SELECT confirm_count, override_count FROM correction_memory WHERE id='m32'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((confirm, overrides), (0, 0), "confirm/override evidence counters default to 0");
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

        conn.execute("INSERT INTO speech_segments (id, audio_path) VALUES ('seg-x', '/tmp/a.wav')", []).unwrap();
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
            .query_row("SELECT COUNT(*), MAX(source_segment IS NULL) FROM correction_memory WHERE id='m1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
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
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='corrections'", [], |r| r.get(0))
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
        let decided_set: i64 =
            conn.query_row("SELECT decided_at IS NOT NULL FROM corrections WHERE id='c1'", [], |r| r.get(0)).unwrap();
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

        conn.execute("INSERT INTO speech_segments (id, audio_path) VALUES ('seg-y', '/tmp/b.wav')", []).unwrap();
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
                .query_row("SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name='model_version_id'", [table], |r| {
                    r.get(0)
                })
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
        let seg_mv: String =
            conn.query_row("SELECT model_version_id FROM speech_segments WHERE id='s1'", [], |r| r.get(0)).unwrap();
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
    fn migration_v23_creates_model_registry() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();
        for table in ["model_versions", "adapters"] {
            let exists: i64 = conn
                .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1", [table], |r| r.get(0))
                .unwrap();
            assert_eq!(exists, 1, "{table} table must exist after initialize()");
        }
        let champ_idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_model_versions_one_champion'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(champ_idx, 1, "the one-champion-per-family partial index must exist");
    }

    /// Helper: insert a model_versions row, returning the rusqlite result so tests can assert
    /// on success/failure of CHECK and UNIQUE constraints.
    fn insert_model_version(
        conn: &rusqlite::Connection,
        id: &str,
        family: &str,
        source: &str,
        status: &str,
    ) -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO model_versions (id, family, checkpoint_sha256, checkpoint_path, source, license, status)
             VALUES (?1, ?2, 'sha', '/p.pt', ?3, 'Apache-2.0', ?4)",
            rusqlite::params![id, family, source, status],
        )
    }

    #[test]
    fn model_versions_check_constraints_reject_garbage() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();
        // Valid source + status inserts cleanly.
        assert!(insert_model_version(conn, "ok", "omniasr-7b", "user-finetuned", "candidate").is_ok());
        // A bogus source is rejected by the CHECK constraint.
        assert!(
            insert_model_version(conn, "bad-src", "omniasr-7b", "pirated", "candidate").is_err(),
            "an invalid source must be rejected by the CHECK constraint"
        );
        // A bogus status is rejected by the CHECK constraint.
        assert!(
            insert_model_version(conn, "bad-st", "omniasr-7b", "meta-stock", "the-best").is_err(),
            "an invalid status must be rejected by the CHECK constraint"
        );
    }

    #[test]
    fn at_most_one_champion_per_family_is_db_enforced() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();

        // One champion per family is fine...
        assert!(insert_model_version(conn, "a-champ", "omniasr-7b", "meta-stock", "champion").is_ok());
        // ...a SECOND champion in the SAME family is physically impossible (partial unique index).
        assert!(
            insert_model_version(conn, "a-champ2", "omniasr-7b", "user-finetuned", "champion").is_err(),
            "two champions in one family must violate the partial unique index"
        );
        // A champion in a DIFFERENT family is allowed.
        assert!(insert_model_version(conn, "w-champ", "whisper-ckb", "meta-stock", "champion").is_ok());
        // Non-champion rows in the same family are unconstrained (many candidates allowed).
        assert!(insert_model_version(conn, "cand1", "omniasr-7b", "user-finetuned", "candidate").is_ok());
        assert!(insert_model_version(conn, "cand2", "omniasr-7b", "user-finetuned", "candidate").is_ok());
    }

    #[test]
    fn adapter_dies_with_its_parent_model_version() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let conn = db.connection();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        insert_model_version(conn, "mv1", "omniasr-7b", "user-finetuned", "candidate").unwrap();
        conn.execute(
            "INSERT INTO adapters (id, parent_model_version_id, base_checkpoint_sha, adapter_sha256)
             VALUES ('ad1', 'mv1', 'baseSHA', 'adapterSHA')",
            [],
        )
        .unwrap();

        // Deleting the parent model version cascades to its adapter (a delta is meaningless alone).
        conn.execute("DELETE FROM model_versions WHERE id='mv1'", []).unwrap();
        let remaining: i64 = conn.query_row("SELECT COUNT(*) FROM adapters WHERE id='ad1'", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 0, "an adapter must be cascade-deleted with its parent model version");
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
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='should_not_persist'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(leaked_table, 0, "the partial table must have been rolled back");
    }

    #[test]
    fn v46_drops_the_cascade_without_dropping_anybody_s_scores() {
        // A table REBUILD is the migration shape that silently loses rows, so this exercises v46
        // against a POPULATED table rather than the empty one initialize() sees — the same reason the
        // v40 test exists. The owner's live library already holds real spot-check scores; a migration
        // that quietly emptied them would destroy the only record of whether a remote reviewer was
        // honest, and nothing downstream would notice a smaller number.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert!(get_current_version(&db).unwrap() >= 46, "v46 must have applied");

        db.insert_segment(&crate::db::SpeechSegment {
            id: "seg-scored".into(),
            audio_path: "/a.wav".into(),
            raw_transcript: "دەقی هەڵە".into(),
            duration_ms: 1000,
            ..Default::default()
        })
        .unwrap();
        db.record_spot_check("seg-scored", "Sara", "edit", "دەقی ڕاست", "دەقی ڕاست").unwrap();
        db.record_spot_check("seg-scored", "Hemn", "accept", "دەقی هەڵە", "دەقی ڕاست").unwrap();

        // Re-apply the real migration over real rows.
        let v46 = MIGRATIONS.iter().find(|m| m.version == 46).expect("v46 exists");
        db.connection().execute_batch(v46.up_sql).expect("re-applying v46 must succeed");

        let report = db.spot_check_report().unwrap();
        assert_eq!(report.len(), 2, "both reviewers' scores must survive the rebuild: {report:?}");
        let sara = report.iter().find(|r| r.reviewer == "Sara").expect("Sara survived");
        assert_eq!(sara.checks, 1);
        assert_eq!(sara.noticed, 1, "and her ANSWER survived, not just her row");
        let hemn = report.iter().find(|r| r.reviewer == "Hemn").expect("Hemn survived");
        assert_eq!(hemn.noticed, 0, "a blind accept must still read as a blind accept");

        // The point of the migration: the FK is gone, so deleting the clip leaves the record standing.
        let sql: String = db
            .connection()
            .query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='spot_checks'", [], |r| r.get(0))
            .unwrap();
        assert!(!sql.to_uppercase().contains("REFERENCES"), "spot_checks must no longer reference the clip: {sql}");
        db.delete_segment("seg-scored").unwrap();
        assert_eq!(
            db.spot_check_report().unwrap().len(),
            2,
            "deleting the clip must not rewrite the history of who reviewed it honestly"
        );
    }

    #[test]
    fn v57_starts_prospectively_after_the_last_legacy_event() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert_eq!(rollback(&db, 2).unwrap(), vec![58, 57], "fixture must return to the v56 schema");

        db.insert_segment(&crate::db::SpeechSegment {
            id: "pay-cutoff".into(),
            audio_path: "/pay-cutoff.wav".into(),
            raw_transcript: "دەق".into(),
            duration_ms: 1_000,
            ..Default::default()
        })
        .unwrap();
        db.connection()
            .execute(
                "INSERT INTO review_events
                    (segment_id, reviewer, action, source, timestamp_ms, duration_ms)
                 VALUES ('pay-cutoff', 'Sara', 'accept', 'legacy', 1, 1000)",
                [],
            )
            .unwrap();
        let legacy_event_id = db.connection().last_insert_rowid();

        assert_eq!(run_migrations(&db).unwrap(), vec![57, 58]);
        let cutoff: i64 = db
            .connection()
            .query_row(
                "SELECT effective_after_event_id FROM review_compensation_policies
                  WHERE policy_version = 'review-iqd-v1-2026-08-21'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cutoff, legacy_event_id);
        let legacy_compensation_action: Option<String> = db
            .connection()
            .query_row("SELECT compensation_action FROM review_events WHERE id = ?1", [legacy_event_id], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(legacy_compensation_action.is_none(), "v57 must not invent a semantic action for history");

        let before = db.review_compensation_summary("Sara").unwrap();
        assert_eq!(before.earned_micro_iqd, 0, "legacy activity is reported, never silently repriced");
        assert_eq!(before.legacy_events_pending_reconciliation, 1);

        let revision = db.segment_review_revision("pay-cutoff").unwrap().unwrap();
        db.record_phone_human_decision_by_at_revision("pay-cutoff", "edit", Some("دەقی ڕاست"), "Sara", revision)
            .unwrap()
            .unwrap();
        let after = db.review_compensation_summary("Sara").unwrap();
        assert_eq!(after.earned_micro_iqd, 5_000_000);
        assert_eq!(after.legacy_events_pending_reconciliation, 1);
        let priced_event_id: i64 = db
            .connection()
            .query_row("SELECT review_event_id FROM review_compensation_ledger", [], |row| row.get(0))
            .unwrap();
        assert!(priced_event_id > cutoff, "only events strictly after the captured cutoff are payable");
    }

    #[test]
    fn v57_policy_and_ledger_rows_are_physically_immutable() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&crate::db::SpeechSegment {
            id: "pay-immutable".into(),
            audio_path: "/pay-immutable.wav".into(),
            raw_transcript: "دەق".into(),
            duration_ms: 1_000,
            ..Default::default()
        })
        .unwrap();
        let revision = db.segment_review_revision("pay-immutable").unwrap().unwrap();
        let operation_id = "423e4567-e89b-42d3-a456-426614174000";
        let operation_hash = "a".repeat(64);
        db.record_phone_human_decision_by_at_revision_with_operation(
            "pay-immutable",
            "edit",
            Some("دەقی ڕاست"),
            "Sara",
            revision,
            operation_id,
            &operation_hash,
        )
        .unwrap()
        .unwrap();
        let ledger_id: i64 =
            db.connection().query_row("SELECT id FROM review_compensation_ledger", [], |row| row.get(0)).unwrap();
        db.record_review_compensation_settlement("Sara", ledger_id, "immutable-payout").unwrap();

        let policy_update = db
            .connection()
            .execute("UPDATE review_compensation_policies SET edit_basis_points = 0", [])
            .unwrap_err()
            .to_string();
        assert!(policy_update.contains("policy is immutable"), "unexpected policy UPDATE error: {policy_update}");
        let policy_delete =
            db.connection().execute("DELETE FROM review_compensation_policies", []).unwrap_err().to_string();
        assert!(policy_delete.contains("policy is immutable"), "unexpected policy DELETE error: {policy_delete}");

        let ledger_update = db
            .connection()
            .execute("UPDATE review_compensation_ledger SET delta_micro_iqd = 0", [])
            .unwrap_err()
            .to_string();
        assert!(ledger_update.contains("ledger is append-only"), "unexpected ledger UPDATE error: {ledger_update}");
        let ledger_delete =
            db.connection().execute("DELETE FROM review_compensation_ledger", []).unwrap_err().to_string();
        assert!(ledger_delete.contains("ledger is append-only"), "unexpected ledger DELETE error: {ledger_delete}");

        let settlement_update = db
            .connection()
            .execute("UPDATE review_compensation_settlements SET allocated_micro_iqd = 0", [])
            .unwrap_err()
            .to_string();
        assert!(
            settlement_update.contains("settlement is immutable"),
            "unexpected settlement UPDATE error: {settlement_update}"
        );
        let settlement_delete =
            db.connection().execute("DELETE FROM review_compensation_settlements", []).unwrap_err().to_string();
        assert!(
            settlement_delete.contains("settlement is immutable"),
            "unexpected settlement DELETE error: {settlement_delete}"
        );

        let operation_update = db
            .connection()
            .execute(
                "UPDATE review_events
                    SET operation_id = '523e4567-e89b-42d3-a456-426614174000'
                  WHERE operation_id = ?1",
                [operation_id],
            )
            .unwrap_err()
            .to_string();
        assert!(
            operation_update.contains("operation identity is immutable"),
            "unexpected operation UPDATE error: {operation_update}"
        );
        let hash_update = db
            .connection()
            .execute(
                "UPDATE review_events SET operation_payload_hash = ?1 WHERE operation_id = ?2",
                rusqlite::params!["b".repeat(64), operation_id],
            )
            .unwrap_err()
            .to_string();
        assert!(
            hash_update.contains("operation identity is immutable"),
            "unexpected operation hash UPDATE error: {hash_update}"
        );
        let stored_operation: (String, String) = db
            .connection()
            .query_row(
                "SELECT operation_id, operation_payload_hash FROM review_events WHERE operation_id = ?1",
                [operation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored_operation, (operation_id.into(), operation_hash));

        let policy: (i64, i64, i64, i64) = db
            .connection()
            .query_row(
                "SELECT edit_basis_points, accept_basis_points, reject_basis_points, skip_basis_points
                   FROM review_compensation_policies",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(policy, (10_000, 1_000, 1_000, 0));
        let ledger: (i64, i64) = db
            .connection()
            .query_row("SELECT COUNT(*), SUM(delta_micro_iqd) FROM review_compensation_ledger", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(ledger, (1, 5_000_000));
        let settlements: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM review_compensation_settlements", [], |row| row.get(0))
            .unwrap();
        assert_eq!(settlements, 1);
    }

    #[test]
    fn v57_refuses_rollback_after_any_post_cutoff_financial_history() {
        // Both branches matter. A normal paid write has a ledger row; a post-cutoff event without
        // one signals interrupted/corrupt accounting and is even less safe to erase by downgrade.
        for history_kind in ["ledger", "unledgered-event"] {
            let db = Database::open(":memory:").unwrap();
            db.initialize().unwrap();
            assert_eq!(rollback(&db, 1).unwrap(), vec![58], "fixture must target v57 rollback semantics");
            db.insert_segment(&crate::db::SpeechSegment {
                id: format!("pay-no-rollback-{history_kind}"),
                audio_path: format!("/pay-no-rollback-{history_kind}.wav"),
                raw_transcript: "دەق".into(),
                duration_ms: 1_000,
                ..Default::default()
            })
            .unwrap();

            if history_kind == "ledger" {
                let revision = db.segment_review_revision("pay-no-rollback-ledger").unwrap().unwrap();
                db.record_phone_human_decision_by_at_revision(
                    "pay-no-rollback-ledger",
                    "edit",
                    Some("دەقی ڕاست"),
                    "Sara",
                    revision,
                )
                .unwrap()
                .unwrap();
            } else {
                db.connection()
                    .execute(
                        "INSERT INTO review_events
                            (segment_id, reviewer, action, compensation_action, source, timestamp_ms, duration_ms)
                         VALUES ('pay-no-rollback-unledgered-event', 'Sara', 'edit', 'edit', 'test', 1, 1000)",
                        [],
                    )
                    .unwrap();
            }
            let event_count_before: i64 =
                db.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
            let ledger_count_before: i64 = db
                .connection()
                .query_row("SELECT COUNT(*) FROM review_compensation_ledger", [], |row| row.get(0))
                .unwrap();

            let error = rollback(&db, 1).expect_err("financial history makes v57 irreversible").to_string();
            assert!(error.contains("CHECK constraint failed"), "unexpected {history_kind} guard error: {error}");
            assert_eq!(get_current_version(&db).unwrap(), 57, "failed rollback must retain its version row");
            let compensation_column: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('review_events') WHERE name='compensation_action'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(compensation_column, 1, "failed rollback must retain the entire v57 schema");
            let counts_after: (i64, i64) = db
                .connection()
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM review_events),
                            (SELECT COUNT(*) FROM review_compensation_ledger)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(counts_after, (event_count_before, ledger_count_before));
            let leaked_guard: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_temp_master
                      WHERE type='table' AND name='review_compensation_rollback_guard'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(leaked_guard, 0, "the failed transactional guard must not poison later connections");
        }
    }

    #[test]
    fn v58_archives_and_removes_exact_known_4208_orphans_idempotently() {
        let db = database_at_v57();
        db.insert_segment(&crate::db::SpeechSegment {
            id: "v58-valid-parent".into(),
            audio_path: "/v58-valid-parent.wav".into(),
            raw_transcript: "دەقی دروست".into(),
            duration_ms: 1_000,
            ..Default::default()
        })
        .unwrap();
        db.connection()
            .execute(
                "INSERT INTO segment_hypotheses
                    (segment_id, model_id, transcript, confidence, created_at, model_version_id)
                 VALUES ('v58-valid-parent', 'champion', 'valid hypothesis', 0.99,
                         '2026-08-21 00:00:00', 'omniasr-7b-test')",
                [],
            )
            .unwrap();
        db.connection()
            .execute(
                "INSERT INTO loop0_shadow_log(id, segment_id, memory_fired, created_at)
                 VALUES (900000, 'v58-valid-parent', 1, '2026-08-21 00:00:01')",
                [],
            )
            .unwrap();

        // Reproduce the cryptographically bound production shape: 2,104 missing-parent hypotheses
        // and the same 2,104 missing-parent LOOP-0 rows, for 4,208 violations total.
        seed_v58_authorized_cohort(&db);
        assert_eq!(foreign_key_violation_count(db.connection()), 4_208);
        db.connection()
            .execute_batch(
                "CREATE TEMP TABLE expected_v58_hypotheses AS
                     SELECT rowid AS original_rowid, segment_id, model_id, transcript, confidence,
                            created_at, model_version_id
                       FROM segment_hypotheses h
                      WHERE NOT EXISTS (SELECT 1 FROM speech_segments s WHERE s.id = h.segment_id);
                 CREATE TEMP TABLE expected_v58_loop0 AS
                     SELECT id, segment_id, memory_fired, created_at
                       FROM loop0_shadow_log l
                      WHERE NOT EXISTS (SELECT 1 FROM speech_segments s WHERE s.id = l.segment_id);",
            )
            .unwrap();

        let expected_hypothesis: (i64, String, String, String, Option<f64>, String, String) = db
            .connection()
            .query_row(
                "SELECT rowid, segment_id, model_id, transcript, confidence, created_at, model_version_id
                   FROM segment_hypotheses WHERE segment_id = '00000000-0000-4000-8000-000000000000'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            )
            .unwrap();
        let expected_loop0: (i64, String, Option<i64>, Option<String>) = db
            .connection()
            .query_row(
                "SELECT id, segment_id, memory_fired, created_at
                   FROM loop0_shadow_log WHERE id = 1997445",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(run_migrations(&db).unwrap(), vec![58]);
        assert_eq!(foreign_key_violation_count(db.connection()), 0);
        let archive_counts: (i64, i64) = db
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM orphan_segment_hypotheses_archive_v58),
                        (SELECT COUNT(*) FROM orphan_loop0_shadow_log_archive_v58)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(archive_counts, (2_104, 2_104), "every one of the 4,208 violations must be archived");
        let live_child_counts: (i64, i64) = db
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM segment_hypotheses),
                        (SELECT COUNT(*) FROM loop0_shadow_log)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(live_child_counts, (1, 1), "valid-parent children must be untouched");

        let archived_hypothesis: (i64, String, String, String, Option<f64>, String, String) = db
            .connection()
            .query_row(
                "SELECT original_rowid, segment_id, model_id, transcript, confidence, created_at, model_version_id
                   FROM orphan_segment_hypotheses_archive_v58
                  WHERE segment_id = '00000000-0000-4000-8000-000000000000'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            )
            .unwrap();
        let archived_loop0: (i64, String, Option<i64>, Option<String>) = db
            .connection()
            .query_row(
                "SELECT id, segment_id, memory_fired, created_at
                   FROM orphan_loop0_shadow_log_archive_v58 WHERE id = 1997445",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(archived_hypothesis, expected_hypothesis, "every hypothesis value and rowid must be exact");
        assert_eq!(archived_loop0, expected_loop0, "every LOOP-0 value and id must be exact");
        let full_archive_symmetric_difference: i64 = db
            .connection()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM (
                        SELECT original_rowid, segment_id, model_id, transcript, confidence,
                               created_at, model_version_id
                          FROM orphan_segment_hypotheses_archive_v58
                        EXCEPT
                        SELECT original_rowid, segment_id, model_id, transcript, confidence,
                               created_at, model_version_id
                          FROM expected_v58_hypotheses
                    ))
                  + (SELECT COUNT(*) FROM (
                        SELECT original_rowid, segment_id, model_id, transcript, confidence,
                               created_at, model_version_id
                          FROM expected_v58_hypotheses
                        EXCEPT
                        SELECT original_rowid, segment_id, model_id, transcript, confidence,
                               created_at, model_version_id
                          FROM orphan_segment_hypotheses_archive_v58
                    ))
                  + (SELECT COUNT(*) FROM (
                        SELECT id, segment_id, memory_fired, created_at
                          FROM orphan_loop0_shadow_log_archive_v58
                        EXCEPT
                        SELECT id, segment_id, memory_fired, created_at FROM expected_v58_loop0
                    ))
                  + (SELECT COUNT(*) FROM (
                        SELECT id, segment_id, memory_fired, created_at FROM expected_v58_loop0
                        EXCEPT
                        SELECT id, segment_id, memory_fired, created_at
                          FROM orphan_loop0_shadow_log_archive_v58
                    ))",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            full_archive_symmetric_difference, 0,
            "all 4,208 archived rows must match the pre-migration source snapshot in both directions"
        );
        let bad_provenance: i64 = db
            .connection()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM orphan_segment_hypotheses_archive_v58
                      WHERE source_table <> 'segment_hypotheses'
                         OR archive_reason <> 'missing speech_segments parent'
                         OR archive_migration_version <> 58
                         OR archived_at = '')
                  + (SELECT COUNT(*) FROM orphan_loop0_shadow_log_archive_v58
                      WHERE source_table <> 'loop0_shadow_log'
                         OR archive_reason <> 'missing speech_segments parent'
                         OR archive_migration_version <> 58
                         OR archived_at = '')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bad_provenance, 0);

        // The migration runner is the idempotency boundary: the second pass is a true no-op and
        // cannot duplicate the archive. The evidence itself is physically immutable afterwards.
        assert!(run_migrations(&db).unwrap().is_empty());
        let archive_counts_after: (i64, i64) = db
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM orphan_segment_hypotheses_archive_v58),
                        (SELECT COUNT(*) FROM orphan_loop0_shadow_log_archive_v58)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(archive_counts_after, archive_counts);
        for sql in [
            "UPDATE orphan_segment_hypotheses_archive_v58 SET transcript = transcript",
            "DELETE FROM orphan_segment_hypotheses_archive_v58",
            "INSERT INTO orphan_segment_hypotheses_archive_v58 SELECT * FROM orphan_segment_hypotheses_archive_v58 LIMIT 1",
            "UPDATE orphan_loop0_shadow_log_archive_v58 SET memory_fired = memory_fired",
            "DELETE FROM orphan_loop0_shadow_log_archive_v58",
        ] {
            let error = db.connection().execute(sql, []).unwrap_err().to_string();
            assert!(error.contains("v58 orphan archive is immutable"), "unexpected archive guard error: {error}");
        }
    }

    #[test]
    fn v58_refuses_an_altered_identity_or_full_source_tuple() {
        let altered_id = database_at_v57();
        seed_v58_authorized_cohort(&altered_id);
        run_with_foreign_keys_off(altered_id.connection(), || {
            altered_id.connection().execute(
                "UPDATE segment_hypotheses SET segment_id='ffffffff-ffff-4fff-8fff-ffffffffffff'
                  WHERE segment_id='00000000-0000-4000-8000-000000000000'",
                [],
            )?;
            altered_id.connection().execute(
                "UPDATE loop0_shadow_log SET segment_id='ffffffff-ffff-4fff-8fff-ffffffffffff'
                  WHERE segment_id='00000000-0000-4000-8000-000000000000'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let identity_error = run_migrations(&altered_id).expect_err("one changed ID must fail the digest");
        assert!(identity_error.to_string().contains("identity digest is not authorized"));
        assert_eq!(get_current_version(&altered_id).unwrap(), 57);

        let altered_tuple = database_at_v57();
        seed_v58_authorized_cohort(&altered_tuple);
        run_with_foreign_keys_off(altered_tuple.connection(), || {
            altered_tuple.connection().execute(
                "UPDATE segment_hypotheses SET transcript = transcript || ' altered'
                  WHERE segment_id='00000000-0000-4000-8000-000000000000'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let tuple_error = run_migrations(&altered_tuple).expect_err("changed transcript evidence must fail");
        assert!(tuple_error.to_string().contains("full source-evidence digest is not authorized"));
        assert_eq!(get_current_version(&altered_tuple).unwrap(), 57);
    }

    #[test]
    fn v58_refuses_wrong_source_shape_and_preexisting_archive_objects() {
        let wrong_shape = database_at_v57();
        seed_v58_authorized_cohort(&wrong_shape);
        run_with_foreign_keys_off(wrong_shape.connection(), || {
            wrong_shape.connection().execute(
                "UPDATE segment_hypotheses SET model_id='different-model'
                  WHERE segment_id='00000000-0000-4000-8000-000000000000'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let shape_error = run_migrations(&wrong_shape).expect_err("wrong source shape must fail closed");
        assert!(shape_error.to_string().contains("do not match the authorized abandoned-import shape"));
        assert_eq!(get_current_version(&wrong_shape).unwrap(), 57);

        let preexisting = database_at_v57();
        preexisting
            .connection()
            .execute("CREATE TABLE orphan_segment_hypotheses_archive_v58(tampered TEXT)", [])
            .unwrap();
        let object_error = run_migrations(&preexisting).expect_err("preexisting archive provenance is ambiguous");
        assert!(object_error.to_string().contains("already exists"), "unexpected error: {object_error}");
        assert_eq!(get_current_version(&preexisting).unwrap(), 57);
        let columns: i64 = preexisting
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('orphan_segment_hypotheses_archive_v58')
                  WHERE name='tampered'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(columns, 1, "failed v58 must not overwrite or drop ambiguous evidence");
    }

    #[test]
    fn v58_refuses_unrelated_fk_damage_and_rolls_back_the_entire_repair() {
        let db = database_at_v57();
        seed_v58_authorized_cohort(&db);
        run_with_foreign_keys_off(db.connection(), || {
            db.connection().execute(
                "INSERT INTO playback_receipts
                    (segment_id, segment_revision, audio_fingerprint, reviewer, session_id,
                     started_at_ms, played_ms, clip_duration_ms, coverage_ratio, policy_version)
                 VALUES ('v58-unrelated-orphan', 1, 'fingerprint', 'Sara', 'session',
                         1, 1000, 1000, 1.0, 1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        assert_eq!(foreign_key_violation_count(db.connection()), 4_209);

        let error = run_migrations(&db).expect_err("an unrecognized FK violation must fail closed").to_string();
        assert!(error.contains("migration v58 left 1 foreign-key violation"), "unexpected v58 error: {error}");
        assert_eq!(get_current_version(&db).unwrap(), 57, "failed repair must not record v58");
        assert_eq!(
            foreign_key_violation_count(db.connection()),
            4_209,
            "the unrelated row and the entire authorized cohort must roll back intact"
        );
        let archive_tables: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'table' AND name IN
                        ('orphan_segment_hypotheses_archive_v58', 'orphan_loop0_shadow_log_archive_v58')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(archive_tables, 0, "a failed transaction must leave no half-created archive");

        // Once an operator separately resolves the unknown class, the same pending migration can
        // safely run and preserve the known orphan. No manual schema surgery or retry flag is needed.
        db.connection().execute("DELETE FROM playback_receipts WHERE segment_id = 'v58-unrelated-orphan'", []).unwrap();
        assert_eq!(run_migrations(&db).unwrap(), vec![58]);
        assert_eq!(foreign_key_violation_count(db.connection()), 0);
        let archived: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM orphan_segment_hypotheses_archive_v58", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, V58_ORPHAN_IDS as i64);
    }

    #[test]
    fn v58_rollback_refuses_missing_parents_then_restores_exact_rows_when_parents_exist() {
        let db = database_at_v57();
        seed_v58_authorized_cohort(&db);
        let expected_hypothesis: (i64, String, String, String, Option<f64>, String, String) = db
            .connection()
            .query_row(
                "SELECT rowid, segment_id, model_id, transcript, confidence, created_at, model_version_id
                   FROM segment_hypotheses WHERE rowid = 2000000",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            )
            .unwrap();
        let expected_loop0: (i64, String, Option<i64>, Option<String>) = db
            .connection()
            .query_row(
                "SELECT id, segment_id, memory_fired, created_at FROM loop0_shadow_log WHERE id = 1997445",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(run_migrations(&db).unwrap(), vec![58]);

        let rollback_error = rollback(&db, 1)
            .expect_err("rollback must not recreate children while their parents are still missing")
            .to_string();
        assert!(rollback_error.contains("CHECK constraint failed"), "unexpected rollback guard: {rollback_error}");
        assert_eq!(get_current_version(&db).unwrap(), 58);
        let preserved_archives: (i64, i64) = db
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM orphan_segment_hypotheses_archive_v58),
                        (SELECT COUNT(*) FROM orphan_loop0_shadow_log_archive_v58)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            preserved_archives,
            (V58_ORPHAN_IDS as i64, V58_ORPHAN_IDS as i64),
            "failed rollback must preserve the complete immutable cohort"
        );
        assert_eq!(foreign_key_violation_count(db.connection()), 0);

        for index in 0..V58_ORPHAN_IDS as i64 {
            let segment_id = v58_fixture_id(index);
            db.insert_segment(&crate::db::SpeechSegment {
                id: segment_id.clone(),
                audio_path: format!("/{segment_id}.wav"),
                raw_transcript: "recovered parent".into(),
                duration_ms: 1_000,
                ..Default::default()
            })
            .unwrap();
        }
        assert_eq!(rollback(&db, 1).unwrap(), vec![58]);
        assert_eq!(get_current_version(&db).unwrap(), 57);
        assert_eq!(foreign_key_violation_count(db.connection()), 0);
        let archive_tables: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'table' AND name IN
                        ('orphan_segment_hypotheses_archive_v58', 'orphan_loop0_shadow_log_archive_v58')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(archive_tables, 0, "successful exact restoration may now remove the archive tables");
        let restored_hypothesis: (i64, String, String, String, Option<f64>, String, String) = db
            .connection()
            .query_row(
                "SELECT rowid, segment_id, model_id, transcript, confidence, created_at, model_version_id
                   FROM segment_hypotheses WHERE rowid = 2000000",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            )
            .unwrap();
        let restored_loop0: (i64, String, Option<i64>, Option<String>) = db
            .connection()
            .query_row(
                "SELECT id, segment_id, memory_fired, created_at FROM loop0_shadow_log WHERE id = 1997445",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(restored_hypothesis, expected_hypothesis);
        assert_eq!(restored_loop0, expected_loop0);

        // Re-applying v58 after a safe rollback sees valid parents, archives nothing, and leaves both
        // restored children in place. This pins the full up/down/up round trip.
        assert_eq!(run_migrations(&db).unwrap(), vec![58]);
        let reapply_counts: (i64, i64, i64, i64) = db
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM segment_hypotheses
                          WHERE segment_id LIKE '00000000-0000-4000-8000-%'),
                        (SELECT COUNT(*) FROM loop0_shadow_log
                          WHERE segment_id LIKE '00000000-0000-4000-8000-%'),
                        (SELECT COUNT(*) FROM orphan_segment_hypotheses_archive_v58),
                        (SELECT COUNT(*) FROM orphan_loop0_shadow_log_archive_v58)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(reapply_counts, (V58_ORPHAN_IDS as i64, V58_ORPHAN_IDS as i64, 0, 0));
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

    #[test]
    fn a_failed_multi_statement_rollback_leaves_the_schema_unchanged() {
        // The FK-off rollback branch already runs down_sql in ONE transaction, and its comment spells out
        // why: a bare execute_batch auto-commits statement-by-statement, so a mid-batch failure leaves the
        // schema half-reverted. The sibling non-FK-off branch did exactly that bare execute_batch. Several
        // down_sql bodies are multi-statement (v6/v9/v17/v22/v25/v31/v36/v37), so a failure partway left
        // the schema mutated while schema_migrations still recorded the version as applied -- run_migrations
        // then skips it forever, with no self-heal path.
        //
        // Deterministic injection using REAL migration data: v6's down_sql drops clipping_ratio, rms_db and
        // snr_db (no IF EXISTS). Dropping snr_db up front makes the THIRD statement fail after the first two
        // have already succeeded -- the exact partial-apply window.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        run_migrations(&db).unwrap();

        let has_column = |name: &str| -> bool {
            let conn = db.connection();
            let mut stmt = conn.prepare("SELECT 1 FROM pragma_table_info('speech_segments') WHERE name = ?1").unwrap();
            stmt.exists(rusqlite::params![name]).unwrap()
        };

        // Pin current version at 6 so rollback(1) targets v6, then poison its final down statement.
        db.connection().execute("DELETE FROM schema_migrations WHERE version > 6", []).unwrap();
        db.connection().execute_batch("ALTER TABLE speech_segments DROP COLUMN snr_db;").unwrap();
        assert_eq!(get_current_version(&db).unwrap(), 6, "test must target v6");
        assert!(has_column("clipping_ratio") && has_column("rms_db"), "fixture columns must exist up front");

        let result = rollback(&db, 1);

        assert!(result.is_err(), "the poisoned third down statement must fail the rollback, got {result:?}");
        // The whole down_sql must roll back as one unit: the first two DROP COLUMNs must NOT have stuck.
        assert!(has_column("clipping_ratio"), "a failed rollback must not leave clipping_ratio dropped");
        assert!(has_column("rms_db"), "a failed rollback must not leave rms_db dropped");
        assert_eq!(get_current_version(&db).unwrap(), 6, "schema and recorded version must stay consistent");
    }
}
