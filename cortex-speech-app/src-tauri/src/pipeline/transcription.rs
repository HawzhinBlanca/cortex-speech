//! Champion transcription, optional refinement and hypothesis population.

use super::*;

impl ProcessingPipeline {
    /// Draft an unbound standalone audio file without publishing it to an existing segment. Existing
    /// segment callers must bind an immutable source and use the side-effect-free bound draft plus a
    /// database-owned commit boundary; accepting an id here would recreate the ID/path mix-up this
    /// split is designed to make unrepresentable.
    pub fn transcribe(
        &self,
        segment_id: Option<&str>,
        audio_path: &str,
        alignment_json: Option<&str>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> AppResult<TranscriptionDraft> {
        if segment_id.is_some() {
            return Err(AppError::Validation(
                "E_TRANSCRIPTION_SOURCE_UNBOUND: existing-segment transcription requires immutable source authority"
                    .into(),
            ));
        }
        self.transcribe_draft_only(None, audio_path, alignment_json, cancel)
    }

    /// Bind one already-imported segment to the exact database and decoded-PCM authority that will
    /// be rechecked after inference. A list row may omit alignment JSON, but any supplied caller
    /// value must still match; the database copy is always the value sent to the engine.
    pub(crate) fn bind_existing_transcription_source(
        &self,
        segment_id: &str,
        requested_audio_path: Option<&str>,
        requested_alignment_json: Option<&str>,
    ) -> AppResult<BoundTranscriptionSource> {
        self.bind_existing_transcription_source_inner(segment_id, requested_audio_path, requested_alignment_json, None)
    }

    /// Batch variant of [`Self::bind_existing_transcription_source`]. A 500-segment recording used
    /// to be decoded and hashed 500 times before inference; this shares exactly one verified lease
    /// per (stored path, canonical PCM hash) without weakening the per-segment database snapshot.
    pub(crate) fn bind_existing_transcription_source_cached(
        &self,
        segment_id: &str,
        requested_audio_path: Option<&str>,
        requested_alignment_json: Option<&str>,
        lease_cache: &TranscriptionSourceLeaseCache,
    ) -> AppResult<BoundTranscriptionSource> {
        self.bind_existing_transcription_source_inner(
            segment_id,
            requested_audio_path,
            requested_alignment_json,
            Some(lease_cache),
        )
    }

    fn bind_existing_transcription_source_inner(
        &self,
        segment_id: &str,
        requested_audio_path: Option<&str>,
        requested_alignment_json: Option<&str>,
        lease_cache: Option<&TranscriptionSourceLeaseCache>,
    ) -> AppResult<BoundTranscriptionSource> {
        let runtime = self.shared_database_runtime(&self.db_path)?;
        let snapshot = runtime
            .open_read()?
            .champion_transcription_source_snapshot(segment_id)?
            .ok_or_else(|| AppError::Validation(format!("Segment '{segment_id}' no longer exists")))?;

        if let Some(requested) = requested_audio_path {
            if requested != snapshot.segment.audio_path {
                return Err(AppError::Validation(format!(
                    "E_TRANSCRIPTION_SOURCE_CHANGED: segment '{segment_id}' now names a different audio path; reload it before transcribing"
                )));
            }
        }
        if let Some(requested) = requested_alignment_json {
            if snapshot.segment.alignment_json.as_deref() != Some(requested) {
                return Err(AppError::Validation(format!(
                    "E_TRANSCRIPTION_SOURCE_CHANGED: segment '{segment_id}' source span changed; reload it before transcribing"
                )));
            }
        }
        let expected_hash = snapshot.audio_content_hash.as_deref().ok_or_else(|| {
            AppError::Validation(format!(
                "E_TRANSCRIPTION_SOURCE_UNVERIFIED: segment '{segment_id}' has no canonical decoded-PCM identity; repair or re-import it before transcribing"
            ))
        })?;
        if !crate::db::is_canonical_audio_content_hash(expected_hash) {
            return Err(AppError::Validation(format!(
                "E_TRANSCRIPTION_SOURCE_UNVERIFIED: segment '{segment_id}' has a malformed decoded-PCM identity; repair or re-import it before transcribing"
            )));
        }
        let verify =
            || crate::media::verify_current_source_lease(Path::new(&snapshot.segment.audio_path), expected_hash);
        let source_lease = if let Some(cache) = lease_cache {
            let key = (snapshot.segment.audio_path.clone(), expected_hash.to_string());
            let verifier = {
                let mut entries = cache.lock().unwrap_or_else(|poisoned| {
                    tracing::warn!("Recovering poisoned batch transcription source-lease cache");
                    poisoned.into_inner()
                });
                Arc::clone(entries.entry(key).or_insert_with(|| Arc::new(OnceLock::new())))
            };
            verifier.get_or_init(verify).clone()
        } else {
            verify()
        }
        .map_err(|error| {
            AppError::Validation(format!(
                "E_TRANSCRIPTION_SOURCE_CHANGED: current audio for segment '{segment_id}' does not match its imported identity: {error}"
            ))
        })?;
        Ok(BoundTranscriptionSource { snapshot, _source_lease: source_lease })
    }

    /// Infer a draft for an immutable bound source without publishing any canonical truth.
    ///
    /// Batch orchestration uses this path so its durable journal and the final segment/hypothesis
    /// update can share one later transaction. The bound source still authorizes the exact segment,
    /// path, span and decoded PCM used by inference, but it grants no write capability here.
    pub(crate) fn transcribe_bound_draft_only(
        &self,
        source: &BoundTranscriptionSource,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> AppResult<TranscriptionDraft> {
        self.transcribe_draft_only(
            Some(&source.snapshot.segment.id),
            &source.snapshot.segment.audio_path,
            source.snapshot.segment.alignment_json.as_deref(),
            cancel,
        )
    }

    /// Import inference is side-effect-free. Canonical rows and any auxiliary hypotheses may only
    /// be published by the later import transaction after every chunk has completed.
    pub(super) fn transcribe_import_draft_only(
        &self,
        segment_id: &str,
        audio_path: &str,
        alignment_json: Option<&str>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> AppResult<TranscriptionDraft> {
        self.transcribe_draft_only(Some(segment_id), audio_path, alignment_json, cancel)
    }

    fn transcribe_draft_only(
        &self,
        segment_id: Option<&str>,
        audio_path: &str,
        alignment_json: Option<&str>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> AppResult<TranscriptionDraft> {
        let path = Path::new(audio_path);
        let duration_ms = audio::get_duration_ms(path)?;
        if duration_ms == 0 {
            return Err(AppError::Validation("Empty audio file".into()));
        }

        // CHAMPION FIRST, BEFORE ANY DECODE (2026-08-20 external review). The WSL-7B branch sends
        // the server a path + source window and never touches PCM — yet this function fully decoded
        // the source before selecting an engine, so every champion call on a 9 s clip cut from a
        // 77-minute episode decoded the whole episode in Rust and threw it away. Work must scale
        // with the CLIP, not the source. Only the fine-tuned override still needs PCM before the
        // engine choice, and it keeps its precedence below.
        if self.should_use_wsl_primary_asr() && !self.finetuned_override_active() {
            let runtime = self.shared_database_runtime(&self.db_path)?;
            let segment_queries = crate::stores::SegmentQueryStore::new(runtime.clone());
            let audio_path_str = path.to_string_lossy().to_string();

            let segment_id: Option<String> = if let Some(id) = segment_id {
                Some(id.to_string())
            } else {
                segment_queries.resolve_transcription_segment(&audio_path_str, alignment_json)?
            };

            if let Some(id) = segment_id {
                tracing::info!("Running WSL 7B ASR for segment ID: {}", id);

                // Tag a 7B failure (server down / timeout / empty) so the UI can offer a champion retry
                // — and NEVER silently fall through to a smaller model here.
                let wsl_result =
                    self.run_wsl_segment_transcript(audio_path, alignment_json, cancel).map_err(tag_7b_unavailable)?;
                let raw_transcript = wsl_result.raw_transcript.clone();
                let confidence = wsl_result.confidence;

                // A TRANSIENT empty 7B result (server up but under load) comes back as Ok("") — NOT an Err
                // — so the map_err(tag_7b_unavailable) above does not catch it. Do not let it fall through
                // to the write below: update_asr_transcript_if_unreviewed would replace a good, unverified
                // stored transcript with "" (silent data loss). Both re-transcribe entry points route
                // through here (batch_transcribe + the per-segment transcribe IPC) with no retry, unlike
                // the import path which retries/escalates for exactly this transient. Surface it as the
                // retryable 7B failure the tag above promises, leaving the existing transcript intact.
                if raw_transcript.trim().is_empty() {
                    return Err(tag_7b_unavailable(AppError::Other(format!(
                        "{WSL_7B_EMPTY_RESULT_MARKER} (the server is likely under load); the existing transcript is left unchanged"
                    ))));
                }

                let db = runtime.open_read()?;

                // Stage 2: Dual-Pass LLM Refinement (OpenRouter when configured + key present)
                let final_text = if let Some(refiner) = self.build_refiner()? {
                    tracing::info!("Running LLM refinement on {} bytes...", raw_transcript.len());
                    let refine_result = if self.settings.ger_refinement_enabled {
                        // Generative error correction: prime the refiner with the N-best (populated
                        // just above) + relevant past corrections (relevance-ranked few-shot).
                        // Context loads are best-effort — refinement legitimately proceeds unprimed
                        // (no hypotheses recorded is a normal state) — but a DB READ FAILURE must be
                        // logged, not folded into "no context": the old unwrap_or_default() made a
                        // persistent DB problem silently produce unprimed GER forever with no trace.
                        // Champion mode has exactly one ASR input. Use the in-memory 7B draft rather
                        // than rereading historical DB hypotheses: an older 300M/1B/MMS/Scribe row can
                        // never leak into refinement, and no partial provenance write is needed before
                        // the whole transcription/refinement result is ready to commit.
                        let hyps = vec![raw_transcript.clone()];
                        let few_shot: Vec<(String, String)> = match crate::jury::get_few_shot_examples(&db, &id, 3) {
                            Ok(examples) => examples.into_iter().map(|e| (e.wrong_transcript, e.human_fix)).collect(),
                            Err(e) => {
                                tracing::warn!(
                                    "GER: could not load few-shot corrections for {id}: {e}; refining unprimed"
                                );
                                Vec::new()
                            }
                        };
                        refiner.refine_with_context(&raw_transcript, &hyps, &few_shot)
                    } else {
                        refiner.refine_text(&raw_transcript)
                    };
                    match refine_result {
                        Ok(refined) => {
                            tracing::info!("LLM Refinement successful.");
                            accept_refinement(&raw_transcript, &refined)
                        }
                        Err(e) => {
                            // HARD STOP (owner rule 2026-08-11): a configured refiner that FAILS is a
                            // failure, not an invitation to ship the unrefined draft. Measured
                            // 2026-08-10: 59 of 487 clips silently kept raw text this way, so the
                            // dataset was part refined and part not with nothing recording which.
                            return Err(AppError::Other(format!(
                                "LLM refinement failed for segment {id}: {e}. Refinement is enabled, so this                                  clip is NOT complete — the run is stopped rather than storing an unrefined                                  draft as if it were finished."
                            )));
                        }
                    }
                } else {
                    raw_transcript.clone()
                };

                // LOOP 0: when enabled, correct previously-learned confusions in the final text
                // before it is returned/stored (opt-in; default off; best-effort).
                let final_text = apply_loop0_firing(self.settings.loop0_firing_enabled, &db, &final_text);

                // Inference returns a closed draft only. Single-clip, batch, and import orchestration
                // each own one later atomic publication boundary; this function cannot create a
                // transcript row or auxiliary hypothesis before that owner is ready.
                let cloud_call = self.llm_refinement_uses_cloud();
                drop(db);

                return Ok(TranscriptionDraft {
                    raw_text: raw_transcript,
                    final_text,
                    confidence,
                    confidence_source: Some("external_provider".to_string()),
                    model_version_id: Some(wsl_result.model_version_id),
                    deployment_sha256: Some(wsl_result.deployment_sha256),
                    cloud_call,
                });
            } else {
                return Err(AppError::Other(
                    "Segment not found in database. Please import the audio file first to generate speech segments."
                        .into(),
                ));
            }
        }

        let decode_timeout = Duration::from_secs((duration_ms as f64 / 1000.0 * 2.0).clamp(30.0, 3600.0) as u64);
        let (sample_rate, pcm) = audio::decode_to_pcm_with_timeout(path, decode_timeout)?;
        let (sample_rate, pcm) = audio::ensure_pcm_16khz(sample_rate, pcm)?;
        if pcm.is_empty() {
            return Err(AppError::Audio(crate::error::AudioError::EmptyBuffer));
        }

        let (chunk_pcm, chunk_suffix) = chunking::slice_pcm_by_alignment(&pcm, sample_rate, alignment_json)?;

        // Primary-engine override: when use_finetuned_asr is set, transcribe with the embedded
        // fine-tuned MMS-CTC engine (best local Sorani quality) regardless of asr_model_size. Any
        // failure (model absent / inference error / empty output) falls through to the configured
        // engine below, so transcription never breaks.
        if self.finetuned_override_active() {
            if let Some((onnx, vocab)) = Self::finetuned_model_paths() {
                match Self::transcribe_chunk_finetuned(&onnx, &vocab, &chunk_pcm) {
                    Ok(raw_text) if !raw_text.trim().is_empty() => {
                        let final_text = match self.build_refiner()? {
                            Some(refiner) => match refiner.refine_text(&raw_text) {
                                Ok(refined) => accept_refinement(&raw_text, &refined),
                                Err(_) => raw_text.clone(),
                            },
                            None => raw_text.clone(),
                        };
                        let final_text = self.fire_loop0_if_enabled(&final_text);
                        let cloud_call = self.llm_refinement_uses_cloud();
                        return Ok(TranscriptionDraft {
                            raw_text,
                            final_text,
                            confidence: None,
                            confidence_source: Some("fine_tuned_no_posterior".to_string()),
                            model_version_id: Some("finetuned-mms-ckb".to_string()),
                            deployment_sha256: None,
                            cloud_call,
                        });
                    }
                    Ok(_) => {
                        tracing::warn!("fine-tuned ASR returned empty output; falling back to the configured engine")
                    }
                    Err(e) => {
                        tracing::warn!("fine-tuned ASR failed ({e}); falling back to the configured engine")
                    }
                }
            } else {
                tracing::warn!(
                    "use_finetuned_asr is set but the fine-tuned model is absent; using the configured engine"
                );
            }
        }

        // F2: the fine-tuned override (above) and the WSL primary pass both declined; if WSL 7B is
        // the selected engine but unresolvable, fall-through to local CTC here would be the silent
        // downgrade. Refuse instead (covers manual per-segment re-transcribe, not just import).
        if self.wsl7b_primary_unresolved() {
            return Err(Self::primary_engine_unavailable_error());
        }

        let model_id = self.local_asr_model_id().to_string();
        if let Some(cached) = self.cache.get_chunk(path, &model_id, chunk_suffix.as_deref()) {
            // The cache stores the RAW ASR text (the key omits the refiner config), so re-run LLM
            // refinement + LOOP-0 with CURRENT settings — otherwise a refiner/settings change would be
            // ignored and the raw element would be contaminated with refined text.
            let raw = cached.raw_transcript.clone();
            let refined = match self.build_refiner()? {
                Some(refiner) => match refiner.refine_text(&raw) {
                    Ok(refined) => accept_refinement(&raw, &refined),
                    Err(_) => raw.clone(),
                },
                None => raw.clone(),
            };
            let fired = self.fire_loop0_if_enabled(&refined);
            return Ok(TranscriptionDraft {
                raw_text: raw,
                final_text: fired,
                confidence: None,
                confidence_source: Some("cache_replay".to_string()),
                model_version_id: Some(model_id.clone()),
                deployment_sha256: None,
                cloud_call: self.llm_refinement_uses_cloud(),
            });
        }

        let f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let (raw_text, confidence, confidence_source) = self.with_asr(|asr| {
            if !asr.is_available() {
                return Err(AppError::Other("ASR model not loaded".into()));
            }
            let timer = crate::inference::InferenceTimer::start("asr");
            let result = asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE);
            timer.finish(result.is_ok());
            result.map_err(AppError::Other)
        })?;

        // Stage 2: Dual-Pass LLM Refinement (OpenRouter when configured + key present)
        let final_text = if let Some(refiner) = self.build_refiner()? {
            tracing::info!("Running LLM refinement on {} bytes...", raw_text.len());
            match refiner.refine_text(&raw_text) {
                Ok(refined) => {
                    tracing::info!("LLM Refinement successful.");
                    accept_refinement(&raw_text, &refined)
                }
                Err(e) => {
                    // HARD STOP (owner rule 2026-08-11), same contract as the champion path above.
                    return Err(AppError::Other(format!(
                        "LLM refinement failed: {e}. Refinement is enabled, so this clip is NOT complete —                          the run is stopped rather than storing an unrefined draft as if it were finished."
                    )));
                }
            }
        } else {
            raw_text.clone()
        };

        // Only cache a GENUINE transcription — never an empty or placeholder result. ASR can legitimately
        // return Ok("") for a quiet-but-real chunk (and this path applies no RMS-normalize/denoise), so
        // without this guard an empty result is baked into the in-memory chunk cache and every later
        // "Re-run ASR" / batch_transcribe just replays the empty no-op instead of re-invoking the model.
        // Mirrors the same guard in build_segments_from_pcm.
        if !raw_text.trim().is_empty() && !crate::quality::is_placeholder_transcript(&raw_text) {
            let entry = crate::cache::CacheEntry {
                audio_hash: String::new(),
                // Cache the RAW ASR text, NOT the refined output: the cache key omits the refiner config,
                // so storing refined text would replay a stale refiner result (and contaminate the raw
                // element) on a later hit. Refinement is re-run per call from the cached raw text.
                raw_transcript: raw_text.clone(),
                normalized_transcript: None,
                created_at: chrono::Utc::now(),
                model_id: model_id.clone(),
            };
            self.cache.set_chunk(path, chunk_suffix.as_deref(), entry);
        }

        let final_text = self.fire_loop0_if_enabled(&final_text);
        Ok(TranscriptionDraft {
            raw_text,
            final_text,
            confidence,
            confidence_source: Some(confidence_source.as_db_value().to_string()),
            model_version_id: Some(model_id),
            deployment_sha256: None,
            cloud_call: self.llm_refinement_uses_cloud(),
        })
    }

    /// Apply LOOP-0 firing to a finalized transcript, opening a short-lived DB connection only when
    /// the opt-in is enabled (so the default-off path pays nothing). Best-effort — a db-open failure
    /// logs and returns the original text rather than failing transcription.
    pub(super) fn fire_loop0_if_enabled(&self, transcript: &str) -> String {
        if !self.settings.loop0_firing_enabled {
            return transcript.to_string();
        }
        match self.open_db() {
            Ok(db) => apply_loop0_firing(true, &db, transcript),
            Err(error) => {
                tracing::warn!("LOOP-0 firing skipped (could not open db): {error}");
                transcript.to_string()
            }
        }
    }

    /// Resolve the embedded fine-tuned MMS-CTC model (`finetuned-mms-ckb/{model.onnx,vocab.json}`)
    /// from the active (user) models dir, then the bundled one. `None` if it is not present.
    pub(super) fn finetuned_model_paths() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        // The search itself lives in `models.rs` so the offline diagnostic/evaluation callers share
        // one coherent root-selection rule.
        crate::models::finetuned_model_paths()
    }

    /// Transcribe one decoded chunk (16 kHz mono i16) with the fine-tuned engine. The fine-tuned
    /// model is trained on short utterances, so a single >~15 s pass can duplicate text — sub-split a
    /// long chunk into balanced ~15 s windows and join the per-window transcripts.
    // Shared by offline diagnostic/evaluation paths: a single unbounded pass over >15 s audio
    // duplicates text on this model, so every such path uses the same windowing.
    pub(crate) fn transcribe_chunk_finetuned(onnx: &Path, vocab: &Path, chunk_pcm: &[i16]) -> Result<String, String> {
        const MAX_WIN: usize = 15 * 16000;
        let f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let n = f32_pcm.len();
        if n == 0 {
            return Ok(String::new());
        }
        let n_win = n.div_ceil(MAX_WIN);
        let step = n.div_ceil(n_win);
        let mut out = String::new();
        let mut a = 0;
        while a < n {
            let b = (a + step).min(n);
            let part = crate::wav2vec2_asr::run_wav2vec2(onnx, vocab, "ckb", &f32_pcm[a..b])?;
            let part = part.trim();
            if !part.is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(part);
            }
            a = b;
        }
        Ok(out)
    }

    /// Whether LLM refinement may run under the CURRENT consent-gated settings. Consults the
    /// same `effective_llm_mode()` gate as `build_refiner` so every refinement decision point
    /// enforces the cloud (Gemini) opt-in (defense in depth): if a future path attempts
    /// refinement without going through `build_refiner`, this guard still blocks cloud use
    /// when the user has not opted in.
    pub(super) fn llm_refinement_permitted(&self) -> bool {
        if self.settings.effective_llm_mode() == crate::settings::LlmMode::None {
            return false;
        }
        // The live check applies ONLY to a path that actually leaves the machine. Local refinement
        // sends nothing anywhere, so gating it on cloud consent would break offline work for no
        // privacy gain — the withdrawal is about egress, not about refinement.
        !self.llm_refinement_uses_cloud() || self.consent.cloud_llm()
    }

    fn llm_refinement_uses_cloud(&self) -> bool {
        match self.settings.effective_llm_mode() {
            crate::settings::LlmMode::Gemini => true,
            crate::settings::LlmMode::Local => !self.settings.llm_endpoint_is_local(),
            crate::settings::LlmMode::None => false,
        }
    }

    /// Build the LLM refiner. When the configured mode is the cloud (Gemini) and an OPENROUTER_API_KEY
    /// is present in secrets.env, route through OpenRouter instead — it is verified working and
    /// reaches Gemini-class models, whereas direct Gemini is commonly 429 quota-blocked. Respects
    /// `None` (refinement disabled) and `Local` (the user's own endpoint).
    pub(super) fn build_refiner(&self) -> AppResult<Option<crate::llm_refiner::LlmRefiner>> {
        use crate::settings::LlmMode;
        // When the user has not opted into cloud LLM, `effective_llm_mode()` downgrades
        // Gemini -> None, so no refiner (and therefore no outbound cloud call) is ever
        // constructed. Mirrors the gate in `llm_refinement_permitted`.
        if !self.llm_refinement_permitted() {
            return Ok(None);
        }
        let refiner_from_settings = |mode: &LlmMode| {
            crate::llm_refiner::LlmRefiner::new(
                mode,
                self.settings.llm_endpoint.clone(),
                self.settings.llm_api_key.clone(),
                self.settings.llm_system_prompt.clone(),
                self.settings.llm_model.clone(),
            )
        };
        let refiner = match self.settings.effective_llm_mode() {
            LlmMode::None => None,
            LlmMode::Local => refiner_from_settings(&LlmMode::Local),
            LlmMode::Gemini => {
                // secrets.env lives in the app data dir, next to the database.
                if let Some(data_dir) = std::path::Path::new(&self.db_path).parent() {
                    if let Some(openrouter_key) = crate::api_keys::ApiKeys::load(data_dir)
                        .map_err(|error| {
                            AppError::Other(format!("Could not load the encrypted API-key store: {error}"))
                        })?
                        .openrouter
                    {
                        return Ok(crate::llm_refiner::LlmRefiner::for_openrouter(
                            openrouter_key,
                            // Pass the CONFIGURED model, not an empty string (which silently defaulted to
                            // openai/gpt-4o-mini — a different family than the "Gemini" mode the owner chose,
                            // with no provenance). Map it to an OpenRouter id; a local-only name falls back
                            // to the Gemini-class model the user expects.
                            openrouter_model_id(&self.settings.llm_model),
                            self.settings.llm_system_prompt.clone(),
                        ));
                    }
                }
                refiner_from_settings(&LlmMode::Gemini)
            }
        };
        Ok(refiner)
    }

    /// Explicit offline diagnostic evaluation. This is intentionally not registered as shipped IPC.
    pub fn run_gold_eval_local(&self, model_id: &str) -> AppResult<crate::eval::EvalRunResult> {
        // HONESTY GUARD (true-10 audit 2026-07-09): the eval row is persisted under `model_id`, so
        // the engine that transcribes MUST be derived from that id. Previously this always ran the
        // ACTIVE local engine and labeled the run with whatever the caller typed — a row labeled
        // "finetuned-mms-ckb" or "omniasr-wsl-7b" could be pure stock CTC output, a mislabeled
        // metric in the app's own honest-CER entrypoint. Only the locally runnable CTC engines are
        // accepted; anything else is an explicit error, never a silently mislabeled number.
        let model_size = match model_id {
            "omniasr-ctc-300m" => crate::settings::AsrModelSize::CTC300M,
            "omniasr-ctc-1b" => crate::settings::AsrModelSize::CTC1B,
            other => {
                return Err(AppError::Validation(format!(
                    "run_gold_eval_local can only run the local CTC engines it can label honestly \
                     (omniasr-ctc-300m, omniasr-ctc-1b); got '{other}'. Eval rows are persisted \
                     under this id, so the transcribing engine must match it exactly."
                )));
            }
        };
        // Open our own DB connection so no AppState lock is held across the (slow) decode+ASR loop —
        // mirrors run_gold_eval_asr. Holding the global db/pipeline mutexes here froze the whole UI.
        let db = self.open_db()?;

        let model_dir = self.root_for_size(&model_size);
        let config = asr::AsrLoadConfig {
            model_size,
            enable_gpu: self.settings.enable_gpu,
            num_threads: self.settings.num_asr_threads,
            language: self.settings.language.clone(),
        };
        self.asr_pool.warmup(&model_dir, &config)?;

        crate::eval::run_gold_eval_with_transcriber(&db, model_id, |gold| {
            let path = std::path::Path::new(&gold.audio_path);
            let (_sr, full_pcm) = audio::decode_to_pcm(path)?;

            let f32_pcm: Vec<f32> = full_pcm.iter().map(|&s| s as f32 / 32768.0).collect();

            self.asr_pool.with_service(&model_dir, &config, |asr| {
                if !asr.is_available() {
                    return Err(AppError::Other("ASR service unavailable".to_string()));
                }
                asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE)
                    .map(|(text, _confidence, _source)| text)
                    .map_err(AppError::Other)
            })
        })
    }

    pub fn populate_hypotheses(&self, db: &Database, segment_id: &str, f32_pcm: &[f32]) -> AppResult<()> {
        self.populate_hypotheses_reusing_primary(db, segment_id, f32_pcm, None)
    }

    pub(super) fn populate_hypotheses_reusing_primary(
        &self,
        db: &Database,
        segment_id: &str,
        f32_pcm: &[f32],
        primary: Option<PrimaryHypothesis<'_>>,
    ) -> AppResult<()> {
        // Guarded HERE rather than at the five call sites: one shared gate cannot be forgotten by a
        // sixth caller, and every caller wants the same answer. See `multi_engine_hypotheses` for
        // what these three engines cost when sherpa-onnx has no GPU (measured: 2.5 clips/minute).
        // The champion's own hypothesis is written by the transcribe path, not here, so turning this
        // off never leaves a clip without the transcript a reviewer is served.
        // The champion path stays single-engine even if a legacy settings file still carries the old
        // `multi_engine_hypotheses=true` default. The user's accuracy contract is explicit: when WSL7B
        // is selected, 300M/1B/MMS may not run automatically or influence the evidence mix. They remain
        // available only after selecting a non-champion engine and explicitly enabling this experiment.
        if !auxiliary_hypotheses_enabled(&self.settings) {
            return Ok(());
        }
        let import_writes = self.import_write_store(db.path())?;
        // 1. OmniASR 300M
        let model_id_300m = "omniasr-ctc-300m";
        let config_300m = asr::AsrLoadConfig {
            model_size: crate::settings::AsrModelSize::CTC300M,
            enable_gpu: self.settings.enable_gpu,
            num_threads: self.settings.num_asr_threads,
            language: self.settings.language.clone(),
        };
        let model_dir_300m = self.root_for_size(&config_300m.model_size);
        let res_300m = reuse_primary_or_infer(primary, model_id_300m, || {
            if !self.size_present(&config_300m.model_size) {
                return None;
            }
            self.asr_pool.with_service(&model_dir_300m, &config_300m, |asr| {
                if !asr.is_available() {
                    return None;
                }
                Some(asr.transcribe(f32_pcm, audio::TARGET_SAMPLE_RATE).map(|(text, conf, _source)| (text, conf)))
            })
        });
        match res_300m {
            Some(Ok((text, conf))) => {
                insert_hypothesis_checked(&import_writes, segment_id, model_id_300m, text, conf)?;
            }
            Some(Err(error)) => {
                tracing::warn!("{model_id_300m} hypothesis transcription failed for {segment_id}: {error}");
            }
            None => tracing::debug!("{model_id_300m} hypothesis model unavailable for {segment_id}"),
        }

        // 2. OmniASR 1B
        let model_id_1b = "omniasr-ctc-1b";
        let config_1b = asr::AsrLoadConfig {
            model_size: crate::settings::AsrModelSize::CTC1B,
            enable_gpu: self.settings.enable_gpu,
            num_threads: self.settings.num_asr_threads,
            language: self.settings.language.clone(),
        };
        let model_dir_1b = self.root_for_size(&config_1b.model_size);
        let res_1b = reuse_primary_or_infer(primary, model_id_1b, || {
            if !self.size_present(&config_1b.model_size) {
                return None;
            }
            self.asr_pool.with_service(&model_dir_1b, &config_1b, |asr| {
                if !asr.is_available() {
                    return None;
                }
                Some(asr.transcribe(f32_pcm, audio::TARGET_SAMPLE_RATE).map(|(text, conf, _source)| (text, conf)))
            })
        });
        match res_1b {
            Some(Ok((text, conf))) => {
                insert_hypothesis_checked(&import_writes, segment_id, model_id_1b, text, conf)?;
            }
            Some(Err(error)) => {
                tracing::warn!("{model_id_1b} hypothesis transcription failed for {segment_id}: {error}");
            }
            None => tracing::debug!("{model_id_1b} hypothesis model unavailable for {segment_id}"),
        }

        // 3. Fine-tuned MMS-CTC (ckb) — the machine's strongest INDEPENDENT local voter (wav2vec2 family,
        // ~21% CER), architecturally distinct from the correlated 300M/1B stock CTC pair. Its absence was
        // a root cause of "the jury escalates ~everything": two weak kin models rarely agree with the 7B,
        // so IRT confidence stays low and T0 almost never auto-accepts. Only runs when the fine-tuned
        // model is installed (a no-op otherwise); a failure is best-effort and never fails population.
        let model_id_finetuned = "finetuned-mms-ckb";
        let res_finetuned = reuse_primary_or_infer(primary, model_id_finetuned, || {
            let (onnx, vocab) = Self::finetuned_model_paths()?;
            let chunk_i16: Vec<i16> = f32_pcm.iter().map(|&s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16).collect();
            Some(Self::transcribe_chunk_finetuned(&onnx, &vocab, &chunk_i16).map(|text| (text, None)))
        });
        match res_finetuned {
            Some(Ok((text, _))) if !text.trim().is_empty() => {
                insert_hypothesis_checked(&import_writes, segment_id, model_id_finetuned, text, None)?;
            }
            Some(Ok(_)) => tracing::debug!("{model_id_finetuned} hypothesis empty for {segment_id}"),
            Some(Err(error)) => {
                tracing::warn!("{model_id_finetuned} hypothesis transcription failed for {segment_id}: {error}");
            }
            None => tracing::debug!("{model_id_finetuned} hypothesis model unavailable for {segment_id}"),
        }

        self.populate_wsl_hypothesis_if_configured(db, &import_writes, segment_id)?;

        Ok(())
    }

    fn populate_wsl_hypothesis_if_configured(
        &self,
        db: &Database,
        import_writes: &crate::stores::ImportWriteStore,
        segment_id: &str,
    ) -> AppResult<()> {
        if self.settings.asr_model_size == crate::settings::AsrModelSize::WSL7B {
            return Ok(());
        }
        if resolve_wsl_7b_client(self.settings.external_asr_script_path()).is_none() {
            return Ok(());
        }
        let Some(expected) = crate::registry::champion_identity(db, crate::deployment::OMNIASR_7B_FAMILY)? else {
            tracing::warn!("WSL 7B auxiliary hypothesis skipped: no registry champion identity is available");
            return Ok(());
        };
        if db
            .get_hypotheses_for_segment(segment_id)?
            .iter()
            .any(|hyp| hyp.model_id == expected.model_version_id && !hyp.transcript.trim().is_empty())
        {
            return Ok(());
        }

        let Some(seg) = db.get_segment_by_id(segment_id)? else {
            return Ok(()); // the row vanished between selection and this auxiliary pass
        };
        match self.run_wsl_segment_transcript(&seg.audio_path, seg.alignment_json.as_deref(), None) {
            Ok(result) => {
                if result.model_version_id != expected.model_version_id
                    || result.deployment_sha256 != expected.deployment_sha256
                {
                    return Err(AppError::Validation(
                        "MODEL_IDENTITY_CHANGED: WSL 7B auxiliary reply does not match the registry champion".into(),
                    ));
                }
                insert_hypothesis_checked(
                    import_writes,
                    segment_id,
                    &result.model_version_id,
                    result.raw_transcript,
                    result.confidence,
                )?;
            }
            Err(error) => {
                tracing::warn!("omniasr-wsl-7b hypothesis transcription failed for {segment_id}: {error}");
            }
        }
        Ok(())
    }

    pub(super) fn run_wsl_segment_transcript(
        &self,
        audio_path: &str,
        alignment_json: Option<&str>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> AppResult<Wsl7bResult> {
        // The resolvable client script stays the "champion is configured" signal (it gates
        // should_use_wsl_primary_asr and the whole fail-hard contract) — but the TRANSPORT no
        // longer spawns it (2026-08-20 external review): Rust already holds the path and the source
        // offsets the client re-derived by snapshot-copying the live DB into WSL per clip. The
        // script remains the manual/CLI transport (scorecards, the WSL console runner).
        if resolve_wsl_7b_client(self.settings.external_asr_script_path()).is_none() {
            return Err(AppError::Validation(
                "External ASR provider is not configured. Set the WSL script path in Settings before using the 7B provider.".into(),
            ));
        }
        run_wsl_segment_transcript_direct(audio_path, alignment_json, cancel)
    }
}

#[cfg(test)]
mod tests {
    //! Coverage for the pre-flight refusal, source-binding and gating arms that run without a live
    //! champion server: unbound-source refusals, snapshot/identity validation, engine-selection
    //! refusals, cloud-routing predicates and the auxiliary-hypothesis guards. Anything that would
    //! contact the WSL 7B socket is exercised only up to its refusal/early-return boundary.

    use super::*;
    use crate::cache::TranscriptCache;
    use crate::db::{Database, SegmentHypothesis, SpeechSegment};
    use crate::fingerprint::AudioFingerprint;
    use crate::models::ModelManager;
    use crate::normalizer::SoraniNormalizer;
    use crate::settings::{AppSettings, AsrModelSize, LlmMode};

    fn test_pipeline(settings: AppSettings) -> (ProcessingPipeline, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("db.sqlite").to_string_lossy().to_string();
        let pipeline = ProcessingPipeline::new(
            db_path,
            Arc::new(SoraniNormalizer::new()),
            Arc::new(TranscriptCache::new(16)),
            Arc::new(AudioFingerprint::new()),
            Arc::new(settings),
            Arc::new(ModelManager::new(dir.path().join("models"))),
        );
        (pipeline, dir)
    }

    fn write_sine_wav(path: &Path, samples: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..samples {
            let t = i as f64 / 16_000.0;
            writer.write_sample((8_000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn register_test_champion(db: &Database, id: &str) {
        crate::registry::register_candidate(
            db,
            &crate::registry::NewModelVersion {
                id: id.into(),
                family: "omniasr-7b".into(),
                model_card_name: Some("test/champion".into()),
                checkpoint_sha256: "a".repeat(64),
                checkpoint_path: "C:/models/test-champion.json".into(),
                source: "cortex-finetuned".into(),
                license: "test-only".into(),
            },
        )
        .unwrap();
        crate::registry::set_champion_for_test(db, id).unwrap();
    }

    #[test]
    fn transcribe_with_a_segment_id_requires_bound_source_authority() {
        let (pipeline, _dir) = test_pipeline(AppSettings::default());
        let error = pipeline
            .transcribe(Some("existing-segment"), "C:/anywhere/audio.wav", None, None)
            .expect_err("an existing-segment id must be refused on the unbound path")
            .to_string();
        assert!(error.contains("E_TRANSCRIPTION_SOURCE_UNBOUND"), "unexpected refusal: {error}");
    }

    #[test]
    fn binding_refuses_missing_span_drifted_and_unverified_segments() {
        let (pipeline, _dir) = test_pipeline(AppSettings::default());
        let db = pipeline.open_db().unwrap();
        db.initialize().unwrap();

        let error = pipeline.bind_existing_transcription_source("ghost", None, None).unwrap_err().to_string();
        assert!(error.contains("no longer exists"), "a vanished segment must fail closed: {error}");

        let stored_alignment = r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":1}"#;
        db.insert_segment(&SpeechSegment {
            id: "span-seg".into(),
            audio_path: "C:/recordings/span.wav".into(),
            raw_transcript: "دەقی نێوخۆیی".into(),
            duration_ms: 1_000,
            alignment_json: Some(stored_alignment.into()),
            ..SpeechSegment::default()
        })
        .unwrap();

        // A caller-supplied span that no longer matches the database copy is stale UI state.
        let drifted = r#"{"source_start_ms":100,"source_end_ms":900,"chunk_index":0,"chunk_count":1}"#;
        let error =
            pipeline.bind_existing_transcription_source("span-seg", None, Some(drifted)).unwrap_err().to_string();
        assert!(
            error.contains("E_TRANSCRIPTION_SOURCE_CHANGED") && error.contains("source span changed"),
            "unexpected span-drift refusal: {error}"
        );

        // A matching span but no canonical decoded-PCM identity: unverifiable, so unusable.
        let error = pipeline
            .bind_existing_transcription_source("span-seg", None, Some(stored_alignment))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("E_TRANSCRIPTION_SOURCE_UNVERIFIED") && error.contains("no canonical decoded-PCM identity"),
            "unexpected unverified refusal: {error}"
        );

        // A present-but-malformed identity is repaired, never trusted.
        db.connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash = 'definitely-not-canonical' WHERE id = 'span-seg'",
                [],
            )
            .unwrap();
        let error = pipeline.bind_existing_transcription_source("span-seg", None, None).unwrap_err().to_string();
        assert!(
            error.contains("E_TRANSCRIPTION_SOURCE_UNVERIFIED") && error.contains("malformed decoded-PCM identity"),
            "unexpected malformed-identity refusal: {error}"
        );
    }

    #[test]
    fn draft_refuses_missing_and_empty_audio_before_any_engine_choice() {
        let (pipeline, dir) = test_pipeline(AppSettings::default());

        let missing = dir.path().join("does-not-exist.wav");
        assert!(
            pipeline.transcribe(None, missing.to_str().unwrap(), None, None).is_err(),
            "a missing source file must fail before any engine work"
        );

        // A zero-sample WAV is refused with an empty-audio classification. Whether the duration
        // probe ("Empty audio file") or the decoder ("Empty audio buffer") trips first depends on
        // how the container reports frame counts; both are the same loud stop, never a blank draft.
        let empty = dir.path().join("empty.wav");
        write_sine_wav(&empty, 0);
        let error = pipeline.transcribe(None, empty.to_str().unwrap(), None, None).unwrap_err().to_string();
        assert!(error.contains("Empty audio"), "unexpected empty-audio refusal: {error}");
    }

    #[test]
    fn wsl_primary_draft_requires_exactly_one_imported_segment() {
        // Champion-primary transcription is DB-bound: with no imported segment there is nothing the
        // draft could be attributed to, and with an ambiguous path the caller must disambiguate.
        // Both refusals happen before any socket or subprocess is touched.
        let (pipeline, dir) =
            test_pipeline(AppSettings { asr_model_size: AsrModelSize::WSL7B, ..AppSettings::default() });
        let db = pipeline.open_db().unwrap();
        db.initialize().unwrap();
        let wav = dir.path().join("unimported.wav");
        write_sine_wav(&wav, 16_000);
        let wav_str = wav.to_string_lossy().to_string();

        let error = pipeline.transcribe(None, &wav_str, None, None).unwrap_err().to_string();
        assert!(
            error.contains("Segment not found in database"),
            "an unimported file must be refused with the import hint: {error}"
        );

        for id in ["shared-1", "shared-2"] {
            db.insert_segment(&SpeechSegment {
                id: id.into(),
                audio_path: wav_str.clone(),
                raw_transcript: "دەقی هاوبەش".into(),
                duration_ms: 1_000,
                ..SpeechSegment::default()
            })
            .unwrap();
        }
        let error = pipeline.transcribe(None, &wav_str, None, None).unwrap_err().to_string();
        assert!(
            error.contains("segments share this audio file"),
            "an ambiguous audio path must demand an explicit segment id: {error}"
        );
    }

    #[test]
    fn finetuned_chunk_transcription_of_empty_pcm_is_empty_without_touching_the_model() {
        let result = ProcessingPipeline::transcribe_chunk_finetuned(
            Path::new("C:/definitely/absent/model.onnx"),
            Path::new("C:/definitely/absent/vocab.json"),
            &[],
        )
        .expect("zero samples short-circuit before any model file is opened");
        assert_eq!(result, "");
    }

    #[test]
    fn gold_eval_local_refuses_engines_it_cannot_label_honestly() {
        let (pipeline, _dir) = test_pipeline(AppSettings::default());
        for dishonest in ["omniasr-wsl-7b", "finetuned-mms-ckb", "made-up-engine"] {
            let error = pipeline.run_gold_eval_local(dishonest).unwrap_err().to_string();
            assert!(
                error.contains("can only run the local CTC engines"),
                "'{dishonest}' must be refused, never mislabeled: {error}"
            );
            assert!(error.contains(dishonest), "the refusal must echo the rejected id: {error}");
        }
    }

    #[test]
    fn local_llm_mode_cloud_routing_depends_on_endpoint_locality_and_consent() {
        // Loopback endpoint: genuinely local, no consent needed, refiner built as configured.
        let (local, _dir_a) = test_pipeline(AppSettings {
            llm_mode: LlmMode::Local,
            llm_endpoint: "http://127.0.0.1:11434".into(),
            ..AppSettings::default()
        });
        assert!(!local.llm_refinement_uses_cloud());
        assert!(local.llm_refinement_permitted());
        let refiner = local.build_refiner().unwrap().expect("a local refiner must be constructed");
        assert!(refiner.endpoint.contains("127.0.0.1"), "unexpected endpoint: {}", refiner.endpoint);

        // "Local" mode pointed at a remote host without cloud consent is effectively cloud and is
        // downgraded to no refinement at all — text must not leave the machine.
        let (remote_unconsented, _dir_b) = test_pipeline(AppSettings {
            llm_mode: LlmMode::Local,
            llm_endpoint: "https://llm.example.com/v1".into(),
            cloud_llm_opt_in: false,
            ..AppSettings::default()
        });
        assert!(!remote_unconsented.llm_refinement_permitted());
        assert!(remote_unconsented.build_refiner().unwrap().is_none(), "no refiner without cloud consent");

        // The same remote endpoint WITH consent is a cloud call and is permitted as one.
        let (remote_consented, _dir_c) = test_pipeline(AppSettings {
            llm_mode: LlmMode::Local,
            llm_endpoint: "https://llm.example.com/v1".into(),
            cloud_llm_opt_in: true,
            ..AppSettings::default()
        });
        assert!(remote_consented.llm_refinement_uses_cloud());
        assert!(remote_consented.llm_refinement_permitted());
        let refiner = remote_consented.build_refiner().unwrap().expect("a consented remote refiner is built");
        assert!(refiner.endpoint.contains("llm.example.com"), "unexpected endpoint: {}", refiner.endpoint);
    }

    #[test]
    fn hypothesis_population_is_gated_off_in_champion_mode() {
        // Champion supremacy: even a legacy multi_engine_hypotheses=true settings file must not
        // let auxiliary engines run or write evidence when WSL7B is selected.
        let (pipeline, _dir) = test_pipeline(AppSettings {
            asr_model_size: AsrModelSize::WSL7B,
            multi_engine_hypotheses: true,
            ..AppSettings::default()
        });
        let db = pipeline.open_db().unwrap();
        db.initialize().unwrap();
        db.insert_segment(&SpeechSegment {
            id: "champion-gated".into(),
            audio_path: "C:/recordings/champion-gated.wav".into(),
            raw_transcript: "دەقی پاڵەوان".into(),
            duration_ms: 1_000,
            ..SpeechSegment::default()
        })
        .unwrap();

        pipeline.populate_hypotheses(&db, "champion-gated", &[0.0f32; 1_600]).unwrap();

        assert!(
            db.get_hypotheses_for_segment("champion-gated").unwrap().is_empty(),
            "champion mode must not mint auxiliary hypothesis evidence"
        );
    }

    #[test]
    fn wsl_auxiliary_hypothesis_skips_gracefully_without_champion_or_segment() {
        let (pipeline, _dir) =
            test_pipeline(AppSettings { asr_model_size: AsrModelSize::CTC300M, ..AppSettings::default() });
        let db = pipeline.open_db().unwrap();
        db.initialize().unwrap();
        let import_writes = pipeline.import_write_store(db.path()).unwrap();
        db.insert_segment(&SpeechSegment {
            id: "aux-seg".into(),
            audio_path: "C:/recordings/aux-seg.wav".into(),
            raw_transcript: "دەقی یاریدەدەر".into(),
            duration_ms: 1_000,
            ..SpeechSegment::default()
        })
        .unwrap();

        // No registry champion identity: the auxiliary pass logs and skips, never guesses.
        pipeline.populate_wsl_hypothesis_if_configured(&db, &import_writes, "aux-seg").unwrap();
        assert!(db.get_hypotheses_for_segment("aux-seg").unwrap().is_empty());

        // The champion already voted for this segment: no duplicate vote, no server call.
        register_test_champion(&db, "champ-aux");
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: "aux-seg".into(),
            model_id: "champ-aux".into(),
            transcript: "دەنگدانی ئامادە".into(),
            confidence: None,
        })
        .unwrap();
        pipeline.populate_wsl_hypothesis_if_configured(&db, &import_writes, "aux-seg").unwrap();
        let votes = db.get_hypotheses_for_segment("aux-seg").unwrap();
        assert_eq!(votes.len(), 1, "an existing champion vote must not be duplicated");
        assert_eq!(votes[0].transcript, "دەنگدانی ئامادە");

        // A row that vanished between selection and the auxiliary pass is a clean no-op.
        pipeline.populate_wsl_hypothesis_if_configured(&db, &import_writes, "aux-ghost").unwrap();
        assert!(db.get_hypotheses_for_segment("aux-ghost").unwrap().is_empty());
    }
}
