use cortex_speech_app_lib::asr::{AsrLoadConfig, AsrPool};
use cortex_speech_app_lib::audio;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

fn app_data_dir() -> PathBuf {
    std::env::var_os("CORTEX_APP_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|path| PathBuf::from(path).join("cortex-speech")))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("cortex-speech"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    info!("Starting Batch Processor...");

    // 1. Locate Database
    let app_data_dir = app_data_dir();

    let db_path = app_data_dir.join("cortex-speech.db");
    info!("Using Database at: {}", db_path.display());

    // Same single-instance lock as the app and batch_importer (the Week-2 write-path audit found this
    // tool claimed parity but took no lock): never write the live DB concurrently with the running
    // app — WAL would prevent corruption, but the repo's cross-process discipline is the InstanceLock.
    let _lock = cortex_speech_app_lib::flock::InstanceLock::try_lock(&app_data_dir)
        .map_err(|e| format!("another Cortex process holds the data dir: {e}"))?;

    // Use open_with_retry + initialize like batch_importer and the app: open() only sets PRAGMAs and
    // creates NO tables/FTS triggers/migrations, so reading or writing against a fresh or stale-schema
    // data dir would fail ("no such table: speech_segments") or silently skip FTS indexing.
    let db = Database::open_with_retry(&db_path.to_string_lossy())?;
    db.initialize()?;

    // 2. Fetch unverified segments
    let unverified = db.get_segments(Some(false))?;
    info!("Found {} unverified segments.", unverified.len());

    if unverified.is_empty() {
        info!("Nothing to do!");
        return Ok(());
    }

    // 3. Group by audio path to minimize decoding
    let mut segments_by_file: HashMap<String, Vec<cortex_speech_app_lib::db::SpeechSegment>> = HashMap::new();
    for seg in unverified {
        segments_by_file.entry(seg.audio_path.clone()).or_default().push(seg);
    }

    info!("Grouped into {} unique audio files.", segments_by_file.len());

    // 4. Initialize Models
    let model_dir = app_data_dir.join("models");
    let asr_pool = AsrPool::new();
    let asr_config = AsrLoadConfig {
        // This offline batch helper fills UNVERIFIED drafts with the always-present BUNDLED engine
        // (OmniASR-CTC-300M = AsrModelSize::default()). NOTE: that is NOT the app default — the GUI's
        // default is the WSL-7B champion (settings.rs) — but the 7B needs the WSL server running and the
        // fine-tuned CTC-1B isn't on a standard install, either of which would make this headless helper
        // fail. The drafts it writes are verified=false and flow through the SAME review/agentic gates as
        // any unreviewed draft (no gold fabrication — see the write below), so this is a deliberate
        // availability trade-off, not the silent downgrade of the user's SELECTED engine that the GUI's
        // F2 guard forbids. Expect bundled-engine draft quality (higher CER than the 7B) from a batch run.
        num_threads: 8,
        ..AsrLoadConfig::default()
    };
    asr_pool.warmup(&model_dir, &asr_config)?;

    let normalizer = SoraniNormalizer::new();

    // 5. Process files
    let mut processed_count = 0;
    let mut deleted_count = 0;

    for (audio_path, segments) in segments_by_file {
        info!("Processing file: {} ({} segments)", audio_path, segments.len());

        let path = Path::new(&audio_path);
        if !path.exists() {
            warn!("File not found: {}. Skipping segments.", audio_path);
            continue;
        }

        let (sample_rate, full_pcm) = match audio::decode_to_pcm(path) {
            Ok(decoded) => decoded,
            Err(e) => {
                error!("Failed to decode {}: {}", audio_path, e);
                continue;
            }
        };

        let mut to_update = Vec::new();
        let mut to_delete = Vec::new();

        for mut seg in segments {
            // Never DELETE a segment that already holds a REAL transcript (raw draft or human annotation)
            // just because THIS run can't (re)transcribe it: an empty/silent bundled-engine result — or an
            // unsliceable window — must not destroy a good draft a stronger engine (e.g. WSL 7B) produced.
            // Deletion is only for FRESH placeholder segments (VAD false-positives: no real transcript yet).
            // (blank-transcript-never-overwrites-good data-loss class.)
            let has_existing_transcript = {
                let raw = seg.raw_transcript.trim();
                let ann = seg.annotated_transcript.as_deref().unwrap_or("").trim();
                (!raw.is_empty() && !cortex_speech_app_lib::quality::is_placeholder_transcript(raw))
                    || (!ann.is_empty() && !cortex_speech_app_lib::quality::is_placeholder_transcript(ann))
            };

            // Slice the per-chunk window from the SAME canonical schema the app writes
            // (source_start_ms / source_end_ms via SegmentSourceMeta) and errors on. The previous code
            // read non-existent `source_start_sample`/`source_end_sample` keys and `unwrap_or`-ed into
            // (0, full_pcm.len()), so EVERY chunk of a multi-chunk file was transcribed against the
            // WHOLE file and persisted as verified — corrupting the dataset. On a missing/unreadable
            // window we now SKIP the segment rather than transcribe the whole file.
            let chunk_pcm: Vec<i16> = match cortex_speech_app_lib::chunking::slice_pcm_by_alignment(
                &full_pcm,
                sample_rate,
                seg.alignment_json.as_deref(),
            ) {
                Ok((pcm, _suffix)) => pcm,
                Err(error) => {
                    warn!("Skipping segment {}: cannot slice chunk window: {error}", seg.id);
                    if !has_existing_transcript {
                        to_delete.push(seg.id.clone());
                    }
                    continue;
                }
            };
            if audio::is_silent(&chunk_pcm) {
                if !has_existing_transcript {
                    to_delete.push(seg.id.clone());
                }
                continue;
            }

            let f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();

            // Transcribe. Batch mode must fail loudly on ASR runtime errors;
            // otherwise a backend fault looks like a blank transcript and can
            // incorrectly delete the segment.
            let asr_result: Result<(String, Option<f64>, cortex_speech_app_lib::asr::ConfidenceSource), String> =
                asr_pool.with_service(&model_dir, &asr_config, |asr| {
                    if !asr.is_available() {
                        return Err("ASR service is unavailable; models may not be downloaded".to_string());
                    }
                    asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE).map_err(|error| {
                        format!("ASR transcription failed for segment {} in {}: {error}", seg.id, audio_path)
                    })
                });
            let (text, confidence, confidence_source) = match asr_result {
                Ok(result) => result,
                Err(error) => {
                    error!("{error}");
                    return Err(std::io::Error::other(error).into());
                }
            };

            if text.trim().is_empty() {
                if !has_existing_transcript {
                    to_delete.push(seg.id.clone());
                }
                continue;
            }

            // Normalize
            let norm = normalizer.normalize(&text);
            if norm.trim().is_empty() {
                if !has_existing_transcript {
                    to_delete.push(seg.id.clone());
                }
                continue;
            }

            // This is a MACHINE ASR draft, not a human approval. Populate only the raw + normalized
            // transcript and leave annotated_transcript empty and verified=false. Setting
            // annotated_transcript (the human-correction field) and verified=true here made
            // training_grade_for_segment grade pure ASR output as human_verified GOLD — fabricating
            // human-verified training data, which the honesty law forbids. The segment now flows through
            // the normal review/agentic gates like any other unreviewed draft.
            seg.raw_transcript = text;
            seg.normalized_transcript = Some(norm);
            seg.confidence = confidence;
            seg.confidence_source = Some(confidence_source.as_db_value().to_string());
            seg.model_version_id = Some("omniasr-ctc-300m".to_string());
            seg.cloud_call = false;
            seg.decoder_config_hash = None;
            seg.normalizer_version = Some("sorani-normalizer-v1".to_string());

            to_update.push(seg);
        }

        // Commit batch
        if !to_update.is_empty() {
            db.insert_segments_batch(&to_update)?;
            processed_count += to_update.len();
        }
        if !to_delete.is_empty() {
            db.delete_segments_batch(&to_delete)?;
            deleted_count += to_delete.len();
        }

        // Free memory immediately
        audio::clear_pcm_cache();
    }

    info!("Batch processor finished! Updated: {}, Deleted: {}", processed_count, deleted_count);
    Ok(())
}
