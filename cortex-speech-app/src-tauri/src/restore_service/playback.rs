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
    db.validate_desktop_review_action_journal()
        .map_err(|error| format!("database restore refused: desktop review action journal is invalid: {error}"))?;
    validate_review_compensation_semantics(db)?;
    validate_review_effect_semantics(db)?;
    validate_playback_receipt_semantics(db)?;
    crate::review_campaign::load(db)
        .map_err(|error| format!("database restore refused: sequential campaign authority is invalid: {error}"))?;
    crate::review_pool::load(db)
        .map_err(|error| format!("database restore refused: flexible review-pool authority is invalid: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_playback_receipt_semantics, validate_restore_target_semantics};
    use crate::db::Database;

    /// A segment carrying the canonical listening identity: content hash, source span, duration.
    fn seeded_db(id: &str) -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&crate::db::SpeechSegment {
            id: id.to_string(),
            audio_path: format!("{id}.wav"),
            raw_transcript: "machine draft".to_string(),
            duration_ms: 1_000,
            ..crate::db::SpeechSegment::default()
        })
        .unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash = ?2,
                        alignment_json = '{\"source_start_ms\":0,\"source_end_ms\":1000}',
                        duration_ms = 1000
                  WHERE id = ?1",
                rusqlite::params![id, "a".repeat(64)],
            )
            .unwrap();
        db
    }

    /// A genuine policy-3 receipt minted through the production front door.
    fn listened(db: &Database, id: &str) {
        db.record_playback_receipt(&crate::db::PlaybackReceipt {
            segment_id: id.to_string(),
            segment_revision: 0,
            audio_content_hash: "a".repeat(64),
            reviewer: Some("Reviewer".to_string()),
            session_id: None,
            started_at_ms: 1,
            played_ms: 1_000,
            clip_duration_ms: 1_000,
            source_start_ms: None,
            source_end_ms: None,
        })
        .unwrap();
    }

    /// Restored files can carry receipt rows written with the guards disabled.
    fn unlock(db: &Database, table: &str) {
        let names = db
            .connection()
            .prepare("SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name=?1")
            .unwrap()
            .query_map([table], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for name in names {
            db.connection().execute(&format!("DROP TRIGGER \"{name}\""), []).unwrap();
        }
        db.connection().execute_batch("PRAGMA ignore_check_constraints = ON; PRAGMA foreign_keys = OFF;").unwrap();
    }

    fn listened_fixture(id: &str) -> Database {
        let db = seeded_db(id);
        listened(&db, id);
        validate_playback_receipt_semantics(&db).expect("a genuine policy-3 receipt must validate first");
        db
    }

    #[test]
    fn a_fresh_database_and_a_genuine_receipt_both_validate_and_the_aggregate_pass_agrees() {
        let db = listened_fixture("clean-clip");
        // The aggregate restore-target pass shares this verdict end to end.
        validate_restore_target_semantics(&db)
            .expect("a genuine listened clip must be a valid aggregate restore target");
    }

    #[test]
    fn receipts_violating_the_canonical_writer_invariants_are_refused() {
        let cases: [(&str, &str); 5] = [
            ("negative played time", "UPDATE playback_receipts SET played_ms=-1"),
            ("coverage disagrees with the integer counters", "UPDATE playback_receipts SET played_ms=1"),
            ("non-positive clip duration", "UPDATE playback_receipts SET clip_duration_ms=0"),
            ("unknown policy version", "UPDATE playback_receipts SET policy_version=9"),
            ("blank segment identity", "UPDATE playback_receipts SET segment_id='   '"),
        ];
        for (label, sabotage) in cases {
            let db = listened_fixture("writer-clip");
            unlock(&db, "playback_receipts");
            assert_eq!(db.connection().execute(sabotage, []).unwrap(), 1, "{label}");
            let error = validate_playback_receipt_semantics(&db).unwrap_err();
            assert!(error.contains("violates the canonical writer invariants"), "{label}: {error}");
        }
    }

    #[test]
    fn receipt_identity_must_match_its_retained_segment() {
        let swapped_hash = format!("UPDATE playback_receipts SET audio_fingerprint='{}'", "b".repeat(64));
        let cases: [(&str, &str, &str); 6] = [
            (
                "receipt points at a missing segment",
                "UPDATE playback_receipts SET segment_id='ghost'",
                "points to a missing segment",
            ),
            (
                "receipt claims a future segment revision",
                "UPDATE playback_receipts SET segment_revision=segment_revision+5",
                "from a future segment revision",
            ),
            (
                "receipt carries a non-canonical audio identity",
                "UPDATE playback_receipts SET audio_fingerprint='not-a-hash'",
                "lacks a canonical decoded-PCM BLAKE3 hash",
            ),
            (
                "receipt loses half its source span",
                "UPDATE playback_receipts SET source_start_ms=NULL",
                "invalid source span",
            ),
            (
                "receipt span disagrees with its own decoded duration",
                "UPDATE playback_receipts SET source_end_ms=source_end_ms+500",
                "source span disagrees with decoded duration",
            ),
            (
                "receipt hash was swapped for a different valid-looking clip",
                swapped_hash.as_str(),
                "disagrees with its retained segment identity",
            ),
        ];
        for (label, sabotage, expected) in cases {
            let db = listened_fixture("identity-clip");
            unlock(&db, "playback_receipts");
            assert_eq!(db.connection().execute(sabotage, []).unwrap(), 1, "{label}");
            let error = validate_playback_receipt_semantics(&db).unwrap_err();
            assert!(error.contains(expected), "{label}: expected '{expected}', got: {error}");
        }

        // The RETAINED segment's own span must also still answer for its decoded duration.
        let db = listened_fixture("segment-span-clip");
        unlock(&db, "speech_segments");
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET alignment_json='{\"source_start_ms\":0,\"source_end_ms\":2000}'
                  WHERE id='segment-span-clip'",
                [],
            )
            .unwrap();
        let error = validate_playback_receipt_semantics(&db).unwrap_err();
        assert!(error.contains("segment source span disagrees with decoded duration"), "{error}");
    }

    #[test]
    fn historical_policy_receipts_cannot_claim_policy3_source_spans() {
        // Policy 1 stored the legacy spectral candidate and predates source spans entirely.
        let policy1 = listened_fixture("policy1-clip");
        unlock(&policy1, "playback_receipts");
        policy1.connection().execute("UPDATE playback_receipts SET policy_version=1", []).unwrap();
        let error = validate_playback_receipt_semantics(&policy1).unwrap_err();
        assert!(error.contains("legacy policy-1") && error.contains("claims a policy-3 source span"), "{error}");
        // Without the span claim, the legacy receipt is preserved as audit evidence.
        policy1
            .connection()
            .execute("UPDATE playback_receipts SET source_start_ms=NULL, source_end_ms=NULL", [])
            .unwrap();
        validate_playback_receipt_semantics(&policy1)
            .expect("a span-free legacy policy-1 receipt is historical evidence, not corruption");

        // Policy 2 stored decoded-PCM BLAKE3 but also predates source-span binding.
        let policy2 = listened_fixture("policy2-clip");
        unlock(&policy2, "playback_receipts");
        policy2.connection().execute("UPDATE playback_receipts SET policy_version=2", []).unwrap();
        let error = validate_playback_receipt_semantics(&policy2).unwrap_err();
        assert!(error.contains("historical policy-2") && error.contains("claims a policy-3 source span"), "{error}");
    }
}
