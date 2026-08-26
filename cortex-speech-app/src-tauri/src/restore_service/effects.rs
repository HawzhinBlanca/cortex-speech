//! Human-decision and review-effect graph validation for staged restores.

use super::authority::{exact_query_rows, require_encoded_row_equality};
use super::compensation::{is_canonical_lowercase_64_hex, is_canonical_lowercase_uuid};

/// Re-derive schema-v60 review-effect meaning from immutable authorities. Triggers constrain new
/// writes, but a restored file may already contain rows created with triggers disabled; this pass
/// therefore cross-checks the complete event/effect/inverse graph before the database is published.
pub(crate) fn validate_review_effect_semantics(db: &crate::db::Database) -> Result<(), String> {
    use rusqlite::OptionalExtension;

    fn optional_text_is_blank(value: Option<&str>) -> bool {
        match value {
            Some(value) => value.trim().is_empty(),
            None => true,
        }
    }

    #[derive(Clone)]
    struct DecisionEffect {
        id: i64,
        review_event_id: Option<i64>,
        segment_id: String,
        reviewer: Option<String>,
        source: String,
        operation_id: Option<String>,
        operation_payload_hash: Option<String>,
        action: String,
        served_transcript: String,
        decision_transcript: Option<String>,
        decision_annotated_transcript: Option<String>,
        decision_verified: i64,
        decision_corrected_at: String,
        decision_rationale: Option<String>,
        requested_action: Option<String>,
        requested_transcript: Option<String>,
        requested_timestamp_ms: Option<i64>,
        prior_revision: i64,
        decision_revision: i64,
        prior_verified: i64,
        prior_annotated_transcript: Option<String>,
        prior_verdict: Option<String>,
        prior_verdict_transcript: Option<String>,
        prior_rationale: Option<String>,
        prior_escalated: i64,
        prior_human_decision: Option<String>,
        prior_corrected_at: Option<String>,
        prior_reviewed_by: Option<String>,
        reversal_operation: Option<String>,
    }

    #[derive(Clone)]
    struct FlagEffect {
        id: i64,
        operation_id: String,
        segment_id: String,
        prior_revision: i64,
        flag_revision: i64,
        prior_verdict: Option<String>,
        prior_rationale: Option<String>,
        flag_rationale: String,
        prior_escalated: i64,
        reversal_operation: Option<String>,
    }

    #[derive(Clone)]
    struct PostV60Event {
        id: i64,
        segment_id: String,
        reviewer: String,
        action: String,
        compensation_action: String,
        source: String,
        operation_id: String,
        operation_payload_hash: String,
        requested_action: String,
        requested_transcript: String,
        served_transcript: String,
        served_revision: i64,
    }

    #[derive(Clone)]
    enum ReviewMutation {
        Decision(Box<DecisionEffect>),
        Flag(FlagEffect),
    }

    #[derive(PartialEq, Eq)]
    struct DecisionOwnedState {
        verified: i64,
        annotated_transcript: Option<String>,
        verdict: Option<String>,
        verdict_transcript: Option<String>,
        escalated: i64,
        human_decision: Option<String>,
        corrected_at: Option<String>,
        reviewed_by: Option<String>,
    }

    #[derive(PartialEq, Eq)]
    struct FlagOwnedState {
        verdict: Option<String>,
        rationale: Option<String>,
        escalated: i64,
    }

    #[derive(Clone, PartialEq, Eq)]
    struct StableHumanState {
        verified: i64,
        annotated_transcript: Option<String>,
        verdict_transcript: Option<String>,
        human_decision: Option<String>,
        corrected_at: Option<String>,
        reviewed_by: Option<String>,
    }

    #[derive(Clone)]
    struct LegacyReviewedState {
        review_revision: i64,
        human_decision: Option<String>,
        verdict: Option<String>,
        verdict_transcript: Option<String>,
        annotated_transcript: Option<String>,
        verified: i64,
        reviewed_by: Option<String>,
        corrected_at: Option<String>,
        escalated: i64,
        is_gold: i64,
        rationale: Option<String>,
    }

    fn decision_terminal_state(effect: &DecisionEffect) -> DecisionOwnedState {
        if effect.reversal_operation.is_some() {
            DecisionOwnedState {
                verified: effect.prior_verified,
                annotated_transcript: effect.prior_annotated_transcript.clone(),
                verdict: effect.prior_verdict.clone(),
                verdict_transcript: effect.prior_verdict_transcript.clone(),
                escalated: effect.prior_escalated,
                human_decision: effect.prior_human_decision.clone(),
                corrected_at: effect.prior_corrected_at.clone(),
                reviewed_by: effect.prior_reviewed_by.clone(),
            }
        } else {
            DecisionOwnedState {
                verified: effect.decision_verified,
                annotated_transcript: effect.decision_annotated_transcript.clone(),
                verdict: Some(format!("human_{}", effect.action)),
                verdict_transcript: if effect.action == "reject" {
                    effect.prior_verdict_transcript.clone()
                } else {
                    effect.decision_transcript.clone()
                },
                escalated: 0,
                human_decision: Some(effect.action.clone()),
                corrected_at: Some(effect.decision_corrected_at.clone()),
                reviewed_by: effect.reviewer.clone(),
            }
        }
    }

    fn flag_terminal_state(effect: &FlagEffect) -> FlagOwnedState {
        if effect.reversal_operation.is_some() {
            FlagOwnedState {
                verdict: effect.prior_verdict.clone(),
                rationale: effect.prior_rationale.clone(),
                escalated: effect.prior_escalated,
            }
        } else {
            FlagOwnedState {
                verdict: Some("escalated".to_string()),
                rationale: Some(effect.flag_rationale.clone()),
                escalated: 1,
            }
        }
    }

    fn decision_prior_stable_state(effect: &DecisionEffect) -> StableHumanState {
        StableHumanState {
            verified: effect.prior_verified,
            annotated_transcript: effect.prior_annotated_transcript.clone(),
            verdict_transcript: effect.prior_verdict_transcript.clone(),
            human_decision: effect.prior_human_decision.clone(),
            corrected_at: effect.prior_corrected_at.clone(),
            reviewed_by: effect.prior_reviewed_by.clone(),
        }
    }

    fn decision_terminal_stable_state(effect: &DecisionEffect) -> StableHumanState {
        let terminal = decision_terminal_state(effect);
        StableHumanState {
            verified: terminal.verified,
            annotated_transcript: terminal.annotated_transcript,
            verdict_transcript: terminal.verdict_transcript,
            human_decision: terminal.human_decision,
            corrected_at: terminal.corrected_at,
            reviewed_by: terminal.reviewed_by,
        }
    }

    type CurrentReviewState = (
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    impl ReviewMutation {
        fn segment_id(&self) -> &str {
            match self {
                Self::Decision(effect) => &effect.segment_id,
                Self::Flag(effect) => &effect.segment_id,
            }
        }

        fn prior_revision(&self) -> i64 {
            match self {
                Self::Decision(effect) => effect.prior_revision,
                Self::Flag(effect) => effect.prior_revision,
            }
        }

        fn applied_revision(&self) -> i64 {
            match self {
                Self::Decision(effect) => effect.decision_revision,
                Self::Flag(effect) => effect.flag_revision,
            }
        }

        fn terminal_revision(&self) -> i64 {
            self.applied_revision()
                + match self {
                    Self::Decision(effect) => i64::from(effect.reversal_operation.is_some()),
                    Self::Flag(effect) => i64::from(effect.reversal_operation.is_some()),
                }
        }
    }

    let mut state_statement = db
        .connection()
        .prepare(
            "SELECT singleton_key, effective_after_review_event_id,
                    effective_after_ledger_id, created_at
               FROM review_effect_state ORDER BY singleton_key",
        )
        .map_err(|error| format!("restore target review-effect frontier is unreadable: {error}"))?;
    let states = state_statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?))
        })
        .map_err(|error| format!("restore target review-effect frontier is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target review-effect frontier is unreadable: {error}"))?;
    drop(state_statement);
    if states.len() != 1 || states[0].0 != 1 || states[0].1 < 0 || states[0].2 < 0 || states[0].3.trim().is_empty() {
        return Err("database restore refused: review_effect_state is not the one canonical schema-v60 frontier row"
            .to_string());
    }
    let event_frontier = states[0].1;
    let ledger_frontier = states[0].2;
    let maximum_event_id: i64 = db
        .connection()
        .query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))
        .map_err(|error| format!("restore target review-event frontier cannot be verified: {error}"))?;
    let maximum_ledger_id: i64 = db
        .connection()
        .query_row("SELECT COALESCE(MAX(id), 0) FROM review_compensation_ledger", [], |row| row.get(0))
        .map_err(|error| format!("restore target review-ledger frontier cannot be verified: {error}"))?;
    if event_frontier > maximum_event_id || ledger_frontier > maximum_ledger_id {
        return Err(format!(
            "database restore refused: review-effect frontiers ({event_frontier}, {ledger_frontier}) exceed retained history ({maximum_event_id}, {maximum_ledger_id})"
        ));
    }

    let mut event_statement = db
        .connection()
        .prepare(
            "SELECT id, segment_id, reviewer, action, compensation_action, source, app_git_sha,
                    playback_guard_version, operation_id, operation_payload_hash,
                    requested_action, requested_transcript, served_transcript, served_revision
               FROM review_events WHERE id > ?1 ORDER BY id",
        )
        .map_err(|error| format!("restore target post-v60 review events are unreadable: {error}"))?;
    let post_v60_events = event_statement
        .query_map([event_frontier], |row| {
            Ok((
                PostV60Event {
                    id: row.get(0)?,
                    segment_id: row.get(1)?,
                    reviewer: row.get(2)?,
                    action: row.get(3)?,
                    compensation_action: row.get(4)?,
                    source: row.get(5)?,
                    operation_id: row.get(8)?,
                    operation_payload_hash: row.get(9)?,
                    requested_action: row.get(10)?,
                    requested_transcript: row.get(11)?,
                    served_transcript: row.get(12)?,
                    served_revision: row.get(13)?,
                },
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(|error| format!("restore target post-v60 review events are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target post-v60 review events are unreadable: {error}"))?;
    drop(event_statement);

    let mut post_v60_events_by_id = std::collections::HashMap::<i64, PostV60Event>::new();
    for (event, git_sha, playback_guard) in &post_v60_events {
        let git_sha = git_sha.as_deref().unwrap_or_default();
        let request_text_is_canonical =
            crate::db::to_nfc(event.requested_transcript.trim()) == event.requested_transcript;
        let served_text_is_canonical = !event.served_transcript.is_empty()
            && crate::db::to_nfc(event.served_transcript.trim()) == event.served_transcript;
        let expected_payload_hash = crate::db::review_operation_payload_hash(
            &event.segment_id,
            &event.requested_action,
            &event.requested_transcript,
            &event.reviewer,
        );
        let request_classification_is_valid = match event.requested_action.as_str() {
            "skip" => event.action == "skip" && event.compensation_action == "skip",
            "bad" | "reject" => event.action == "reject" && event.compensation_action == "reject",
            "accept" | "edit" => {
                let expected_compensation = if crate::normalizer::learning_text_key(&event.requested_transcript)
                    == crate::normalizer::learning_text_key(&event.served_transcript)
                {
                    "accept"
                } else {
                    "edit"
                };
                matches!(event.action.as_str(), "accept" | "edit") && event.compensation_action == expected_compensation
            }
            _ => false,
        };
        if !matches!(event.source.as_str(), "couch" | "couch_spot_check")
            || !matches!(event.action.as_str(), "accept" | "edit" | "reject" | "skip")
            || !matches!(event.requested_action.as_str(), "accept" | "edit" | "reject" | "bad" | "skip")
            || !is_canonical_lowercase_uuid(&event.operation_id)
            || !is_canonical_lowercase_64_hex(&event.operation_payload_hash)
            || event.operation_payload_hash != expected_payload_hash
            || !request_text_is_canonical
            || !served_text_is_canonical
            || event.served_revision < 0
            || !request_classification_is_valid
            || git_sha.len() != 40
            || !git_sha.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !matches!(playback_guard.as_deref(), Some("content-hash-raw-counter-v3" | "interval-authority-v4"))
        {
            return Err(format!(
                "database restore refused: post-v60 review event {} lacks canonical Couch/build/playback provenance",
                event.id
            ));
        }
        post_v60_events_by_id.insert(event.id, event.clone());
        let total_effects: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM human_decision_effect_events WHERE review_event_id = ?1",
                [event.id],
                |row| row.get(0),
            )
            .map_err(|error| format!("restore target decision-effect linkage is unreadable: {error}"))?;
        if event.source == "couch" && event.action != "skip" {
            let exact_effects: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*)
                       FROM human_decision_effect_events effect
                       JOIN review_compensation_ledger ledger
                         ON ledger.review_event_id = ?1
                        AND ledger.reverses_entry_id IS NULL
                      WHERE effect.review_event_id = ?1
                        AND effect.segment_id = ?2
                        AND effect.reviewer = ?3
                        AND effect.source = 'couch'
                        AND effect.action = ?4
                        AND ledger.segment_id = effect.segment_id
                        AND ledger.reviewer = effect.reviewer
                        AND ledger.source = effect.source
                        AND ledger.effective_decision = effect.action
                        AND ledger.decision_revision IS effect.decision_revision",
                    rusqlite::params![event.id, event.segment_id, event.reviewer, event.action],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target decision-effect linkage is unreadable: {error}"))?;
            if total_effects != 1 || exact_effects != 1 {
                return Err(format!(
                    "database restore refused: post-v60 Couch decision event {} does not have exactly one matching human/pay effect",
                    event.id
                ));
            }
        } else if total_effects != 0 {
            return Err(format!(
                "database restore refused: post-v60 {}/{} event {} must not create a human-decision effect",
                event.source, event.action, event.id
            ));
        }
    }

    let mut effect_statement = db
        .connection()
        .prepare(
            "SELECT effect.id, effect.review_event_id, effect.segment_id, effect.reviewer,
                    effect.source, effect.operation_id, effect.operation_payload_hash,
                    effect.action, effect.served_transcript, effect.decision_transcript,
                    effect.decision_annotated_transcript, effect.decision_verified,
                    effect.decision_corrected_at, effect.decision_rationale, effect.requested_action,
                    effect.requested_transcript, effect.requested_timestamp_ms,
                    effect.prior_revision, effect.decision_revision, effect.prior_verified,
                    effect.prior_annotated_transcript, effect.prior_verdict,
                    effect.prior_verdict_transcript, effect.prior_rationale, effect.prior_escalated,
                    effect.prior_human_decision, effect.prior_corrected_at,
                    effect.prior_reviewed_by, reversal.operation_id
               FROM human_decision_effect_events effect
               LEFT JOIN human_decision_effect_reversals reversal
                 ON reversal.effect_event_id = effect.id
              ORDER BY effect.id",
        )
        .map_err(|error| format!("restore target human-decision effects are unreadable: {error}"))?;
    let effects = effect_statement
        .query_map([], |row| {
            Ok(DecisionEffect {
                id: row.get(0)?,
                review_event_id: row.get(1)?,
                segment_id: row.get(2)?,
                reviewer: row.get(3)?,
                source: row.get(4)?,
                operation_id: row.get(5)?,
                operation_payload_hash: row.get(6)?,
                action: row.get(7)?,
                served_transcript: row.get(8)?,
                decision_transcript: row.get(9)?,
                decision_annotated_transcript: row.get(10)?,
                decision_verified: row.get(11)?,
                decision_corrected_at: row.get(12)?,
                decision_rationale: row.get(13)?,
                requested_action: row.get(14)?,
                requested_transcript: row.get(15)?,
                requested_timestamp_ms: row.get(16)?,
                prior_revision: row.get(17)?,
                decision_revision: row.get(18)?,
                prior_verified: row.get(19)?,
                prior_annotated_transcript: row.get(20)?,
                prior_verdict: row.get(21)?,
                prior_verdict_transcript: row.get(22)?,
                prior_rationale: row.get(23)?,
                prior_escalated: row.get(24)?,
                prior_human_decision: row.get(25)?,
                prior_corrected_at: row.get(26)?,
                prior_reviewed_by: row.get(27)?,
                reversal_operation: row.get(28)?,
            })
        })
        .map_err(|error| format!("restore target human-decision effects are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target human-decision effects are unreadable: {error}"))?;
    drop(effect_statement);

    let effects_by_id = effects.iter().map(|effect| (effect.id, effect)).collect::<std::collections::HashMap<_, _>>();
    for effect in &effects {
        if effect.id <= 0
            || effect.segment_id.trim().is_empty()
            || effect.decision_revision != effect.prior_revision + 1
            || !matches!(effect.action.as_str(), "accept" | "edit" | "reject")
            || !matches!(effect.decision_verified, 0 | 1)
            || !matches!(effect.prior_verified, 0 | 1)
            || !matches!(effect.prior_escalated, 0 | 1)
            || effect.decision_corrected_at.trim().is_empty()
            || effect.decision_rationale != effect.prior_rationale
            || effect.served_transcript.is_empty()
            || crate::db::to_nfc(effect.served_transcript.trim()) != effect.served_transcript
        {
            return Err(format!(
                "database restore refused: human-decision effect {} violates its immutable identity/revision boundary",
                effect.id
            ));
        }
        let canonical_decision_text = effect
            .decision_transcript
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty() && crate::db::to_nfc(text.trim()) == text);
        if (matches!(effect.action.as_str(), "accept" | "edit")
            && (!canonical_decision_text || effect.decision_annotated_transcript != effect.decision_transcript))
            || (effect.action == "reject" && effect.decision_transcript.is_some())
        {
            return Err(format!(
                "database restore refused: human-decision effect {} has no exact canonical post-decision transcript",
                effect.id
            ));
        }
        if let Some(event_id) = effect.review_event_id {
            let Some(event) = post_v60_events_by_id.get(&event_id) else {
                return Err(format!(
                    "database restore refused: phone decision effect {} names no post-v60 review event",
                    effect.id
                ));
            };
            let exact_link: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*)
                       FROM review_events event
                       JOIN review_compensation_ledger ledger
                         ON ledger.review_event_id = event.id
                        AND ledger.reverses_entry_id IS NULL
                      WHERE event.id = ?1 AND event.id > ?2
                        AND event.segment_id = ?3
                        AND event.reviewer = ?4
                        AND event.source = 'couch'
                        AND event.action = ?5
                        AND ledger.segment_id = ?3
                        AND ledger.reviewer = ?4
                        AND ledger.source = 'couch'
                        AND ledger.effective_decision = ?5
                        AND ledger.decision_revision IS ?6",
                    rusqlite::params![
                        event_id,
                        event_frontier,
                        effect.segment_id,
                        effect.reviewer,
                        effect.action,
                        effect.decision_revision,
                    ],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target phone-effect linkage is unreadable: {error}"))?;
            if effect.source != "couch"
                || optional_text_is_blank(effect.reviewer.as_deref())
                || effect.operation_id.is_some()
                || effect.operation_payload_hash.is_some()
                || effect.requested_action.is_some()
                || effect.requested_transcript.is_some()
                || effect.requested_timestamp_ms.is_some()
                || event.segment_id != effect.segment_id
                || event.reviewer.as_str() != effect.reviewer.as_deref().unwrap_or_default()
                || event.action != effect.action
                || event.served_transcript != effect.served_transcript
                || event.served_revision != effect.prior_revision
                || exact_link != 1
            {
                return Err(format!(
                    "database restore refused: phone decision effect {} is not the exact post-v60 event/pay effect",
                    effect.id
                ));
            }
        } else {
            let desktop_request_ok = match (
                effect.operation_id.as_deref(),
                effect.operation_payload_hash.as_deref(),
                effect.requested_action.as_deref(),
                effect.requested_timestamp_ms,
            ) {
                (Some(operation_id), Some(payload_hash), Some(requested_action), Some(timestamp_ms)) => {
                    is_canonical_lowercase_uuid(operation_id)
                        && is_canonical_lowercase_64_hex(payload_hash)
                        && matches!(requested_action, "accept" | "edit" | "reject")
                        && timestamp_ms > 0
                        && effect
                            .requested_transcript
                            .as_deref()
                            .map_or(true, |text| crate::db::to_nfc(text.trim()) == text && !text.is_empty())
                        && crate::db::desktop_decision_payload_hash(
                            &effect.segment_id,
                            requested_action,
                            effect.requested_transcript.as_deref(),
                            Some(timestamp_ms),
                        ) == payload_hash
                }
                _ => false,
            };
            if effect.source != "desktop" || effect.reviewer.is_some() || !desktop_request_ok {
                return Err(format!(
                    "database restore refused: unlinked human-decision effect {} is outside the exact anonymous desktop operation boundary",
                    effect.id
                ));
            }
        }

        let original_reversal_count: i64 = if let Some(event_id) = effect.review_event_id {
            db.connection()
                .query_row(
                    "SELECT COUNT(*)
                       FROM review_compensation_ledger original
                       JOIN review_compensation_ledger reversal
                         ON reversal.reverses_entry_id = original.entry_id
                      WHERE original.review_event_id = ?1
                        AND original.reverses_entry_id IS NULL",
                    [event_id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target effect reversal linkage is unreadable: {error}"))?
        } else {
            0
        };
        if let Some(operation_id) = effect.reversal_operation.as_deref() {
            if !is_canonical_lowercase_uuid(operation_id) {
                return Err(format!(
                    "database restore refused: human-decision reversal {} has no canonical operation UUID",
                    effect.id
                ));
            }
            if let Some(event_id) = effect.review_event_id {
                let exact_inverse: i64 = db
                    .connection()
                    .query_row(
                        "SELECT COUNT(*)
                           FROM review_events event
                           JOIN review_compensation_ledger original
                             ON original.review_event_id = event.id
                            AND original.reverses_entry_id IS NULL
                           JOIN review_compensation_ledger reversal
                             ON reversal.reverses_entry_id = original.entry_id
                          WHERE event.id = ?1
                            AND event.operation_id = ?2
                            AND reversal.id > ?3
                            AND reversal.entry_key = 'undo:' || ?2
                            AND reversal.policy_version = original.policy_version
                            AND reversal.canonical_work_id = original.canonical_work_id
                            AND reversal.canonical_identity_kind = original.canonical_identity_kind
                            AND reversal.reviewer = original.reviewer
                            AND reversal.segment_id = original.segment_id
                            AND reversal.source = 'couch_undo'
                            AND reversal.compensation_action = 'undo'
                            AND reversal.effective_decision = 'undo'
                            AND reversal.decision_revision IS original.decision_revision
                            AND reversal.duration_ms = original.duration_ms
                            AND reversal.rate_basis_points = 0
                            AND reversal.entitlement_micro_iqd = 0
                            AND reversal.delta_micro_iqd = -original.delta_micro_iqd
                            AND reversal.delta_corrected_ms = -original.delta_corrected_ms",
                        rusqlite::params![event_id, operation_id, ledger_frontier],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("restore target effect reversal linkage is unreadable: {error}"))?;
                if original_reversal_count != 1 || exact_inverse != 1 {
                    return Err(format!(
                        "database restore refused: phone decision reversal {} lacks its exact operation-bound compensation inverse",
                        effect.id
                    ));
                }
            } else {
                let conflicting_pay_inverse: i64 = db
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM review_compensation_ledger
                          WHERE entry_key = 'undo:' || ?1",
                        [operation_id],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("restore target desktop reversal identity is unreadable: {error}"))?;
                if conflicting_pay_inverse != 0 {
                    return Err(format!(
                        "database restore refused: desktop decision reversal {} reuses a paid-review inverse identity",
                        effect.id
                    ));
                }
            }
        } else if original_reversal_count != 0 {
            return Err(format!(
                "database restore refused: active phone decision effect {} already has a compensation inverse",
                effect.id
            ));
        }
    }

    let mut reversal_statement = db
        .connection()
        .prepare(
            "SELECT id FROM review_compensation_ledger
              WHERE id > ?1 AND reverses_entry_id IS NOT NULL ORDER BY id",
        )
        .map_err(|error| format!("restore target post-v60 compensation reversals are unreadable: {error}"))?;
    let post_v60_reversal_ids = reversal_statement
        .query_map([ledger_frontier], |row| row.get::<_, i64>(0))
        .map_err(|error| format!("restore target post-v60 compensation reversals are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target post-v60 compensation reversals are unreadable: {error}"))?;
    drop(reversal_statement);
    for reversal_id in post_v60_reversal_ids {
        let matching_effect_inverse: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*)
                   FROM review_compensation_ledger reversal
                   JOIN review_compensation_ledger original
                     ON original.entry_id = reversal.reverses_entry_id
                   JOIN human_decision_effect_events effect
                     ON effect.review_event_id = original.review_event_id
                   JOIN human_decision_effect_reversals effect_reversal
                     ON effect_reversal.effect_event_id = effect.id
                   JOIN review_events event ON event.id = effect.review_event_id
                  WHERE reversal.id = ?1
                    AND reversal.entry_key = 'undo:' || effect_reversal.operation_id
                    AND event.operation_id = effect_reversal.operation_id",
                [reversal_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("restore target post-v60 compensation reversal linkage is unreadable: {error}"))?;
        if matching_effect_inverse != 1 {
            return Err(format!(
                "database restore refused: post-v60 compensation reversal {reversal_id} is not owned by one exact human-effect reversal"
            ));
        }
    }

    let (legacy_example_columns, legacy_example_rows) =
        exact_query_rows(db, "legacy agent-example snapshot", "SELECT * FROM legacy_agent_examples_v60")?;
    let (raw_legacy_example_columns, raw_legacy_example_rows) = exact_query_rows(
        db,
        "raw legacy agent examples",
        "SELECT example.rowid AS original_rowid, example.id, example.segment_id,
                example.audio_features, example.wrong_transcript, example.human_fix,
                example.created_at, example.source, example.verified_by_human,
                example.corrector_model_id
           FROM agent_examples example
          WHERE example.effect_event_id IS NULL
            AND EXISTS (
                 SELECT 1 FROM legacy_agent_examples_v60 legacy
                  WHERE legacy.id = example.id
            )",
    )?;
    require_encoded_row_equality(
        "legacy agent-example snapshot versus retained raw rows",
        legacy_example_columns,
        legacy_example_rows,
        raw_legacy_example_columns,
        raw_legacy_example_rows,
    )?;
    let forged_unbound_human_examples: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*)
               FROM agent_examples example
              WHERE example.effect_event_id IS NULL
                AND (example.source = 'human' OR example.verified_by_human = 1)
                AND NOT EXISTS (
                     SELECT 1 FROM legacy_agent_examples_v60 legacy
                      WHERE legacy.id = example.id AND legacy.original_rowid = example.rowid
                )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("restore target unbound agent-example provenance is unreadable: {error}"))?;
    if forged_unbound_human_examples != 0 {
        return Err(
            "database restore refused: post-v60 unbound rows cannot claim human agent-example provenance".to_string()
        );
    }

    let (legacy_correction_columns, legacy_correction_rows) =
        exact_query_rows(db, "legacy correction snapshot", "SELECT * FROM legacy_corrections_v60")?;
    let (raw_legacy_correction_columns, raw_legacy_correction_rows) = exact_query_rows(
        db,
        "raw legacy corrections",
        "SELECT correction.rowid AS original_rowid, correction.id, correction.segment_id,
                correction.audio_content_hash, correction.raw_hypothesis,
                correction.ensemble_hyps_json, correction.agreement_score,
                correction.jury_verdict, correction.human_fix,
                correction.model_version_id, correction.adapter_id,
                correction.reviewer_id, correction.loop_applied, correction.decided_at
           FROM corrections correction
          WHERE correction.effect_event_id IS NULL",
    )?;
    require_encoded_row_equality(
        "legacy correction snapshot versus retained raw rows",
        legacy_correction_columns,
        legacy_correction_rows,
        raw_legacy_correction_columns,
        raw_legacy_correction_rows,
    )?;

    let mut example_statement = db
        .connection()
        .prepare(
            "SELECT id, segment_id, wrong_transcript, human_fix, source,
                    verified_by_human, effect_event_id
               FROM agent_examples WHERE effect_event_id IS NOT NULL ORDER BY id",
        )
        .map_err(|error| format!("restore target effect-bound human examples are unreadable: {error}"))?;
    let examples = example_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(|error| format!("restore target effect-bound human examples are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target effect-bound human examples are unreadable: {error}"))?;
    drop(example_statement);
    for (id, segment_id, wrong, fix, source, verified, effect_id) in examples {
        let Some(effect) = effects_by_id.get(&effect_id).copied() else {
            return Err(format!(
                "database restore refused: effect-bound agent example {id} names a missing decision effect"
            ));
        };
        let exact_correction_text: Option<(String, String)> = db
            .connection()
            .query_row(
                "SELECT raw_hypothesis, human_fix FROM corrections WHERE effect_event_id = ?1",
                [effect_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("restore target example/correction linkage is unreadable: {error}"))?;
        let retained_draft: Option<(Option<String>, String)> = db
            .connection()
            .query_row(
                "SELECT normalized_transcript, raw_transcript FROM speech_segments WHERE id = ?1",
                [&effect.segment_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("restore target example wrong-side provenance is unreadable: {error}"))?;
        let expected_wrong = retained_draft.and_then(|(normalized, raw)| {
            crate::db::rejected_transcript_for_learning(
                &fix,
                &[
                    effect.prior_verdict_transcript.clone(),
                    effect.prior_annotated_transcript.clone(),
                    normalized,
                    Some(raw),
                ],
            )
        });
        if !is_canonical_lowercase_uuid(&id)
            || segment_id != effect.segment_id
            || effect.action != "edit"
            || source != "human"
            || verified != 1
            || wrong.trim().is_empty()
            || fix.trim().is_empty()
            || crate::normalizer::learning_text_key(&wrong) == crate::normalizer::learning_text_key(&fix)
            || effect.decision_transcript.as_deref() != Some(fix.as_str())
            || expected_wrong.as_deref() != Some(wrong.as_str())
            || exact_correction_text.as_ref() != Some(&(wrong.clone(), fix.clone()))
        {
            return Err(format!(
                "database restore refused: effect-bound agent example {id} is not one genuine human edit"
            ));
        }
    }

    let mut correction_statement = db
        .connection()
        .prepare(
            "SELECT id, segment_id, audio_content_hash, raw_hypothesis, human_fix,
                    reviewer_id, effect_event_id
               FROM corrections WHERE effect_event_id IS NOT NULL ORDER BY id",
        )
        .map_err(|error| format!("restore target effect-bound corrections are unreadable: {error}"))?;
    let corrections = correction_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(|error| format!("restore target effect-bound corrections are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target effect-bound corrections are unreadable: {error}"))?;
    drop(correction_statement);
    let mut correction_text_by_effect = std::collections::HashMap::<i64, (String, String)>::new();
    for (id, segment_id, audio_hash, wrong, fix, reviewer, effect_id) in corrections {
        let Some(effect) = effects_by_id.get(&effect_id).copied() else {
            return Err(format!(
                "database restore refused: effect-bound correction {id} names a missing decision effect"
            ));
        };
        let reviewer_matches = match (reviewer.as_deref(), effect.reviewer.as_deref()) {
            (None, None) => true,
            (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
            _ => false,
        };
        let retained_segment_matches = match segment_id.as_deref() {
            Some(segment_id) if segment_id == effect.segment_id => {
                db.connection()
                    .query_row(
                        "SELECT audio_content_hash = ?2 FROM speech_segments WHERE id = ?1",
                        rusqlite::params![segment_id, audio_hash],
                        |row| row.get::<_, bool>(0),
                    )
                    .optional()
                    .map_err(|error| format!("restore target correction segment identity is unreadable: {error}"))?
                    == Some(true)
            }
            _ => false,
        };
        let retained_draft: Option<(Option<String>, String)> = db
            .connection()
            .query_row(
                "SELECT normalized_transcript, raw_transcript FROM speech_segments WHERE id = ?1",
                [&effect.segment_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("restore target correction wrong-side provenance is unreadable: {error}"))?;
        let expected_wrong = retained_draft.map(|(normalized, raw)| {
            crate::db::rejected_transcript_for_learning(
                &fix,
                &[
                    effect.prior_verdict_transcript.clone(),
                    effect.prior_annotated_transcript.clone(),
                    normalized,
                    Some(raw.clone()),
                ],
            )
            .unwrap_or(raw)
        });
        if !is_canonical_lowercase_uuid(&id)
            || effect.action != "edit"
            || !retained_segment_matches
            || !reviewer_matches
            || !crate::db::is_canonical_audio_content_hash(&audio_hash)
            || wrong.trim().is_empty()
            || fix.trim().is_empty()
            || crate::normalizer::learning_text_key(&wrong) == crate::normalizer::learning_text_key(&fix)
            || effect.decision_transcript.as_deref() != Some(fix.as_str())
            || expected_wrong.as_deref() != Some(wrong.as_str())
        {
            return Err(format!(
                "database restore refused: effect-bound correction {id} violates edit/audio/reviewer identity"
            ));
        }
        if correction_text_by_effect.insert(effect_id, (wrong, fix)).is_some() {
            return Err(format!("database restore refused: decision effect {effect_id} owns more than one correction"));
        }
    }

    let mut memory_statement = db
        .connection()
        .prepare(
            "SELECT id, wrong_token, human_token, slot_key, phonetic_key, source_segment,
                    confidence, hit_count, last_fired_at, confirm_count, override_count,
                    legacy_seed
               FROM correction_memory ORDER BY id",
        )
        .map_err(|error| format!("restore target correction-memory identities are unreadable: {error}"))?;
    let memories = memory_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, f64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
            ))
        })
        .map_err(|error| format!("restore target correction-memory identities are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target correction-memory identities are unreadable: {error}"))?;
    drop(memory_statement);
    let memory_ids = memories.iter().map(|memory| memory.0.as_str()).collect::<std::collections::HashSet<_>>();
    for (
        id,
        wrong,
        human,
        slot,
        _phonetic,
        source_segment,
        confidence,
        hit_count,
        last_fired_at,
        confirm_count,
        override_count,
        legacy_seed,
    ) in &memories
    {
        if *legacy_seed == 0 {
            let capture_count: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM correction_memory_contributions
                      WHERE memory_id = ?1 AND capture_delta = 1",
                    [id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target correction-memory capture lineage is unreadable: {error}"))?;
            let capture_origin_count: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*)
                       FROM correction_memory_contributions contribution
                       JOIN human_decision_effect_events effect
                         ON effect.id = contribution.effect_event_id
                      WHERE contribution.memory_id = ?1
                        AND contribution.capture_delta = 1
                        AND (?2 IS NULL OR effect.segment_id = ?2)",
                    rusqlite::params![id, source_segment],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target correction-memory capture identity is unreadable: {error}"))?;
            if !is_canonical_lowercase_uuid(id)
                || wrong.trim().is_empty()
                || human.trim().is_empty()
                || slot.trim().is_empty()
                || crate::normalizer::learning_text_key(wrong) == crate::normalizer::learning_text_key(human)
                || !confidence.is_finite()
                || (*confidence - 0.5).abs() > f64::EPSILON
                || *hit_count != 0
                || *confirm_count != 0
                || *override_count != 0
                || last_fired_at.is_some()
                || capture_count == 0
                || capture_origin_count == 0
            {
                return Err(format!(
                    "database restore refused: post-v60 correction memory {id} lacks its zero-baseline capture identity"
                ));
            }
        } else if *legacy_seed != 1 {
            return Err(format!("database restore refused: correction memory {id} has an invalid legacy boundary"));
        }
    }

    let mut contribution_statement = db
        .connection()
        .prepare(
            "SELECT effect_event_id, memory_id, capture_delta, confirm_delta,
                    override_delta, fired_at
               FROM correction_memory_contributions ORDER BY effect_event_id, memory_id",
        )
        .map_err(|error| format!("restore target correction-memory contributions are unreadable: {error}"))?;
    let contributions = contribution_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(|error| format!("restore target correction-memory contributions are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target correction-memory contributions are unreadable: {error}"))?;
    drop(contribution_statement);
    for (effect_id, memory_id, capture, confirm, override_delta, fired_at) in &contributions {
        let Some(effect) = effects_by_id.get(effect_id).copied() else {
            return Err(format!(
                "database restore refused: correction-memory contribution {effect_id}/{memory_id} names a missing effect"
            ));
        };
        let evidence_fired = confirm + override_delta > 0;
        if !memory_ids.contains(memory_id.as_str())
            || !matches!(effect.action.as_str(), "accept" | "edit")
            || !matches!(*capture, 0 | 1)
            || !matches!(*confirm, 0 | 1)
            || !matches!(*override_delta, 0 | 1)
            || capture + confirm + override_delta == 0
            || confirm + override_delta > 1
            || (*capture == 1 && effect.action != "edit")
            || evidence_fired != fired_at.as_deref().is_some_and(|value| !value.trim().is_empty())
        {
            return Err(format!(
                "database restore refused: correction-memory contribution {effect_id}/{memory_id} violates its action/evidence identity"
            ));
        }
    }

    // Re-derive every post-v60 memory capture from the exact immutable correction owned by the
    // same decision effect. Merely linking arbitrary tokens to an edit effect is not provenance:
    // those tokens feed the live corrector. The extracted substitution tuple (including phonetic
    // key) and the contribution set must be byte-exact.
    type MemoryNaturalKey = (String, String, String, String);
    let memory_by_id =
        memories.iter().map(|memory| (memory.0.as_str(), memory)).collect::<std::collections::HashMap<_, _>>();
    let mut capture_ids_by_effect = std::collections::HashMap::<i64, std::collections::BTreeSet<String>>::new();
    let mut first_capture_effect_by_memory = std::collections::HashMap::<String, i64>::new();
    for (effect_id, memory_id, capture, _, _, _) in &contributions {
        if *capture == 1 {
            capture_ids_by_effect.entry(*effect_id).or_default().insert(memory_id.clone());
            first_capture_effect_by_memory
                .entry(memory_id.clone())
                .and_modify(|existing| *existing = (*existing).min(*effect_id))
                .or_insert(*effect_id);
        }
    }
    let memory_id_by_natural_key = memories
        .iter()
        .map(|memory| ((memory.3.clone(), memory.1.clone(), memory.2.clone(), memory.4.clone()), memory.0.clone()))
        .collect::<std::collections::HashMap<MemoryNaturalKey, String>>();

    for memory in &memories {
        if memory.11 != 0 {
            continue;
        }
        let Some(first_effect_id) = first_capture_effect_by_memory.get(&memory.0) else {
            return Err(format!(
                "database restore refused: post-v60 correction memory {} has no first capture effect",
                memory.0
            ));
        };
        let Some(first_effect) = effects_by_id.get(first_effect_id).copied() else {
            return Err(format!(
                "database restore refused: post-v60 correction memory {} names a missing first capture effect",
                memory.0
            ));
        };
        if memory.5.as_deref() != Some(first_effect.segment_id.as_str()) {
            return Err(format!(
                "database restore refused: post-v60 correction memory {} source segment differs from its first capture",
                memory.0
            ));
        }
    }

    for effect in &effects {
        let segment_is_gold: bool = db
            .connection()
            .query_row("SELECT is_gold FROM speech_segments WHERE id = ?1", [&effect.segment_id], |row| row.get(0))
            .optional()
            .map_err(|error| format!("restore target correction-memory segment state is unreadable: {error}"))?
            .unwrap_or(false);
        let mut expected_capture_ids = std::collections::BTreeSet::<String>::new();
        if !segment_is_gold {
            if let Some((wrong, fix)) = correction_text_by_effect.get(&effect.id) {
                let mut seen = std::collections::HashSet::<MemoryNaturalKey>::new();
                for extracted in crate::corrections::extract_substitution_memories(wrong, fix) {
                    let natural_key =
                        (extracted.slot_key, extracted.wrong_token, extracted.human_token, extracted.phonetic_key);
                    if seen.insert(natural_key.clone()) {
                        let Some(memory_id) = memory_id_by_natural_key.get(&natural_key) else {
                            return Err(format!(
                                "database restore refused: decision effect {} is missing an exactly derived correction memory",
                                effect.id
                            ));
                        };
                        expected_capture_ids.insert(memory_id.clone());
                    }
                }
            }
        }
        let actual_capture_ids = capture_ids_by_effect.get(&effect.id).cloned().unwrap_or_default();
        if actual_capture_ids != expected_capture_ids {
            return Err(format!(
                "database restore refused: decision effect {} has arbitrary or incomplete correction-memory captures",
                effect.id
            ));
        }
    }

    for (effect_id, memory_id, _, confirm, override_delta, _) in &contributions {
        if confirm + override_delta == 0 {
            continue;
        }
        let effect = effects_by_id[effect_id];
        let memory = memory_by_id[memory_id.as_str()];
        let existed_before_effect = memory.11 == 1
            || first_capture_effect_by_memory
                .get(memory_id)
                .is_some_and(|capture_effect_id| *capture_effect_id < *effect_id);
        let Some(reference) = effect.decision_transcript.as_deref() else {
            return Err(format!(
                "database restore refused: memory outcome {effect_id}/{memory_id} has no accepted decision text"
            ));
        };
        let entry = crate::corrections::MemoryEntry {
            wrong_token: memory.1.clone(),
            human_token: memory.2.clone(),
            slot_key: memory.3.clone(),
            phonetic_key: memory.4.clone(),
            confidence: memory.6,
            hit_count: memory.7,
        };
        let expected_outcome = crate::corrections::classify_memory_outcome(
            &effect.served_transcript,
            reference,
            &entry,
            &crate::corrections::FiringConfig::default(),
        );
        let outcome_matches = match expected_outcome {
            crate::corrections::MemoryOutcome::Confirm => *confirm == 1 && *override_delta == 0,
            crate::corrections::MemoryOutcome::Override => *confirm == 0 && *override_delta == 1,
            crate::corrections::MemoryOutcome::Neutral => false,
        };
        if !existed_before_effect || !outcome_matches {
            return Err(format!(
                "database restore refused: correction-memory outcome {effect_id}/{memory_id} is not re-derived from the served/decision text"
            ));
        }
    }

    let mut flag_statement = db
        .connection()
        .prepare(
            "SELECT effect.id, effect.operation_id, effect.segment_id, effect.prior_revision,
                    effect.flag_revision, effect.prior_verdict, effect.prior_rationale,
                    effect.flag_rationale, effect.prior_escalated, reversal.operation_id
               FROM review_flag_effect_events effect
               LEFT JOIN review_flag_effect_reversals reversal
                 ON reversal.flag_effect_event_id = effect.id
              ORDER BY effect.id",
        )
        .map_err(|error| format!("restore target review-flag effects are unreadable: {error}"))?;
    let flags = flag_statement
        .query_map([], |row| {
            Ok(FlagEffect {
                id: row.get(0)?,
                operation_id: row.get(1)?,
                segment_id: row.get(2)?,
                prior_revision: row.get(3)?,
                flag_revision: row.get(4)?,
                prior_verdict: row.get(5)?,
                prior_rationale: row.get(6)?,
                flag_rationale: row.get(7)?,
                prior_escalated: row.get(8)?,
                reversal_operation: row.get(9)?,
            })
        })
        .map_err(|error| format!("restore target review-flag effects are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target review-flag effects are unreadable: {error}"))?;
    drop(flag_statement);
    for flag in &flags {
        if flag.id <= 0
            || !is_canonical_lowercase_uuid(&flag.operation_id)
            || flag.segment_id.trim().is_empty()
            || flag.flag_revision != flag.prior_revision + 1
            || flag.flag_rationale.trim().is_empty()
            || crate::db::to_nfc(flag.flag_rationale.trim()) != flag.flag_rationale
            || !matches!(flag.prior_escalated, 0 | 1)
            || flag.reversal_operation.as_deref().is_some_and(|operation| !is_canonical_lowercase_uuid(operation))
        {
            return Err(format!(
                "database restore refused: review-flag effect {} violates its immutable revision/operation identity",
                flag.id
            ));
        }
        let initial_collision_count: i64 = db
            .connection()
            .query_row(
                "SELECT
                     (SELECT COUNT(*) FROM review_events WHERE operation_id = ?1)
                   + (SELECT COUNT(*) FROM human_decision_effect_events WHERE operation_id = ?1)
                   + (SELECT COUNT(*) FROM human_decision_effect_reversals WHERE operation_id = ?1)
                   + (SELECT COUNT(*) FROM review_flag_effect_reversals WHERE operation_id = ?1)",
                [&flag.operation_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("restore target flag operation identity is unreadable: {error}"))?;
        if initial_collision_count != 0 {
            return Err(format!(
                "database restore refused: review-flag effect {} reuses another review operation identity",
                flag.id
            ));
        }
        if let Some(operation_id) = flag.reversal_operation.as_deref() {
            let collision_count: i64 = db
                .connection()
                .query_row(
                    "SELECT
                         (SELECT COUNT(*) FROM review_events WHERE operation_id = ?1)
                       + (SELECT COUNT(*) FROM human_decision_effect_events WHERE operation_id = ?1)
                       + (SELECT COUNT(*) FROM human_decision_effect_reversals WHERE operation_id = ?1)
                       + (SELECT COUNT(*) FROM review_flag_effect_events WHERE operation_id = ?1)",
                    [operation_id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target flag-reversal identity is unreadable: {error}"))?;
            if collision_count != 0 {
                return Err(format!(
                    "database restore refused: review-flag reversal {} reuses another review operation identity",
                    flag.id
                ));
            }
        }
    }

    let mut expected_active_decisions = std::collections::BTreeMap::<String, i64>::new();
    for effect in &effects {
        if effect.reversal_operation.is_none() {
            expected_active_decisions.insert(effect.segment_id.clone(), effect.id);
        }
    }
    let mut actual_active_statement = db
        .connection()
        .prepare("SELECT segment_id, id FROM effective_human_decision_effects_v60 ORDER BY segment_id")
        .map_err(|error| format!("restore target effective decision projection is unreadable: {error}"))?;
    let actual_active_decisions = actual_active_statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
        .map_err(|error| format!("restore target effective decision projection is unreadable: {error}"))?
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
        .map_err(|error| format!("restore target effective decision projection is unreadable: {error}"))?;
    drop(actual_active_statement);
    if actual_active_decisions != expected_active_decisions {
        return Err(
            "database restore refused: effective human-decision projection does not select the latest active effect"
                .to_string(),
        );
    }

    let mut expected_active_flags = std::collections::BTreeMap::<String, i64>::new();
    for flag in &flags {
        if flag.reversal_operation.is_none() {
            expected_active_flags.insert(flag.segment_id.clone(), flag.id);
        }
    }
    let mut actual_flag_statement = db
        .connection()
        .prepare("SELECT segment_id, id FROM effective_review_flag_effects_v60 ORDER BY segment_id")
        .map_err(|error| format!("restore target effective flag projection is unreadable: {error}"))?;
    let actual_active_flags = actual_flag_statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
        .map_err(|error| format!("restore target effective flag projection is unreadable: {error}"))?
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
        .map_err(|error| format!("restore target effective flag projection is unreadable: {error}"))?;
    drop(actual_flag_statement);
    if actual_active_flags != expected_active_flags {
        return Err(
            "database restore refused: effective review-flag projection does not select the latest active effect"
                .to_string(),
        );
    }

    let mut legacy_reviewed_statement = db
        .connection()
        .prepare(
            "SELECT id, review_revision, human_decision, verdict, verdict_transcript,
                    annotated_transcript, verified, reviewed_by, corrected_at, escalated,
                    is_gold, rationale
               FROM legacy_reviewed_segments_v60 ORDER BY id",
        )
        .map_err(|error| format!("restore target legacy reviewed-segment authority is unreadable: {error}"))?;
    let legacy_reviewed_segments = legacy_reviewed_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                LegacyReviewedState {
                    review_revision: row.get(1)?,
                    human_decision: row.get(2)?,
                    verdict: row.get(3)?,
                    verdict_transcript: row.get(4)?,
                    annotated_transcript: row.get(5)?,
                    verified: row.get(6)?,
                    reviewed_by: row.get(7)?,
                    corrected_at: row.get(8)?,
                    escalated: row.get(9)?,
                    is_gold: row.get(10)?,
                    rationale: row.get(11)?,
                },
            ))
        })
        .map_err(|error| format!("restore target legacy reviewed-segment authority is unreadable: {error}"))?
        .collect::<Result<std::collections::HashMap<_, _>, _>>()
        .map_err(|error| format!("restore target legacy reviewed-segment authority is unreadable: {error}"))?;
    drop(legacy_reviewed_statement);

    let mut mutations_by_segment = std::collections::BTreeMap::<String, Vec<ReviewMutation>>::new();
    for effect in effects.iter().cloned() {
        mutations_by_segment
            .entry(effect.segment_id.clone())
            .or_default()
            .push(ReviewMutation::Decision(Box::new(effect)));
    }
    for flag in flags.iter().cloned() {
        mutations_by_segment.entry(flag.segment_id.clone()).or_default().push(ReviewMutation::Flag(flag));
    }

    for (segment_id, mutations) in &mut mutations_by_segment {
        mutations.sort_by_key(|mutation| (mutation.applied_revision(), mutation.prior_revision()));
        let first = mutations
            .first()
            .ok_or_else(|| format!("database restore refused: empty review-effect chain for {segment_id}"))?;

        // Flags deliberately do not copy or mutate the human transcript/verification fields.  Bind
        // those untouched fields to the first decision's immutable prior snapshot (when one follows
        // the flag), otherwise to the retained row, then replay every later decision across any
        // intervening flags.  This prevents a forged first flag from laundering an unbound verified
        // annotation merely because the exhaustive scan sees that an effect names the segment.
        let first_decision = mutations.iter().find_map(|mutation| match mutation {
            ReviewMutation::Decision(effect) => Some(effect),
            ReviewMutation::Flag(_) => None,
        });
        let (baseline_human_state, current_is_gold): (StableHumanState, i64) = if let Some(effect) = first_decision {
            let is_gold = db
                .connection()
                .query_row("SELECT is_gold FROM speech_segments WHERE id = ?1", [segment_id], |row| row.get(0))
                .optional()
                .map_err(|error| format!("restore target review baseline is unreadable: {error}"))?
                .ok_or_else(|| format!("database restore refused: review-effect segment {segment_id} is missing"))?;
            (decision_prior_stable_state(effect), is_gold)
        } else {
            db.connection()
                .query_row(
                    "SELECT verified, annotated_transcript, verdict_transcript,
                            human_decision, corrected_at, reviewed_by, is_gold
                       FROM speech_segments WHERE id = ?1",
                    [segment_id],
                    |row| {
                        Ok((
                            StableHumanState {
                                verified: row.get(0)?,
                                annotated_transcript: row.get(1)?,
                                verdict_transcript: row.get(2)?,
                                human_decision: row.get(3)?,
                                corrected_at: row.get(4)?,
                                reviewed_by: row.get(5)?,
                            },
                            row.get(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| format!("restore target review baseline is unreadable: {error}"))?
                .ok_or_else(|| format!("database restore refused: review-effect segment {segment_id} is missing"))?
        };
        if let Some(legacy) = legacy_reviewed_segments.get(segment_id) {
            let baseline_matches = match first {
                ReviewMutation::Decision(effect) => {
                    effect.prior_revision >= legacy.review_revision
                        && effect.prior_verified == legacy.verified
                        && effect.prior_annotated_transcript == legacy.annotated_transcript
                        && effect.prior_verdict == legacy.verdict
                        && effect.prior_verdict_transcript == legacy.verdict_transcript
                        && effect.prior_rationale == legacy.rationale
                        && effect.prior_escalated == legacy.escalated
                        && effect.prior_human_decision == legacy.human_decision
                        && effect.prior_corrected_at == legacy.corrected_at
                        && effect.prior_reviewed_by == legacy.reviewed_by
                }
                ReviewMutation::Flag(flag) => {
                    flag.prior_revision >= legacy.review_revision
                        && flag.prior_verdict == legacy.verdict
                        && flag.prior_rationale == legacy.rationale
                        && flag.prior_escalated == legacy.escalated
                }
            } && baseline_human_state.verified == legacy.verified
                && baseline_human_state.annotated_transcript == legacy.annotated_transcript
                && baseline_human_state.verdict_transcript == legacy.verdict_transcript
                && baseline_human_state.human_decision == legacy.human_decision
                && baseline_human_state.corrected_at == legacy.corrected_at
                && baseline_human_state.reviewed_by == legacy.reviewed_by
                && current_is_gold == legacy.is_gold;
            if !baseline_matches {
                return Err(format!(
                    "database restore refused: review-effect chain for segment {segment_id} does not start from its immutable pre-v60 reviewed state"
                ));
            }
        } else {
            let unbound_human_prior = baseline_human_state.verified != 0
                || !optional_text_is_blank(baseline_human_state.annotated_transcript.as_deref())
                || !optional_text_is_blank(baseline_human_state.human_decision.as_deref())
                || !optional_text_is_blank(baseline_human_state.reviewed_by.as_deref())
                || !optional_text_is_blank(baseline_human_state.corrected_at.as_deref())
                || current_is_gold != 0;
            let unbound_flag_prior = match first {
                ReviewMutation::Flag(flag) => {
                    flag.prior_escalated != 0
                        || flag
                            .prior_verdict
                            .as_deref()
                            .is_some_and(|value| value.starts_with("human_") || value == "escalated")
                }
                ReviewMutation::Decision(effect) => effect
                    .prior_verdict
                    .as_deref()
                    .is_some_and(|value| value.starts_with("human_") || value == "escalated"),
            };
            if unbound_human_prior || unbound_flag_prior {
                return Err(format!(
                    "database restore refused: review-effect chain for segment {segment_id} starts from unsnapshotted human review truth"
                ));
            }
        }

        let mut expected_stable_human_state = baseline_human_state;
        let mut expected_rationale = match first {
            ReviewMutation::Decision(effect) => effect.prior_rationale.clone(),
            ReviewMutation::Flag(effect) => effect.prior_rationale.clone(),
        };
        for mutation in mutations.iter() {
            match mutation {
                ReviewMutation::Decision(effect) => {
                    if decision_prior_stable_state(effect) != expected_stable_human_state {
                        return Err(format!(
                            "database restore refused: review effect chain for segment {segment_id} changes human transcript/verification fields across a flag without authority"
                        ));
                    }
                    if effect.prior_rationale != expected_rationale
                        || effect.decision_rationale != effect.prior_rationale
                    {
                        return Err(format!(
                            "database restore refused: review effect chain for segment {segment_id} changes rationale across a human decision"
                        ));
                    }
                    expected_stable_human_state = decision_terminal_stable_state(effect);
                    expected_rationale = effect.decision_rationale.clone();
                }
                ReviewMutation::Flag(effect) => {
                    if effect.prior_rationale != expected_rationale {
                        return Err(format!(
                            "database restore refused: review effect chain for segment {segment_id} has a forged flag rationale prior-state"
                        ));
                    }
                    expected_rationale = flag_terminal_state(effect).rationale;
                }
            }
        }
        for pair in mutations.windows(2) {
            if pair[1].applied_revision() <= pair[0].applied_revision()
                || pair[1].prior_revision() < pair[0].terminal_revision()
            {
                return Err(format!(
                    "database restore refused: review effects for segment {segment_id} overlap or reverse a shadowed mutation"
                ));
            }
            let prior_snapshot_continuous = match (&pair[0], &pair[1]) {
                (ReviewMutation::Decision(previous), ReviewMutation::Decision(next)) => {
                    decision_terminal_state(previous)
                        == DecisionOwnedState {
                            verified: next.prior_verified,
                            annotated_transcript: next.prior_annotated_transcript.clone(),
                            verdict: next.prior_verdict.clone(),
                            verdict_transcript: next.prior_verdict_transcript.clone(),
                            escalated: next.prior_escalated,
                            human_decision: next.prior_human_decision.clone(),
                            corrected_at: next.prior_corrected_at.clone(),
                            reviewed_by: next.prior_reviewed_by.clone(),
                        }
                }
                (ReviewMutation::Flag(previous), ReviewMutation::Flag(next)) => {
                    flag_terminal_state(previous)
                        == FlagOwnedState {
                            verdict: next.prior_verdict.clone(),
                            rationale: next.prior_rationale.clone(),
                            escalated: next.prior_escalated,
                        }
                }
                (ReviewMutation::Decision(previous), ReviewMutation::Flag(next)) => {
                    let terminal = decision_terminal_state(previous);
                    terminal.verdict == next.prior_verdict && terminal.escalated == next.prior_escalated
                }
                (ReviewMutation::Flag(previous), ReviewMutation::Decision(next)) => {
                    let terminal = flag_terminal_state(previous);
                    terminal.verdict == next.prior_verdict && terminal.escalated == next.prior_escalated
                }
            };
            if !prior_snapshot_continuous {
                return Err(format!(
                    "database restore refused: review effect chain for segment {segment_id} has a forged or discontinuous prior snapshot"
                ));
            }
        }

        let Some(latest) = mutations.last() else {
            continue;
        };
        debug_assert_eq!(latest.segment_id(), segment_id);
        let current: Option<CurrentReviewState> = db
            .connection()
            .query_row(
                "SELECT review_revision, verified, annotated_transcript, verdict,
                        verdict_transcript, escalated, human_decision, corrected_at,
                        reviewed_by, rationale
                   FROM speech_segments WHERE id = ?1",
                [segment_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("restore target current review-effect state is unreadable: {error}"))?;
        let Some((
            current_revision,
            current_verified,
            current_annotated,
            current_verdict,
            current_verdict_transcript,
            current_escalated,
            current_human_decision,
            current_corrected_at,
            current_reviewed_by,
            current_rationale,
        )) = current
        else {
            return Err(format!(
                "database restore refused: reviewed segment {segment_id} is missing while its immutable schema-v60 effect history remains"
            ));
        };
        if current_revision < latest.terminal_revision() {
            return Err(format!(
                "database restore refused: segment {segment_id} predates its latest review-effect revision"
            ));
        }
        let current_stable_human_state = StableHumanState {
            verified: current_verified,
            annotated_transcript: current_annotated.clone(),
            verdict_transcript: current_verdict_transcript.clone(),
            human_decision: current_human_decision.clone(),
            corrected_at: current_corrected_at.clone(),
            reviewed_by: current_reviewed_by.clone(),
        };
        if current_stable_human_state != expected_stable_human_state {
            return Err(format!(
                "database restore refused: segment {segment_id} has unbound human transcript/verification state outside its exact review-effect chain"
            ));
        }
        if current_rationale != expected_rationale {
            return Err(format!(
                "database restore refused: segment {segment_id} rationale disagrees with its exact mixed decision/flag effect chain"
            ));
        }

        match latest {
            ReviewMutation::Decision(effect) if effect.reversal_operation.is_none() => {
                let expected_verdict = format!("human_{}", effect.action);
                let expected_verdict_transcript = if effect.action == "reject" {
                    effect.prior_verdict_transcript.as_ref()
                } else {
                    effect.decision_transcript.as_ref()
                };
                if current_revision < effect.decision_revision
                    || current_human_decision.as_deref() != Some(effect.action.as_str())
                    || current_verdict.as_deref() != Some(expected_verdict.as_str())
                    || current_escalated != 0
                    || current_verified != effect.decision_verified
                    || current_annotated != effect.decision_annotated_transcript
                    || current_verdict_transcript.as_ref() != expected_verdict_transcript
                    || current_corrected_at.as_deref() != Some(effect.decision_corrected_at.as_str())
                    || current_reviewed_by != effect.reviewer
                {
                    return Err(format!(
                        "database restore refused: segment {segment_id} disagrees with its latest active human-decision effect {}",
                        effect.id
                    ));
                }
            }
            ReviewMutation::Decision(effect) => {
                let exact_inverse_revision = effect.decision_revision + 1;
                let exact_snapshot = current_verified == effect.prior_verified
                    && current_annotated == effect.prior_annotated_transcript
                    && current_verdict == effect.prior_verdict
                    && current_verdict_transcript == effect.prior_verdict_transcript
                    && current_escalated == effect.prior_escalated
                    && current_human_decision == effect.prior_human_decision
                    && current_corrected_at == effect.prior_corrected_at
                    && current_reviewed_by == effect.prior_reviewed_by;
                if current_revision < exact_inverse_revision || !exact_snapshot {
                    return Err(format!(
                        "database restore refused: segment {segment_id} does not reflect human-decision reversal {}",
                        effect.id
                    ));
                }
            }
            ReviewMutation::Flag(flag) if flag.reversal_operation.is_none() => {
                if current_revision < flag.flag_revision
                    || current_verdict.as_deref() != Some("escalated")
                    || current_escalated != 1
                    || current_human_decision.as_deref().is_some_and(|value| !value.trim().is_empty())
                    || current_rationale.as_deref() != Some(flag.flag_rationale.as_str())
                {
                    return Err(format!(
                        "database restore refused: segment {segment_id} disagrees with its latest active review-flag effect {}",
                        flag.id
                    ));
                }
            }
            ReviewMutation::Flag(flag) => {
                let exact_inverse_revision = flag.flag_revision + 1;
                let exact_snapshot = current_verdict == flag.prior_verdict
                    && current_rationale == flag.prior_rationale
                    && current_escalated == flag.prior_escalated
                    && optional_text_is_blank(current_human_decision.as_deref());
                if current_revision < exact_inverse_revision || !exact_snapshot {
                    return Err(format!(
                        "database restore refused: segment {segment_id} does not reflect review-flag reversal {}",
                        flag.id
                    ));
                }
            }
        }
    }

    // Exhaustive current-row coverage closes the renderer/staged-file bypass: every row that can
    // presently export or advertise human-reviewed truth must be explained either by the immutable
    // pre-v60 snapshot or by the validated schema-v60 mutation chain above. A target-added row is
    // not legitimate merely because no effect happens to name it.
    let mut current_reviewed_statement = db
        .connection()
        .prepare(
            "SELECT segment.id, segment.review_revision, segment.human_decision,
                    segment.verdict, segment.verdict_transcript, segment.annotated_transcript,
                    segment.verified, segment.reviewed_by, segment.corrected_at,
                    segment.escalated, segment.is_gold, segment.rationale
               FROM speech_segments segment
              WHERE segment.verified = 1
                 OR segment.is_gold = 1
                 OR segment.human_decision IS NOT NULL
                 OR segment.reviewed_by IS NOT NULL
                 OR segment.corrected_at IS NOT NULL
                 OR segment.escalated = 1
                 OR segment.verdict = 'escalated'
                 OR segment.verdict LIKE 'human_%'
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
              ORDER BY segment.id",
        )
        .map_err(|error| format!("restore target current reviewed-row authority is unreadable: {error}"))?;
    let current_reviewed_rows = current_reviewed_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                LegacyReviewedState {
                    review_revision: row.get(1)?,
                    human_decision: row.get(2)?,
                    verdict: row.get(3)?,
                    verdict_transcript: row.get(4)?,
                    annotated_transcript: row.get(5)?,
                    verified: row.get(6)?,
                    reviewed_by: row.get(7)?,
                    corrected_at: row.get(8)?,
                    escalated: row.get(9)?,
                    is_gold: row.get(10)?,
                    rationale: row.get(11)?,
                },
            ))
        })
        .map_err(|error| format!("restore target current reviewed-row authority is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target current reviewed-row authority is unreadable: {error}"))?;
    drop(current_reviewed_statement);
    for (segment_id, current) in current_reviewed_rows {
        if mutations_by_segment.contains_key(&segment_id) {
            continue;
        }
        let Some(legacy) = legacy_reviewed_segments.get(&segment_id) else {
            return Err(format!(
                "database restore refused: current reviewed segment {segment_id} has neither immutable legacy authority nor a schema-v60 effect chain"
            ));
        };
        let exact_legacy_terminal = current.review_revision >= legacy.review_revision
            && current.human_decision == legacy.human_decision
            && current.verdict == legacy.verdict
            && current.verdict_transcript == legacy.verdict_transcript
            && current.annotated_transcript == legacy.annotated_transcript
            && current.verified == legacy.verified
            && current.reviewed_by == legacy.reviewed_by
            && current.corrected_at == legacy.corrected_at
            && current.escalated == legacy.escalated
            && current.is_gold == legacy.is_gold
            && current.rationale == legacy.rationale;
        if !exact_legacy_terminal {
            return Err(format!(
                "database restore refused: current reviewed segment {segment_id} disagrees with its immutable pre-v60 terminal state"
            ));
        }
    }

    Ok(())
}
