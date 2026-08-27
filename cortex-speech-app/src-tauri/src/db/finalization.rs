use super::*;

impl Database {
    // The parameters are deliberately explicit: each is a distinct fact about ONE adjudication (which clip,
    // what verdict, whose text, when, who, at which revision, whether this call finalizes, and which
    // surface audits it). Bundling them into a struct would move the width rather than remove it,
    // and this is a private helper with three call sites — all in this file.
    //
    // `audit_source`: when `Some`, the review_events audit row — the basis reviewers are PAID on —
    // is written INSIDE this same transaction. The 2026-08-20 hunt measured the alternative: the
    // phone wrote it as a separate best-effort INSERT after the commit, so a kill (or SQLITE_BUSY)
    // in between left a verified row whose completed work no pay metric could ever see, and the
    // outbox replay hit the duplicate fast-path without backfilling it. Requires `annotator`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_human_decision_by_with_finalize(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        timestamp_ms: Option<i64>,
        annotator: Option<&str>,
        expected_revision: Option<i64>,
        finalize: bool,
        audit_source: Option<&str>,
        audit_operation: Option<(&str, &str)>,
        audit_request: Option<(&str, &str)>,
        decision_limit: Option<&ReviewDecisionLimit>,
        required_playback: Option<&PlaybackDecisionProof>,
        review_draft_revision: Option<i64>,
    ) -> AppResult<Option<HumanDecisionCommit>> {
        if audit_source.is_none() && annotator.is_some() {
            return Err(AppError::Validation(
                "named reviewers cannot use the anonymous desktop decision boundary; use the attributed Couch writer"
                    .into(),
            ));
        }
        if decision_limit.is_some() && (audit_source != Some("couch") || annotator.is_none()) {
            return Err(AppError::Validation(
                "controlled-review limits are valid only for attributed Couch decisions".into(),
            ));
        }
        let playback_identity_is_valid = matches!((audit_source, annotator), (Some("couch"), Some(_)) | (None, None));
        if required_playback.is_some() && !playback_identity_is_valid {
            return Err(AppError::Validation(
                "playback-bound review writes require an attributed Couch reviewer or anonymous desktop identity"
                    .into(),
            ));
        }
        if expected_revision.is_some_and(|revision| revision < 0) {
            return Err(AppError::Validation("human decision revision must be non-negative".into()));
        }
        if timestamp_ms.is_some_and(|timestamp| timestamp <= 0) {
            return Err(AppError::Validation("human decision timestamp must be positive".into()));
        }
        if review_draft_revision.is_some()
            && !(audit_source.is_none()
                && annotator.is_none()
                && timestamp_ms.is_none()
                && audit_operation.is_some()
                && expected_revision == review_draft_revision)
        {
            return Err(AppError::Validation(
                "review drafts may be cleared only by their exact typed desktop decision revision".into(),
            ));
        }
        if audit_source.is_none() && audit_operation.is_some() && timestamp_ms.is_none() {
            let typed_payload_is_exact =
                expected_revision.zip(audit_operation).is_some_and(|(revision, (_, supplied_hash))| {
                    required_playback.and_then(|playback| playback.authority_session_id.as_deref()).is_some_and(
                        |authority_id| {
                            supplied_hash
                                == desktop_review_v1_payload_hash(
                                    segment_id,
                                    revision,
                                    decision,
                                    corrected_transcript,
                                    authority_id,
                                )
                        },
                    )
                });
            if !typed_payload_is_exact {
                return Err(AppError::Validation(
                    "replayable desktop decisions without a client timestamp require the exact typed review payload"
                        .into(),
                ));
            }
        }
        human_verdict_for_decision(decision)?;
        let corrected_owned: Option<String> =
            corrected_transcript.map(|t| to_nfc(t.trim())).filter(|value| !value.is_empty());
        let (requested_action, requested_transcript) = audit_request
            .map(|(action, transcript)| (action.trim().to_string(), to_nfc(transcript.trim())))
            .unwrap_or_else(|| (decision.to_string(), corrected_owned.clone().unwrap_or_default()));
        if requested_action.is_empty() || requested_action.chars().any(char::is_control) {
            return Err(AppError::Validation("review request action must be a non-blank canonical token".into()));
        }
        if audit_source == Some("couch")
            && !matches!(requested_action.as_str(), "accept" | "edit" | "reject" | "bad" | "skip")
        {
            return Err(AppError::Validation("Couch request action is outside the accepted client vocabulary".into()));
        }
        let desktop_requested_transcript = corrected_owned.clone();
        let desktop_requested_timestamp_ms = if audit_source.is_none() {
            Some(timestamp_ms.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or(1)
                    .max(1)
            }))
        } else {
            None
        };
        let supplied_desktop_operation = audit_source.is_none() && audit_operation.is_some();
        let generated_operation = audit_operation.is_none().then(|| {
            let payload_hash = if let (Some("couch"), Some(reviewer)) = (audit_source, annotator) {
                review_operation_payload_hash(segment_id, &requested_action, &requested_transcript, reviewer)
            } else {
                desktop_decision_payload_hash(
                    segment_id,
                    decision,
                    desktop_requested_transcript.as_deref(),
                    desktop_requested_timestamp_ms,
                )
            };
            (uuid::Uuid::new_v4().to_string(), payload_hash)
        });
        let operation_identity = audit_operation.or_else(|| {
            generated_operation
                .as_ref()
                .map(|(operation_id, payload_hash)| (operation_id.as_str(), payload_hash.as_str()))
        });
        let (operation_id, operation_payload_hash) = operation_identity
            .ok_or_else(|| AppError::Other("human decision operation identity could not be generated".into()))?;
        validate_review_operation_identity(operation_id, operation_payload_hash)?;
        if let (Some("couch"), Some(reviewer)) = (audit_source, annotator) {
            let expected_payload_hash =
                review_operation_payload_hash(segment_id, &requested_action, &requested_transcript, reviewer);
            if operation_payload_hash != expected_payload_hash {
                return Err(AppError::Validation(
                    "Couch operation payload hash does not match its exact submitted request".into(),
                ));
            }
        }
        // All classifications, the pre-state snapshot, the revision CAS, pay and learning artifacts
        // live under one write reservation.  No renderer snapshot and no pre-transaction read is
        // authoritative for an irreversible human decision.
        let (commit, wrote) = self.with_full_sync(|| {
            let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
            Self::require_canonical_operation_namespace_on(&tx, operation_id)?;
            if supplied_desktop_operation {
                if let Some(commit) =
                    Self::desktop_human_decision_replay_on(&tx, operation_id, operation_payload_hash, segment_id)?
                {
                    let cleared_draft = if let Some(draft_revision) = review_draft_revision {
                        tx.execute(
                            "DELETE FROM review_drafts WHERE segment_id = ?1 AND base_revision = ?2",
                            params![segment_id, draft_revision],
                        )? > 0
                    } else {
                        false
                    };
                    if cleared_draft {
                        tx.commit()?;
                    } else {
                        tx.rollback()?;
                    }
                    return Ok((Some(commit), cleared_draft));
                }
            }
            let Some((prior, prior_revision, stored_content_hash)) = Self::decision_snapshot_on(&tx, segment_id)?
            else {
                tx.rollback()?;
                return Ok((None, false));
            };
            if expected_revision.is_some_and(|expected| expected != prior_revision) {
                tx.rollback()?;
                return Ok((None, false));
            }
            // The legacy phone writers exist only in the test build. Keep their broad historical
            // characterization useful without weakening the production boundary: synthesize a
            // structurally exact policy-4 Couch authority inside THIS SAME transaction. A failing
            // CAS/effect/ledger assertion rolls the fixture authority back with the decision. Real
            // production callers can reach this function only through the playback-proof-bearing
            // writer below, and the adversarial Couch endpoint tests exercise that path directly.
            #[cfg(test)]
            let synthetic_playback = if audit_source == Some("couch") && required_playback.is_none() {
                let reviewer = annotator
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| AppError::Validation("test Couch authority has no reviewer".into()))?;
                let audio_content_hash =
                    stored_content_hash.clone().filter(|value| is_canonical_audio_content_hash(value)).ok_or_else(
                        || AppError::Validation("test Couch authority has no canonical audio identity".into()),
                    )?;
                let (source_start_ms, source_end_ms) = canonical_source_span(prior.alignment_json.as_deref())
                    .ok_or_else(|| AppError::Validation("test Couch authority has no canonical source span".into()))?;
                if !source_span_matches_duration(source_start_ms, source_end_ms, prior.duration_ms) {
                    return Err(AppError::Validation("test Couch authority span disagrees with duration".into()));
                }
                let playback_receipt_id = uuid::Uuid::new_v4().to_string();
                let issued_at_ms = 1_i64;
                let synthetic_source_hash = "e".repeat(64);
                let (_, interval_union_sha256) = validate_desktop_playback_intervals(
                    &[DesktopPlaybackInterval { start_ms: 0, end_ms: prior.duration_ms }],
                    prior.duration_ms,
                )?;
                tx.execute(
                    "INSERT INTO desktop_playback_sessions_v4
                        (playback_receipt_id,media_grant_id,client_attempt_id,surface,
                         session_binding_sha256,grant_source_path_sha256,segment_id,segment_revision,
                         audio_content_hash,reviewer,clip_duration_ms,source_start_ms,source_end_ms,
                         issued_at_ms,expires_at_ms)
                     VALUES (?1,?2,?3,'couch',?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                    params![
                        playback_receipt_id,
                        uuid::Uuid::new_v4().to_string(),
                        uuid::Uuid::new_v4().to_string(),
                        "f".repeat(64),
                        synthetic_source_hash,
                        segment_id,
                        prior_revision,
                        audio_content_hash,
                        reviewer,
                        prior.duration_ms,
                        source_start_ms,
                        source_end_ms,
                        issued_at_ms,
                        issued_at_ms + 60_000,
                    ],
                )?;
                tx.execute(
                    "INSERT INTO desktop_playback_intervals_v4
                        (playback_receipt_id,ordinal,start_ms,end_ms,observed_at_ms)
                     VALUES (?1,0,0,?2,?3)",
                    params![playback_receipt_id, prior.duration_ms, issued_at_ms],
                )?;
                tx.execute(
                    "INSERT INTO playback_receipts
                        (segment_id,segment_revision,audio_fingerprint,reviewer,session_id,
                         started_at_ms,played_ms,clip_duration_ms,coverage_ratio,policy_version,
                         source_start_ms,source_end_ms,authority_session_id,interval_union_sha256)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?7,1.0,4,?8,?9,?5,?10)",
                    params![
                        segment_id,
                        prior_revision,
                        audio_content_hash,
                        reviewer,
                        playback_receipt_id,
                        issued_at_ms,
                        prior.duration_ms,
                        source_start_ms,
                        source_end_ms,
                        interval_union_sha256,
                    ],
                )?;
                Some(PlaybackDecisionProof {
                    segment_revision: prior_revision,
                    audio_content_hash,
                    source_start_ms,
                    source_end_ms,
                    authority_session_id: Some(playback_receipt_id),
                    source_lease: None,
                })
            } else {
                None
            };
            #[cfg(not(test))]
            let synthetic_playback: Option<PlaybackDecisionProof> = None;
            let required_playback = required_playback.or(synthetic_playback.as_ref());
            if let Some(limit) = decision_limit {
                let Some(reviewer) = annotator else {
                    return Err(AppError::Validation("controlled-review decision has no reviewer".into()));
                };
                enforce_review_action_limit_on(&tx, reviewer, limit)?;
            }
            if let Some(proof) = required_playback {
                if proof.segment_revision != prior_revision
                    || !is_canonical_audio_content_hash(&proof.audio_content_hash)
                    || proof.source_start_ms < 0
                    || proof.source_end_ms <= proof.source_start_ms
                    || stored_content_hash.as_deref() != Some(proof.audio_content_hash.as_str())
                {
                    tx.rollback()?;
                    return Ok((None, false));
                }
                let evidence_is_sufficient = if let Some(playback_receipt_id) = proof.authority_session_id.as_deref() {
                    let issued_source_hash: Option<String> = tx
                        .query_row(
                            "SELECT grant_source_path_sha256
                               FROM desktop_playback_sessions_v4
                              WHERE playback_receipt_id=?1 AND segment_id=?2",
                            params![playback_receipt_id, segment_id],
                            |row| row.get(0),
                        )
                        .optional()?;
                    let synthetic_authority = synthetic_playback.as_ref().is_some_and(|synthetic| {
                        synthetic.authority_session_id.as_deref() == Some(playback_receipt_id)
                    });
                    let current_source_hash = if synthetic_authority {
                        issued_source_hash
                            .clone()
                            .ok_or_else(|| AppError::Validation("test Couch authority has no source identity".into()))?
                    } else {
                        canonical_grant_source_path_sha256(Path::new(&prior.audio_path))?
                    };
                    let leased_source_matches = synthetic_authority
                        || proof.source_lease.as_ref().is_some_and(|lease| {
                            lease.audio_content_hash == proof.audio_content_hash
                                && canonical_grant_source_path_sha256(&lease.source_path)
                                    .is_ok_and(|lease_hash| lease_hash == current_source_hash)
                        });
                    if issued_source_hash.as_deref() != Some(current_source_hash.as_str()) || !leased_source_matches {
                        tx.rollback()?;
                        return Ok((None, false));
                    }
                    has_sufficient_desktop_playback_evidence_v4_on(
                        &tx,
                        segment_id,
                        proof.segment_revision,
                        &proof.audio_content_hash,
                        proof.source_start_ms,
                        proof.source_end_ms,
                        annotator,
                        playback_receipt_id,
                    )?
                } else {
                    has_sufficient_playback_evidence_on(
                        &tx,
                        segment_id,
                        proof.segment_revision,
                        &proof.audio_content_hash,
                        proof.source_start_ms,
                        proof.source_end_ms,
                        annotator,
                    )?
                };
                if !evidence_is_sufficient {
                    tx.rollback()?;
                    return Ok((None, false));
                }
                if audit_source == Some("couch") {
                    let reviewer = annotator.ok_or_else(|| {
                        AppError::Validation("Couch policy-4 playback consumption has no reviewer".into())
                    })?;
                    let authority_id = proof.authority_session_id.as_deref().ok_or_else(|| {
                        AppError::Validation(
                            "E_NO_PLAYBACK_EVIDENCE: Couch verdicts require one exact policy-4 authority".into(),
                        )
                    })?;
                    consume_couch_playback_authority_on(
                        &tx,
                        authority_id,
                        "canonical",
                        operation_id,
                        reviewer,
                        segment_id,
                        timestamp_ms.unwrap_or_else(|| {
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|duration| duration.as_millis() as i64)
                                .unwrap_or(1)
                                .max(1)
                        }),
                    )?;
                }
            }

            // Freeze the exact server-owned transcript that this decision classified.  The Couch
            // and desktop surfaces both serve the annotated draft when present, otherwise the raw
            // draft.  Keeping this immutable pre-state beside the effect lets restore re-derive
            // accept-vs-edit compensation instead of trusting the asserted payable action.
            let served_transcript = to_nfc(
                crate::corrections::loop0_draft_text(prior.annotated_transcript.as_deref(), &prior.raw_transcript)
                    .trim(),
            );
            if served_transcript.is_empty() {
                return Err(AppError::Validation(
                    "human decision refused: the server-owned served transcript is blank".into(),
                ));
            }

            // Pay follows what changed relative to the exact text served; corpus provenance may further
            // reclassify an accept of earlier human-authored text as an edit. Both are derived inside the
            // same transaction as the update.
            let compensation_action = if audit_source == Some("couch") {
                Self::phone_compensation_action_on(&tx, segment_id, decision, corrected_owned.as_deref())?
            } else {
                decision.to_string()
            };
            let (decision_owned, resolved_text) =
                Self::authoritative_decision_on(&tx, segment_id, decision, corrected_owned.as_deref())?;
            let decision = decision_owned.as_str();
            let corrected_owned = resolved_text.or(corrected_owned);
            let accept_snapshot = if decision == "accept" && corrected_owned.is_none() {
                Some(to_nfc(
                    crate::corrections::loop0_draft_text(prior.annotated_transcript.as_deref(), &prior.raw_transcript)
                        .trim(),
                ))
                .filter(|text| !text.is_empty())
            } else {
                None
            };
            let corrected_owned = corrected_owned.or(accept_snapshot);
            let corrected_transcript = corrected_owned.as_deref();
            if matches!(decision, "accept" | "edit") && corrected_transcript.is_none() {
                return Err(AppError::Validation(format!(
                    "Human {decision} decisions require non-blank accepted text"
                )));
            }
            let content_hash = stored_content_hash.filter(|hash| is_canonical_audio_content_hash(hash));
            if decision == "edit" && content_hash.is_none() {
                return Err(AppError::Validation(format!(
                    "human edit refused: segment {segment_id} has no canonical server-owned PCM content hash"
                )));
            }
            let human_verdict = human_verdict_for_decision(decision)?;
            let rejected_learning_transcript = if decision == "edit" {
                corrected_transcript.and_then(|fix| {
                    rejected_transcript_for_learning(
                        fix,
                        &[
                            prior.verdict_transcript.clone(),
                            prior.annotated_transcript.clone(),
                            prior.normalized_transcript.clone(),
                            Some(prior.raw_transcript.clone()),
                        ],
                    )
                })
            } else {
                None
            };
            let wrong_side = (decision == "edit")
                .then(|| rejected_learning_transcript.clone().unwrap_or_else(|| prior.raw_transcript.clone()));
            let finalized_text =
                crate::corrections::loop0_draft_text(prior.annotated_transcript.as_deref(), &prior.raw_transcript)
                    .to_string();
            let confidence_reference = match decision {
                "edit" => corrected_transcript.map(str::to_string),
                "accept" => corrected_transcript.map(str::to_string),
                _ => None,
            };
            type MemoryOutcomeUpdate = (String, String, String, crate::corrections::MemoryOutcome);
            let confidence_updates: Vec<MemoryOutcomeUpdate> = if !prior.is_gold {
                if let Some(reference) = confidence_reference.as_deref() {
                    let memories = Self::load_correction_memories_on(&tx)?;
                    crate::corrections::classify_memory_outcomes(
                        &finalized_text,
                        reference,
                        &memories,
                        &crate::corrections::FiringConfig::default(),
                    )
                    .into_iter()
                    .map(|(index, outcome)| {
                        let memory = &memories[index];
                        (memory.slot_key.clone(), memory.wrong_token.clone(), memory.human_token.clone(), outcome)
                    })
                    .collect()
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };

            let changed = tx.execute(
                "UPDATE speech_segments
             SET human_decision     = ?2,
                 verdict            = ?3,
                 verdict_transcript = COALESCE(?4, verdict_transcript),
                 escalated          = 0,
                 reviewed_by        = ?5,
                 annotated_transcript = CASE WHEN ?6 THEN COALESCE(?4, annotated_transcript)
                                             ELSE annotated_transcript END,
                 verified           = CASE WHEN ?6 THEN 1 ELSE verified END,
                 corrected_at       = datetime('now'),
                 updated_at         = datetime('now')
             WHERE id = ?1
               AND review_revision = ?7",
                params![segment_id, decision, human_verdict, corrected_transcript, annotator, finalize, prior_revision],
            )?;
            if changed == 0 {
                tx.rollback()?;
                return Ok((None, false));
            }
            let decided_revision: i64 = tx.query_row(
                "SELECT review_revision FROM speech_segments WHERE id = ?1",
                params![segment_id],
                |row| row.get(0),
            )?;

            if decided_revision != prior_revision + 1 {
                return Err(AppError::Other("human decision did not advance exactly one review revision".into()));
            }
            let Some((post_decision, post_revision, _)) = Self::decision_snapshot_on(&tx, segment_id)? else {
                return Err(AppError::Other("segment disappeared after its human decision update".into()));
            };
            if post_revision != decided_revision {
                return Err(AppError::Other("human decision post-state revision drifted before effect capture".into()));
            }
            let decision_transcript = if decision == "reject" {
                None
            } else {
                Some(
                    post_decision
                        .verdict_transcript
                        .as_deref()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .ok_or_else(|| AppError::Other("accept/edit decision has no exact retained transcript".into()))?
                        .to_string(),
                )
            };
            let decision_corrected_at = post_decision
                .corrected_at
                .clone()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| AppError::Other("human decision has no exact corrected_at post-state".into()))?;
            let mut review_event_id = None;
            if let (Some(source), Some(who)) = (audit_source, annotator) {
                let event_ts = timestamp_ms.unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0)
                });
                tx.execute(
                    "INSERT INTO review_events
                    (segment_id, reviewer, action, compensation_action, source, timestamp_ms,
                     duration_ms, operation_id, operation_payload_hash, requested_action,
                     requested_transcript, served_transcript, served_revision, app_git_sha,
                     playback_guard_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6,
                         (SELECT duration_ms FROM speech_segments WHERE id = ?1), ?7, ?8, ?9, ?10,
                          ?11, ?12, ?13, ?14)",
                    params![
                        segment_id,
                        who,
                        decision,
                        compensation_action,
                        source,
                        event_ts,
                        operation_id,
                        operation_payload_hash,
                        requested_action,
                        requested_transcript,
                        &served_transcript,
                        prior_revision,
                        crate::GIT_SHA,
                        if required_playback.and_then(|proof| proof.authority_session_id.as_ref()).is_some() {
                            "interval-authority-v4"
                        } else {
                            "content-hash-raw-counter-v3"
                        },
                    ],
                )?;
                let event_id = tx.last_insert_rowid();
                Self::append_review_compensation_tx(
                    &tx,
                    event_id,
                    segment_id,
                    who,
                    source,
                    &compensation_action,
                    decision,
                    Some(decided_revision),
                )?;
                review_event_id = Some(event_id);
            }

            let effect_source = audit_source.unwrap_or("desktop");
            let effect_reviewer = if audit_source == Some("couch") { annotator } else { None };
            let effect_operation_id = (effect_source == "desktop").then_some(operation_id);
            let effect_operation_payload_hash = (effect_source == "desktop").then_some(operation_payload_hash);
            let effect_requested_action = (effect_source == "desktop").then_some(requested_action.as_str());
            let effect_requested_transcript =
                (effect_source == "desktop").then_some(desktop_requested_transcript.as_deref()).flatten();
            let desktop_review_contract_version = review_draft_revision.is_some().then_some(1_i64);
            let playback_authority_session_id =
                if desktop_review_contract_version == Some(1) || effect_source == "couch" {
                    Some(required_playback.and_then(|playback| playback.authority_session_id.as_deref()).ok_or_else(
                        || AppError::Validation("review effect has no exact policy-4 playback authority".into()),
                    )?)
                } else {
                    None
                };
            tx.execute(
                "INSERT INTO human_decision_effect_events
                 (review_event_id, segment_id, reviewer, source, operation_id,
                  operation_payload_hash, action, served_transcript, decision_transcript,
                  decision_annotated_transcript, decision_verified, decision_corrected_at,
                  decision_rationale, requested_action, requested_transcript, requested_timestamp_ms, prior_revision,
                  decision_revision, prior_verified, prior_annotated_transcript, prior_verdict,
                  prior_verdict_transcript, prior_rationale, prior_escalated, prior_human_decision,
                  prior_corrected_at, prior_reviewed_by, desktop_review_contract_version,
                  playback_authority_session_id)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                      ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                      ?28, ?29)",
                params![
                    review_event_id,
                    segment_id,
                    effect_reviewer,
                    effect_source,
                    effect_operation_id,
                    effect_operation_payload_hash,
                    decision,
                    &served_transcript,
                    decision_transcript,
                    post_decision.annotated_transcript,
                    post_decision.verified as i32,
                    decision_corrected_at,
                    post_decision.rationale,
                    effect_requested_action,
                    effect_requested_transcript,
                    desktop_requested_timestamp_ms,
                    prior_revision,
                    decided_revision,
                    prior.verified as i32,
                    prior.annotated_transcript,
                    prior.verdict,
                    prior.verdict_transcript,
                    prior.rationale,
                    prior.escalated as i32,
                    prior.human_decision,
                    prior.corrected_at,
                    prior.reviewed_by,
                    desktop_review_contract_version,
                    playback_authority_session_id,
                ],
            )?;
            let effect_event_id = tx.last_insert_rowid();

            let genuine_edit = wrong_side
                .as_deref()
                .zip(corrected_transcript)
                .filter(|(wrong, fix)| learning_text_key(wrong) != learning_text_key(fix));
            if !prior.is_gold && genuine_edit.is_some() {
                if let (Some(wrong), Some(fix)) = (rejected_learning_transcript.as_deref(), corrected_transcript) {
                    tx.execute(
                        "INSERT INTO agent_examples
                        (id, segment_id, wrong_transcript, human_fix, source, verified_by_human,
                         effect_event_id)
                     VALUES (?1, ?2, ?3, ?4, 'human', 1, ?5)",
                        params![uuid::Uuid::new_v4().to_string(), segment_id, wrong, fix, effect_event_id],
                    )?;
                }
            }
            if let Some((wrong, fix)) = genuine_edit {
                tx.execute(
                    "INSERT INTO corrections
                    (id, segment_id, audio_content_hash, raw_hypothesis, human_fix, jury_verdict,
                     model_version_id, reviewer_id, effect_event_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        uuid::Uuid::new_v4().to_string(),
                        segment_id,
                        content_hash,
                        wrong,
                        fix,
                        prior.verdict,
                        prior.model_version_id,
                        effect_reviewer,
                        effect_event_id,
                    ],
                )?;
            }

            // One immutable contribution row per (decision effect, memory). New memory baselines never
            // mutate; Undo makes their contribution disappear through the effective-effect view.
            let mut contributions: HashMap<String, (i64, i64, i64, bool)> = HashMap::new();
            if !prior.is_gold {
                if let Some((wrong, fix)) = genuine_edit {
                    let mut seen = std::collections::HashSet::new();
                    for memory in crate::corrections::extract_substitution_memories(wrong, fix) {
                        if !seen.insert((
                            memory.slot_key.clone(),
                            memory.wrong_token.clone(),
                            memory.human_token.clone(),
                        )) {
                            continue;
                        }
                        tx.execute(
                            "INSERT INTO correction_memory
                        (id, wrong_token, human_token, slot_key, phonetic_key, source_segment,
                         model_version_id, confidence, hit_count, confirm_count, override_count,
                         last_fired_at, legacy_seed)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 0, 0, NULL, 0)
                     ON CONFLICT(slot_key, wrong_token, human_token) DO NOTHING",
                            params![
                                uuid::Uuid::new_v4().to_string(),
                                memory.wrong_token,
                                memory.human_token,
                                memory.slot_key,
                                memory.phonetic_key,
                                segment_id,
                                prior.model_version_id,
                                crate::corrections::beta_confidence(0, 0),
                            ],
                        )?;
                        let memory_id: String = tx.query_row(
                            "SELECT id FROM correction_memory
                      WHERE slot_key = ?1 AND wrong_token = ?2 AND human_token = ?3",
                            params![memory.slot_key, memory.wrong_token, memory.human_token],
                            |row| row.get(0),
                        )?;
                        contributions.entry(memory_id).or_insert((0, 0, 0, false)).0 = 1;
                    }
                }
            }
            for (slot_key, wrong_token, human_token, outcome) in confidence_updates {
                let Some(memory_id) = tx
                    .query_row(
                        "SELECT id FROM correction_memory
                      WHERE slot_key = ?1 AND wrong_token = ?2 AND human_token = ?3",
                        params![slot_key, wrong_token, human_token],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                else {
                    return Err(AppError::Other(
                        "effective correction memory disappeared inside decision transaction".into(),
                    ));
                };
                let entry = contributions.entry(memory_id).or_insert((0, 0, 0, false));
                match outcome {
                    crate::corrections::MemoryOutcome::Confirm => entry.1 = 1,
                    crate::corrections::MemoryOutcome::Override => entry.2 = 1,
                    crate::corrections::MemoryOutcome::Neutral => continue,
                }
                entry.3 = true;
            }
            for (memory_id, (capture, confirm, override_delta, fired)) in contributions {
                tx.execute(
                    "INSERT INTO correction_memory_contributions
                    (effect_event_id, memory_id, capture_delta, confirm_delta, override_delta, fired_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, CASE WHEN ?6 = 1 THEN datetime('now') END)",
                    params![effect_event_id, memory_id, capture, confirm, override_delta, fired as i32],
                )?;
            }

            if let Some(ts_ms) = timestamp_ms {
                tx.execute(
                    "INSERT INTO decision_log (segment_id, decision_type, timestamp_ms, human_decision, created_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                    params![segment_id, decision, ts_ms, decision],
                )?;
            }

            let Some((segment, authoritative_revision, _)) = Self::decision_snapshot_on(&tx, segment_id)? else {
                return Err(AppError::Other("segment disappeared inside human decision transaction".into()));
            };
            if authoritative_revision != decided_revision {
                return Err(AppError::Other("human decision authoritative row revision drifted before commit".into()));
            }

            if let Some(draft_revision) = review_draft_revision {
                tx.execute(
                    "DELETE FROM review_drafts WHERE segment_id = ?1 AND base_revision = ?2",
                    params![segment_id, draft_revision],
                )?;
            }

            tx.commit()?;
            Ok((
                Some(HumanDecisionCommit {
                    effect_event_id,
                    segment_id: segment_id.to_string(),
                    effective_action: decision.to_string(),
                    prior_revision,
                    decided_revision,
                    segment,
                }),
                true,
            ))
        })?;
        if wrote {
            self.track_write()?;
        }
        Ok(commit)
    }

    /// Load all LOOP-0 correction memories for the firing rule. `apply_memories` applies the
    /// confidence / hit-count / phonetic gates itself, so every stored row is returned here.
    pub fn load_correction_memories(&self) -> AppResult<Vec<crate::corrections::MemoryEntry>> {
        Self::load_correction_memories_on(&self.conn)
    }

    /// Return escalated segments ordered riskiest-first (lowest agreement_score).
    /// The escalated clips awaiting a human, narrowed to the active voice focus when there is one.
    ///
    /// `focus` is the same allow-list `pending_segment_ids_focused` and `get_segments_page_focused`
    /// take: `None` is the whole backlog, `Some(set)` only these ids. It is a parameter and not an
    /// internal read for the same reason as the others — the policy is resolved once, fail-closed, at
    /// the command boundary (`voice_focus::resolve`).
    pub fn get_escalation_queue(
        &self,
        limit: usize,
        focus: Option<&std::collections::HashSet<String>>,
    ) -> AppResult<Vec<SpeechSegment>> {
        let mut binds: Vec<Value> = vec![Value::Integer(limit as i64)];
        let focus_clause = match focus {
            Some(set) => {
                let mut ids: Vec<&str> = set.iter().map(String::as_str).collect();
                ids.sort_unstable();
                let ids_json = serde_json::to_string(&ids)
                    .map_err(|e| AppError::Validation(format!("Could not encode voice-focus ids: {e}")))?;
                binds.push(Value::Text(ids_json));
                format!(" AND id IN (SELECT value FROM json_each(?{}))", binds.len())
            }
            None => String::new(),
        };
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS}
             FROM speech_segments
             WHERE escalated = 1
               AND (human_decision IS NULL OR human_decision = ''){focus_clause}
             ORDER BY COALESCE(agreement_score, 0.5) ASC, id ASC
             LIMIT ?1"
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), Self::map_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}
