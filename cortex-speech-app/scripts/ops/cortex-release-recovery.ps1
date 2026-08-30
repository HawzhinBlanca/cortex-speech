# Crash/power-loss recovery arm for a Cortex private-production handover.
param([switch]$Register)

$ErrorActionPreference = 'Stop'
$releaseRoot = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$controller = Join-Path $releaseRoot 'scripts\release_private_production.py'
$dataDir = Join-Path $env:APPDATA 'cortex-speech'

if ($Register) {
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' `
        -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`""
    # 24 hours, not the original 2: a spent -Once trigger never fires again, and a recovery that
    # keeps failing (funnel down after a power event, python missing at boot) used to burn its
    # whole window in silence and then leave every couch route 503 forever with no further
    # attempts. Failures now also leave a breadcrumb the alarm forwarder pages on
    # (logs\release-recovery-failure.json), so the longer window is watched, not just longer.
    $trigger = New-ScheduledTaskTrigger -Once -At ((Get-Date).AddMinutes(2)) `
        -RepetitionInterval (New-TimeSpan -Minutes 5) -RepetitionDuration (New-TimeSpan -Hours 24)
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable `
        -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Minutes 10) `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName 'CortexReleaseRecovery' -Action $action -Trigger $trigger `
        -Settings $settings -Force | Out-Null
    Write-Output 'CortexReleaseRecovery armed: first check in two minutes, then every five minutes for 24 hours.'
    exit 0
}

$python = if (Get-Command py.exe -ErrorAction SilentlyContinue) { 'py.exe' } elseif (
    Get-Command python.exe -ErrorAction SilentlyContinue
) { 'python.exe' } else { throw 'Neither py.exe nor python.exe is available for release recovery' }

& $python $controller recover --data-dir $dataDir
exit $LASTEXITCODE
