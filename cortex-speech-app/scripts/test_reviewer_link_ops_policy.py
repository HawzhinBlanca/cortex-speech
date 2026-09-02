#!/usr/bin/env python3
"""Policy pins for the reviewer-link operations tooling (continuity gate, vault, health probe).

These three scripts guard the only copy of the reviewer pairing credentials. Each pin below is an
invariant whose silent loss re-opens a measured incident:

  - a printed token is a leaked credential (the token IS the link IS the identity);
  - a restore under a live server pretends to fix links the running server will not honour;
  - an empty reviewers map treated as a backup would "restore" the outage it was meant to undo;
  - a second reviver in the probe races the watchdog and fights a headless importer's lock
    (built and reverted 2026-08-28 — the probe detects, the watchdog heals).

Grep pins run everywhere; the executable selftests run only where DPAPI exists (Windows).
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

SCRIPTS = pathlib.Path(__file__).resolve().parent
CONTINUITY = SCRIPTS / "check_reviewer_link_continuity.py"
VAULT = SCRIPTS / "reviewer_link_vault.py"
PROBE = SCRIPTS / "ops" / "review-health-probe.ps1"

CHECKS: list[tuple[pathlib.Path, str, str]] = [
    # continuity: identity is compared by full digest and released immediately — never the token.
    (CONTINUITY, "hashlib.sha256", "continuity must compare token fingerprints, not tokens"),
    (CONTINUITY, "del token", "continuity must drop each decrypted token as soon as it is hashed"),
    (CONTINUITY, "casefold", "continuity must match reviewer names the way same_reviewer does"),
    # vault: refuses garbage in, proves restorability out, undoes Stop's revocation marker.
    (VAULT, "refusing to treat it as a credential set", "vault must never snapshot an empty reviewers map"),
    (VAULT, "couch_is_serving() and not force_live", "restore must refuse while the server is serving"),
    (VAULT, "couch_session.revoked", "restore must undo Stop's revocation marker"),
    (VAULT, "del token", "vault must drop each decrypted token as soon as it is hashed"),
    # probe: four gates, alarm on red, and NO reviver — the watchdog is the one healer.
    (PROBE, "check_reviewer_links_live.py", "probe must walk the public funnel path with real credentials"),
    (PROBE, "check_reviewer_queues_live.py", "probe must prove every reviewer has servable work"),
    (PROBE, "check_reviewer_link_continuity.py", "probe must catch a reminted token within one cycle"),
    (PROBE, "reviewer_link_vault.py", "probe must snapshot the credentials it watches"),
]

# Absence pins: a matching line is the regression.
FORBIDDEN: list[tuple[pathlib.Path, str, str]] = [
    # NOT a blanket Start-Process ban: Invoke-Gate legitimately launches the Python gates that way
    # (first draft of this pin banned Start-Process outright and redded the healthy probe). What a
    # reviver — and only a reviver — must read is the release record's appExe field to know what to
    # launch. Its presence is the regression.
    (
        PROBE,
        "appExe",
        "the probe must never launch the app: a second reviver races the 5-minute watchdog and, "
        "being lock-blind, fights a headless batch import (built and reverted 2026-08-28)",
    ),
]


def run_pins() -> list[str]:
    failures: list[str] = []
    for path, needle, why in CHECKS:
        if not path.is_file():
            failures.append(f"{path.name}: MISSING — {why}")
            continue
        if needle not in path.read_text(encoding="utf-8"):
            failures.append(f"{path.name}: pin '{needle}' gone — {why}")
    for path, needle, why in FORBIDDEN:
        if path.is_file() and needle in path.read_text(encoding="utf-8"):
            failures.append(f"{path.name}: forbidden '{needle}' present — {why}")
    return failures


def run_selftests() -> list[str]:
    """The scripts' own executable proofs — only where DPAPI exists."""
    if sys.platform != "win32":
        print("  (selftests skipped: DPAPI is Windows-only; grep pins above still enforced)")
        return []
    failures = []
    for script in (CONTINUITY, VAULT):
        proc = subprocess.run(
            [sys.executable, str(script), "--selftest"], capture_output=True, text=True, timeout=120
        )
        if proc.returncode != 0 or "SELFTEST OK" not in proc.stdout:
            failures.append(f"{script.name} --selftest failed (exit {proc.returncode}): {proc.stdout[-300:]}")
    return failures


def main() -> int:
    failures = run_pins() + run_selftests()
    if failures:
        for f in failures:
            print(f"FAIL: {f}")
        return 1
    print(f"reviewer-link ops policy OK ({len(CHECKS)} pins, {len(FORBIDDEN)} absence pins, selftests run)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
