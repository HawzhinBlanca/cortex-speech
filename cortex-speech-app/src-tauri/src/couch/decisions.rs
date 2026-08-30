use super::*;

/// Hand back the work this reviewer was provisionally leased when the batch is NOT going to be served.
///
/// Only this reviewer's leases, and only if still theirs: another request must not be told the work is
/// held by someone who is being handed an error instead of a queue.
pub(super) fn release_unserved_leases(state: &Mutex<CouchState>, serving: &[String], reviewer: &str) {
    let mut guard = lock_state(state);
    for id in serving {
        if guard.leases.get(id).is_some_and(|(who, _)| who == reviewer) {
            guard.leases.remove(id);
        }
    }
}

/// Text a human must never be able to VERIFY as gold, for every decide path on this server.
///
/// `quality::is_placeholder_transcript` is the DECLARED authority on what a placeholder is, and the
/// three decide guards used to re-implement it as "empty or `[bracketed]`" instead. That copy drifted:
/// the authority also refuses a bare "n/a" / "null" (case-insensitively), and a reviewer typing either
/// one had it accepted and minted as a human-verified transcript, which the export then shipped.
///
/// The bracket test is kept as a strict ADDITION, not a replacement — it still refuses any `[...]`
/// marker a future importer invents before the authority has been taught about it.
pub(super) fn refuses_verification_as_placeholder(text: &str) -> bool {
    let trimmed = text.trim();
    crate::quality::is_placeholder_transcript(trimmed) || (trimmed.starts_with('[') && trimmed.ends_with(']'))
}

/// Whether this reviewer's decision is ALREADY STORED on the row (P1.2).
///
/// A phone on the edge of Wi-Fi drops requests. If the write lands but the response is lost, the page
/// retries — and without this check the retry is recorded as a SECOND human decision: another undo
/// entry, and for an edit another DPO learning pair distilled from a correction the human made once.
/// Treating an identical re-submit as already-done makes retry safe, which is what lets the page
/// retry at all.
///
/// Deliberately narrow. It matches only when the SAME reviewer's stored decision has the SAME
/// outcome; change one character and it is a genuine re-review that must be recorded. Text is
/// NFC-compared because the write path canonicalizes, so a decomposed phone-IME paste would otherwise
/// never match the value it just stored.
///
/// This deliberately does NOT look at `verified`. Current phone decisions finalize atomically, but a
/// row created by an older release can still contain a committed decision without phone finalization.
/// Recognizing that legacy half-row lets replay finish only annotation/verification without minting a
/// duplicate learning pair. Callers pair this with `prev.verified` to distinguish complete repeats.
pub(super) fn is_repeat_of_stored_decision(
    prev: &SpeechSegment,
    reviewer: &str,
    decision: &str,
    text: Option<&str>,
) -> bool {
    if !prev.reviewed_by.as_deref().is_some_and(|stored| same_reviewer(stored, reviewer)) {
        return false;
    }
    match (decision, text) {
        ("reject", _) => prev.human_decision.as_deref() == Some("reject"),
        // A repeat of an accept can arrive classified as either: the first submit may have been an
        // "edit", after which the stored text IS the review text, so the retry re-classifies as
        // "accept". Both are the same human act.
        //
        // Matched against EITHER column holding the human's text. After a complete submit that is
        // `annotated_transcript`; after a half-written one only `verdict_transcript` exists, because
        // the upsert that would have copied it across is exactly the write that failed.
        (_, Some(t)) => {
            let want = crate::db::to_nfc(t);
            matches!(prev.human_decision.as_deref(), Some("accept") | Some("edit"))
                && [prev.annotated_transcript.as_deref(), prev.verdict_transcript.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(|stored| crate::db::to_nfc(stored) == want)
        }
        _ => false,
    }
}

#[derive(serde::Deserialize)]
pub(super) struct DecisionBody {
    /// Client-authored identity for this one human operation. The phone persists it before its first
    /// POST and reuses it byte-for-byte on every retry, including after an app/server restart.
    #[serde(default, rename = "operationId")]
    operation_id: Option<String>,
    id: String,
    /// "accept" | "edit" | "bad" | "skip" (the last writes nothing — see `api_decision`)
    action: String,
    #[serde(default)]
    text: String,
    /// Who the PAGE believed it was when this decision was made.
    ///
    /// Only ever set on a replay out of the phone's outbox, and checked against the cookie identity
    /// rather than trusted. The outbox lives in localStorage, which is per-ORIGIN, not per-reviewer:
    /// two reviewers using the same browser (or one person handed a colleague's link) share it, so a
    /// decision queued while offline by Sara could be flushed under Hemn's cookie and be recorded,
    /// permanently, as Hemn's judgement of a clip he never heard. Attribution is the one thing a
    /// review corpus cannot be wrong about.
    #[serde(default)]
    reviewer: Option<String>,
    /// The row's monotonic database revision as it was when the clip was SERVED. The
    /// queue payload carries it and the page echoes it back, so a decision made against a draft
    /// that a background writer replaced in between is refused rather than recorded — the accept/
    /// edit classification and the DPO pair are only meaningful against the text the reviewer saw.
    /// Required for every verdict. `skip` is the sole exemption because it writes no judgement.
    #[serde(default, rename = "rowVersion")]
    row_version: Option<String>,
    /// Immutable controlled-pilot baseline carried by the queue item.  Old localStorage outboxes
    /// cannot acquire it retroactively, so a pre-activation operation must reload before it can write.
    #[serde(default, rename = "pilotAfterReviewEventId")]
    pilot_after_review_event_id: Option<i64>,
    /// Server-issued, finalized policy-4 interval authority. Required for every new non-skip verdict.
    #[serde(default, rename = "playbackReceiptId")]
    playback_receipt_id: Option<String>,
    /// Legacy policy-3 scalar fields are decoded only so a completed rolling-deploy operation can be
    /// acknowledged before fresh proof checks. They never authorize a new verdict.
    #[serde(default, rename = "heardMs")]
    heard_ms: Option<i64>,
    #[serde(default, rename = "clipDurationMs")]
    clip_duration_ms: Option<i64>,
}

/// Durable identity for an HTTP pool undo. Unlike the legacy bodyless endpoint, this request keeps
/// the exact decision and reversal operation stable across a lost response, renderer reload, and
/// app restart. All three fields are one contract: partial coordinates are refused rather than
/// guessed from the mutable "latest effective" view.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UndoBody {
    #[serde(default, rename = "poolDecisionId")]
    pool_decision_id: Option<String>,
    #[serde(default, rename = "decisionOperationId")]
    decision_operation_id: Option<String>,
    #[serde(default, rename = "reversalOperationId")]
    reversal_operation_id: Option<String>,
}

#[derive(Debug)]
pub(super) struct PoolUndoTarget {
    decision_id: i64,
    decision_operation_id: String,
    reversal_operation_id: String,
}

fn canonical_request_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|parsed| parsed.hyphenated().to_string() == value)
}

fn parse_undo_body(body: &[u8]) -> Result<Option<PoolUndoTarget>, String> {
    let parsed = if body.is_empty() {
        UndoBody::default()
    } else {
        serde_json::from_slice::<UndoBody>(body).map_err(|error| format!("bad json: {error}"))?
    };
    match (parsed.pool_decision_id, parsed.decision_operation_id, parsed.reversal_operation_id) {
        (None, None, None) => Ok(None),
        (Some(decision_id), Some(decision_operation_id), Some(reversal_operation_id)) => {
            let decision_id = decision_id
                .parse::<i64>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| "poolDecisionId must be a positive decimal database identity".to_string())?;
            if !canonical_request_uuid(&decision_operation_id) {
                return Err("decisionOperationId must be a lowercase hyphenated UUID".to_string());
            }
            if !canonical_request_uuid(&reversal_operation_id) {
                return Err("reversalOperationId must be a lowercase hyphenated UUID".to_string());
            }
            if decision_operation_id == reversal_operation_id {
                return Err("the decision and reversal operation IDs must be distinct".to_string());
            }
            Ok(Some(PoolUndoTarget { decision_id, decision_operation_id, reversal_operation_id }))
        }
        _ => {
            Err("pool undo requires poolDecisionId, decisionOperationId, and reversalOperationId together".to_string())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReviewOperationState {
    New,
    ExactReplay,
    Reused,
}

/// Hash the exact financial operation contract, not the server's later provenance classification.
/// In particular, a submitted `edit` that is materially unchanged may be paid as an `accept`, but
/// changing `edit` to `accept` while reusing the UUID is still a different client operation and must
/// be rejected. Length framing makes the tuple unambiguous for arbitrary Unicode transcript text.
pub(super) fn decision_operation_payload_hash(segment_id: &str, action: &str, text: &str, reviewer: &str) -> String {
    crate::db::review_operation_payload_hash(segment_id, action, text, reviewer)
}

/// Match a retried phone operation against the immutable receipt that already committed it.
///
/// Reviewer identity is case-insensitive throughout Couch, but the v1 payload hash deliberately
/// preserved the reviewer's exact spelling.  A roster correction such as `Rubar` -> `rubar` after an
/// app restart must therefore rederive the old digest with the spelling stored in the receipt, not
/// with the current session spelling.  The stored spelling is only hash authority after we prove it
/// denotes the currently authenticated reviewer; a different person, segment, action, or transcript
/// remains a hard UUID-reuse conflict.
pub(super) fn operation_receipt_matches_request(
    stored_payload_hash: &str,
    stored_segment_id: &str,
    stored_reviewer: &str,
    request_segment_id: &str,
    request_action: &str,
    request_text: &str,
    authenticated_reviewer: &str,
) -> bool {
    stored_segment_id == request_segment_id
        && same_reviewer(stored_reviewer, authenticated_reviewer)
        && stored_payload_hash
            == decision_operation_payload_hash(request_segment_id, request_action, request_text, stored_reviewer)
}

pub(super) fn review_operation_state(
    db: &Database,
    operation_id: &str,
    segment_id: &str,
    action: &str,
    text: &str,
    reviewer: &str,
) -> crate::error::AppResult<ReviewOperationState> {
    let Some(receipt) = db.review_operation(operation_id)? else {
        return Ok(ReviewOperationState::New);
    };
    if operation_receipt_matches_request(
        &receipt.operation_payload_hash,
        &receipt.segment_id,
        &receipt.reviewer,
        segment_id,
        action,
        text,
        reviewer,
    ) {
        Ok(ReviewOperationState::ExactReplay)
    } else {
        Ok(ReviewOperationState::Reused)
    }
}

/// A UNIQUE conflict can mean another identical retry committed while this request was in flight.
/// Re-read the immutable receipt before deciding whether a write error is success, conflict, or a
/// genuine retryable failure. This is the post-write half of idempotency; the preflight alone has a
/// check-then-insert race.
pub(super) fn operation_result_after_write_failure(
    db: &Database,
    reviewer: &str,
    parsed: &DecisionBody,
    operation_id: &str,
    fallback_status: u16,
    fallback_message: &str,
) -> Reply {
    match review_operation_state(db, operation_id, &parsed.id, &parsed.action, &parsed.text, reviewer) {
        Ok(ReviewOperationState::ExactReplay) => {
            json_reply_with_accounting(200, serde_json::json!({ "ok": true, "duplicate": true }), db, reviewer)
        }
        Ok(ReviewOperationState::Reused) => err_reply(409, "operation UUID is already bound to another decision"),
        Ok(ReviewOperationState::New) => err_reply(fallback_status, fallback_message),
        Err(error) => err_reply(500, &format!("{fallback_message}; operation receipt lookup failed: {error}")),
    }
}

#[cfg(test)]
pub(super) fn api_independent_decision(
    db: &Database,
    parsed: &DecisionBody,
    reviewer: &str,
    session_binding_sha256: &str,
    state: &Mutex<CouchState>,
    campaign: &crate::review_campaign::SequentialReviewCampaign,
) -> Reply {
    let Some(operation_id) = parsed.operation_id.as_deref() else {
        return err_reply(400, "operationId is required — reload this page before deciding");
    };
    let Ok(uuid) = uuid::Uuid::parse_str(operation_id) else {
        return err_reply(400, "operationId must be a canonical UUID");
    };
    if uuid.hyphenated().to_string() != operation_id {
        return err_reply(400, "operationId must be a lowercase hyphenated UUID");
    }
    let Some(canonical_reviewer) = campaign.authorized_reviewer() else {
        return err_reply(503, "independent review campaign has no active reviewer identity");
    };
    // Persist and hash the immutable campaign roster spelling. Authentication remains
    // case-insensitive, but the schema-61 trigger deliberately compares NEW.reviewer to that exact
    // spelling; hashing the transient cookie spelling would also make a later case-only login unable
    // to replay the durable receipt.
    let operation_payload_hash =
        decision_operation_payload_hash(&parsed.id, &parsed.action, &parsed.text, canonical_reviewer);
    match crate::review_campaign::independent_operation(db, operation_id) {
        Ok(Some(receipt))
            if receipt.campaign_id == campaign.campaign_id
                && operation_receipt_matches_request(
                    &receipt.operation_payload_hash,
                    &receipt.segment_id,
                    &receipt.reviewer,
                    &parsed.id,
                    &parsed.action,
                    &parsed.text,
                    reviewer,
                ) =>
        {
            remember_independent_undo(state, reviewer, operation_id, &receipt.segment_id, receipt.decision_id);
            forget_work_audio_assignment(state, &parsed.id, reviewer);
            return json_reply_with_accounting(
                200,
                serde_json::json!({
                    "ok": true,
                    "duplicate": true,
                    "independentDecisionId": receipt.decision_id,
                }),
                db,
                reviewer,
            );
        }
        Ok(Some(_)) => return err_reply(409, "operation UUID is already bound to another independent decision"),
        Ok(None) => {}
        Err(error) => return err_reply(500, &format!("independent operation lookup failed: {error}")),
    }
    // Operation ids are global human-operation identities even though the two passes use separate
    // tables. Reusing a first-pass UUID in the second-pass namespace must never create two meanings.
    match db.review_operation(operation_id) {
        Ok(Some(_)) => return err_reply(409, "operation UUID is already bound to a first-pass decision"),
        Ok(None) => {}
        Err(error) => return err_reply(500, &format!("operation receipt lookup failed: {error}")),
    }

    let (segment, current_revision) = match db.get_segment_by_id_with_revision(&parsed.id) {
        Ok(Some(row)) => row,
        Ok(None) => return err_reply(404, "no such segment"),
        Err(error) => return err_reply(500, &error.to_string()),
    };
    let Some(row_version) = parsed.row_version.as_deref() else {
        return err_reply(400, "rowVersion is required — reload this clip before deciding");
    };
    let Ok(served_revision) = row_version.parse::<i64>() else {
        return err_reply(400, "rowVersion is invalid — reload this clip before deciding");
    };
    if served_revision != current_revision {
        return err_reply(409, "this clip changed since it was served — reload for the fresh draft");
    }
    if !lock_state(state).served_work.contains(&(parsed.id.clone(), reviewer.to_string())) {
        return err_reply(409, "independent review requires this clip to be served first — reload the queue");
    }
    let current_campaign = match active_campaign_policy(db, reviewer, state) {
        Ok(Some(policy)) => policy,
        Ok(None) => return err_reply(503, "independent review campaign is no longer active"),
        Err(error) => return err_reply(503, &error),
    };
    if &current_campaign != campaign || !current_campaign.is_blinded_second_pass() {
        return err_reply(503, "independent review campaign changed while this decision was being checked");
    }
    let (allowed_dialects, focus) = match reviewer_policy(reviewer, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    if !reviewer_policy_allows(allowed_dialects.as_deref(), focus.as_deref(), &segment) {
        forget_work_audio_assignment(state, &parsed.id, reviewer);
        return err_reply(403, "this clip is outside your current review assignment — reload your queue");
    }
    match crate::review_campaign::independent_segment_pending(db, campaign, &parsed.id) {
        Ok(true) => {}
        Ok(false) => return err_reply(409, "this independent clip already has a decision — reload your queue"),
        Err(error) => return err_reply(503, &error),
    }

    let (action, submitted_transcript): (&str, Option<String>) = match parsed.action.as_str() {
        "skip" => ("skip", None),
        "bad" => ("reject", None),
        "accept" | "edit" => {
            let text = parsed.text.trim().to_string();
            if text.is_empty() {
                return err_reply(400, "empty transcript");
            }
            if refuses_verification_as_placeholder(&text) {
                return err_reply(400, "placeholder transcript cannot be verified");
            }
            let submitted_key = crate::normalizer::learning_text_key(&crate::db::to_nfc(&text));
            let raw_key = crate::normalizer::learning_text_key(&crate::db::to_nfc(segment.raw_transcript.trim()));
            (if submitted_key == raw_key { "accept" } else { "edit" }, Some(text))
        }
        other => return err_reply(400, &format!("unknown action '{other}'")),
    };
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let (content_hash, source_span, playback_proof) = if action == "skip" {
        (None, None, None)
    } else {
        if parsed.heard_ms.is_some() || parsed.clip_duration_ms.is_some() {
            return err_reply(
                428,
                &format!(
                    "E_NO_PLAYBACK_EVIDENCE: {LEGACY_RAW_COUNTER_REFUSAL_MARKER}; legacy playback counters cannot authorize a verdict"
                ),
            );
        }
        let content_hash = match db.segment_audio_content_hash(&parsed.id) {
            Ok(Some(value)) => value,
            Ok(None) => return err_reply(503, "playback identity is unavailable for this clip"),
            Err(error) => return err_reply(500, &format!("playback identity lookup failed: {error}")),
        };
        let source_span = match db.segment_source_span(&parsed.id) {
            Ok(Some(value)) => value,
            Ok(None) => return err_reply(503, "playback source span is unavailable for this clip"),
            Err(error) => return err_reply(500, &format!("playback source-span lookup failed: {error}")),
        };
        let Some(playback_receipt_id) = parsed.playback_receipt_id.as_deref() else {
            return err_reply(428, "E_NO_PLAYBACK_EVIDENCE: finalize this clip's playback attempt before deciding");
        };
        let proof = match db.couch_playback_proof_v4(
            &parsed.id,
            served_revision,
            &content_hash,
            reviewer,
            session_binding_sha256,
            playback_receipt_id,
        ) {
            Ok(Some(proof)) => proof,
            Ok(None) => {
                return err_reply(
                    428,
                    "E_NO_PLAYBACK_EVIDENCE: playback authority does not match this reviewer, session, clip, or revision",
                );
            }
            Err(error) => return playback_error_reply(&error.to_string()),
        };
        (Some(content_hash), Some(source_span), Some(proof))
    };

    {
        let mut guard = lock_state(state);
        if !guard.in_flight_operations.insert(operation_id.to_string()) {
            return err_reply(503, "this operation is still being saved — retrying is safe");
        }
    }
    let input = crate::review_campaign::IndependentDecisionInput {
        segment_id: &parsed.id,
        reviewer: canonical_reviewer,
        action,
        submitted_transcript: submitted_transcript.as_deref(),
        served_transcript: segment.raw_transcript.trim(),
        served_revision,
        audio_content_hash: content_hash.as_deref(),
        playback_authority_session_id: playback_proof.as_ref().and_then(|proof| proof.authority_session_id.as_deref()),
        source_start_ms: source_span.map(|span| span.0),
        source_end_ms: source_span.map(|span| span.1),
        duration_ms: segment.duration_ms,
        requested_action: &parsed.action,
        requested_transcript: &parsed.text,
        operation_id,
        operation_payload_hash: &operation_payload_hash,
        created_at_ms: now_ms,
    };
    let committed = crate::review_campaign::record_independent_decision(db, campaign, &input);
    lock_state(state).in_flight_operations.remove(operation_id);
    let decision_id = match committed {
        Ok(Some(id)) => id,
        Ok(None) => return err_reply(409, "this clip changed while the decision was being saved — reload"),
        Err(error) => match crate::review_campaign::independent_operation(db, operation_id) {
            Ok(Some(receipt))
                if receipt.campaign_id == campaign.campaign_id
                    && operation_receipt_matches_request(
                        &receipt.operation_payload_hash,
                        &receipt.segment_id,
                        &receipt.reviewer,
                        &parsed.id,
                        &parsed.action,
                        &parsed.text,
                        reviewer,
                    ) =>
            {
                receipt.decision_id
            }
            Ok(Some(_)) => return err_reply(409, "operation UUID is already bound to another decision"),
            Ok(None) => return err_reply(500, &error),
            Err(lookup_error) => {
                return err_reply(500, &format!("{error}; operation receipt lookup failed: {lookup_error}"));
            }
        },
    };
    remember_independent_undo(state, reviewer, operation_id, &parsed.id, decision_id);
    forget_work_audio_assignment(state, &parsed.id, reviewer);
    json_reply_with_accounting(
        200,
        serde_json::json!({ "ok": true, "independentDecisionId": decision_id }),
        db,
        reviewer,
    )
}

/// Record a second-or-later pool judgement without mutating the canonical first answer. The queue
/// always serves the raw OmniASR-7B draft in pool mode, keeping this observation independent from the
/// correction already stored on `speech_segments`.
#[cfg(test)]
pub(super) fn api_pool_decision(
    db: &Database,
    parsed: &DecisionBody,
    reviewer: &str,
    state: &Mutex<CouchState>,
    pool: &crate::review_pool::ReviewPool,
) -> Reply {
    let Some(operation_id) = parsed.operation_id.as_deref() else {
        return err_reply(400, "operationId is required — reload this page before deciding");
    };
    let Ok(uuid) = uuid::Uuid::parse_str(operation_id) else {
        return err_reply(400, "operationId must be a canonical UUID");
    };
    if uuid.hyphenated().to_string() != operation_id {
        return err_reply(400, "operationId must be a lowercase hyphenated UUID");
    }
    let operation_payload_hash = decision_operation_payload_hash(&parsed.id, &parsed.action, &parsed.text, reviewer);
    match crate::review_pool::operation(db, operation_id) {
        Ok(Some(receipt))
            if receipt.pool_id == pool.pool_id
                && operation_receipt_matches_request(
                    &receipt.operation_payload_hash,
                    &receipt.segment_id,
                    &receipt.reviewer,
                    &parsed.id,
                    &parsed.action,
                    &parsed.text,
                    reviewer,
                ) =>
        {
            remember_pool_undo(state, reviewer, operation_id, &receipt.segment_id, receipt.decision_id);
            forget_work_audio_assignment(state, &parsed.id, reviewer);
            return json_reply_with_accounting(
                200,
                serde_json::json!({
                    "ok": true,
                    "duplicate": true,
                    "poolDecisionId": receipt.decision_id,
                }),
                db,
                reviewer,
            );
        }
        Ok(Some(_)) => return err_reply(409, "operation UUID is already bound to another pool decision"),
        Ok(None) => {}
        Err(error) => return err_reply(500, &format!("pool operation lookup failed: {error}")),
    }
    // One UUID has one meaning across every review namespace.
    match db.review_operation(operation_id) {
        Ok(Some(_)) => return err_reply(409, "operation UUID is already bound to a canonical decision"),
        Ok(None) => {}
        Err(error) => return err_reply(500, &format!("operation receipt lookup failed: {error}")),
    }
    match crate::review_campaign::independent_operation(db, operation_id) {
        Ok(Some(_)) => return err_reply(409, "operation UUID is already bound to a legacy independent decision"),
        Ok(None) => {}
        Err(error) => return err_reply(500, &format!("legacy operation receipt lookup failed: {error}")),
    }

    let (segment, current_revision) = match db.get_segment_by_id_with_revision(&parsed.id) {
        Ok(Some(row)) => row,
        Ok(None) => return err_reply(404, "no such segment"),
        Err(error) => return err_reply(500, &error.to_string()),
    };
    let Some(row_version) = parsed.row_version.as_deref() else {
        return err_reply(400, "rowVersion is required — reload this clip before deciding");
    };
    let Ok(served_revision) = row_version.parse::<i64>() else {
        return err_reply(400, "rowVersion is invalid — reload this clip before deciding");
    };
    if served_revision != current_revision {
        return err_reply(409, "this clip changed since it was served — reload for the fresh draft");
    }
    if !lock_state(state).served_work.contains(&(parsed.id.clone(), reviewer.to_string())) {
        return err_reply(409, "pool review requires this clip to be served first — reload the queue");
    }
    let current_pool = match active_pool_policy(db, state) {
        Ok(Some(policy)) => policy,
        Ok(None) => return err_reply(503, "review pool is no longer active"),
        Err(error) => return err_reply(503, &error),
    };
    if &current_pool != pool {
        return err_reply(503, "review pool changed while this decision was being checked");
    }
    if !pool.contains(&parsed.id) {
        forget_work_audio_assignment(state, &parsed.id, reviewer);
        return err_reply(403, "this clip is outside the active review pool");
    }
    let (allowed_dialects, focus) = match reviewer_policy(reviewer, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    if !reviewer_policy_allows(allowed_dialects.as_deref(), focus.as_deref(), &segment) {
        forget_work_audio_assignment(state, &parsed.id, reviewer);
        return err_reply(403, "this clip is outside your current review pool — reload your queue");
    }
    match crate::review_pool::reviewer_already_saw(db, &parsed.id, reviewer) {
        Ok(true) => return err_reply(409, "you already reviewed this clip — reload for another one"),
        Ok(false) => {}
        Err(error) => return err_reply(503, &error),
    }

    let (action, submitted_transcript): (&str, Option<String>) = match parsed.action.as_str() {
        "skip" => ("skip", None),
        "bad" => ("reject", None),
        "accept" | "edit" => {
            let text = parsed.text.trim().to_string();
            if text.is_empty() {
                return err_reply(400, "empty transcript");
            }
            if refuses_verification_as_placeholder(&text) {
                return err_reply(400, "placeholder transcript cannot be verified");
            }
            let submitted_key = crate::normalizer::learning_text_key(&crate::db::to_nfc(&text));
            let raw_key = crate::normalizer::learning_text_key(&crate::db::to_nfc(segment.raw_transcript.trim()));
            (if submitted_key == raw_key { "accept" } else { "edit" }, Some(text))
        }
        other => return err_reply(400, &format!("unknown action '{other}'")),
    };
    if parsed.heard_ms.is_some_and(|value| value < 0) || parsed.clip_duration_ms.is_some_and(|value| value < 0) {
        return err_reply(400, "playback counters must not be negative");
    }

    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let (content_hash, source_span) = if action == "skip" {
        (None, None)
    } else {
        let content_hash = match db.segment_audio_content_hash(&parsed.id) {
            Ok(Some(value)) => value,
            Ok(None) => return err_reply(503, "playback identity is unavailable for this clip"),
            Err(error) => return err_reply(500, &format!("playback identity lookup failed: {error}")),
        };
        let source_span = match db.segment_source_span(&parsed.id) {
            Ok(Some(value)) => value,
            Ok(None) => return err_reply(503, "playback source span is unavailable for this clip"),
            Err(error) => return err_reply(500, &format!("playback source-span lookup failed: {error}")),
        };
        let Some(heard_ms) = parsed.heard_ms else {
            return err_reply(428, "E_NO_PLAYBACK_EVIDENCE: listen to the clip before deciding");
        };
        let receipt = crate::db::PlaybackReceipt {
            segment_id: parsed.id.clone(),
            segment_revision: served_revision,
            audio_content_hash: content_hash.clone(),
            reviewer: Some(reviewer.to_string()),
            session_id: None,
            started_at_ms: now_ms,
            played_ms: heard_ms,
            clip_duration_ms: parsed.clip_duration_ms.unwrap_or(0),
            source_start_ms: None,
            source_end_ms: None,
        };
        match db.record_playback_receipt_if_at_revision(&receipt, served_revision) {
            Ok(true) => {}
            Ok(false) => return err_reply(409, "this clip changed since it was served — reload for the fresh draft"),
            Err(error) => return err_reply(500, &format!("playback receipt not recorded: {error}")),
        }
        match db.has_sufficient_playback_evidence(&parsed.id, served_revision, &content_hash, Some(reviewer)) {
            Ok(true) => {}
            Ok(false) => {
                return err_reply(
                    428,
                    &db.require_playback_evidence(&parsed.id, served_revision, &content_hash, Some(reviewer))
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "E_NO_PLAYBACK_EVIDENCE".to_string()),
                );
            }
            Err(error) => return err_reply(500, &format!("playback evidence check failed: {error}")),
        }
        (Some(content_hash), Some(source_span))
    };

    {
        let mut guard = lock_state(state);
        if guard.holder(&parsed.id, Instant::now()).is_some_and(|who| who != reviewer) {
            return err_reply(409, "another reviewer is working on this clip");
        }
        if !guard.in_flight_operations.insert(operation_id.to_string()) {
            return err_reply(503, "this operation is still being saved — retrying is safe");
        }
    }
    let input = crate::review_pool::PoolDecisionInput {
        segment_id: &parsed.id,
        reviewer,
        action,
        submitted_transcript: submitted_transcript.as_deref(),
        served_transcript: segment.raw_transcript.trim(),
        served_revision,
        audio_content_hash: content_hash.as_deref(),
        source_start_ms: source_span.map(|span| span.0),
        source_end_ms: source_span.map(|span| span.1),
        duration_ms: segment.duration_ms,
        requested_action: &parsed.action,
        requested_transcript: &parsed.text,
        operation_id,
        operation_payload_hash: &operation_payload_hash,
        created_at_ms: now_ms,
    };
    let committed = crate::review_pool::record_decision(db, pool, &input);
    lock_state(state).in_flight_operations.remove(operation_id);
    let decision_id = match committed {
        Ok(Some(id)) => id,
        Ok(None) => return err_reply(409, "this clip changed while the decision was being saved — reload"),
        Err(error) => match crate::review_pool::operation(db, operation_id) {
            Ok(Some(receipt))
                if receipt.pool_id == pool.pool_id
                    && operation_receipt_matches_request(
                        &receipt.operation_payload_hash,
                        &receipt.segment_id,
                        &receipt.reviewer,
                        &parsed.id,
                        &parsed.action,
                        &parsed.text,
                        reviewer,
                    ) =>
            {
                receipt.decision_id
            }
            Ok(Some(_)) => return err_reply(409, "operation UUID is already bound to another pool decision"),
            Ok(None) if error.contains("duplicated") || error.contains("independent") => {
                return err_reply(409, "this clip already has your review — reload for another one");
            }
            Ok(None) => return err_reply(500, &error),
            Err(lookup_error) => {
                return err_reply(500, &format!("{error}; operation receipt lookup failed: {lookup_error}"));
            }
        },
    };
    remember_pool_undo(state, reviewer, operation_id, &parsed.id, decision_id);
    forget_work_audio_assignment(state, &parsed.id, reviewer);
    json_reply_with_accounting(200, serde_json::json!({ "ok": true, "poolDecisionId": decision_id }), db, reviewer)
}

/// Record one phone decision through the shared human-decision path. Verdict, provenance/learning
/// effects, annotation, and verification commit atomically; an accepted response can never leave a
/// decided-but-pending row. The decision is attributed to `reviewer`, and refused outright on a clip
/// another reviewer currently holds.
///
/// `action: "skip"` is the one path that writes NOTHING to the corpus — see the block that handles it.
pub(super) fn api_decision_authenticated(
    db: &Database,
    body: &[u8],
    reviewer: &str,
    session_binding_sha256: &str,
    state: &Mutex<CouchState>,
) -> Reply {
    let parsed: DecisionBody = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => return err_reply(400, &format!("bad json: {e}")),
    };
    if crate::validation::input::validate_identifier(&parsed.id).is_err() {
        return err_reply(400, "bad id");
    }
    if crate::validation::input::validate_text(&parsed.text, 100_000, "Transcript").is_err() {
        return err_reply(400, "text too large");
    }
    // ATTRIBUTION FENCE. A queued decision names its author; the cookie names who is asking now. When
    // they disagree the decision belongs to somebody else and must not be written under this name —
    // refuse it and let the page hold it for the reviewer who actually made it. Absent field = a live
    // submit from a page that has no reason to claim anything, which is the ordinary path.
    if let Some(claimed) = parsed.reviewer.as_deref() {
        if !claimed.eq_ignore_ascii_case(reviewer) {
            return err_reply(409, &format!("this decision was made by {claimed}, not {reviewer}"));
        }
    }
    let early_pool = match active_pool_policy(db, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    if let Some(pool) = early_pool.as_ref() {
        // Pool mode never mints synthetic hidden checks: every real judgement contributes to visible
        // coverage. A remembered pre-pool check on a verified clip therefore becomes an ordinary,
        // append-only pool observation rather than swallowing the review as a score-only event.
        let pool_replay = parsed
            .operation_id
            .as_deref()
            .map(|operation_id| crate::review_pool::operation(db, operation_id))
            .transpose();
        let pool_replay = match pool_replay {
            // `transpose()` preserves both option layers: `Some(Ok(None))` is `Ok(Some(None))`.
            // Testing only the outer option routes every brand-new UUID into the replay path, where
            // an unreviewed clip is correctly rejected as not yet canonical. Flatten before testing
            // so only an actually durable pool receipt is a replay.
            Ok(receipt) => receipt.flatten().is_some(),
            Err(error) => return err_reply(500, &error),
        };
        let already_canonical = match db.get_segment_by_id(&parsed.id) {
            Ok(Some(segment)) => segment.verified && segment.human_decision.is_some(),
            Ok(None) => false,
            Err(error) => return err_reply(500, &error.to_string()),
        };
        if pool_replay || already_canonical {
            // ONLY this branch is refused in production. A pool observation is a SECOND judgement on a
            // clip that already carries a canonical human answer, and `review_pool::record_decision`
            // never writes `review_compensation_ledger` — serving it would take playback-evidenced work
            // for free, so it fails closed until an owner-approved pool pay contract exists.
            //
            // The refusal used to sit at `is_some()`, before this routing ran, and `couch::start` had a
            // matching one. Together they killed ALL phone review whenever a pool row existed — and the
            // live library carries one permanently — including the FIRST-pass path below, which is fully
            // paid under review-iqd-v1-2026-08-21 and is the only path any reviewer is actually using.
            #[cfg(test)]
            return api_pool_decision(db, &parsed, reviewer, state, pool);
            #[cfg(not(test))]
            {
                let _ = pool;
                return err_reply(
                    503,
                    &format!("{PAY_POLICY_REQUIRED}: external flexible-pool decisions are disabled"),
                );
            }
        }
    }
    let early_campaign = match active_campaign_policy(db, reviewer, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    if let Some(campaign) = early_campaign.as_ref().filter(|policy| policy.is_blinded_second_pass()) {
        // Same law as the pool fence above, same shape: `record_independent_decision` never writes
        // `review_compensation_ledger`, so a blinded second-pass decision is playback-evidenced,
        // durable semantic work that would earn exactly NOTHING — and unlike the pool branch this
        // path had no fence at all, so the moment a second pass activated, up to eight reviewers
        // would work unpaid with every request returning 200 (2026-08-30 audit). It fails closed
        // until an owner-approved second-pass pay contract writes the ledger. cfg(test) passes
        // through so the campaign lifecycle tests keep exercising the real recording path.
        #[cfg(test)]
        return api_independent_decision(db, &parsed, reviewer, session_binding_sha256, state, campaign);
        #[cfg(not(test))]
        {
            let _ = campaign;
            return err_reply(
                503,
                &format!(
                    "{PAY_POLICY_REQUIRED}: blinded second-pass decisions are disabled until a pay contract exists"
                ),
            );
        }
    }

    // Validate and look up a supplied operation identity before consulting mutable corpus state. An
    // exact durable receipt is sufficient proof that this request already committed, even if the app
    // restarted, the row revision advanced, the lease expired, or the current focus later changed.
    // Conversely, a bound UUID with ANY different contract is a hard conflict, never a fresh write.
    let operation_payload_hash = if let Some(operation_id) = parsed.operation_id.as_deref() {
        let Ok(uuid) = uuid::Uuid::parse_str(operation_id) else {
            return err_reply(400, "operationId must be a canonical UUID");
        };
        if uuid.hyphenated().to_string() != operation_id {
            return err_reply(400, "operationId must be a lowercase hyphenated UUID");
        }
        let payload_hash = decision_operation_payload_hash(&parsed.id, &parsed.action, &parsed.text, reviewer);
        match review_operation_state(db, operation_id, &parsed.id, &parsed.action, &parsed.text, reviewer) {
            Ok(ReviewOperationState::ExactReplay) => {
                let effect = match db.human_decision_effect_for_operation(operation_id) {
                    Ok(effect) => effect,
                    Err(error) => return err_reply(500, &format!("decision effect lookup failed: {error}")),
                };
                let effect_event_id = effect.as_ref().map(|value| value.0);
                if let Some((effect_event_id, segment_id)) = effect.as_ref() {
                    remember_phone_undo(state, reviewer, operation_id, segment_id, *effect_event_id);
                }
                return json_reply_with_accounting(
                    200,
                    serde_json::json!({
                        "ok": true,
                        "duplicate": true,
                        "effectEventId": effect_event_id,
                    }),
                    db,
                    reviewer,
                );
            }
            Ok(ReviewOperationState::Reused) => {
                return err_reply(409, "operation UUID is already bound to another decision");
            }
            Ok(ReviewOperationState::New) => {}
            Err(error) => return err_reply(500, &format!("operation receipt lookup failed: {error}")),
        }
        Some(payload_hash)
    } else {
        None
    };
    let (prev, request_revision) = match db.get_segment_by_id_with_revision(&parsed.id) {
        Ok(Some(row)) => row,
        Ok(None) => return err_reply(404, "no such segment"),
        Err(e) => return err_reply(500, &e.to_string()),
    };
    let pilot_policy = match active_pilot_policy(reviewer, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    let campaign_policy = match active_campaign_policy(db, reviewer, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    let pool_policy = match active_pool_policy(db, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    if pool_policy != early_pool {
        return err_reply(503, "review pool changed while this decision was being checked");
    }
    if pilot_policy.is_some() && (campaign_policy.is_some() || pool_policy.is_some()) {
        return err_reply(503, "conflicting paid-review policies are active");
    }
    let pilot_namespace = match pilot_policy.as_ref() {
        Some(policy) => match policy.policy_sha256() {
            Ok(policy_sha256) => Some((policy_sha256, policy.after_review_event_id)),
            Err(error) => return err_reply(503, &format!("controlled review pilot is unavailable: {error}")),
        },
        None => None,
    };
    // Hidden checks are deliberately served with their RAW, known-wrong draft even when the row also
    // carries its human answer in annotated_transcript. Pay/action classification must describe what
    // the reviewer did to the text they actually saw; QC correctness is scored separately against the
    // answer key below. Keeping those two baselines distinct prevents an attentive correction from
    // being paid as a 10% accept and a blind raw accept from being paid as a 100% edit.
    let was_served_as_spot_check = if let Some((policy_sha256, after_review_event_id)) = pilot_namespace.as_ref() {
        match db.review_pilot_hidden_keys(policy_sha256, *after_review_event_id, reviewer) {
            Ok(ids) => ids.iter().any(|id| id == &parsed.id),
            Err(error) => return err_reply(503, &format!("hidden-check authorization is unavailable: {error}")),
        }
    } else {
        let guard = lock_state(state);
        guard.spot_checks.contains(&(parsed.id.clone(), reviewer.to_string()))
    };

    let (decision, text): (&str, Option<String>) = if parsed.action == "skip" {
        ("skip", None)
    } else {
        match parsed.action.as_str() {
            "accept" | "edit" => {
                let text = parsed.text.trim().to_string();
                if text.is_empty() {
                    return err_reply(400, "empty transcript");
                }
                // Same guard as the desktop editor: a placeholder draft ("[Pending WSL 7B ASR]" /
                // "[ASR unavailable…]") must never be verified as gold.
                if refuses_verification_as_placeholder(&text) {
                    return err_reply(400, "placeholder transcript cannot be verified");
                }
                // Pay/corpus semantics follow a MATERIAL transcript change, not byte trivia. NFC
                // composition and collapsed whitespace are the same words under the project's own
                // no-op learning key; treating either as an edit would let a formatting-only change
                // earn the 100% correction rate instead of the authorized 10% accept rate.
                let submitted_key = crate::normalizer::learning_text_key(&crate::db::to_nfc(&text));
                let served_text =
                    if was_served_as_spot_check { prev.raw_transcript.clone() } else { review_text(&prev) };
                let served_key = crate::normalizer::learning_text_key(&crate::db::to_nfc(served_text.trim()));
                let is_edit = submitted_key != served_key;
                (if is_edit { "edit" } else { "accept" }, Some(text))
            }
            "bad" => ("reject", None),
            other => return err_reply(400, &format!("unknown action '{other}'")),
        }
    };
    if parsed.heard_ms.is_some_and(|value| value < 0) {
        return err_reply(400, "heardMs must be a non-negative media-time counter");
    }
    if parsed.clip_duration_ms.is_some_and(|value| value < 0) {
        return err_reply(400, "clipDurationMs must not be negative");
    }

    // A dropped RESPONSE (not a dropped request) means the write landed and the page never heard so.
    // Answer the retry with the success it already earned, before taking a lease, minting a second
    // receipt, or pushing an undo entry for a decision that is already recorded.
    //
    // `already_recorded` without `verified` is the HALF-WRITTEN state: write one committed and the row
    // upsert did not. That is not a duplicate to be waved through — the clip is still unverified, so it
    // would be served as pending work forever — and it is not new work either, because replaying write
    // one would double the learning pair. It is an interrupted write to be FINISHED, below.
    let already_recorded =
        parsed.action != "skip" && is_repeat_of_stored_decision(&prev, reviewer, decision, text.as_deref());
    if already_recorded && prev.verified {
        // Policy-4 pages possessed a durable operation UUID before finalization. Their exact lost-
        // response retry was handled by `review_operation_state` above. A different UUID carrying a
        // receipt is therefore a new operation, not the bounded legacy rolling-deploy exception;
        // ACKing it would make a consumed playback authority appear reusable and leave the new UUID
        // unbound for a later, different decision. Only the pre-policy-4 no-receipt outbox may use the
        // compatibility ACK below.
        if parsed.playback_receipt_id.is_some() {
            return err_reply(409, "this decision already committed under a different operation identity");
        }
        // BOUNDED ROLLING-COMPATIBILITY EXCEPTION. The immediately previous page can have committed a
        // verdict before operation UUIDs existed, lost the response, then acquire a UUID only while
        // its legacy localStorage array is upgraded. There is no truthful event to which that later
        // UUID can be attached without mutating immutable financial history, so this pure ACK does
        // not reserve it. The page deletes that one-use UUID immediately; this branch writes no row,
        // event, ledger entry, receipt, or undo state and therefore cannot duplicate compensation.
        // Carries the total as well: this is the RETRY path after a dropped response, which is
        // exactly when a reviewer is least sure their work registered.
        forget_work_audio_assignment(state, &parsed.id, reviewer);
        return json_reply_with_accounting(200, serde_json::json!({ "ok": true, "duplicate": true }), db, reviewer);
    }
    if already_recorded {
        match crate::migrations::get_current_version(db) {
            Ok(version) if version >= 60 => {
                return err_reply(
                    409,
                    "this legacy interrupted decision requires offline repair before review can continue; no corpus state was changed",
                );
            }
            Ok(_) => {}
            Err(error) => return err_reply(500, &format!("schema authority lookup failed: {error}")),
        }
    }

    // A completed hidden check never changes the corpus row, so the corpus duplicate predicate above
    // cannot recognize its retry. The served-check set persists across restart and the DB result is
    // first-answer immutable; once both say this reviewer already answered, every replay is a pure ACK.
    // This must be before the new-write rowVersion guard so an outbox created by the previous page build
    // can drain, and before `audit` so retries cannot append another pay-shaped generic review event.
    let still_has_answer_key = prev.verified
        && !crate::quality::is_excluded_from_exports(&prev)
        && crate::quality::human_verified_text(&prev).is_some();
    if was_served_as_spot_check && still_has_answer_key {
        match db.has_spot_check_result(&parsed.id, reviewer) {
            Ok(true) => {
                return json_reply_with_accounting(200, serde_json::json!({ "ok": true }), db, reviewer);
            }
            Ok(false) => {}
            Err(error) => return err_reply(500, &format!("spot-check retry lookup failed: {error}")),
        }
    }

    // A NEW (or half-written) verdict without the serve-time revision has no proof of which draft the
    // reviewer judged. Reject it before minting playback evidence, claiming a lease, scoring a hidden
    // check, writing an audit event, or touching the corpus. The completed identical retry above is
    // deliberately earlier: acknowledging bytes that are already durably stored writes nothing and
    // lets an outbox created by the immediately previous phone build drain safely after deployment.
    // Hidden checks require the same parseable, fresh stamp as corpus decisions: grading does not
    // mutate the answer-key row, but its playback evidence still belongs to the exact bytes served.
    // A skip remains the only new-action exemption because it writes no verdict or paid judgement.
    let served_revision = if parsed.action == "skip" {
        None
    } else {
        let Some(stamp) = parsed.row_version.as_deref() else {
            return err_reply(400, "rowVersion is required — reload this clip before deciding");
        };
        let Ok(revision) = stamp.parse::<i64>() else {
            return err_reply(400, "rowVersion is invalid — reload this clip before deciding");
        };
        Some(revision)
    };

    // WRITE-TIME AUTHORIZATION. Queue filtering alone is not a security boundary: an outbox can
    // retain an old id after the owner changes reviewer_dialects.json / voice_focus.json, and a valid
    // bearer can POST any known id without fetching `/api/queue`. Re-read both hot policies for every
    // non-duplicate submit and refuse before ANY state or database write. An identical lost-response
    // retry above is only an acknowledgement of a write that already committed, so it needs no new
    // authorization and remains idempotent.
    let (allowed_dialects, focus) = match reviewer_policy(reviewer, state) {
        Ok(policy) => policy,
        Err(e) => return err_reply(503, &e),
    };
    let current_pilot_policy = match active_pilot_policy(reviewer, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    if current_pilot_policy != pilot_policy {
        return err_reply(503, "controlled review policy changed while this decision was being checked");
    }
    let current_campaign_policy = match active_campaign_policy(db, reviewer, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    if current_campaign_policy != campaign_policy {
        return err_reply(503, "sequential review campaign changed while this decision was being checked");
    }
    let current_pool_policy = match active_pool_policy(db, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    if current_pool_policy != pool_policy {
        return err_reply(503, "review pool changed while this decision was being checked");
    }
    match pilot_policy.as_ref() {
        Some(policy) if parsed.pilot_after_review_event_id != Some(policy.after_review_event_id) => {
            return err_reply(409, "controlled review pilot changed — reload the queue before deciding");
        }
        None if parsed.pilot_after_review_event_id.is_some() => {
            return err_reply(409, "controlled review pilot is no longer active — reload the queue before deciding");
        }
        _ => {}
    }
    if !reviewer_policy_allows(allowed_dialects.as_deref(), focus.as_deref(), &prev) {
        // If policy changed after this reviewer was served the clip, release only THEIR lease so it
        // can immediately return to an eligible reviewer. Never disturb somebody else's live work.
        let mut guard = lock_state(state);
        if guard.holder(&parsed.id, Instant::now()).is_some_and(|who| who == reviewer) {
            guard.leases.remove(&parsed.id);
        }
        guard.served_work.remove(&(parsed.id.clone(), reviewer.to_string()));
        return err_reply(403, "this clip is outside your current review assignment — reload your queue");
    }

    // SPOT-CHECK DETECTION, hoisted above the version fence (2026-08-20 hunt). Grading a check
    // writes NOTHING to the corpus row, so a revision bump between serve and submit cannot
    // invalidate it — and bulk metadata runs (a rights stamp, a re-annotation) bump every GOLD
    // row's revision at once, which would otherwise 409 every check in flight and cost the honesty
    // measurement its scores. The full staleness rationale for the key itself is on the grading
    // block below.
    let (expected_key, invalid_pilot_key, served_in_this_pilot): (Option<String>, bool, bool) =
        if pilot_policy.is_some() {
            if !was_served_as_spot_check {
                (None, false, false)
            } else {
                let answer = if crate::quality::is_excluded_from_exports(&prev) {
                    None
                } else {
                    crate::quality::human_verified_text(&prev)
                };
                match answer {
                    Some(answer) => (Some(answer.to_string()), false, true),
                    None => (None, true, true),
                }
            }
        } else {
            let mut guard = lock_state(state);
            let key = (parsed.id.clone(), reviewer.to_string());
            if !guard.spot_checks.contains(&key) {
                (None, false, false)
            } else {
                let served_in_this_pilot = guard.pilot_spot_checks.contains(&key);
                let answer = if crate::quality::is_excluded_from_exports(&prev) {
                    None
                } else {
                    crate::quality::human_verified_text(&prev)
                };
                if let Some(answer) = answer {
                    (Some(answer.to_string()), false, served_in_this_pilot)
                } else if served_in_this_pilot {
                    // Replacing this bounded pilot key would consume a third distinct key; treating the
                    // submit as ordinary work would silently erase the measurement. Pause instead.
                    (None, true, true)
                } else {
                    // The answer key is gone — the thing that made this a check. Drop the stale pair so it
                    // cannot swallow this clip again, and treat the submit as what it is: real work.
                    guard.spot_checks.remove(&key);
                    tracing::info!(
                        "Couch Review: stale spot check for {} dropped — the human answer key is gone",
                        parsed.id
                    );
                    (None, false, false)
                }
            }
        };
    if invalid_pilot_key {
        return err_reply(503, "Review is temporarily paused: a controlled hidden-check key is no longer valid");
    }
    if pilot_policy.is_some() && expected_key.is_some() && !served_in_this_pilot {
        return err_reply(409, "controlled review pilot requires a fresh queue before this check can be submitted");
    }
    if pilot_policy.is_some() && expected_key.is_none() {
        let mut guard = lock_state(state);
        if !guard.holder(&parsed.id, Instant::now()).is_some_and(|who| who == reviewer) {
            return err_reply(409, "controlled review pilot requires this work to be served first — reload the queue");
        }
    }

    // SERVE/DECIDE VERSION FENCE (text-provenance audit #4). The queue stamped this clip's
    // monotonic revision into the payload; if the row changed between serve and submit (batch
    // re-transcribe, refine loop, desktop edit — all of which target exactly the unverified rows
    // the couch serves), the reviewer judged text that no longer exists: recording it would
    // misclassify accept-vs-edit against the NEW row and mint a DPO pair anti-training the fresher
    // draft. Refuse; the page reloads and serves the current text.
    //
    // Placed BEFORE the receipt mint (audit fix 2026-08-20). The receipt's revision, content hash,
    // and duration are all resolved from the row, so minting one for a submit whose serve predates
    // a re-chunk would bind this reviewer's real listening to audio they never heard — evidence
    // manufactured by ordering. A stale serve must be refused while it is still only a claim.
    //
    // Two exemptions, each because the fence would refuse an act that cannot conflict:
    //   * a replay finishing an already-recorded decision — its write one landed, so the stamp has
    //     legitimately moved;
    //   * a SKIP — it writes no verdict, and fencing it boomeranged the clip back to the same
    //     reviewer forever (the lease is kept on a 409) while the offline replay of the skip was
    //     dropped into the "work lost" banner, all for an act with nothing to be stale about.
    // A hidden check is NOT exempt: the browser's heard milliseconds belong to the exact audio it
    // was served. Rebinding them to whatever revision happens to exist at submit time would create
    // playback evidence for bytes the reviewer may never have heard, even though grading leaves the
    // corpus row unchanged. Availability after a benign metadata stamp is secondary to evidence
    // identity; the reviewer reloads and hears the current row.
    // Neither exemption can manufacture evidence: the mint below re-verifies the revision
    // atomically and simply declines the receipt when the row has moved.
    if !already_recorded && parsed.action != "skip" && served_revision != Some(request_revision) {
        return err_reply(409, "this clip changed since it was served — reload for the fresh draft");
    }

    // Rolling-deploy exception: completed legacy verdict/check retries above are pure ACKs and may
    // omit this new field. Every operation that can still write anything must have its durable client
    // UUID before the first side effect (including a playback receipt and the zero-credit skip event).
    let Some(operation_id) = parsed.operation_id.as_deref() else {
        return err_reply(400, "operationId is required — reload this page before deciding");
    };
    let Some(operation_payload_hash) = operation_payload_hash.as_deref() else {
        return err_reply(400, "operationId payload could not be validated — reload this page before deciding");
    };

    // Serialize the last pre-receipt check with the database commit. The database repeats the cap
    // check under BEGIN IMMEDIATE (the cross-connection authority); this outer lock ensures a losing
    // in-process request leaves no playback-receipt side effect before that transaction refuses it.
    // A regular-work skip is zero-pay and not a verdict, but it DOES consume a pilot corpus safety
    // slot: otherwise repeated skips could refill queues forever. A hidden-check skip belongs to the
    // separately bounded two-key QC budget and is recorded as a failed QC result below, not corpus work.
    let mut pilot_decision_limit: Option<ReviewDecisionLimit> = None;
    let _pilot_commit_guard = if expected_key.is_none() {
        let guard = lock_pilot_decision_commit();
        let current = match active_pilot_policy(reviewer, state) {
            Ok(policy) => policy,
            Err(error) => return err_reply(503, &error),
        };
        if current != pilot_policy {
            return err_reply(503, "controlled review policy changed while this decision was being checked");
        }
        let current_campaign = match active_campaign_policy(db, reviewer, state) {
            Ok(policy) => policy,
            Err(error) => return err_reply(503, &error),
        };
        if current_campaign != campaign_policy {
            return err_reply(503, "sequential review campaign changed while this decision was being committed");
        }
        let current_pool = match active_pool_policy(db, state) {
            Ok(policy) => policy,
            Err(error) => return err_reply(503, &error),
        };
        if current_pool != pool_policy {
            return err_reply(503, "review pool changed while this decision was being committed");
        }
        if let Some(policy) = current.as_ref() {
            match pilot_remaining_slots(db, reviewer, policy) {
                Ok(0) => {
                    return err_reply(409, "controlled review pilot complete — no more review actions are authorized");
                }
                Ok(_) => {}
                Err(error) => return err_reply(503, &format!("controlled review pilot is unavailable: {error}")),
            }
            pilot_decision_limit = match build_pilot_decision_limit(policy) {
                Ok(limit) => Some(limit),
                Err(error) => return err_reply(503, &error),
            };
        }
        Some(guard)
    } else {
        None
    };

    // PLAYBACK AUTHORITY. A new verdict never mints evidence from request counters. The page must first
    // finalize a server-issued, cookie-session-bound attempt carrying a normalized interval union. This
    // lookup acquires an immutable source lease and the decision transaction repeats the exact policy-4
    // predicate while consuming the receipt once. Skip remains exempt because it is explicitly no verdict.
    let content_hash = match db.segment_audio_content_hash(&parsed.id) {
        Ok(Some(value)) => Some(value),
        Ok(None) if parsed.action == "skip" => None,
        Ok(None) => {
            return err_reply(
                503,
                "playback identity is unavailable — this clip has no canonical server-derived audio content hash",
            );
        }
        Err(error) => return err_reply(500, &format!("playback identity lookup failed: {error}")),
    };
    let revision = request_revision;
    let playback_proof = if parsed.action == "skip" {
        None
    } else {
        if parsed.heard_ms.is_some() || parsed.clip_duration_ms.is_some() {
            return err_reply(
                428,
                &format!(
                    "E_NO_PLAYBACK_EVIDENCE: {LEGACY_RAW_COUNTER_REFUSAL_MARKER}; legacy playback counters cannot authorize a verdict"
                ),
            );
        }
        let Some(content_hash) = content_hash.as_deref() else {
            return err_reply(
                503,
                "playback identity is unavailable — this clip has no canonical server-derived audio content hash",
            );
        };
        let Some(playback_receipt_id) = parsed.playback_receipt_id.as_deref() else {
            return err_reply(428, "E_NO_PLAYBACK_EVIDENCE: finalize this clip's playback attempt before deciding");
        };
        match db.couch_playback_proof_v4(
            &parsed.id,
            revision,
            content_hash,
            reviewer,
            session_binding_sha256,
            playback_receipt_id,
        ) {
            Ok(Some(proof)) => Some(proof),
            Ok(None) => {
                return err_reply(
                    428,
                    "E_NO_PLAYBACK_EVIDENCE: playback authority does not match this reviewer, session, clip, or revision",
                );
            }
            Err(error) => return playback_error_reply(&error.to_string()),
        }
    };
    // SKIP — the explicit NO-VERDICT (R4.4), handled before any of the write machinery because it
    // shares none of it.
    //
    // A reviewer who genuinely cannot call a clip — two people talking over each other, an accent they
    // do not have, audio that will not play — previously had exactly two ways forward, and both write a
    // judgement they cannot stand behind: "Looks good" promotes an unheard draft to gold, "Reject"
    // permanently excludes a clip that may be perfectly fine. A guess is worse for the corpus than an
    // honest "I don't know", because nothing downstream can tell the two apart. So this writes NOTHING
    // to the row — no decision, no verdict, no attribution, no `verified` — and does only two things:
    //
    //   * records the act in the audit trail, so the owner can see which clips defeated a human (a clip
    //     several reviewers skip is telling you something about the clip), and
    //   * takes it out of THIS reviewer's queue and releases their lease, so it goes to somebody who
    //     can judge it instead of being handed straight back on the next refill.
    //
    // Nothing to undo, because nothing was written; the clip is simply still pending for everyone else.
    if parsed.action == "skip" && expected_key.is_none() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);
        if let Err(error) = db.record_review_event_with_operation_limit(
            &parsed.id,
            reviewer,
            "skip",
            "couch",
            now_ms,
            operation_id,
            operation_payload_hash,
            &parsed.action,
            &parsed.text,
            pilot_decision_limit.as_ref(),
        ) {
            tracing::warn!("Couch Review skip event not recorded for {}: {error}", parsed.id);
            if error.to_string().contains(REVIEW_PILOT_LIMIT_REACHED) {
                let mut guard = lock_state(state);
                if guard.leases.get(&parsed.id).is_some_and(|(who, _)| who == reviewer) {
                    guard.leases.remove(&parsed.id);
                }
                return err_reply(409, "controlled review pilot complete — no more review actions are authorized");
            }
            return operation_result_after_write_failure(
                db,
                reviewer,
                &parsed,
                operation_id,
                500,
                "skip not recorded — retrying is safe",
            );
        }
        {
            let mut guard = lock_state(state);
            // Only OUR OWN lease. A clip another reviewer currently holds is not ours to hand back —
            // that would free their in-progress work out from under them.
            if guard.holder(&parsed.id, Instant::now()).is_some_and(|who| who == reviewer) {
                guard.leases.remove(&parsed.id);
            }
            guard.served_work.remove(&(parsed.id.clone(), reviewer.to_string()));
            guard.skipped.entry(reviewer.to_string()).or_default().insert(parsed.id.clone());
        }
        return json_reply_with_accounting(200, serde_json::json!({ "ok": true, "skipped": true }), db, reviewer);
    }

    // SPOT CHECK (P2.1): this clip already has a human answer, so the reviewer was being measured, not
    // asked to do new work. Record the score and stop — the corpus row is left completely untouched,
    // because a mechanism that grades reviewers must never be able to alter the data it grades against.
    //
    // The reply is byte-identical to a normal success. A reviewer who could tell a test from real work
    // would simply be careful on the tests, which measures nothing.
    // A spot check is BY DEFINITION a clip that already carries a human answer to grade against, and
    // `prev.verified` is what makes it one. The served-set is never pruned and now SURVIVES RESTARTS,
    // so a pair could outlive the answer key: the owner un-verifies the clip at the desktop (or
    // re-transcribes it, which also clears `verified`), api_queue then serves it as ordinary pending
    // work — nothing filters served checks out of the work queue — and the reviewer transcribes it for
    // real. Grading that against a key that no longer exists recorded a bogus score, returned a reply
    // deliberately indistinguishable from success, and wrote NOTHING to the corpus. The clip stayed
    // unverified, came back in the next batch, and was swallowed again every single time.
    // Carries the ANSWER KEY, not a bool: a check with no key cannot be graded, and returning the key
    // itself makes that structural instead of a comment (external review 2026-08-06).
    //
    // The staleness test used to be `!prev.verified`, which is strictly weaker than the predicate that
    // MINTED the key — `list_spot_check_candidates` requires `human_verified_text(&seg).is_some()`.
    // Anything that strips the human answer while leaving `verified = 1` left the pair live, and the
    // grader then did `.unwrap_or_default()`, scoring the reviewer against "". That is reachable
    // through the ordinary desktop "mark bad": it writes human_decision='reject' / verdict='human_reject'
    // while KEEPING verified, and for a key whose answer lived in verdict_transcript
    // `human_verified_text` then returns None. compute_cer("", submitted) is 1.0 for any non-empty
    // answer, so a reviewer who transcribed the clip CORRECTLY was recorded at a fabricated 1.00 CER
    // (or 0.00 for a reject) and averaged into spot_check_report with no filter — while the HTTP reply
    // was byte-identical to a real success. Test the key, not its artefact.
    if let Some(expected) = expected_key {
        match active_pilot_policy(reviewer, state) {
            Ok(current) if current == pilot_policy => {}
            Ok(_) => return err_reply(503, "controlled review policy changed while this check was being saved"),
            Err(error) => return err_reply(503, &error),
        }
        let submitted = text.as_deref().unwrap_or_default();
        let recorded = if let Some((policy_sha256, after_review_event_id)) = pilot_namespace.as_ref() {
            db.record_pilot_spot_check_with_operation_request(
                policy_sha256,
                *after_review_event_id,
                &parsed.id,
                reviewer,
                decision,
                submitted,
                &expected,
                &parsed.action,
                &parsed.text,
                playback_proof.as_ref(),
                operation_id,
                operation_payload_hash,
            )
        } else {
            db.record_spot_check_with_operation_request(
                &parsed.id,
                reviewer,
                decision,
                submitted,
                &expected,
                &parsed.action,
                &parsed.text,
                playback_proof.as_ref(),
                operation_id,
                operation_payload_hash,
            )
        };
        if let Err(e) = recorded {
            tracing::warn!("Couch Review spot-check not recorded for {}: {e}", parsed.id);
            let message = e.to_string();
            if message.contains(PLAYBACK_EVIDENCE_CHANGED) || message.contains(HIDDEN_ANSWER_KEY_CHANGED) {
                return err_reply(409, "this clip changed while the decision was being saved — reload it");
            }
            // A 200 removes this decision from the phone's durable outbox forever. If the score did
            // not commit, success would silently discard the first answer and let a later attempt
            // replace the measurement. Return the same retryable 5xx class used by ordinary decision
            // write failures; keep the client-facing text generic so the hidden check stays hidden.
            // The retry is safe: record_spot_check is first-answer immutable, and the early completed-
            // check ACK above absorbs every replay once the insert has committed.
            return operation_result_after_write_failure(
                db,
                reviewer,
                &parsed,
                operation_id,
                500,
                "decision not recorded — retrying is safe",
            );
        }
        // `record_spot_check` committed the immutable first score, audit event, and pay-ledger delta
        // together. A second best-effort audit here would re-open the kill window and double-count a
        // dropped-response retry.
        return json_reply_with_accounting(200, serde_json::json!({ "ok": true }), db, reviewer);
    }

    // THE LATE-SUBMIT GUARD. The lease is RELEASED the moment a clip is decided — correctly, since it
    // is no longer pending work — which leaves an already-decided clip unprotected by the collision
    // check below. A stale page could then submit onto it minutes later and silently replace another
    // human's verdict, `reviewed_by` and all: precisely the destruction leases exist to prevent, just
    // arriving late instead of concurrently. A decided clip never legitimately reaches a phone queue
    // (the queue serves unverified rows only), so refusing costs nothing real.
    //
    // The SAME reviewer is exempt: correcting your own decision is a genuine re-review, and the
    // retry case above has already been answered.
    if prev.verified && !prev.reviewed_by.as_deref().is_some_and(|stored| same_reviewer(stored, reviewer)) {
        return match prev.reviewed_by.as_deref() {
            Some(other) => err_reply(409, &format!("already reviewed by {other}")),
            None => err_reply(409, "already reviewed at the desktop"),
        };
    }

    // THE COLLISION GUARD, placed here — after every validation, immediately before the write.
    //
    // Check and claim happen under ONE lock so they are atomic. Refusing is the honest outcome: the two
    // reviewers judged the clip independently, and silently keeping whichever submit landed second would
    // destroy the other's verdict with nobody aware. Claiming (not just checking) matters because a
    // submit can arrive for an UNLEASED clip — a page kept open across a restart — and a bare
    // check-then-write would let two such submits both pass and both write.
    //
    // It sits after validation so a REJECTED request (bad action, placeholder text, missing row) cannot
    // leave a 15-minute lease on a clip it never decided, locking other reviewers out of it.
    {
        let now = Instant::now();
        let mut guard = lock_state(state);
        if guard.holder(&parsed.id, now).is_some_and(|who| who != reviewer) {
            return err_reply(409, "another reviewer is working on this clip");
        }
        if !already_recorded && !guard.in_flight_operations.insert(operation_id.to_string()) {
            return err_reply(503, "this operation is still being saved — retrying is safe");
        }
        guard.leases.insert(parsed.id.clone(), (reviewer.to_string(), now));
    }
    // New decisions finalize in the SAME transaction that mints the DPO pair/correction memories.
    // A legacy half-written row is finalized without replaying those side effects.
    let Some(corpus_playback_proof) = playback_proof.as_ref() else {
        return err_reply(428, "E_NO_PLAYBACK_EVIDENCE: canonical verdict has no policy-4 authority");
    };
    if already_recorded {
        return match db.finalize_phone_human_decision_at_revision(&parsed.id, text.as_deref(), request_revision) {
            Ok(Some(_)) => {
                forget_work_audio_assignment(state, &parsed.id, reviewer);
                json_reply_with_accounting(200, serde_json::json!({ "ok": true, "duplicate": true }), db, reviewer)
            }
            Ok(None) => {
                err_reply(409, "this interrupted legacy decision changed before finalization — reload the fresh row")
            }
            Err(error) => err_reply(500, &error.to_string()),
        };
    }

    let commit = match db.record_phone_human_decision_by_at_revision_with_operation_limit(
        &parsed.id,
        decision,
        text.as_deref(),
        reviewer,
        request_revision,
        corpus_playback_proof,
        operation_id,
        operation_payload_hash,
        &parsed.action,
        &parsed.text,
        pilot_decision_limit.as_ref(),
    ) {
        Ok(Some(commit)) => commit,
        Ok(None) => {
            lock_state(state).in_flight_operations.remove(operation_id);
            return operation_result_after_write_failure(
                db,
                reviewer,
                &parsed,
                operation_id,
                409,
                "this clip changed while the decision was being saved — reload for the fresh draft",
            );
        }
        Err(error) => {
            // A concurrent identical request may have won the UNIQUE race. Resolve the immutable
            // effect before publishing an undo token; no row snapshot is ever reconstructed here.
            if matches!(
                review_operation_state(db, operation_id, &parsed.id, &parsed.action, &parsed.text, reviewer,),
                Ok(ReviewOperationState::ExactReplay)
            ) {
                let effect = match db.human_decision_effect_for_operation(operation_id) {
                    Ok(Some(effect)) => effect,
                    Ok(None) => {
                        lock_state(state).in_flight_operations.remove(operation_id);
                        return err_reply(500, "committed decision is missing its immutable effect");
                    }
                    Err(lookup_error) => {
                        lock_state(state).in_flight_operations.remove(operation_id);
                        return err_reply(500, &format!("decision effect lookup failed: {lookup_error}"));
                    }
                };
                remember_phone_undo(state, reviewer, operation_id, &effect.1, effect.0);
                forget_work_audio_assignment(state, &parsed.id, reviewer);
                lock_state(state).in_flight_operations.remove(operation_id);
                return json_reply_with_accounting(
                    200,
                    serde_json::json!({
                        "ok": true,
                        "duplicate": true,
                        "effectEventId": effect.0,
                    }),
                    db,
                    reviewer,
                );
            }
            if error.to_string().contains(REVIEW_PILOT_LIMIT_REACHED) {
                let mut guard = lock_state(state);
                if guard.leases.get(&parsed.id).is_some_and(|(who, _)| who == reviewer) {
                    guard.leases.remove(&parsed.id);
                }
                guard.in_flight_operations.remove(operation_id);
                return err_reply(409, "controlled review pilot complete — no more review actions are authorized");
            }
            lock_state(state).in_flight_operations.remove(operation_id);
            return operation_result_after_write_failure(db, reviewer, &parsed, operation_id, 500, &error.to_string());
        }
    };
    lock_state(state).in_flight_operations.remove(operation_id);
    remember_phone_undo(state, reviewer, operation_id, &commit.segment_id, commit.effect_event_id);
    forget_work_audio_assignment(state, &parsed.id, reviewer);
    json_reply_with_accounting(
        200,
        serde_json::json!({ "ok": true, "effectEventId": commit.effect_event_id }),
        db,
        reviewer,
    )
}
/// Undo this reviewer's last decision through its immutable database effect. The phone supplies no
/// row snapshot and cannot restore fields the decision did not own.
pub(super) fn api_independent_undo(
    db: &Database,
    reviewer: &str,
    state: &Mutex<CouchState>,
    campaign: &crate::review_campaign::SequentialReviewCampaign,
) -> Reply {
    let entry = lock_state(state).independent_undo.get_mut(reviewer).and_then(|stack| stack.pop());
    let entry = match entry {
        Some(entry) => entry,
        None => match crate::review_campaign::latest_independent_decision(db, &campaign.campaign_id, reviewer) {
            Ok(Some((decision_id, seg_id, operation_id))) => IndependentUndoEntry { operation_id, seg_id, decision_id },
            Ok(None) => return err_reply(409, "nothing to undo"),
            Err(error) => return err_reply(500, &format!("independent undo target lookup failed: {error}")),
        },
    };
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let reversal_operation_id = match crate::review_campaign::independent_reversal_operation_id(&entry.operation_id) {
        Ok(operation_id) => operation_id,
        Err(error) => {
            lock_state(state).independent_undo.entry(reviewer.to_string()).or_default().push(entry);
            return err_reply(500, &format!("independent undo identity cannot be derived: {error}"));
        }
    };
    if let Err(error) = crate::review_campaign::reverse_independent_decision(
        db,
        campaign,
        entry.decision_id,
        reviewer,
        &reversal_operation_id,
        now_ms,
    ) {
        lock_state(state).independent_undo.entry(reviewer.to_string()).or_default().push(entry);
        return err_reply(500, &error);
    }
    // Retain the exact target so a lost bodyless response replays the same reversal instead of
    // retracting an older decision. The next newly committed decision replaces it at the stack tail.
    let id = entry.seg_id.clone();
    remember_independent_undo(state, reviewer, &entry.operation_id, &entry.seg_id, entry.decision_id);
    {
        let mut guard = lock_state(state);
        guard.leases.insert(id.clone(), (reviewer.to_string(), Instant::now()));
    }
    let row_version = db
        .get_segment_by_id_with_revision(&id)
        .ok()
        .flatten()
        .map(|(_, revision)| revision)
        .unwrap_or_default()
        .to_string();
    json_reply_with_accounting(
        200,
        serde_json::json!({ "id": id, "rowVersion": row_version, "independent": true }),
        db,
        reviewer,
    )
}

pub(super) fn api_pool_undo(
    db: &Database,
    reviewer: &str,
    state: &Mutex<CouchState>,
    pool: &crate::review_pool::ReviewPool,
    target: &PoolUndoTarget,
) -> Reply {
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let id = match crate::review_pool::reverse_decision_addressed(
        db,
        pool,
        target.decision_id,
        reviewer,
        &target.decision_operation_id,
        &target.reversal_operation_id,
        now_ms,
    ) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return err_reply(
                409,
                "pool undo target is stale or does not match this reviewer — reload before retrying",
            );
        }
        Err(error) => return err_reply(500, &error),
    };
    remember_pool_undo(state, reviewer, &target.decision_operation_id, &id, target.decision_id);
    {
        let mut guard = lock_state(state);
        guard.leases.insert(id.clone(), (reviewer.to_string(), Instant::now()));
        guard.served_work.insert((id.clone(), reviewer.to_string()));
    }
    let row_version = db
        .get_segment_by_id_with_revision(&id)
        .ok()
        .flatten()
        .map(|(_, revision)| revision)
        .unwrap_or_default()
        .to_string();
    json_reply_with_accounting(
        200,
        serde_json::json!({
            "id": id,
            "rowVersion": row_version,
            "independent": true,
            "reviewPool": true,
            "poolDecisionId": target.decision_id,
            "reversalOperationId": target.reversal_operation_id,
        }),
        db,
        reviewer,
    )
}

#[cfg(test)]
pub(super) fn api_decision(db: &Database, body: &[u8], reviewer: &str, state: &Mutex<CouchState>) -> Reply {
    api_decision_authenticated(db, body, reviewer, &couch_session_binding_sha256("couch-test-session"), state)
}

#[cfg(test)]
pub(super) fn api_undo(db: &Database, reviewer: &str, state: &Mutex<CouchState>) -> Reply {
    api_undo_with_body(db, &[], reviewer, state)
}

pub(super) fn api_undo_with_body(db: &Database, body: &[u8], reviewer: &str, state: &Mutex<CouchState>) -> Reply {
    let pool_target = match parse_undo_body(body) {
        Ok(target) => target,
        Err(error) => return err_reply(400, &error),
    };
    let pool = match active_pool_policy(db, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    if let Some(pool) = pool.as_ref() {
        if let Some(target) = pool_target.as_ref() {
            return api_pool_undo(db, reviewer, state, pool, target);
        }
        let (has_pool_token, has_canonical_token) = {
            let guard = lock_state(state);
            (
                guard.pool_undo.get(reviewer).is_some_and(|stack| !stack.is_empty()),
                guard.undo.get(reviewer).is_some_and(|stack| !stack.is_empty()),
            )
        };
        if has_pool_token && !has_canonical_token {
            return err_reply(409, "pool undo requires the exact durable target from the decision response");
        }
        if !has_pool_token && !has_canonical_token {
            // Read the append-only table, including decisions already reversed. The effective view
            // drops the just-undone row and was the source of the lost-response/restart bug: a retry
            // then selected the previous decision. A bodyless request may fall through to canonical
            // undo only when canonical truth is durably newer than every pool action.
            let pool_latest: Result<Option<i64>, _> = db
                .connection()
                .query_row(
                    "SELECT created_at_ms FROM review_pool_decisions
                      WHERE pool_id=?1 AND reviewer=?2 COLLATE NOCASE
                      ORDER BY id DESC LIMIT 1",
                    rusqlite::params![pool.pool_id, reviewer],
                    |row| row.get(0),
                )
                .optional();
            let pool_latest = match pool_latest {
                Ok(value) => value,
                Err(error) => return err_reply(500, &format!("pool undo order cannot be read: {error}")),
            };
            let canonical_latest: Result<Option<i64>, _> = db
                .connection()
                .query_row(
                    "SELECT event.timestamp_ms
                   FROM effective_human_decision_effects_v60 effect
                   JOIN review_events event ON event.id=effect.review_event_id
                  WHERE effect.reviewer=?1 COLLATE NOCASE
                  ORDER BY effect.id DESC LIMIT 1",
                    [reviewer],
                    |row| row.get(0),
                )
                .optional();
            let canonical_latest = match canonical_latest {
                Ok(value) => value,
                Err(error) => return err_reply(500, &format!("canonical undo order cannot be read: {error}")),
            };
            // map_or, not is_none_or: the latter is stable only since 1.82 and this crate's MSRV is 1.81.
            if pool_latest.is_some_and(|at| canonical_latest.map_or(true, |other| at >= other)) {
                return err_reply(409, "pool undo requires the exact durable target from the decision response");
            }
        }
    } else if pool_target.is_some() {
        return err_reply(409, "the addressed review pool is no longer active");
    }
    let campaign = match active_campaign_policy(db, reviewer, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    if let Some(campaign) = campaign.as_ref().filter(|policy| policy.is_blinded_second_pass()) {
        return api_independent_undo(db, reviewer, state, campaign);
    }
    let popped = lock_state(state).undo.get_mut(reviewer).and_then(|stack| {
        let insertion_index = stack.len().checked_sub(1)?;
        stack.pop().map(|entry| (entry, insertion_index))
    });
    let (entry, insertion_index) = match popped {
        Some(value) => value,
        None => match db.latest_phone_human_decision_effect(reviewer) {
            Ok(Some((effect_event_id, operation_id, seg_id))) => {
                (UndoEntry { operation_id, seg_id, effect_event_id }, 0)
            }
            Ok(None) => return err_reply(409, "nothing to undo"),
            Err(error) => return err_reply(500, &format!("undo target lookup failed: {error}")),
        },
    };
    let id = entry.seg_id.clone();
    let outcome = match db.undo_human_decision(entry.effect_event_id, Some(reviewer), &entry.operation_id) {
        Ok(outcome) => outcome,
        Err(error) => {
            retain_phone_undo_token(state, reviewer, insertion_index, entry);
            return err_reply(500, &error.to_string());
        }
    };
    let (segment, restored_revision) = match outcome {
        HumanDecisionUndoOutcome::Applied { restored_revision, segment } => (segment, Some(restored_revision)),
        HumanDecisionUndoOutcome::AlreadyApplied { segment, .. } => (segment, None),
        HumanDecisionUndoOutcome::Conflict { segment } => {
            retain_phone_undo_token(state, reviewer, insertion_index, entry);
            let owner = segment.reviewed_by.clone();
            return match owner {
                Some(other) if !other.eq_ignore_ascii_case(reviewer) => {
                    err_reply(409, &format!("{other} has reviewed this clip since — undo would erase their work"))
                }
                None => err_reply(409, "this clip changed at the desktop since — undo refused without mutation"),
                _ => err_reply(409, "this clip changed since the decision — undo refused without mutation"),
            };
        }
    };
    // Keep the same stable token after success. If the response is lost, the bodyless retry must
    // replay this exact idempotent undo instead of falling through to an older decision.
    retain_phone_undo_token(state, reviewer, insertion_index, entry);
    {
        let mut guard = lock_state(state);
        let now = Instant::now();
        let current = guard.holder(&id, now).map(str::to_string);
        match current {
            Some(other) if other != reviewer => {
                tracing::info!(
                    "Couch Review: undo by {reviewer} left {id} leased to {other} — not stealing an active lease"
                );
            }
            _ => {
                guard.leases.insert(id.clone(), (reviewer.to_string(), now));
            }
        }
    }
    let row_version = restored_revision
        .or_else(|| db.get_segment_by_id_with_revision(&id).ok().flatten().map(|(_, revision)| revision))
        .unwrap_or_default()
        .to_string();
    json_reply_with_accounting(
        200,
        serde_json::json!({ "id": id, "rowVersion": row_version, "segment": segment }),
        db,
        reviewer,
    )
}
// ─── Tests ────────────────────────────────────────────────────────────────────
