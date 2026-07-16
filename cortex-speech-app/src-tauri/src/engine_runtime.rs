//! Live driver for [`crate::engine_supervisor`] — owns the champion 7B WSL server as a HELD child
//! process, ticks the pure supervision policy against the real health probe, and tree-kills the
//! child on app shutdown.
//!
//! Why a held child: `start_7b_server.ps1`'s nohup-detach explicitly dies under a non-interactive
//! runner ("WSL kills the session's children when the launching wsl.exe exits" — the script's own
//! NOTE), so app-owned supervision must keep the launching `wsl.exe` alive itself. Holding the child
//! also gives shutdown a real handle to tree-kill: ending the held `wsl.exe` tears down its WSL
//! session children by the same semantics that killed the nohup.
//!
//! OWNER-GATED: the loop only acts while `settings.champion_supervision_enabled` is true (default
//! OFF — supervision auto-loads a ~30 GB model server, an owner decision). The setting is re-read
//! every tick, so toggling it in settings.json takes effect without an app restart; turning it OFF
//! also stops the server the app owns.
//!
//! HONEST SCOPE (adversarially reviewed):
//! - Shutdown kill is BEST-EFFORT-ON-ORDERLY-EXIT: `RunEvent::Exit` fires on normal close/exit, but
//!   an abnormal app death (panic-abort, taskkill of the app, OS shutdown timeout) orphans the held
//!   wsl.exe + server — same orphan property the detached ps1 path always had. The orphan is then
//!   unowned (the supervision-off toggle cannot stop it); mitigation: the next enabled loop probes it
//!   healthy and simply adopts it (Idle), never double-spawning.
//!   // ponytail: best-effort kill; a Windows Job Object (KILL_ON_JOB_CLOSE) is the upgrade path if
//!   // orphans ever bite in practice.
//! - Manual start (the interactive ps1) racing supervision during a warm-up window can briefly load
//!   two ~30 GB replicas (the server binds early / listens late, so the port probe is silent while
//!   loading); it self-heals — the listen() loser dies, and supervision probes the PORT, not its own
//!   child, so a winning manual server just reads healthy → Idle.

use crate::engine_supervisor::{Decision, SupervisionState, SupervisorPolicy};
use std::process::Child;
use std::sync::Mutex;
use std::time::Duration;

/// Tick cadence. The probe itself is bounded (~3 s TCP probe inside WSL, on the blocking pool).
const TICK: Duration = Duration::from_secs(15);
/// Warm-up grace after a (re)start: the ~30 GB server takes 1–5 min to answer (per the start script);
/// silence within this window is "still loading", not failure — SupervisionState does the accounting.
const WARMUP: Duration = Duration::from_secs(6 * 60);

/// The one owned child (the `wsl.exe` holding the server). Global: there is exactly one champion.
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

/// Set once by [`begin_shutdown`]. Closes the Exit-vs-tick race: a supervision tick whose probe
/// finished just before app exit could otherwise `start_child` AFTER the Exit handler killed the
/// child, spawning a server nothing kills.
static SHUTTING_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn lock_child() -> std::sync::MutexGuard<'static, Option<Child>> {
    CHILD.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("Recovering poisoned champion-child lock");
        poisoned.into_inner()
    })
}

/// Build the `bash -lc` line that launches the server HELD. Pure — unit-tested. Mirrors
/// start_7b_server.ps1's launch exactly (env prefix, same defaults, same ~/cortex_7b_server.log)
/// minus `nohup`/`&`: `exec` replaces bash with the python server, so the held wsl.exe IS the server.
pub(crate) fn launch_bash_line(
    server_script_windows: &str,
    model_dir: &str,
    python: &str,
    port: &str,
    devices: Option<&str>,
) -> String {
    let dev = devices.map(|d| format!("CORTEX_7B_DEVICES='{d}' ")).unwrap_or_default();
    // wslpath runs inside this same bash, so no second wsl.exe round-trip is needed.
    format!(
        "{dev}CORTEX_7B_MODEL_DIR='{model_dir}' CORTEX_7B_PORT={port} exec '{python}' \"$(wslpath -a '{script}')\" >> ~/cortex_7b_server.log 2>&1",
        script = server_script_windows.replace('\\', "/"),
    )
}

/// Where `cortex_7b_server.py` lives on Windows: `CORTEX_7B_SERVER_SCRIPT` override first, else next
/// to the start script the desktop launcher already points at (`CORTEX_7B_START_SCRIPT`).
fn server_script_path() -> Option<String> {
    if let Ok(p) = std::env::var("CORTEX_7B_SERVER_SCRIPT") {
        return Some(p);
    }
    let start = std::env::var("CORTEX_7B_START_SCRIPT").ok()?;
    let candidate = std::path::Path::new(&start).parent()?.join("cortex_7b_server.py");
    candidate.is_file().then(|| candidate.to_string_lossy().into_owned())
}

fn start_child() -> Result<(), String> {
    if SHUTTING_DOWN.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("app is shutting down — not starting the champion server".to_string());
    }
    kill_owned_child(); // never leak a previous child
    let script = server_script_path()
        .ok_or("cortex_7b_server.py not found — set CORTEX_7B_SERVER_SCRIPT or CORTEX_7B_START_SCRIPT")?;
    let model_dir = std::env::var("CORTEX_7B_MODEL_DIR").unwrap_or_else(|_| "/home/ai/cortex_champion_model".into());
    let python = std::env::var("CORTEX_7B_PYTHON").unwrap_or_else(|_| "/home/ai/.venv-wsl-whisper/bin/python".into());
    let port = std::env::var("CORTEX_7B_PORT").unwrap_or_else(|_| "8799".into());
    let devices = std::env::var("CORTEX_7B_DEVICES").ok();
    let line = launch_bash_line(&script, &model_dir, &python, &port, devices.as_deref());

    let mut cmd = std::process::Command::new("wsl");
    cmd.arg("--").arg("bash").arg("-lc").arg(&line);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let child = cmd.spawn().map_err(|e| format!("could not spawn wsl for the champion server: {e}"))?;
    tracing::info!("Champion supervision: started held wsl child (pid {})", child.id());
    *lock_child() = Some(child);
    Ok(())
}

/// App-exit hook: refuse any further starts (closing the Exit-vs-tick race), then kill the child.
pub fn begin_shutdown() {
    SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
    kill_owned_child();
}

/// Tree-kill the owned child (if any) and reap it. Windows: `taskkill /T /F` reaches the whole
/// wsl.exe tree; ending the launching wsl.exe tears down the WSL-session children. Idempotent.
pub fn kill_owned_child() {
    let Some(mut child) = lock_child().take() else { return };
    let pid = child.id();
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    let _ = child.kill(); // cross-platform fallback; no-op if taskkill already ended it
    let _ = child.wait(); // reap — never leave a zombie handle
    tracing::info!("Champion supervision: killed held child (pid {pid})");
}

/// Spawn the supervision loop. Safe to call unconditionally at setup: while the setting is off the
/// loop costs one settings read per tick and owns nothing.
pub fn spawn_supervision_loop(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        use tauri::Manager;
        let mut state = SupervisionState::new(SupervisorPolicy::default(), WARMUP);
        let t0 = std::time::Instant::now();
        let mut was_enabled = false;
        let mut gave_up_logged = false;
        loop {
            tokio::time::sleep(TICK).await;
            let enabled = app
                .try_state::<crate::AppState>()
                .map(|s| s.lock_settings().champion_supervision_enabled)
                .unwrap_or(false);
            if !enabled {
                if was_enabled {
                    // Owner turned supervision off: stop supervising AND stop the server we own.
                    kill_owned_child();
                    state = SupervisionState::new(SupervisorPolicy::default(), WARMUP);
                    gave_up_logged = false;
                    was_enabled = false;
                }
                continue;
            }
            was_enabled = true;
            let healthy =
                tokio::task::spawn_blocking(|| crate::pipeline::probe_wsl_7b_server(3)).await.unwrap_or(false);
            match state.tick(healthy, t0.elapsed()) {
                Decision::Restart => {
                    if let Err(e) = start_child() {
                        tracing::warn!("Champion supervision: start failed: {e}");
                    }
                }
                Decision::GaveUp => {
                    if !gave_up_logged {
                        tracing::error!(
                            "Champion supervision GAVE UP after repeated failures — manual start required \
                             (toggle supervision off/on to reset, or use the manual engine start)."
                        );
                        gave_up_logged = true;
                    }
                }
                Decision::Idle => gave_up_logged = false,
                Decision::Wait(_) | Decision::Cooldown(_) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_line_mirrors_ps1_minus_detach() {
        let line = launch_bash_line("C:\\repo\\scripts\\cortex_7b_server.py", "/m", "/py", "8799", Some("0,1"));
        assert!(
            line.starts_with("CORTEX_7B_DEVICES='0,1' CORTEX_7B_MODEL_DIR='/m' CORTEX_7B_PORT=8799 exec '/py'"),
            "{line}"
        );
        assert!(line.contains("wslpath -a 'C:/repo/scripts/cortex_7b_server.py'"), "{line}");
        assert!(line.contains(">> ~/cortex_7b_server.log 2>&1"), "{line}");
        assert!(!line.contains("nohup"), "held child must not detach: {line}");
    }

    #[test]
    fn bash_line_without_devices_omits_the_env() {
        let line = launch_bash_line("s.py", "/m", "/py", "1", None);
        assert!(!line.contains("CORTEX_7B_DEVICES"), "{line}");
    }

    #[test]
    fn kill_with_no_child_is_a_noop() {
        kill_owned_child();
        kill_owned_child();
    }

    #[test]
    fn start_is_refused_after_begin_shutdown() {
        // Closes the Exit-vs-tick race: a tick deciding Restart after the Exit handler ran must not
        // spawn a server nothing kills.
        begin_shutdown();
        let err = start_child().expect_err("start_child must refuse during shutdown");
        assert!(err.contains("shutting down"), "{err}");
        // Restore for any other test in this process (statics are process-global).
        SHUTTING_DOWN.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}
