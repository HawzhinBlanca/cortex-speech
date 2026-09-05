//! Monotonic review, payment, and consent authority for restore admission.

const DURABLE_REVIEW_RESTORE_TABLES: [&str; 35] = [
    "review_pilot_hidden_keys",
    "review_events",
    "spot_checks",
    "review_compensation_ledger",
    "review_compensation_settlements",
    "review_compensation_policies",
    "review_effect_state",
    "human_decision_effect_events",
    "human_decision_effect_reversals",
    "review_flag_effect_events",
    "review_flag_effect_reversals",
    "correction_memory",
    "correction_memory_contributions",
    "corrections",
    "playback_receipts",
    "legacy_agent_examples_v60",
    "legacy_corrections_v60",
    "legacy_reviewed_segments_v60",
    "legacy_machine_verdict_segments_v60",
    "review_campaign_registry",
    "review_campaign_focus",
    "review_campaign_transitions",
    "independent_review_decisions",
    "independent_review_reversals",
    "review_campaign_adjudications",
    "review_pool_registry",
    "review_pool_members",
    "review_pool_decisions",
    "review_pool_reversals",
    "review_pool_owner_adjudications",
    "review_pool_voice_certificates",
    "review_pool_dedup_manifests",
    "review_pool_duplicate_exclusions",
    "review_pool_dedup_supersessions",
    "desktop_review_action_events_v1",
];

const EFFECT_BOUND_AGENT_EXAMPLES_RESTORE_PROJECTION: &str =
    "SELECT * FROM agent_examples WHERE effect_event_id IS NOT NULL";
const LEGACY_CORRECTION_MEMORY_RESTORE_PROJECTION: &str = "SELECT * FROM correction_memory WHERE legacy_seed = 1";
const LEGACY_AGENT_EXAMPLES_RESTORE_PROJECTION: &str = "SELECT * FROM legacy_agent_examples_v60";
const LEGACY_CORRECTIONS_RESTORE_PROJECTION: &str = "SELECT * FROM legacy_corrections_v60";
const LEGACY_REVIEWED_SEGMENTS_RESTORE_PROJECTION: &str = "SELECT * FROM legacy_reviewed_segments_v60";
const LEGACY_MACHINE_VERDICTS_RESTORE_PROJECTION: &str = "SELECT * FROM legacy_machine_verdict_segments_v60";
const LEGACY_DESKTOP_ACTIONS_RESTORE_PROJECTION: &str = "SELECT * FROM desktop_review_legacy_actions_v1";
const DESKTOP_ACTION_JOURNAL_ORDERED_RESTORE_PROJECTION: &str =
    "SELECT * FROM desktop_review_action_events_v1 ORDER BY id";

// A forward Undo/redecision legitimately changes the mutable human projection, so it cannot be an
// exact-row restore floor. The reviewed clip and every source/export identity already present at the
// floor are nevertheless monotonic authority: a newer target may fill a previously-null identity,
// but it may not delete the clip or change an established hash/span/duration. Encoding one row per
// present attribute gives `require_encoded_row_superset` exactly those semantics.
const REVIEWED_SEGMENT_EXPORT_IDENTITY_RESTORE_PROJECTION: &str = "WITH reviewed AS (
        SELECT segment.id, segment.audio_content_hash, segment.audio_fingerprint,
               segment.alignment_json, segment.duration_ms
          FROM speech_segments segment
         WHERE segment.human_decision IS NOT NULL
            OR segment.reviewed_by IS NOT NULL
            OR (
               segment.verified = 1
               AND (segment.annotated_transcript IS NOT NULL OR segment.verdict_transcript IS NOT NULL)
            )
            OR EXISTS (
               SELECT 1 FROM review_events event
                WHERE event.segment_id = segment.id
                  AND event.source <> 'couch_spot_check'
                  AND event.action IN ('accept', 'edit', 'reject')
            )
            OR EXISTS (
               SELECT 1 FROM review_compensation_ledger ledger
                WHERE ledger.segment_id = segment.id
                  AND ledger.compensation_action = 'undo'
            )
    )
    SELECT id, 'segment' AS identity_kind, 'present' AS identity_value FROM reviewed
    UNION ALL
    SELECT id, 'audio_content_hash', audio_content_hash FROM reviewed
     WHERE audio_content_hash IS NOT NULL
    UNION ALL
    SELECT id, 'audio_fingerprint', printf('%lld',audio_fingerprint) FROM reviewed
     WHERE audio_fingerprint IS NOT NULL
    UNION ALL
    SELECT id, 'alignment_json', alignment_json FROM reviewed
     WHERE alignment_json IS NOT NULL
    UNION ALL
    SELECT id, 'duration_ms', printf('%lld',duration_ms) FROM reviewed
    ORDER BY id, identity_kind";

const REVIEWED_SEGMENT_ACTIVITY_PROJECTION: &str = "SELECT segment.id,
            segment.audio_content_hash,
            segment.audio_fingerprint,
            segment.alignment_json,
            segment.duration_ms,
            segment.human_decision,
            segment.verdict,
            segment.verdict_transcript,
            segment.annotated_transcript,
            segment.verified,
            segment.reviewed_by,
            segment.corrected_at,
            segment.review_revision,
            segment.escalated,
            segment.is_gold
       FROM speech_segments segment
      WHERE segment.human_decision IS NOT NULL
         OR segment.reviewed_by IS NOT NULL
         OR (
            segment.verified = 1
            AND (segment.annotated_transcript IS NOT NULL OR segment.verdict_transcript IS NOT NULL)
         )
         OR EXISTS (
            SELECT 1 FROM review_events event
             WHERE event.segment_id = segment.id
               AND event.source <> 'couch_spot_check'
               AND event.action IN ('accept', 'edit', 'reject')
      )
         OR EXISTS (
            SELECT 1 FROM review_compensation_ledger ledger
             WHERE ledger.segment_id = segment.id
               AND ledger.compensation_action = 'undo'
      )";

fn encode_durable_sqlite_value(value: rusqlite::types::ValueRef<'_>, encoded: &mut Vec<u8>) {
    use rusqlite::types::ValueRef;

    match value {
        ValueRef::Null => encoded.push(0),
        ValueRef::Integer(value) => {
            encoded.push(1);
            encoded.extend_from_slice(&value.to_be_bytes());
        }
        ValueRef::Real(value) => {
            encoded.push(2);
            encoded.extend_from_slice(&value.to_bits().to_be_bytes());
        }
        ValueRef::Text(value) => {
            encoded.push(3);
            encoded.extend_from_slice(&(value.len() as u64).to_be_bytes());
            encoded.extend_from_slice(value);
        }
        ValueRef::Blob(value) => {
            encoded.push(4);
            encoded.extend_from_slice(&(value.len() as u64).to_be_bytes());
            encoded.extend_from_slice(value);
        }
    }
}

pub(super) fn exact_query_rows(
    db: &crate::db::Database,
    label: &str,
    sql: &str,
) -> Result<(Vec<String>, Vec<Vec<u8>>), String> {
    let mut statement = db
        .connection()
        .prepare(sql)
        .map_err(|error| format!("durable restore floor {label} is unreadable: {error}"))?;
    let columns = statement.column_names().iter().map(|name| (*name).to_string()).collect::<Vec<_>>();
    let column_count = statement.column_count();
    let mut query =
        statement.query([]).map_err(|error| format!("durable restore floor {label} cannot be scanned: {error}"))?;
    let mut rows = Vec::new();
    while let Some(row) =
        query.next().map_err(|error| format!("durable restore floor {label} cannot be scanned: {error}"))?
    {
        let mut encoded = Vec::new();
        for column in 0..column_count {
            let value = row
                .get_ref(column)
                .map_err(|error| format!("durable restore floor {label} has an unreadable value: {error}"))?;
            encode_durable_sqlite_value(value, &mut encoded);
        }
        rows.push(encoded);
    }
    Ok((columns, rows))
}

fn exact_table_rows(db: &crate::db::Database, table: &str) -> Result<(Vec<String>, Vec<Vec<u8>>), String> {
    // `table` is selected only from DURABLE_REVIEW_RESTORE_TABLES, never caller input.
    exact_query_rows(db, &format!("table {table}"), &format!("SELECT * FROM \"{table}\""))
}

fn require_encoded_row_superset(
    label: &str,
    floor_columns: Vec<String>,
    floor_rows: Vec<Vec<u8>>,
    target_columns: Vec<String>,
    target_rows: Vec<Vec<u8>>,
) -> Result<(), String> {
    if target_columns != floor_columns {
        return Err(format!(
            "database restore refused: target {label} columns do not match the authoritative review-history floor"
        ));
    }
    let mut target_counts = std::collections::BTreeMap::<Vec<u8>, usize>::new();
    for row in target_rows {
        *target_counts.entry(row).or_default() += 1;
    }
    let mut missing = 0usize;
    for row in floor_rows {
        match target_counts.get_mut(&row) {
            Some(count) if *count > 0 => *count -= 1,
            _ => missing += 1,
        }
    }
    if missing != 0 {
        return Err(format!(
            "database restore refused: target would drop or modify {missing} durable row(s) from {label}"
        ));
    }
    Ok(())
}

pub(super) fn require_encoded_row_equality(
    label: &str,
    floor_columns: Vec<String>,
    floor_rows: Vec<Vec<u8>>,
    target_columns: Vec<String>,
    target_rows: Vec<Vec<u8>>,
) -> Result<(), String> {
    if target_columns != floor_columns {
        return Err(format!(
            "database restore refused: target {label} columns do not match the authoritative review-history floor"
        ));
    }
    let row_counts = |rows: Vec<Vec<u8>>| {
        let mut counts = std::collections::BTreeMap::<Vec<u8>, usize>::new();
        for row in rows {
            *counts.entry(row).or_default() += 1;
        }
        counts
    };
    if row_counts(floor_rows) != row_counts(target_rows) {
        return Err(format!(
            "database restore refused: target must exactly preserve {label}; pseudo-legacy additions are forbidden"
        ));
    }
    Ok(())
}

fn require_desktop_action_journal_prefix(
    floor: &crate::db::Database,
    target: &crate::db::Database,
) -> Result<(), String> {
    let label = "desktop review action journal";
    let (floor_columns, floor_rows) =
        exact_query_rows(floor, label, DESKTOP_ACTION_JOURNAL_ORDERED_RESTORE_PROJECTION)?;
    let (target_columns, target_rows) =
        exact_query_rows(target, label, DESKTOP_ACTION_JOURNAL_ORDERED_RESTORE_PROJECTION)?;
    if target_columns != floor_columns {
        return Err(format!(
            "database restore refused: target {label} columns do not match the authoritative review-history floor"
        ));
    }
    if target_rows.len() < floor_rows.len() || target_rows[..floor_rows.len()] != floor_rows {
        return Err(
            "database restore refused: target desktop review action journal does not extend the exact authoritative prefix"
                .to_string(),
        );
    }
    Ok(())
}

/// Require `target` to contain every exact append-only durable row in `floor`. Values as well as
/// identities are compared with SQLite storage-class fidelity; a row with the same primary key but
/// changed text, amount, policy, timestamp, or REAL bits is therefore a regression, not a match.
///
/// The mutable human fields in `speech_segments` are deliberately not compared as an exact row: a
/// legitimate forward Undo/redecision replaces that projection while extending the immutable
/// effect/reversal/action journals. The clip's established export/pay identities remain monotonic
/// below, while every publication path separately calls `validate_restore_target_semantics` to
/// reconstruct and verify the terminal human projection from immutable authority.
pub(crate) fn require_durable_review_history_superset(
    floor: &crate::db::Database,
    target: &crate::db::Database,
) -> Result<(), String> {
    for table in DURABLE_REVIEW_RESTORE_TABLES {
        let (floor_columns, floor_rows) = exact_table_rows(floor, table)?;
        let (target_columns, target_rows) = exact_table_rows(target, table)?;
        require_encoded_row_superset(table, floor_columns, floor_rows, target_columns, target_rows)?;
    }
    require_desktop_action_journal_prefix(floor, target)?;
    let (floor_columns, floor_rows) =
        exact_query_rows(floor, "effect-bound agent examples", EFFECT_BOUND_AGENT_EXAMPLES_RESTORE_PROJECTION)?;
    let (target_columns, target_rows) =
        exact_query_rows(target, "effect-bound agent examples", EFFECT_BOUND_AGENT_EXAMPLES_RESTORE_PROJECTION)?;
    require_encoded_row_superset(
        "effect-bound agent examples",
        floor_columns,
        floor_rows,
        target_columns,
        target_rows,
    )?;
    let (floor_columns, floor_rows) =
        exact_query_rows(floor, "legacy correction memories", LEGACY_CORRECTION_MEMORY_RESTORE_PROJECTION)?;
    let (target_columns, target_rows) =
        exact_query_rows(target, "legacy correction memories", LEGACY_CORRECTION_MEMORY_RESTORE_PROJECTION)?;
    require_encoded_row_equality("legacy correction memories", floor_columns, floor_rows, target_columns, target_rows)?;
    for (label, projection) in [
        ("legacy agent-example snapshot", LEGACY_AGENT_EXAMPLES_RESTORE_PROJECTION),
        ("legacy correction snapshot", LEGACY_CORRECTIONS_RESTORE_PROJECTION),
        ("legacy reviewed-segment snapshot", LEGACY_REVIEWED_SEGMENTS_RESTORE_PROJECTION),
        ("legacy machine-verdict snapshot", LEGACY_MACHINE_VERDICTS_RESTORE_PROJECTION),
        ("legacy desktop-action baseline", LEGACY_DESKTOP_ACTIONS_RESTORE_PROJECTION),
    ] {
        let (floor_columns, floor_rows) = exact_query_rows(floor, label, projection)?;
        let (target_columns, target_rows) = exact_query_rows(target, label, projection)?;
        require_encoded_row_equality(label, floor_columns, floor_rows, target_columns, target_rows)?;
    }
    let label = "reviewed speech-segment export projection";
    let (floor_columns, floor_rows) =
        exact_query_rows(floor, label, REVIEWED_SEGMENT_EXPORT_IDENTITY_RESTORE_PROJECTION)?;
    let (target_columns, target_rows) =
        exact_query_rows(target, label, REVIEWED_SEGMENT_EXPORT_IDENTITY_RESTORE_PROJECTION)?;
    require_encoded_row_superset(label, floor_columns, floor_rows, target_columns, target_rows)?;
    Ok(())
}

/// Consent withdrawal is monotonic authority, not ordinary restorable dataset state.
///
/// `rights_revoked_at` is intentionally the one-way tombstone consulted by every export path.  A
/// snapshot taken before a withdrawal therefore cannot be allowed to replace a newer generation and
/// quietly make the recording exportable again.  Prefer the server-derived decoded-PCM hash so a
/// legitimate relink/rename cannot evade (or spuriously break) the floor.  Legacy rows without a
/// canonical hash fall back to their exact stored recording path; an ambiguous legacy identity fails
/// closed rather than pretending the withdrawal was preserved.
pub(crate) fn require_consent_revocation_superset(
    floor: &crate::db::Database,
    target: &crate::db::Database,
) -> Result<(), String> {
    #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
    enum RevokedRecordingIdentity {
        ContentHash(String),
        LegacyPath(String),
    }

    let mut statement = floor
        .connection()
        .prepare(
            "SELECT audio_path, NULLIF(TRIM(COALESCE(audio_content_hash, '')), '')
               FROM speech_segments
              WHERE NULLIF(TRIM(COALESCE(rights_revoked_at, '')), '') IS NOT NULL
              ORDER BY id",
        )
        .map_err(|error| format!("restore revocation floor is unreadable: {error}"))?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)))
        .map_err(|error| format!("restore revocation floor is unreadable: {error}"))?;
    let mut identities = std::collections::BTreeSet::new();
    for row in rows {
        let (audio_path, audio_content_hash) =
            row.map_err(|error| format!("restore revocation floor is unreadable: {error}"))?;
        let identity = match audio_content_hash {
            Some(hash) if crate::db::is_canonical_audio_content_hash(&hash) => {
                RevokedRecordingIdentity::ContentHash(hash)
            }
            _ => {
                if audio_path.trim().is_empty() || audio_path.contains('\0') {
                    return Err("database restore refused: a withdrawn legacy recording has no safe durable identity"
                        .to_string());
                }
                RevokedRecordingIdentity::LegacyPath(audio_path)
            }
        };
        identities.insert(identity);
    }
    drop(statement);

    let mut missing_identities = 0usize;
    let mut resurrected_identities = 0usize;
    for identity in identities {
        let (row_count, unrevoked_count): (i64, i64) = match identity {
            RevokedRecordingIdentity::ContentHash(hash) => target.connection().query_row(
                "SELECT COUNT(*),
                            COALESCE(SUM(CASE
                                WHEN NULLIF(TRIM(COALESCE(rights_revoked_at, '')), '') IS NULL
                                THEN 1 ELSE 0 END), 0)
                       FROM speech_segments
                      WHERE audio_content_hash = ?1",
                [hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ),
            RevokedRecordingIdentity::LegacyPath(path) => target.connection().query_row(
                "SELECT COUNT(*),
                            COALESCE(SUM(CASE
                                WHEN NULLIF(TRIM(COALESCE(rights_revoked_at, '')), '') IS NULL
                                THEN 1 ELSE 0 END), 0)
                       FROM speech_segments
                      WHERE audio_path = ?1",
                [path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ),
        }
        .map_err(|error| format!("restore target revocation authority is unreadable: {error}"))?;
        if row_count == 0 {
            missing_identities += 1;
        } else if unrevoked_count != 0 {
            resurrected_identities += 1;
        }
    }

    if missing_identities != 0 || resurrected_identities != 0 {
        return Err(format!(
            "database restore refused: target would forget {missing_identities} withdrawn recording identity/identities and resurrect {resurrected_identities} withdrawn recording identity/identities"
        ));
    }
    Ok(())
}

/// The complete monotonic restore floor.  Keep review/payment evidence and consent withdrawals next
/// to one another so every page-publication path has one admission call and cannot remember one while
/// accidentally omitting the other.
pub(super) fn require_restore_authority_superset(
    floor: &crate::db::Database,
    target: &crate::db::Database,
) -> Result<(), String> {
    require_durable_review_history_superset(floor, target)?;
    require_consent_revocation_superset(floor, target)
}

pub(crate) fn has_durable_review_activity(db: &crate::db::Database) -> Result<bool, String> {
    // The policy table is installed before the first paid action and is protected by exact-row
    // comparison below. Once any actual audit/payment/grant row exists, a bare DB-only swap is no
    // longer an adequate recovery protocol because it cannot bind the companion policy/config files.
    for table in [
        "review_pilot_hidden_keys",
        "review_events",
        "spot_checks",
        "review_compensation_ledger",
        "review_compensation_settlements",
        "human_decision_effect_events",
        "human_decision_effect_reversals",
        "review_flag_effect_events",
        "review_flag_effect_reversals",
        "correction_memory",
        "correction_memory_contributions",
        "corrections",
        "playback_receipts",
        "legacy_agent_examples_v60",
        "legacy_corrections_v60",
        "legacy_reviewed_segments_v60",
        "legacy_machine_verdict_segments_v60",
        "review_campaign_registry",
        "review_campaign_focus",
        "review_campaign_transitions",
        "independent_review_decisions",
        "independent_review_reversals",
        "review_campaign_adjudications",
        "review_pool_registry",
        "review_pool_members",
        "review_pool_decisions",
        "review_pool_reversals",
        "review_pool_owner_adjudications",
        "review_pool_voice_certificates",
        "review_pool_dedup_manifests",
        "review_pool_duplicate_exclusions",
        "review_pool_dedup_supersessions",
        "desktop_review_legacy_actions_v1",
        "desktop_review_action_events_v1",
    ] {
        let exists: bool = db
            .connection()
            .query_row(&format!("SELECT EXISTS(SELECT 1 FROM \"{table}\" LIMIT 1)"), [], |row| row.get(0))
            .map_err(|error| format!("bare restore could not verify durable review history in {table}: {error}"))?;
        if exists {
            return Ok(true);
        }
    }
    let effect_bound_example_exists: bool = db
        .connection()
        .query_row("SELECT EXISTS(SELECT 1 FROM agent_examples WHERE effect_event_id IS NOT NULL LIMIT 1)", [], |row| {
            row.get(0)
        })
        .map_err(|error| format!("bare restore could not verify effect-bound human examples: {error}"))?;
    if effect_bound_example_exists {
        return Ok(true);
    }
    // The singleton exists in every pristine schema-v60 database.  It becomes durable activity only
    // when it records a non-empty pre-v60 frontier; presence alone must not disable a first restore.
    let nonempty_effect_frontier: bool = db
        .connection()
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM review_effect_state
                  WHERE singleton_key = 1
                    AND (effective_after_review_event_id > 0 OR effective_after_ledger_id > 0)
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("bare restore could not verify the review-effect frontier: {error}"))?;
    if nonempty_effect_frontier {
        return Ok(true);
    }
    let reviewed_truth_exists: bool = db
        .connection()
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM ({REVIEWED_SEGMENT_ACTIVITY_PROJECTION}) LIMIT 1)"),
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("bare restore could not verify reviewed segment truth: {error}"))?;
    if reviewed_truth_exists {
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn fresh_db() -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db
    }

    fn segment(db: &Database, id: &str) {
        db.insert_segment(&crate::db::SpeechSegment {
            id: id.to_string(),
            audio_path: format!("{id}.wav"),
            raw_transcript: "machine draft".to_string(),
            duration_ms: 1_000,
            ..crate::db::SpeechSegment::default()
        })
        .unwrap();
    }

    /// The production review-event writer refuses segments without a canonical PCM hash and source
    /// span, so event-recording fixtures need the full paid identity.
    fn paid_segment(db: &Database, id: &str) {
        segment(db, id);
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
    }

    /// A restored file may carry rows written with the schema guards disabled — that is the exact
    /// state these validators exist for, so corruptions drop the guards first.
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

    fn couch_skip(db: &Database, id: &str, index: u64) {
        let operation = format!("00000000-0000-4000-8000-{index:012x}");
        db.record_review_event_with_operation(
            id,
            "Reviewer",
            "skip",
            "couch",
            i64::try_from(index).unwrap(),
            &operation,
            &crate::db::review_operation_payload_hash(id, "skip", "", "Reviewer"),
        )
        .unwrap();
    }

    #[test]
    fn encoded_row_superset_and_equality_enforce_exact_multiset_semantics() {
        let columns = || vec!["id".to_string(), "value".to_string()];
        let row = |bytes: &[u8]| bytes.to_vec();

        // Exact match and true superset both pass; the target may only ADD rows.
        require_encoded_row_superset("t", columns(), vec![row(b"a")], columns(), vec![row(b"a"), row(b"b")]).unwrap();
        // Multiset semantics: two identical durable rows need two surviving copies, not one.
        let error =
            require_encoded_row_superset("t", columns(), vec![row(b"a"), row(b"a")], columns(), vec![row(b"a")])
                .unwrap_err();
        assert!(error.contains("drop or modify 1 durable row"), "{error}");
        // A changed column set is refused before any row-level comparison can mislead.
        let error = require_encoded_row_superset("t", columns(), vec![], vec!["id".to_string()], vec![]).unwrap_err();
        assert!(error.contains("columns do not match"), "{error}");

        // Equality additionally forbids pseudo-legacy additions.
        require_encoded_row_equality("t", columns(), vec![row(b"a")], columns(), vec![row(b"a")]).unwrap();
        let error =
            require_encoded_row_equality("t", columns(), vec![row(b"a")], columns(), vec![row(b"a"), row(b"a")])
                .unwrap_err();
        assert!(error.contains("pseudo-legacy additions are forbidden"), "{error}");
        let error =
            require_encoded_row_equality("t", columns(), vec![], vec!["other".to_string(), "cols".to_string()], vec![])
                .unwrap_err();
        assert!(error.contains("columns do not match"), "{error}");
    }

    #[test]
    fn dropping_a_durable_review_row_from_the_target_is_refused() {
        // Two independently initialized databases are NOT one lineage (installation timestamps in
        // the policy tables differ), so the target must be an actual copy of the floor — exactly
        // the snapshot/restore relationship this floor exists for.
        let tmp = tempfile::TempDir::new().unwrap();
        let floor_path = tmp.path().join("floor.db");
        let target_path = tmp.path().join("target.db");
        let floor = Database::open(floor_path.to_string_lossy().as_ref()).unwrap();
        floor.initialize().unwrap();
        floor.backup(&target_path).unwrap();
        let target = Database::open(target_path.to_string_lossy().as_ref()).unwrap();
        require_durable_review_history_superset(&floor, &target)
            .expect("a byte-copied generation satisfies its own review-history floor");

        paid_segment(&floor, "clip");
        couch_skip(&floor, "clip", 1);
        let error = require_durable_review_history_superset(&floor, &target).unwrap_err();
        assert!(
            error.contains("review_events") && error.contains("drop or modify"),
            "a target missing a durable review event must be refused: {error}"
        );
    }

    #[test]
    fn consent_withdrawal_is_monotonic_across_restore_generations() {
        let floor = fresh_db();
        segment(&floor, "withdrawn");
        unlock(&floor, "speech_segments");
        floor
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash=?2, rights_revoked_at='2026-08-30T00:00:00Z'
                  WHERE id=?1",
                rusqlite::params!["withdrawn", "a".repeat(64)],
            )
            .unwrap();

        // A target that FORGETS the withdrawn recording would silently re-permit export elsewhere.
        let empty_target = fresh_db();
        let error = require_consent_revocation_superset(&floor, &empty_target).unwrap_err();
        assert!(error.contains("forget 1"), "{error}");

        // A target carrying the same recording UNREVOKED resurrects the withdrawn voice.
        let resurrected = fresh_db();
        segment(&resurrected, "withdrawn");
        unlock(&resurrected, "speech_segments");
        resurrected
            .connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                rusqlite::params!["withdrawn", "a".repeat(64)],
            )
            .unwrap();
        let error = require_consent_revocation_superset(&floor, &resurrected).unwrap_err();
        assert!(error.contains("resurrect 1"), "{error}");

        // Preserving the tombstone (any timestamp) satisfies the monotonic floor.
        resurrected
            .connection()
            .execute("UPDATE speech_segments SET rights_revoked_at='2026-08-31T00:00:00Z' WHERE id='withdrawn'", [])
            .unwrap();
        require_consent_revocation_superset(&floor, &resurrected).unwrap();

        // A legacy row without a canonical hash falls back to its exact stored path identity...
        let legacy_floor = fresh_db();
        segment(&legacy_floor, "legacy");
        unlock(&legacy_floor, "speech_segments");
        legacy_floor
            .connection()
            .execute("UPDATE speech_segments SET rights_revoked_at='2026-08-30T00:00:00Z' WHERE id='legacy'", [])
            .unwrap();
        let legacy_target = fresh_db();
        segment(&legacy_target, "legacy"); // same legacy.wav path, not revoked
        let error = require_consent_revocation_superset(&legacy_floor, &legacy_target).unwrap_err();
        assert!(error.contains("resurrect 1"), "{error}");

        // ...and a blank legacy identity fails closed instead of pretending it was preserved.
        legacy_floor.connection().execute("UPDATE speech_segments SET audio_path='   ' WHERE id='legacy'", []).unwrap();
        let error = require_consent_revocation_superset(&legacy_floor, &legacy_target).unwrap_err();
        assert!(error.contains("no safe durable identity"), "{error}");
    }

    #[test]
    fn durable_review_activity_detection_matches_the_bare_restore_gate() {
        let db = fresh_db();
        assert!(!has_durable_review_activity(&db).unwrap(), "a pristine database has no review activity");
        paid_segment(&db, "clip");
        assert!(!has_durable_review_activity(&db).unwrap(), "an unreviewed clip is not review activity");
        couch_skip(&db, "clip", 2);
        assert!(has_durable_review_activity(&db).unwrap(), "any journaled review action arms the gate");

        // The schema-v60 frontier singleton exists in every pristine database; only a NON-EMPTY
        // frontier is durable activity.
        let frontier_db = fresh_db();
        assert!(!has_durable_review_activity(&frontier_db).unwrap());
        unlock(&frontier_db, "review_effect_state");
        frontier_db
            .connection()
            .execute("UPDATE review_effect_state SET effective_after_review_event_id=5 WHERE singleton_key=1", [])
            .unwrap();
        assert!(has_durable_review_activity(&frontier_db).unwrap());

        // Reviewed truth living only on the segment row itself also counts.
        let reviewed_db = fresh_db();
        segment(&reviewed_db, "clip");
        unlock(&reviewed_db, "speech_segments");
        reviewed_db
            .connection()
            .execute("UPDATE speech_segments SET human_decision='accept' WHERE id='clip'", [])
            .unwrap();
        assert!(has_durable_review_activity(&reviewed_db).unwrap());
    }
}
