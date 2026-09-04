# Review-pipeline health probe.
#
# WHY THIS EXISTS: the 2026-08-20 incident silenced six of eight reviewers for NINE DAYS because
# every break in the serving chain (funnel -> auth -> queue) was invisible from this machine: the
# watchdog probes 127.0.0.1 and the owner reads no log files. This probe walks the same path a
# reviewer's phone walks -- the PUBLIC funnel URL with real credentials -- and lands a visible
# alarm on the owner's screen the half-hour something breaks, instead of day nine.
#
# It composes the existing read-only reviewer gates and backs up the credential set:
#   check_reviewer_links_live.py --funnel   (public URL -> TLS -> auth for every distributed link)
#   check_reviewer_queues_live.py           (every authenticated reviewer has servable work)
# Claim probes are handled before cookie resolution; no decision endpoints are touched.
# The additional continuity check compares distributed credentials, and the vault snapshots them.
#
# Alarm = a popup in the interactive session (msg.exe) + REVIEW-PIPELINE-ALERT.txt on the Desktop.
# Recovery clears the desktop flag on the next green run and says so in the log.

param(
    [string]$PythonPath = '',
    [string]$LogDirectory = '',
    [string]$AlertPath = '',
    [ValidateRange(1, 240)][int]$GateTimeoutSeconds = 240,
    [switch]$NoPopup
)

$ErrorActionPreference = 'Stop'
$repoApp   = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
# The repo's own locked interpreter, by ABSOLUTE path. Bare `python` resolved to another agent's
# venv in one shell and to nothing in the scheduled-task context — the first scheduled fire died
# before its first log line over exactly this. The .policy-python venv is what the policy suite
# itself pins, so the probe and the gates can never disagree about the interpreter again.
$python    = Join-Path $repoApp '.policy-python\Scripts\python.exe'
$logDir    = Join-Path $repoApp 'logs'
$alertFile = Join-Path ([Environment]::GetFolderPath('Desktop')) 'REVIEW-PIPELINE-ALERT.txt'
if ($PythonPath) { $python = $PythonPath }
if ($LogDirectory) { $logDir = $LogDirectory }
if ($AlertPath) { $alertFile = $AlertPath }
$logFile = Join-Path $logDir 'review-health.log'
$heartbeat = Join-Path $logDir 'review-health.json'
$timeoutSec = $GateTimeoutSeconds

function Write-HealthLog([string]$line) {
    $stamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
    Add-Content -LiteralPath $logFile -Value "$stamp $line" -Encoding utf8
}

function Invoke-Gate([string]$name, [string[]]$gateArgs) {
    # Bounded, captured run of one read-only gate. A hung probe must never pile up behind the
    # 30-minute schedule, so the process is killed hard at $timeoutSec and that counts as RED.
    # ProcessStartInfo treats executable/working-directory paths literally, including brackets.
    # Both pipes drain asynchronously, so a verbose failing gate cannot deadlock the wait.
    $p = New-Object System.Diagnostics.Process
    try {
        $p.StartInfo.FileName = $python
        # These arguments are fixed relative script names and switches defined below, never shell text.
        $p.StartInfo.Arguments = $gateArgs -join ' '
        $p.StartInfo.WorkingDirectory = $repoApp
        $p.StartInfo.UseShellExecute = $false
        $p.StartInfo.CreateNoWindow = $true
        $p.StartInfo.RedirectStandardOutput = $true
        $p.StartInfo.RedirectStandardError = $true
        $p.StartInfo.EnvironmentVariables['PYTHONIOENCODING'] = 'utf-8'
        $p.StartInfo.StandardOutputEncoding = [System.Text.Encoding]::UTF8
        $p.StartInfo.StandardErrorEncoding = [System.Text.Encoding]::UTF8
        if (-not $p.Start()) { throw 'gate process did not start' }
        $stdout = $p.StandardOutput.ReadToEndAsync()
        $stderr = $p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit($timeoutSec * 1000)) {
            try { $p.Kill() } catch {}
            $null = $p.WaitForExit(5000)
            return @{ ok = $false; detail = "$name TIMED OUT after ${timeoutSec}s" }
        }
        $text = ($stdout.GetAwaiter().GetResult() + "`n" + $stderr.GetAwaiter().GetResult()).Trim()
        $tail = ($text -split "`n" | Select-Object -Last 6) -join ' | '
        if ($tail.Length -gt 2400) { $tail = '[truncated] ' + $tail.Substring($tail.Length - 2400) }
        return @{ ok = ($p.ExitCode -eq 0); detail = "$name exit=$($p.ExitCode): $tail" }
    } catch {
        return @{ ok = $false; detail = "$name probe failed: $($_.Exception.Message)" }
    } finally {
        try { if (-not $p.HasExited) { $p.Kill(); $null = $p.WaitForExit(5000) } } catch {}
        $p.Dispose()
    }
}

function Publish-HealthFailure([string]$detail) {
    # Each destination is attempted independently: a broken log path must not suppress the
    # desktop alert, and an unavailable desktop must not leave a previous green heartbeat.
    try { New-Item -ItemType Directory -Force $logDir | Out-Null } catch {}
    try { Write-HealthLog "ALERT $($detail -replace "`n", ' // ')" } catch { Write-Warning $_.Exception.Message }
    try {
        @{ at = (Get-Date -Format o); ok = $false; detail = $detail } |
            ConvertTo-Json -Compress | Set-Content -LiteralPath $heartbeat -Encoding utf8
    } catch { Write-Warning $_.Exception.Message }
    $message = "REVIEW PIPELINE CHECK FAILED (reviewer availability is not verified)`n`n$detail`n`n" +
        "Checked: public link authentication, reviewer queues, distributed-link continuity and credential backup.`n" +
        "Inspect review-health.log and the scheduled task result. This alarm repeats until checks recover."
    try { Set-Content -LiteralPath $alertFile -Value $message -Encoding utf8 } catch { Write-Warning $_.Exception.Message }
    if (-not $NoPopup) {
        try { & msg.exe * /TIME:600 $message 2>$null } catch {}
    }
}

try {
New-Item -ItemType Directory -Force $logDir | Out-Null
if (-not (Test-Path -LiteralPath $logDir -PathType Container)) { throw 'health log destination is not a directory' }
if (-not (Test-Path -LiteralPath $python -PathType Leaf)) {
    throw "probe misconfigured: locked interpreter missing at $python"
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
# re-add reminted three tokens and killed three distributed links. Alle's phone said "link expired";
# this probe said OK. A reviewer holding a dead link cannot report it: all they see is the same page
# a network blip produces.
$results = @(
    Invoke-Gate 'links'      @('scripts\check_reviewer_links_live.py', '--funnel', '--require-links', '--require-private-production')
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
        ConvertTo-Json -Compress | Set-Content -LiteralPath $heartbeat -Encoding utf8
    if (Test-Path -LiteralPath $alertFile) {
        Remove-Item -LiteralPath $alertFile -Force
        Write-HealthLog 'RECOVERED - desktop alert cleared'
    }
    exit 0
}

Publish-HealthFailure (($red | ForEach-Object { $_.detail }) -join "`n")
exit 1
} catch {
    # A probe that dies must SAY SO: the first scheduled run crashed before its first log line and
    # the only witness was a raw LastTaskResult. Anything landing here is a probe bug, not a
    # pipeline verdict.
    Publish-HealthFailure "probe crashed: $($_.Exception.Message)"
    exit 1
}
