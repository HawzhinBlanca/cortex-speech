//! Tauri desktop shell smoke test — launches the real binary and verifies clean exit.

use assert_cmd::Command;
use std::time::Duration;

#[test]
fn tauri_shell_smoke_exits_zero() {
    let mut cmd = Command::cargo_bin("cortex-speech-app").expect("binary built");
    cmd.env("CORTEX_SMOKE_TEST", "1").env("RUST_LOG", "info").timeout(Duration::from_secs(60));

    let output = cmd.output().expect("smoke binary should run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");

    assert!(output.status.success(), "smoke binary exited with {:?}\n{}", output.status.code(), combined);
    assert!(
        combined.contains("CORTEX_SMOKE_TEST: shell initialized, exiting"),
        "smoke startup did not reach the Tauri setup hook\n{}",
        combined
    );
    assert!(
        !combined.contains("Essential models missing"),
        "smoke startup reported missing required runtime models\n{}",
        combined
    );
    assert!(
        !combined.contains("Requested ASR model CTC1B is not installed"),
        "smoke startup used the stale CTC1B default/fallback path\n{}",
        combined
    );
}
