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
        /// Which desktop request contract wrote this row: Some(1) for the typed
        /// `commit_review_v1` path, None for the retired legacy command. The two use DIFFERENT
        /// payload-hash domains, so the digest cannot be checked without knowing which.
        desktop_review_contract_version: Option<i64>,
        /// The policy-4 authority the typed desktop contract hashes into its payload digest.
        playback_authority_session_id: Option<String>,
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
                    effect.prior_reviewed_by, reversal.operation_id,
                    effect.desktop_review_contract_version, effect.playback_authority_session_id
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
                desktop_review_contract_version: row.get(29)?,
                playback_authority_session_id: row.get(30)?,
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
                        // DISPATCH ON THE CONTRACT THAT WROTE THE ROW. The typed desktop path
                        // (`commit_review_v1`) hashes with `desktop_review_v1_payload_hash` over
                        // (segment_id, base_revision, decision, corrected, authority_session_id)
                        // under the domain prefix "cortex-desktop-review-ipc-v1\0"; the retired
                        // legacy command used `desktop_decision_payload_hash` under
                        // "cortex-desktop-human-decision-v1\0". Different prefixes cannot produce
                        // equal digests, so recomputing only the legacy formula -- as this did --
                        // refused EVERY production desktop decision, and with the legacy command
                        // retired ("no production write path") that is all of them. Any restore of
                        // a database containing one desktop review was refused. `db/core.rs` and
                        // `db/review.rs` already dispatch on this column; this pass did not even
                        // SELECT it. Legacy rows keep the old formula, so nothing is loosened.
                        && match (effect.desktop_review_contract_version, effect.playback_authority_session_id.as_deref())
                        {
                            (Some(1), Some(authority_session_id)) => {
                                crate::db::desktop_review_v1_payload_hash(
                                    &effect.segment_id,
                                    effect.prior_revision,
                                    requested_action,
                                    effect.requested_transcript.as_deref(),
                                    authority_session_id,
                                ) == payload_hash
                            }
                            // v1 without its authority is not a v1 row: the writer requires one
                            // (finalization.rs), so its absence is corruption, not a legacy row.
                            (Some(_), _) => false,
                            (None, _) => {
                                crate::db::desktop_decision_payload_hash(
                                    &effect.segment_id,
                                    requested_action,
                                    effect.requested_transcript.as_deref(),
                                    Some(timestamp_ms),
                                ) == payload_hash
                            }
                        }
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
                          -- ?2 is the UNDO's operation id, taken from
                          -- human_decision_effect_reversals.operation_id. It is bound here to the
                          -- INVERSE ledger entry (`entry_key = 'undo:' || ?2`), which is where it
                          -- belongs. It must NOT be compared against event.operation_id: that
                          -- column holds the DECISION's operation, a different value by
                          -- construction, so `event.operation_id = ?2` could never be true and this
                          -- query refused every genuine phone undo -- state that couch's own
                          -- api_undo produces. The binding stays complete without it: event.id
                          -- pins the event, original.review_event_id pins that event's one
                          -- un-reversed pay entry, reversal.reverses_entry_id pins the inverse to
                          -- that exact entry, and the field equalities below prove the inverse is
                          -- an exact negation.
                          WHERE event.id = ?1
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
                  -- The same impossible comparison as the effect-side query above, in its second
                  -- home: effect_reversal.operation_id is the UNDO's operation, event.operation_id
                  -- is the DECISION's, so `event.operation_id = effect_reversal.operation_id` never
                  -- held and every genuine phone undo was refused here too. The entry_key join
                  -- already binds the inverse to its undo operation; the review_events join is kept
                  -- because it still requires the effect's event to exist.
                  WHERE reversal.id = ?1
                    AND reversal.entry_key = 'undo:' || effect_reversal.operation_id",
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

#[cfg(test)]
mod tests {
    use super::validate_review_effect_semantics;
    use crate::db::Database;

    // This module's own fixtures rather than the ones in commands.rs: those live in another file's
    // test module (unimportable), and that file is under active refactor elsewhere, so growing it
    // would only manufacture merge conflicts. Everything here is deliberately minimal — one paid
    // clip, one real decision — because each test's job is to corrupt exactly ONE thing.

    fn canonical_operation(index: u64) -> String {
        format!("00000000-0000-4000-8000-{index:012x}")
    }

    /// A segment carrying the canonical pay evidence: content hash, fingerprint, source span.
    fn paid_segment(db: &Database, id: &str) {
        db.insert_segment(&crate::db::SpeechSegment {
            id: id.to_string(),
            audio_path: format!("{id}.wav"),
            raw_transcript: "machine draft".to_string(),
            duration_ms: 1_000,
            confidence: Some(0.99),
            ..crate::db::SpeechSegment::default()
        })
        .unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash = ?2,
                        audio_fingerprint = ?3,
                        alignment_json = '{\"source_start_ms\":0,\"source_end_ms\":1000}',
                        duration_ms = 1000
                  WHERE id = ?1",
                rusqlite::params![id, "a".repeat(64), 424_242_i64],
            )
            .unwrap();
    }

    fn seeded_db(id: &str) -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        paid_segment(&db, id);
        db
    }

    /// Record a real Couch review event through the production API.
    fn couch_event(db: &Database, id: &str, action: &str, index: u64) {
        db.record_review_event_with_operation(
            id,
            "Reviewer",
            action,
            "couch",
            i64::try_from(index).unwrap(),
            &canonical_operation(index),
            &crate::db::review_operation_payload_hash(id, action, "", "Reviewer"),
        )
        .unwrap();
    }

    #[test]
    fn a_clean_restore_target_passes_and_the_frontier_row_is_singular() {
        // The baseline: without this, every refusal below could be passing for the wrong reason.
        let db = seeded_db("clean-clip");
        validate_review_effect_semantics(&db).expect("a freshly initialized database is a valid restore target");

        couch_event(&db, "clean-clip", "skip", 401);
        validate_review_effect_semantics(&db).expect("a skip creates no effect and stays valid");

        // review_effect_state is the schema-v60 frontier and must be exactly one row. The FIRST
        // version of this test tried to insert a second row and asserted only `if rows > 1` — but
        // the column is `singleton_key INTEGER PRIMARY KEY CHECK(singleton_key = 1)`, so a second
        // row is impossible and the assertion never ran. A conditional assertion that cannot fire
        // is not a test. The reachable violation is ZERO rows: a restored file whose frontier was
        // dropped has no cutoff at all, and every pre-v60 row would read as effective.
        db.connection().execute("DROP TRIGGER review_effect_state_immutable_delete", []).unwrap();
        assert_eq!(db.connection().execute("DELETE FROM review_effect_state", []).unwrap(), 1);
        let error = validate_review_effect_semantics(&db).unwrap_err();
        assert!(error.contains("one canonical schema-v60 frontier row"), "{error}");
    }

    #[test]
    fn a_forged_effect_on_a_non_decision_event_is_refused() {
        // A skip is not a decision: it pays nothing and must leave no human-decision effect. Forging
        // one would invent paid, reviewed truth for a clip nobody judged — so the trigger is dropped
        // first (a restored file may already contain rows written with triggers disabled, which is
        // exactly the case this whole validation pass exists for).
        let db = seeded_db("forged-clip");
        couch_event(&db, "forged-clip", "skip", 402);
        validate_review_effect_semantics(&db).unwrap();

        let event_id: i64 =
            db.connection().query_row("SELECT MAX(id) FROM review_events", [], |row| row.get(0)).unwrap();
        let prior_revision = db.segment_review_revision("forged-clip").unwrap().unwrap();
        db.connection().execute("DROP TRIGGER human_decision_effect_events_validate_review_event_insert", []).unwrap();
        db.connection()
            .execute(
                "INSERT INTO human_decision_effect_events
                    (review_event_id, segment_id, reviewer, source, action,
                     served_transcript, decision_transcript, decision_annotated_transcript,
                     decision_verified, decision_corrected_at,
                     prior_revision, decision_revision, prior_verified, prior_escalated)
                 VALUES (?1, 'forged-clip', 'Reviewer', 'couch', 'edit',
                         'served transcript', 'forged edit', 'forged edit', 1,
                         '2026-08-29 00:00:00', ?2, ?2 + 1, 0, 0)",
                rusqlite::params![event_id, prior_revision],
            )
            .unwrap();
        let error = validate_review_effect_semantics(&db).unwrap_err();
        assert!(error.contains("must not create a human-decision effect"), "{error}");
    }

    #[test]
    fn a_decision_event_stripped_of_its_pay_effect_is_refused() {
        // The mirror of the forgery above, and the one that costs a reviewer money: a real Couch
        // decision whose human/pay effect is missing from the restored file. The work happened; the
        // evidence that it should be paid did not survive. Publishing that silently is the failure.
        let db = seeded_db("stripped-clip");
        let revision = db.segment_review_revision("stripped-clip").unwrap().unwrap();
        db.record_phone_human_decision_by_at_revision_with_operation(
            "stripped-clip",
            "accept",
            Some("machine draft"),
            "Reviewer",
            revision,
            &canonical_operation(403),
            &crate::db::review_operation_payload_hash("stripped-clip", "accept", "machine draft", "Reviewer"),
        )
        .unwrap()
        .unwrap();
        validate_review_effect_semantics(&db).expect("a genuine phone decision is a valid restore target");

        db.connection().execute("DROP TRIGGER IF EXISTS human_decision_effect_events_immutable_delete", []).ok();
        let removed = db.connection().execute("DELETE FROM human_decision_effect_events", []).unwrap();
        assert_eq!(removed, 1, "the fixture must have created exactly one pay effect to remove");
        let error = validate_review_effect_semantics(&db).unwrap_err();
        assert!(
            error.contains("does not have exactly one matching human/pay effect"),
            "a decision without its pay effect must be refused: {error}"
        );
    }

    /// A real phone decision, returning its effect id. The effect rows are trigger-protected, so
    /// every corruption below drops the guard first — a restored file can already contain rows
    /// written with triggers disabled, which is the situation this whole pass exists for.
    fn decided(db: &Database, id: &str, index: u64) -> i64 {
        let revision = db.segment_review_revision(id).unwrap().unwrap();
        db.record_phone_human_decision_by_at_revision_with_operation(
            id,
            "edit",
            Some("corrected text"),
            "Reviewer",
            revision,
            &canonical_operation(index),
            &crate::db::review_operation_payload_hash(id, "edit", "corrected text", "Reviewer"),
        )
        .unwrap()
        .unwrap();
        db.connection().query_row("SELECT MAX(id) FROM human_decision_effect_events", [], |row| row.get(0)).unwrap()
    }

    /// Put the connection into the state a RESTORED FILE can genuinely arrive in: triggers dropped,
    /// CHECK constraints ignored, foreign keys off. This is not a way around the schema — it is the
    /// threat model. Every corruption below is refused by a CHECK or FK on a normally-configured
    /// connection (measured: `decision_revision = prior_revision + 1`, the accept/edit transcript
    /// CHECK, and the review_event_id FK all bite first), so those constraints ARE the first line of
    /// defence and are worth knowing about. The validator is the second line, and it is the only one
    /// that still applies to bytes written elsewhere with the guards disabled — which is exactly why
    /// `validate_review_effect_semantics` exists and why these arms must be tested.
    fn unlock_effects(db: &Database) {
        for trigger in [
            "human_decision_effect_events_immutable_update",
            "human_decision_effect_events_validate_review_event_insert",
        ] {
            db.connection().execute(&format!("DROP TRIGGER IF EXISTS {trigger}"), []).ok();
        }
        db.connection().execute_batch("PRAGMA ignore_check_constraints = ON; PRAGMA foreign_keys = OFF;").unwrap();
    }

    #[test]
    fn an_effect_that_breaks_its_revision_or_identity_boundary_is_refused() {
        // decision_revision must be exactly prior_revision + 1. Anything else means the effect
        // claims a place in the revision chain it did not earn, which is how a forged decision
        // slips ahead of, or on top of, a real one.
        // Each corruption is pinned to the message it ACTUALLY produces, not to the one it seems
        // like it should. A revision-shifted effect is caught earlier, by the event/effect pay
        // match: once the revision moves, the effect no longer answers for its event. That refusal
        // is correct and is the one worth pinning; asserting the later message would have meant
        // loosening the test until it passed.
        let corruptions: [(&str, &str, &str); 4] = [
            (
                "revision skips ahead",
                "UPDATE human_decision_effect_events SET decision_revision = prior_revision + 2",
                "does not have exactly one matching human/pay effect",
            ),
            (
                "revision regresses",
                "UPDATE human_decision_effect_events SET decision_revision = prior_revision",
                "does not have exactly one matching human/pay effect",
            ),
            (
                // Also caught by the pay match rather than the identity check: the effect's action
                // must answer its event's action, so retyping it as an unpaid `skip` breaks the
                // pairing before the identity clause is ever reached.
                "non-decision action",
                "UPDATE human_decision_effect_events SET action = 'skip'",
                "does not have exactly one matching human/pay effect",
            ),
            (
                "blank decision timestamp",
                "UPDATE human_decision_effect_events SET decision_corrected_at = '  '",
                "violates its immutable identity/revision boundary",
            ),
        ];
        for (label, sabotage, expected) in corruptions {
            let db = seeded_db("identity-clip");
            decided(&db, "identity-clip", 420);
            validate_review_effect_semantics(&db).expect("the genuine decision must validate first");
            unlock_effects(&db);
            db.connection().execute(sabotage, []).unwrap();
            let error = validate_review_effect_semantics(&db).unwrap_err();
            assert!(error.contains(expected), "{label}: expected '{expected}', got: {error}");
        }
    }

    #[test]
    fn an_effect_whose_post_decision_text_is_not_canonical_is_refused() {
        // The decision transcript IS the reviewer's paid output. A blank one, or one that
        // disagrees with the annotated transcript the dataset actually serves, means the row no
        // longer records what the human decided — and `annotated_transcript` is human-only by law.
        let corruptions: [(&str, &str); 3] = [
            (
                "blank decision text",
                "UPDATE human_decision_effect_events SET decision_transcript = '   ', decision_annotated_transcript = '   '",
            ),
            (
                "decision text disagrees with what is served",
                "UPDATE human_decision_effect_events SET decision_annotated_transcript = 'something else'",
            ),
            (
                "untrimmed decision text is not NFC-canonical",
                "UPDATE human_decision_effect_events SET decision_transcript = '  corrected text  ', decision_annotated_transcript = '  corrected text  '",
            ),
        ];
        for (label, sabotage) in corruptions {
            let db = seeded_db("text-clip");
            decided(&db, "text-clip", 421);
            validate_review_effect_semantics(&db).unwrap();
            unlock_effects(&db);
            db.connection().execute(sabotage, []).unwrap();
            let error = validate_review_effect_semantics(&db).unwrap_err();
            assert!(error.contains("no exact canonical post-decision transcript"), "{label}: {error}");
        }
    }

    #[test]
    fn a_phone_effect_pointing_at_a_missing_event_is_refused() {
        // A phone effect names the review event that authorized it. Repointing it at an event that
        // does not exist severs the decision from its provenance while leaving it in the dataset:
        // paid, reviewed-looking truth with nothing behind it.
        let db = seeded_db("orphan-clip");
        decided(&db, "orphan-clip", 422);
        validate_review_effect_semantics(&db).unwrap();
        unlock_effects(&db);
        db.connection().execute("UPDATE human_decision_effect_events SET review_event_id = 999999", []).unwrap();
        let error = validate_review_effect_semantics(&db).unwrap_err();
        assert!(
            error.contains("names no post-v60 review event")
                || error.contains("does not have exactly one matching human/pay effect"),
            "severing an effect from its event must be refused, either as an orphan effect or as an \
             event left without its pay effect: {error}"
        );
    }

    #[test]
    fn a_review_event_without_canonical_provenance_is_refused() {
        // The event carries the build and playback-guard provenance that makes a Couch decision
        // auditable. A restored file whose events lost it cannot prove which build produced them.
        let corruptions: [(&str, &str); 3] = [
            ("unknown playback guard", "UPDATE review_events SET playback_guard_version = 'legacy-v0'"),
            ("truncated build sha", "UPDATE review_events SET app_git_sha = 'abc123'"),
            ("forged operation id", "UPDATE review_events SET operation_id = 'not-a-uuid'"),
        ];
        // Every case must reach a verdict, and the test must PROVE it reached one. The first draft
        // used `continue` when the corrupting write was refused, which would have let all three
        // cases skip and the test pass having asserted nothing -- the same vacuous shape as the
        // frontier test above. Now each case records which layer refused, and the tally is asserted.
        let mut refused_by_schema = 0;
        let mut refused_by_validator = 0;
        for (label, sabotage) in corruptions {
            let db = seeded_db("provenance-clip");
            decided(&db, "provenance-clip", 423);
            validate_review_effect_semantics(&db).unwrap();
            // The REAL trigger names — the first draft guessed
            // ("review_events_immutable_update", "review_events_no_update"), neither of which
            // exists, so every corruption bounced off the untouched guards and the test proved
            // nothing. The tally assertion below is what surfaced that.
            for trigger in [
                "review_events_v60_post_cutoff_immutable_update",
                "review_events_v60_provenance_immutable_update",
                "review_event_operation_immutable_update",
            ] {
                db.connection().execute(&format!("DROP TRIGGER IF EXISTS {trigger}"), []).ok();
            }
            if db.connection().execute(sabotage, []).is_err() {
                refused_by_schema += 1; // an immutability trigger held: also a pass, but a different one
                continue;
            }
            let error = validate_review_effect_semantics(&db).unwrap_err();
            assert!(
                error.contains("lacks canonical Couch/build/playback provenance")
                    || error.contains("does not have exactly one matching human/pay effect"),
                "{label}: {error}"
            );
            refused_by_validator += 1;
        }
        assert_eq!(
            refused_by_schema + refused_by_validator,
            3,
            "every provenance corruption must reach a verdict from some layer"
        );
        assert!(
            refused_by_validator > 0,
            "at least one corruption must survive the schema and be caught by the VALIDATOR, or this \
             test proves only that triggers exist ({refused_by_schema} refused by schema)"
        );
    }

    /// A paid phone decision that was then undone through the production APIs, exactly as
    /// `couch::api_undo` does it. Returns the effect id the reversal hangs off.
    fn decided_then_undone(db: &Database, id: &str, decide_index: u64, undo_index: u64) -> i64 {
        let effect_id = decided(db, id, decide_index);
        // The actor must OWN the effect -- an anonymous caller cannot reverse a reviewer's paid
        // decision -- which is why couch passes Some(reviewer) at couch/decisions.rs.
        assert!(matches!(
            db.undo_human_decision(effect_id, Some("Reviewer"), &canonical_operation(undo_index)).unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));
        effect_id
    }

    #[test]
    fn a_genuine_phone_undo_is_a_valid_restore_target() {
        // REGRESSION. This state -- decide on the phone, then Undo -- was refused outright with
        // "lacks its exact operation-bound compensation inverse", because the query compared the
        // UNDO's operation id against the review event's, which carries the DECISION's. Those
        // differ by construction, so any backup containing an undone phone decision could not be
        // restored. Built here entirely from production APIs: no forgery, no dropped triggers.
        let db = seeded_db("genuine-undo");
        decided_then_undone(&db, "genuine-undo", 430, 431);

        // The evidence the validator must accept, asserted so this test documents the shape.
        let (events, reversals): (i64, i64) = db
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM review_events),
                        (SELECT COUNT(*) FROM human_decision_effect_reversals)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((events, reversals), (1, 1));
        let inverses: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM review_compensation_ledger WHERE reverses_entry_id IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(inverses, 1, "the undo must have produced exactly one compensation inverse");

        validate_review_effect_semantics(&db).expect("a phone decision undone through the production API must restore");
    }

    #[test]
    fn a_forged_or_missing_pay_inverse_is_still_refused_after_the_fix() {
        // The other half of the regression: removing the impossible clause must not have opened the
        // door. Each corruption below breaks the inverse in a way that would let an undo stand
        // while the money it claws back does not move, or moves by the wrong amount.
        let corruptions: [(&str, &str); 4] = [
            ("inverse deleted entirely", "DELETE FROM review_compensation_ledger WHERE reverses_entry_id IS NOT NULL"),
            (
                "inverse keyed to a different undo operation",
                "UPDATE review_compensation_ledger SET entry_key = 'undo:00000000-0000-4000-8000-0000000009ff'
                  WHERE reverses_entry_id IS NOT NULL",
            ),
            (
                "inverse does not negate the amount",
                "UPDATE review_compensation_ledger SET delta_micro_iqd = delta_micro_iqd + 1
                  WHERE reverses_entry_id IS NOT NULL",
            ),
            (
                "inverse pays out instead of clawing back",
                "UPDATE review_compensation_ledger SET entitlement_micro_iqd = 5000000
                  WHERE reverses_entry_id IS NOT NULL",
            ),
        ];
        for (label, sabotage) in corruptions {
            let db = seeded_db("forged-inverse");
            decided_then_undone(&db, "forged-inverse", 432, 433);
            validate_review_effect_semantics(&db).expect("the genuine undo must validate first");

            for trigger in [
                "review_compensation_ledger_immutable_update",
                "review_compensation_ledger_immutable_delete",
                "review_compensation_ledger_append_only_update",
                "review_compensation_ledger_append_only_delete",
            ] {
                db.connection().execute(&format!("DROP TRIGGER IF EXISTS {trigger}"), []).ok();
            }
            db.connection().execute_batch("PRAGMA ignore_check_constraints = ON; PRAGMA foreign_keys = OFF;").unwrap();
            let changed = db.connection().execute(sabotage, []).unwrap_or(0);
            assert!(changed > 0, "{label}: the corruption must actually apply, or this case proves nothing");

            let error = validate_review_effect_semantics(&db).unwrap_err();
            assert!(
                error.contains("lacks its exact operation-bound compensation inverse"),
                "{label}: a broken pay inverse must still be refused: {error}"
            );
        }
    }

    #[test]
    fn an_undo_must_carry_a_canonical_operation_identity() {
        // The undo's operation id is what addresses its pay inverse (`entry_key = 'undo:' || id`).
        // A non-canonical id can address nothing, so the decision reversal would stand while the
        // money it claws back never moves. Only testable now that a genuine undo validates at all.
        let db = seeded_db("undo-identity");
        let effect_id = decided_then_undone(&db, "undo-identity", 440, 441);
        validate_review_effect_semantics(&db).expect("the genuine undo must validate first");

        db.connection().execute("DROP TRIGGER IF EXISTS human_decision_effect_reversals_immutable_update", []).ok();
        db.connection().execute_batch("PRAGMA ignore_check_constraints = ON; PRAGMA foreign_keys = OFF;").unwrap();
        let changed = db
            .connection()
            .execute(
                "UPDATE human_decision_effect_reversals SET operation_id = 'not-a-uuid' WHERE effect_event_id = ?1",
                [effect_id],
            )
            .unwrap();
        assert_eq!(changed, 1, "the corruption must apply, or this test proves nothing");
        let error = validate_review_effect_semantics(&db).unwrap_err();
        assert!(error.contains("has no canonical operation UUID"), "{error}");
    }

    #[test]
    fn pay_clawed_back_while_the_decision_still_stands_is_refused() {
        // The asymmetry that costs a reviewer money. The compensation inverse survives the restore
        // but the decision reversal does not, so the edit reads as ACTIVE and still sits in the
        // dataset while the pay for it was reversed. Publishing that silently is the whole failure.
        let db = seeded_db("half-undo");
        let effect_id = decided_then_undone(&db, "half-undo", 442, 443);
        validate_review_effect_semantics(&db).expect("the genuine undo must validate first");

        db.connection().execute("DROP TRIGGER IF EXISTS human_decision_effect_reversals_immutable_delete", []).ok();
        db.connection().execute_batch("PRAGMA ignore_check_constraints = ON; PRAGMA foreign_keys = OFF;").unwrap();
        let removed = db
            .connection()
            .execute("DELETE FROM human_decision_effect_reversals WHERE effect_event_id = ?1", [effect_id])
            .unwrap();
        assert_eq!(removed, 1, "exactly one decision reversal must be removed");
        let surviving_inverse: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM review_compensation_ledger WHERE reverses_entry_id IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(surviving_inverse, 1, "the pay inverse must remain -- that asymmetry IS the defect under test");

        let error = validate_review_effect_semantics(&db).unwrap_err();
        assert!(
            error.contains("already has a compensation inverse")
                || error.contains("is not owned by one exact human-effect reversal"),
            "an active decision whose pay was reversed must be refused: {error}"
        );
    }

    /// Insert the effect row the TYPED desktop contract produces, exactly as
    /// `finalize_desktop_review_v1_with_playback` + `record_human_decision_by_with_finalize` write
    /// it: source='desktop', review_event_id NULL, reviewer NULL, contract version 1, a policy-4
    /// authority id, and an operation_payload_hash from `desktop_review_v1_payload_hash` over
    /// (segment_id, base_revision, decision, corrected, authority_session_id).
    ///
    /// Inserted directly rather than driven through the command layer because minting a real
    /// policy-4 authority needs the whole playback stack; the SHAPE is what this test is about, and
    /// it is taken from the writer's own code rather than invented.
    fn insert_typed_desktop_effect(db: &Database, id: &str, authority: &str, hash: &str) {
        let prior_revision = db.segment_review_revision(id).unwrap().unwrap();
        db.connection().execute_batch("PRAGMA ignore_check_constraints = ON; PRAGMA foreign_keys = OFF;").unwrap();
        for trigger in [
            "human_decision_effect_events_validate_review_event_insert",
            "human_decision_effect_events_immutable_update",
            // Requires the authority to reference a real policy-4 receipt row. Minting one needs
            // the whole playback stack, and a restored file can arrive without the trigger anyway
            // -- which is the state this validator exists to judge.
            "human_decision_effect_events_v67_policy4_validate_insert",
        ] {
            db.connection().execute(&format!("DROP TRIGGER IF EXISTS {trigger}"), []).ok();
        }
        db.connection()
            .execute(
                "INSERT INTO human_decision_effect_events
                    (review_event_id, segment_id, reviewer, source, action,
                     served_transcript, decision_transcript, decision_annotated_transcript,
                     decision_verified, decision_corrected_at,
                     prior_revision, decision_revision, prior_verified, prior_escalated,
                     operation_id, operation_payload_hash, requested_action, requested_transcript,
                     requested_timestamp_ms, desktop_review_contract_version,
                     playback_authority_session_id)
                 VALUES (NULL, ?1, NULL, 'desktop', 'edit',
                         'machine draft', 'desktop corrected', 'desktop corrected', 1,
                         '2026-08-29 00:00:00', ?2, ?2 + 1, 0, 0,
                         ?3, ?4, 'edit', 'desktop corrected', ?5, 1, ?6)",
                rusqlite::params![
                    id,
                    prior_revision,
                    canonical_operation(450),
                    hash,
                    1_700_000_000_000_i64,
                    authority,
                ],
            )
            .unwrap();
        // A real write advances the segment too; without this the row claims a decision revision
        // the segment never reached and a DIFFERENT guard fires first ("segment ... predates its
        // latest review-effect revision"), which would have hidden whether the digest check works.
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET review_revision = ?2 + 1,
                        verified = 1,
                        human_decision = 'edit',
                        annotated_transcript = 'desktop corrected',
                        verdict = 'human_edit',
                        verdict_transcript = 'desktop corrected',
                        -- Must EQUAL the effect's decision_corrected_at: the segment's stable human
                        -- state is compared field-for-field against what the effect chain implies.
                        corrected_at = '2026-08-29 00:00:00'
                  WHERE id = ?1",
                rusqlite::params![id, prior_revision],
            )
            .unwrap();
    }

    #[test]
    fn a_typed_desktop_review_is_a_valid_restore_target() {
        // REGRESSION. The desktop branch recomputed the RETIRED legacy digest
        // (`desktop_decision_payload_hash`, domain "cortex-desktop-human-decision-v1\0") while the
        // only shipping desktop writer stores `desktop_review_v1_payload_hash` (domain
        // "cortex-desktop-review-ipc-v1\0"). Different domain prefixes cannot collide, so the
        // comparison was false for EVERY production desktop decision and any backup containing one
        // was refused. Mirror image of the phone-undo defect fixed in 22e2ddeb, in the same file.
        let db = seeded_db("typed-desktop");
        let authority = "11111111-2222-4333-8444-555555555555";
        let prior_revision = db.segment_review_revision("typed-desktop").unwrap().unwrap();
        let genuine = crate::db::desktop_review_v1_payload_hash(
            "typed-desktop",
            prior_revision,
            "edit",
            Some("desktop corrected"),
            authority,
        );
        // The two digests must differ, or this test would pass for the wrong reason.
        let legacy = crate::db::desktop_decision_payload_hash(
            "typed-desktop",
            "edit",
            Some("desktop corrected"),
            Some(1_700_000_000_000),
        );
        assert_ne!(genuine, legacy, "the two payload-hash domains must be distinct");

        insert_typed_desktop_effect(&db, "typed-desktop", authority, &genuine);
        validate_review_effect_semantics(&db)
            .expect("a typed desktop review written by the shipping contract must restore");
    }

    #[test]
    fn a_desktop_effect_cannot_cross_or_partially_erase_its_operation_boundary() {
        // An unlinked desktop effect is accepted only as one exact anonymous operation. A restored
        // file must not be able to re-label it as Couch work, attach a reviewer identity, or erase
        // one member of the operation tuple while leaving reviewed-looking dataset truth behind.
        for (label, sabotage) in [
            ("Couch source without a review event", "UPDATE human_decision_effect_events SET source = 'couch'"),
            (
                "named reviewer on anonymous desktop work",
                "UPDATE human_decision_effect_events SET reviewer = 'Reviewer'",
            ),
            ("missing operation id", "UPDATE human_decision_effect_events SET operation_id = NULL"),
            ("missing operation payload hash", "UPDATE human_decision_effect_events SET operation_payload_hash = NULL"),
            ("missing requested action", "UPDATE human_decision_effect_events SET requested_action = NULL"),
            ("missing requested timestamp", "UPDATE human_decision_effect_events SET requested_timestamp_ms = NULL"),
        ] {
            let db = seeded_db("desktop-boundary");
            let authority = "11111111-2222-4333-8444-555555555555";
            let prior_revision = db.segment_review_revision("desktop-boundary").unwrap().unwrap();
            let genuine = crate::db::desktop_review_v1_payload_hash(
                "desktop-boundary",
                prior_revision,
                "edit",
                Some("desktop corrected"),
                authority,
            );
            insert_typed_desktop_effect(&db, "desktop-boundary", authority, &genuine);
            validate_review_effect_semantics(&db).expect("the genuine desktop effect must validate first");

            assert_eq!(
                db.connection().execute(sabotage, []).unwrap(),
                1,
                "{label}: the corruption must apply, or this case proves nothing"
            );
            let error = validate_review_effect_semantics(&db).unwrap_err();
            assert!(
                error.contains("outside the exact anonymous desktop operation boundary"),
                "{label}: expected the exact desktop-boundary refusal, got: {error}"
            );
        }
    }

    #[test]
    fn a_forged_typed_desktop_review_is_still_refused() {
        // The other half: teaching the validator the v1 contract must not let anything through.
        // Each case is a row claiming contract 1 whose digest does not answer for its own contents.
        let authority = "11111111-2222-4333-8444-555555555555";
        let cases: [(&str, fn(&str, i64, &str) -> String); 3] = [
            // The retired legacy digest, which is precisely what the buggy validator expected.
            ("legacy digest on a v1 row", |id, _rev, _auth| {
                crate::db::desktop_decision_payload_hash(id, "edit", Some("desktop corrected"), Some(1_700_000_000_000))
            }),
            // A v1 digest bound to a DIFFERENT playback authority -- replaying another clip's proof.
            ("v1 digest for another authority", |id, rev, _auth| {
                crate::db::desktop_review_v1_payload_hash(
                    id,
                    rev,
                    "edit",
                    Some("desktop corrected"),
                    "99999999-2222-4333-8444-555555555555",
                )
            }),
            // A v1 digest over text the row does not contain -- the transcript was swapped after.
            ("v1 digest over different text", |id, rev, auth| {
                crate::db::desktop_review_v1_payload_hash(id, rev, "edit", Some("something else entirely"), auth)
            }),
        ];
        for (label, forge) in cases {
            let db = seeded_db("forged-desktop");
            let prior_revision = db.segment_review_revision("forged-desktop").unwrap().unwrap();
            let hash = forge("forged-desktop", prior_revision, authority);
            insert_typed_desktop_effect(&db, "forged-desktop", authority, &hash);
            let error = validate_review_effect_semantics(&db).unwrap_err();
            assert!(
                error.contains("outside the exact anonymous desktop operation boundary"),
                "{label}: must still be refused, got: {error}"
            );
        }

        // And a row claiming contract 1 with NO authority is corruption, not a legacy row: the
        // writer requires an authority for v1, so it must not fall back to the legacy formula.
        let db = seeded_db("no-authority");
        let prior_revision = db.segment_review_revision("no-authority").unwrap().unwrap();
        let hash = crate::db::desktop_review_v1_payload_hash(
            "no-authority",
            prior_revision,
            "edit",
            Some("desktop corrected"),
            authority,
        );
        insert_typed_desktop_effect(&db, "no-authority", authority, &hash);
        db.connection()
            .execute("UPDATE human_decision_effect_events SET playback_authority_session_id = NULL", [])
            .unwrap();
        let error = validate_review_effect_semantics(&db).unwrap_err();
        assert!(error.contains("outside the exact anonymous desktop operation boundary"), "{error}");
    }
}
