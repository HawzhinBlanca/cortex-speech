//! Append-only record of every review-pool export batch (schema 73, owner 2026-09-15: "add the batch
//! record inside the database too … make sure we dont export earlier exports").
//!
//! A batch binds each delivered clip to the pool's audio identity, the resolution evidence and the
//! exact text the export shipped, and the trust policy that decided it. A later `--batch` export
//! skips a clip only when the SAME evidence (or, for a legacy artifact, the same text) was delivered;
//! a clip whose authority changed since is re-delivered and marked so (audit 2026-09-15: 64 clips of
//! the 2026-09-08 TTS test carried a different text than the canon now yields). History never
//! changes: triggers refuse UPDATE/DELETE, rollback refuses while any batch exists.

use crate::db::Database;
use crate::error::{AppError, AppResult};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const SCHEMA_VERSION: i64 = 73;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Exported,
    SkippedPreviouslyExported,
    ReExportedChangedAuthority,
}

impl Disposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Disposition::Exported => "exported",
            Disposition::SkippedPreviouslyExported => "skipped-previously-exported",
            Disposition::ReExportedChangedAuthority => "re-exported-changed-authority",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "exported" => Some(Disposition::Exported),
            "skipped-previously-exported" => Some(Disposition::SkippedPreviouslyExported),
            "re-exported-changed-authority" => Some(Disposition::ReExportedChangedAuthority),
            _ => None,
        }
    }
    pub fn delivered(self) -> bool {
        !matches!(self, Disposition::SkippedPreviouslyExported)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportBatchMember {
    pub segment_id: String,
    /// `None` only for a legacy backfill whose artifact carried no per-clip evidence digest.
    pub resolution_evidence_sha256: Option<String>,
    /// SHA-256 of the exact exported text (`text` in asr/metadata.jsonl); `None` if unknown.
    pub transcript_sha256: Option<String>,
    pub disposition: Disposition,
}

#[derive(Debug, Clone)]
pub struct ExportBatchRecord {
    pub batch_id: String,
    pub voice_name: String,
    pub legacy: bool,
    pub export_manifest_sha256: String,
    pub certificate_sha256: Option<String>,
    pub output_dir: String,
    pub total_duration_ms: i64,
    pub created_at_ms: i64,
    /// `trust::describe()` at export time (owner + trusted names): the policy that decided the batch.
    pub trust_policy: serde_json::Value,
    pub members: Vec<ExportBatchMember>,
}

/// What an earlier batch delivered for one clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered {
    pub batch_id: String,
    pub resolution_evidence_sha256: Option<String>,
    pub transcript_sha256: Option<String>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn valid_sha(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn supported(db: &Database) -> AppResult<bool> {
    let version: i64 =
        db.connection().query_row("SELECT COALESCE(MAX(version),0) FROM schema_migrations", [], |r| r.get(0))?;
    Ok(version >= SCHEMA_VERSION)
}

fn require_supported(db: &Database) -> AppResult<()> {
    if supported(db)? {
        Ok(())
    } else {
        Err(AppError::Validation("export batches require schema 73".into()))
    }
}

/// Everything earlier batches of `pool_id` delivered, per clip (skipped rows are not deliveries).
/// Fails closed below schema 73: a caller that wants to skip earlier exports must not silently get
/// "nothing was exported".
pub fn delivered(db: &Database, pool_id: &str) -> AppResult<HashMap<String, Vec<Delivered>>> {
    require_supported(db)?;
    let mut statement = db.connection().prepare(
        "SELECT m.segment_id, m.batch_id, m.resolution_evidence_sha256, m.transcript_sha256
           FROM review_pool_export_batch_members m JOIN review_pool_export_batches b ON b.batch_id=m.batch_id
          WHERE b.pool_id=?1 AND m.disposition<>'skipped-previously-exported'
          ORDER BY b.created_at_ms, m.batch_id",
    )?;
    let mut out: HashMap<String, Vec<Delivered>> = HashMap::new();
    for row in statement.query_map([pool_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Delivered { batch_id: r.get(1)?, resolution_evidence_sha256: r.get(2)?, transcript_sha256: r.get(3)? },
        ))
    })? {
        let (id, delivered) = row?;
        out.entry(id).or_default().push(delivered);
    }
    Ok(out)
}

/// Skip only when an earlier batch delivered this exact authority: the same resolution evidence,
/// or (legacy artifact without evidence) the same exported text. Anything else is re-delivered.
pub fn already_delivered(history: &[Delivered], evidence_sha256: &str, transcript_sha256: &str) -> bool {
    history.iter().any(|d| match (&d.resolution_evidence_sha256, &d.transcript_sha256) {
        (Some(evidence), _) => evidence == evidence_sha256,
        (None, Some(text)) => text == transcript_sha256,
        (None, None) => false,
    })
}

/// Record a batch. Idempotent for the same batch id + manifest digest (`Ok(false)`); a different
/// manifest under a known id is refused, so a batch id names exactly one artifact forever.
pub fn record(db: &Database, record: &ExportBatchRecord) -> AppResult<bool> {
    require_supported(db)?;
    let id = record.batch_id.trim();
    if id.len() < 8 || id.len() > 120 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) {
        return Err(AppError::Validation("export batch id must be 8-120 characters of [A-Za-z0-9._-]".into()));
    }
    if !valid_sha(&record.export_manifest_sha256)
        || record.certificate_sha256.as_deref().is_some_and(|s| !valid_sha(s))
        || record.members.iter().any(|m| {
            m.resolution_evidence_sha256.as_deref().is_some_and(|s| !valid_sha(s))
                || m.transcript_sha256.as_deref().is_some_and(|s| !valid_sha(s))
        })
    {
        return Err(AppError::Validation("export batch digests must be lowercase SHA-256 hex".into()));
    }
    if record.members.is_empty() || record.total_duration_ms < 0 || record.created_at_ms <= 0 {
        return Err(AppError::Validation("export batch needs members, a duration and a creation time".into()));
    }
    if !record.legacy
        && record.members.iter().any(|m| m.resolution_evidence_sha256.is_none() || m.transcript_sha256.is_none())
    {
        return Err(AppError::Validation("an approved-subset batch binds evidence and text for every member".into()));
    }
    let policy_json = serde_json::to_string(&record.trust_policy).map_err(|e| AppError::Other(e.to_string()))?;
    if policy_json.len() > 4000 || !record.trust_policy.is_object() {
        return Err(AppError::Validation("trust policy must be a small JSON object".into()));
    }
    let policy_sha = sha256_hex(policy_json.as_bytes());
    let pool = crate::review_pool::load(db)
        .map_err(AppError::Validation)?
        .ok_or_else(|| AppError::Validation("review pool is not active".into()))?;
    db.with_full_sync(|| {
        let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row("SELECT export_manifest_sha256 FROM review_pool_export_batches WHERE batch_id=?1", [id], |r| r.get(0))
            .optional()?;
        if let Some(existing) = existing {
            if existing == record.export_manifest_sha256 {
                tx.commit()?;
                return Ok(false);
            }
            return Err(AppError::Validation(format!("export batch {id} already names a different artifact")));
        }
        let exported = record.members.iter().filter(|m| m.disposition.delivered()).count();
        tx.execute(
            "INSERT INTO review_pool_export_batches(batch_id,pool_id,voice_name,kind,export_manifest_sha256,certificate_sha256,
                output_dir,exported_segments,skipped_segments,total_duration_ms,trust_policy_sha256,trust_policy_json,app_git_sha,created_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                id,
                pool.pool_id,
                record.voice_name.trim(),
                if record.legacy { "legacy" } else { "approved-subset" },
                record.export_manifest_sha256,
                record.certificate_sha256,
                record.output_dir,
                i64::try_from(exported).map_err(|_| AppError::Validation("too many members".into()))?,
                i64::try_from(record.members.len() - exported).map_err(|_| AppError::Validation("too many members".into()))?,
                record.total_duration_ms,
                policy_sha,
                policy_json,
                crate::GIT_SHA,
                record.created_at_ms,
            ],
        )?;
        for member in &record.members {
            // The pool's immutable audio identity is the binding; a clip outside the pool cannot be a batch member.
            let hash: String = tx
                .query_row(
                    "SELECT audio_content_hash FROM review_pool_members WHERE pool_id=?1 AND segment_id=?2",
                    params![pool.pool_id, member.segment_id],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| AppError::Validation(format!("{}: not a member of the active pool", member.segment_id)))?;
            tx.execute(
                "INSERT INTO review_pool_export_batch_members(batch_id,segment_id,audio_content_hash,resolution_evidence_sha256,transcript_sha256,disposition)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    id,
                    member.segment_id,
                    hash,
                    member.resolution_evidence_sha256,
                    member.transcript_sha256,
                    member.disposition.as_str()
                ],
            )?;
        }
        tx.commit()?;
        Ok(true)
    })
}

/// `pool_admin export-batches`: every batch, plus the clips delivered earlier whose authority no
/// longer stands (unresolved, rejected, held, or re-decided), so a trainer knows what to drop or
/// wait for. Replaces the hand-written 2026-09-11 reconciliation note.
pub fn describe(db: &Database) -> AppResult<serde_json::Value> {
    if !supported(db)? {
        return Ok(serde_json::json!({ "schemaSupported": false, "batches": [] }));
    }
    let mut statement = db.connection().prepare(
        "SELECT batch_id,voice_name,kind,exported_segments,skipped_segments,total_duration_ms,app_git_sha,created_at_ms,output_dir,trust_policy_json
           FROM review_pool_export_batches ORDER BY created_at_ms, batch_id",
    )?;
    let batches = statement
        .query_map([], |r| {
            let policy: String = r.get(9)?;
            Ok(serde_json::json!({
                "batchId": r.get::<_, String>(0)?, "voiceName": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?,
                "exportedSegments": r.get::<_, i64>(3)?, "skippedSegments": r.get::<_, i64>(4)?,
                "totalDurationMs": r.get::<_, i64>(5)?, "appGitSha": r.get::<_, String>(6)?,
                "createdAtMs": r.get::<_, i64>(7)?, "outputDir": r.get::<_, String>(8)?,
                "trustPolicy": serde_json::from_str::<serde_json::Value>(&policy).unwrap_or(serde_json::Value::Null),
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let Some(pool) = crate::review_pool::load(db).map_err(AppError::Validation)? else {
        return Ok(serde_json::json!({ "schemaSupported": true, "poolActive": false, "batches": batches }));
    };
    let history = delivered(db, &pool.pool_id)?;
    let blocked = crate::training_quarantine::TrainingBoundary::capture(db)?.blocked;
    let resolutions: HashMap<String, crate::review_pool::SegmentResolution> =
        crate::review_pool::segment_resolutions(db, None)
            .map_err(AppError::Validation)?
            .into_iter()
            .map(|r| (r.segment_id.clone(), r))
            .collect();
    let mut withdrawn = Vec::new();
    for (id, deliveries) in &history {
        let Some(latest) = deliveries.last() else { continue };
        let reason = match resolutions.get(id) {
            None => Some("no longer in the active pool"),
            Some(r) if !matches!(r.status.as_str(), "resolved" | "ownerResolved") => Some("no longer resolved"),
            Some(r) if r.final_action.as_deref() != Some("retain") => Some("now rejected"),
            Some(_) if blocked.contains(id) => Some("now training-quarantined"),
            Some(r) if latest.resolution_evidence_sha256.as_deref().is_some_and(|e| e != r.evidence_sha256) => {
                Some("re-decided since delivery")
            }
            Some(_) => None,
        };
        if let Some(reason) = reason {
            withdrawn.push(serde_json::json!({ "id": id, "lastBatchId": latest.batch_id, "reason": reason }));
        }
    }
    withdrawn.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(serde_json::json!({
        "schemaSupported": true,
        "poolActive": true,
        "deliveredSegments": history.len(),
        "deliveredButWithdrawn": withdrawn,
        "batches": batches,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_delivered_matches_evidence_or_legacy_text_only() {
        let by_evidence = Delivered {
            batch_id: "b".into(),
            resolution_evidence_sha256: Some("e".repeat(64)),
            transcript_sha256: None,
        };
        let by_text = Delivered {
            batch_id: "b".into(),
            resolution_evidence_sha256: None,
            transcript_sha256: Some("t".repeat(64)),
        };
        let unknown = Delivered { batch_id: "b".into(), resolution_evidence_sha256: None, transcript_sha256: None };
        assert!(already_delivered(std::slice::from_ref(&by_evidence), &"e".repeat(64), &"x".repeat(64)));
        assert!(!already_delivered(&[by_evidence], &"f".repeat(64), &"t".repeat(64)), "evidence changed: re-deliver");
        assert!(already_delivered(std::slice::from_ref(&by_text), &"f".repeat(64), &"t".repeat(64)));
        assert!(!already_delivered(&[by_text], &"f".repeat(64), &"u".repeat(64)), "legacy text differs: re-deliver");
        assert!(!already_delivered(&[unknown], &"e".repeat(64), &"t".repeat(64)));
    }

    #[test]
    fn below_schema_73_the_batch_registry_fails_closed() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        assert!(supported(&db).unwrap());
        crate::migrations::rollback(&db, 1).unwrap();
        assert!(!supported(&db).unwrap());
        assert!(delivered(&db, "pool").unwrap_err().to_string().contains("schema 73"));
        assert_eq!(describe(&db).unwrap()["schemaSupported"], false);
    }

    #[test]
    fn a_record_needs_an_active_pool_a_well_formed_id_and_bound_members() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let record = |id: &str, legacy: bool| ExportBatchRecord {
            batch_id: id.into(),
            voice_name: "Lamo".into(),
            legacy,
            export_manifest_sha256: "a".repeat(64),
            certificate_sha256: None,
            output_dir: "C:/exports/x".into(),
            total_duration_ms: 1000,
            created_at_ms: 1,
            trust_policy: serde_json::json!({ "owner": "Hawzhin", "trusted": [] }),
            members: vec![ExportBatchMember {
                segment_id: "clip".into(),
                resolution_evidence_sha256: None,
                transcript_sha256: Some("b".repeat(64)),
                disposition: Disposition::Exported,
            }],
        };
        assert!(super::record(&db, &record("short", true)).unwrap_err().to_string().contains("8-120"));
        let err = super::record(&db, &record("approved-x", false)).unwrap_err().to_string();
        assert!(err.contains("binds evidence"), "{err}");
        assert!(super::record(&db, &record("legacy-batch-1", true)).unwrap_err().to_string().contains("not active"));
    }
}
