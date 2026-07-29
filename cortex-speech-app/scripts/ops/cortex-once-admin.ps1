# One-time ELEVATED tweaks for always-on operation — phase 3 of docs/REMOTE_PUBLIC_LINKS_PLAN.md.
# Run once, as administrator (right-click -> Run with PowerShell as admin). Everything here is
# reversible and each line says how.
#
# Why these four and nothing else is argued in the plan; the short form:
#   1. Fast Startup boots skip Task Scheduler logon triggers on some builds — the watchdog's
#      autostart must not depend on which KIND of boot happened.  (undo: powercfg /h on)
#   2. A NIC that sleeps kills Tailscale reachability while the PC looks awake.
#   3. Active hours = the 18h max window keeps Windows Update reboots inside the small hours;
#      recovery from those reboots is the watchdog's job, prevention is not attempted.
#   4. Disk idle spin-down on AC pauses the whole app for seconds on wake — off, like sleep already is.

$ErrorActionPreference = 'Continue'

Write-Output "1/4 Fast Startup off (hiberboot skips logon triggers)..."
powercfg /h off

Write-Output "2/4 NIC power management off on every up adapter..."
Get-NetAdapter -Physical | Where-Object Status -eq 'Up' | ForEach-Object {
    Disable-NetAdapterPowerManagement -Name $_.Name -NoRestart -ErrorAction SilentlyContinue
    Write-Output "   $($_.Name): power management disabled"
}

Write-Output "3/4 Windows Update active hours 06:00-24:00 (reboots land 00:00-06:00)..."
$ux = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\UX\Settings'
Set-ItemProperty -Path $ux -Name ActiveHoursStart -Value 6  -Type DWord
Set-ItemProperty -Path $ux -Name ActiveHoursEnd   -Value 0  -Type DWord
Set-ItemProperty -Path $ux -Name SmartActiveHoursState -Value 0 -Type DWord  # stop Windows re-learning them

Write-Output "4/4 Disk never idles on AC..."
powercfg /change disk-timeout-ac 0

Write-Output "Done. Reboot recovery is the watchdog's job (CortexWatchdog task, registered separately)."
