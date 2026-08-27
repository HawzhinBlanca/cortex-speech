# Daily, isolated recovery proof for the newest verified Cortex snapshot.
param([switch]$Register, [switch]$DryRun)

$ErrorActionPreference = 'Stop'
$repoApp = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$restoreScript = Join-Path $repoApp 'scripts\restore_drill.py'
$dataDir = if ($env:CORTEX_RESTORE_DRILL_DATA_DIR) { $env:CORTEX_RESTORE_DRILL_DATA_DIR } else {
    Join-Path $env:APPDATA 'cortex-speech'
}
$logDir = Join-Path $dataDir 'logs'

if ($Register) {
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' `
        -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`""
    $trigger = New-ScheduledTaskTrigger -Daily -At '03:00'
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable `
        -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Minutes 15) `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName 'CortexDailyRestoreDrill' -Action $action -Trigger $trigger `
        -Settings $settings -Force | Out-Null
    Write-Output 'CortexDailyRestoreDrill registered: daily at 03:00, isolated, StartWhenAvailable.'
    exit 0
}

function Get-ConfiguredOffsiteRoot {
    $settingsPath = Join-Path $dataDir 'settings.json'
    if (-not (Test-Path -LiteralPath $settingsPath)) { return $null }
    try {
        $settings = Get-Content -LiteralPath $settingsPath -Raw | ConvertFrom-Json
        if ($settings.backup_second_dir) { return (Join-Path ([string]$settings.backup_second_dir) 'snapshots') }
    } catch { }
    return $null
}

function Get-LatestCompleteSnapshot([string[]]$roots) {
    $candidates = @()
    foreach ($root in $roots) {
        if (-not $root -or -not (Test-Path -LiteralPath $root)) { continue }
        $candidates += @(Get-ChildItem -LiteralPath $root -Directory -ErrorAction SilentlyContinue |
            Where-Object {
                $_.Name -match '^snapshot_[0-9]+$' -and
                (Test-Path -LiteralPath (Join-Path $_.FullName 'cortex-speech.db')) -and
                (Test-Path -LiteralPath (Join-Path $_.FullName 'SNAPSHOT_MANIFEST.json'))
            } |
            ForEach-Object {
                [pscustomobject]@{ Epoch = [int64]($_.Name.Substring(9)); Path = $_.FullName }
            })
    }
    return @($candidates | Sort-Object Epoch -Descending | Select-Object -First 1)[0]
}

$offsite = Get-ConfiguredOffsiteRoot
$local = Join-Path $dataDir 'snapshots'
$latest = Get-LatestCompleteSnapshot @($offsite, $local)
if ($null -eq $latest) {
    Write-Error 'No complete local or offsite periodic snapshot exists for the daily restore drill.'
    exit 1
}
if ($DryRun) {
    Write-Output "RESTORE-DRILL-ACTION: would restore $($latest.Path) into an isolated temporary profile"
    exit 0
}

$python = if (Get-Command py.exe -ErrorAction SilentlyContinue) { 'py.exe' } elseif (
    Get-Command python.exe -ErrorAction SilentlyContinue
) { 'python.exe' } else { throw 'Neither py.exe nor python.exe is available for restore_drill.py' }
$started = [DateTimeOffset]::UtcNow
$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$output = @(& $python $restoreScript $latest.Path 2>&1)
$exitCode = $LASTEXITCODE
$stopwatch.Stop()
$rtoSeconds = [Math]::Round($stopwatch.Elapsed.TotalSeconds, 3)
$pass = $exitCode -eq 0 -and $rtoSeconds -le 300
$report = [ordered]@{
    schema = 1
    startedAtUtc = $started.ToString('o')
    snapshot = $latest.Path
    snapshotEpochSecs = $latest.Epoch
    isolatedTemporaryProfile = $true
    rtoSeconds = $rtoSeconds
    targetRtoSeconds = 300
    restoreExitCode = $exitCode
    pass = $pass
    output = @($output | ForEach-Object { [string]$_ })
}
if (-not (Test-Path -LiteralPath $logDir)) { New-Item -ItemType Directory -Force -Path $logDir | Out-Null }
$final = Join-Path $logDir 'daily-restore-drill-latest.json'
$temp = Join-Path $logDir ('.daily-restore-drill-' + [guid]::NewGuid().ToString('N') + '.tmp')
[System.IO.File]::WriteAllText($temp, (($report | ConvertTo-Json -Depth 5) + [Environment]::NewLine))
Move-Item -LiteralPath $temp -Destination $final -Force
$output | Write-Output
Write-Output "RESTORE-DRILL-RTO-SECONDS: $rtoSeconds (target <= 300)"
if (-not $pass) { exit 1 }
exit 0
