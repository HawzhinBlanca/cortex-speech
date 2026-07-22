//! Jury / cloud-judge IPC command wrappers — slice 7 of the Week-4 `commands.rs` decomposition.
//!
//! Behaviour and command NAMES unchanged: `commands.rs` re-exports this module (`pub use jury::*;`),
//! so `lib.rs`'s invoke_handler still names `commands::run_jury_pipeline` and the frontend invokes are
//! untouched. Same functions, only relocated — the consent/key gates and endpoint resolution are byte
//! for byte identical.
//!
//! Only the thin command wrappers move here; the shared jury machinery
//! (JuryDbSource, run_jury_pipeline_core_via, reference_selection_*, the consent gates, endpoint
//! resolution) STAYS in commands.rs — it is also used by batch_transcribe — and is reached via super::.
//! These are all consent-gated + `run_blocking`: no cloud request is ever offloaded before its
//! cloud-opt-in + key check runs eagerly on the caller thread.

use super::{
    jury_db_source, open_jury_db_connection, reference_selection_evidence, reference_selection_for_segment,
    require_cloud_llm_consent, require_cloud_stt_consent, resolve_t2_endpoint, run_blocking,
    run_jury_pipeline_core_via, RATE_LIMITER, SCRIBE_VOTES_IN_FLIGHT, SCRIBE_VOTE_MODEL_ID, STRICT_RATE_LIMITER,
};
use crate::validation::input as validate;
use crate::AppState;
use tauri::State;

#[tauri::command]
pub async fn run_t0_gate(
    state: State<'_, AppState>,
    segment_ids: Vec<String>,
) -> Result<crate::jury::T0GateReport, String> {
    RATE_LIMITER.check("run_t0_gate")?;
    let (autonomy, learn) = {
        let s = state.lock_settings();
        (s.jury_autonomy_level.clone(), s.irt_ability_learning_enabled)
    };
    let db = state.db_arc();
    run_blocking(move || {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        crate::jury::run_t0_gate(&db, &segment_ids, &autonomy, learn).map_err(|e| e.to_string())
    })
    .await
}

/// Add an independent ElevenLabs Scribe hypothesis for the given segments (typically the escalated,
/// hard ones), so the IRT jury sees an architecturally-INDEPENDENT vote rather than only kin OmniASR
/// models — closing the confidently-wrong-correlated-error hole. Opt-in and cost-bounded: it
/// transcribes only the segments the caller chooses and skips any that already have a Scribe vote
/// (idempotent). Scribe is a second opinion (~32% WER on Sorani), never auto-accepted as gold.
/// Returns the number of votes added. Re-run the jury afterwards to fold the new votes into consensus.
#[tauri::command]
pub async fn add_scribe_votes(ids: Vec<String>, state: State<'_, AppState>) -> Result<usize, String> {
    STRICT_RATE_LIMITER.check("add_scribe_votes")?;
    // Same cloud-STT privacy gate as transcribe_audio_with_scribe: no segment audio is uploaded to
    // ElevenLabs unless the user explicitly opted in, even if a key is present in secrets.env.
    require_cloud_stt_consent(&state)?;
    for id in &ids {
        validate::validate_identifier(id)?;
    }
    let data_dir = state.lock_data_dir().clone().ok_or_else(|| "App data directory is unavailable".to_string())?;
    let key = crate::api_keys::ApiKeys::load(&data_dir)
        .elevenlabs
        .ok_or_else(|| "No ElevenLabs API key configured — add ELEVENLABS_API_KEY to secrets.env".to_string())?;

    // Read which segments still need a Scribe vote, then RELEASE the db lock before any network call —
    // never hold the global db mutex across a blocking cloud request (round-7 concurrency lesson). We
    // keep the FULL segment (audio_path + alignment_json) so each vote can be sliced to that segment's
    // own audio window rather than the whole source file.
    let to_vote: Vec<crate::db::SpeechSegment> = {
        let db = state.lock_db();
        let segs = db.get_segments_by_ids(&ids).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for seg in segs {
            let existing = db.get_hypotheses_for_segment(&seg.id).map_err(|e| e.to_string())?;
            if !existing.iter().any(|h| h.model_id == SCRIBE_VOTE_MODEL_ID) {
                // Keep the full segment (audio_path + alignment_json) so the Scribe vote covers the SAME
                // audio span as the local hypotheses. Without that window the vote would be the
                // whole-recording transcript (segments share the source path), which can never align with
                // the short local hyps and poisons consensus.
                out.push(seg);
            }
        }
        out
    };

    // The per-segment decode/slice + Scribe POST loop runs on the blocking pool so the UI thread stays
    // responsive across the whole batch. Every privacy gate above (STT consent, id validation, key) ran
    // EAGERLY, so nothing is offloaded without consent. The per-vote insert locks the SAME global db
    // mutex as state.lock_db() (db_arc is Arc::clone of it), with the same poison recovery — still a
    // brief per-insert lock, exactly as before, just taken from a pool thread.
    let db_arc = state.db_arc();
    // One batch at a time (restores the old main-thread serialization). Set AFTER all fallible eager
    // work (gates + gather) so no early `?` return can leak the flag; reset on every path after the
    // await, including run_blocking's JoinError path (result is captured, not `?`-propagated).
    if SCRIBE_VOTES_IN_FLIGHT.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err("A Scribe vote batch is already running — wait for it to finish.".to_string());
    }
    let result = run_blocking(move || {
        let mut added = 0usize;
        for seg in to_vote {
            // Send ONLY this segment's sliced audio window to Scribe — never the whole source file. The
            // whole file would store a whole-recording transcript against one segment's `scribe-v1` vote
            // (corrupting consensus) and cost ~N× more. `segment_audio_as_wav_bytes` decodes (cached) and
            // slices by the segment alignment — the same window the T2 listener already sends.
            let wav = match crate::agentic::segment_audio_as_wav_bytes(&seg) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!("Scribe vote skipped: could not slice segment audio: {e}");
                    continue;
                }
            };
            match crate::scribe_api::transcribe_wav_bytes(
                &wav,
                "segment.wav",
                &key,
                crate::scribe_api::DEFAULT_MODEL,
                crate::scribe_api::SORANI_LANGUAGE_CODE,
            ) {
                Ok(transcript) => {
                    let hyp = crate::db::SegmentHypothesis {
                        segment_id: seg.id.clone(),
                        model_id: SCRIBE_VOTE_MODEL_ID.to_string(),
                        transcript,
                        confidence: None,
                    };
                    // Brief lock for the local insert only (same mutex + poison recovery as lock_db).
                    let db = db_arc.lock().unwrap_or_else(|poisoned| {
                        tracing::warn!("Recovering poisoned database lock");
                        poisoned.into_inner()
                    });
                    if let Err(e) = db.insert_hypothesis(&hyp) {
                        tracing::warn!("Failed to store Scribe vote: {e}");
                    } else {
                        added += 1;
                    }
                }
                Err(e) => tracing::warn!("Scribe vote failed for a segment: {e}"),
            }
        }
        Ok(added)
    })
    .await;
    SCRIBE_VOTES_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
    result
}

#[tauri::command]
pub async fn run_dpo_update(state: State<'_, AppState>, endpoint: String) -> Result<String, String> {
    RATE_LIMITER.check("run_dpo_update")?;
    // DPO POSTs private, transcript-derived preference pairs outbound — a parallel cloud-LLM channel,
    // so it requires the same explicit cloud-LLM opt-in (the endpoint allow-list is a separate,
    // non-consent control). Gate before building/serializing any of that private data — the consent
    // check runs EAGERLY here, so no request is ever offloaded without opt-in.
    require_cloud_llm_consent(&state)?;
    // Build + POST on a SEPARATE WAL connection (see open_jury_db_connection), never the global lock —
    // run_dpo_update performs a blocking outbound HTTP POST (up to ~120s on a stalled endpoint).
    // Open the connection eagerly, then move it into run_blocking so the POST runs on the blocking pool
    // — off the UI thread AND without holding lock_db(), so it never freezes the window or starves
    // other DB-touching IPCs (get_segments, search, ...). Database is Send (Connection + String).
    // Mirrors run_jury_pipeline / run_t2_for_segment.
    let db = open_jury_db_connection(&state)
        .ok_or_else(|| "App data directory is unavailable for the DPO update.".to_string())?;
    run_blocking(move || crate::jury::learning::run_dpo_update(&db, &endpoint).map_err(|e| e.to_string())).await
}

#[tauri::command]
pub async fn run_jury_pipeline(
    state: State<'_, AppState>,
    segment_ids: Vec<String>,
) -> Result<serde_json::Value, String> {
    STRICT_RATE_LIMITER.check("run_jury_pipeline")?;
    let settings = state.lock_settings().clone();
    // Run on a dedicated connection so the global db Mutex is not held across the jury's blocking T2
    // cloud calls — holding it would freeze the UI's get_segments for the whole run (JuryDbSource
    // retries the dedicated open and only falls back to the shared handle on a hard failure). The
    // whole T0→T1→T2 chain also runs on the blocking pool via run_blocking so the UI THREAD itself
    // stays responsive too (the T2 consent gate lives inside run_jury_pipeline_core_via, on settings
    // cloned eagerly here — cloud is never reached without jury_cloud_opt_in). All captures are owned:
    // settings clone, segment_ids, jury_data_dir, and the Send JuryDbSource (path + Arc handle).
    let jury_data_dir = state.lock_data_dir().clone();
    let source = jury_db_source(&state);
    run_blocking(move || {
        source.with(|db| run_jury_pipeline_core_via(db, &settings, segment_ids, jury_data_dir.as_deref()))
    })
    .await
}

/// `run_t2_for_segment` — run Gemini audio judge on a single segment directly.
///
/// Useful for re-running T2 from the Review Inbox or a manual trigger without
/// going through the full pipeline again.
#[tauri::command]
pub async fn run_t2_for_segment(
    state: State<'_, AppState>,
    segment_id: String,
    api_key: String,
) -> Result<crate::jury::t2_listener::T2Result, String> {
    STRICT_RATE_LIMITER.check("run_t2_for_segment")?;
    validate::validate_identifier(&segment_id)?;

    let settings = state.lock_settings().clone();
    let data_dir = state.lock_data_dir().clone();
    // T2 transport: direct Gemini (the passed key) by default, or OpenRouter (its key from secrets.env)
    // when the jury provider is "openrouter". `api_key`/`jury_model` are the resolved judge credentials.
    let (t2_endpoint, api_key, jury_model) = resolve_t2_endpoint(&settings, &api_key, data_dir.as_deref());
    // Floor at 3: self-consistency is meaningless below 3 samples, and a misconfigured 1 would let a
    // single Gemini sample masquerade as a "majority". majority_vote also requires >= 2 agreeing
    // samples, so this is defense in depth at the config boundary.
    let n_samples = (settings.jury_self_consistency_n as usize).max(3);
    let cloud_opt_in = settings.jury_cloud_opt_in;

    if !cloud_opt_in {
        return Err("Cloud opt-in is required for T2. Enable it in Settings → Listening Jury.".into());
    }
    if api_key.trim().is_empty() {
        return Err(
            "A judge API key is required for T2 (a Gemini key, or an OpenRouter key when the jury provider is OpenRouter)."
                .into(),
        );
    }

    // Gather every DB input under a BRIEF lock, then drop it before listen_and_judge. Holding the
    // global AppState db Mutex across the cloud T2 round-trip (n_samples Gemini audio calls) would
    // freeze every other DB-touching command app-wide for the whole network call. The lock is
    // re-acquired only for the final verdict write below.
    let (audio_b64, hyps, reference_report, t2_evidence, few_shots) = {
        let db = state.lock_db();
        let seg = db
            .get_segment_by_id(&segment_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Segment not found: {segment_id}"))?;

        // Base64-encode only the segment span for chunked long-form sources.
        let audio_b64 = crate::agentic::segment_audio_as_wav_base64(&seg)
            .map_err(|e| format!("Cannot prepare segment audio '{}': {e}", seg.audio_path))?;

        // Build a single hypothesis from raw transcript (T2 will hear the audio and judge)
        let mut hyps = db.get_hypotheses_for_segment(&segment_id).map_err(|e| e.to_string())?;
        if hyps.is_empty() {
            hyps.push(crate::db::SegmentHypothesis {
                segment_id: segment_id.clone(),
                model_id: "asr".into(),
                transcript: seg.raw_transcript.clone(),
                confidence: seg.confidence,
            });
        }

        let mut duration_cache = std::collections::HashMap::new();
        let mut identity_cache = std::collections::HashMap::new();
        let reference_report =
            reference_selection_for_segment(&db, &settings, &seg, &hyps, &mut duration_cache, &mut identity_cache)?;
        let t2_evidence = reference_report.as_ref().map(reference_selection_evidence).into_iter().collect::<Vec<_>>();

        let few_shots = crate::jury::get_few_shot_examples(&db, &segment_id, 5).map_err(|e| e.to_string())?;
        (audio_b64, hyps, reference_report, t2_evidence, few_shots)
    };

    // The gather block above released the global DB lock when it ended (all reads are done, few_shots
    // included), so the blocking T2 cloud call below (Gemini, n_samples retries — multiple seconds)
    // never starves other DB users like the UI's get_segments. It also runs on the blocking pool via
    // run_blocking so it never blocks the UI thread itself — the cloud_opt_in + api_key gates above
    // already ran eagerly, so no request is offloaded without consent. The verdict write re-acquires
    // the DB lock briefly on the caller thread after the await. All inputs are owned + moved in; only
    // reference_report/segment_id/result are used afterward.
    let result = run_blocking(move || {
        Ok(crate::jury::t2_listener::listen_and_judge_via(
            &t2_endpoint,
            &audio_b64,
            &hyps,
            &t2_evidence,
            &few_shots,
            &api_key,
            &jury_model,
            n_samples,
        ))
    })
    .await?;

    // If T2 produced a verdict, write it to the DB — but ONLY when the Autonomy Dial permits a MACHINE
    // commit (ActConfirm / ActAuto). Under Observe or Propose (the shipped default: "agent stages
    // verdicts; human confirms each one") a machine verdict must NOT be auto-committed — it is returned in
    // the T2Result for the human to confirm. This mirrors the pipeline chokepoint run_jury_pipeline_core_via,
    // which gates EVERY machine commit behind the same check; this direct IPC command (re-run T2 from the
    // Review Inbox) previously routed AROUND that gate and silently accepted a machine transcript under a
    // dial level that forbids any machine commit (round-24 hunt #1 fixed the pipeline path but named T2).
    let machine_commits_allowed = matches!(
        settings.jury_autonomy_level,
        crate::settings::AutonLevel::ActConfirm | crate::settings::AutonLevel::ActAuto
    );
    if machine_commits_allowed {
        if let Some(ref verdict) = result.verdict {
            let evidence_payload = match &reference_report {
                Some(report) => serde_json::json!({
                    "t2Evidence": verdict.evidence.clone(),
                    "referenceSelection": report,
                }),
                None => serde_json::json!(verdict.evidence.clone()),
            };
            let ev_json = serde_json::to_string(&evidence_payload)
                .map_err(|e| format!("Failed to serialize T2 evidence for {segment_id}: {e}"))?;
            // Re-acquire the lock only to persist the verdict.
            state
                .lock_db()
                .write_segment_verdict(
                    &segment_id,
                    "jury_accept",
                    Some(&verdict.transcript),
                    Some(&verdict.reason),
                    Some(ev_json.as_str()),
                    Some(verdict.confidence),
                    false,
                )
                .map_err(|e| e.to_string())?;
        }
    }

    Ok(result)
}
