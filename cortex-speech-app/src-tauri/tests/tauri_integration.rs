//! Real Tauri binary integration — import → export → validate without UI mocks.

mod fixtures;

use assert_cmd::Command;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// assert_cmd's `.timeout()` KILLS the child when the budget runs out and, on Windows, reports the
/// kill as a plain exit code 1 with empty stdout and stderr -- indistinguishable from a real
/// `CORTEX_INTEGRATION_FAIL` exit. Measured 2026-09-02 on the hosted Windows runner (PR #77): the
/// gate went red on "code=1, stdout=\"\", stderr=\"\"" after exactly 120.05 s, on a Rust tree
/// identical to a green main. Name the budget so the next such failure explains itself.
const EXE_BUDGET: Duration = Duration::from_secs(120);

#[test]
fn tauri_integration_import_export_validate() {
    // The real Tauri GUI binary runs an in-process integration runner ~1.2s after startup, then
    // prints CORTEX_INTEGRATION_OK and exits 0. Under headless timing the app's event loop can
    // occasionally return (still exit 0) BEFORE that runner thread emits the marker — an inherent
    // startup timing race, not a pipeline failure. Retry the spawn so this flake cannot redden the
    // suite. Crucially we only retry the exit-0-but-no-marker case: `.success()` still fails fast on
    // a genuine non-zero exit (CORTEX_INTEGRATION_FAIL), so a real pipeline break fails immediately.
    let mut last = String::new();
    for attempt in 1..=3 {
        let fixture_dir = TempDir::new().expect("tempdir");
        fixtures::create_test_wav(&fixture_dir.path().join("clip_a.wav"), 1.0, 16000, 440.0).expect("wav a");
        fixtures::create_test_wav(&fixture_dir.path().join("clip_b.wav"), 0.8, 16000, 880.0).expect("wav b");
        // The FIXTURE dir was already disposable; the app's DATA dir was not. Without this the app
        // falls back to `TEMP\cortex-integration-<pid>` (lib.rs `get_app_data_dir`) and nothing removes
        // it — measured 124 stale dirs / 84 MB, and this test leaks one PER ATTEMPT, up to 3 a run.
        let data_dir = TempDir::new().expect("tempdir");

        let started = Instant::now();
        let output = Command::cargo_bin("cortex-speech-app")
            .expect("binary built")
            .env("CORTEX_INTEGRATION_TEST", "1")
            .env("CORTEX_INTEGRATION_FIXTURE", fixture_dir.path())
            .env("CORTEX_APP_DATA_DIR", data_dir.path())
            .env("RUST_LOG", "error")
            .timeout(EXE_BUDGET)
            .output()
            .expect("spawn the real binary");
        let elapsed = started.elapsed();
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let verdict = if elapsed >= EXE_BUDGET - Duration::from_secs(2) {
                format!(
                    "the exe was KILLED at the {}s budget after {:.1}s: a startup or runtime stall on this \
                     machine, not a pipeline verdict (a real pipeline failure prints CORTEX_INTEGRATION_FAIL \
                     and exits within seconds)",
                    EXE_BUDGET.as_secs(),
                    elapsed.as_secs_f64()
                )
            } else {
                format!("the exe exited {:?} after {:.1}s", output.status.code(), elapsed.as_secs_f64())
            };
            panic!("{verdict}\nstdout={stdout:?}\nstderr={stderr:?}");
        }
        last = String::from_utf8_lossy(&output.stdout).to_string();
        if last.contains("CORTEX_INTEGRATION_OK") {
            return; // real import -> export -> validate pipeline success
        }
        eprintln!("integration attempt {attempt}/3: exit 0 but marker absent (startup timing race) — retrying");
    }
    panic!("expected integration marker in stdout after 3 attempts, got: {last}");
}
