//! Unit tests for `pipeline.rs`, split out via `#[path]` (Week-4 decomposition) to keep pipeline.rs under the
//! 3-4k-line target. Included from pipeline.rs as `#[cfg(test)] #[path = "pipeline_tests.rs"] mod tests;`; `super::*`
//! still resolves to the parent module. Tests are UNCHANGED — only relocated.

use crate::audio::compute_waveform;
use crate::cache::TranscriptCache;
use crate::chunking::{slice_pcm_by_alignment, SegmentSourceMeta};
use crate::db::{Database, SegmentHypothesis, SourceTranscriptRecord, SpeechSegment};
use crate::fingerprint::AudioFingerprint;
use crate::models::ModelManager;
use crate::normalizer::SoraniNormalizer;
use crate::settings::{AppSettings, AsrModelSize};
use std::path::Path;
use std::sync::Arc;

#[test]
fn streaming_service_rebuild_is_bounded_to_once_per_file() {
    // P1.4b (audit R4): the streaming loop must build a denoiser/diarization service when unset, and
    // re-attempt a present-but-inactive one AT MOST ONCE PER FILE — never a full GPU-then-CPU ONNX load
    // on every 90 s window for an unloadable model. An ACTIVE service is always reused. args:
    // should_rebuild_streaming_service(present, active, already_tried).
    use super::should_rebuild_streaming_service as rebuild;
    assert!(rebuild(false, false, false), "unset -> build");
    assert!(rebuild(false, false, true), "unset -> build (a fresh/None guard always builds)");
    assert!(!rebuild(true, true, false), "present + active -> reuse, never rebuild");
    assert!(!rebuild(true, true, true), "present + active -> reuse");
    assert!(rebuild(true, false, false), "present + inactive + not tried yet -> attempt once");
    assert!(!rebuild(true, false, true), "present + inactive + already tried -> SKIP (no per-window reload)");
}

#[test]
fn primary_7b_failures_carry_the_ui_sentinel_and_never_leak_a_small_model() {
    use super::{tag_7b_unavailable, ASR_7B_UNAVAILABLE_TAG};
    use crate::error::AppError;

    // The "7B selected but not wired up" error the UI keys on to show retry-or-offline.
    let unavailable = super::ProcessingPipeline::primary_engine_unavailable_error().to_string();
    assert!(
        unavailable.contains(ASR_7B_UNAVAILABLE_TAG),
        "the UI relies on this sentinel to offer the choice instead of a dead-end: {unavailable}"
    );
    assert!(
        unavailable.to_lowercase().contains("champion") || unavailable.contains("OmniASR-7B"),
        "the error must name the champion so the user knows which engine is required: {unavailable}"
    );

    // A raw server-down failure gets tagged at the call site.
    let tagged = tag_7b_unavailable(AppError::Other("connection refused".into())).to_string();
    assert!(tagged.contains(ASR_7B_UNAVAILABLE_TAG));
    assert!(tagged.contains("connection refused"), "original cause is preserved for the log/detail");

    // Idempotent: an already-tagged error is not double-prefixed.
    let once = tag_7b_unavailable(AppError::Validation(format!("{ASR_7B_UNAVAILABLE_TAG}: x")));
    assert_eq!(
        once.to_string().matches(ASR_7B_UNAVAILABLE_TAG).count(),
        1,
        "double-tagging would corrupt the sentinel match"
    );
}

#[test]
fn resume_skips_persisted_but_unjournaled_file_to_avoid_duplicates() {
    use super::resume_should_skip_file;
    // A crash can land in the window between persist_segments (an atomic batch commit) and
    // mark_import_file_done: the file's segments are already in the DB, but the resume journal never
    // recorded it. On resume, that file MUST be skipped (its rows adopted) or it gets reprocessed and
    // every segment is DUPLICATED. This is the regression the resume-journal-gap fix closes.

    // Fresh import (not resuming): never skip, even if the path somehow already has rows — a fresh
    // import of a previously-imported directory is a legitimate (separate) re-import, not a resume.
    assert!(!resume_should_skip_file(false, false, false));
    assert!(!resume_should_skip_file(false, false, true), "a fresh import must never skip on pre-existing rows");

    // Resuming, journal recorded the file: skip (pre-existing P3.2 behavior, preserved).
    assert!(resume_should_skip_file(true, true, false));

    // Resuming, NOT journaled but segments already persisted: MUST skip. Before the fix this branch
    // returned false and the in-flight file was reprocessed into duplicate segments.
    assert!(
        resume_should_skip_file(true, false, true),
        "resume must skip a persisted-but-unjournaled file, else duplicate segments on re-import"
    );

    // Resuming, not journaled, no rows yet (a genuinely unprocessed file past the crash point): process it.
    assert!(!resume_should_skip_file(true, false, false));
}

#[test]
fn segment_id_by_alignment_distinguishes_no_row_from_db_error() {
    use super::resolve_segment_id_by_alignment;
    // With the schema present: a matching (audio_path, alignment_json) resolves to its id.
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let seg = SpeechSegment {
        id: "seg-a".into(),
        audio_path: "/audio/a.wav".into(),
        alignment_json: Some(r#"{"source_start_ms":0}"#.into()),
        ..Default::default()
    };
    db.insert_segment(&seg).unwrap();
    assert_eq!(
        resolve_segment_id_by_alignment(db.connection(), "/audio/a.wav", r#"{"source_start_ms":0}"#).unwrap(),
        Some("seg-a".to_string())
    );
    // No matching row is a legitimate None (caller falls through to "segment not found"), NOT an error.
    assert_eq!(
        resolve_segment_id_by_alignment(db.connection(), "/audio/a.wav", r#"{"nope":1}"#).unwrap(),
        None,
        "no matching row must be Ok(None), not an error"
    );
    // A REAL DB error (query against a connection with no speech_segments table) must PROPAGATE.
    // Before the fix, .ok() collapsed it to None, so a locked/IO/corrupt read told the user to
    // re-import an already-imported file and hid the fault. This is the regression assertion.
    let bare = rusqlite::Connection::open_in_memory().unwrap();
    assert!(
        resolve_segment_id_by_alignment(&bare, "/audio/a.wav", "{}").is_err(),
        "a real DB error must surface as Err, not masquerade as no-such-row None"
    );
}

#[test]
fn loop0_firing_respects_opt_in_and_fires_when_enabled() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    // Capture a CONFIRMED memory: the same confusion corrected on two segments -> hit_count 1,
    // past the anti-one-off guard.
    for id in ["pf-1", "pf-2"] {
        let seg = SpeechSegment {
            id: id.to_string(),
            audio_path: format!("/audio/{id}.wav"),
            raw_transcript: "ئەو ساڵە باش بوو".to_string(),
            ..Default::default()
        };
        db.insert_segment(&seg).unwrap();
        db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).unwrap();
    }
    let input = "ئەو ساڵە باش بوو";
    // Opt-in OFF -> the transcript is untouched.
    assert_eq!(super::apply_loop0_firing(false, &db, input), input, "firing must be off by default");
    // Opt-in ON -> the learned fix fires.
    assert_eq!(
        super::apply_loop0_firing(true, &db, input),
        "ئەو ساڵە خراپ بوو",
        "the learned correction must fire when enabled"
    );
}

#[test]
fn finetuned_downgrade_message_is_loud_only_when_fallbacks_happened() {
    // F2 no-silent-downgrade: silence is only allowed when nothing fell back. Partial and total
    // downgrades produce distinct, actionable messages with real counts.
    use super::ProcessingPipeline as P;
    assert_eq!(P::finetuned_downgrade_message(0, 0), None, "engine never selected -> silent");
    assert_eq!(P::finetuned_downgrade_message(25, 0), None, "all chunks drafted fine-tuned -> silent");
    let partial = P::finetuned_downgrade_message(10, 3).expect("partial downgrade is loud");
    assert!(partial.contains("3 of 10"), "partial message carries the real counts: {partial}");
    assert!(partial.contains("STOCK"), "names the engine that actually drafted: {partial}");
    let total = P::finetuned_downgrade_message(10, 10).expect("total downgrade is loud");
    assert!(total.contains("ALL 10"), "total downgrade says ALL with the count: {total}");
    assert!(total.contains("Verify model integrity"), "actionable next step included: {total}");
}

#[test]
fn loop0_would_fire_is_the_shadow_signal_independent_of_the_toggle() {
    // P1.3: the shadow predicate is true iff a confirmed correction memory would change the text —
    // regardless of the firing opt-in, and without mutating anything. This is what feeds C5.
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    for id in ["wf-1", "wf-2"] {
        let seg = SpeechSegment {
            id: id.to_string(),
            audio_path: format!("/audio/{id}.wav"),
            raw_transcript: "ئەو ساڵە باش بوو".to_string(),
            ..Default::default()
        };
        db.insert_segment(&seg).unwrap();
        db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).unwrap();
    }
    let mems = db.load_correction_memories().unwrap();
    assert!(super::loop0_would_fire(&mems, "ئەو ساڵە باش بوو"), "a matching memory would fire");
    assert!(!super::loop0_would_fire(&mems, "شتێکی جیاواز"), "unrelated text would not fire");
    assert!(!super::loop0_would_fire(&mems, "   "), "blank text never fires");
    assert!(!super::loop0_would_fire(&[], "ئەو ساڵە باش بوو"), "no memories -> never fires");

    // Whitespace normalization is NOT a memory firing. apply_memories rebuilds the text via
    // split_whitespace()+join(" "), so a draft with non-canonical whitespace (double space, leading/
    // trailing, tab) but NO matching memory would otherwise differ from its input and flip the shadow
    // signal true — inflating the C5 over-trigger honesty metric with pure whitespace edits.
    assert!(!super::loop0_would_fire(&mems, "شتێکی  جیاواز"), "a double space alone is not a firing");
    assert!(!super::loop0_would_fire(&mems, " شتێکی جیاواز "), "leading/trailing space alone is not a firing");
    assert!(!super::loop0_would_fire(&mems, "شتێکی\tجیاواز"), "a tab alone is not a firing");
    // A genuine memory match still fires even when the draft carries irregular whitespace around it.
    assert!(super::loop0_would_fire(&mems, "  ئەو ساڵە باش بوو  "), "a real match still fires despite padding");
}

#[test]
fn build_refiner_routes_gemini_through_openrouter_when_key_present() {
    use crate::settings::LlmMode;
    let settings = AppSettings { llm_mode: LlmMode::Gemini, cloud_llm_opt_in: true, ..AppSettings::default() };
    let (pipeline, dir) = test_pipeline_with_settings(settings);
    std::fs::write(dir.path().join("secrets.env"), "OPENROUTER_API_KEY=test-or-key\n").unwrap();
    let refiner = pipeline.build_refiner().expect("a refiner should be built");
    assert!(
        refiner.endpoint.contains("openrouter.ai"),
        "Gemini mode + OpenRouter key should route through OpenRouter, got: {}",
        refiner.endpoint
    );
    assert_ne!(refiner.model, "openai/gpt-4o-mini", "Gemini mode must not silently use gpt-4o-mini");
    assert!(refiner.model.contains("gemini"), "Gemini mode maps to a Gemini-class model, got: {}", refiner.model);
}

#[test]
fn openrouter_model_id_maps_families_and_defaults_to_gemini() {
    assert_eq!(super::openrouter_model_id("google/gemini-2.5-pro"), "google/gemini-2.5-pro");
    assert_eq!(super::openrouter_model_id("openai/gpt-4o"), "openai/gpt-4o");
    assert_eq!(super::openrouter_model_id("gemini-2.5-flash"), "google/gemini-2.5-flash");
    // A local-only name has no OpenRouter equivalent -> Gemini-class default, never gpt-4o-mini.
    assert_eq!(super::openrouter_model_id("heretic-final:latest"), "google/gemini-2.5-pro");
    assert_eq!(super::openrouter_model_id("  "), "google/gemini-2.5-pro");
}

#[test]
fn build_refiner_none_mode_disables_refinement() {
    use crate::settings::LlmMode;
    let (pipeline, _dir) =
        test_pipeline_with_settings(AppSettings { llm_mode: LlmMode::None, ..AppSettings::default() });
    assert!(pipeline.build_refiner().is_none());
}

#[test]
fn build_refiner_respects_cloud_opt_out() {
    use crate::settings::LlmMode;
    // Gemini selected but cloud NOT opted in -> no refiner, even with a key present (privacy).
    let (pipeline, dir) = test_pipeline_with_settings(AppSettings {
        llm_mode: LlmMode::Gemini,
        cloud_llm_opt_in: false,
        ..AppSettings::default()
    });
    std::fs::write(dir.path().join("secrets.env"), "OPENROUTER_API_KEY=test-or-key\n").unwrap();
    assert!(pipeline.build_refiner().is_none(), "cloud opt-out must disable refinement even with a key");
}

#[test]
fn build_scribe_segments_maps_timing_text_and_meta() {
    use crate::scribe_api::ScribeSegment;
    let segs = vec![
        ScribeSegment { text: "ئەمە یەکەمە".into(), source_start_ms: 0, source_end_ms: 1500 },
        ScribeSegment { text: "دووەم".into(), source_start_ms: 2000, source_end_ms: 3000 },
    ];
    let out = super::ProcessingPipeline::build_scribe_speech_segments(
        &segs,
        "/a/b.wav",
        5000,
        false,
        false,
        crate::scribe_api::DEFAULT_MODEL,
    );
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].raw_transcript, "ئەمە یەکەمە");
    assert_eq!(out[0].audio_path, "/a/b.wav");
    assert_eq!(out[0].duration_ms, 1500);
    assert!(!out[0].id.is_empty(), "each segment gets an id");
    assert!(out[0].normalized_transcript.is_none(), "auto_normalize=false -> no normalized text");
    let m0 =
        crate::chunking::SegmentSourceMeta::from_alignment_json(out[0].alignment_json.as_deref().unwrap()).unwrap();
    assert_eq!((m0.source_start_ms, m0.source_end_ms, m0.chunk_index, m0.chunk_count), (0, 1500, 0, 2));
    let m1 =
        crate::chunking::SegmentSourceMeta::from_alignment_json(out[1].alignment_json.as_deref().unwrap()).unwrap();
    assert_eq!((m1.source_start_ms, m1.source_end_ms), (2000, 3000));
}

#[test]
fn build_scribe_segments_fills_open_end_and_normalizes() {
    use crate::scribe_api::ScribeSegment;
    // An open end (0) extends to the file duration; auto-normalize folds the Arabic Kaf.
    let segs = vec![ScribeSegment { text: "كوردی".into(), source_start_ms: 1000, source_end_ms: 0 }];
    let out = super::ProcessingPipeline::build_scribe_speech_segments(
        &segs,
        "/x.wav",
        4000,
        true,
        false,
        crate::scribe_api::DEFAULT_MODEL,
    );
    assert_eq!(out.len(), 1);
    let m = crate::chunking::SegmentSourceMeta::from_alignment_json(out[0].alignment_json.as_deref().unwrap()).unwrap();
    assert_eq!((m.source_start_ms, m.source_end_ms), (1000, 4000), "open end clamps to duration");
    assert_eq!(out[0].duration_ms, 3000);
    let norm = out[0].normalized_transcript.as_deref().unwrap();
    assert_ne!(norm, "كوردی", "normalizer should fold the Arabic Kaf to Kurdish Kaf");
}

#[test]
fn build_scribe_segments_empty_input() {
    assert!(super::ProcessingPipeline::build_scribe_speech_segments(
        &[],
        "/x.wav",
        1000,
        false,
        false,
        crate::scribe_api::DEFAULT_MODEL
    )
    .is_empty());
}

#[test]
fn build_scribe_segments_no_overflow_on_extreme_timestamps() {
    use crate::scribe_api::ScribeSegment;
    // Untrusted timestamps (saturated i64) must not overflow-panic the duration math.
    let segs = vec![ScribeSegment { text: "x".into(), source_start_ms: i64::MIN, source_end_ms: i64::MAX }];
    let out = super::ProcessingPipeline::build_scribe_speech_segments(
        &segs,
        "/x.wav",
        1000,
        false,
        false,
        crate::scribe_api::DEFAULT_MODEL,
    );
    assert_eq!(out.len(), 1);
    assert!(out[0].duration_ms >= 0, "duration is never negative and never overflows");
}

#[test]
fn scribe_segments_round_trip_through_the_database() {
    // Smoke test: the Scribe import path persists segments AND their playback timing survives a
    // real DB round-trip (migrate -> insert_segments_batch -> read back). Covers what the pure
    // build test cannot: serialization of text, duration, and the SegmentSourceMeta alignment.
    use crate::scribe_api::ScribeSegment;
    let db = crate::db::Database::open(":memory:").expect("open in-memory db");
    db.initialize().expect("migrate schema");
    let scribe = vec![
        ScribeSegment { text: "ئەمە یەکەمە".into(), source_start_ms: 0, source_end_ms: 1500 },
        ScribeSegment { text: "دووەمین پارچە".into(), source_start_ms: 2000, source_end_ms: 5000 },
    ];
    let built = super::ProcessingPipeline::build_scribe_speech_segments(
        &scribe,
        "/audio/x.wav",
        6000,
        false,
        false,
        crate::scribe_api::DEFAULT_MODEL,
    );
    db.insert_segments_batch(&built).expect("insert batch");

    let mut back = db.get_segments(None).expect("read back");
    assert_eq!(back.len(), 2, "both Scribe segments persisted");
    let start_of = |s: &crate::db::SpeechSegment| {
        crate::chunking::SegmentSourceMeta::from_alignment_json(s.alignment_json.as_deref().unwrap_or(""))
            .map_or(i64::MAX, |m| m.source_start_ms)
    };
    back.sort_by_key(start_of);
    assert_eq!(back[0].raw_transcript, "ئەمە یەکەمە", "text survives the round-trip");
    assert_eq!(back[0].audio_path, "/audio/x.wav");
    assert_eq!(back[0].duration_ms, 1500);
    let m0 =
        crate::chunking::SegmentSourceMeta::from_alignment_json(back[0].alignment_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        (m0.source_start_ms, m0.source_end_ms, m0.chunk_index, m0.chunk_count),
        (0, 1500, 0, 2),
        "alignment time-range + chunk indices survive persistence — playback hits the right slice"
    );
    let m1 =
        crate::chunking::SegmentSourceMeta::from_alignment_json(back[1].alignment_json.as_deref().unwrap()).unwrap();
    assert_eq!((m1.source_start_ms, m1.source_end_ms), (2000, 5000));
}

#[test]
fn scribe_key_gate_requires_opt_in() {
    // Key present but cloud STT NOT opted in -> None (no cloud calls without explicit opt-in).
    let (pipeline, dir) =
        test_pipeline_with_settings(AppSettings { cloud_stt_opt_in: false, ..AppSettings::default() });
    std::fs::write(dir.path().join("secrets.env"), "ELEVENLABS_API_KEY=test-scribe-key\n").unwrap();
    assert!(pipeline.scribe_api_key_if_enabled().is_none());
}

#[test]
fn scribe_key_gate_returns_key_when_opted_in() {
    let (pipeline, dir) = test_pipeline_with_settings(AppSettings { cloud_stt_opt_in: true, ..AppSettings::default() });
    std::fs::write(dir.path().join("secrets.env"), "ELEVENLABS_API_KEY=test-scribe-key\n").unwrap();
    assert_eq!(pipeline.scribe_api_key_if_enabled().as_deref(), Some("test-scribe-key"));
}

#[test]
fn scribe_key_gate_none_without_key() {
    let (pipeline, _dir) =
        test_pipeline_with_settings(AppSettings { cloud_stt_opt_in: true, ..AppSettings::default() });
    assert!(pipeline.scribe_api_key_if_enabled().is_none(), "opted in but no key -> None");
}

#[test]
fn cancelled_directory_import_clears_running_status() {
    // Round-2 audit MEDIUM: a cancel mid directory-import early-returned (token.check()?) before
    // finish_import_status, so get_import_status().running stayed true forever. The RAII guard now
    // clears it on every exit path.
    let (pipeline, dir) = test_pipeline_with_settings(AppSettings::default());
    let import_dir = dir.path().join("to_import");
    std::fs::create_dir_all(&import_dir).unwrap();
    std::fs::write(import_dir.join("a.wav"), b"not-real-audio").unwrap();

    let token = crate::cancel::CancellationToken::new();
    token.cancel(); // pre-cancel so the loop's first token.check()? returns Err before any decode

    let res = pipeline.import_directory_with_agent_run_id(&import_dir, Some(token), None, None, |_evt| {});
    assert!(res.is_err(), "a cancelled import returns Err");
    assert!(!pipeline.import_status().running, "running must be cleared after a cancelled import");
}

#[test]
fn fire_loop0_if_enabled_method_uses_the_pipelines_own_db() {
    // The method (used by the cached / non-WSL transcribe paths) opens its own DB connection.
    let settings = AppSettings { loop0_firing_enabled: true, ..AppSettings::default() };
    let (pipeline, _dir) = test_pipeline_with_settings(settings);
    {
        let db = pipeline.open_db().unwrap();
        db.initialize().unwrap();
        for id in ["fm-1", "fm-2"] {
            let seg = SpeechSegment {
                id: id.to_string(),
                audio_path: format!("/a/{id}.wav"),
                raw_transcript: "ئەو ساڵە باش بوو".to_string(),
                ..Default::default()
            };
            db.insert_segment(&seg).unwrap();
            db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).unwrap();
        }
    }
    assert_eq!(
        pipeline.fire_loop0_if_enabled("ئەو ساڵە باش بوو"),
        "ئەو ساڵە خراپ بوو",
        "the method fires the learned fix using the pipeline's own db when enabled"
    );

    // With the opt-in off, the method is a pure no-op (and never even opens the db).
    let (off_pipeline, _dir2) =
        test_pipeline_with_settings(AppSettings { loop0_firing_enabled: false, ..AppSettings::default() });
    assert_eq!(
        off_pipeline.fire_loop0_if_enabled("ئەو ساڵە باش بوو"),
        "ئەو ساڵە باش بوو",
        "the method is a no-op when the opt-in is off"
    );
}

#[test]
fn the_gemini_key_comes_from_the_encrypted_store_not_the_scrubbed_setting() {
    // REGRESSION, and it made the feature unusable rather than merely awkward.
    //
    // `ensure_source_reference_transcripts` read `settings.llm_api_key`. `AppSettings::load` CLEARS
    // that field and rewrites settings.json, so a plaintext key never survives on disk (P0.3) - which
    // means the field is empty on every run after the one where it was typed. The whole-file reference
    // therefore failed with "Gemini API key is required" no matter what the owner entered, while
    // `llm_api_key_configured` stayed true so the UI reported a key that was gone. Measured 2026-08-10:
    // an import failed 3/3 on exactly that error, with secrets.env holding an EMPTY GEMINI_API_KEY
    // because no UI path had ever written one.
    //
    // Every other cloud path (scribe, jury, OpenRouter) reads secrets.env via ApiKeys. This pins that
    // the reference transcript now agrees with them.
    let (pipeline, dir) = test_pipeline_with_settings(AppSettings::default());
    let data_dir = dir.path();

    // Nothing anywhere -> None, so the caller can say so instead of calling Gemini with "".
    assert_eq!(pipeline.jury_cloud_api_key(), None, "no key anywhere must resolve to None");

    // A key in the ENCRYPTED store is found. Plaintext in secrets.env is a supported form
    // (parse_env_file reads both), which keeps this test independent of DPAPI availability.
    //
    // SETTLE LOOP, not decoration. Writing a file and immediately reading it back through other code
    // is a known flake on this box: it passes when run alone and fails under the full 1159-test suite,
    // which is exactly what this test did on its first full run. The repo's resolve_file_in tests carry
    // the same loop for the same reason. Reading through jury_cloud_api_key (not fs::read) means the
    // wait ends when the code under test can actually see the key.
    std::fs::write(data_dir.join("secrets.env"), "GEMINI_API_KEY=AIzaFromTheStore\n").unwrap();
    let mut seen = None;
    for _ in 0..500 {
        seen = pipeline.jury_cloud_api_key();
        if seen.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        seen.as_deref(),
        Some("AIzaFromTheStore"),
        "the store is where every other cloud path looks, and now this one too"
    );

    // BEFORE THE FIX this whole test is unreachable: the code read settings.llm_api_key, which is
    // "" on a default AppSettings, so it would have returned None here and failed the import.
    let typed = AppSettings { llm_api_key: "AIzaJustTyped".to_string(), ..AppSettings::default() };
    let (typed_pipeline, typed_dir) = test_pipeline_with_settings(typed);
    assert_eq!(
        typed_pipeline.jury_cloud_api_key().as_deref(),
        Some("AIzaJustTyped"),
        "a key typed in THIS session must still work before any reload scrubs it - refusing it would \
         be a surprising 'I just entered it' failure"
    );
    drop(typed_dir);
}

fn test_pipeline_with_settings(settings: AppSettings) -> (super::ProcessingPipeline, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("db.sqlite").to_string_lossy().to_string();
    let models_dir = dir.path().join("models");
    let pipeline = super::ProcessingPipeline::new(
        db_path,
        Arc::new(SoraniNormalizer::new()),
        Arc::new(TranscriptCache::new(10)),
        Arc::new(AudioFingerprint::new()),
        Arc::new(settings),
        Arc::new(ModelManager::new(models_dir)),
    );
    (pipeline, dir)
}

fn test_pipeline_for_status() -> (super::ProcessingPipeline, tempfile::TempDir) {
    test_pipeline_with_settings(AppSettings::default())
}

#[test]
fn wsl_without_script_uses_local_asr_fallback() {
    let settings = AppSettings { asr_model_size: AsrModelSize::WSL7B, ..AppSettings::default() };
    let (pipeline, _dir) = test_pipeline_with_settings(settings);

    assert!(!pipeline.should_use_wsl_primary_asr());
    // Probe PER FILE, exactly as production now does. Computing the expectation from the
    // all-or-nothing `resolved_dir()` baked the resolver bug into the test: with an empty user dir
    // and a bundled CTC-1B present, it predicted CTC300M while the app correctly selected CTC1B.
    let has_1b = crate::asr::omniasr_model_present(
        &pipeline.model_manager.resolve_root_for("omniasr-ctc-1b/model.int8.onnx"),
        &AsrModelSize::CTC1B,
    );
    let expected_size = if has_1b { AsrModelSize::CTC1B } else { AsrModelSize::CTC300M };
    let expected_id = if has_1b { "omniasr-ctc-1b" } else { "omniasr-ctc-300m" };
    assert_eq!(pipeline.active_local_asr_model_size(), expected_size);
    assert_eq!(pipeline.local_asr_model_id(), expected_id);
}

#[test]
fn wsl_with_script_uses_wsl_primary_asr() {
    let settings = AppSettings {
        asr_model_size: AsrModelSize::WSL7B,
        external_asr_script_path: "/root/cortex_env/omniasr.py".to_string(),
        ..AppSettings::default()
    };
    let (pipeline, _dir) = test_pipeline_with_settings(settings);

    assert!(pipeline.should_use_wsl_primary_asr());
}

fn test_segment(id: &str) -> SpeechSegment {
    SpeechSegment {
        id: id.to_string(),
        created_at: None,
        audio_path: format!("{id}.wav"),
        raw_transcript: "raw transcript".to_string(),
        normalized_transcript: None,
        annotated_transcript: None,
        alignment_json: None,
        duration_ms: 4000,
        speaker_id: None,
        verified: false,
        confidence: Some(0.9),
        ctc_score: None,
        clipping_ratio: Some(0.0),
        rms_db: Some(-20.0),
        snr_db: Some(20.0),
        split: Some("train".to_string()),
        signal_anomaly_score: None,
        verdict: None,
        verdict_transcript: None,
        rationale: None,
        evidence_json: None,
        agreement_score: None,
        escalated: false,
        human_decision: None,
        corrected_at: None,
        is_gold: false,
        alignment_quality: None,
        ..SpeechSegment::default()
    }
}

fn insert_hypothesis(db: &Database, segment_id: &str, model_id: &str, transcript: &str) {
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: segment_id.to_string(),
        model_id: model_id.to_string(),
        transcript: transcript.to_string(),
        confidence: Some(0.9),
    })
    .unwrap();
}

fn source_record_for_audio(
    audio_path: &Path,
    model_id: &str,
    transcript_path: &Path,
    transcript_text: &str,
) -> SourceTranscriptRecord {
    let identity = super::source_audio_identity(audio_path).expect("hash test audio");
    SourceTranscriptRecord {
        audio_path: audio_path.to_string_lossy().to_string(),
        model_id: model_id.to_string(),
        audio_content_hash: Some(identity.content_hash),
        audio_size_bytes: Some(identity.size_bytes),
        transcript_path: transcript_path.to_string_lossy().to_string(),
        transcript_text: transcript_text.to_string(),
        created_at: None,
    }
}

#[test]
fn multi_model_hypothesis_stage_reports_verified_coverage() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let segment = test_segment("covered-seg");
    db.insert_segment(&segment).unwrap();
    insert_hypothesis(&db, &segment.id, "omniasr-wsl-7b", "best phrase");
    insert_hypothesis(&db, &segment.id, "omniasr-ctc-300m", "backup phrase");

    let event = super::multi_model_hypothesis_stage(&db, "covered.wav", std::slice::from_ref(&segment));

    match event {
        super::PipelineEvent::AgentStage { stage, status, detail, current, total, .. } => {
            assert_eq!(stage, "multi_model_hypotheses");
            assert_eq!(status, "completed");
            assert_eq!(current, 1);
            assert_eq!(total, 1);
            assert!(detail.contains("omniasr-wsl-7b"));
            assert!(detail.contains("omniasr-ctc-300m"));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn multi_model_hypothesis_stage_blocks_incomplete_coverage() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let covered = test_segment("covered-seg");
    let blocked = test_segment("blocked-seg");
    db.insert_segment(&covered).unwrap();
    db.insert_segment(&blocked).unwrap();
    insert_hypothesis(&db, &covered.id, "omniasr-wsl-7b", "best phrase");
    insert_hypothesis(&db, &covered.id, "omniasr-ctc-300m", "backup phrase");
    insert_hypothesis(&db, &blocked.id, "omniasr-wsl-7b", "single model phrase");

    let event = super::multi_model_hypothesis_stage(&db, "mixed.wav", &[covered, blocked]);

    match event {
        super::PipelineEvent::AgentStage { stage, status, detail, current, total, .. } => {
            assert_eq!(stage, "multi_model_hypotheses");
            assert_eq!(status, "blocked");
            assert_eq!(current, 1);
            assert_eq!(total, 2);
            assert!(detail.contains("Only 1/2"));
            assert!(detail.contains("blocked-seg"));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn import_status_recovers_poisoned_lock() {
    let (pipeline, _dir) = test_pipeline_for_status();
    let handle = pipeline.import_status_handle();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = handle.lock().expect("lock import status");
        panic!("poison import status");
    }));

    pipeline.set_import_status(3, 7, "file.wav");
    let status = pipeline.import_status();
    assert!(status.running);
    assert_eq!(status.current, 3);
    assert_eq!(status.total, 7);
    assert_eq!(status.file, "file.wav");

    pipeline.finish_import_status();
    assert!(!pipeline.import_status().running);
}

#[test]
fn service_locks_recover_poisoned_state() {
    let (pipeline, _dir) = test_pipeline_for_status();

    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = pipeline.diarization_service.lock().expect("lock diarization service");
        panic!("poison diarization service");
    }));
    assert!(pipeline.lock_diarization_service().is_none());

    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = pipeline.denoiser_service.lock().expect("lock denoiser service");
        panic!("poison denoiser service");
    }));
    assert!(pipeline.lock_denoiser_service().is_none());
}

#[test]
fn decoded_window_accumulator_recovers_poisoned_lock() {
    let windows =
        std::sync::Mutex::new(vec![crate::audio::PcmWindow { offset_ms: 0, sample_rate: 16_000, pcm: vec![1, 2, 3] }]);

    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = windows.lock().expect("lock decoded windows");
        panic!("poison decoded windows");
    }));

    super::lock_decoded_windows(&windows).push(crate::audio::PcmWindow {
        offset_ms: 1000,
        sample_rate: 16_000,
        pcm: vec![4, 5, 6],
    });

    let recovered = super::lock_decoded_windows(&windows).clone();
    assert_eq!(recovered.len(), 2);
    assert_eq!(recovered[1].offset_ms, 1000);
    assert_eq!(recovered[1].pcm, vec![4, 5, 6]);
}

#[test]
fn wsl_primary_import_pass_skips_missing_script_after_local_fallback() {
    let settings = AppSettings { asr_model_size: AsrModelSize::WSL7B, ..AppSettings::default() };
    let (pipeline, dir) = test_pipeline_with_settings(settings);
    let db_path = dir.path().join("db.sqlite");
    let db = Database::open(db_path.to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let segment = SpeechSegment {
        id: "local-fallback".to_string(),
        audio_path: "C:\\missing\\audio.wav".to_string(),
        raw_transcript: "local fallback transcript".to_string(),
        duration_ms: 1000,
        ..SpeechSegment::default()
    };
    db.insert_segment(&segment).unwrap();

    let mut segments = vec![segment];
    let updated = pipeline.run_primary_wsl_pass_for_import(&db, &mut segments, None).unwrap();

    assert_eq!(updated, 0);
    assert_eq!(segments[0].raw_transcript, "local fallback transcript");
    assert_eq!(segments[0].verdict, None);
    assert!(!segments[0].escalated);
    assert_eq!(segments[0].rationale, None);

    let fresh = db.get_segments_by_ids(&["local-fallback".to_string()]).unwrap().remove(0);
    assert_eq!(fresh.raw_transcript, "local fallback transcript");
    assert_eq!(fresh.verdict, None);
    assert!(!fresh.escalated);
    assert_eq!(fresh.rationale, None);
}

#[test]
fn wsl_primary_import_pass_cancels_and_rolls_back_on_infrastructure_failure() {
    // The owner forces the 7B Champion: if the 7B path errors (server down / unreachable / hung —
    // here simulated by a missing audio file so transcribe() fails BEFORE spawning any subprocess),
    // the WHOLE import is cancelled and every segment it just created is rolled back, rather than
    // leaving a library of "[Pending WSL 7B ASR]" placeholders or silently downgrading. This locks
    // in that fail-hard contract AND the cascade cleanup that leaves no orphaned hypothesis rows.
    let settings = AppSettings {
        asr_model_size: AsrModelSize::WSL7B,
        // Non-empty => should_use_wsl_primary_asr() is true so the pass actually runs (it Ok(0)-skips
        // otherwise). The script never executes: transcribe() errors earlier on the missing audio.
        external_asr_script_path: "cortex_7b_client.py".to_string(),
        ..AppSettings::default()
    };
    let (pipeline, dir) = test_pipeline_with_settings(settings);
    let db = Database::open(dir.path().join("db.sqlite").to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    // Two freshly-imported segments whose audio file does not exist => transcribe() fails hard
    // (get_duration_ms on a missing path) on every attempt = an infrastructure failure, not a
    // reachable-but-silent clip.
    let missing_audio = dir.path().join("does-not-exist.wav").to_string_lossy().to_string();
    let mut segments = Vec::new();
    for i in 0..2 {
        let seg = SpeechSegment {
            id: format!("import-{i}"),
            audio_path: missing_audio.clone(),
            raw_transcript: "draft text".to_string(),
            duration_ms: 1000,
            ..SpeechSegment::default()
        };
        db.insert_segment(&seg).unwrap();
        segments.push(seg);
    }
    // A stale hypothesis on the first segment must be cascade-deleted with it (no orphan rows).
    insert_hypothesis(&db, "import-0", "omniasr-wsl-7b", "stale draft");

    let import_ids: Vec<String> = segments.iter().map(|s| s.id.clone()).collect();
    let result = pipeline.run_primary_wsl_pass_for_import(&db, &mut segments, None);

    // The import is cancelled (fail-hard), not silently downgraded to a weaker engine.
    let msg = result.expect_err("a 7B infrastructure failure must cancel the import").to_string();
    assert!(msg.contains("rolled back"), "error must state the rollback: {msg}");
    assert!(msg.contains("7B server"), "error must name the 7B server: {msg}");

    // Every segment the import created is gone, and the cascade removed the hypothesis with it.
    assert!(
        db.get_segments_by_ids(&import_ids).unwrap().is_empty(),
        "all import segments must be deleted after a fail-hard cancel"
    );
    assert!(
        db.get_hypotheses_for_segment("import-0").unwrap().is_empty(),
        "segment_hypotheses must cascade-delete with the rolled-back segment (no orphans)"
    );
}

#[test]
fn parses_wsl_segment_result_from_stdout_marker() {
    let stdout = "loading model\n__RESULT__={\"raw_transcript\":\"دەقی دروست\",\"confidence\":0.94}\ndone\n";
    let (text, confidence) = super::parse_wsl_segment_result(stdout).unwrap();
    assert_eq!(text, "دەقی دروست");
    assert_eq!(confidence, Some(0.94));
}

#[test]
fn rejects_missing_wsl_segment_result_stdout_marker() {
    // No __RESULT__ line at all is the real infrastructure failure. (An empty-but-PRESENT result
    // is legitimate and returns Ok — see empty_7b_result_is_legitimate_not_infra_failure.)
    let err = super::parse_wsl_segment_result("loading model\nfinished without result\n").unwrap_err();
    assert!(err.to_string().contains("did not return a __RESULT__ line"));
}

#[test]
fn refinement_guard_keeps_small_edits_and_rejects_hallucinations() {
    // A light post-edit (fix one letter) is well within the CER bound => accepted.
    let raw = "ئەم دەقە دروستە بۆ تاقیکردنەوە";
    let small_edit = "ئەم دەقە دروستە بۆ تاقیکردنەوەی";
    assert_eq!(super::accept_refinement(raw, small_edit), small_edit);

    // An empty refinement is never accepted (no-blank guard) => raw is kept.
    assert_eq!(super::accept_refinement(raw, "   "), raw);

    // A wholesale rewrite far from the audio-grounded raw (CER > 0.6) is a hallucination => raw kept.
    let hallucination = "abcdefghij klmnopqrst uvwxyz 1234567890 zzzz";
    assert_eq!(super::accept_refinement(raw, hallucination), raw);
}

#[test]
fn wsl7b_without_script_is_unresolved_not_silently_downgraded() {
    // F2: WSL 7B selected + empty script + fine-tuned engine off => the ONLY thing left is stock
    // local CTC, which the owner never chose. The primary path must report "unresolved" so it
    // fails loudly instead of silently transcribing with 300M.
    let settings = AppSettings {
        asr_model_size: AsrModelSize::WSL7B,
        external_asr_script_path: String::new(),
        use_finetuned_asr: false,
        ..AppSettings::default()
    };
    let (pipeline, _dir) = test_pipeline_with_settings(settings);
    assert!(pipeline.wsl7b_primary_unresolved(), "WSL7B + no script + no finetuned must be unresolved");
    assert!(!pipeline.should_use_wsl_primary_asr(), "no script => not the WSL primary path");
    let msg = super::ProcessingPipeline::primary_engine_unavailable_error().to_string();
    assert!(
        msg.contains(super::ASR_7B_UNAVAILABLE_TAG),
        "error must carry the UI sentinel so the app offers retry-or-offline, not a dead-end: {msg}"
    );
    assert!(
        msg.contains("offline model"),
        "error must name the offline-model choice the owner can deliberately pick: {msg}"
    );
    assert!(msg.contains("silently downgrade"), "error must state the no-downgrade contract: {msg}");
}

#[test]
fn preflight_is_noop_when_not_wsl_primary() {
    // Default engine is not the WSL primary (empty script) => preflight returns Ok without ever
    // spawning wsl, so a machine without WSL is unaffected.
    let (pipeline, _dir) = test_pipeline_with_settings(AppSettings::default());
    assert!(pipeline.wsl_7b_server_preflight().is_ok());
}

#[test]
#[ignore] // needs WSL + a running cortex_7b_server.py on 127.0.0.1:8799
fn wsl_7b_preflight_passes_when_server_up() {
    let settings = AppSettings {
        asr_model_size: AsrModelSize::WSL7B,
        external_asr_script_path: "cortex_7b_client.py".to_string(),
        ..AppSettings::default()
    };
    let (pipeline, _dir) = test_pipeline_with_settings(settings);
    pipeline.wsl_7b_server_preflight().expect("preflight must pass when the 7B server is up");
}

#[test]
fn wsl7b_with_script_is_resolved() {
    // A configured script means the WSL primary pass runs; not the unresolved/fail-hard state.
    let settings = AppSettings {
        asr_model_size: AsrModelSize::WSL7B,
        external_asr_script_path: "cortex_7b_client.py".to_string(),
        ..AppSettings::default()
    };
    let (pipeline, _dir) = test_pipeline_with_settings(settings);
    assert!(!pipeline.wsl7b_primary_unresolved(), "a set script path must NOT be unresolved");
    assert!(pipeline.should_use_wsl_primary_asr());
}

#[test]
fn local_ctc_engine_is_never_flagged_unresolved() {
    // Selecting a local CTC model explicitly is a legitimate primary — never the F2 downgrade state.
    for size in [AsrModelSize::CTC300M, AsrModelSize::CTC1B] {
        let settings = AppSettings { asr_model_size: size.clone(), ..AppSettings::default() };
        let (pipeline, _dir) = test_pipeline_with_settings(settings);
        assert!(!pipeline.wsl7b_primary_unresolved(), "local CTC {size:?} must not be unresolved");
    }
}

#[test]
fn configured_source_reference_failure_is_fatal_before_chunking() {
    let settings = AppSettings {
        jury_cloud_opt_in: true,
        llm_api_key: "test-key".to_string(),
        source_reference_models: vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()],
        ..AppSettings::default()
    };
    let (pipeline, dir) = test_pipeline_with_settings(settings);
    let db_path = dir.path().join("db.sqlite");
    let db = Database::open(db_path.to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    let missing_audio = dir.path().join("missing-source.wav");

    let err = pipeline
        .ensure_source_reference_transcripts(&missing_audio, &db)
        .expect_err("configured source reference failure must be fatal");

    let message = err.to_string();
    assert!(message.contains("All whole-file reference transcript models failed"));
    assert!(message.contains("gemini-2.5-pro"));
    assert!(message.contains("gemini-2.5-flash"));
}

#[test]
fn partial_source_reference_failure_is_fatal_before_chunking() {
    let settings = AppSettings {
        jury_cloud_opt_in: true,
        llm_api_key: "test-key".to_string(),
        source_reference_models: vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()],
        ..AppSettings::default()
    };
    let (pipeline, dir) = test_pipeline_with_settings(settings);
    let db_path = dir.path().join("db.sqlite");
    let db = Database::open(db_path.to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    let missing_audio = dir.path().join("missing-source.wav");
    let transcript_path = dir.path().join("existing-reference.txt");
    std::fs::write(&transcript_path, "existing whole-file reference").unwrap();
    db.upsert_source_transcript(&SourceTranscriptRecord {
        audio_path: missing_audio.to_string_lossy().to_string(),
        model_id: "gemini-2.5-pro".to_string(),
        audio_content_hash: None,
        audio_size_bytes: None,
        transcript_path: transcript_path.to_string_lossy().to_string(),
        transcript_text: "existing whole-file reference".to_string(),
        created_at: None,
    })
    .unwrap();

    let err = pipeline
        .ensure_source_reference_transcripts(&missing_audio, &db)
        .expect_err("partial source reference failure must be fatal");

    let message = err.to_string();
    assert!(message.contains("All whole-file reference transcript models failed before chunking"));
    assert!(message.contains("incomplete source-reference evidence"));
    assert!(message.contains("gemini-2.5-pro"));
    assert!(message.contains("gemini-2.5-flash"));
}

#[test]
fn edited_source_reference_file_syncs_database_before_reuse() {
    let settings = AppSettings {
        jury_cloud_opt_in: true,
        llm_api_key: "test-key".to_string(),
        source_reference_models: vec!["gemini-2.5-pro".to_string()],
        ..AppSettings::default()
    };
    let (pipeline, dir) = test_pipeline_with_settings(settings);
    let db_path = dir.path().join("db.sqlite");
    let db = Database::open(db_path.to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    let audio = dir.path().join("manual-source.wav");
    std::fs::write(&audio, b"audio-v1").unwrap();
    let transcript_path = dir.path().join("manual-source-reference.txt");
    std::fs::write(&transcript_path, "manual corrected whole-file reference").unwrap();
    let mut record = source_record_for_audio(&audio, "gemini-2.5-pro", &transcript_path, "stale database reference");
    record.transcript_text = "stale database reference".to_string();
    db.upsert_source_transcript(&record).unwrap();

    let records = pipeline.ensure_source_reference_transcripts(&audio, &db).unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].transcript_text, "manual corrected whole-file reference");
    let synced = db
        .get_source_transcript(&audio.to_string_lossy(), "gemini-2.5-pro")
        .unwrap()
        .expect("synced source transcript");
    assert_eq!(synced.transcript_text, "manual corrected whole-file reference");
}

#[test]
fn changed_audio_path_does_not_reuse_stale_source_reference() {
    let settings = AppSettings {
        jury_cloud_opt_in: true,
        llm_api_key: "test-key".to_string(),
        source_reference_models: vec!["gemini-2.5-pro".to_string()],
        ..AppSettings::default()
    };
    let (pipeline, dir) = test_pipeline_with_settings(settings);
    let db_path = dir.path().join("db.sqlite");
    let db = Database::open(db_path.to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    let audio = dir.path().join("same-path.wav");
    std::fs::write(&audio, b"audio-v1").unwrap();
    let transcript_path = dir.path().join("same-path-reference.txt");
    std::fs::write(&transcript_path, "cached whole-file reference").unwrap();
    let record = source_record_for_audio(&audio, "gemini-2.5-pro", &transcript_path, "cached whole-file reference");
    db.upsert_source_transcript(&record).unwrap();

    std::fs::write(&audio, b"audio-v2-with-different-content").unwrap();

    let err = pipeline
        .ensure_source_reference_transcripts(&audio, &db)
        .expect_err("stale source reference must not be reused for changed audio bytes");

    let message = err.to_string();
    assert!(message.contains("All whole-file reference transcript models failed before chunking"));
    assert!(message.contains("gemini-2.5-pro"));
}

#[test]
fn empty_source_reference_file_is_not_reused() {
    let settings = AppSettings {
        jury_cloud_opt_in: true,
        llm_api_key: "test-key".to_string(),
        source_reference_models: vec!["gemini-2.5-pro".to_string()],
        ..AppSettings::default()
    };
    let (pipeline, dir) = test_pipeline_with_settings(settings);
    let db_path = dir.path().join("db.sqlite");
    let db = Database::open(db_path.to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    let missing_audio = dir.path().join("missing-source.wav");
    let transcript_path = dir.path().join("empty-reference.txt");
    std::fs::write(&transcript_path, " \n").unwrap();
    db.upsert_source_transcript(&SourceTranscriptRecord {
        audio_path: missing_audio.to_string_lossy().to_string(),
        model_id: "gemini-2.5-pro".to_string(),
        audio_content_hash: None,
        audio_size_bytes: None,
        transcript_path: transcript_path.to_string_lossy().to_string(),
        transcript_text: "existing whole-file reference".to_string(),
        created_at: None,
    })
    .unwrap();

    let err = pipeline
        .ensure_source_reference_transcripts(&missing_audio, &db)
        .expect_err("empty saved reference file must not be reused");

    let message = err.to_string();
    assert!(message.contains("All whole-file reference transcript models failed before chunking"));
    assert!(message.contains("Cannot inspect audio file"));
}

#[test]
fn source_reference_cloud_opt_in_without_key_is_fatal_before_chunking() {
    let settings = AppSettings {
        jury_cloud_opt_in: true,
        llm_api_key: String::new(),
        llm_api_key_configured: false,
        source_reference_models: vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()],
        ..AppSettings::default()
    };
    let (pipeline, dir) = test_pipeline_with_settings(settings);
    let db_path = dir.path().join("db.sqlite");
    let db = Database::open(db_path.to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    let missing_audio = dir.path().join("missing-source.wav");

    let err = pipeline
        .ensure_source_reference_transcripts(&missing_audio, &db)
        .expect_err("enabled source reference mode without a key must be fatal");

    let message = err.to_string();
    assert!(message.contains("Gemini API key is required for whole-file reference transcript"));
    assert!(message.contains("jury cloud opt-in is enabled"));
}

#[test]
fn speaker_hint_from_filename_stem_when_multi_chunk_enabled() {
    let settings = AppSettings { assign_speaker_from_filename: true, ..AppSettings::default() };
    let path = Path::new(r"C:\recordings\interview_guest.wav");
    let speaker = if settings.assign_speaker_from_filename {
        path.file_stem().map(|s| s.to_string_lossy().into_owned())
    } else {
        None
    };
    assert_eq!(speaker.as_deref(), Some("interview_guest"));
}

#[test]
fn speaker_hint_skipped_for_single_chunk_setting_off() {
    let settings = AppSettings { assign_speaker_from_filename: false, ..AppSettings::default() };
    let chunk_count = 3u32;
    let speaker =
        if chunk_count > 1 && settings.assign_speaker_from_filename { Some("stem".to_string()) } else { None };
    assert!(speaker.is_none());
}

#[test]
fn decode_pcm_windows_splits_long_wav() {
    use crate::audio;
    use hound::{WavSpec, WavWriter};
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("long.wav");
    let spec =
        WavSpec { channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut writer = WavWriter::create(&path, spec).unwrap();
    for i in 0..(100 * 16000) {
        let t = i as f64 / 16000.0;
        let s = (i16::MAX as f64 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16;
        writer.write_sample(s).unwrap();
    }
    writer.finalize().unwrap();

    let mut windows = Vec::new();
    audio::decode_pcm_windows(&path, 30_000, |w| {
        windows.push((w.offset_ms, w.pcm.len()));
        Ok(())
    })
    .unwrap();

    assert!(windows.len() >= 3, "expected multiple windows, got {windows:?}");
    assert_eq!(windows.first().map(|w| w.0), Some(0));
}

#[test]
fn should_stream_decode_for_long_file() {
    use crate::chunking;
    assert!(chunking::should_stream_decode(60_000, 15_000));
    assert!(!chunking::should_stream_decode(10_000, 15_000));
}

#[test]
fn get_waveform_chunk_slice_is_shorter_than_full() {
    let pcm: Vec<i16> = (0..32000).map(|i| (i as i16).wrapping_mul(100)).collect();
    let full = compute_waveform(&pcm, 50);
    let meta = SegmentSourceMeta { source_start_ms: 500, source_end_ms: 1500, chunk_index: 0, chunk_count: 1 };
    let (chunk_pcm, _) = slice_pcm_by_alignment(&pcm, 16000, Some(&meta.to_alignment_json())).unwrap();
    let chunk = compute_waveform(&chunk_pcm, 50);
    assert_eq!(full.len(), 50);
    assert_eq!(chunk.len(), 50);
    assert_ne!(full, chunk);
}

#[test]
fn subprocess_error_preview_handles_empty_stderr() {
    assert_eq!(super::subprocess_error_preview(" \r\n\t "), "(no stderr output)");
}

#[test]
fn subprocess_error_preview_caps_long_stderr_without_splitting_chars() {
    let long = format!("{}{}", "ژ".repeat(super::SUBPROCESS_ERROR_PREVIEW_CHARS), "extra");

    let preview = super::subprocess_error_preview(&long);

    assert!(preview.contains("[truncated subprocess stderr]"));
    assert!(!preview.contains("extra"));
    assert_eq!(preview.lines().next().unwrap().chars().count(), super::SUBPROCESS_ERROR_PREVIEW_CHARS);
}

#[test]
fn empty_7b_result_is_legitimate_not_infra_failure() {
    // A reachable 7B server emitting a __RESULT__ line with an empty transcript (a silent/music
    // clip) must parse Ok(("", ..)) so the caller escalates only THAT segment — never Err, which
    // would roll back the whole import and leave the file permanently unimportable via the 7B.
    let (text, conf) = super::parse_wsl_segment_result("__RESULT__={\"raw_transcript\": \"\", \"confidence\": null}")
        .expect("an empty-but-present result is a legitimate outcome, not an infra failure");
    assert_eq!(text, "");
    assert_eq!(conf, None);

    // A real transcript still parses.
    let (text, _) = super::parse_wsl_segment_result("__RESULT__={\"raw_transcript\": \"ئەمە\", \"confidence\": 0.9}")
        .expect("a real transcript parses");
    assert_eq!(text, "ئەمە");

    // NO __RESULT__ line at all is the real infrastructure failure -> Err.
    assert!(
        super::parse_wsl_segment_result("some noise\nno result here").is_err(),
        "absence of a __RESULT__ line is an infra failure"
    );
}

#[test]
fn win_path_to_wsl_translates_drive_paths() {
    assert_eq!(super::win_path_to_wsl(r"C:\data\cortex\x.db"), "/mnt/c/data/cortex/x.db");
    assert_eq!(super::win_path_to_wsl(r"D:\a\b"), "/mnt/d/a/b");
    // Extended-length prefix stripped, then drive-mapped.
    assert_eq!(super::win_path_to_wsl(r"\\?\C:\a"), "/mnt/c/a");
    // Already-POSIX / non-drive path: backslashes normalised, returned as-is.
    assert_eq!(super::win_path_to_wsl("/mnt/c/already"), "/mnt/c/already");
}

#[test]
fn scribe_segments_carry_cloud_call_provenance() {
    // PROVENANCE (the project's one law): Scribe is the ONE path that uploads raw audio to a cloud
    // provider, yet its rows were built via `..Default::default()` — persisting cloud_call=false and
    // model_version_id=NULL for exactly the segments whose audio left the machine. The local-ASR draft
    // path stamps cloud_call honestly (llm_refinement_uses_cloud()); this path must too.
    use crate::scribe_api::ScribeSegment;

    let segs = vec![ScribeSegment {
        text: "ئەمە تاقیکردنەوەیە".into(), source_start_ms: 0, source_end_ms: 1500
    }];
    let out = super::ProcessingPipeline::build_scribe_speech_segments(
        &segs,
        "/a/b.wav",
        5000,
        false,
        false,
        crate::scribe_api::DEFAULT_MODEL,
    );
    assert!(out[0].cloud_call, "a Scribe row's audio WAS uploaded — cloud_call must be true");
    assert_eq!(
        out[0].model_version_id.as_deref(),
        Some(crate::scribe_api::DEFAULT_MODEL),
        "the Scribe model must be recorded as the row's model_version_id"
    );
}

/// A pipeline as a long import actually holds it: cloned from the stored instance, then run on.
fn pipeline_with(settings: AppSettings) -> super::ProcessingPipeline {
    super::ProcessingPipeline::new(
        ":memory:".to_string(),
        Arc::new(SoraniNormalizer::new()),
        Arc::new(TranscriptCache::new(8)),
        Arc::new(AudioFingerprint::new()),
        Arc::new(settings),
        Arc::new(ModelManager::new(std::path::PathBuf::from("models"))),
    )
}

fn all_cloud_on() -> AppSettings {
    AppSettings { cloud_llm_opt_in: true, cloud_stt_opt_in: true, jury_cloud_opt_in: true, ..AppSettings::default() }
}

#[test]
fn revoking_cloud_consent_reaches_an_import_already_in_flight() {
    // THE DEFECT (audit 2026-08-06). Every long entry point does `state.lock_pipeline().clone()`
    // and runs on that clone, while update_settings replaced `self.settings` on the STORED instance.
    // An Arc swap is invisible through a clone taken earlier, so an import started before the user
    // switched cloud off kept uploading audio and transcripts for every remaining file — while the
    // save succeeded, get_settings said off, and the toggle rendered off.
    let mut stored = pipeline_with(all_cloud_on());
    let in_flight = stored.clone(); // the worker's copy, taken BEFORE the user changes anything

    assert!(in_flight.consent.cloud_stt(), "precondition: the clone starts with consent granted");

    // The user switches every cloud toggle OFF mid-import.
    let mut off = all_cloud_on();
    off.cloud_llm_opt_in = false;
    off.cloud_stt_opt_in = false;
    off.jury_cloud_opt_in = false;
    stored.update_settings(off);

    // The WORKER's clone must observe it. Before the fix all three of these still said "allowed".
    assert!(!in_flight.consent.cloud_stt(), "cloud STT consent must reach the in-flight clone");
    assert!(!in_flight.consent.cloud_llm(), "cloud LLM consent must reach the in-flight clone");
    assert!(!in_flight.consent.jury_cloud(), "jury cloud consent must reach the in-flight clone");
    assert!(
        in_flight.scribe_api_key_if_enabled().is_none(),
        "a withdrawn cloud-STT consent must stop the key ever being read"
    );
}

#[test]
fn granting_consent_mid_run_does_not_retroactively_enable_a_run_started_without_it() {
    // Fail-safe in BOTH directions. The user started this import understanding no data would leave
    // the machine; switching cloud on later must not silently upload the rest of it. The snapshot
    // and the live flag are ANDed, so the snapshot keeps this run offline.
    let mut stored = pipeline_with(AppSettings::default()); // all cloud OFF
    let in_flight = stored.clone();
    stored.update_settings(all_cloud_on());

    assert!(
        in_flight.scribe_api_key_if_enabled().is_none(),
        "a run begun without consent must stay offline even after consent is granted"
    );
}

#[test]
fn withdrawing_cloud_consent_does_not_disable_local_refinement() {
    // Local refinement sends nothing anywhere. Gating it on cloud consent would break offline work
    // for no privacy gain, so llm_refinement_permitted applies the live check only to cloud paths.
    let mut stored = pipeline_with(AppSettings {
        llm_mode: crate::settings::LlmMode::Local,
        llm_endpoint: "http://127.0.0.1:11434".to_string(),
        ..AppSettings::default()
    });
    let in_flight = stored.clone();
    assert!(in_flight.llm_refinement_permitted(), "local refinement is permitted to begin with");

    let mut revoked = stored.settings.as_ref().clone();
    revoked.cloud_llm_opt_in = false;
    stored.update_settings(revoked);

    assert!(in_flight.llm_refinement_permitted(), "revoking CLOUD consent must not disable purely-local refinement");
}

#[test]
fn revocation_takes_effect_even_when_the_settings_save_fails() {
    // The privacy asymmetry (external review 2026-08-06). commands::update_settings persists BEFORE
    // committing, so a full or read-only disk returns Err and never reaches update_settings' commit.
    // A PREFERENCE staying at its old value there is correct. A WITHDRAWAL doing so is not: the
    // running import would keep uploading with only a save error to show for it.
    //
    // This reproduces the failed-save path exactly — revoke_consent_now runs, update_settings never
    // does — and asserts egress has already stopped.
    // No `mut`: revoke_consent_now takes &self, which is exactly why it can run through the pipeline
    // lock without a mutable borrow — update_settings needs &mut, a withdrawal does not.
    let stored = pipeline_with(all_cloud_on());
    let in_flight = stored.clone();
    assert!(in_flight.consent.cloud_stt(), "precondition: consent granted");

    let mut off = all_cloud_on();
    off.cloud_llm_opt_in = false;
    off.cloud_stt_opt_in = false;
    off.jury_cloud_opt_in = false;
    stored.revoke_consent_now(&off); // ... and then the save fails, so update_settings is NOT called.

    assert!(!in_flight.consent.cloud_stt(), "a withdrawal must not wait for a successful disk write");
    assert!(!in_flight.consent.cloud_llm());
    assert!(!in_flight.consent.jury_cloud());
    assert!(
        in_flight.scribe_api_key_if_enabled().is_none(),
        "egress must already be stopped even though the settings save failed"
    );
    // The divergence is one-directional: the SNAPSHOT still says opted-in (disk and get_settings
    // report the old value, and the user is shown the save error), while egress is off. Safer than
    // displayed — never the reverse.
    assert!(stored.settings.cloud_stt_opt_in, "the un-saved snapshot legitimately still reads opted-in");
}

#[test]
fn revoke_consent_now_never_grants() {
    // Turning an opt-in ON must still go through update_settings, which commits only after a
    // successful persist — otherwise a grant that never reached disk would enable cloud egress for a
    // session and silently vanish on the next launch.
    let stored = pipeline_with(AppSettings::default()); // all cloud OFF
    stored.revoke_consent_now(&all_cloud_on());
    assert!(!stored.consent.cloud_stt(), "revoke_consent_now must never turn consent ON");
    assert!(!stored.consent.cloud_llm());
    assert!(!stored.consent.jury_cloud());
}

/// The 7B gate must admit EXACTLY its permit count concurrently — no more, no fewer.
///
/// Both directions are failures with teeth. Admitting more than the server has replicas puts the
/// extra requests in the socket queue, where they burn their client timeout waiting and get
/// misreported as "server not running" — the cumulative-timeout blowout the gate exists to prevent.
/// Admitting fewer wastes a GPU: measured 2026-08-11, the one-permit version held a 2-replica server
/// to 23.1 s/clip with both cards under 20%.
///
/// Fail-before: reverting `acquire()` to a plain exclusive Mutex drives observed concurrency to 1 and
/// trips the lower assertion.
#[test]
fn the_7b_gate_admits_exactly_its_permit_count_at_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // SAFETY: single-threaded at this point; the worker threads below start after it is set.
    std::env::set_var("CORTEX_7B_CONCURRENCY", "2");
    assert_eq!(super::wsl_7b_concurrency(), 2, "the env var must reach the gate");

    let in_flight = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let _permit = super::WSL_7B_GATE.acquire();
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                // Long enough that every thread overlaps if the gate lets them.
                std::thread::sleep(std::time::Duration::from_millis(60));
                in_flight.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });

    let observed = peak.load(Ordering::SeqCst);
    assert!(observed <= 2, "gate admitted {observed} at once, above its 2 permits — requests would queue and time out");
    assert_eq!(observed, 2, "gate never admitted 2 at once, so a second replica would sit idle");
    assert_eq!(in_flight.load(Ordering::SeqCst), 0, "every permit must be returned on drop");

    std::env::remove_var("CORTEX_7B_CONCURRENCY");
}

/// An unusable CORTEX_7B_CONCURRENCY must fall back to 1 — the SAFE end. Over-admitting reintroduces
/// the cumulative-timeout failure; under-admitting is merely slower.
#[test]
fn seven_b_concurrency_falls_back_to_one_for_every_unusable_value() {
    assert_eq!(super::parse_wsl_7b_concurrency(None), 1);
    assert_eq!(super::parse_wsl_7b_concurrency(Some("")), 1);
    assert_eq!(super::parse_wsl_7b_concurrency(Some("0")), 1, "zero would admit nobody and deadlock");
    assert_eq!(super::parse_wsl_7b_concurrency(Some("-2")), 1);
    assert_eq!(super::parse_wsl_7b_concurrency(Some("two")), 1);
    assert_eq!(super::parse_wsl_7b_concurrency(Some("9")), 1, "above the cap -> 1, never uncapped");
    assert_eq!(super::parse_wsl_7b_concurrency(Some("2")), 2);
    assert_eq!(super::parse_wsl_7b_concurrency(Some(" 4 ")), 4);
}
