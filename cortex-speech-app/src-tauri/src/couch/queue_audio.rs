use super::*;

/// The review text shown/edited on the phone: same precedence as the desktop editor
/// (human-curated annotated transcript when present, else the raw draft).
///
/// DELIBERATELY NOT `quality::effective_transcript`, even though that is the canonical VERBATIM LAW
/// accessor and this looks like a duplicate of it. `effective_transcript` puts `verdict_transcript`
/// first for a human-decided clip — and on a SPOT-CHECK clip that field holds the ANSWER KEY. The
/// listening QC works precisely by serving a already-answered clip with its RAW, known-wrong draft;
/// showing the stored answer instead would hand every reviewer the solution and auto-pass every
/// check, silently. Tried 2026-08-15; caught by
/// `every_decision_lands_in_the_append_only_audit_trail`, which saw the spot check reclassify from
/// "edit" to "accept" because the submitted text then matched what was served.
///
/// Phone finalization now shares the human-decision transaction, so a correction cannot exist in
/// `verdict_transcript` while the row remains pending. This precedence remains separate solely to
/// protect blinded spot checks from seeing their answer key.
pub(super) fn review_text(seg: &SpeechSegment) -> String {
    seg.annotated_transcript.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| seg.raw_transcript.clone())
}

/// Was this clip MEASURED to span a turn between two people? (Migration v47.)
///
/// The reviewer needs this before they decide, not after. Chunks are cut on silence and labelled
/// afterwards, so a clip holding two speakers still shows exactly one `SPEAKER_xx` — the phone would
/// otherwise present a two-speaker clip as ordinary work, and "Looks good" would walk it into a
/// single-speaker corpus with an authoritative-looking wrong label attached.
///
/// `None` (not measured) is FALSE here, and the page shows nothing rather than "one speaker". Absence
/// of a measurement is not evidence of a single speaker, and the badge never claims it is.
pub(super) fn holds_a_speaker_change(seg: &SpeechSegment) -> bool {
    seg.speaker_change_score.is_some_and(|s| (s as f32) < crate::diarization::SPEAKER_CHANGE_THRESHOLD)
}

pub(super) fn build_pilot_decision_limit(
    policy: &crate::review_pilot::ReviewPilotPolicy,
) -> Result<ReviewDecisionLimit, String> {
    ReviewDecisionLimit::new(
        policy.after_review_event_id,
        policy.max_total_corpus_actions,
        policy.reviewers.iter().map(|reviewer| (reviewer.name.clone(), reviewer.max_corpus_actions)).collect(),
    )
    .map_err(|error| error.to_string())
}

/// Re-read and compare the operating file with the snapshot bound to this session. A live edit is a
/// pause, not a hot reset: Stop + Start is the explicit boundary that can authorize a new baseline.
pub(super) fn active_pilot_policy(
    reviewer: &str,
    state: &Mutex<CouchState>,
) -> Result<Option<crate::review_pilot::ReviewPilotPolicy>, String> {
    let (data_dir, bound, live_names) = {
        let st = lock_state(state);
        let names: Vec<String> = if st.pairing_codes.is_empty() {
            st.reviewers.values().cloned().collect()
        } else {
            st.pairing_codes.values().cloned().collect()
        };
        (st.session_store.as_ref().map(|(dir, _)| dir.clone()), st.pilot_policy.clone(), names)
    };
    let current = match data_dir.as_deref() {
        Some(dir) => crate::review_pilot::load(dir)?,
        None => None,
    };
    if current != bound {
        return Err(
            "controlled review policy changed during this session; review is paused until the owner stops and restarts it"
                .to_string(),
        );
    }
    if let Some(policy) = current.as_ref() {
        if !policy.matches_session(&live_names) {
            return Err(
                "controlled review pilot roster changed; review is paused until exactly the two authorized reviewers are restored"
                    .to_string(),
            );
        }
        if policy.cap_for(reviewer).is_none() {
            return Err("this reviewer is not authorized for the controlled review pilot".to_string());
        }
    }
    Ok(current)
}

/// Re-read the database-owned sequential campaign and compare it with the policy bound at Start.
/// A SQL edit or focus-file change while paid work is live is a pause, never a hot authorization
/// change. The reviewer check is repeated at every request boundary so a retained cookie cannot be
/// used outside the single-reviewer first pass.
pub(super) fn active_campaign_policy(
    db: &Database,
    reviewer: &str,
    state: &Mutex<CouchState>,
) -> Result<Option<crate::review_campaign::SequentialReviewCampaign>, String> {
    let (bound, pool_bound, data_dir, live_names) = {
        let guard = lock_state(state);
        let names: Vec<String> = if guard.pairing_codes.is_empty() {
            guard.reviewers.values().cloned().collect()
        } else {
            guard.pairing_codes.values().cloned().collect()
        };
        (
            guard.campaign_policy.clone(),
            guard.pool_policy.clone(),
            guard.session_store.as_ref().map(|(dir, _)| dir.clone()),
            names,
        )
    };
    if pool_bound.is_some() {
        return Ok(None);
    }
    let current = crate::review_campaign::load(db)?;
    if current != bound {
        return Err(
            "sequential review campaign changed during this session; review is paused until the owner stops and restarts it"
                .to_string(),
        );
    }
    if let Some(policy) = current.as_ref() {
        if !policy.matches_reviewer(reviewer)
            || live_names.len() != 1
            || !live_names.iter().all(|name| policy.matches_reviewer(name))
        {
            return Err(format!("this reviewer is outside the active {} campaign phase", policy.phase().as_str()));
        }
        let dir = data_dir
            .as_deref()
            .ok_or_else(|| "sequential review campaign has no durable data directory".to_string())?;
        crate::review_campaign::validate_focus(dir, policy)?;
    }
    Ok(current)
}

/// Re-prove the immutable pool registry on every queue/decision boundary. Membership tables are
/// append-only and digest-bound, so equality with the Start snapshot proves that reviewers are still
/// playing against exactly the same voice-organized clip set.
pub(super) fn active_pool_policy(
    db: &Database,
    state: &Mutex<CouchState>,
) -> Result<Option<crate::review_pool::ReviewPool>, String> {
    let bound = { lock_state(state).pool_policy.clone() };
    let matches = match bound.as_ref() {
        Some(policy) => crate::review_pool::registry_matches(db, policy)?,
        None => crate::review_pool::load(db)?.is_none(),
    };
    if !matches {
        return Err("review pool changed during this session; review is paused until the owner stops and restarts it"
            .to_string());
    }
    Ok(bound)
}

/// Remaining review-action slots for this reviewer, validated against the global count as well.
/// Returns an error for unauthorized/over-cap history rather than hiding it behind zero.
pub(super) fn pilot_remaining_slots(
    db: &Database,
    reviewer: &str,
    policy: &crate::review_pilot::ReviewPilotPolicy,
) -> Result<usize, String> {
    let limit = build_pilot_decision_limit(policy)?;
    let progress = db.review_decision_progress(&limit).map_err(|error| error.to_string())?;
    let reviewer_cap =
        policy.cap_for(reviewer).ok_or_else(|| "reviewer is outside the controlled review pilot".to_string())?;
    let reviewer_count = progress
        .by_reviewer
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(reviewer.trim()))
        .map(|(_, count)| *count)
        .unwrap_or(0);
    let reviewer_remaining = reviewer_cap.saturating_sub(reviewer_count);
    let total_remaining = policy.max_total_corpus_actions.saturating_sub(progress.total_review_actions);
    usize::try_from(reviewer_remaining.min(total_remaining))
        .map_err(|_| "controlled review pilot remaining count is invalid".to_string())
}

/// Number of distinct hidden keys this bounded pilot may ever mint for one reviewer.
///
/// This is derived from the same action cap and cadence that bound the queue.  Unlike the ordinary
/// long-running campaign, the pilot does not need the independent three-key launch floor: ten
/// actions at one check per eight require exactly two keys.  The served count is persisted so a page
/// reload or process restart cannot reset the budget and quietly consume a third key.
pub(super) fn pilot_spot_check_quota(
    policy: &crate::review_pilot::ReviewPilotPolicy,
    reviewer: &str,
) -> Result<usize, String> {
    let cap = policy.cap_for(reviewer).ok_or_else(|| "reviewer is outside the controlled review pilot".to_string())?;
    let cap = usize::try_from(cap).map_err(|_| "controlled review pilot action cap is invalid".to_string())?;
    Ok(cap.div_ceil(SPOT_CHECK_EVERY))
}

/// Plan one pilot refill without ever minting more distinct keys than `quota`.
///
/// Outstanding keys are re-served first.  They do not consume a second quota unit; this is what
/// keeps a reload from either evading the hidden checks or burning a fresh key.  Completed/declined
/// checks stay in `distinct_served`, so later refills do not over-test the bounded ten-action sample.
pub(super) fn pilot_spot_check_plan(
    work_items: usize,
    quota: usize,
    distinct_served: usize,
    outstanding: usize,
    force_complete: bool,
) -> (usize, usize) {
    let desired = if force_complete { quota } else { work_items.div_ceil(SPOT_CHECK_EVERY).min(quota) };
    let resend = desired.min(outstanding);
    let fresh = desired.saturating_sub(resend).min(quota.saturating_sub(distinct_served));
    (resend, fresh)
}

/// Rehydrate pilot checks that were already handed to this reviewer but have not been answered.
/// Every returned row is revalidated against the current key, focus, dialect and audio state.  A
/// previously minted key that silently ceased to be valid is a hard error: replacing it would exceed
/// the proven two-key capacity, while pretending it remains a check would corrupt reviewer scoring.
pub(super) fn pilot_outstanding_spot_checks(
    db: &Database,
    policy_sha256: &str,
    after_review_event_id: i64,
    reviewer: &str,
    ids: &[String],
    allowed_dialects: Option<&[String]>,
    focus: Option<&HashSet<String>>,
) -> Result<Vec<SpeechSegment>, String> {
    let rows = db.get_segments_by_ids_with_revisions(ids).map_err(|error| error.to_string())?;
    let mut by_id: HashMap<&str, &SpeechSegment> =
        rows.iter().map(|(segment, _)| (segment.id.as_str(), segment)).collect();
    let mut out = Vec::new();
    for id in ids {
        if db
            .review_pilot_hidden_key_resolved(policy_sha256, after_review_event_id, reviewer, id)
            .map_err(|error| error.to_string())?
        {
            continue;
        }
        let Some(segment) = by_id.remove(id.as_str()) else {
            return Err(format!("previously served hidden-check key {id} no longer exists"));
        };
        let expected = crate::quality::human_verified_text(segment)
            .ok_or_else(|| format!("previously served hidden-check key {id} no longer has a human answer"))?;
        if !segment.verified
            || !Path::new(&segment.audio_path).is_file()
            || !crate::dialect::reviewer_may_judge(allowed_dialects, &segment.audio_path)
            || focus.is_some_and(|allowed| !allowed.contains(id))
            || crate::normalizer::learning_text_key(expected)
                == crate::normalizer::learning_text_key(&segment.raw_transcript)
        {
            return Err(format!("previously served hidden-check key {id} is no longer eligible"));
        }
        out.push(segment.clone());
    }
    Ok(out)
}

pub(super) type ReviewerPolicy = (Option<Vec<String>>, Option<Arc<HashSet<String>>>);
pub(super) type SpotCheckSelection = Result<Option<(Vec<SpeechSegment>, Option<usize>)>, String>;

pub(super) fn lock_pilot_decision_commit() -> std::sync::MutexGuard<'static, ()> {
    PILOT_DECISION_COMMIT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Resolve the policy that authorizes one reviewer's queue and decisions.
///
/// This is deliberately shared by BOTH boundaries. Filtering only `/api/queue` is not authorization:
/// a phone can retain an id in its outbox, and anyone holding a valid reviewer credential can POST a
/// known id without fetching a queue first. Re-reading here preserves the policy files' hot-reload
/// contract and makes a change effective on the next decision as well as the next queue fetch.
pub(super) fn reviewer_policy(reviewer: &str, state: &Mutex<CouchState>) -> Result<ReviewerPolicy, String> {
    let (dir, pool_focus) = {
        let guard = lock_state(state);
        (
            guard.session_store.as_ref().map(|(data_dir, _db_path)| data_dir.clone()),
            guard.pool_policy.as_ref().map(crate::review_pool::ReviewPool::segment_ids),
        )
    };
    let allowed_dialects = match dir.as_ref().map(|d| crate::dialect::load_roster(d)) {
        Some(Err(e)) => return Err(format!("policy file broken, no clips served: {e}")),
        // Matched the way the session layer matches names (trim + ASCII case), never an exact
        // HashMap::get — a roster key that binds nobody is the wrong-dialect incident returning.
        Some(Ok(roster)) => crate::dialect::allowed_for(&roster, reviewer).cloned(),
        None => None,
    };
    // Once the database-bound pool is active it is the single focus authority. The legacy one-voice
    // JSON remains historical evidence but cannot silently narrow this three-voice pool back to Lamo.
    let focus = match pool_focus {
        Some(ids) => Some(ids),
        None => crate::voice_focus::resolve(dir.as_deref())?,
    };
    Ok((allowed_dialects, focus))
}

pub(super) fn reviewer_policy_allows(
    allowed_dialects: Option<&[String]>,
    focus: Option<&HashSet<String>>,
    segment: &SpeechSegment,
) -> bool {
    crate::dialect::reviewer_may_judge(allowed_dialects, &segment.audio_path)
        && focus.map_or(true, |ids| ids.contains(&segment.id))
}

/// Pending (unverified) clips, oldest first — the same "work that needs doing" the desktop queue leads
/// with — MINUS anything another reviewer currently holds, and leased to this reviewer on the way out.
///
/// The lease is what makes two phones safe: without it both are handed the same head-of-queue clips and
/// race to decide them (duplicated effort at best, one reviewer's verdict silently overwriting the
/// other's at worst). Batches are small so the first reviewer to load cannot lease the entire backlog,
/// and leases expire so a closed browser tab never strands work.
pub(super) fn api_queue(db: &Database, reviewer: &str, state: &Mutex<CouchState>) -> Reply {
    // P1.3: IDs, not whole rows. This walked EVERY pending segment's full record — transcript,
    // alignment JSON, evidence JSON, the lot — to hand out at most QUEUE_BATCH of them. The counts
    // below genuinely need every pending row (they depend on in-memory LEASE state, which no SQL
    // aggregate can see), but a row that is only being COUNTED does not need anything except its id.
    // The <= 25 clips actually served are hydrated after the lock is released.
    // Dialect roster, re-read per fetch so the owner can change who reviews what without restarting
    // the app. A reviewer with no entry is unrestricted, exactly as before this existed.
    // Voice focus (owner instruction 2026-08-19): when `<data_dir>/voice_focus.json` names a set of
    // clips, every reviewer's queue narrows to it — so paid hours build the one speaker's set being
    // collected now instead of spreading across 34 h of mixed audio. Same hot-reload-per-fetch.
    //
    // A policy file that EXISTS but cannot be honoured is a 503, not a shrug (owner instruction
    // 2026-08-20 — present-but-broken fails CLOSED). Failing open here silently pointed every paid
    // reviewer at work the file was written to keep them off; the page treats >=500 as retryable and
    // shows the status, so the line stops loudly and the very next fetch after the owner fixes the
    // file works. A MISSING file is still "no restriction", exactly as before either file existed.
    let (allowed_dialects, focus) = match reviewer_policy(reviewer, state) {
        Err(e) => return err_reply(503, &e),
        Ok(policy) => policy,
    };
    let pilot_policy = match active_pilot_policy(reviewer, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    let pool_policy = match active_pool_policy(db, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    let campaign_policy = match active_campaign_policy(db, reviewer, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    if pilot_policy.is_some() && (campaign_policy.is_some() || pool_policy.is_some()) {
        return err_reply(503, "conflicting paid-review policies are active");
    }
    let pilot_slots = match pilot_policy.as_ref() {
        Some(policy) => match pilot_remaining_slots(db, reviewer, policy) {
            Ok(remaining) => Some(remaining),
            Err(error) => return err_reply(503, &format!("controlled review pilot is unavailable: {error}")),
        },
        None => None,
    };
    // Verify the full activity + weighted-pay view BEFORE leasing work or marking a hidden check as
    // served. If accounting is unhealthy, this request must leave no queue/session side effects.
    let accounting = match reviewer_accounting(db, reviewer) {
        Ok(accounting) => accounting,
        Err(error) => {
            tracing::error!("Couch Review queue paused because accounting is unavailable for {reviewer}: {error}");
            return err_reply(503, "Review is temporarily paused: reviewer accounting is unavailable");
        }
    };
    // SQLite is the lifetime authority for pilot hidden assignments. Import any remembered/in-memory
    // mirror first (legacy upgrade), then use only the transactionally bounded set for planning. An
    // empty mirror after a crash, Stop/Start, or snapshot recovery therefore cannot mint key three.
    let durable_pilot_check_ids = if let Some(policy) = pilot_policy.as_ref() {
        let policy_sha256 = match policy.policy_sha256() {
            Ok(digest) => digest,
            Err(error) => return err_reply(503, &format!("controlled review pilot is unavailable: {error}")),
        };
        let quota = match pilot_spot_check_quota(policy, reviewer) {
            Ok(quota) => quota,
            Err(error) => return err_reply(503, &format!("controlled review pilot is unavailable: {error}")),
        };
        let remembered_ids: Vec<String> = {
            let guard = lock_state(state);
            guard
                .pilot_spot_checks
                .iter()
                .filter(|(_, name)| name.eq_ignore_ascii_case(reviewer))
                .map(|(id, _)| id.clone())
                .collect()
        };
        match db.reserve_review_pilot_hidden_keys(
            &policy_sha256,
            policy.after_review_event_id,
            reviewer,
            &remembered_ids,
            quota,
        ) {
            Ok(ids) => Some(ids),
            Err(error) => {
                return err_reply(
                    503,
                    &format!("Review is temporarily paused: controlled hidden-check history is invalid ({error})"),
                );
            }
        }
    } else {
        None
    };
    let pending_result = match pool_policy.as_ref() {
        Some(pool) => crate::review_pool::pending_segment_ids(db, pool, reviewer, allowed_dialects.as_deref())
            .map_err(crate::error::AppError::Validation),
        None => match campaign_policy.as_ref().filter(|policy| policy.is_blinded_second_pass()) {
            Some(policy) => crate::review_campaign::independent_pending_segment_ids(db, policy)
                .map_err(crate::error::AppError::Validation),
            None => db.pending_segment_ids_focused(allowed_dialects.as_deref(), focus.as_deref()),
        },
    };
    let pending_ids = match pending_result {
        Ok(ids) => ids,
        Err(e) => return err_reply(500, &e.to_string()),
    };
    let pending_total = pilot_slots.map_or(pending_ids.len(), |remaining| pending_ids.len().min(remaining));
    // An empty queue means two very different things, and the page must not say "all clips reviewed"
    // for the second: everything really is done, OR this reviewer is restricted to a dialect that has
    // no work right now (today: everything playable is Hawleri, so a Sorani-only reviewer has none).
    // A VOICE FOCUS counts as a restriction here too (2026-08-20 hunt): with the queue narrowed to
    // one speaker and that set drained, an unrestricted reviewer used to be shown "all clips
    // reviewed 🎉" while thousands of pending clips sat outside the focus — a lie about the
    // library, told at the exact moment the owner would want to widen the focus.
    let restricted_and_empty =
        (allowed_dialects.is_some() || focus.is_some() || pilot_slots.is_some()) && pending_total == 0;
    let mut guard = lock_state(state);
    let now = guard.now();
    let mut serving: Vec<String> = Vec::new();
    let mut held_by_others = 0usize;
    let mut skipped_by_you = 0usize;
    let serving_limit = pilot_slots.map_or(QUEUE_BATCH, |remaining| QUEUE_BATCH.min(remaining));
    for id in pending_ids {
        // Someone else's live lease: skip it, but COUNT it. Our own is renewed below (a reviewer
        // reloading the page must get their own in-progress work back, not a fresh batch that
        // abandons it).
        if guard.holder(&id, now).is_some_and(|who| who != reviewer) {
            held_by_others += 1;
            continue;
        }
        // This reviewer already said they cannot judge this one (R4.4). Not theirs to be handed
        // again — but COUNTED, because it is still pending work somebody owes a verdict, and an
        // empty batch full of these must not draw "🎉 all clips reviewed".
        if guard.skipped.get(reviewer).is_some_and(|ids| ids.contains(&id)) {
            skipped_by_you += 1;
            continue;
        }
        if serving.len() >= serving_limit {
            continue; // keep counting what is left, but hand out no more this round
        }
        if let Some(pool) = pool_policy.as_ref() {
            if let Err(error) = pool.verify_audio_available(&id) {
                tracing::error!("Couch Review pool paused before lease because selected audio is unavailable: {error}");
                return err_reply(503, "Review is temporarily paused: selected audio is unavailable");
            }
        }
        guard.leases.insert(id.clone(), (reviewer.to_string(), now));
        serving.push(id);
    }
    // Drop leases that expired while their holder was away, so the map cannot grow without bound across
    // a long session. Cheap: the pending queue is the only thing that can be leased.
    guard.leases.retain(|_, (_, granted)| now.duration_since(*granted) < LEASE_TTL);
    // Snapshotted under the lock and handed to the spot-check selection below, which runs OUTSIDE it.
    // Spot checks are inserted after the loop above, so the skip filter in that loop never sees them —
    // without passing this along, a check somebody skipped was re-inserted into every batch forever.
    let skipped_by_me = guard.skipped.get(reviewer).cloned().unwrap_or_default();
    // A pilot key budget is DISTINCT-KEY based, not request based. Snapshot it under the same lock
    // that protects insertion so two concurrent reloads cannot both observe room for the final key.
    if let Some(ids) = durable_pilot_check_ids.as_ref() {
        for id in ids {
            let key = (id.clone(), reviewer.to_string());
            guard.pilot_spot_checks.insert(key.clone());
            guard.spot_checks.insert(key);
        }
    }
    let mut pilot_check_ids: Vec<String> = durable_pilot_check_ids.unwrap_or_else(|| {
        guard
            .pilot_spot_checks
            .iter()
            .filter(|(_, name)| name.eq_ignore_ascii_case(reviewer))
            .map(|(id, _)| id.clone())
            .collect()
    });
    pilot_check_ids.sort();
    pilot_check_ids.dedup();
    // A persisted session is the production paid-review path. Ephemeral test/dev callers preserve
    // their historical best-effort behaviour, while a real durable reviewer link must never receive
    // unmeasured work after its hidden-key pool runs dry.
    let require_complete_spot_checks = campaign_policy.is_none()
        && pool_policy.is_none()
        && guard.session_store.is_some()
        && (!guard.pairing_codes.is_empty() || !guard.reviewers.is_empty());
    drop(guard);

    // Hydrate ONLY what is being served — at most QUEUE_BATCH rows — and do it OUTSIDE the state lock,
    // which the whole-library read never was. `get_segments_by_ids` re-imposes its own global ordering,
    // so the rows are indexed by id and re-emitted in `serving` order; handing a reviewer the same
    // clips in a different order than they were leased would be a silent behaviour change.
    let rows = match db.get_segments_by_ids_with_revisions(&serving) {
        Ok(rows) => rows,
        Err(e) => return err_reply(500, &e.to_string()),
    };
    let by_id: std::collections::HashMap<&str, (&SpeechSegment, i64)> =
        rows.iter().map(|(segment, revision)| (segment.id.as_str(), (segment, *revision))).collect();
    // filter_map, not unwrap: a clip can be deleted between the id query and this fetch. Serving one
    // fewer clip is correct; panicking on a race in the reviewer's request path is not. Its lease simply
    // expires.
    let mut queue: Vec<serde_json::Value> = serving
        .iter()
        .filter_map(|id| by_id.get(id.as_str()))
        .map(|(s, revision)| {
            serde_json::json!({
                "id": s.id,
                // Alle's independent pass is genuinely blind: the backend serves the champion raw
                // draft even though speech_segments now contains Rubar's first-pass correction.
                // This is a data boundary, not a presentation hint.
                "text": if pool_policy.is_some()
                    || campaign_policy.as_ref().is_some_and(|policy| policy.is_blinded_second_pass()) {
                    s.raw_transcript.clone()
                } else {
                    review_text(s)
                },
                "durationMs": s.duration_ms,
                "speakerId": pool_policy
                    .as_ref()
                    .and_then(|pool| pool.voice_for(&s.id))
                    .map(str::to_string)
                    .or_else(|| s.speaker_id.clone()),
                "speakerChange": holds_a_speaker_change(s),
                // The row's change-fingerprint at serve time; the page echoes it on the decision so
                // a draft replaced by a background writer in between is refused, never recorded.
                "rowVersion": revision.to_string(),
                // A pre-pilot durable outbox has no baseline and is refused before any write.  The
                // page echoes this field exactly; it is authorization context, not a client claim.
                "pilotAfterReviewEventId": pilot_policy.as_ref().map(|policy| policy.after_review_event_id),
            })
        })
        .collect();

    // Salt the batch with spot checks (P2.1). They are NOT leased: two reviewers meeting the same
    // known-answer clip is the point — independent measurement — not a collision. They also carry no
    // marker of any kind in the payload, because a reviewer who can spot the test is not being tested.
    let pilot_catchup = pilot_policy.is_some() && pilot_slots == Some(0);
    if !queue.is_empty() || pilot_catchup {
        // CEILING, not floor. `len / EVERY` silently gives ZERO checks to any batch under EVERY
        // items — so a reviewer arriving late to a nearly-drained queue, or sharing a small backlog,
        // would never be measured at all. Rounding up guarantees at least one check in every non-empty
        // batch while holding the ~1-in-8 ratio wherever the batch is large enough for it to mean
        // something. A reviewer must not be able to coast just because their batches came out short.
        let work_len = queue.len();
        let cadence_wanted = work_len.div_ceil(SPOT_CHECK_EVERY);
        let selected: SpotCheckSelection = if campaign_policy.is_some() || pool_policy.is_some() {
            // These workflows use real independent reviews as their quality authority, not synthetic
            // or circular hidden keys. In pool mode every actual judgement must count toward the
            // clip's visible coverage; swallowing one as a hidden test would make that claim false.
            Ok(None)
        } else if let Some(policy) = pilot_policy.as_ref() {
            (|| {
                let quota = pilot_spot_check_quota(policy, reviewer)?;
                let policy_sha256 = policy.policy_sha256()?;
                let outstanding = pilot_outstanding_spot_checks(
                    db,
                    &policy_sha256,
                    policy.after_review_event_id,
                    reviewer,
                    &pilot_check_ids,
                    allowed_dialects.as_deref(),
                    focus.as_deref(),
                )?;
                let (resend, fresh_wanted) =
                    pilot_spot_check_plan(work_len, quota, pilot_check_ids.len(), outstanding.len(), pilot_catchup);
                let mut candidates: Vec<SpeechSegment> = outstanding.into_iter().take(resend).collect();
                if fresh_wanted > 0 {
                    // Already-minted pilot ids are either re-served above or permanently consumed by
                    // an answer/decline.  Excluding them here makes `fresh_wanted` mean fresh exactly.
                    let mut exclude = skipped_by_me.clone();
                    exclude.extend(pilot_check_ids.iter().cloned());
                    let fresh = db
                        .list_spot_check_candidates(
                            fresh_wanted,
                            reviewer,
                            &exclude,
                            allowed_dialects.as_deref(),
                            focus.as_deref(),
                        )
                        .map_err(|error| error.to_string())?;
                    if fresh.len() < fresh_wanted {
                        Err("genuine hidden-check capacity is below the controlled-pilot requirement".to_string())
                    } else {
                        candidates.extend(fresh.into_iter().map(|(segment, _)| segment));
                        Ok(Some((candidates, Some(quota))))
                    }
                } else {
                    Ok(Some((candidates, Some(quota))))
                }
            })()
        } else {
            match db.list_spot_check_candidates(
                cadence_wanted,
                reviewer,
                &skipped_by_me,
                allowed_dialects.as_deref(),
                focus.as_deref(),
            ) {
                Ok(candidates) if require_complete_spot_checks && candidates.len() < cadence_wanted => {
                    Err("genuine hidden-check capacity is below the required campaign coverage".to_string())
                }
                Ok(candidates) => Ok(Some((candidates.into_iter().map(|(segment, _)| segment).collect(), None))),
                Err(error) if require_complete_spot_checks => Err(format!("hidden-check selection failed: {error}")),
                Err(error) => {
                    tracing::warn!("Couch Review spot-check selection failed: {error}");
                    Ok(None)
                }
            }
        };
        match selected {
            Err(error) => {
                // These work ids were leased above but no batch is being served. Release only this
                // reviewer's batch so another request is not told the work is held while the paid
                // line is correctly paused on the same quality-capacity failure.
                release_unserved_leases(state, &serving, reviewer);
                return err_reply(503, &format!("Review is temporarily paused: {error}"));
            }
            Ok(Some((candidates, pilot_quota))) => {
                let wanted = candidates.len();
                // STAMP FIRST, before anything is reserved or recorded. The row stamp is a second,
                // fallible read that used to degrade to JSON null through `.ok().flatten()` — and a
                // work clip ALWAYS carries one, so a null `rowVersion` is a payload shape only a trap
                // clip can have: a fingerprint. It is also self-defeating, because the decide path
                // refuses a missing rowVersion ("reload this clip"), leaving the check undecidable and
                // its score unearnable. A failed read is a retryable 503 instead.
                let mut checks: Vec<(SpeechSegment, String)> = Vec::with_capacity(wanted);
                for segment in candidates {
                    match db.segment_row_stamp(&segment.id) {
                        Ok(Some(stamp)) => checks.push((segment, stamp)),
                        other => {
                            tracing::error!(
                                "Couch Review could not stamp hidden check {}; the batch was refused: {other:?}",
                                segment.id
                            );
                            release_unserved_leases(state, &serving, reviewer);
                            return err_reply(503, "Review is temporarily paused: a hidden check could not be stamped");
                        }
                    }
                }
                if let Some(quota) = pilot_quota {
                    let Some(policy) = pilot_policy.as_ref() else {
                        return err_reply(503, "Review is temporarily paused: pilot key has no active policy");
                    };
                    let policy_sha256 = match policy.policy_sha256() {
                        Ok(digest) => digest,
                        Err(error) => {
                            return err_reply(
                                503,
                                &format!("Review is temporarily paused: pilot policy identity failed ({error})"),
                            );
                        }
                    };
                    let candidate_ids: Vec<String> = checks.iter().map(|(segment, _)| segment.id.clone()).collect();
                    let authorized = match db.reserve_review_pilot_hidden_keys(
                        &policy_sha256,
                        policy.after_review_event_id,
                        reviewer,
                        &candidate_ids,
                        quota,
                    ) {
                        Ok(ids) => ids.into_iter().collect::<HashSet<_>>(),
                        Err(error) => {
                            release_unserved_leases(state, &serving, reviewer);
                            return err_reply(
                                503,
                                &format!("Review is temporarily paused: hidden-check reservation failed ({error})"),
                            );
                        }
                    };
                    if candidate_ids.iter().any(|id| !authorized.contains(id)) {
                        release_unserved_leases(state, &serving, reviewer);
                        return err_reply(503, "Review is temporarily paused: hidden-check reservation is incomplete");
                    }
                }
                // Serialize remembered-file mirror writes so an older cache snapshot cannot replace a
                // newer one. Pilot only: SQLite has already committed the pilot authority above, so
                // that file is restart convenience and may be rebuilt from the database. The ordinary
                // namespace reserves nothing durable — see the persist branch below.
                let _pilot_persist = pilot_quota.map(|_| lock_session_persist());
                let mut guard = lock_state(state);
                let mut grew = false;
                // Exactly the keys THIS request minted, so an unserved batch can put the set back the
                // way it found it. A key left behind for a clip the reviewer never received would make
                // a later ordinary serve of that same clip score as a hidden check.
                let mut minted: Vec<(String, String)> = Vec::new();
                for (idx, (seg, row_stamp)) in checks.into_iter().enumerate() {
                    let key = (seg.id.clone(), reviewer.to_string());
                    if guard.spot_checks.insert(key.clone()) {
                        grew = true;
                        minted.push(key.clone());
                    }
                    if pilot_quota.is_some() && guard.pilot_spot_checks.insert(key.clone()) {
                        grew = true;
                    }
                    // SPREAD EVENLY, and this is a fix, not a preference. The position used to be
                    // `((idx + 1) * SPOT_CHECK_EVERY).min(queue.len())`, and the comment above it
                    // claimed to interleave "rather than append in a run at the tail". Measured across a
                    // five-batch session: a spot check landed LAST in 5 of 5 batches. `wanted` is
                    // `div_ceil`, so a 25-clip batch asks for 4 checks while only three multiples of 8
                    // fall inside it (8, 16, 24) — the fourth computed 32, clamped to the end, and was
                    // appended every single time. A reviewer who noticed that the last clip of every
                    // batch is a trap could pass every test in a session by listening to one clip.
                    //
                    // Dividing the batch into `wanted + 1` gaps puts every check strictly inside it:
                    // 25 work clips and 4 checks give 5, 11, 17, 23 (the `+ idx` accounts for the
                    // earlier insertions having shifted everything after them). The `.min` is kept only
                    // so the index can never exceed the length — with this formula it does not bind, and
                    // Vec::insert panicking is not an acceptable way to find that out.
                    let at = ((idx + 1) * (work_len + 1) / (wanted + 1) + idx).min(queue.len());
                    queue.insert(
                        at,
                        serde_json::json!({
                            "id": seg.id,
                            // The RAW draft — the known-wrong one. Serving the corrected text would
                            // make the check unpassable-by-failing: there would be nothing to catch.
                            "text": seg.raw_transcript,
                            "durationMs": seg.duration_ms,
                            "speakerId": pool_policy
                                .as_ref()
                                .and_then(|pool| pool.voice_for(&seg.id))
                                .map(str::to_string)
                                .or_else(|| seg.speaker_id.clone()),
                            // Computed exactly as for work clips. A spot check that carried this
                            // field differently would be spottable, and a reviewer who can spot the
                            // test is not being tested.
                            "speakerChange": holds_a_speaker_change(&seg),
                            // Same field as work clips for the same indistinguishability reason —
                            // resolved above, never degraded to null.
                            "rowVersion": row_stamp,
                            "pilotAfterReviewEventId": pilot_policy.as_ref().map(|policy| policy.after_review_event_id),
                        }),
                    );
                }
                let pilot_snapshot = (pilot_quota.is_some() && grew).then(|| snapshot_session_save(&guard)).flatten();
                #[cfg(test)]
                let inject_pilot_save_failure = pilot_quota.is_some() && grew && guard.fail_session_persist;
                // Persist the assignment. Under the PILOT namespace this file is a mirror only — the
                // exact assignments are already FULL-synchronous in the durable pilot table reserved
                // above, and the next request or process start rehydrates the mirror from that
                // authority — so a failure is logged and the queue still goes out.
                //
                // The ordinary namespace has NO such reservation: this file is the only durable record
                // that these clips were served as checks. The app restarts 4-9 times a day, and a
                // restart rehydrates `spot_checks` from the stale file WITHOUT the pair — so the
                // reviewer's answer takes the `was_served_as_spot_check == false` path, 409s "already
                // reviewed", and the score is lost with nothing in the log to explain it. Refuse the
                // batch instead: a retryable 503 costs a reload, a silent unrecorded check costs a
                // measurement nobody can reconstruct.
                drop(guard);
                if grew {
                    let saved = if pilot_quota.is_some() {
                        #[cfg(test)]
                        if inject_pilot_save_failure {
                            Err("injected session persistence failure".to_string())
                        } else {
                            pilot_snapshot
                                .as_ref()
                                .ok_or_else(|| "controlled pilot has no durable session store".to_string())
                                .and_then(save_session_snapshot)
                        }
                        #[cfg(not(test))]
                        {
                            pilot_snapshot
                                .as_ref()
                                .ok_or_else(|| "controlled pilot has no durable session store".to_string())
                                .and_then(save_session_snapshot)
                        }
                    } else {
                        persist_session_state(state)
                    };
                    if let Err(error) = saved {
                        if pilot_quota.is_some() {
                            tracing::warn!(
                                "controlled hidden-check session mirror was not saved; SQLite remains authoritative: {error}"
                            );
                        } else {
                            tracing::error!(
                                "Couch Review refused a batch because its hidden-check assignment could not be saved: {error}"
                            );
                            {
                                let mut guard = lock_state(state);
                                for key in minted {
                                    guard.spot_checks.remove(&key);
                                }
                            }
                            release_unserved_leases(state, &serving, reviewer);
                            return err_reply(
                                503,
                                "Review is temporarily paused: the hidden-check assignment could not be saved",
                            );
                        }
                    }
                }
            }
            Ok(None) => {}
        }
    }
    // Record delivery only after every fallible hidden-check/durability gate above has passed. A
    // provisional lease is deliberately NOT enough: failed queue construction returns no ids to the
    // reviewer and therefore must not mint an audio authorization that `/api/renew` could resurrect.
    // `by_id` excludes rows deleted between the pending-id scan and hydration, so receipts are issued
    // only for ordinary work that is actually present in the successful response.
    {
        let mut guard = lock_state(state);
        for id in serving.iter().filter(|id| by_id.contains_key(id.as_str())) {
            guard.served_work.insert((id.clone(), reviewer.to_string()));
        }
    }
    // Two things travel with the queue, and both exist to stop the page from misleading its reviewer:
    //
    //   `reviewer` — WHO the server is recording these decisions as. Attribution a reviewer cannot see
    //   is attribution they cannot correct.
    //
    //   `heldByOthers` — how much pending work is currently leased elsewhere. Whoever loads first can
    //   take up to QUEUE_BATCH clips (ponytail: a fixed batch, not a fair-share scheduler — leases
    //   expire and batches drain, so the imbalance is self-correcting). On a backlog smaller than one
    //   batch that leaves a second reviewer with an EMPTY queue, and "🎉 Queue reviewed" would then be a
    //   flat lie. This count lets the page say "someone else has them" instead.
    //   `skippedByYou` — how many pending clips THIS reviewer declined to judge. Same class of lie as
    //   `heldByOthers`, one step further along: a reviewer who skips their way through a small backlog
    //   would otherwise be congratulated with "🎉 all clips reviewed" over work nobody has judged.
    //
    //   `pendingTotal` — how much pending work EXISTS, leased or not. The page could previously only
    //   count the batch in its hands, so "clip 7 of 25" was the whole truth it had: a reviewer working
    //   a 400-clip backlog saw a bar that filled up and reset, over and over, with no way to tell a
    //   nearly-finished corpus from a barely-started one. Honest overall progress needs the total, and
    //   the total is free here — the query already walked every pending row.
    let mut payload = serde_json::json!({
        "reviewer": reviewer,
        "playbackContractVersion": COUCH_PLAYBACK_POLICY_VERSION,
        "items": queue,
        "heldByOthers": held_by_others,
        "skippedByYou": skipped_by_you,
        "pendingTotal": pending_total,
        "pilotRemainingReviewActions": pilot_slots,
        "campaignPhase": campaign_policy.as_ref().map(|policy| policy.phase().as_str()),
        "reviewPool": pool_policy.is_some(),
        // "there is no work in YOUR dialect", not "everything is reviewed".
        "noWorkInYourDialect": restricted_and_empty,
    });
    merge_json_object(&mut payload, accounting);
    json_reply(200, payload)
}

/// Everything that determines a clip's WAV bytes, hashed into one value used as BOTH the cache key and
/// the ETag. Deriving them from the same fingerprint is what makes a 304 free: the server can answer
/// "unchanged" without decoding anything.
///
/// ponytail: covers the segment's identity and its alignment — a re-alignment moves the boundaries and
/// therefore the bytes, and must invalidate both. It does NOT hash the source file's contents, so
/// overwriting a source WAV in place while keeping its path would serve stale bytes for up to the
/// cache's lifetime. That is out of reach of the app itself (imports are write-once under a
/// content-addressed name), and hashing hundreds of MB per request to close it would cost far more
/// than it buys. Add a mtime/len check here if sources ever become mutable.
pub(super) fn audio_fingerprint(seg: &crate::db::SpeechSegment) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    seg.id.hash(&mut h);
    seg.audio_path.hash(&mut h);
    seg.alignment_json.hash(&mut h);
    seg.duration_ms.hash(&mut h);
    h.finish()
}

/// Materialised clip bytes, newest last. Bounded by total SIZE, not entry count — clips vary.
///
/// What a repeat `/api/audio` hit actually costs without this, read out of audio.rs rather than
/// assumed: `decode_to_pcm` consults an LRU of decoded source PCM, but its key is
/// `pcm_cache_key` — which OPENS THE SOURCE FILE AND BLAKE3-HASHES ALL OF IT before the cache can be
/// consulted at all. So every request re-reads the whole source (the owner's is 172 MB), then clones
/// the entire decoded PCM out of the LRU (`cached.clone()`, another ~172 MB memcpy), then re-slices the
/// clip and re-encodes the WAV sample by sample. The decode itself is cached; nothing else on that path
/// is. Caching the finished bytes here skips all of it.
///
/// It matters because the page asks for the same clip up to three times — the `<audio>` element, the
/// waveform's `decodeAudioData`, and the prefetch — and range support adds a fourth (Safari opens media
/// with a 2-byte probe). Immutable caching now lets the browser elide most of those, but a reviewer
/// replaying a clip, or a second reviewer meeting the same spot check, still arrives here.
pub(super) static AUDIO_CACHE: Mutex<Vec<(u64, Arc<Vec<u8>>)>> = Mutex::new(Vec::new());

/// ~32 MB, i.e. roughly 80 clips at the measured 300-500 KB each: a whole batch plus its spot checks.
pub(super) const AUDIO_CACHE_BYTES: usize = 32 * 1024 * 1024;

/// Cached clip bytes, materialising on a miss. The decode happens OUTSIDE the cache lock: it takes
/// seconds on a long source, and holding the lock through it would serialise every reviewer's audio
/// behind one clip. Two racing misses for the same clip therefore both decode and the second simply
/// replaces the first — wasteful once, never wrong.
pub(super) fn cached_audio(fp: u64, seg: &crate::db::SpeechSegment) -> Result<Arc<Vec<u8>>, String> {
    {
        let mut cache = lock_audio_cache();
        if let Some(pos) = cache.iter().position(|(k, _)| *k == fp) {
            let hit = cache.remove(pos);
            let bytes = Arc::clone(&hit.1);
            cache.push(hit); // newest last
            return Ok(bytes);
        }
    }
    let bytes = Arc::new(crate::agentic::segment_audio_as_wav_bytes(seg).map_err(|e| e.to_string())?);
    let mut cache = lock_audio_cache();
    cache.retain(|(k, _)| *k != fp);
    cache.push((fp, Arc::clone(&bytes)));
    let mut total: usize = cache.iter().map(|(_, b)| b.len()).sum();
    while total > AUDIO_CACHE_BYTES && cache.len() > 1 {
        total -= cache.remove(0).1.len(); // evict oldest
    }
    Ok(bytes)
}

/// Same poisoning stance as `lock_state`: a panic elsewhere must not take the audio route down.
pub(super) fn lock_audio_cache() -> std::sync::MutexGuard<'static, Vec<(u64, Arc<Vec<u8>>)>> {
    AUDIO_CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A single `bytes=` range against a known body length, per RFC 9110.
///
/// Deliberately narrow: ONE range only, and a multi-range request falls back to the full body (a legal
/// answer — 206 multipart is optional, and no media element asks for it). Returns None for anything
/// unsatisfiable so the caller can answer 416 rather than guess.
pub(super) fn parse_range(spec: &str, len: usize) -> Option<(usize, usize)> {
    let spec = spec.trim().strip_prefix("bytes=")?;
    if len == 0 || spec.contains(',') {
        return None;
    }
    let (first, last) = spec.split_once('-')?;
    let (start, end) = match (first.trim(), last.trim()) {
        // "bytes=-N": the LAST n bytes. N >= len means the whole body, not an error. N == 0 falls out
        // as unsatisfiable on its own (start == len fails the guard below), which is what RFC 9110
        // requires — an earlier `.max(1)` here quietly turned it into a request for the final byte.
        ("", n) => (len.saturating_sub(n.parse::<usize>().ok()?), len - 1),
        // "bytes=N-": from N to the end.
        (n, "") => (n.parse::<usize>().ok()?, len - 1),
        (a, b) => (a.parse::<usize>().ok()?, b.parse::<usize>().ok()?.min(len - 1)),
    };
    (start <= end && start < len).then_some((start, end))
}

#[derive(Clone, Copy)]
pub(super) enum AudioAssignment {
    Work,
    HiddenCheck { served_in_pilot: bool, distinct_pilot_keys: usize },
}

pub(super) fn forget_work_audio_assignment(state: &Mutex<CouchState>, id: &str, reviewer: &str) {
    let mut guard = lock_state(state);
    if guard.leases.get(id).is_some_and(|(who, _)| who == reviewer) {
        guard.leases.remove(id);
    }
    guard.served_work.remove(&(id.to_string(), reviewer.to_string()));
    guard.remove_playback_attempts_for_assignment(id, reviewer);
}

/// Resolve the in-memory proof that this exact reviewer was handed this exact object. A live lease
/// by itself is intentionally insufficient: renew is allowed to reclaim an expired assignment, so
/// treating any lease as delivery would turn that reliability endpoint into an object-id oracle.
pub(super) fn audio_assignment(
    db: &Database,
    id: &str,
    reviewer: &str,
    state: &Mutex<CouchState>,
    pilot_policy: Option<&crate::review_pilot::ReviewPilotPolicy>,
) -> Result<AudioAssignment, Reply> {
    let key = (id.to_string(), reviewer.to_string());
    let (skipped, served_work, remembered_hidden, remembered_pilot_count) = {
        let mut guard = lock_state(state);
        let now = guard.now();
        let holder = guard.holder(id, now).map(str::to_string);
        let skipped = guard.skipped.get(reviewer).is_some_and(|ids| ids.contains(id));
        if skipped {
            if holder.as_deref() == Some(reviewer) {
                guard.leases.remove(id);
            }
            guard.served_work.remove(&key);
        }
        let served_work = holder.as_deref() == Some(reviewer) && guard.served_work.contains(&key);
        let remembered_hidden = guard.spot_checks.contains(&key);
        let remembered_pilot_count = guard
            .pilot_spot_checks
            .iter()
            .filter(|(_, who)| who.eq_ignore_ascii_case(reviewer))
            .map(|(segment_id, _)| segment_id)
            .collect::<HashSet<_>>()
            .len();
        (skipped, served_work, remembered_hidden, remembered_pilot_count)
    };
    if skipped {
        return Err(err_reply(403, "audio is no longer assigned to this reviewer — reload your queue"));
    }
    if let Some(policy) = pilot_policy {
        let policy_sha256 = policy
            .policy_sha256()
            .map_err(|error| err_reply(503, &format!("controlled review pilot is unavailable: {error}")))?;
        let ids = db
            .review_pilot_hidden_keys(&policy_sha256, policy.after_review_event_id, reviewer)
            .map_err(|error| err_reply(503, &format!("hidden-check authorization is unavailable: {error}")))?;
        if ids.iter().any(|segment_id| segment_id == id) {
            let resolved = db
                .review_pilot_hidden_key_resolved(&policy_sha256, policy.after_review_event_id, reviewer, id)
                .map_err(|error| err_reply(503, &format!("hidden-check authorization is unavailable: {error}")))?;
            if resolved {
                return Err(err_reply(403, "audio is no longer assigned to this reviewer — reload your queue"));
            }
            return Ok(AudioAssignment::HiddenCheck { served_in_pilot: true, distinct_pilot_keys: ids.len() });
        }
    }
    if served_work {
        return Ok(AudioAssignment::Work);
    }
    if pilot_policy.is_none() && remembered_hidden {
        return Ok(AudioAssignment::HiddenCheck {
            served_in_pilot: false,
            distinct_pilot_keys: remembered_pilot_count,
        });
    }
    Err(err_reply(403, "audio is not assigned to this reviewer — reload your queue"))
}

/// Revalidate every policy that made a queue item servable before returning its audio. Authentication
/// answers who the caller is; this is the separate object-level authorization boundary that answers
/// whether that reviewer may fetch this segment NOW.
pub(super) fn authorize_audio(
    db: &Database,
    id: &str,
    reviewer: &str,
    state: &Mutex<CouchState>,
) -> Result<SpeechSegment, Reply> {
    let (allowed_dialects, focus) = reviewer_policy(reviewer, state).map_err(|error| err_reply(503, &error))?;
    let pilot_policy = active_pilot_policy(reviewer, state).map_err(|error| err_reply(503, &error))?;
    let campaign_policy = active_campaign_policy(db, reviewer, state).map_err(|error| err_reply(503, &error))?;
    if pilot_policy.is_some() && campaign_policy.is_some() {
        return Err(err_reply(503, "conflicting paid-review policies are active"));
    }
    let assignment = audio_assignment(db, id, reviewer, state, pilot_policy.as_ref())?;

    match (assignment, pilot_policy.as_ref()) {
        (AudioAssignment::Work, Some(policy)) => {
            // Serialize the live cap read with in-process pilot decisions. The database remains the
            // cross-connection authority; this closes the local last-slot/read race without holding
            // the guard while audio is decoded.
            let _pilot_guard = lock_pilot_decision_commit();
            match pilot_remaining_slots(db, reviewer, policy) {
                Ok(0) => {
                    forget_work_audio_assignment(state, id, reviewer);
                    return Err(err_reply(
                        403,
                        "controlled review pilot is complete — no more work audio is authorized",
                    ));
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(err_reply(503, &format!("controlled review pilot is unavailable: {error}")));
                }
            }
        }
        (AudioAssignment::HiddenCheck { served_in_pilot, distinct_pilot_keys }, Some(policy)) => {
            let quota = pilot_spot_check_quota(policy, reviewer)
                .map_err(|error| err_reply(503, &format!("controlled review pilot is unavailable: {error}")))?;
            if !served_in_pilot {
                return Err(err_reply(403, "this hidden-check audio is outside the active pilot"));
            }
            if distinct_pilot_keys > quota {
                return Err(err_reply(503, "controlled hidden-check authorization is inconsistent"));
            }
            // Hidden-only catch-up is deliberately still playable after the corpus-action cap. Those
            // checks are the evidence required to finish the bounded pilot; they consume no corpus slot.
        }
        _ => {}
    }

    let seg = match db.get_segment_by_id(id) {
        Ok(Some(seg)) => seg,
        Ok(None) => {
            if matches!(assignment, AudioAssignment::Work) {
                forget_work_audio_assignment(state, id, reviewer);
            }
            return Err(err_reply(403, "audio is no longer assigned to this reviewer — reload your queue"));
        }
        Err(error) => {
            tracing::error!("Couch Review audio authorization lookup failed: {error}");
            return Err(err_reply(503, "audio authorization is temporarily unavailable"));
        }
    };

    let eligible = match assignment {
        AudioAssignment::Work => {
            let raw = seg.raw_transcript.trim();
            let campaign_eligible = match campaign_policy.as_ref() {
                Some(policy) if policy.is_blinded_second_pass() => {
                    crate::review_campaign::independent_segment_pending(db, policy, id).map_err(|error| {
                        tracing::error!("Couch Review independent assignment lookup failed: {error}");
                        err_reply(503, "independent review authorization is temporarily unavailable")
                    })?
                }
                _ => match active_pool_policy(db, state).map_err(|error| err_reply(503, &error))?.as_ref() {
                    // OWNER CANON 2026-08-29: the pool queue serves clips NEAREST a decision first,
                    // so the FIRST clip in every reviewer's queue usually already carries one review
                    // — `verified=true` BY DESIGN, not a revocation. Refusing verified work here
                    // 403'd every pool reviewer's playback/start the night the decision-first
                    // ordering shipped (2026-08-30: player stuck at 00:00/00:00, then the honest
                    // mustListen refusal on save; reproduced end to end with a real credential).
                    // Membership in the immutable active pool is the authority; blindness is
                    // untouched — the queue serves the raw champion draft and audio bytes never
                    // carry the first reviewer's answer.
                    Some(pool) => pool.contains(id),
                    None => !seg.verified,
                },
            };
            campaign_eligible
                && !raw.is_empty()
                && !raw.starts_with('[')
                && Path::new(&seg.audio_path).is_file()
                && reviewer_policy_allows(allowed_dialects.as_deref(), focus.as_deref(), &seg)
        }
        AudioAssignment::HiddenCheck { .. } => {
            let completed = db.has_spot_check_result(id, reviewer).map_err(|error| {
                tracing::error!("Couch Review hidden-check authorization lookup failed: {error}");
                err_reply(503, "audio authorization is temporarily unavailable")
            })?;
            let expected = (!completed).then(|| crate::quality::human_verified_text(&seg)).flatten();
            !completed
                && seg.verified
                && !seg.raw_transcript.is_empty()
                && (seg.is_gold || seg.reviewed_by.is_none())
                && Path::new(&seg.audio_path).is_file()
                && reviewer_policy_allows(allowed_dialects.as_deref(), focus.as_deref(), &seg)
                && expected.is_some_and(|answer| {
                    crate::normalizer::learning_text_key(answer)
                        != crate::normalizer::learning_text_key(&seg.raw_transcript)
                })
        }
    };
    if !eligible {
        if matches!(assignment, AudioAssignment::Work) {
            forget_work_audio_assignment(state, id, reviewer);
        }
        return Err(err_reply(403, "audio is no longer assigned to this reviewer — reload your queue"));
    }

    // Close state races between the first receipt lookup and the database/policy reads above. A
    // completed decision removes the lease; a skip records its exclusion. Neither may be followed by
    // a conditional 304 that silently re-authorizes cached biometric bytes.
    let still_assigned = if matches!(assignment, AudioAssignment::HiddenCheck { .. }) && pilot_policy.is_some() {
        let Some(policy) = pilot_policy.as_ref() else {
            return Err(err_reply(503, "controlled hidden-check policy disappeared during authorization"));
        };
        let policy_sha256 = policy
            .policy_sha256()
            .map_err(|error| err_reply(503, &format!("controlled review pilot is unavailable: {error}")))?;
        let durable = db
            .review_pilot_hidden_keys(&policy_sha256, policy.after_review_event_id, reviewer)
            .map_err(|error| err_reply(503, &format!("hidden-check authorization is unavailable: {error}")))?;
        durable.iter().any(|segment_id| segment_id == id)
            && !db
                .review_pilot_hidden_key_resolved(&policy_sha256, policy.after_review_event_id, reviewer, id)
                .map_err(|error| err_reply(503, &format!("hidden-check authorization is unavailable: {error}")))?
    } else {
        let key = (id.to_string(), reviewer.to_string());
        let mut guard = lock_state(state);
        let skipped = guard.skipped.get(reviewer).is_some_and(|ids| ids.contains(id));
        let now = guard.now();
        match assignment {
            AudioAssignment::Work => {
                !skipped && guard.holder(id, now).is_some_and(|who| who == reviewer) && guard.served_work.contains(&key)
            }
            AudioAssignment::HiddenCheck { .. } => !skipped && guard.spot_checks.contains(&key),
        }
    };
    if !still_assigned {
        return Err(err_reply(403, "audio is no longer assigned to this reviewer — reload your queue"));
    }
    Ok(seg)
}

/// Authorization failures must never become shared or reusable cache entries either.
pub(super) fn private_audio_failure(mut reply: Reply) -> Reply {
    reply.3.push(("Cache-Control", "private, no-store".to_string()));
    reply.3.push(("Vary", "Cookie".to_string()));
    reply
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlaybackStartBody {
    id: String,
    #[serde(rename = "rowVersion")]
    row_version: String,
    #[serde(rename = "clientAttemptId")]
    client_attempt_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlaybackFinalizeBody {
    #[serde(rename = "playbackReceiptId")]
    playback_receipt_id: String,
    #[serde(rename = "clientAttemptId")]
    client_attempt_id: String,
    intervals: Vec<PlaybackIntervalBody>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlaybackIntervalBody {
    #[serde(rename = "startMs")]
    start_ms: i64,
    #[serde(rename = "endMs")]
    end_ms: i64,
}

pub(super) fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|parsed| parsed.hyphenated().to_string() == value)
}

pub(super) fn playback_attempt_query(url: &str) -> Result<Option<String>, Reply> {
    let Some((_, query)) = url.split_once('?') else {
        return Ok(None);
    };
    let mut attempt = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "playbackAttemptId" {
            if attempt.is_some() || !canonical_uuid(value) {
                return Err(private_audio_failure(err_reply(400, "invalid playback attempt identity")));
            }
            attempt = Some(value.to_string());
        }
    }
    Ok(attempt)
}

pub(super) fn api_playback_start(
    db: &Database,
    body: &[u8],
    reviewer: &str,
    session_binding_sha256: &str,
    state: &Mutex<CouchState>,
) -> Reply {
    let parsed: PlaybackStartBody = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(error) => return err_reply(400, &format!("bad json: {error}")),
    };
    if crate::validation::input::validate_identifier(&parsed.id).is_err() || !canonical_uuid(&parsed.client_attempt_id)
    {
        return err_reply(400, "invalid Couch playback start identity");
    }
    let Ok(expected_revision) = parsed.row_version.parse::<i64>() else {
        return err_reply(400, "rowVersion is invalid — reload this clip");
    };
    let segment = match authorize_audio(db, &parsed.id, reviewer, state) {
        Ok(segment) => segment,
        Err(reply) => return reply,
    };
    let current_revision = match db.segment_review_revision(&parsed.id) {
        Ok(Some(value)) => value,
        Ok(None) => return err_reply(404, "no such segment"),
        Err(error) => return err_reply(500, &format!("playback revision lookup failed: {error}")),
    };
    if current_revision != expected_revision {
        return err_reply(409, "this clip changed since it was served — reload for the fresh draft");
    }
    let content_hash = match db.segment_audio_content_hash(&parsed.id) {
        Ok(Some(value)) => value,
        Ok(None) => return err_reply(503, "playback identity is unavailable for this clip"),
        Err(error) => return err_reply(500, &format!("playback identity lookup failed: {error}")),
    };
    let (source_start_ms, source_end_ms) = match db.segment_source_span(&parsed.id) {
        Ok(Some(value)) => value,
        Ok(None) => return err_reply(503, "playback source span is unavailable for this clip"),
        Err(error) => return err_reply(500, &format!("playback source-span lookup failed: {error}")),
    };
    if !crate::db::source_span_matches_duration(source_start_ms, source_end_ms, segment.duration_ms) {
        return err_reply(503, "playback duration and source span disagree");
    }
    let now = lock_state(state).now();
    let issued_at_ms =
        SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_millis() as i64).unwrap_or(1).max(1);
    let expires_at_ms = issued_at_ms.saturating_add(COUCH_PLAYBACK_ATTEMPT_TTL.as_millis() as i64);
    let expires_at = now.checked_add(COUCH_PLAYBACK_ATTEMPT_TTL).unwrap_or(now);
    let client_key = (session_binding_sha256.to_string(), parsed.client_attempt_id.clone());
    let mut guard = lock_state(state);
    guard.prune_playback_attempts(now);
    if let Some(existing_id) = guard.playback_attempt_clients.get(&client_key).cloned() {
        let Some(existing) = guard.playback_attempts.get(&existing_id) else {
            guard.playback_attempt_clients.remove(&client_key);
            return err_reply(503, "playback attempt index is inconsistent — reload this clip");
        };
        let authority = &existing.authority;
        if authority.reviewer != reviewer
            || authority.segment_id != parsed.id
            || authority.segment_revision != expected_revision
            || authority.audio_content_hash != content_hash
            || authority.clip_duration_ms != segment.duration_ms
            || authority.source_start_ms != source_start_ms
            || authority.source_end_ms != source_end_ms
        {
            return err_reply(409, "clientAttemptId is already bound to another exact playback request");
        }
        return json_reply(
            200,
            serde_json::json!({
                "playbackContractVersion": COUCH_PLAYBACK_POLICY_VERSION,
                "playbackReceiptId": authority.playback_receipt_id,
                "clientAttemptId": authority.client_attempt_id,
                "segmentId": authority.segment_id,
                "segmentRevision": authority.segment_revision,
                "clipDurationMs": authority.clip_duration_ms,
                "expiresAtMs": authority.expires_at_ms,
                "duplicate": true,
            }),
        );
    }
    let playback_receipt_id = uuid::Uuid::new_v4().to_string();
    let authority = CouchPlaybackAttemptAuthority {
        playback_receipt_id: playback_receipt_id.clone(),
        media_grant_id: uuid::Uuid::new_v4().to_string(),
        client_attempt_id: parsed.client_attempt_id,
        session_binding_sha256: session_binding_sha256.to_string(),
        reviewer: reviewer.to_string(),
        segment_id: parsed.id,
        segment_revision: expected_revision,
        audio_content_hash: content_hash,
        source_path: PathBuf::from(segment.audio_path),
        clip_duration_ms: segment.duration_ms,
        source_start_ms,
        source_end_ms,
        issued_at_ms,
        expires_at_ms,
    };
    guard.playback_attempt_clients.insert(client_key, playback_receipt_id.clone());
    guard.playback_attempts.insert(
        playback_receipt_id.clone(),
        CouchPlaybackAttempt { authority: authority.clone(), media_served_at: None, expires_at },
    );
    json_reply(
        200,
        serde_json::json!({
            "playbackContractVersion": COUCH_PLAYBACK_POLICY_VERSION,
            "playbackReceiptId": playback_receipt_id,
            "clientAttemptId": authority.client_attempt_id,
            "segmentId": authority.segment_id,
            "segmentRevision": authority.segment_revision,
            "clipDurationMs": authority.clip_duration_ms,
            "expiresAtMs": authority.expires_at_ms,
        }),
    )
}

pub(super) fn validate_audio_playback_attempt(
    db: &Database,
    id: &str,
    reviewer: &str,
    session_binding_sha256: &str,
    playback_receipt_id: &str,
    state: &Mutex<CouchState>,
) -> Result<(), Reply> {
    let authority = {
        let mut guard = lock_state(state);
        let now = guard.now();
        guard.prune_playback_attempts(now);
        guard.playback_attempts.get(playback_receipt_id).map(|attempt| attempt.authority.clone())
    }
    .ok_or_else(|| {
        private_audio_failure(err_reply(409, "playback attempt is missing or expired — reload this clip"))
    })?;
    if authority.session_binding_sha256 != session_binding_sha256
        || !same_reviewer(&authority.reviewer, reviewer)
        || authority.segment_id != id
    {
        return Err(private_audio_failure(err_reply(403, "playback attempt belongs to another session or clip")));
    }
    let revision = db
        .segment_review_revision(id)
        .map_err(|error| private_audio_failure(err_reply(500, &format!("playback revision lookup failed: {error}"))))?;
    let content_hash = db
        .segment_audio_content_hash(id)
        .map_err(|error| private_audio_failure(err_reply(500, &format!("playback identity lookup failed: {error}"))))?;
    let source_span = db
        .segment_source_span(id)
        .map_err(|error| private_audio_failure(err_reply(500, &format!("playback span lookup failed: {error}"))))?;
    if revision != Some(authority.segment_revision)
        || content_hash.as_deref() != Some(authority.audio_content_hash.as_str())
        || source_span != Some((authority.source_start_ms, authority.source_end_ms))
    {
        return Err(private_audio_failure(err_reply(
            409,
            "playback attempt no longer matches this clip revision — reload it",
        )));
    }
    Ok(())
}

pub(super) fn mark_audio_playback_attempt_served(
    playback_receipt_id: &str,
    reviewer: &str,
    session_binding_sha256: &str,
    state: &Mutex<CouchState>,
) {
    let mut guard = lock_state(state);
    let now = guard.now();
    if let Some(attempt) = guard.playback_attempts.get_mut(playback_receipt_id) {
        if attempt.authority.session_binding_sha256 == session_binding_sha256
            && same_reviewer(&attempt.authority.reviewer, reviewer)
            && attempt.media_served_at.is_none()
        {
            attempt.media_served_at = Some(now);
        }
    }
}

pub(super) fn playback_error_reply(error: &str) -> Reply {
    if error.contains("E_NO_PLAYBACK_EVIDENCE") || error.contains("E_PLAYBACK_TIME_IMPLAUSIBLE") {
        err_reply(428, error)
    } else if error.contains(PLAYBACK_EVIDENCE_CHANGED)
        || error.contains("different")
        || error.contains("already bound")
        || error.contains("replay")
    {
        err_reply(409, error)
    } else if error.contains("invalid") || error.contains("malformed") || error.contains("interval") {
        err_reply(400, error)
    } else {
        err_reply(500, error)
    }
}

pub(super) fn api_playback_finalize(
    db: &Database,
    body: &[u8],
    reviewer: &str,
    session_binding_sha256: &str,
    state: &Mutex<CouchState>,
) -> Reply {
    let parsed: PlaybackFinalizeBody = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(error) => return err_reply(400, &format!("bad json: {error}")),
    };
    if !canonical_uuid(&parsed.playback_receipt_id) || !canonical_uuid(&parsed.client_attempt_id) {
        return err_reply(400, "invalid Couch playback finalization identity");
    }
    let intervals: Vec<DesktopPlaybackInterval> = parsed
        .intervals
        .iter()
        .map(|interval| DesktopPlaybackInterval { start_ms: interval.start_ms, end_ms: interval.end_ms })
        .collect();
    match db.replay_finalized_couch_playback_receipt_v1(
        &parsed.playback_receipt_id,
        &parsed.client_attempt_id,
        session_binding_sha256,
        reviewer,
        &intervals,
    ) {
        Ok(Some(receipt)) => {
            return json_reply(
                200,
                serde_json::json!({
                    "playbackContractVersion": COUCH_PLAYBACK_POLICY_VERSION,
                    "playbackReceiptId": receipt.playback_receipt_id,
                    "segmentId": receipt.segment_id,
                    "segmentRevision": receipt.segment_revision,
                    "uniqueTraversedMs": receipt.unique_played_ms,
                    "clipDurationMs": receipt.clip_duration_ms,
                    "coverageRatio": receipt.coverage_ratio,
                    "duplicate": true,
                }),
            );
        }
        Ok(None) => {}
        Err(error) => return playback_error_reply(&error.to_string()),
    }
    let (authority, elapsed_ms) = {
        let mut guard = lock_state(state);
        let now = guard.now();
        guard.prune_playback_attempts(now);
        let Some(attempt) = guard.playback_attempts.get(&parsed.playback_receipt_id) else {
            return err_reply(409, "playback attempt is missing or expired — reload and replay this clip");
        };
        if attempt.authority.client_attempt_id != parsed.client_attempt_id
            || attempt.authority.session_binding_sha256 != session_binding_sha256
            || !same_reviewer(&attempt.authority.reviewer, reviewer)
        {
            return err_reply(403, "playback attempt belongs to another session");
        }
        let Some(served_at) = attempt.media_served_at else {
            return err_reply(428, "E_NO_PLAYBACK_EVIDENCE: this attempt never received authorized media");
        };
        let elapsed = now.duration_since(served_at);
        (attempt.authority.clone(), i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
    };
    let receipt = match db.finalize_couch_playback_attempt_v1(&authority, &intervals, elapsed_ms) {
        Ok(receipt) => receipt,
        Err(error) => return playback_error_reply(&error.to_string()),
    };
    lock_state(state).remove_playback_attempt(&parsed.playback_receipt_id);
    json_reply(
        200,
        serde_json::json!({
            "playbackContractVersion": COUCH_PLAYBACK_POLICY_VERSION,
            "playbackReceiptId": receipt.playback_receipt_id,
            "segmentId": receipt.segment_id,
            "segmentRevision": receipt.segment_revision,
            "uniqueTraversedMs": receipt.unique_played_ms,
            "clipDurationMs": receipt.clip_duration_ms,
            "coverageRatio": receipt.coverage_ratio,
        }),
    )
}

/// Clip audio, authorization-revalidated, cache-efficient and range-capable.
///
/// `private, no-cache` permits the browser to retain the biometric bytes but requires every reuse to
/// cross this live authorization boundary. A still-authorized replay remains cheap through the ETag
/// 304; a completed/revoked/out-of-policy assignment is refused before the validator is considered.
/// The arguments are the independently authenticated request authorities; grouping them into an
/// opaque bag would make it easier to omit one during a security-sensitive call-site change.
#[allow(clippy::too_many_arguments)]
pub(super) fn api_audio_authenticated(
    db: &Database,
    raw_id: &str,
    reviewer: &str,
    session_binding_sha256: &str,
    state: &Mutex<CouchState>,
    playback_receipt_id: Option<&str>,
    is_head: bool,
    range: Option<&str>,
    if_none_match: Option<&str>,
) -> Reply {
    let id = raw_id.split('?').next().unwrap_or(raw_id);
    if crate::validation::input::validate_identifier(id).is_err() {
        return private_audio_failure(err_reply(400, "bad id"));
    }
    let seg = match authorize_audio(db, id, reviewer, state) {
        Ok(seg) => seg,
        Err(reply) => return private_audio_failure(reply),
    };
    if let Some(playback_receipt_id) = playback_receipt_id {
        if let Err(reply) =
            validate_audio_playback_attempt(db, id, reviewer, session_binding_sha256, playback_receipt_id, state)
        {
            return reply;
        }
    }
    let etag = format!("\"{:016x}\"", audio_fingerprint(&seg));
    // Storage is private and every reuse revalidates the live cookie + assignment. `Vary: Cookie`
    // prevents a cache entry authorized for one reviewer session being selected for another; ETag
    // keeps an authorized replay at zero audio bytes on the wire.
    let base = || {
        vec![
            ("Cache-Control", "private, no-cache".to_string()),
            ("Vary", "Cookie".to_string()),
            ("ETag", etag.clone()),
            ("Accept-Ranges", "bytes".to_string()),
        ]
    };
    // A conditional hit is answered without touching the decoder — the whole point of deriving the
    // ETag from the fingerprint rather than from the bytes. Weak-comparison prefix and `*` both count,
    // and a multi-value If-None-Match is split rather than compared whole.
    if if_none_match.is_some_and(|inm| {
        inm.split(',').any(|c| {
            let c = c.trim();
            c == "*" || c == etag || c.strip_prefix("W/").is_some_and(|c| c == etag)
        })
    }) {
        if !is_head {
            if let Some(playback_receipt_id) = playback_receipt_id {
                mark_audio_playback_attempt_served(playback_receipt_id, reviewer, session_binding_sha256, state);
            }
        }
        return (304, "audio/wav", Vec::new(), base());
    }
    let bytes = match cached_audio(audio_fingerprint(&seg), &seg) {
        Ok(bytes) => bytes,
        Err(e) => return private_audio_failure(err_reply(500, &e)),
    };
    let len = bytes.len();
    let reply = match range {
        None => (200, "audio/wav", bytes.as_ref().clone(), base()),
        Some(spec) => match parse_range(spec, len) {
            Some((start, end)) => {
                let mut headers = base();
                headers.push(("Content-Range", format!("bytes {start}-{end}/{len}")));
                (206, "audio/wav", bytes[start..=end].to_vec(), headers)
            }
            // Unsatisfiable, and saying so is required: a client that gets a 200 full body here
            // believes its range was honoured and reads the wrong offsets.
            None => {
                let mut headers = base();
                headers.push(("Content-Range", format!("bytes */{len}")));
                (416, "text/plain; charset=utf-8", b"range not satisfiable".to_vec(), headers)
            }
        },
    };
    if !is_head && matches!(reply.0, 200 | 206) {
        if let Some(playback_receipt_id) = playback_receipt_id {
            mark_audio_playback_attempt_served(playback_receipt_id, reviewer, session_binding_sha256, state);
        }
    }
    reply
}

#[cfg(test)]
pub(super) fn api_audio(
    db: &Database,
    raw_id: &str,
    reviewer: &str,
    state: &Mutex<CouchState>,
    range: Option<&str>,
    if_none_match: Option<&str>,
) -> Reply {
    api_audio_authenticated(
        db,
        raw_id,
        reviewer,
        &couch_session_binding_sha256("couch-test-session"),
        state,
        None,
        false,
        range,
        if_none_match,
    )
}

#[derive(serde::Deserialize)]
pub(super) struct RenewBody {
    id: String,
}

/// Extend this reviewer's hold on the clip they still have open (P1.3).
///
/// A 15-minute lease can genuinely expire mid-clip on a hard piece of audio. Without renewal the
/// reviewer discovers it only at save time, as a 409, with their correction already typed and now
/// unsaveable — the exact work-destroying moment leases exist to prevent. The page heartbeats while a
/// clip is open, so an ACTIVE reviewer keeps their clip indefinitely while an idle one still releases
/// it on schedule.
///
/// Reclaiming an unheld clip is allowed only when `/api/queue` actually delivered it to this
/// reviewer: if the lease lapsed but nobody else took it, the page that still has it open should keep
/// it. A bearer presenting an arbitrary known id must not be able to mint that delivery proof.
pub(super) fn api_renew(body: &[u8], reviewer: &str, state: &Mutex<CouchState>) -> Reply {
    let parsed: RenewBody = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => return err_reply(400, &format!("bad json: {e}")),
    };
    if crate::validation::input::validate_identifier(&parsed.id).is_err() {
        return err_reply(400, "bad id");
    }
    let mut guard = lock_state(state);
    let now = guard.now();
    let key = (parsed.id.clone(), reviewer.to_string());
    if guard.skipped.get(reviewer).is_some_and(|ids| ids.contains(&parsed.id)) {
        return err_reply(409, "this clip is no longer assigned — reload your queue");
    }
    if guard.holder(&parsed.id, now).is_some_and(|who| who != reviewer) {
        // Someone else took it while this reviewer was away. Telling them NOW — rather than at save —
        // is the whole point: they can still copy their correction before it is refused.
        return err_reply(409, "another reviewer is working on this clip");
    }
    if !guard.served_work.contains(&key) {
        // Hidden checks are deliberately not leased. Their exact served receipt is enough for the
        // page heartbeat to succeed, while audio and decision paths independently revalidate that
        // the key is still outstanding and policy-eligible.
        if guard.spot_checks.contains(&key) {
            return json_reply(200, serde_json::json!({ "ok": true, "ttlSeconds": LEASE_TTL.as_secs() }));
        }
        return err_reply(409, "this clip was not served to this reviewer — reload your queue");
    }
    // Refresh the reviewer's WHOLE batch, not just the clip on screen.
    //
    // api_queue leases up to QUEUE_BATCH clips in one shot, every one stamped with the same instant,
    // so they all expire together — while the page only ever heartbeats `queue[i]`. That gave the
    // reviewer LEASE_TTL to finish the entire batch (36 seconds per clip at 25), and real
    // listen-and-correct on Sorani audio is nowhere near that. Everything they had not yet reached
    // silently fell out from under them, another reviewer's next fetch picked it up, and the first
    // reviewer's eventual save was refused 409 with their correction already typed. A renew request
    // is proof this person is working; their claim on the rest of their batch is exactly as live as
    // their claim on the clip in front of them.
    let served_ids: HashSet<String> =
        guard.served_work.iter().filter(|(_, who)| who == reviewer).map(|(id, _)| id.clone()).collect();
    for (id, (who, granted)) in guard.leases.iter_mut() {
        if who == reviewer && served_ids.contains(id) {
            *granted = now;
        }
    }
    guard.leases.insert(parsed.id.clone(), (reviewer.to_string(), now));
    json_reply(200, serde_json::json!({ "ok": true, "ttlSeconds": LEASE_TTL.as_secs() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(id: &str) -> SpeechSegment {
        SpeechSegment { id: id.to_string(), audio_path: format!(r"D:\clips\{id}.wav"), ..SpeechSegment::default() }
    }

    fn header(reply: &Reply, name: &str) -> Option<String> {
        reply.3.iter().find(|(key, _)| *key == name).map(|(_, value)| value.clone())
    }

    // The two-speaker badge the reviewer sees BEFORE deciding. `None` is "not measured", and it must
    // read as false — absence of a measurement is not evidence of a single speaker, and a clip
    // wrongly presented as ordinary work walks a two-speaker chunk into a single-speaker corpus.
    #[test]
    fn a_speaker_change_is_claimed_only_when_it_was_actually_measured() {
        let with_score = |score: Option<f64>| {
            let mut s = seg("s1");
            s.speaker_change_score = score;
            s
        };
        assert!(!holds_a_speaker_change(&with_score(None)), "unmeasured must never claim one speaker or two");
        let threshold = crate::diarization::SPEAKER_CHANGE_THRESHOLD as f64;
        assert!(holds_a_speaker_change(&with_score(Some(threshold - 0.01))), "below the threshold IS a turn");
        assert!(!holds_a_speaker_change(&with_score(Some(threshold))), "exactly at the threshold is not below it");
        assert!(!holds_a_speaker_change(&with_score(Some(0.99))), "a confidently single-speaker clip");
    }

    // Both authorization filters the queue and the decision boundary share. Getting either backwards
    // serves a restricted reviewer a dialect they cannot judge, which is the failure dialect.rs exists
    // to stop.
    #[test]
    fn reviewer_policy_allows_combines_dialect_and_focus() {
        let hawleri = SpeechSegment {
            id: "h1".into(),
            audio_path: r"D:\Kurdish Corpora\KBHP_ep01.wav".into(),
            ..SpeechSegment::default()
        };
        let unmapped = seg("u1");

        assert!(reviewer_policy_allows(None, None, &hawleri), "no roster and no focus is unrestricted");
        let sorani_only = vec![crate::dialect::SORANI.to_string()];
        assert!(
            !reviewer_policy_allows(Some(sorani_only.as_slice()), None, &hawleri),
            "a Sorani-only reviewer is not given KBHP"
        );
        let hawleri_ok = vec![crate::dialect::HAWLERI.to_string()];
        assert!(reviewer_policy_allows(Some(hawleri_ok.as_slice()), None, &hawleri), "the competent reviewer is");
        assert!(
            !reviewer_policy_allows(Some(hawleri_ok.as_slice()), None, &unmapped),
            "an UNMAPPED source fails closed for a restricted reviewer"
        );
        assert!(reviewer_policy_allows(None, None, &unmapped), "but not for an unrestricted one");

        // Focus narrows independently of dialect, and both must agree.
        let focus: HashSet<String> = ["h1".to_string()].into_iter().collect();
        assert!(reviewer_policy_allows(None, Some(&focus), &hawleri), "the focused clip passes");
        assert!(!reviewer_policy_allows(None, Some(&focus), &unmapped), "anything outside the focus does not");
        assert!(
            !reviewer_policy_allows(Some(sorani_only.as_slice()), Some(&focus), &hawleri),
            "focus does not override the dialect refusal"
        );
    }

    // Every status this maps to is a different instruction to the phone: 428 = replay the clip,
    // 409 = reload it, 400 = the request itself was malformed, 500 = the server broke. Collapsing any
    // pair strands a reviewer with typed corrections and no usable next step.
    #[test]
    fn playback_errors_map_to_the_status_that_tells_the_phone_what_to_do() {
        let status = |message: &str| playback_error_reply(message).0;

        assert_eq!(status("E_NO_PLAYBACK_EVIDENCE: this attempt never received authorized media"), 428);
        assert_eq!(status("E_PLAYBACK_TIME_IMPLAUSIBLE: 4000 ms claimed in 40 ms"), 428);

        assert_eq!(status(PLAYBACK_EVIDENCE_CHANGED), 409, "the evidence moved under the decision");
        assert_eq!(status("the served clip was different"), 409);
        assert_eq!(status("clientAttemptId is already bound to another exact playback request"), 409);
        assert_eq!(status("this receipt is a replay"), 409);

        assert_eq!(status("invalid playback attempt identity"), 400);
        assert_eq!(status("malformed request body"), 400);
        assert_eq!(status("interval ends before it starts"), 400);

        assert_eq!(status("database is locked"), 500, "anything unrecognised is a server fault, not the phone's");
        // The message itself always reaches the reviewer, whatever the status.
        let reply = playback_error_reply("database is locked");
        assert_eq!(String::from_utf8(reply.2).unwrap(), "database is locked");
    }

    // A refusal on the audio route must be as unshareable as the audio it refused: caches are
    // per-session and vary on the credential, so a proxy or a shared device cannot replay one
    // reviewer's refusal (or its absence) to another.
    #[test]
    fn private_audio_failures_are_never_shared_across_sessions() {
        let reply = private_audio_failure(err_reply(403, "audio is not assigned to this reviewer"));
        assert_eq!(reply.0, 403, "the status and body pass through untouched");
        assert_eq!(String::from_utf8(reply.2.clone()).unwrap(), "audio is not assigned to this reviewer");
        assert_eq!(header(&reply, "Cache-Control").as_deref(), Some("private, no-store"));
        assert_eq!(header(&reply, "Vary").as_deref(), Some("Cookie"));
    }

    // The identity gate on every playback receipt. It accepts EXACTLY the canonical hyphenated
    // rendering — a re-cased or braced spelling of the same UUID is a different string everywhere the
    // receipt is later matched, so accepting it would silently split one attempt into two.
    #[test]
    fn canonical_uuid_accepts_only_the_exact_hyphenated_rendering() {
        let id = "2f2d9b66-8566-4d1c-8c14-e18d006b776f";
        assert!(canonical_uuid(id));
        assert!(!canonical_uuid(&id.to_uppercase()), "a re-cased rendering is a different key");
        assert!(!canonical_uuid(&format!("{{{id}}}")), "the braced form is not canonical");
        assert!(!canonical_uuid(&id.replace('-', "")), "the simple form is not canonical");
        assert!(!canonical_uuid(&format!("urn:uuid:{id}")), "the URN form is not canonical");
        assert!(!canonical_uuid(""), "empty is not an identity");
        assert!(!canonical_uuid("not-a-uuid"), "and neither is arbitrary text");
        // Version is NOT checked here (unlike the media grant id) — canonical rendering is the whole
        // contract, because these ids are minted by this process and only ever compared for equality.
        assert!(canonical_uuid("2f2d9b66-8566-1d1c-8c14-e18d006b776f"), "any version renders canonically");
    }

    // The audio URL's only accepted parameter. A second copy, or a value that is not exactly one
    // canonical UUID, is refused outright rather than resolved to "the first one wins" — a receipt
    // chosen by parameter order is a receipt an attacker chooses.
    #[test]
    fn playback_attempt_query_takes_one_canonical_id_or_refuses() {
        let id = "2f2d9b66-8566-4d1c-8c14-e18d006b776f";

        assert_eq!(playback_attempt_query("/api/audio/seg-1"), Ok(None), "no query, no attempt");
        assert_eq!(playback_attempt_query("/api/audio/seg-1?"), Ok(None), "an empty query is not a bad one");
        assert_eq!(playback_attempt_query("/api/audio/seg-1?t=123"), Ok(None), "unrelated parameters are ignored");
        assert_eq!(
            playback_attempt_query(&format!("/api/audio/seg-1?t=1&playbackAttemptId={id}&x=2")),
            Ok(Some(id.to_string())),
            "the parameter is found wherever it sits"
        );

        for bad in [
            format!("/api/audio/seg-1?playbackAttemptId={id}&playbackAttemptId={id}"),
            "/api/audio/seg-1?playbackAttemptId=not-a-uuid".to_string(),
            "/api/audio/seg-1?playbackAttemptId=".to_string(),
            "/api/audio/seg-1?playbackAttemptId".to_string(),
            format!("/api/audio/seg-1?playbackAttemptId={}", id.to_uppercase()),
        ] {
            let reply = playback_attempt_query(&bad).expect_err(&format!("{bad} must be refused"));
            assert_eq!(reply.0, 400, "{bad}");
            assert_eq!(
                header(&reply, "Cache-Control").as_deref(),
                Some("private, no-store"),
                "a refusal on the audio route stays private: {bad}"
            );
        }
    }

    // The bounded pilot's refill arithmetic. The whole point is that a page reload can neither evade
    // the hidden checks nor burn a fresh key, so `resend` must always be preferred and `fresh` must
    // never push the distinct-key count past the proven quota.
    #[test]
    fn spot_check_plan_reuses_outstanding_keys_before_minting_any() {
        // A full batch with nothing outstanding mints up to the quota and no more.
        assert_eq!(pilot_spot_check_plan(25, 2, 0, 0, false), (0, 2), "25 work items want 4 checks, quota allows 2");
        // One check already handed out is RE-SERVED, and does not consume a second quota unit.
        assert_eq!(pilot_spot_check_plan(8, 2, 0, 1, false), (1, 0), "the outstanding key is re-served, not replaced");
        // Outstanding beyond the desire is capped at the desire.
        assert_eq!(pilot_spot_check_plan(25, 2, 1, 3, false), (2, 0), "never re-serve more than this refill wants");
        // Quota already spent on distinct keys: nothing fresh may be minted.
        assert_eq!(pilot_spot_check_plan(25, 2, 2, 0, false), (0, 0), "a spent quota mints nothing");
        // No work in this batch means no checks unless completion is being forced.
        assert_eq!(pilot_spot_check_plan(0, 2, 0, 0, false), (0, 0), "no work, no checks");
        assert_eq!(pilot_spot_check_plan(0, 2, 0, 0, true), (0, 2), "forcing completion asks for the whole quota");
        // Forcing completion with one key already served and still outstanding: that key is re-served
        // (it does not spend a second quota unit) and exactly one fresh key is minted, landing on the
        // quota of two DISTINCT keys and never past it.
        assert_eq!(pilot_spot_check_plan(0, 2, 1, 1, true), (1, 1), "re-serve first, then mint the last slot");
        assert_eq!(pilot_spot_check_plan(0, 2, 2, 1, true), (1, 0), "with the quota already spent, only re-serve");
    }

    // The clip-bytes cache key. It must move when the AUDIO would move (identity, source, boundaries,
    // length) and stay put when only the transcript changes — a stale ETag here serves a reviewer the
    // previous clip's bytes against the current clip's text.
    #[test]
    fn the_audio_fingerprint_tracks_the_bytes_not_the_text() {
        let base = seg("s1");
        let fingerprint = audio_fingerprint(&base);
        assert_eq!(fingerprint, audio_fingerprint(&seg("s1")), "the same clip fingerprints the same");

        let mut retranscribed = base.clone();
        retranscribed.raw_transcript = "a different draft".into();
        retranscribed.annotated_transcript = Some("a human correction".into());
        assert_eq!(audio_fingerprint(&retranscribed), fingerprint, "editing the TEXT does not move the audio");

        let mut realigned = base.clone();
        realigned.alignment_json = Some(r#"{"words":[]}"#.into());
        assert_ne!(audio_fingerprint(&realigned), fingerprint, "a re-alignment moves the clip boundaries");
        let mut relinked = base.clone();
        relinked.audio_path = r"D:\clips\elsewhere.wav".into();
        assert_ne!(audio_fingerprint(&relinked), fingerprint, "a different source is different bytes");
        let mut retrimmed = base.clone();
        retrimmed.duration_ms = base.duration_ms + 1;
        assert_ne!(audio_fingerprint(&retrimmed), fingerprint, "a different length is different bytes");
    }
}
