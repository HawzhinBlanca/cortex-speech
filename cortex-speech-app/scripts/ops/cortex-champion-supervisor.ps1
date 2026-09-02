# Cortex Speech - keep the OmniASR-7B champion server alive across reboots (owner-registered).
#
# The reviewer line has a watchdog (cortex-watchdog.ps1); the champion had nothing: measured
# 2026-09-02, after the 00:06 reboot every reviewer link came back and the champion stayed dark on
# port 8799 until a person started it by hand. This script is that missing supervisor. Each pass calls
# the repo's idempotent start_7b_server.ps1, which answers READY without touching a champion that
# already serves the exact registry pointer and otherwise launches it behind its heartbeat guard.
#
#   -DryRun     print exactly what a pass or a registration would do; touch nothing.
#   -Register   register the scheduled task (at-logon + every 5 minutes, this interactive user,
#               run-only-when-logged-on). Registering a task is a privileged boot configuration:
#               the OWNER runs this, never an agent.
#   (no switch) one supervision pass.
#
# Read-only verification lives in scripts/check_champion_supervision.py. Nothing here ever changes
# the champion registry pointer, the model, or the GPU clocks.
param(
    [switch]$Register,
    [switch]$DryRun,
    [string]$TaskName = 'CortexChampionSupervisor'
)
$ErrorActionPreference = 'Stop'
$repoApp = Resolve-Path (Join-Path $PSScriptRoot '..\..')   # cortex-speech-app/
$starter = Join-Path $repoApp 'scripts\start_7b_server.ps1'
if (-not (Test-Path -LiteralPath $starter)) { throw "champion starter not found at $starter" }
$logDir = if ($env:CORTEX_CHAMPION_SUPERVISOR_LOG_DIR) { $env:CORTEX_CHAMPION_SUPERVISOR_LOG_DIR } else { Join-Path $env:APPDATA 'cortex-speech\logs' }
$log = Join-Path $logDir 'champion-supervisor.log'
function Write-Log([string]$message) {
    if ($DryRun) { return }
    New-Item -ItemType Directory -Force -Path $logDir | Out-Null
    Add-Content -LiteralPath $log -Value ("{0} {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $message)
}

if ($Register) {
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' `
        -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`""
    # Same shape as the reviewer watchdog: an at-logon trigger bound to the exact interactive
    # principal (an unscoped AtLogOn means "any user" and needs administrator rights), plus a clock
    # trigger that starts within a minute and repeats every 5 minutes so a registration made after
    # logon is not unmonitored until the next sign-in.
    $currentPrincipal = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
    $logon = New-ScheduledTaskTrigger -AtLogOn -User $currentPrincipal
    $clock = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) `
        -RepetitionInterval (New-TimeSpan -Minutes 5)
    # A champion launch loads ~17 GB per GPU and can take minutes: no execution time limit, and a
    # pass that overlaps a still-running one is dropped, never doubled. Battery flags stay ON so a
    # desktop behind a UPS keeps healing during a power event.
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable `
        -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    if ($DryRun) {
        Write-Output "DRY RUN: would register '$TaskName' for ${currentPrincipal}: at-logon + every 5 minutes, StartWhenAvailable, no time limit, IgnoreNew."
        Write-Output "DRY RUN: action = powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`""
        Write-Output "DRY RUN: nothing registered."
        exit 0
    }
    Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger @($logon, $clock) `
        -Settings $settings -Force | Out-Null
    Write-Log "registered (starter: $starter)"
    Write-Output "$TaskName registered: at-logon + every 5 minutes, run-only-when-logged-on."
    exit 0
}

# One supervision pass. start_7b_server.ps1 is idempotent and identity-bound: READY when the exact
# pointer-selected champion already answers, otherwise a guarded launch that waits for readiness.
if ($DryRun) {
    Write-Output "DRY RUN: would run one pass: powershell -NoProfile -ExecutionPolicy Bypass -File `"$starter`""
    Write-Output "DRY RUN: the starter is idempotent (READY exits without touching a serving champion); nothing executed."
    exit 0
}
Write-Log "pass: invoking $starter"
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $starter
$code = $LASTEXITCODE
Write-Log "pass: starter exit $code"
exit $code
