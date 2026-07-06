use cortex_speech_app_lib::cache::TranscriptCache;
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::error::{AppError, AppResult};
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use cortex_speech_app_lib::pipeline::ProcessingPipeline;
use cortex_speech_app_lib::settings::AppSettings;
use std::path::PathBuf;
use std::sync::Arc;

fn app_data_dir() -> PathBuf {
    std::env::var_os("CORTEX_APP_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|path| PathBuf::from(path).join("cortex-speech")))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("cortex-speech"))
}

fn main() -> AppResult<()> {
    tracing_subscriber::fmt::init();
    println!("Starting Single File Test...");

    let args: Vec<String> = std::env::args().collect();
    let target_file_path =
        args.get(1).map(PathBuf::from).or_else(|| std::env::var_os("CORTEX_TEST_AUDIO").map(PathBuf::from));
    let Some(target_file_path) = target_file_path else {
        return Err(AppError::Validation("Usage: test_file <audio-file> or set CORTEX_TEST_AUDIO.".to_string()));
    };
    let target_file = target_file_path.as_path();

    println!("Testing file: {}", target_file.display());
    if !target_file.exists() {
        return Err(AppError::Validation(format!("Target audio file does not exist: {}", target_file.display())));
    }

    let app_data_dir = app_data_dir();
    std::fs::create_dir_all(&app_data_dir)?;
    let db_path = app_data_dir.join("cortex-speech.db");

    let db = Database::open_with_retry(&db_path.to_string_lossy())?;
    db.initialize()?;
    cortex_speech_app_lib::migrations::run_migrations(&db)?;

    let settings = AppSettings::load(&app_data_dir.join("settings.json"));

    println!("Stage 1 (Audio->Text): {:?}", settings.asr_model_size);
    println!("Stage 2 (Text Refiner): {:?} ({})", settings.llm_mode, settings.llm_model);

    let normalizer = Arc::new(SoraniNormalizer::new());
    let cache = Arc::new(TranscriptCache::new(1000));
    let fingerprint = Arc::new(AudioFingerprint::new());
    let model_manager = Arc::new(ModelManager::new(app_data_dir.join("models")));

    let pipeline = ProcessingPipeline::new(
        db_path.to_string_lossy().to_string(),
        normalizer,
        cache,
        fingerprint,
        Arc::new(settings),
        model_manager,
    );

    let segments = pipeline.import_single_file(target_file)?;
    if segments.is_empty() {
        return Err(AppError::Validation(format!("No speech segments were imported from {}", target_file.display())));
    }

    println!("====== RESULTS ======");
    let mut non_empty_transcripts = 0usize;
    let mut transcription_errors = 0usize;
    for (i, seg) in segments.iter().enumerate() {
        if i > 0 {
            println!("Sleeping 6s to avoid rate limiting...");
            std::thread::sleep(std::time::Duration::from_secs(6));
        }
        println!("Chunk {}: [{} ms]", i, seg.duration_ms);
        match pipeline.transcribe(Some(&seg.id), &seg.audio_path, seg.alignment_json.as_deref(), None) {
            Ok((raw, corrected, confidence)) => {
                println!("Raw Draft: {}", raw);
                println!("Gemini Output: {}", corrected);
                if !raw.trim().is_empty() {
                    non_empty_transcripts += 1;
                }
                if let Some(c) = confidence {
                    println!("ASR Confidence: {:.2}%", c * 100.0);
                }
            }
            Err(e) => {
                println!("Transcription Error: {}", e);
                transcription_errors += 1;
            }
        }
        println!("---------------------");
    }
    if transcription_errors > 0 {
        return Err(AppError::Other(format!(
            "{transcription_errors}/{} segment transcriptions failed for {}",
            segments.len(),
            target_file.display()
        )));
    }
    if non_empty_transcripts == 0 {
        return Err(AppError::Validation(format!(
            "ASR produced 0/{} non-empty transcripts for {}",
            segments.len(),
            target_file.display()
        )));
    }

    Ok(())
}
