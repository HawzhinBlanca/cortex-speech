//! Real Tauri binary integration — import → export → validate without UI mocks.

mod fixtures;

use assert_cmd::Command;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn tauri_integration_import_export_validate() {
    let fixture_dir = TempDir::new().expect("tempdir");
    fixtures::create_test_wav(&fixture_dir.path().join("clip_a.wav"), 1.0, 16000, 440.0).expect("wav a");
    fixtures::create_test_wav(&fixture_dir.path().join("clip_b.wav"), 0.8, 16000, 880.0).expect("wav b");

    let output = Command::cargo_bin("cortex-speech-app")
        .expect("binary built")
        .env("CORTEX_INTEGRATION_TEST", "1")
        .env("CORTEX_INTEGRATION_FIXTURE", fixture_dir.path())
        .env("RUST_LOG", "error")
        .timeout(Duration::from_secs(120))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);
    assert!(stdout.contains("CORTEX_INTEGRATION_OK"), "expected integration marker in stdout, got: {stdout}");
}
