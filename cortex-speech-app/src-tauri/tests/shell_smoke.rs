//! Tauri desktop shell smoke test — launches the real binary and verifies clean exit.

use assert_cmd::Command;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn tauri_shell_smoke_exits_zero() {
    // Without this the app falls back to `TEMP\cortex-smoke-<pid>` (lib.rs `get_app_data_dir`) and
    // NOTHING removes it: measured 122 stale dirs / 76 MB on the owner's box, one more every sweep.
    // `TempDir` deletes on drop, so the dir this test causes is the test's to clean up.
    let data_dir = TempDir::new().expect("tempdir");

    let mut cmd = Command::cargo_bin("cortex-speech-app").expect("binary built");
    cmd.env("CORTEX_SMOKE_TEST", "1")
        .env("CORTEX_APP_DATA_DIR", data_dir.path())
        .env("RUST_LOG", "info")
        .timeout(Duration::from_secs(60));

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

    // The orderly-exit marker, asserted end-to-end on the REAL binary. This run is a clean exit
    // (`CORTEX_SMOKE_TEST` reaches `RunEvent::Exit` via `handle.exit(0)`), so the marker must be there.
    //
    // It matters because the watchdog reads exactly this file to tell "the owner closed it" from "it
    // died": absent-while-not-running is the ONLY evidence of an abnormal death, and 16 log files
    // spanning a month proved the logs cannot supply it. Verified by hand when it was written, which is
    // not a gate — delete the `fs::write` in lib.rs and nothing else in the suite notices.
    //
    // Asserting the marker's CONTENT, not just its presence: an empty or truncated file is still a
    // file, and the watchdog prints this line to the owner.
    let marker = data_dir.path().join("logs").join("last-exit.txt");
    let recorded = std::fs::read_to_string(&marker).unwrap_or_else(|e| {
        panic!("no orderly-exit marker at {} after a CLEAN exit ({e}) — the watchdog cannot then distinguish a crash from the owner closing the window\n{combined}", marker.display())
    });
    assert!(recorded.starts_with("orderly exit "), "exit marker exists but does not say what it is: {recorded:?}");
}
