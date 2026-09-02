# Registers the CortexAlarmForwarder scheduled task. Needs elevation ONCE, because the task uses
# S4U logon (no stored password, "run whether user is logged on or not") -- the property that lets
# the alarm forwarder keep checking while the machine sits at the lock screen after an unattended
# reboot, which is exactly when every Interactive-logon cortex task sleeps.
# Safe to re-run: -Force replaces the existing definition.
$ErrorActionPreference = 'Stop'

$script = Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) 'cortex-alarm-forwarder.ps1'
if (-not (Test-Path $script)) { throw "forwarder script not found at $script" }

$action = New-ScheduledTaskAction -Execute 'powershell.exe' `
    -Argument "-NoProfile -ExecutionPolicy Bypass -File `"$script`""
$trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) `
    -RepetitionInterval (New-TimeSpan -Minutes 5)
$principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType S4U -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Minutes 4)

Register-ScheduledTask -TaskName 'CortexAlarmForwarder' -Action $action -Trigger $trigger `
    -Principal $principal -Settings $settings -Force | Out-Null
Write-Output 'CortexAlarmForwarder registered (S4U, every 5 minutes).'
