# Cortex watchdog — phase 3 of docs/REMOTE_PUBLIC_LINKS_PLAN.md.
#
# One mechanism is both the autostart and the crash/wedge healer: a scheduled task runs this at
# logon and every 5 minutes. It probes the review server and relaunches the app only when the
# server is genuinely unreachable — so a healthy app is never touched, and a wedged one (process
# alive, port dead) is killed and restarted rather than trusted.
#
# Registration (one-time, as the logged-on user — see docs/REMOTE_PUBLIC_LINKS_PLAN.md):
#   powershell -ExecutionPolicy Bypass -File cortex-watchdog.ps1 -Register
# To stop the app ON PURPOSE without it resurrecting within 5 minutes:
#   schtasks /change /tn CortexWatchdog /disable   (re-enable with /enable)
#
# Design notes, argued in the plan:
#   * ANY HTTP status counts as alive — 401 is the server's normal answer to an unauthenticated
#     probe, and auth working IS the server working. Only refused/timeout means dead.
#   * "Run only when user is logged on": a WebView2 GUI cannot render in Session 0, which is why
#     service wrappers (NSSM/WinSW) are ruled out entirely.
#   * Paths are derived from this script's own location, never hardcoded — the repo's hygiene gate
#     forbids private absolute paths, and the watchdog must survive the repo being moved.
#   * The optional dead-man ping reads %APPDATA%\cortex-speech\healthcheck.url (untracked, owner
#     creates it). The GET carries liveness and source IP only — no data, per the privacy stance.

param([switch]$Register)

$ErrorActionPreference = 'Stop'
$repoApp = Resolve-Path (Join-Path $PSScriptRoot '..\..')   # cortex-speech-app/
$exe = Join-Path $repoApp 'src-tauri\target\release\cortex-speech-app.exe'
$probeUrl = 'http://127.0.0.1:8737/'
$logDir = Join-Path $env:APPDATA 'cortex-speech\logs'
$log = Join-Path $logDir 'watchdog.log'

function Write-Log([string]$msg) {
    if (-not (Test-Path $logDir)) { New-Item -ItemType Directory -Force $logDir | Out-Null }
    Add-Content -Path $log -Value ("{0}  {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $msg)
}

if ($Register) {
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' `
        -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`""
    # Two triggers: at logon (the autostart), and a repeating clock (the healer). Task Scheduler
    # caps a repetition trigger's duration; (New-TimeSpan -Days 3650) is rejected on Win11, so the
    # logon trigger carries an indefinite repetition instead.
    $logon = New-ScheduledTaskTrigger -AtLogOn
    $logon.Repetition = (New-ScheduledTaskTrigger -Once -At (Get-Date) `
        -RepetitionInterval (New-TimeSpan -Minutes 5)).Repetition
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable `
        -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero)
    Register-ScheduledTask -TaskName 'CortexWatchdog' -Action $action -Trigger $logon `
        -Settings $settings -Force | Out-Null
    Write-Log "registered (exe: $exe)"
    Write-Output "CortexWatchdog registered: at-logon + every 5 minutes, run-only-when-logged-on."
    exit 0
}

# ── the probe ──────────────────────────────────────────────────────────────────
$alive = $false
try {
    $resp = Invoke-WebRequest -Uri $probeUrl -UseBasicParsing -TimeoutSec 5
    $alive = $true   # 2xx/3xx
} catch {
    # A status-carrying refusal (401 et al.) is the server ANSWERING — alive. Only a transport
    # failure (refused, timeout, reset) leaves .Response empty and means dead.
    if ($null -ne $_.Exception.Response) { $alive = $true }
}

if ($alive) {
    # Optional dead-man ping: silence at healthchecks.io alerts the owner's phone.
    $hcFile = Join-Path $env:APPDATA 'cortex-speech\healthcheck.url'
    if (Test-Path $hcFile) {
        $hc = (Get-Content $hcFile -TotalCount 1).Trim()
        if ($hc -match '^https://') {
            try { Invoke-WebRequest -Uri $hc -UseBasicParsing -TimeoutSec 10 | Out-Null } catch {}
        }
    }
    exit 0
}

# ── port dead: decide WHY before touching anything ────────────────────────────
# A closed port has two honest meanings, and only the session file tells them apart:
#   * couch_session.json EXISTS  -> the server is SUPPOSED to be serving (resume would bring it up),
#     so a running process with a dead port is wedged: kill and relaunch.
#   * no session file            -> the owner pressed Stop. A running app with couch off is a
#     HEALTHY state — killing it here would resurrect-loop the app every 5 minutes and make Stop
#     feel haunted. Leave a running app alone; only launch if the process itself is gone
#     (the autostart half of this task).
$session = Join-Path $env:APPDATA 'cortex-speech\couch_session.json'
$proc = Get-Process -Name cortex-speech-app -ErrorAction SilentlyContinue
if (-not (Test-Path $session)) {
    if ($proc) { exit 0 }   # deliberate Stop; the app is fine
    Write-Log "app not running (no session) - launching for availability"
} elseif ($proc) {
    Write-Log "session expected but port dead - killing wedged pid(s): $($proc.Id -join ', ')"
    $proc | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2   # flock.rs clears the stale lock on the next start
} else {
    Write-Log "session expected but app not running - relaunching"
}
if (-not (Test-Path $exe)) {
    Write-Log "exe missing at $exe - nothing to launch (mid-rebuild?)"
    exit 1
}
Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe)
Write-Log "launched $exe"
