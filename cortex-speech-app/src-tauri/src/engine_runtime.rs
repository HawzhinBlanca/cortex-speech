//! Live driver for [`crate::engine_supervisor`]. It owns the champion 7B WSL launcher, checks the
//! exact registry-selected deployment identity, and contains the Windows process tree in a Job
//! Object configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
//!
//! The Job Object handle is held for the entire launcher lifetime. Orderly stop closes it and reaps
//! the launcher; Windows also closes it when the app is terminated or crashes, so the contained
//! process tree cannot outlive the app. Assignment is fail-closed: a launcher that cannot be placed
//! in the Job Object is killed and reaped before start returns an error.

use crate::engine_supervisor::{Decision, SupervisionState, SupervisorPolicy};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use tauri::Manager;

/// Tick cadence. The exact-identity health request is bounded and runs on the blocking pool.
const TICK: Duration = Duration::from_secs(15);
/// Warm-up grace after a (re)start: the ~30 GB server takes 1–5 min to answer (per the start script);
/// silence within this window is "still loading", not failure — SupervisionState does the accounting.
const WARMUP: Duration = Duration::from_secs(6 * 60);

/// A non-inheritable Windows Job Object whose last-handle close terminates every contained process.
///
/// The handle is deliberately process-local: `CreateJobObjectW` receives no inheritable security
/// attributes, so the champion cannot keep its own containment handle alive.
#[cfg(target_os = "windows")]
struct KillOnCloseJob {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(target_os = "windows")]
impl KillOnCloseJob {
    fn new() -> Result<Self, String> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        // SAFETY: null security/name pointers request a private, non-inheritable unnamed job.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(format!("could not create champion Job Object: {}", std::io::Error::last_os_error()));
        }
        let job = Self { handle };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `job.handle` is live and `limits` has the exact layout/size required by this
        // information class for the duration of the call.
        let configured = unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            return Err(format!(
                "could not configure champion Job Object kill-on-close: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(job)
    }

    fn assign(&self, child: &Child) -> Result<(), String> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        // SAFETY: both handles are live, owned by this process, and remain live for the call.
        let assigned = unsafe { AssignProcessToJobObject(self.handle, child.as_raw_handle().cast()) };
        if assigned == 0 {
            return Err(format!(
                "could not assign champion launcher pid {} to its kill-on-close Job Object: {}",
                child.id(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: this type uniquely owns `handle`, and Drop runs at most once.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }
}

// A HANDLE is safe to move between threads when ownership remains unique. Windows synchronizes
// Job Object operations in the kernel; all access here is additionally serialized by `CHILD`.
#[cfg(target_os = "windows")]
unsafe impl Send for KillOnCloseJob {}

struct OwnedChampionProcess {
    child: Child,
    #[cfg(target_os = "windows")]
    job: Option<KillOnCloseJob>,
}

impl OwnedChampionProcess {
    fn id(&self) -> u32 {
        self.child.id()
    }

    fn terminate_and_reap(&mut self) {
        #[cfg(target_os = "windows")]
        {
            // Closing the last job handle is the tree-kill. Do it before waiting for the launcher.
            drop(self.job.take());
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        }
        // Cross-platform root-process fallback. On Windows the Job Object has already been closed,
        // so descendants cannot escape even if the launcher takes unusually long to report exit.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for OwnedChampionProcess {
    fn drop(&mut self) {
        self.terminate_and_reap();
    }
}

#[cfg(target_os = "windows")]
fn spawn_contained(mut command: Command) -> Result<OwnedChampionProcess, String> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

    let job = KillOnCloseJob::new()?;
    // The process must not execute even one instruction until Job assignment succeeds. In
    // particular, an assignment failure cannot race a WSL/Linux descendant into existence.
    command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    let mut child = command.spawn().map_err(|error| format!("could not spawn wsl for the champion server: {error}"))?;
    if let Err(assign_error) = job.assign(&child) {
        // Assignment failure is fail-closed. Reap even when kill reports that the process already
        // exited; never return with an uncontained launcher still running.
        let kill_result = child.kill();
        let wait_result = child.wait();
        drop(job);
        return Err(format!(
            "{assign_error}; uncontained launcher cleanup: kill={kill_result:?}, wait={wait_result:?}"
        ));
    }
    if let Err(resume_error) = resume_suspended_process(&child) {
        // Now contained: closing the job is the primary cleanup, followed by root kill/reap as a
        // bounded defensive fallback. Never retain a permanently suspended champion launcher.
        drop(job);
        let kill_result = child.kill();
        let wait_result = child.wait();
        return Err(format!("{resume_error}; suspended launcher cleanup: kill={kill_result:?}, wait={wait_result:?}"));
    }
    Ok(OwnedChampionProcess { child, job: Some(job) })
}

/// Resume the single primary thread of a process created with `CREATE_SUSPENDED`.
#[cfg(target_os = "windows")]
fn resume_suspended_process(child: &Child) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    // SAFETY: this creates a read-only system thread snapshot with no caller-owned pointers.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!(
            "could not enumerate the suspended champion launcher threads: {}",
            std::io::Error::last_os_error()
        ));
    }
    let result = (|| {
        let mut entry = THREADENTRY32 { dwSize: std::mem::size_of::<THREADENTRY32>() as u32, ..Default::default() };
        // SAFETY: `snapshot` is live and `entry.dwSize` identifies the writable structure.
        if unsafe { Thread32First(snapshot, &raw mut entry) } == 0 {
            return Err(format!(
                "could not read the suspended champion launcher thread snapshot: {}",
                std::io::Error::last_os_error()
            ));
        }
        loop {
            if entry.th32OwnerProcessID == child.id() {
                // SAFETY: the snapshot supplied this thread id; the handle is checked and closed.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(format!(
                        "could not open suspended champion launcher thread {}: {}",
                        entry.th32ThreadID,
                        std::io::Error::last_os_error()
                    ));
                }
                // SAFETY: `thread` is a live handle opened with THREAD_SUSPEND_RESUME.
                let previous_suspend_count = unsafe { ResumeThread(thread) };
                let resume_error = (previous_suspend_count == u32::MAX).then(std::io::Error::last_os_error);
                // SAFETY: unique local ownership; close exactly once after ResumeThread.
                unsafe { CloseHandle(thread) };
                if let Some(error) = resume_error {
                    return Err(format!("could not resume the champion launcher primary thread: {error}"));
                }
                if previous_suspend_count != 1 {
                    return Err(format!(
                        "champion launcher primary thread had unexpected suspend count {previous_suspend_count}"
                    ));
                }
                return Ok(());
            }
            // SAFETY: same live snapshot and initialized writable entry as above.
            if unsafe { Thread32Next(snapshot, &raw mut entry) } == 0 {
                return Err(format!(
                    "suspended champion launcher pid {} had no discoverable primary thread",
                    child.id()
                ));
            }
        }
    })();
    // SAFETY: unique local ownership; close exactly once on every path after creation.
    unsafe { CloseHandle(snapshot) };
    result
}

#[cfg(not(target_os = "windows"))]
fn spawn_contained(mut command: Command) -> Result<OwnedChampionProcess, String> {
    let child = command.spawn().map_err(|error| format!("could not spawn wsl for the champion server: {error}"))?;
    Ok(OwnedChampionProcess { child })
}

/// The one owned launcher plus its containment handle. Global: there is exactly one champion.
static CHILD: Mutex<Option<OwnedChampionProcess>> = Mutex::new(None);

/// Set once by [`begin_shutdown`]. Closes the Exit-vs-tick race: a supervision tick whose probe
/// finished just before app exit could otherwise `start_child` AFTER the Exit handler killed the
/// child, spawning a server nothing kills.
static SHUTTING_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Promotion is a cross-system saga. While it owns this lease, neither the periodic supervisor nor
/// the renderer-facing Start command may restart whichever pointer happens to be visible midway
/// through the saga. The production promotion runtime has a dedicated restart entry point below.
static PROMOTION_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set when startup found a promotion that could not be safely resumed/rolled back. Starting a model
/// in that state would turn an explicitly unverified rollback into a silently serving deployment.
static PROMOTION_RECOVERY_BLOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) struct PromotionLease;

impl Drop for PromotionLease {
    fn drop(&mut self) {
        PROMOTION_ACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(crate) fn acquire_promotion_lease() -> Result<PromotionLease, String> {
    PROMOTION_ACTIVE
        .compare_exchange(false, true, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst)
        .map_err(|_| "another champion promotion/recovery is already active".to_string())?;
    Ok(PromotionLease)
}

pub(crate) fn set_promotion_recovery_blocked(blocked: bool) {
    PROMOTION_RECOVERY_BLOCKED.store(blocked, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn champion_operational_block_reason() -> Option<&'static str> {
    if PROMOTION_RECOVERY_BLOCKED.load(std::sync::atomic::Ordering::SeqCst) {
        Some("Champion promotion recovery/rollback is not yet verified; serving readiness is blocked.")
    } else if PROMOTION_ACTIVE.load(std::sync::atomic::Ordering::SeqCst) {
        Some("Champion promotion or rollback is in progress; serving readiness is temporarily blocked.")
    } else {
        None
    }
}

fn lock_child() -> std::sync::MutexGuard<'static, Option<OwnedChampionProcess>> {
    CHILD.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("Recovering poisoned champion-child lock");
        poisoned.into_inner()
    })
}

/// Build the argument vector for the held WSL server without a shell. Paths containing whitespace,
/// apostrophes or metacharacters stay data because each token is passed with `Command::arg`; the old
/// `bash -lc` string interpolated them into executable syntax.
pub(crate) fn champion_wsl_args(
    server_script_wsl: &str,
    champion_pointer_wsl: &str,
    python: &str,
    port: &str,
    devices: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "--".to_string(),
        "env".to_string(),
        format!("CORTEX_7B_CHAMPION_POINTER={champion_pointer_wsl}"),
        format!("CORTEX_7B_PORT={port}"),
    ];
    if let Some(devices) = devices {
        args.push(format!("CORTEX_7B_DEVICES={devices}"));
    }
    args.push(python.to_string());
    args.push(server_script_wsl.to_string());
    args
}

/// The layouts `cortex_7b_server.py` can sit in, relative to the running exe: beside it (a bundled
/// install), and the dev/personal-use tree `cortex-speech-app/scripts/` reached from
/// `src-tauri/target/{release,debug}/`. Personal use IS the ship target (CLAUDE.md, owner 2026-07-10),
/// so the repo layout is a first-class case, not a developer convenience.
const SERVER_SCRIPT_RELATIVE_TO_EXE: [&str; 4] = [
    "cortex_7b_server.py",
    "../../../../scripts/cortex_7b_server.py",
    "../../../scripts/cortex_7b_server.py",
    "../../scripts/cortex_7b_server.py",
];

/// Pure resolution (unit-tested): first the explicit override, then beside the launcher's start
/// script, then the known layouts under `exe_dir`.
///
/// The last group exists because BOTH env vars are process-inherited, and the champion is started by
/// whatever launched the app. `CORTEX_7B_SERVER_SCRIPT` is set at USER scope on this machine, so a
/// launcher that does not inherit it — a shell, a service, a scheduler — silently produced an app
/// that could never start its champion. Measured 2026-08-18: supervision failed every ~8 minutes for
/// two hours after the app was relaunched from a shell without it, while the models sat on disk the
/// whole time. A file the app can simply FIND should never depend on an environment variable being
/// carried through, which is the same lesson `resolve_model_file` already encodes for models.
fn resolve_server_script(
    env_override: Option<String>,
    start_script: Option<String>,
    exe_dir: Option<&std::path::Path>,
    resource_dir: Option<&std::path::Path>,
) -> Option<String> {
    if let Some(p) = env_override.filter(|p| !p.trim().is_empty()) {
        return Some(p);
    }
    if let Some(start) = start_script {
        if let Some(candidate) = std::path::Path::new(&start).parent().map(|d| d.join("cortex_7b_server.py")) {
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    let from_exe = exe_dir.and_then(|exe_dir| {
        SERVER_SCRIPT_RELATIVE_TO_EXE.iter().find_map(|rel| {
            let candidate = exe_dir.join(rel);
            // `canonicalize` collapses the `..` hops so the logged path is the real one, and fails
            // closed when the file is absent.
            candidate.canonicalize().ok().filter(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned())
        })
    });
    from_exe.or_else(|| {
        resource_dir.and_then(|root| {
            ["cortex_7b_server.py", "scripts/cortex_7b_server.py", "_up_/scripts/cortex_7b_server.py"].iter().find_map(
                |rel| {
                    root.join(rel)
                        .canonicalize()
                        .ok()
                        .filter(|path| path.is_file())
                        .map(|path| path.to_string_lossy().into_owned())
                },
            )
        })
    })
}

/// Where `cortex_7b_server.py` lives on Windows. See [`resolve_server_script`].
pub(crate) fn server_script_path(app: &tauri::AppHandle) -> Option<String> {
    use tauri::Manager;
    resolve_server_script(
        std::env::var("CORTEX_7B_SERVER_SCRIPT").ok(),
        std::env::var("CORTEX_7B_START_SCRIPT").ok(),
        std::env::current_exe().ok().as_deref().and_then(|p| p.parent()).map(std::path::Path::to_path_buf).as_deref(),
        app.path().resource_dir().ok().as_deref(),
    )
}

pub(crate) fn resolve_client_script(app: &tauri::AppHandle) -> Option<String> {
    if let Some(path) = std::env::var("CORTEX_7B_CLIENT_SCRIPT").ok().filter(|path| !path.trim().is_empty()) {
        return Some(path);
    }
    let exe_dir = std::env::current_exe().ok().and_then(|path| path.parent().map(PathBuf::from));
    let resource_dir = app.path().resource_dir().ok();
    let candidates = [
        exe_dir.as_ref().map(|root| root.join("cortex_7b_client.py")),
        exe_dir.as_ref().map(|root| root.join("../../../../scripts/cortex_7b_client.py")),
        exe_dir.as_ref().map(|root| root.join("../../../scripts/cortex_7b_client.py")),
        exe_dir.as_ref().map(|root| root.join("../../scripts/cortex_7b_client.py")),
        resource_dir.as_ref().map(|root| root.join("cortex_7b_client.py")),
        resource_dir.as_ref().map(|root| root.join("scripts/cortex_7b_client.py")),
        resource_dir.as_ref().map(|root| root.join("_up_/scripts/cortex_7b_client.py")),
    ];
    if let Some(path) =
        candidates.into_iter().flatten().find_map(|path| path.canonicalize().ok().filter(|resolved| resolved.is_file()))
    {
        return Some(path.to_string_lossy().into_owned());
    }
    // Backward-compatible owner configuration. A POSIX path is already inside WSL; a Windows path
    // must exist before it can be executed. It is a last resort behind the bundled/repo script.
    app.try_state::<crate::AppState>()
        .and_then(|state| state.lock_settings().external_asr_script_path())
        .filter(|path| path.starts_with('/') || std::path::Path::new(path).is_file())
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChampionComponentSha256 {
    pub base: String,
    pub adapter: String,
    pub adapter_config: String,
    pub tokenizer: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadedChampionHealth {
    pub schema: u32,
    pub status: String,
    pub protocol: String,
    pub protocol_version: u32,
    pub family: String,
    pub model_version_id: String,
    pub deployment_sha256: String,
    pub manifest_sha256: String,
    pub component_sha256: ChampionComponentSha256,
    pub language: String,
    pub provenance_kind: String,
    pub worker: String,
}

impl LoadedChampionHealth {
    pub fn matches(&self, expected: &crate::registry::DeploymentIdentity) -> bool {
        self.schema == 1
            && self.status == "ready"
            && self.protocol == crate::deployment::SERVER_PROTOCOL
            && self.protocol_version == crate::deployment::SERVER_PROTOCOL_VERSION
            && self.family == crate::deployment::OMNIASR_7B_FAMILY
            && self.language == "ckb_Arab"
            && self.model_version_id == expected.model_version_id
            && self.deployment_sha256 == expected.deployment_sha256
    }
}

fn parse_health_marker(stdout: &str) -> Result<LoadedChampionHealth, String> {
    let mut health = None;
    for line in stdout.lines() {
        if let Some(payload) = line.strip_prefix("__HEALTH__=") {
            if health.is_some() {
                return Err("champion health client returned multiple __HEALTH__ lines".into());
            }
            health = Some(
                serde_json::from_str::<LoadedChampionHealth>(payload)
                    .map_err(|error| format!("champion health reply is invalid: {error}"))?,
            );
        }
    }
    let health = health.ok_or_else(|| "champion health client returned no __HEALTH__ line".to_string())?;
    for (label, value) in [
        ("deployment", health.deployment_sha256.as_str()),
        ("manifest", health.manifest_sha256.as_str()),
        ("base", health.component_sha256.base.as_str()),
        ("adapter", health.component_sha256.adapter.as_str()),
        ("adapter config", health.component_sha256.adapter_config.as_str()),
        ("tokenizer", health.component_sha256.tokenizer.as_str()),
    ] {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(format!("champion health {label} identity is not a canonical SHA-256"));
        }
    }
    crate::validation::input::validate_identifier(&health.model_version_id)
        .map_err(|error| format!("champion health model identity is invalid: {error}"))?;
    if health.worker.trim().is_empty() || !matches!(health.provenance_kind.as_str(), "flywheel" | "legacy_bootstrap") {
        return Err("champion health worker/provenance identity is invalid".into());
    }
    Ok(health)
}

/// Query the lightweight identity endpoint through the bundled client. The child, output and total
/// wall-clock wait are all bounded; an open port belonging to the wrong deployment never reads ready.
pub fn query_loaded_champion(app: &tauri::AppHandle, timeout: Duration) -> Result<LoadedChampionHealth, String> {
    let client = resolve_client_script(app).ok_or_else(|| "cortex_7b_client.py could not be resolved".to_string())?;
    query_loaded_champion_with_client(&client, timeout)
}

pub(crate) fn query_loaded_champion_with_client(
    client: &str,
    timeout: Duration,
) -> Result<LoadedChampionHealth, String> {
    let client_wsl =
        if client.starts_with('/') { client.to_string() } else { crate::pipeline::win_path_to_wsl(client) };
    let python = std::env::var("CORTEX_7B_PYTHON").unwrap_or_else(|_| "/home/ai/.venv-wsl-whisper/bin/python".into());
    let port = std::env::var("CORTEX_7B_PORT").unwrap_or_else(|_| crate::pipeline::WSL_7B_SERVER_PORT.to_string());
    let mut command = Command::new("wsl");
    command
        .arg("--")
        .arg("env")
        .arg(format!("CORTEX_7B_PORT={port}"))
        .arg(format!("CORTEX_7B_HEALTH_TIMEOUT_SECONDS={}", timeout.as_secs().max(1)))
        .arg(python)
        .arg(client_wsl)
        .arg("--health")
        .arg("--stdout-only")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|error| format!("champion health client launch failed: {error}"))?;
    const MAX_OUTPUT: u64 = 1024 * 1024;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(ref mut stream) = stdout {
            use std::io::Read;
            let _ = stream.take(MAX_OUTPUT).read_to_end(&mut bytes);
        }
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(ref mut stream) = stderr {
            use std::io::Read;
            let _ = stream.take(MAX_OUTPUT).read_to_end(&mut bytes);
        }
        bytes
    });
    let deadline = std::time::Instant::now() + timeout + Duration::from_secs(2);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                crate::pipeline::kill_and_reap_wsl_child(&mut child, "champion identity health probe");
                let _ = crate::pipeline::join_wsl_pipe_reader(stdout_reader, "health stdout");
                let _ = crate::pipeline::join_wsl_pipe_reader(stderr_reader, "health stderr");
                return Err(format!("champion identity health probe exceeded {}s", timeout.as_secs() + 2));
            }
            Err(error) => {
                crate::pipeline::kill_and_reap_wsl_child(&mut child, "failed champion identity health probe");
                let _ = crate::pipeline::join_wsl_pipe_reader(stdout_reader, "health stdout");
                let _ = crate::pipeline::join_wsl_pipe_reader(stderr_reader, "health stderr");
                return Err(format!("champion identity health wait failed: {error}"));
            }
        }
    };
    let stdout = crate::pipeline::join_wsl_pipe_reader(stdout_reader, "health stdout");
    let stderr = crate::pipeline::join_wsl_pipe_reader(stderr_reader, "health stderr");
    if !status.success() {
        let preview: String = String::from_utf8_lossy(&stderr).chars().take(500).collect();
        return Err(format!("champion identity health client failed: {}", preview.trim()));
    }
    parse_health_marker(&String::from_utf8_lossy(&stdout))
}

pub fn exact_champion_ready(app: &tauri::AppHandle, timeout: Duration) -> bool {
    if champion_operational_block_reason().is_some() {
        return false;
    }
    let expected = app.try_state::<crate::AppState>().and_then(|state| {
        crate::registry::champion_identity(&state.lock_db(), crate::deployment::OMNIASR_7B_FAMILY).ok().flatten()
    });
    expected.is_some_and(|expected| query_loaded_champion(app, timeout).is_ok_and(|loaded| loaded.matches(&expected)))
}

fn ensure_start_allowed(during_promotion: bool) -> Result<(), String> {
    if SHUTTING_DOWN.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("app is shutting down — not starting the champion server".to_string());
    }
    if !during_promotion && PROMOTION_RECOVERY_BLOCKED.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("champion start is blocked because promotion recovery is unverified".to_string());
    }
    Ok(())
}

fn start_child(app: &tauri::AppHandle, during_promotion: bool) -> Result<(), String> {
    ensure_start_allowed(during_promotion)?;
    kill_owned_child(); // never leak a previous child
    let script = server_script_path(app)
        .ok_or(
        "cortex_7b_server.py not found beside the app, in the repo layout, or via \n         CORTEX_7B_SERVER_SCRIPT / CORTEX_7B_START_SCRIPT",
    )?;
    let python = std::env::var("CORTEX_7B_PYTHON").unwrap_or_else(|_| "/home/ai/.venv-wsl-whisper/bin/python".into());
    let port = std::env::var("CORTEX_7B_PORT").unwrap_or_else(|_| "8799".into());
    let devices = std::env::var("CORTEX_7B_DEVICES").ok();
    let data_dir = app
        .try_state::<crate::AppState>()
        .and_then(|state| state.lock_data_dir().clone())
        .ok_or_else(|| "application data directory is unavailable for champion pointer resolution".to_string())?;
    let pointer = data_dir.join("champion.json");
    crate::atomic_file::recover_interrupted_replace(&pointer)
        .map_err(|error| format!("could not recover champion pointer: {error}"))?;
    if !pointer.is_file() {
        return Err(format!("champion pointer does not exist: {}", pointer.display()));
    }
    let script_wsl = crate::pipeline::win_path_to_wsl(&script);
    let pointer_wsl = crate::pipeline::win_path_to_wsl(&pointer.to_string_lossy());
    let args = champion_wsl_args(&script_wsl, &pointer_wsl, &python, &port, devices.as_deref());

    let logs = data_dir.join("logs");
    std::fs::create_dir_all(&logs).map_err(|error| format!("could not create champion log directory: {error}"))?;
    let log_path = logs.join("cortex_7b_server.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("could not open champion log {}: {error}", log_path.display()))?;
    let log_stderr = log.try_clone().map_err(|error| format!("could not clone champion log handle: {error}"))?;

    let mut cmd = Command::new("wsl");
    cmd.args(args);
    cmd.stdin(Stdio::null()).stdout(Stdio::from(log)).stderr(Stdio::from(log_stderr));
    let child = spawn_contained(cmd)?;
    tracing::info!("Champion supervision: started contained wsl child (pid {})", child.id());
    *lock_child() = Some(child);
    Ok(())
}

/// Restart the exact registry-selected deployment. Used by owner-approved promotion/rollback and
/// by the Start button; no detached PowerShell process or model-dir environment is involved.
pub fn restart_current_champion(app: &tauri::AppHandle) -> Result<(), String> {
    if PROMOTION_ACTIVE.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("champion restart is owned by an active promotion/recovery".to_string());
    }
    start_child(app, false)
}

/// Only the production promotion runtime may restart while it holds [`PromotionLease`].
pub(crate) fn restart_current_champion_for_promotion(app: &tauri::AppHandle) -> Result<(), String> {
    if !PROMOTION_ACTIVE.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("promotion restart requires the active promotion lease".to_string());
    }
    start_child(app, true)
}

/// App-exit hook: refuse any further starts (closing the Exit-vs-tick race), then kill the child.
pub fn begin_shutdown() {
    SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
    kill_owned_child();
}

/// Close the owned Job Object (tree-kill) and reap the launcher. Taking the global option makes
/// orderly stop idempotent; abnormal termination is covered by Windows closing the same handle.
pub fn kill_owned_child() {
    let child = { lock_child().take() };
    let Some(child) = child else { return };
    let pid = child.id();
    drop(child);
    tracing::info!("Champion supervision: stopped contained child tree (pid {pid})");
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
            if PROMOTION_ACTIVE.load(std::sync::atomic::Ordering::SeqCst)
                || PROMOTION_RECOVERY_BLOCKED.load(std::sync::atomic::Ordering::SeqCst)
            {
                continue;
            }
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
            let health_app = app.clone();
            let healthy =
                tokio::task::spawn_blocking(move || exact_champion_ready(&health_app, Duration::from_secs(3)))
                    .await
                    .unwrap_or(false);
            match state.tick(healthy, t0.elapsed()) {
                Decision::Restart => {
                    if let Err(e) = start_child(&app, false) {
                        tracing::warn!("Champion supervision: start failed: {e}");
                    }
                }
                Decision::GaveUp => {
                    if !gave_up_logged {
                        tracing::error!(
                            "Champion supervision GAVE UP after repeated failures — owner intervention required \
                             (toggle supervision off/on to reset, or use the champion restart control)."
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

    static GLOBAL_STATE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn champion_launch_is_an_argument_vector_not_shell_source() {
        let args = champion_wsl_args(
            "/mnt/c/a path/owner's/cortex_7b_server.py",
            "/mnt/c/data path/champion.json",
            "/python with space",
            "8799",
            Some("0,1"),
        );
        assert_eq!(args[0..2], ["--", "env"]);
        assert!(args.contains(&"CORTEX_7B_CHAMPION_POINTER=/mnt/c/data path/champion.json".to_string()));
        assert!(args.contains(&"CORTEX_7B_DEVICES=0,1".to_string()));
        assert_eq!(args.last().unwrap(), "/mnt/c/a path/owner's/cortex_7b_server.py");
        assert!(args.iter().all(|arg| !arg.contains("bash -c") && !arg.contains("CORTEX_7B_MODEL_DIR")));
    }

    #[test]
    fn champion_launch_without_devices_omits_the_env() {
        let args = champion_wsl_args("/s.py", "/p.json", "/py", "1", None);
        assert!(args.iter().all(|arg| !arg.starts_with("CORTEX_7B_DEVICES=")), "{args:?}");
    }

    #[test]
    fn kill_with_no_child_is_a_noop() {
        kill_owned_child();
        kill_owned_child();
    }

    #[test]
    fn promotion_lease_is_exclusive_and_releases_on_drop() {
        let _guard = GLOBAL_STATE_TEST_LOCK.lock().unwrap();
        PROMOTION_ACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
        let lease = acquire_promotion_lease().expect("first promotion owns the process-wide lease");
        assert!(acquire_promotion_lease().is_err(), "a concurrent promotion must fail before any mutation");
        drop(lease);
        let replacement = acquire_promotion_lease().expect("dropping the lease permits the next promotion");
        drop(replacement);
    }

    #[test]
    fn recovery_fence_blocks_ordinary_start_but_not_saga_restart() {
        let _guard = GLOBAL_STATE_TEST_LOCK.lock().unwrap();
        SHUTTING_DOWN.store(false, std::sync::atomic::Ordering::SeqCst);
        PROMOTION_RECOVERY_BLOCKED.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(champion_operational_block_reason().is_some());
        assert!(ensure_start_allowed(false).is_err());
        assert!(ensure_start_allowed(true).is_ok(), "the saga must be able to restart candidate or incumbent");
        PROMOTION_RECOVERY_BLOCKED.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(champion_operational_block_reason().is_none());
    }

    #[test]
    fn start_is_refused_after_begin_shutdown() {
        let _guard = GLOBAL_STATE_TEST_LOCK.lock().unwrap();
        // Closes the Exit-vs-tick race: a tick deciding Restart after the Exit handler ran must not
        // spawn a server nothing kills.
        begin_shutdown();
        let err = ensure_start_allowed(false).expect_err("start_child must refuse during shutdown");
        assert!(err.contains("shutting down"), "{err}");
        // Restore for any other test in this process (statics are process-global).
        SHUTTING_DOWN.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn the_champion_script_is_found_without_any_env_var() {
        // REGRESSION 2026-08-18. Both env vars are process-inherited, so an app launched by anything
        // that does not carry `CORTEX_7B_SERVER_SCRIPT` (set at USER scope on this machine) could
        // never start its champion — measured failing every ~8 minutes for two hours, with the models
        // sitting on disk the whole time. A file the app can FIND must not depend on an env var
        // surviving the launcher.
        let tmp = tempfile::TempDir::new().unwrap();
        // The dev/personal-use layout: exe at src-tauri/target/release, script at scripts/.
        let exe_dir = tmp.path().join("src-tauri/target/release");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let scripts = tmp.path().join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        let script = scripts.join("cortex_7b_server.py");
        std::fs::write(&script, b"# champion server").unwrap();

        let found = resolve_server_script(None, None, Some(&exe_dir), None)
            .expect("with no env at all, the script beside the app tree must still be found");
        assert!(std::path::Path::new(&found).is_file(), "resolution must return a real file, got {found}");
        assert!(found.ends_with("cortex_7b_server.py"), "resolved the wrong file: {found}");
    }

    #[test]
    fn an_explicit_override_still_wins_and_a_missing_script_fails_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe_dir = tmp.path().join("src-tauri/target/release");
        std::fs::create_dir_all(&exe_dir).unwrap();

        // Override beats everything, and is returned verbatim — an operator pointing at a specific
        // build must never be silently redirected to a file that happens to be lying around.
        assert_eq!(
            resolve_server_script(Some("D:/pinned/server.py".into()), None, Some(&exe_dir), None).as_deref(),
            Some("D:/pinned/server.py")
        );
        // An empty override is not an override.
        assert_eq!(resolve_server_script(Some("   ".into()), None, Some(&exe_dir), None), None);
        // Nothing anywhere: fail closed rather than invent a path the launcher would then fail on.
        assert_eq!(resolve_server_script(None, None, Some(&exe_dir), None), None);
    }

    fn health_json(model: &str, deployment: &str) -> String {
        format!(
            "__HEALTH__={{\"schema\":1,\"status\":\"ready\",\"protocol\":\"cortex-omniasr-adapter\",\"protocolVersion\":1,\"family\":\"omniasr-7b\",\"modelVersionId\":\"{model}\",\"deploymentSha256\":\"{deployment}\",\"manifestSha256\":\"{}\",\"componentSha256\":{{\"base\":\"{}\",\"adapter\":\"{}\",\"adapterConfig\":\"{}\",\"tokenizer\":\"{}\"}},\"language\":\"ckb_Arab\",\"provenanceKind\":\"flywheel\",\"worker\":\"gpu0\"}}",
            "b".repeat(64),
            "c".repeat(64),
            "d".repeat(64),
            "e".repeat(64),
            "f".repeat(64),
        )
    }

    #[test]
    fn exact_health_requires_both_model_id_and_raw_deployment_hash() {
        let expected = crate::registry::DeploymentIdentity {
            model_version_id: "candidate-1".into(),
            deployment_sha256: "a".repeat(64),
        };
        let loaded = parse_health_marker(&health_json("candidate-1", &"a".repeat(64))).unwrap();
        assert!(loaded.matches(&expected));

        let wrong_model = parse_health_marker(&health_json("candidate-2", &"a".repeat(64))).unwrap();
        assert!(!wrong_model.matches(&expected), "a right hash under the wrong registry id is not ready");
        let wrong_bytes = parse_health_marker(&health_json("candidate-1", &"9".repeat(64))).unwrap();
        assert!(!wrong_bytes.matches(&expected), "a right id serving different bytes is not ready");
    }

    #[test]
    fn health_parser_rejects_ambiguous_or_malformed_identity() {
        let valid = health_json("candidate-1", &"a".repeat(64));
        assert!(parse_health_marker(&format!("{valid}\n{valid}")).is_err(), "two health markers are ambiguous");
        assert!(parse_health_marker("__HEALTH__={\"status\":\"ready\"}").is_err());
        assert!(parse_health_marker("noise only").is_err());
    }

    /// Helper process for [`windows_job_close_kills_launcher_and_descendant`]. The gate makes the
    /// descendant start only after the parent has been assigned to the Job Object, eliminating a
    /// test-only assignment/inheritance race.
    #[cfg(target_os = "windows")]
    #[test]
    fn job_object_kill_on_close_helper() {
        let Ok(role) = std::env::var("CORTEX_JOB_DRILL_ROLE") else { return };
        match role.as_str() {
            "parent" => {
                let gate = PathBuf::from(std::env::var_os("CORTEX_JOB_DRILL_GATE").expect("missing drill gate"));
                let pid_file =
                    PathBuf::from(std::env::var_os("CORTEX_JOB_DRILL_DESC_PID").expect("missing descendant pid file"));
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !gate.is_file() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert!(gate.is_file(), "containment drill gate was never opened");

                let mut command = Command::new(std::env::current_exe().expect("test executable"));
                command
                    .arg("engine_runtime::tests::job_object_kill_on_close_helper")
                    .arg("--exact")
                    .arg("--nocapture")
                    .env("CORTEX_JOB_DRILL_ROLE", "descendant")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x08000000;
                command.creation_flags(CREATE_NO_WINDOW);
                let descendant = command.spawn().expect("spawn harmless drill descendant");
                std::fs::write(pid_file, descendant.id().to_string()).expect("publish descendant pid");

                // Both helpers are intentionally inert. The parent retains the Child handle while
                // the kernel Job Object, owned by the outer test, provides process-tree teardown.
                let _descendant = descendant;
                loop {
                    std::thread::sleep(Duration::from_secs(60));
                }
            }
            "descendant" => loop {
                std::thread::sleep(Duration::from_secs(60));
            },
            other => panic!("unknown containment drill role: {other}"),
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_job_close_kills_launcher_and_descendant() {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };

        const DEADLINE: Duration = Duration::from_secs(5);
        let tmp = tempfile::TempDir::new().expect("drill tempdir");
        let gate = tmp.path().join("assigned.gate");
        let pid_file = tmp.path().join("descendant.pid");
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .arg("engine_runtime::tests::job_object_kill_on_close_helper")
            .arg("--exact")
            .arg("--nocapture")
            .env("CORTEX_JOB_DRILL_ROLE", "parent")
            .env("CORTEX_JOB_DRILL_GATE", &gate)
            .env("CORTEX_JOB_DRILL_DESC_PID", &pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Exercise the exact production primitive: suspended create -> Job assignment -> resume.
        let mut owned = spawn_contained(command).expect("spawn and contain harmless drill launcher");
        std::fs::write(&gate, b"assigned").expect("release assigned helper");

        let publish_deadline = std::time::Instant::now() + DEADLINE;
        let descendant_pid = loop {
            if let Ok(raw) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = raw.trim().parse::<u32>() {
                    break pid;
                }
            }
            if let Ok(Some(status)) = owned.child.try_wait() {
                panic!("drill launcher exited before publishing its descendant: {status}");
            }
            assert!(
                std::time::Instant::now() < publish_deadline,
                "drill descendant was not published within {DEADLINE:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        };

        // SAFETY: the PID was just published by the live helper. The returned handle is checked and
        // closed below; requested rights only permit querying/waiting.
        let descendant_handle =
            unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, descendant_pid) };
        assert!(!descendant_handle.is_null(), "open drill descendant: {}", std::io::Error::last_os_error());
        // SAFETY: both process handles are live for these zero-time readiness checks.
        assert_eq!(
            unsafe { WaitForSingleObject(owned.child.as_raw_handle().cast(), 0) },
            WAIT_TIMEOUT,
            "launcher must be alive before close"
        );
        assert_eq!(
            unsafe { WaitForSingleObject(descendant_handle, 0) },
            WAIT_TIMEOUT,
            "descendant must be alive before close"
        );

        // This is the production stop primitive: last Job Object handle close, no taskkill/shell.
        drop(owned.job.take());
        // SAFETY: `descendant_handle` remains a valid waitable process handle until CloseHandle.
        let descendant_wait = unsafe { WaitForSingleObject(descendant_handle, DEADLINE.as_millis() as u32) };
        // SAFETY: unique local ownership; close exactly once.
        unsafe { CloseHandle(descendant_handle) };
        assert_eq!(descendant_wait, WAIT_OBJECT_0, "Job close did not kill/reap descendant by {DEADLINE:?}");

        let parent_deadline = std::time::Instant::now() + DEADLINE;
        loop {
            match owned.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < parent_deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => panic!("Job close did not kill/reap launcher by {DEADLINE:?}"),
                Err(error) => panic!("could not wait for contained launcher: {error}"),
            }
        }
    }
}
