use cortex_speech_app_lib::models::ModelManager;
use cortex_speech_app_lib::settings::AsrModelSize;
use std::path::PathBuf;

fn app_data_dir() -> PathBuf {
    std::env::var_os("CORTEX_APP_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|path| PathBuf::from(path).join("cortex-speech")))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("cortex-speech"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let app_data_dir = app_data_dir();
    let model_dir = app_data_dir.join("models");
    let manager = ModelManager::new(model_dir);

    println!("Ensuring directory...");
    manager.ensure_dir()?;

    println!("Downloading bundled default CTC300M model...");
    manager.download_omniasr(AsrModelSize::CTC300M, |progress| {
        println!("Progress: {:.1}%", progress * 100.0);
    })?;

    println!("Model downloaded successfully!");
    Ok(())
}
