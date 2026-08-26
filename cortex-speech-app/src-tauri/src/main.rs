#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(exit_code) = cortex_speech_app_lib::media_materialization_worker::run_special_process_mode() {
        std::process::exit(exit_code);
    }
    if let Some(exit_code) = cortex_speech_app_lib::technical_audio_probe::run_special_process_mode() {
        std::process::exit(exit_code);
    }
    if let Some(exit_code) = cortex_speech_app_lib::asr::run_special_process_mode() {
        std::process::exit(exit_code);
    }
    cortex_speech_app_lib::run()
}
