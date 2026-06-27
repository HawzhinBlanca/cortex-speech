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
        // Match the app default and bundled runtime model. Requesting CTC1B here
        // makes this helper fail on the standard packaged install.
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

        let (_sample_rate, full_pcm) = match audio::decode_to_pcm(path) {
            Ok(decoded) => decoded,
            Err(e) => {
                error!("Failed to decode {}: {}", audio_path, e);
                continue;
            }
        };

        let mut to_update = Vec::new();
        let mut to_delete = Vec::new();

        for mut seg in segments {
            // Parse alignment JSON
            let alignment: serde_json::Value = if let Some(j) = &seg.alignment_json {
                serde_json::from_str(j).unwrap_or(serde_json::json!({}))
            } else {
                serde_json::json!({})
            };

            let start_sample = alignment["source_start_sample"].as_u64().unwrap_or(0) as usize;
            let end_sample = alignment["source_end_sample"].as_u64().unwrap_or(full_pcm.len() as u64) as usize;

            let safe_start = start_sample.min(full_pcm.len());
            let safe_end = end_sample.min(full_pcm.len());

            if safe_end <= safe_start {
                warn!("Invalid chunk boundaries for {}. Deleting.", seg.id);
                to_delete.push(seg.id.clone());
                continue;
            }

            let chunk_pcm = &full_pcm[safe_start..safe_end];
            if audio::is_silent(chunk_pcm) {
                to_delete.push(seg.id.clone());
                continue;
            }

            let f32_pcm: Vec<f32> = chunk_pcm.iter().map(|&s| s as f32 / 32768.0).collect();

            // Transcribe. Batch mode must fail loudly on ASR runtime errors;
            // otherwise a backend fault looks like a blank transcript and can
            // incorrectly delete the segment.
            let asr_result: Result<(String, Option<f64>), String> =
                asr_pool.with_service(&model_dir, &asr_config, |asr| {
                    if !asr.is_available() {
                        return Err("ASR service is unavailable; models may not be downloaded".to_string());
                    }
                    asr.transcribe(&f32_pcm, audio::TARGET_SAMPLE_RATE).map_err(|error| {
                        format!("ASR transcription failed for segment {} in {}: {error}", seg.id, audio_path)
                    })
                });
            let (text, confidence) = match asr_result {
                Ok(result) => result,
                Err(error) => {
                    error!("{error}");
                    return Err(std::io::Error::other(error).into());
                }
            };

            if text.trim().is_empty() {
                to_delete.push(seg.id.clone());
                continue;
            }

            // Normalize
            let norm = normalizer.normalize(&text);
            if norm.trim().is_empty() {
                to_delete.push(seg.id.clone());
                continue;
            }

            seg.raw_transcript = text;
            seg.normalized_transcript = Some(norm.clone());
            seg.annotated_transcript = Some(norm);
            seg.verified = true;
            seg.confidence = confidence;

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
