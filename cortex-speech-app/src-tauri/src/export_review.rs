//! Export-only human consensus authority. Never stored as a first review or accepted from IPC.

use crate::db::{Database, SpeechSegment};
use crate::error::{AppError, AppResult};
use crate::review_pool::SegmentResolution;
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::collections::HashMap;

type PoolRegistryBoundary = (String, i64, String, String, String, Option<String>);

/// Capture absence as well as presence: an empty authority list is not proof that a legacy
/// export may still publish after pool activation. Includes schema/frozen identity/dedup binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportReviewBoundary {
    schema_version: i64,
    registry: Option<PoolRegistryBoundary>,
}

impl ExportReviewBoundary {
    pub(crate) fn capture(db: &Database) -> AppResult<Self> {
        let schema_version =
            crate::migrations::get_current_version(db).map_err(|error| AppError::Validation(error.to_string()))?;
        let registry = if schema_version < crate::review_pool::REVIEW_POOL_BASE_SCHEMA_VERSION {
            None
        } else {
            let dedup = if schema_version >= crate::review_pool::REVIEW_POOL_DEDUP_SCHEMA_VERSION {
                "(SELECT manifest_sha256 FROM review_pool_dedup_manifests WHERE pool_id=registry.pool_id)"
            } else {
                "NULL"
            };
            db.connection()
                .query_row(
                    &format!(
                        "SELECT pool_id, focus_segment_count, focus_sha256, champion_model_version_id,
                        champion_deployment_sha256, {dedup}
                   FROM review_pool_registry registry WHERE singleton_key=1"
                    ),
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
                )
                .optional()?
        };
        Ok(Self { schema_version, registry })
    }

    pub(crate) fn verify(&self, db: &Database) -> AppResult<()> {
        if Self::capture(db)? != *self {
            return Err(AppError::Validation("export review scope changed before publication".into()));
        }
        Ok(())
    }

    pub(crate) fn verify_authorities<'a>(
        &self,
        db: &Database,
        authorities: impl IntoIterator<Item = &'a ExportReviewAuthority>,
    ) -> AppResult<()> {
        self.verify(db)?;
        verify_authorities(db, authorities)
    }
}

/// A retained final outcome, distinct from the canonical row's original paid opinion.
/// Fields are private and deserialization is deliberately unavailable: only proven database
/// resolutions may construct this authority. Public exports contain no reviewer names.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExportReviewAuthority {
    schema_version: u32,
    segment_id: String,
    resolution_status: String,
    final_action: String,
    final_transcript: String,
    evidence_sha256: String,
    reviewer_count: usize,
}

impl ExportReviewAuthority {
    pub(crate) fn retained(resolution: &SegmentResolution) -> AppResult<Self> {
        let text = resolution.final_transcript.as_deref().unwrap_or_default();
        if !matches!(resolution.status.as_str(), "resolved" | "ownerResolved")
            || resolution.final_action.as_deref() != Some("retain")
            || text.trim().is_empty()
            || crate::quality::is_placeholder_transcript(text)
            || resolution.evidence_sha256.len() != 64
            || resolution.reviewer_count < 2
            || !resolution.evidence_sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(AppError::Validation(format!(
                "{}: export has no valid retained consensus authority",
                resolution.segment_id
            )));
        }
        Ok(Self {
            schema_version: 1,
            segment_id: resolution.segment_id.clone(),
            resolution_status: resolution.status.clone(),
            final_action: "retain".into(),
            final_transcript: text.to_string(),
            evidence_sha256: resolution.evidence_sha256.clone(),
            reviewer_count: resolution.reviewer_count,
        })
    }

    pub fn transcript(&self) -> &str {
        &self.final_transcript
    }

    /// A final retain is not a fabricated first-review edit/accept event.
    pub fn decision(&self) -> &'static str {
        "retain"
    }

    pub fn evidence_sha256(&self) -> &str {
        &self.evidence_sha256
    }
}

/// One validated consensus snapshot for a batch learning consumer. `None` preserves legacy
/// no-pool behavior; an active pool (even with zero retained outcomes) never widens to the library.
/// Construct once per batch, not once per example. The original learning artifacts remain intact.
pub(crate) struct LearningReviewScope {
    retained: Option<HashMap<String, ExportReviewAuthority>>,
    boundary: ExportReviewBoundary,
}

impl LearningReviewScope {
    pub(crate) fn capture(db: &Database) -> AppResult<Self> {
        let boundary = ExportReviewBoundary::capture(db)?;
        let Some(scope) = crate::review_pool::exportable_segment_ids(db).map_err(AppError::Validation)? else {
            boundary.verify(db)?;
            return Ok(Self { retained: None, boundary });
        };
        let resolutions = crate::review_pool::segment_resolutions(db, None).map_err(AppError::Validation)?;
        let mut retained = HashMap::new();
        for resolution in resolutions {
            if scope.contains(&resolution.segment_id)
                && matches!(resolution.status.as_str(), "resolved" | "ownerResolved")
                && resolution.final_action.as_deref() == Some("retain")
            {
                retained.insert(resolution.segment_id.clone(), ExportReviewAuthority::retained(&resolution)?);
            }
        }
        boundary.verify(db)?;
        Ok(Self { retained: Some(retained), boundary })
    }

    pub(crate) fn is_active(&self) -> bool {
        self.retained.is_some()
    }

    pub(crate) fn includes(&self, segment_id: &str) -> bool {
        self.retained.as_ref().map_or(true, |rows| rows.contains_key(segment_id))
    }

    pub(crate) fn authority(&self, segment_id: &str) -> Option<&ExportReviewAuthority> {
        self.retained.as_ref().and_then(|rows| rows.get(segment_id))
    }

    pub(crate) fn verify_authorities<'a>(
        &self,
        db: &Database,
        authorities: impl IntoIterator<Item = &'a ExportReviewAuthority>,
    ) -> AppResult<()> {
        self.boundary.verify_authorities(db, authorities)
    }
}

/// Re-prove the captured outcome immediately before publication. Membership alone misses a
/// changed matching pair, owner ruling, or reversal that keeps the same segment IDs in scope.
pub(crate) fn verify_current<'a>(
    db: &Database,
    segments: impl IntoIterator<Item = &'a SpeechSegment>,
) -> AppResult<()> {
    let captured: Vec<_> = segments.into_iter().collect();
    if captured.is_empty() {
        return Ok(());
    }
    let current = LearningReviewScope::capture(db)?;
    for segment in captured {
        if !current.includes(&segment.id) || current.authority(&segment.id) != segment.export_review.as_ref() {
            return Err(AppError::Validation(format!(
                "{}: export consensus authority changed before publication",
                segment.id
            )));
        }
    }
    current.boundary.verify(db)
}

// Private: callers must pair authority verification with a captured publication boundary.
fn verify_authorities<'a>(
    db: &Database,
    authorities: impl IntoIterator<Item = &'a ExportReviewAuthority>,
) -> AppResult<()> {
    let captured: Vec<_> = authorities.into_iter().collect();
    if captured.is_empty() {
        return Ok(());
    }
    let current: HashMap<_, _> = crate::review_pool::segment_resolutions(db, None)
        .map_err(AppError::Validation)?
        .into_iter()
        .map(|resolution| (resolution.segment_id.clone(), resolution))
        .collect();
    for expected in captured {
        let actual = current
            .get(&expected.segment_id)
            .ok_or_else(|| AppError::Validation("export consensus authority disappeared".into()))?;
        if ExportReviewAuthority::retained(actual)? != *expected {
            return Err(AppError::Validation(format!(
                "{}: export consensus authority changed before publication",
                expected.segment_id
            )));
        }
    }
    Ok(())
}
