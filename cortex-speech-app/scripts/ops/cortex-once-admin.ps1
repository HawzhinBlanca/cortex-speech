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

# EVERY step VERIFIES rather than announcing. The first version printed a success line per step and
# ended with "Done." — and on its first real run (2026-08-02, owner's box) step 3 threw three
# PathNotFound errors while step 2 printed "power management disabled" for an adapter whose driver
# exposes no power-management properties at all. Two of six did nothing and the script said Done.
# That is the same shape as every other defect this repo has hunted: reporting an outcome nobody
# checked. Results are collected here and printed as a verified summary at the end.
$results = [ordered]@{}

Write-Output "1/6 Fast Startup off (hiberboot skips logon triggers)..."
powercfg /h off
# VERIFIED against `powercfg /a`, not the HiberbootEnabled registry flag. That flag can read 1 on a
# machine where Fast Startup is impossible, because Fast Startup is hybrid shutdown BUILT ON
# hibernation: with hibernation unavailable the flag is inert. Reading the flag alone reports a
# problem that does not exist (it did, on this box, before this comment was written).
$sleep = (powercfg /a) -join "`n"
$fastStartupPossible = $sleep -notmatch '(?s)Fast Startup.*?Hibernation is not available'
$results['1 Fast Startup cannot occur'] = if ($fastStartupPossible) { 'NO - still possible, check powercfg /a' } else { 'yes' }

Write-Output "2/6 NIC power management off on every up adapter..."
foreach ($nic in @(Get-NetAdapter -Physical | Where-Object Status -eq 'Up')) {
    # NO -NoRestart. That switch STAGES the change and leaves the running adapter untouched, so the
    # first version set nothing and the read-back said "still Enabled" — correctly, and the script
    # called it verified anyway (see the summary logic below, which had the same disease). Applying it
    # bounces the adapter for a few seconds; this is a one-time local admin step, and a NIC that
    # sleeps is precisely what makes the machine unreachable while it looks awake.
    Write-Output "   $($nic.Name): applying (the adapter will bounce for a few seconds)..."
    Disable-NetAdapterPowerManagement -Name $nic.Name -ErrorAction SilentlyContinue
    # Read it BACK, elevated. NOTE: this cmdlet returns NOTHING when run unelevated, which reads as
    # "the driver has no power-management settings" and is wrong — do not diagnose this from a normal
    # shell.
    $pm = Get-NetAdapterPowerManagement -Name $nic.Name -ErrorAction SilentlyContinue
    $results["2 NIC '$($nic.Name)'"] = if ($null -eq $pm) {
        'UNKNOWN - no power-management object returned (are you elevated?)'
    } elseif ($pm.AllowComputerToTurnOffDevice -eq 'Disabled') { 'disabled' } else { "still $($pm.AllowComputerToTurnOffDevice)" }
}

Write-Output "3/6 Windows Update active hours 06:00-24:00 (reboots land 00:00-06:00)..."
$ux = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\UX\Settings'
# CREATE the key first. It does not exist until something writes an update setting, so on a machine
# where the owner has never touched active hours, all three writes below throw PathNotFound — which is
# exactly what happened on the first real run. -Force creates the whole path and is a no-op if present.
if (-not (Test-Path $ux)) { New-Item -Path $ux -Force | Out-Null }
Set-ItemProperty -Path $ux -Name ActiveHoursStart -Value 6  -Type DWord
Set-ItemProperty -Path $ux -Name ActiveHoursEnd   -Value 0  -Type DWord
Set-ItemProperty -Path $ux -Name SmartActiveHoursState -Value 0 -Type DWord  # stop Windows re-learning them
$got = Get-ItemProperty -Path $ux -ErrorAction SilentlyContinue
$results['3 Active hours 06:00-24:00'] =
    if ($got.ActiveHoursStart -eq 6 -and $got.ActiveHoursEnd -eq 0) { 'set' } else { "NOT SET (start=$($got.ActiveHoursStart) end=$($got.ActiveHoursEnd))" }

Write-Output "4/6 Disk never idles on AC..."
powercfg /change disk-timeout-ac 0
$diskAc = (powercfg /query SCHEME_CURRENT SUB_DISK DISKIDLE | Select-String 'Current AC Power Setting Index') -replace '.*:\s*', ''
$results['4 Disk idle on AC'] = if ($diskAc -match '0x00000000') { 'never' } else { "NOT SET ($diskAc)" }

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
$arso = (Get-ItemProperty $wlKey -Name ARSOUserConsent -ErrorAction SilentlyContinue).ARSOUserConsent
$blocked = (Get-ItemProperty $sysPol -Name DisableAutomaticRestartSignOn -ErrorAction SilentlyContinue).DisableAutomaticRestartSignOn
$results['5 ARSO after update restart'] =
    if ($arso -eq 1 -and $blocked -ne 1) { 'enabled' } else { "NOT ENABLED (consent=$arso policyBlock=$blocked)" }

# 6. Read-only check, not a change. BitLocker with a pre-boot PIN or password protector stops the
#    machine BEFORE Windows starts, so no amount of ARSO or autologin brings the server back
#    unattended. Worth knowing which world this box is in; querying it needs admin, which is why the
#    check lives here.
Write-Output "6/6 BitLocker pre-boot check (read-only)..."
$preBoot = @()
try {
    Get-BitLockerVolume -ErrorAction Stop | ForEach-Object {
        $prot = ($_.KeyProtector | ForEach-Object { $_.KeyProtectorType }) -join ','
        Write-Output ("   $($_.MountPoint) status=$($_.VolumeStatus) protection=$($_.ProtectionStatus) protectors=$prot")
        if ($prot -match 'Pin|Password') {
            $preBoot += $_.MountPoint
            Write-Warning "   $($_.MountPoint) has a PRE-BOOT PIN/password: unattended reboot recovery is IMPOSSIBLE while that protector exists."
        }
    }
    $results['6 No pre-boot PIN'] = if ($preBoot.Count) { "NO - $($preBoot -join ',') would block unattended recovery" } else { 'yes' }
} catch {
    $results['6 No pre-boot PIN'] = 'BitLocker not available on this machine'
}

# The summary is the deliverable. Anything not verified says so in its own words rather than being
# folded into a cheerful "Done." — the owner has to know WHICH of these actually took, because the
# ones that did not are exactly the ones that will surprise him after a reboot.
Write-Output ""
Write-Output "===== VERIFIED RESULT (read back from the system, not from what was attempted) ====="
# WHITELIST the good states. The first version blacklisted values starting with NO/NOT, which missed
# "still Enabled" and printed "All items verified" over an item that plainly had not taken — the exact
# failure this summary exists to prevent, reintroduced inside the fix for it. A checker that only
# catches the failures its author imagined is not a checker. Anything not explicitly known-good is
# treated as NOT done.
$GOOD = @('yes', 'set', 'never', 'enabled', 'disabled', 'BitLocker not available on this machine')
$failed = 0
foreach ($k in $results.Keys) {
    $v = $results[$k]
    $ok = $GOOD -contains $v
    if (-not $ok) { $failed++ }
    Write-Output ("  {0,-34} {1}{2}" -f $k, $v, $(if ($ok) { '' } else { '   <-- NOT DONE' }))
}
Write-Output ""
if ($failed) {
    Write-Warning "$failed item(s) above did NOT take. Re-read them - the script attempted every step, but attempting is not doing."
} else {
    Write-Output "All items verified. Update restarts self-recover (ARSO + CortexWatchdog)."
}
Write-Output "Power cuts and manual restarts still need autologin - an owner step, see the comment at 5/6."
