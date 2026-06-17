use assert_cmd::Command;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn test_file_requires_audio_input() {
    let app_data = TempDir::new().expect("temp app data");

    let output = Command::cargo_bin("test_file")
        .expect("binary built")
        .env("CORTEX_APP_DATA_DIR", app_data.path())
        .env_remove("CORTEX_TEST_AUDIO")
        .env("RUST_LOG", "error")
        .timeout(Duration::from_secs(30))
        .assert()
        .failure()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage: test_file <audio-file>"), "expected usage error, got: {stderr}");
    assert_no_app_database(app_data.path());
}

#[test]
fn test_file_rejects_missing_audio_path() {
    let app_data = TempDir::new().expect("temp app data");
    let missing_audio = app_data.path().join("missing.wav");

    let output = Command::cargo_bin("test_file")
        .expect("binary built")
        .arg(&missing_audio)
        .env("CORTEX_APP_DATA_DIR", app_data.path())
        .env_remove("CORTEX_TEST_AUDIO")
        .env("RUST_LOG", "error")
        .timeout(Duration::from_secs(30))
        .assert()
        .failure()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Target audio file does not exist"), "expected missing-file error, got: {stderr}");
    assert_no_app_database(app_data.path());
}

fn assert_no_app_database(app_data: &std::path::Path) {
    assert!(
        !app_data.join("cortex-speech.db").exists(),
        "invalid test_file invocation should fail before creating app database"
    );
}
