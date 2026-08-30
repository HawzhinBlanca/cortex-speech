# Cortex alarm forwarder -- the single place where every detector's verdict becomes a notification.
#
# WHY THIS EXISTS (2026-08-30 audit): a power loss took the whole review service down for 14.4 hours
# (2026-08-29 19:11 -> 2026-08-30 09:33) and produced ZERO alerts. Every detector already worked --
# the probe wrote its log, the watchdog wrote its heartbeat, the drill wrote its report -- but every
# alarm terminated on this PC's own screen or in a log file nobody was reading. This script is the
# terminus that leaves the room.
#
# WHAT IT DOES, every 5 minutes, as a scheduled task registered with S4U (no stored password, runs
# whether the user is logged on or not -- so it keeps checking while the machine sits at the lock
# screen after an unattended reboot, which is exactly when the old Interactive-only tasks all slept):
#   1. Reads every existing detector's OUTPUT (never re-implements a check, never heals anything --
#      one healer per resource stays the law; this is detection-to-notification only):
#        probe alert file + probe liveness, watchdog liveness + give-up, pool certification,
#        restore-drill report, local + offsite snapshot age, disk headroom, champion port.
#   2. Dedupes: a persisting condition re-alerts every 6 hours, not every 5 minutes; a recovered
#      condition sends one recovery note and clears.
#   3. Notifies:
#        - ALWAYS: appends to <data>\logs\alarm-forwarder.log
#        - CRITICAL: writes CORTEX-ALARMS.txt on the Desktop and tries msg.exe (both are
#          best-effort -- at the lock screen there is no interactive session to receive them)
#        - If <data>\alert-webhook.url exists: POSTs the alarm text (plain text body) to that URL.
#          Works as-is with an ntfy topic, a healthchecks.io /fail endpoint, or any webhook relay
#          that accepts a text POST. THE OWNER CREATES THIS FILE; nothing is sent anywhere until
#          he chooses the destination.
#        - If <data>\healthcheck.url exists AND no CRITICAL condition holds: GETs it (dead-man
#          heartbeat -- the external service alarms on SILENCE, covering total host loss, which no
#          on-host code can ever report). This is the same file the production watchdog already
#          honours; creating it arms both.
#
# PowerShell 5.1. No pipeline-chain operators, no ternaries, every check individually try/caught so
# one broken source can never hide the others.

$ErrorActionPreference = 'Continue'

$dataDir   = Join-Path $env:APPDATA 'cortex-speech'
$repoApp   = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$logDir    = Join-Path $dataDir 'logs'
$logFile   = Join-Path $logDir 'alarm-forwarder.log'
$stateFile = Join-Path $dataDir 'alarm-forwarder-state.json'
$webhookFile     = Join-Path $dataDir 'alert-webhook.url'
$healthcheckFile = Join-Path $dataDir 'healthcheck.url'
$desktopAlarm    = Join-Path ([Environment]::GetFolderPath('Desktop')) 'CORTEX-ALARMS.txt'
$reAlertMinutes  = 360

New-Item -ItemType Directory -Force $logDir | Out-Null
# Cap the forwarder's own log so the alarm system can never become a disk-pressure source itself.
try {
    if ((Test-Path $logFile) -and ((Get-Item $logFile).Length -gt 5MB)) {
        $tail = Get-Content $logFile -Tail 2000
        Set-Content -Path $logFile -Value $tail -Encoding utf8
    }
} catch {}

function Write-FwdLog([string]$line) {
    $stamp = (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd HH:mm:ssZ')
    try { Add-Content -Path $logFile -Value "$stamp $line" -Encoding utf8 } catch {}
}

# ---------------------------------------------------------------------------------------------
# Collect findings: each is @{ Id; Severity ('CRITICAL'|'WARN'); Message }.
$findings = New-Object System.Collections.ArrayList

function Add-Finding([string]$id, [string]$severity, [string]$message) {
    [void]$findings.Add(@{ Id = $id; Severity = $severity; Message = $message })
}

function Newest-SnapshotAgeMinutes([string]$root) {
    # Rotating snapshots only: pinned/ is rotation-exempt and its age proves nothing about the net.
    $dirs = Get-ChildItem -Path $root -Directory -ErrorAction Stop | Where-Object { $_.Name -like 'snapshot_*' }
    $newest = $dirs | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1
    if ($null -eq $newest) { return $null }
    return [int]((Get-Date).ToUniversalTime() - $newest.LastWriteTimeUtc).TotalMinutes
}

# 1) The probe's own alarm file: it already diagnosed a reviewer-facing failure in detail.
try {
    $probeAlert = Join-Path ([Environment]::GetFolderPath('Desktop')) 'REVIEW-PIPELINE-ALERT.txt'
    if (Test-Path $probeAlert) {
        $body = (Get-Content $probeAlert -Tail 5 -ErrorAction Stop) -join ' | '
        Add-Finding 'probe-alert' 'CRITICAL' "review pipeline alert is raised: $body"
    }
} catch { Add-Finding 'probe-alert' 'WARN' "probe alert file unreadable: $($_.Exception.Message)" }

# 2) Probe liveness: the 30-minute funnel/queue/continuity/vault gate must itself be alive.
try {
    $probeLog = Join-Path $repoApp 'logs\review-health.log'
    if (-not (Test-Path $probeLog)) {
        Add-Finding 'probe-stale' 'CRITICAL' 'review-health probe has never written its log'
    } else {
        $age = [int]((Get-Date).ToUniversalTime() - (Get-Item $probeLog).LastWriteTimeUtc).TotalMinutes
        if ($age -gt 95) {
            Add-Finding 'probe-stale' 'CRITICAL' "review-health probe silent for ${age} min (30-min cadence) -- link/queue failures are currently invisible"
        }
    }
} catch { Add-Finding 'probe-stale' 'WARN' "probe liveness unreadable: $($_.Exception.Message)" }

# 3) Watchdog liveness + terminal give-up. It runs minutely; silence means nothing is healing the app.
try {
    $wdLog = Join-Path $logDir 'watchdog.log'
    if (-not (Test-Path $wdLog)) {
        Add-Finding 'watchdog-stale' 'CRITICAL' 'watchdog has never written its log'
    } else {
        $age = [int]((Get-Date).ToUniversalTime() - (Get-Item $wdLog).LastWriteTimeUtc).TotalMinutes
        if ($age -gt 20) {
            Add-Finding 'watchdog-stale' 'CRITICAL' "watchdog silent for ${age} min (minutely cadence) -- nothing is healing the app"
        }
        $recent = Get-Content $wdLog -Tail 200 -ErrorAction Stop
        $gaveUp = $recent | Where-Object { $_ -match 'give-up' } | Select-Object -Last 1
        if ($null -ne $gaveUp) {
            # Terminal state: the watchdog stopped healing on purpose and waits for a human.
            Add-Finding 'watchdog-gaveup' 'CRITICAL' "watchdog reached give-up and has stopped healing: $gaveUp"
        }
    }
} catch { Add-Finding 'watchdog-stale' 'WARN' "watchdog log unreadable: $($_.Exception.Message)" }

# 4) Pool certification: the watchdog re-certifies every 5 minutes; a red gate here is the paid
#    review path degrading (serving readiness, rights, disk) -- not campaign progress, which is
#    allowed to be incomplete (allClipsResolved/finalDatasetReady are progress, never alarms).
try {
    $certPath = Join-Path $logDir 'pool-certification.json'
    if (Test-Path $certPath) {
        $cert = Get-Content $certPath -Raw -ErrorAction Stop | ConvertFrom-Json
        $certAge = [int]((Get-Date).ToUniversalTime() - (Get-Item $certPath).LastWriteTimeUtc).TotalMinutes
        if ($certAge -gt 30) {
            Add-Finding 'cert-stale' 'WARN' "pool certification is ${certAge} min old (5-min cadence)"
        }
        if ($cert.gates.reviewReady -ne $true) {
            Add-Finding 'cert-review-ready' 'CRITICAL' 'pool certification: reviewReady=false -- the serving path is not certified'
        }
        if ($cert.gates.rightsComplete -ne $true) {
            Add-Finding 'cert-rights' 'CRITICAL' 'pool certification: rightsComplete=false'
        }
        if ($cert.disk.healthy -ne $true) {
            Add-Finding 'cert-disk' 'CRITICAL' 'pool certification: disk gate unhealthy'
        }
    }
} catch { Add-Finding 'cert-unreadable' 'WARN' "pool certification unreadable: $($_.Exception.Message)" }

# 5) Restore drill: a failing or stale drill means restores are UNPROVEN -- found at the worst
#    possible moment otherwise. Nothing consumed this report before today (audit finding).
try {
    $drillPath = Join-Path $logDir 'daily-restore-drill-latest.json'
    if (-not (Test-Path $drillPath)) {
        Add-Finding 'drill-missing' 'WARN' 'daily restore drill has never written a report'
    } else {
        $drill = Get-Content $drillPath -Raw -ErrorAction Stop | ConvertFrom-Json
        if ($drill.pass -ne $true) {
            Add-Finding 'drill-failed' 'CRITICAL' 'daily restore drill FAILED -- snapshots are not proven restorable'
        }
        $started = [DateTime]::Parse($drill.startedAtUtc, $null, [System.Globalization.DateTimeStyles]::RoundtripKind)
        $drillAgeHours = [int]((Get-Date).ToUniversalTime() - $started.ToUniversalTime()).TotalHours
        if ($drillAgeHours -gt 50) {
            Add-Finding 'drill-stale' 'WARN' "restore drill last ran ${drillAgeHours}h ago (daily cadence)"
        }
    }
} catch { Add-Finding 'drill-unreadable' 'WARN' "restore drill report unreadable: $($_.Exception.Message)" }

# 6) Snapshot nets, LOCAL and OFFSITE. The offsite copy's own death was warn-only and invisible by
#    design in-app (audit P1); its whole purpose is surviving loss of C:, so silence here is critical.
try {
    $localAge = Newest-SnapshotAgeMinutes (Join-Path $dataDir 'snapshots')
    if ($null -eq $localAge) {
        Add-Finding 'snapshots-local' 'CRITICAL' 'no local rotating snapshot exists'
    } elseif ($localAge -gt 60) {
        Add-Finding 'snapshots-local' 'CRITICAL' "newest LOCAL snapshot is ${localAge} min old (9-min cadence) -- the snapshot net is down"
    }
} catch { Add-Finding 'snapshots-local' 'CRITICAL' "local snapshot root unreadable: $($_.Exception.Message)" }

try {
    $settings = Get-Content (Join-Path $dataDir 'settings.json') -Raw -ErrorAction Stop | ConvertFrom-Json
    $offsiteRoot = $settings.backup_second_dir
    if ([string]::IsNullOrWhiteSpace($offsiteRoot)) {
        Add-Finding 'snapshots-offsite' 'WARN' 'no backup_second_dir configured -- a C: loss loses every backup'
    } else {
        $offsiteAge = Newest-SnapshotAgeMinutes (Join-Path $offsiteRoot 'snapshots')
        if ($null -eq $offsiteAge) {
            Add-Finding 'snapshots-offsite' 'CRITICAL' "no rotating snapshot under $offsiteRoot"
        } elseif ($offsiteAge -gt 60) {
            Add-Finding 'snapshots-offsite' 'CRITICAL' "newest OFFSITE snapshot is ${offsiteAge} min old -- the off-drive net died silently (C: loss would lose everything since)"
        }
        $offsiteDrive = [System.IO.Path]::GetPathRoot($offsiteRoot)
        $offsiteFree = (Get-PSDrive -Name $offsiteDrive.Substring(0,1) -ErrorAction Stop).Free
        if ($offsiteFree -lt 50GB) {
            Add-Finding 'disk-offsite-low' 'WARN' ("offsite drive {0} has {1:N0} GB free" -f $offsiteDrive, ($offsiteFree/1GB))
        }
    }
} catch { Add-Finding 'snapshots-offsite' 'CRITICAL' "offsite snapshot root unreachable: $($_.Exception.Message)" }

# 7) C: headroom. The known incident class: another process fills C:, the WAL cannot grow, reviewer
#    writes die at 0 bytes. WARN early, CRITICAL at the certification gate's own minimum.
try {
    $freeC = (Get-PSDrive -Name C -ErrorAction Stop).Free
    if ($freeC -lt 25GB) {
        Add-Finding 'disk-c-low' 'CRITICAL' ("C: has only {0:N0} GB free -- reviewer writes fail at 0" -f ($freeC/1GB))
    } elseif ($freeC -lt 75GB) {
        Add-Finding 'disk-c-low' 'WARN' ("C: is down to {0:N0} GB free" -f ($freeC/1GB))
    }
} catch { Add-Finding 'disk-c-low' 'WARN' "C: free space unreadable: $($_.Exception.Message)" }

# 8) Champion 7B server: reviewers are unaffected (they judge existing drafts), but every drafting
#    batch hard-stops by canon while it is down, and after a reboot only a human restarts it.
try {
    $appUp = $null -ne (Get-Process -Name 'cortex-speech-app' -ErrorAction SilentlyContinue)
    if ($appUp) {
        $champion = Test-NetConnection -ComputerName 127.0.0.1 -Port 8799 -WarningAction SilentlyContinue -InformationLevel Quiet
        if (-not $champion) {
            Add-Finding 'champion-down' 'WARN' 'champion 7B server (port 8799) is down -- drafting hard-stops until start_7b_server.ps1 is run'
        }
    } else {
        Add-Finding 'app-down' 'WARN' 'cortex-speech-app.exe is not running (watchdog should be reviving it -- see watchdog findings if this persists)'
    }
} catch { Add-Finding 'champion-down' 'WARN' "champion probe failed: $($_.Exception.Message)" }

# ---------------------------------------------------------------------------------------------
# Dedup against persisted state, then notify.
$state = @{}
try {
    if (Test-Path $stateFile) {
        $raw = Get-Content $stateFile -Raw -ErrorAction Stop | ConvertFrom-Json
        foreach ($property in $raw.PSObject.Properties) { $state[$property.Name] = $property.Value }
    }
} catch { $state = @{} }

$nowUtc = (Get-Date).ToUniversalTime()
$toSend = New-Object System.Collections.ArrayList
$activeIds = @{}

foreach ($finding in $findings) {
    $activeIds[$finding.Id] = $true
    $previous = $state[$finding.Id]
    $shouldSend = $true
    if ($null -ne $previous) {
        try {
            $last = [DateTime]::Parse($previous.lastAlertUtc, $null, [System.Globalization.DateTimeStyles]::RoundtripKind)
            if (($nowUtc - $last.ToUniversalTime()).TotalMinutes -lt $reAlertMinutes) { $shouldSend = $false }
        } catch {}
    }
    if ($shouldSend) {
        [void]$toSend.Add($finding)
        $state[$finding.Id] = @{ lastAlertUtc = $nowUtc.ToString('o'); severity = $finding.Severity }
    }
    Write-FwdLog "$($finding.Severity) $($finding.Id): $($finding.Message)"
}

# Recovery notes: anything previously alerted that no longer trips gets one all-clear line.
$recovered = New-Object System.Collections.ArrayList
foreach ($id in @($state.Keys)) {
    if (-not $activeIds.ContainsKey($id)) {
        [void]$recovered.Add($id)
        $state.Remove($id)
    }
}

try { $state | ConvertTo-Json -Depth 4 | Set-Content -Path $stateFile -Encoding utf8 } catch {}

$critical = @($findings | Where-Object { $_.Severity -eq 'CRITICAL' })

if ($findings.Count -eq 0 -and $recovered.Count -eq 0) {
    Write-FwdLog 'OK all checks green'
}

# Desktop file + popup for CRITICAL (best-effort; unreachable at the lock screen, which is what the
# webhook and the dead-man exist for).
if ($critical.Count -gt 0) {
    try {
        $lines = @("CORTEX ALARMS -- $($nowUtc.ToString('yyyy-MM-dd HH:mm')) UTC") + ($critical | ForEach-Object { "[$($_.Severity)] $($_.Id): $($_.Message)" })
        Set-Content -Path $desktopAlarm -Value ($lines -join [Environment]::NewLine) -Encoding utf8
    } catch {}
    try { & msg.exe * /TIME:30 "CORTEX: $($critical.Count) critical alarm(s) -- see CORTEX-ALARMS.txt" 2>$null } catch {}
} elseif (Test-Path $desktopAlarm) {
    # All-clear: the forwarder removes only its OWN file, never the probe's.
    try { Remove-Item $desktopAlarm -Force } catch {}
}

# Webhook: plain-text POST, one line per newly-raised or re-raised alarm plus recovery notes.
if (Test-Path $webhookFile) {
    $webhook = (Get-Content $webhookFile -TotalCount 1).Trim()
    $bodyLines = @($toSend | ForEach-Object { "[$($_.Severity)] $($_.Id): $($_.Message)" })
    foreach ($id in $recovered) { $bodyLines += "[RESOLVED] ${id}: condition cleared" }
    if ($bodyLines.Count -gt 0 -and $webhook -match '^https://') {
        try {
            Invoke-RestMethod -Method Post -Uri $webhook -Body ($bodyLines -join "`n") -TimeoutSec 20 | Out-Null
            Write-FwdLog "webhook: sent $($bodyLines.Count) line(s)"
        } catch { Write-FwdLog "webhook: send FAILED: $($_.Exception.Message)" }
    }
}

# Dead-man heartbeat: silence (host down, forwarder dead, power loss) makes the external service
# alarm. Deliberately NOT pinged while a critical condition holds, so a degraded-but-up host also
# trips it.
if ((Test-Path $healthcheckFile) -and $critical.Count -eq 0) {
    $ping = (Get-Content $healthcheckFile -TotalCount 1).Trim()
    if ($ping -match '^https://') {
        try {
            Invoke-RestMethod -Method Get -Uri $ping -TimeoutSec 20 | Out-Null
            Write-FwdLog 'heartbeat: pinged'
        } catch { Write-FwdLog "heartbeat: ping FAILED: $($_.Exception.Message)" }
    }
}

if ($critical.Count -gt 0) { exit 1 } else { exit 0 }
