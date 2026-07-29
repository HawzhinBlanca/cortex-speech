# One-time ELEVATED tweaks for always-on operation — phase 3 of docs/REMOTE_PUBLIC_LINKS_PLAN.md.
# Run once, as administrator (right-click -> Run with PowerShell as admin). Everything here is
# reversible and each line says how.
#
# Why these six and nothing else is argued in the plan; the short form:
#   1. Fast Startup boots skip Task Scheduler logon triggers on some builds — the watchdog's
#      autostart must not depend on which KIND of boot happened.  (undo: powercfg /h on)
#   2. A NIC that sleeps kills Tailscale reachability while the PC looks awake.
#   3. Active hours = the 18h max window keeps Windows Update reboots inside the small hours;
#      recovery from those reboots is the watchdog's job, prevention is not attempted.
#   4. Disk idle spin-down on AC pauses the whole app for seconds on wake — off, like sleep already is.
#   5. ARSO signs the user back in after an UPDATE restart and leaves the screen locked, which is the
#      only thing that gives the (necessarily interactive-only) watchdog a session to run in.
#   6. A read-only BitLocker check: a pre-boot PIN makes unattended recovery impossible, full stop.
#
# This script never touches the account password. Autologin — the only thing that also covers power
# cuts and manual restarts — is deliberately left to the owner via Sysinternals Autologon.

$ErrorActionPreference = 'Continue'

Write-Output "1/6 Fast Startup off (hiberboot skips logon triggers)..."
powercfg /h off

Write-Output "2/6 NIC power management off on every up adapter..."
Get-NetAdapter -Physical | Where-Object Status -eq 'Up' | ForEach-Object {
    Disable-NetAdapterPowerManagement -Name $_.Name -NoRestart -ErrorAction SilentlyContinue
    Write-Output "   $($_.Name): power management disabled"
}

Write-Output "3/6 Windows Update active hours 06:00-24:00 (reboots land 00:00-06:00)..."
$ux = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\UX\Settings'
Set-ItemProperty -Path $ux -Name ActiveHoursStart -Value 6  -Type DWord
Set-ItemProperty -Path $ux -Name ActiveHoursEnd   -Value 0  -Type DWord
Set-ItemProperty -Path $ux -Name SmartActiveHoursState -Value 0 -Type DWord  # stop Windows re-learning them

Write-Output "4/6 Disk never idles on AC..."
powercfg /change disk-timeout-ac 0

# 5. THE REBOOT HOLE. CortexWatchdog is registered "run only when user is logged on", because a
#    WebView2 GUI cannot render in Session 0 (this is why NSSM/WinSW are ruled out, not a preference).
#    So after a reboot with nobody logged on, the watchdog never fires and the review server stays
#    down until someone walks to the PC.
#
#    ARSO (Automatic Restart Sign-On) closes that for the case that actually causes reboots here:
#    Windows Update. After an update-initiated restart Windows signs the user back in and then LOCKS
#    the workstation -- a locked session is still a logged-on interactive session, so scheduled tasks
#    run. Secure AND recovered, with no password stored anywhere.
#
#    Scope, honestly: ARSO covers UPDATE-initiated restarts only. A power cut or a manual restart
#    still lands at the sign-in screen with the server down. Closing THAT needs autologin (Sysinternals
#    Autologon, which stores the password as an LSA secret) -- an owner step, deliberately not scripted
#    here, because nothing in this repo should ever handle the account password.
#    (undo: Set-ItemProperty $wlKey -Name ARSOUserConsent -Value 0)
Write-Output "5/6 Auto sign-in after update restarts (ARSO; screen stays locked)..."
$wlKey = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
Set-ItemProperty -Path $wlKey -Name ARSOUserConsent -Value 1 -Type DWord
$sysPol = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\System'
if (Test-Path $sysPol) {
    # An explicit policy of 1 here overrides the consent above; clear it rather than fight it.
    Remove-ItemProperty -Path $sysPol -Name DisableAutomaticRestartSignOn -ErrorAction SilentlyContinue
}
Write-Output ("   ARSOUserConsent = " + (Get-ItemProperty $wlKey -Name ARSOUserConsent).ARSOUserConsent)

# 6. Read-only check, not a change. BitLocker with a pre-boot PIN or password protector stops the
#    machine BEFORE Windows starts, so no amount of ARSO or autologin brings the server back
#    unattended. Worth knowing which world this box is in; querying it needs admin, which is why the
#    check lives here.
Write-Output "6/6 BitLocker pre-boot check (read-only)..."
try {
    Get-BitLockerVolume -ErrorAction Stop | ForEach-Object {
        $prot = ($_.KeyProtector | ForEach-Object { $_.KeyProtectorType }) -join ','
        Write-Output ("   $($_.MountPoint) status=$($_.VolumeStatus) protection=$($_.ProtectionStatus) protectors=$prot")
        if ($prot -match 'Pin|Password') {
            Write-Warning "   $($_.MountPoint) has a PRE-BOOT PIN/password: unattended reboot recovery is IMPOSSIBLE while that protector exists."
        }
    }
} catch { Write-Output "   BitLocker not available or not enabled on this machine." }

Write-Output ""
Write-Output "Done. Update restarts now self-recover (ARSO + CortexWatchdog)."
Write-Output "Power cuts and manual restarts still need autologin - an owner step, see the comment at 5/6."
