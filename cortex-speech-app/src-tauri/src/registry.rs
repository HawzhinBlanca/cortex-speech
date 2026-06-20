//! Model registry — the safe control surface for ingesting and promoting ASR model versions.
//!
//! The schema (migration v23) enforces the hard invariants at the DB level: at most one
//! `champion` per family (a partial unique index) and a closed `source`/`status` vocabulary
//! (CHECK constraints). This module is the gated control surface on top of that:
//!
//!   * **Import gate.** Registering a *user/cortex* fine-tune REQUIRES a non-empty checkpoint
//!     content hash. The architecture doc calls this out as a bug-class to close: an empty pin
//!     sails through `models::verify_extracted_against_pin` (which treats an empty pin as OK,
//!     correct for first-seed bootstrap) and would otherwise reach the trusted promotion path
//!     unverifiable. Stock seeds may still carry an empty pin during bootstrap, so the gate is
//!     scoped to externally-produced checkpoints.
//!   * **No import-straight-to-champion.** A freshly imported version is always a `candidate`;
//!     becoming `champion` is a separate, gated promotion — never a side effect of import.
//!   * **Atomic promotion.** Promoting a version demotes the prior champion of its family in the
//!     same transaction, so the one-champion invariant is never even momentarily tripped.

use crate::db::Database;
use crate::error::{AppError, AppResult};
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};

/// A row of the `model_versions` registry (the columns callers actually read back).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelVersion {
    pub id: String,
    pub family: String,
    pub model_card_name: Option<String>,
    pub checkpoint_sha256: String,
    pub checkpoint_path: String,
    pub source: String,
    pub license: String,
    pub status: String,
}

/// What a caller supplies to register a new candidate. `status` is deliberately absent — a
/// freshly imported version is always a `candidate`; promotion is a separate gated step.
#[derive(Debug, Clone)]
pub struct NewModelVersion {
    pub id: String,
    pub family: String,
    pub model_card_name: Option<String>,
    pub checkpoint_sha256: String,
    pub checkpoint_path: String,
    pub source: String,
    pub license: String,
}

/// Sources whose checkpoints originate OUTSIDE the trusted stock seed and therefore MUST carry
/// a non-empty content hash before they may enter the registry. Stock seeds (`meta-stock`) may
/// legitimately have an empty pin during first-seed bootstrap.
fn requires_nonempty_pin(source: &str) -> bool {
    matches!(source, "user-finetuned" | "cortex-finetuned")
}

const SELECT_COLS: &str =
    "id, family, model_card_name, checkpoint_sha256, checkpoint_path, source, license, status";

fn map_version(row: &Row) -> rusqlite::Result<ModelVersion> {
    Ok(ModelVersion {
        id: row.get(0)?,
        family: row.get(1)?,
        model_card_name: row.get(2)?,
        checkpoint_sha256: row.get(3)?,
        checkpoint_path: row.get(4)?,
        source: row.get(5)?,
        license: row.get(6)?,
        status: row.get(7)?,
    })
}

/// Register a new model version as a `candidate`. Enforces the non-empty-pin import gate for
/// externally-produced checkpoints. The DB's CHECK constraint independently rejects an unknown
/// `source`, so an invalid source is refused even if this gate is bypassed.
pub fn register_candidate(db: &Database, new: &NewModelVersion) -> AppResult<()> {
    if requires_nonempty_pin(&new.source) && new.checkpoint_sha256.trim().is_empty() {
        return Err(AppError::Validation(format!(
            "refusing to register {} model '{}' with an empty checkpoint_sha256: a fine-tuned \
             checkpoint must be content-hash pinned before it can be trusted for promotion",
            new.source, new.id
        )));
    }
    db.connection().execute(
        "INSERT INTO model_versions
            (id, family, model_card_name, checkpoint_sha256, checkpoint_path, source, license, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'candidate')",
        params![
            new.id,
            new.family,
            new.model_card_name,
            new.checkpoint_sha256,
            new.checkpoint_path,
            new.source,
            new.license,
        ],
    )?;
    Ok(())
}

/// Promote a registered version to `champion`, atomically demoting the prior champion of the
/// same family to `rolled_back`. The whole swap is one transaction so the one-champion-per-family
/// invariant is never tripped, even momentarily.
pub fn promote_to_champion(db: &Database, id: &str) -> AppResult<()> {
    let conn = db.connection();
    let tx = conn.unchecked_transaction()?;

    let family: String = tx
        .query_row("SELECT family FROM model_versions WHERE id = ?1", params![id], |r| r.get(0))
        .map_err(|_| AppError::Validation(format!("cannot promote unknown model version '{id}'")))?;

    // Demote the incumbent champion of this family (if any other row holds it).
    tx.execute(
        "UPDATE model_versions SET status = 'rolled_back'
         WHERE family = ?1 AND status = 'champion' AND id <> ?2",
        params![family, id],
    )?;
    // Crown the new champion.
    tx.execute(
        "UPDATE model_versions SET status = 'champion', promoted_at = datetime('now') WHERE id = ?1",
        params![id],
    )?;
    tx.commit()?;
    Ok(())
}

/// The current champion for a family, if one is crowned.
pub fn get_champion(db: &Database, family: &str) -> AppResult<Option<ModelVersion>> {
    let conn = db.connection();
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLS} FROM model_versions WHERE family = ?1 AND status = 'champion'"
    ))?;
    let mut rows = stmt.query(params![family])?;
    match rows.next()? {
        Some(row) => Ok(Some(map_version(row)?)),
        None => Ok(None),
    }
}

/// Fetch a single model version by id.
pub fn get_model_version(db: &Database, id: &str) -> AppResult<Option<ModelVersion>> {
    let conn = db.connection();
    let mut stmt =
        conn.prepare(&format!("SELECT {SELECT_COLS} FROM model_versions WHERE id = ?1"))?;
    let mut rows = stmt.query(params![id])?;
    match rows.next()? {
        Some(row) => Ok(Some(map_version(row)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db
    }

    fn candidate(id: &str, family: &str, source: &str, sha: &str) -> NewModelVersion {
        NewModelVersion {
            id: id.to_string(),
            family: family.to_string(),
            model_card_name: None,
            checkpoint_sha256: sha.to_string(),
            checkpoint_path: "/models/x.pt".to_string(),
            source: source.to_string(),
            license: "Apache-2.0".to_string(),
        }
    }

    #[test]
    fn import_gate_rejects_empty_pin_for_finetuned_sources() {
        let db = open();
        // A user/cortex fine-tune with no content hash is refused at the gate...
        assert!(register_candidate(&db, &candidate("u1", "omniasr-7b", "user-finetuned", "")).is_err());
        assert!(register_candidate(&db, &candidate("c1", "omniasr-7b", "cortex-finetuned", "   ")).is_err());
        // ...and nothing was written.
        assert!(get_model_version(&db, "u1").unwrap().is_none());
        assert!(get_model_version(&db, "c1").unwrap().is_none());
    }

    #[test]
    fn import_gate_allows_empty_pin_for_stock_seed() {
        let db = open();
        // A stock seed may bootstrap with an empty pin (its archive hash is computed later).
        register_candidate(&db, &candidate("s1", "omniasr-ctc-1b", "meta-stock", "")).unwrap();
        let v = get_model_version(&db, "s1").unwrap().unwrap();
        assert_eq!(v.status, "candidate", "a freshly registered version is always a candidate");
    }

    #[test]
    fn register_never_imports_straight_to_champion() {
        let db = open();
        register_candidate(&db, &candidate("u1", "omniasr-7b", "user-finetuned", "sha-abc")).unwrap();
        let v = get_model_version(&db, "u1").unwrap().unwrap();
        assert_eq!(v.status, "candidate");
        assert!(get_champion(&db, "omniasr-7b").unwrap().is_none(), "no champion exists until promotion");
    }

    #[test]
    fn promotion_atomically_swaps_the_family_champion() {
        let db = open();
        register_candidate(&db, &candidate("v1", "omniasr-7b", "meta-stock", "sha1")).unwrap();
        register_candidate(&db, &candidate("v2", "omniasr-7b", "user-finetuned", "sha2")).unwrap();

        promote_to_champion(&db, "v1").unwrap();
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "v1");

        // Promoting v2 must demote v1 in the same step — never two champions in one family.
        promote_to_champion(&db, "v2").unwrap();
        let champ = get_champion(&db, "omniasr-7b").unwrap().unwrap();
        assert_eq!(champ.id, "v2", "the newly promoted version is champion");
        assert_eq!(get_model_version(&db, "v1").unwrap().unwrap().status, "rolled_back",
            "the prior champion is rolled back, not left as a second champion");
    }

    #[test]
    fn champions_are_independent_across_families() {
        let db = open();
        register_candidate(&db, &candidate("o1", "omniasr-7b", "meta-stock", "sha1")).unwrap();
        register_candidate(&db, &candidate("w1", "whisper-ckb", "meta-stock", "sha2")).unwrap();
        promote_to_champion(&db, "o1").unwrap();
        promote_to_champion(&db, "w1").unwrap();
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "o1");
        assert_eq!(get_champion(&db, "whisper-ckb").unwrap().unwrap().id, "w1");
    }

    #[test]
    fn promoting_an_unknown_version_errors_and_changes_nothing() {
        let db = open();
        register_candidate(&db, &candidate("v1", "omniasr-7b", "meta-stock", "sha1")).unwrap();
        promote_to_champion(&db, "v1").unwrap();
        assert!(promote_to_champion(&db, "ghost").is_err(), "promoting a nonexistent id must error");
        // The real champion is untouched.
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "v1");
    }
}
