# Review-pipeline health probe.
#
# WHY THIS EXISTS: the 2026-08-20 incident silenced six of eight reviewers for NINE DAYS because
# every break in the serving chain (funnel -> auth -> queue) was invisible from this machine: the
# watchdog probes 127.0.0.1 and the owner reads no log files. This probe walks the same path a
# reviewer's phone walks -- the PUBLIC funnel URL with real credentials -- and lands a visible
# alarm on the owner's screen the half-hour something breaks, instead of day nine.
#
# It composes the two existing read-only gates and adds NOTHING mutable:
#   check_reviewer_links_live.py --funnel   (public URL -> TLS -> auth for every distributed link)
#   check_reviewer_queues_live.py           (every authenticated reviewer has servable work)
# Both are verified non-mutating (claim probes are handled before cookie resolution; no decision
# endpoints are touched). This script only reads, writes its own log/heartbeat, and alarms.
#
# Alarm = a popup in the interactive session (msg.exe) + REVIEW-PIPELINE-ALERT.txt on the Desktop.
# Recovery clears the desktop flag on the next green run and says so in the log.

$ErrorActionPreference = 'Continue'
$repoApp   = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
# The repo's own locked interpreter, by ABSOLUTE path. Bare `python` resolved to another agent's
# venv in one shell and to nothing in the scheduled-task context — the first scheduled fire died
# before its first log line over exactly this. The .policy-python venv is what the policy suite
# itself pins, so the probe and the gates can never disagree about the interpreter again.
$python    = Join-Path $repoApp '.policy-python\Scripts\python.exe'
$logDir    = Join-Path $repoApp 'logs'
$logFile   = Join-Path $logDir 'review-health.log'
$heartbeat = Join-Path $logDir 'review-health.json'
$alertFile = Join-Path ([Environment]::GetFolderPath('Desktop')) 'REVIEW-PIPELINE-ALERT.txt'
$timeoutSec = 240

New-Item -ItemType Directory -Force $logDir | Out-Null

function Write-HealthLog([string]$line) {
    $stamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
    Add-Content -Path $logFile -Value "$stamp $line" -Encoding utf8
}

function Invoke-Gate([string]$name, [string[]]$gateArgs) {
    # Bounded, captured run of one read-only gate. A hung probe must never pile up behind the
    # 30-minute schedule, so the process is killed hard at $timeoutSec and that counts as RED.
    $out = Join-Path $env:TEMP ("review-health-" + $name + ".out")
    $err = Join-Path $env:TEMP ("review-health-" + $name + ".err")
    $p = Start-Process -FilePath $python -ArgumentList $gateArgs -WorkingDirectory $repoApp `
        -NoNewWindow -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
    # PS 5.1: without touching .Handle first, .ExitCode after WaitForExit() is $null, which made a
    # green gate compare as RED ("exit=") on this exact machine. Caught by the pre-register test.
    $null = $p.Handle
    if (-not $p.WaitForExit($timeoutSec * 1000)) {
        try { $p.Kill() } catch {}
        return @{ ok = $false; detail = "$name TIMED OUT after ${timeoutSec}s" }
    }
    $text = ((Get-Content $out -Raw -ErrorAction SilentlyContinue) + "`n" +
             (Get-Content $err -Raw -ErrorAction SilentlyContinue)).Trim()
    $tail = ($text -split "`n" | Select-Object -Last 6) -join ' | '
    return @{ ok = ($p.ExitCode -eq 0); detail = "$name exit=$($p.ExitCode): $tail" }
}

try {
if (-not (Test-Path $python)) {
    Write-HealthLog "ALERT probe misconfigured: locked interpreter missing at $python"
    exit 1
}

# NO auto-revive here — deliberately (added 2026-08-28, removed the same night). The
# CortexPrivateProductionWatchdog already relaunches a dead app within 5 minutes: it probes the
# port, binds the listener PID to the exact release exe, hash-verifies before launching, respects
# the importer's database lock, and caps consecutive kills. A second reviver on this 30-minute
# cycle could never beat it and, being lock-blind, would fight a headless batch import by
# relaunching the GUI into a held cortex.lock every half hour. Proven live: a 23:37:33 kill drill
# was revived at 23:37:44 and the watchdog's next tick (23:38:09) found a healthy app — the two
# supervisors do not race because this one no longer supervises. This probe DETECTS and ALARMS
# (the gates below go red the moment reviewers cannot work); the watchdog HEALS.
# 'continuity' answers the question the other two CANNOT: does the link the reviewer already has
# still work? Both gates below read the pairing token out of couch_session.json, so they authenticate
# whatever the server currently honours -- and stayed green through 2026-08-22..24 while a roster
# re-add reminted three tokens and killed three distributed links. Aram's phone said "link expired";
# this probe said OK. A reviewer holding a dead link cannot report it: all they see is the same page
# a network blip produces.
$results = @(
    Invoke-Gate 'links'      @('scripts\check_reviewer_links_live.py', '--funnel')
    Invoke-Gate 'queues'     @('scripts\check_reviewer_queues_live.py')
    Invoke-Gate 'continuity' @('scripts\check_reviewer_link_continuity.py')
    # 'vault' snapshots couch_session.json into link_vault/ whenever the credential set changes.
    # The tokens exist in exactly ONE file; Stop revokes them and corruption erases them. With a
    # snapshot, `reviewer_link_vault.py restore` resurrects the exact tokens, so the links already
    # on reviewers' phones keep working instead of being reminted.
    Invoke-Gate 'vault'      @('scripts\reviewer_link_vault.py', 'vault')
)
$red = @($results | Where-Object { -not $_.ok })

if ($red.Count -eq 0) {
    $summary = ($results | ForEach-Object { $_.detail }) -join ' || '
    Write-HealthLog "OK $summary"
    @{ at = (Get-Date -Format o); ok = $true; detail = $summary } |
        ConvertTo-Json -Compress | Set-Content -Path $heartbeat -Encoding utf8
    if (Test-Path $alertFile) {
        Remove-Item $alertFile -Force -ErrorAction SilentlyContinue
        Write-HealthLog 'RECOVERED - desktop alert cleared'
    }
    exit 0
}

$failText = ($red | ForEach-Object { $_.detail }) -join "`n"
Write-HealthLog "ALERT $($failText -replace "`n", ' // ')"
@{ at = (Get-Date -Format o); ok = $false; detail = $failText } |
    ConvertTo-Json -Compress | Set-Content -Path $heartbeat -Encoding utf8

$msg = "REVIEW PIPELINE BROKEN (reviewers may be locked out)`n`n$failText`n`n" +
       "Checked: funnel link auth + reviewer queues + continuity of the links already sent.`n" +
       "Re-run by hand: python scripts\check_reviewer_links_live.py --funnel  (in cortex-speech-app)`n" +
       "If it names REMINTED reviewers, re-send those links from Couch Review settings, then:`n" +
       "  python scripts\check_reviewer_link_continuity.py --accept`n" +
       "This alarm repeats every 30 min until the checks go green."
Set-Content -Path $alertFile -Value $msg -Encoding utf8
# msg.exe reaches the interactive desktop from a scheduled task; ignore failure (e.g. no session).
try { & msg.exe * /TIME:600 $msg 2>$null } catch {}
exit 1
} catch {
    # A probe that dies must SAY SO: the first scheduled run crashed before its first log line and
    # the only witness was a raw LastTaskResult. Anything landing here is a probe bug, not a
    # pipeline verdict.
    try { Write-HealthLog "ALERT probe crashed: $($_ | Out-String)" } catch {}
    exit 1
}
