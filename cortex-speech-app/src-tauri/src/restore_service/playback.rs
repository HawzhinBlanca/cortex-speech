//! Playback-receipt and aggregate restore-target validation.

use super::compensation::validate_review_compensation_semantics;
use super::effects::validate_review_effect_semantics;

/// Recompute every restored listening receipt from its integer media counters. A staged file can
/// carry rows that predate the current triggers/writer; trusting their stored REAL would let
/// `played_ms = 0, coverage_ratio = 1` become durable no-listen authority after a restore.
pub(crate) fn validate_playback_receipt_semantics(db: &crate::db::Database) -> Result<(), String> {
    use rusqlite::OptionalExtension;

    // Policy 4 is a multi-table authority, not just a shaped receipt row. Reuse the database's
    // canonical startup/staged-restore proof so this layer cannot drift from the writer contract.
    db.validate_policy4_restore_authority()
        .map_err(|error| format!("database restore refused: policy-4 playback authority is invalid: {error}"))?;

    let mut statement = db
        .connection()
        .prepare(
            "SELECT id, segment_id, segment_revision, audio_fingerprint, played_ms,
                    clip_duration_ms, coverage_ratio, policy_version, started_at_ms,
                    source_start_ms, source_end_ms
               FROM playback_receipts ORDER BY id",
        )
        .map_err(|error| format!("restore target playback receipts are unreadable: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, f64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<i64>>(10)?,
            ))
        })
        .map_err(|error| format!("restore target playback receipts are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target playback receipts are unreadable: {error}"))?;
    drop(statement);

    for (
        id,
        segment_id,
        segment_revision,
        stored_audio_identity,
        played_ms,
        clip_duration_ms,
        coverage,
        policy_version,
        started_at_ms,
        source_start_ms,
        source_end_ms,
    ) in rows
    {
        let span_bound_policy = policy_version == crate::db::PLAYBACK_POLICY_VERSION
            || policy_version == crate::db::DESKTOP_PLAYBACK_POLICY_VERSION;
        let expected_coverage = if clip_duration_ms > 0 && played_ms >= 0 {
            (played_ms as f64 / clip_duration_ms as f64).min(1.0)
        } else {
            f64::NAN
        };
        let tolerance = 1e-12_f64.max(expected_coverage.abs() * f64::EPSILON * 8.0);
        if id <= 0
            || segment_id.trim().is_empty()
            || segment_revision < 0
            || stored_audio_identity.trim().is_empty()
            || played_ms < 0
            || started_at_ms < 0
            || clip_duration_ms <= 0
            || !coverage.is_finite()
            || !expected_coverage.is_finite()
            || (coverage - expected_coverage).abs() > tolerance
            || !(matches!(policy_version, 1 | 2) || span_bound_policy)
        {
            return Err(format!(
                "database restore refused: playback receipt {id} violates the canonical writer invariants"
            ));
        }

        let current: Option<(i64, Option<String>, i64, Option<String>)> = db
            .connection()
            .query_row(
                "SELECT COALESCE(review_revision, 0),
                        NULLIF(TRIM(COALESCE(audio_content_hash, '')), ''),
                        COALESCE(duration_ms, 0), alignment_json
                   FROM speech_segments WHERE id = ?1",
                [&segment_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|error| format!("restore target playback segment identity is unreadable: {error}"))?;
        let Some((current_revision, current_content_hash, current_duration, current_alignment_json)) = current else {
            return Err(format!("database restore refused: playback receipt {id} points to a missing segment"));
        };
        // Production minting reads the current revision atomically; a future-revision receipt is
        // impossible and would become a pre-minted authorization after the next segment UPDATE.
        if segment_revision > current_revision {
            return Err(format!("database restore refused: playback receipt {id} is from a future segment revision"));
        }
        // Policy 1 stored the v50 64-bit spectral candidate in the legacy `audio_fingerprint`
        // receipt column. Preserve it as historical audit evidence; policy 2 stored decoded-PCM
        // BLAKE3 but predates source-span binding. Neither can authorize policy-3 decisions.
        if policy_version == 1 {
            if source_start_ms.is_some() || source_end_ms.is_some() {
                return Err(format!(
                    "database restore refused: legacy policy-1 playback receipt {id} claims a policy-3 source span"
                ));
            }
            continue;
        }
        if !crate::db::is_canonical_audio_content_hash(&stored_audio_identity) {
            return Err(format!(
                "database restore refused: content-hash playback receipt {id} lacks a canonical decoded-PCM BLAKE3 hash"
            ));
        }
        let receipt_source_span = match (source_start_ms, source_end_ms) {
            (Some(start), Some(end)) if start >= 0 && end > start => Some((start, end)),
            (None, None) if policy_version == 2 => None,
            _ => {
                return Err(format!(
                    "database restore refused: policy-{policy_version} playback receipt {id} has an invalid source span"
                ));
            }
        };
        if policy_version == 2 && receipt_source_span.is_some() {
            return Err(format!(
                "database restore refused: historical policy-2 playback receipt {id} claims a policy-3 source span"
            ));
        }
        if span_bound_policy
            && !receipt_source_span
                .is_some_and(|(start, end)| crate::db::source_span_matches_duration(start, end, clip_duration_ms))
        {
            return Err(format!(
                "database restore refused: policy-{policy_version} playback receipt {id} source span disagrees with decoded duration"
            ));
        }
        let Some(current_content_hash) =
            current_content_hash.filter(|value| crate::db::is_canonical_audio_content_hash(value))
        else {
            return Err(format!(
                "database restore refused: content-hash playback receipt {id} has no canonical server-derived segment BLAKE3 identity"
            ));
        };
        let current_source_span = crate::db::canonical_source_span(current_alignment_json.as_deref());
        if span_bound_policy
            && !current_source_span
                .is_some_and(|(start, end)| crate::db::source_span_matches_duration(start, end, current_duration))
        {
            return Err(format!(
                "database restore refused: policy-{policy_version} playback receipt {id} segment source span disagrees with decoded duration"
            ));
        }
        // Policies 3 and 4 freeze the segment's audio identity for the lifetime of the receipt. Unrelated
        // metadata writes legitimately advance `review_revision`, so an older receipt revision is
        // expected, but its decoded-PCM BLAKE3, duration, and exact source window must still equal
        // the retained server row.  Checking only when revisions happened to be equal let a staged
        // database bump an unrelated column and then substitute a different valid-looking hash.
        let identity_must_match = span_bound_policy || segment_revision == current_revision;
        if identity_must_match
            && (stored_audio_identity != current_content_hash
                || clip_duration_ms != current_duration
                || current_duration <= 0
                || (span_bound_policy && receipt_source_span != current_source_span))
        {
            return Err(format!(
                "database restore refused: content-hash playback receipt {id} disagrees with its retained segment identity"
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_restore_target_semantics(db: &crate::db::Database) -> Result<(), String> {
    validate_review_compensation_semantics(db)?;
    validate_review_effect_semantics(db)?;
    validate_playback_receipt_semantics(db)?;
    crate::review_campaign::load(db)
        .map_err(|error| format!("database restore refused: sequential campaign authority is invalid: {error}"))?;
    crate::review_pool::load(db)
        .map_err(|error| format!("database restore refused: flexible review-pool authority is invalid: {error}"))?;
    Ok(())
}
