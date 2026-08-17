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

# -DryRun decides and REPORTS without killing or launching anything, so every branch can be drilled.
#
# This script had no test of any kind, and its most dangerous branch is the one that force-kills. The
# branch that must LEAVE A HEALTHY APP ALONE (the owner pressed Stop) was reviewed but never verified,
# and it could not be: proving it for real means pressing Stop, which deletes the session file and
# revokes the owner's live link. Ports and paths are overridable for the same reason — a drill must
# never touch the real profile. Production behaviour is unchanged when neither is set.
param([switch]$Register, [switch]$DryRun)

$ErrorActionPreference = 'Stop'
$repoApp = Resolve-Path (Join-Path $PSScriptRoot '..\..')   # cortex-speech-app/
# CORTEX_WATCHDOG_EXE exists for the same reason the DATA_DIR/PORT overrides do: the three branches
# that depend on a LIVE process could only ever be drilled when the real app happened to be running,
# so the coverage of the force-kill decision — the most dangerous line in the availability path — was
# a coin toss on machine state. With the exe path overridable, the drill points this at a harmless
# decoy process it starts itself and reaches all three deterministically. Production is unaffected
# when unset.
$exe = if ($env:CORTEX_WATCHDOG_EXE) { $env:CORTEX_WATCHDOG_EXE } else {
    Join-Path $repoApp 'src-tauri\target\release\cortex-speech-app.exe'
}
$dataDir = if ($env:CORTEX_WATCHDOG_DATA_DIR) { $env:CORTEX_WATCHDOG_DATA_DIR } else { Join-Path $env:APPDATA 'cortex-speech' }
$port = if ($env:CORTEX_WATCHDOG_PORT) { $env:CORTEX_WATCHDOG_PORT } else { '8737' }
# TLS FIRST, then plain HTTP. Couch Review serves TLS on every interface with a self-signed
# certificate it generates locally, so an http:// probe against it does not fail cleanly — it comes
# back as "corrupt message of type InvalidContentType", which this script read as a DEAD PORT and
# answered by force-killing a perfectly healthy app, every five minutes, forever.
#
# MEASURED 2026-08-16: exactly that. Three probes 5s apart, then a kill, then a relaunch, in a loop —
# the watchdog became the outage it exists to prevent. Both schemes are tried because the same script
# has to supervise an older HTTP build and the current TLS one.
#
# Certificate validation is disabled for THIS probe only: the certificate is self-signed by design
# and this is a localhost liveness check, not an authentication decision. It reads no response body.
$probeUrls = @("https://127.0.0.1:$port/", "http://127.0.0.1:$port/")
try {
    Add-Type -TypeDefinition @'
using System.Net;
using System.Security.Cryptography.X509Certificates;
public static class CortexProbeCerts {
    public static void TrustAll() {
        ServicePointManager.ServerCertificateValidationCallback = delegate { return true; };
        ServicePointManager.SecurityProtocol = SecurityProtocolType.Tls12;
    }
}
'@ -ErrorAction Stop
    [CortexProbeCerts]::TrustAll()
} catch {
    # Already loaded in this session, or the type exists — either way the callback is set.
}
$logDir = Join-Path $dataDir 'logs'
$log = Join-Path $logDir 'watchdog.log'

# The one line a drill reads. Printed for every decision so a test asserts the CHOICE, not a side effect.
function Report([string]$action) { Write-Output "WATCHDOG-ACTION: $action" }

# Logging must never be able to stop the watchdog. With $ErrorActionPreference = 'Stop' a failed
# Add-Content (full disk, the log opened by an editor, a locked profile) threw and aborted the run
# BEFORE the kill/relaunch below — so the one condition most likely to take the app down was also the
# condition that disabled its recovery. Recovering matters more than recording why.
function Write-Log([string]$msg) {
    try {
        if (-not (Test-Path $logDir)) { New-Item -ItemType Directory -Force $logDir | Out-Null }
        Add-Content -Path $log -Value ("{0}  {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $msg)
    } catch { }
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
    # Battery flags are ON by default and would silently disable the whole watchdog the moment Windows
    # believes it is on battery — which includes a desktop behind a UPS, exactly the machine most
    # likely to be running this. An always-on server must not stop healing itself during a power event.
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable `
        -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName 'CortexWatchdog' -Action $action -Trigger $logon `
        -Settings $settings -Force | Out-Null
    Write-Log "registered (exe: $exe)"
    Write-Output "CortexWatchdog registered: at-logon + every 5 minutes, run-only-when-logged-on."
    exit 0
}

# ── the probe ──────────────────────────────────────────────────────────────────
# THREE attempts, and a timeout longer than anything the server may legitimately be inside.
#
# The old single 5s probe force-killed healthy apps. couch.rs spawns ONE accept thread per reviewer,
# and while that thread is inside handle_request it is not accepting — so the probe's connection sits
# in the listen backlog unanswered. Two ordinary things hold it past 5s: any DB call contending with
# the desktop app (busy_timeout is 10s — twice the old probe), and materialising a clip's WAV. A
# transport timeout leaves $_.Exception.Response empty, which read as "dead", so the busier the
# reviewer the likelier the watchdog was to kill the app mid-review and destroy their in-flight work.
#
# 20s clears busy_timeout with margin, and one answer out of three attempts is enough to prove life.
$alive = $false
foreach ($attempt in 1..3) {
    foreach ($probeUrl in $probeUrls) {
        try {
            Invoke-WebRequest -Uri $probeUrl -UseBasicParsing -TimeoutSec 20 | Out-Null
            $alive = $true   # 2xx/3xx
        } catch {
            # A status-carrying refusal (401 et al.) is the server ANSWERING — alive. Only a transport
            # failure (refused, timeout, reset) leaves .Response empty and means dead.
            if ($null -ne $_.Exception.Response) { $alive = $true }
            # An SSL/content-type mismatch is ALSO the server answering — it spoke, we mis-heard it.
            # Treating a protocol mismatch as death is what turned this watchdog into a kill loop.
            elseif ($_.Exception.Message -match 'SSL|TLS|secure channel|corrupt message|InvalidContentType') { $alive = $true }
        }
        if ($alive) { break }
    }
    if ($alive) { break }
    if ($attempt -lt 3) { Start-Sleep -Seconds 5 }
}

# Consecutive forced kills that did NOT restore service. Killing is only ever justified as a way to
# fix a wedge; when it demonstrably is not fixing one, continuing is just an app that dies every five
# minutes forever. Reset the moment the server answers.
$killCountFile = Join-Path $logDir 'watchdog-kills.txt'
$maxConsecutiveKills = 3

# STARTUP GRACE. A freshly launched app that has not opened 8737 YET is indistinguishable from a
# wedged one by port alone — and the difference is the whole decision. Startup work scales with the
# library: measured 2026-08-14 it took 8m16s to reach couch, and on 2026-08-15, after the library
# tripled to 14,828 clips, it exceeded the 5-minute check interval entirely. The watchdog then killed
# the app at 5 minutes, three times in a row, and the app never once got far enough to serve. A
# watchdog that shortens startup until startup can never finish is a denial of service on its own app.
#
# 10 minutes. THE THING THIS WAS SIZED FOR NO LONGER EXISTS (2026-08-17). The grace was 45 minutes
# because startup to couch measured 8m16s at ~500 clips and 18m11s at 14,828, and "it scales with the
# library" was taken as a fact of life. It was one line: Database::backup paced the SQLite online
# backup at 5 pages per 250 ms — 80 KB/s — and take_snapshot runs SYNCHRONOUSLY on the startup path,
# so the whole delay was the app copying its own database at dial-up speed before opening the port.
# Fixed there; MEASURED startup to couch afterwards: 6.4 SECONDS at 14,828 clips.
#
# So the generosity now costs what it was buying. At 45 minutes a genuinely wedged app sits unnoticed
# for three quarters of an hour — and the old comment's claim that being slow "costs ONE extra check
# cycle" was never true. 10 minutes is ~90x the measured startup, which is ample headroom for a much
# larger library or a busy disk, while cutting the worst case for a wedged app by more than four
# times. The kill path still works for anything older than this.
# Overridable so the decision tests can exercise BOTH sides: 0 makes every process instantly
# "old enough" to reach the kill path, which is the only way to test the kill path without waiting
# 20 real minutes.
$startupGraceMinutes = if ($env:CORTEX_WATCHDOG_STARTUP_GRACE_MIN) { [double]$env:CORTEX_WATCHDOG_STARTUP_GRACE_MIN } else { 10 }

if ($alive) {
    if (Test-Path $killCountFile) { Remove-Item $killCountFile -Force -ErrorAction SilentlyContinue }
    Report 'alive'
    if ($DryRun) { exit 0 }
    # Optional dead-man ping: silence at healthchecks.io alerts the owner's phone.
    $hcFile = Join-Path $dataDir 'healthcheck.url'
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
$session = Join-Path $dataDir 'couch_session.json'
# Matched by PATH, not by name. `Get-Process -Name cortex-speech-app` force-kills ANY process with
# that name — a second checkout, a debug build, an installed copy under Program Files — while the
# relaunch below only ever starts THIS one. The watchdog would happily kill a build it is not
# responsible for and then report success. (A process whose Path cannot be read is not ours to kill.)
$exeFull = try { (Resolve-Path $exe -ErrorAction Stop).Path } catch { $exe }
$proc = @(Get-Process -Name cortex-speech-app -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path -eq $exeFull })
if (-not (Test-Path $session)) {
    if ($proc.Count) { Report 'leave-alone (deliberate Stop)'; exit 0 }   # the app is fine
    Report 'launch (no session, not running)'
    Write-Log "app not running (no session) - launching for availability"
} elseif ($proc.Count) {
    # THE KILL LOOP. A present session file does NOT mean couch can come up. resume() swallows every
    # start failure (something else holding 8737), and load_session refuses a session whose db_path
    # moved — both leave the file on disk with the port dead and the app running, forever. Killing
    # then never helps and the app dies every five minutes, losing in-flight work each time.
    # Youngest instance decides: if ANY matching process is still inside its startup grace, the port
    # being closed is expected, not evidence of a wedge.
    $youngestMin = ($proc | ForEach-Object { ((Get-Date) - $_.StartTime).TotalMinutes } | Measure-Object -Minimum).Minimum
    if ($youngestMin -lt $startupGraceMinutes) {
        Report ("starting-up ({0:N1} min old, grace {1} min)" -f $youngestMin, $startupGraceMinutes)
        Write-Log ("port not open yet but the app is only {0:N1} min old (grace {1} min) - leaving it alone to finish starting" -f $youngestMin, $startupGraceMinutes)
        exit 0
    }
    $kills = 0
    if (Test-Path $killCountFile) { $kills = [int]((Get-Content $killCountFile -TotalCount 1).Trim()) }
    if ($kills -ge $maxConsecutiveKills) {
        Report 'give-up (kill cap reached)'
        Write-Log "port still dead after $kills forced restarts - NOT killing again; couch cannot start (port taken, or the library moved). Owner action needed."
        exit 1
    }
    Report "kill-and-relaunch (attempt $($kills + 1)/$maxConsecutiveKills)"
    if ($DryRun) { exit 0 }
    Write-Log "session expected but port dead - killing wedged pid(s): $($proc.Id -join ', ') (attempt $($kills + 1)/$maxConsecutiveKills)"
    $proc | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2   # flock.rs clears the stale lock on the next start
    try { Set-Content -Path $killCountFile -Value ([string]($kills + 1)) -Encoding utf8 } catch { }
} else {
    Report 'relaunch (session expected, not running)'
    # WHY the app was gone is the whole question, and until the exit marker existed this line could not
    # answer it: "session expected but app not running" reads identically whether the owner closed the
    # window or the process died. The app clears logs\last-exit.txt at every start and writes it only
    # from RunEvent::Exit, so present = it got there, absent = it did not. Five relaunches in the week
    # of 2026-07-27 are all recorded without this distinction and stay unattributable.
    $exitMarker = Join-Path $logDir 'last-exit.txt'
    $how = if (Test-Path $exitMarker) {
        "clean exit ($((Get-Content $exitMarker -TotalCount 1 -EA SilentlyContinue)))"
    } else {
        'NO exit marker - died without reaching shutdown'
    }
    Write-Log "session expected but app not running - relaunching [$how]"
}
if ($DryRun) { exit 0 }
if (-not (Test-Path $exe)) {
    Write-Log "exe missing at $exe - nothing to launch (mid-rebuild?)"
    exit 1
}
Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe)
Write-Log "launched $exe"
