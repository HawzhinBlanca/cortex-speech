"""Pins for the alarm forwarder — the terminus that makes every detector's verdict leave the room.

Written 2026-08-30, the day after a 14.4-hour total outage (power loss 19:11 → logon 09:33)
produced ZERO notifications while every detector worked: the probe logged, the watchdog logged,
the drill reported — and every alarm died on this PC's own screen. The forwarder reads each
detector's OUTPUT and notifies (Desktop + popup + optional owner-configured webhook + dead-man
heartbeat). These pins keep three properties from regressing:

  1. COVERAGE — every detector stays wired: dropping one check silently re-blinds the system.
  2. DETECTION-ONLY — the forwarder never heals anything (one healer per resource is the law;
     a second reviver was already shipped and reverted once, 2026-08-2x).
  3. CONSENT — nothing leaves the machine except to URLs the OWNER placed in the two config
     files, and only over https.

Content pins only: this suite also runs on Linux/macOS CI, where schtasks and the live task do
not exist, so the gate reads the scripts — never the machine (the cross-platform-vacuous gate
class is the documented failure mode this avoids).
"""

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
FORWARDER = REPO_ROOT / "scripts" / "ops" / "cortex-alarm-forwarder.ps1"
REGISTRAR = REPO_ROOT / "scripts" / "ops" / "register-alarm-forwarder.ps1"


def _pin(source: str, needle: str, why: str) -> None:
    if needle not in source:
        raise AssertionError(f"alarm forwarder lost a pinned property ({why}): missing {needle!r}")


def test_pin_helper_is_not_vacuous() -> None:
    try:
        _pin("real content", "a needle that is certainly absent", "self-check")
    except AssertionError:
        return
    raise AssertionError("_pin accepted a missing needle — every pin below would pass vacuously")


def test_every_detector_is_wired() -> None:
    src = FORWARDER.read_text(encoding="utf-8-sig")
    for check_id, anchor in (
        ("probe-alert", "REVIEW-PIPELINE-ALERT.txt"),
        ("probe-stale", "review-health.log"),
        ("watchdog-stale", "watchdog.log"),
        ("watchdog-gaveup", "give-up"),
        ("cert-review-ready", "reviewReady"),
        ("cert-rights", "rightsComplete"),
        ("cert-disk", "disk.healthy"),
        ("drill-failed", "daily-restore-drill-latest.json"),
        ("snapshots-local", "'snapshots'"),
        ("snapshots-offsite", "backup_second_dir"),
        ("disk-c-low", "Get-PSDrive -Name C"),
        ("champion-down", "8799"),
        # A stranded handover 503-blocks every reviewer while watchdog AND probe both stay green
        # (adversarially confirmed 2026-08-30) -- only this check sees it.
        ("handover-stranded", "private-production-maintenance.json"),
        ("recovery-failing", "release-recovery-failure.json"),
    ):
        _pin(src, f"'{check_id}'", f"detector {check_id} must stay wired")
        _pin(src, anchor, f"detector {check_id} must read its real source")


def test_forwarder_detects_and_never_heals() -> None:
    src = FORWARDER.read_text(encoding="utf-8-sig")
    for forbidden, why in (
        ("Start-Process", "the forwarder must never launch or revive anything"),
        ("Stop-Process", "the forwarder must never kill anything"),
        ("Restart-", "the forwarder must never restart anything"),
        ("schtasks /run", "the forwarder must never trigger another healer"),
        ("Remove-Item $probeAlert", "the probe owns its own alert file; the forwarder may remove only its own"),
    ):
        if forbidden in src:
            raise AssertionError(f"alarm forwarder crossed detection-only: {forbidden!r} — {why}")
    # The one file it may clean up is its own Desktop alarm, and only on all-clear.
    _pin(src, "Remove-Item $desktopAlarm", "own-file cleanup on recovery")


def test_nothing_leaves_the_machine_without_owner_consent() -> None:
    src = FORWARDER.read_text(encoding="utf-8-sig")
    # Both destinations come from owner-created files; no URL literal is baked in.
    _pin(src, "alert-webhook.url", "webhook destination is owner-configured")
    _pin(src, "healthcheck.url", "dead-man destination is owner-configured")
    for scheme_guard in ("$webhook -match '^https://'", "$ping -match '^https://'"):
        _pin(src, scheme_guard, "outbound calls require https")
    lowered = src.lower()
    for literal in ("https://ntfy", "https://hc-ping", "https://hooks.", "https://discord"):
        if literal in lowered:
            raise AssertionError(
                f"alarm forwarder hardcodes an external destination ({literal!r}) — the owner chooses, in the config file"
            )


def test_alarm_hygiene_dedup_heartbeat_and_bounded_log() -> None:
    src = FORWARDER.read_text(encoding="utf-8-sig")
    _pin(src, "$reAlertMinutes  = 360", "a persisting condition re-alerts every 6h, not every 5min")
    _pin(src, "alarm-forwarder-state.json", "dedup state survives restarts")
    _pin(src, "[RESOLVED]", "recovery sends an all-clear, so a cleared alarm never lingers as fear")
    # The dead-man deliberately goes SILENT while a critical condition holds, so a degraded-but-up
    # host still trips the external service.
    _pin(src, "-and $critical.Count -eq 0", "no heartbeat while critical")
    _pin(src, "5MB", "the forwarder's own log is size-capped, never a disk-pressure source")
    # Campaign PROGRESS is not an alarm: allClipsResolved/finalDatasetReady are legitimately false
    # mid-campaign and must never page anyone.
    for progress_gate in ("allClipsResolved", "finalDatasetReady"):
        occurrences = [line for line in src.splitlines() if progress_gate in line and "Add-Finding" in line]
        if occurrences:
            raise AssertionError(f"campaign progress gate {progress_gate!r} must never raise an alarm: {occurrences}")


def test_registrar_keeps_the_lock_screen_property() -> None:
    src = REGISTRAR.read_text(encoding="utf-8-sig")
    _pin(src, "-LogonType S4U", "the task must run at the lock screen (Interactive tasks all slept through the 14.4h outage)")
    _pin(src, "MultipleInstances IgnoreNew", "overlapping fires must not stack")
    _pin(src, "-RunLevel Limited", "no elevation at runtime")


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
    print(f"ALARM FORWARDER POLICY: {len(tests)} tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
