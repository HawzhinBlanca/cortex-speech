# Crash/power-loss recovery arm for a Cortex private-production handover.
param([switch]$Register)

$ErrorActionPreference = 'Stop'
$releaseRoot = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$controller = Join-Path $releaseRoot 'scripts\release_private_production.py'
$dataDir = Join-Path $env:APPDATA 'cortex-speech'

if ($Register) {
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' `
        -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`""
    $trigger = New-ScheduledTaskTrigger -Once -At ((Get-Date).AddMinutes(2)) `
        -RepetitionInterval (New-TimeSpan -Minutes 5) -RepetitionDuration (New-TimeSpan -Hours 2)
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable `
        -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Minutes 10) `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName 'CortexReleaseRecovery' -Action $action -Trigger $trigger `
        -Settings $settings -Force | Out-Null
    Write-Output 'CortexReleaseRecovery armed: first check in two minutes, then every five minutes for two hours.'
    exit 0
}

$python = if (Get-Command py.exe -ErrorAction SilentlyContinue) { 'py.exe' } elseif (
    Get-Command python.exe -ErrorAction SilentlyContinue
) { 'python.exe' } else { throw 'Neither py.exe nor python.exe is available for release recovery' }

& $python $controller recover --data-dir $dataDir
exit $LASTEXITCODE
