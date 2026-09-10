//! Training-only quality holds. Reviews, playback, compensation and source audio are untouched.
//! Holds and manual clearances are append-only SQLite authority, included in restore floors.
//! An overlapping reimport of the same canonical PCM cannot evade a hold by changing its row ID.

use crate::db::Database;
use crate::error::{AppError, AppResult};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuarantineMember {
    pub segment_id: String,
    pub audio_content_hash: String,
    pub source_start_ms: i64,
    pub source_end_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuarantinePlan {
    pub schema_version: u32,
    pub batch_id: String,
    pub reason: String,
    pub evidence_sha256: String,
    pub members: Vec<QuarantineMember>,
}

fn valid_sha(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn validate_reason(reason: &str, evidence: &str) -> AppResult<()> {
    if reason.trim().is_empty() || reason.chars().count() > 2000 || !valid_sha(evidence) {
        return Err(AppError::Validation("quarantine requires a reason and exact SHA-256 evidence".into()));
    }
    Ok(())
}

fn identity(db: &Database, id: &str) -> AppResult<QuarantineMember> {
    Ok(db.connection().query_row(
        "SELECT id,audio_content_hash,json_extract(alignment_json,'$.source_start_ms'),json_extract(alignment_json,'$.source_end_ms') FROM speech_segments WHERE id=?1",
        [id],
        |r| {
            Ok(QuarantineMember {
                segment_id: r.get(0)?,
                audio_content_hash: r.get(1)?,
                source_start_ms: r.get(2)?,
                source_end_ms: r.get(3)?,
            })
        },
    )?)
}

impl QuarantinePlan {
    fn validate(&self) -> AppResult<()> {
        validate_reason(&self.reason, &self.evidence_sha256)?;
        if self.schema_version != 1
            || uuid::Uuid::parse_str(&self.batch_id).is_err()
            || self.members.is_empty()
            || self.members.len() > 100_000
            || self.members.windows(2).any(|w| w[0].segment_id >= w[1].segment_id)
            || self.members.iter().any(|m| {
                m.segment_id.trim().is_empty()
                    || !valid_sha(&m.audio_content_hash)
                    || m.source_start_ms < 0
                    || m.source_end_ms <= m.source_start_ms
            })
        {
            return Err(AppError::Validation("invalid, unordered or duplicate quarantine manifest members".into()));
        }
        Ok(())
    }

    pub fn sha256(&self) -> AppResult<String> {
        self.validate()?;
        Ok(Sha256::digest(serde_json::to_vec(self)?).iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

/// Read-only plan; application re-proves every source identity in one transaction.
pub fn prepare(db: &Database, ids: &[String], reason: &str, evidence_sha256: &str) -> AppResult<QuarantinePlan> {
    let unique: BTreeSet<_> = ids.iter().collect();
    if unique.len() != ids.len() {
        return Err(AppError::Validation("duplicate quarantine input IDs".into()));
    }
    let plan = QuarantinePlan {
        schema_version: 1,
        batch_id: uuid::Uuid::new_v4().to_string(),
        reason: reason.into(),
        evidence_sha256: evidence_sha256.into(),
        members: unique.into_iter().map(|id| identity(db, id)).collect::<AppResult<_>>()?,
    };
    plan.validate()?;
    Ok(plan)
}

/// Returns false only for an exact complete retry. Never recreates a cleared hold on retry.
pub fn apply(db: &Database, plan: &QuarantinePlan) -> AppResult<bool> {
    db.with_full_sync(|| apply_inner(db, plan))
}

fn apply_inner(db: &Database, plan: &QuarantinePlan) -> AppResult<bool> {
    let digest = plan.sha256()?;
    validate_history(db)?;
    let transaction = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)?;
    let existing: (i64, i64) = transaction.query_row(
        "SELECT COUNT(*),COALESCE(SUM(plan_sha256=?2),0) FROM training_quarantine_holds WHERE batch_id=?1",
        params![plan.batch_id, digest],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if existing.0 > 0 {
        if existing.0 != plan.members.len() as i64 || existing.1 != existing.0 {
            return Err(AppError::Validation(
                "quarantine batch ID reused with different or incomplete authority".into(),
            ));
        }
        transaction.commit()?;
        return Ok(false);
    }
    for member in &plan.members {
        if identity(db, &member.segment_id)? != *member {
            return Err(AppError::Validation(format!("{}: quarantine source identity changed", member.segment_id)));
        }
        transaction.execute(
            "INSERT INTO training_quarantine_holds(batch_id,segment_id,audio_content_hash,source_start_ms,source_end_ms,reason,evidence_sha256,plan_sha256)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![plan.batch_id, member.segment_id, member.audio_content_hash, member.source_start_ms,
                member.source_end_ms, plan.reason, plan.evidence_sha256, digest],
        )?;
    }
    transaction.commit()?;
    Ok(true)
}

/// Privileged offline operator only; a fresh manual listening assessment must identify its evidence.
/// Clearing one hold does not clear any other hold that covers the same recording.
pub fn clear(db: &Database, batch_id: &str, segment_id: &str, reason: &str, evidence: &str) -> AppResult<bool> {
    db.with_full_sync(|| clear_inner(db, batch_id, segment_id, reason, evidence))
}

fn clear_inner(db: &Database, batch_id: &str, segment_id: &str, reason: &str, evidence: &str) -> AppResult<bool> {
    validate_reason(reason, evidence)?;
    validate_history(db)?;
    let transaction = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)?;
    let existing: Option<(String, String)> = transaction
        .query_row(
            "SELECT reason,evidence_sha256 FROM training_quarantine_clearances WHERE batch_id=?1 AND segment_id=?2",
            params![batch_id, segment_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some(old) = existing {
        if old != (reason.into(), evidence.into()) {
            return Err(AppError::Validation("manual clearance already has different evidence".into()));
        }
        transaction.commit()?;
        return Ok(false);
    }
    let held = transaction.query_row(
        "SELECT segment_id,audio_content_hash,source_start_ms,source_end_ms FROM training_quarantine_holds WHERE batch_id=?1 AND segment_id=?2",
        params![batch_id, segment_id], |r| Ok(QuarantineMember {
            segment_id: r.get(0)?, audio_content_hash: r.get(1)?, source_start_ms: r.get(2)?, source_end_ms: r.get(3)?,
        }),
    )?;
    if identity(db, segment_id)? != held {
        return Err(AppError::Validation(
            "manual clearance refers to changed audio identity; reassess the current clip".into(),
        ));
    }
    transaction.execute(
        "INSERT INTO training_quarantine_clearances(batch_id,segment_id,reason,evidence_sha256) VALUES(?1,?2,?3,?4)",
        params![batch_id, segment_id, reason, evidence],
    )?;
    transaction.commit()?;
    Ok(true)
}

/// Capture once per batch. Historical schemas fail closed for training: they cannot represent holds.
/// Includes all rows/clearances in the digest so even add-then-clear drift invalidates publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrainingBoundary {
    pub(crate) blocked: BTreeSet<String>,
    pub(crate) sha256: String,
}

impl TrainingBoundary {
    pub(crate) fn capture(db: &Database) -> AppResult<Self> {
        validate_history(db)?;
        let mut digest = Sha256::new();
        for query in [
            "SELECT json_array(batch_id,segment_id,audio_content_hash,source_start_ms,source_end_ms,reason,evidence_sha256,plan_sha256,created_at) FROM training_quarantine_holds ORDER BY batch_id,segment_id",
            "SELECT json_array(batch_id,segment_id,reason,evidence_sha256,created_at) FROM training_quarantine_clearances ORDER BY batch_id,segment_id",
        ] {
            digest.update(query.as_bytes());
            let mut statement = db.connection().prepare(query)?;
            for row in statement.query_map([], |r| r.get::<_, String>(0))? {
                let value = row?;
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
        }
        let mut statement = db.connection().prepare("SELECT segment_id FROM active_training_quarantined_segments")?;
        let blocked = statement.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<BTreeSet<_>, _>>()?;
        Ok(Self { blocked, sha256: digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect() })
    }

    pub(crate) fn verify(&self, db: &Database) -> AppResult<()> {
        if *self != Self::capture(db)? {
            return Err(AppError::Validation("training quarantine changed before publication".into()));
        }
        Ok(())
    }
}

pub fn blocked_segment_ids(db: &Database) -> AppResult<BTreeSet<String>> {
    Ok(TrainingBoundary::capture(db)?.blocked)
}

/// Startup and restore proof: removing a member or editing a reason cannot preserve the bound batch.
pub(crate) fn validate_history(db: &Database) -> AppResult<()> {
    let mut statement =
        db.connection().prepare("SELECT DISTINCT batch_id FROM training_quarantine_holds ORDER BY batch_id")?;
    let batches = statement.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    for batch in batches {
        let (reason, evidence, digest): (String, String, String) = db.connection().query_row(
            "SELECT reason,evidence_sha256,plan_sha256 FROM training_quarantine_holds WHERE batch_id=?1 LIMIT 1",
            [&batch],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mismatches: i64 = db.connection().query_row(
            "SELECT COUNT(*) FROM training_quarantine_holds WHERE batch_id=?1 AND (reason<>?2 OR evidence_sha256<>?3 OR plan_sha256<>?4)",
            params![batch, reason, evidence, digest], |r| r.get(0),
        )?;
        let mut members = db.connection().prepare("SELECT segment_id,audio_content_hash,source_start_ms,source_end_ms FROM training_quarantine_holds WHERE batch_id=?1 ORDER BY segment_id")?;
        let members = members
            .query_map([&batch], |r| {
                Ok(QuarantineMember {
                    segment_id: r.get(0)?,
                    audio_content_hash: r.get(1)?,
                    source_start_ms: r.get(2)?,
                    source_end_ms: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let plan = QuarantinePlan { schema_version: 1, batch_id: batch, reason, evidence_sha256: evidence, members };
        if mismatches != 0 || plan.sha256()? != digest {
            return Err(AppError::Validation("training quarantine batch is incomplete or altered".into()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        for (id, start, end) in [("original", 100, 200), ("alias", 150, 250), ("adjacent", 200, 300)] {
            db.connection().execute("INSERT INTO speech_segments(id,audio_path,raw_transcript,audio_content_hash,alignment_json,duration_ms) VALUES(?1,'fixture.wav','test',?2,json_object('source_start_ms',?3,'source_end_ms',?4),?4-?3)", params![id, "a".repeat(64), start, end]).unwrap();
        }
        db
    }

    fn plan(db: &Database) -> QuarantinePlan {
        prepare(db, &["original".into()], "Uncertain acoustic candidate, not a confirmed duplicate", &"b".repeat(64))
            .unwrap()
    }

    #[test]
    fn holds_block_overlapping_reimports_but_not_adjacent_audio_and_preserve_history() {
        let db = fixture();
        let original = serde_json::to_string(&db.get_segments(None).unwrap()).unwrap();
        let plan = plan(&db);
        let before = TrainingBoundary::capture(&db).unwrap();
        assert!(apply(&db, &plan).unwrap());
        assert!(!apply(&db, &plan).unwrap());
        assert_eq!(blocked_segment_ids(&db).unwrap(), BTreeSet::from(["original".into(), "alias".into()]));
        assert!(before.verify(&db).is_err());
        assert_eq!(original, serde_json::to_string(&db.get_segments(None).unwrap()).unwrap());
        for table in ["review_events", "review_compensation_ledger", "review_pool_decisions"] {
            assert_eq!(
                db.connection()
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        validate_history(&db).unwrap();
        assert!(crate::migrations::rollback(&db, 1).is_err());
        assert!(db.connection().execute("DELETE FROM training_quarantine_holds", []).is_err());
        assert!(db.connection().execute("UPDATE training_quarantine_holds SET reason='changed'", []).is_err());
    }

    #[test]
    fn manual_clearance_is_exact_append_only_and_does_not_clear_other_holds() {
        let db = fixture();
        let first = plan(&db);
        let second = plan(&db);
        apply(&db, &first).unwrap();
        apply(&db, &second).unwrap();
        let held = TrainingBoundary::capture(&db).unwrap();
        assert!(clear(&db, &first.batch_id, "original", "Manual audio assessment", &"c".repeat(64)).unwrap());
        assert!(!clear(&db, &first.batch_id, "original", "Manual audio assessment", &"c".repeat(64)).unwrap());
        assert!(clear(&db, &first.batch_id, "original", "Different assessment", &"c".repeat(64)).is_err());
        assert_eq!(blocked_segment_ids(&db).unwrap().len(), 2);
        clear(&db, &second.batch_id, "original", "Manual audio assessment", &"d".repeat(64)).unwrap();
        assert!(blocked_segment_ids(&db).unwrap().is_empty());
        assert!(held.verify(&db).is_err());
        assert!(!apply(&db, &first).unwrap());
        assert!(blocked_segment_ids(&db).unwrap().is_empty());
        assert!(crate::migrations::rollback(&db, 1).is_err(), "even cleared history survives rollback");
    }

    #[test]
    fn stale_or_duplicate_or_reused_plans_fail_atomically() {
        let db = fixture();
        let mut stale = prepare(&db, &["alias".into(), "original".into()], "assessment", &"b".repeat(64)).unwrap();
        stale.members[1].source_end_ms += 1;
        assert!(apply(&db, &stale).is_err());
        assert!(blocked_segment_ids(&db).unwrap().is_empty());
        assert!(prepare(&db, &["alias".into(), "alias".into()], "assessment", &"b".repeat(64)).is_err());
        let mut valid = plan(&db);
        apply(&db, &valid).unwrap();
        valid.reason = "different assessment".into();
        assert!(apply(&db, &valid).is_err());
        assert!(clear(&db, &uuid::Uuid::new_v4().to_string(), "original", "manual", &"b".repeat(64)).is_err());
    }

    #[test]
    fn incomplete_or_invalid_alias_span_is_held_conservatively() {
        let db = fixture();
        apply(&db, &plan(&db)).unwrap();
        for alignment in
            ["{}", r#"{"source_start_ms":600,"source_end_ms":500}"#, r#"{"source_start_ms":"200","source_end_ms":300}"#]
        {
            db.connection()
                .execute("UPDATE speech_segments SET alignment_json=?1 WHERE id='adjacent'", [alignment])
                .unwrap();
            assert!(blocked_segment_ids(&db).unwrap().contains("adjacent"));
        }
    }

    #[test]
    fn manual_clearance_cannot_release_a_replaced_recording() {
        let db = fixture();
        let plan = plan(&db);
        apply(&db, &plan).unwrap();
        db.connection()
            .execute("UPDATE speech_segments SET audio_content_hash=?1 WHERE id='original'", [&"e".repeat(64)])
            .unwrap();
        assert!(clear(&db, &plan.batch_id, "original", "Old audio assessment", &"f".repeat(64))
            .unwrap_err()
            .to_string()
            .contains("changed audio identity"));
        assert!(blocked_segment_ids(&db).unwrap().contains("original"));
    }

    #[test]
    fn quarantine_filters_memory_use_without_erasing_feedback_or_stored_memories() {
        let db = fixture();
        crate::migrations::rollback(&db, (crate::migrations::max_supported_version() - 59) as usize).unwrap();
        db.connection().execute("INSERT INTO correction_memory(id,wrong_token,human_token,slot_key,phonetic_key,source_segment,confidence,hit_count) VALUES('memory','wrong','right','slot','phonetic','original',0.9,3)", []).unwrap();
        crate::migrations::run_migrations(&db).unwrap();
        assert_eq!(db.load_correction_memories().unwrap().len(), 1);
        apply(&db, &plan(&db)).unwrap();
        assert!(db.load_correction_memories().unwrap().is_empty());
        assert_eq!(
            db.connection()
                .query_row("SELECT COUNT(*) FROM correction_memory WHERE id='memory'", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn missing_tables_and_tampered_batch_fail_closed_and_restore_cannot_erase_holds() {
        let floor = fixture();
        let target = fixture();
        apply(&floor, &plan(&floor)).unwrap();
        assert!(crate::restore_service::require_durable_review_history_superset(&floor, &target)
            .unwrap_err()
            .contains("training_quarantine_holds"));
        assert!(crate::restore_service::has_durable_review_activity(&floor).unwrap());
        floor.connection().execute_batch("DROP TRIGGER training_quarantine_holds_no_update; UPDATE training_quarantine_holds SET reason='tampered';").unwrap();
        assert!(validate_history(&floor).is_err());
        crate::migrations::rollback(&target, 1).unwrap();
        assert!(blocked_segment_ids(&target).is_err());
    }
}
