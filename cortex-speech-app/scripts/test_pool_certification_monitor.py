#!/usr/bin/env python3
"""Static contract for the five-minute read-only pool certification hook.

The watchdog's process-control branches are drilled separately. This gate protects the properties
that matter specifically to certification: it runs only after liveness succeeds, requests the
read-only review-ready report, never turns a certification failure into a restart, and suppresses
the external success ping when the data plane is unhealthy.
"""

from pathlib import Path


SCRIPT = Path(__file__).resolve().parent / "ops" / "cortex-watchdog.ps1"


def main() -> int:
    text = SCRIPT.read_text(encoding="utf-8")
    required = {
        "five-minute certifier invocation": "certify --db $dbPath --require-review-ready",
        "read-only/no-lease contract": "does not\n    # fetch a queue, take a lease",
        "report publication": "pool-certification.json",
        "failure does not restart": "NOT trigger the destructive restart path",
        "dead-man success is gated": "$certHealthy -and (Test-Path $hcFile)",
        "dry run remains side-effect free": "if ($DryRun) { exit 0 }",
    }
    missing = [label for label, token in required.items() if token not in text]
    if missing:
        raise AssertionError(f"pool certification watchdog contract is missing: {missing}")
    alive = text.index("if ($alive)")
    dry_run = text.index("if ($DryRun) { exit 0 }", alive)
    certifier = text.index("certify --db $dbPath --require-review-ready", alive)
    if not dry_run < certifier:
        raise AssertionError("watchdog -DryRun reaches the certifier and is no longer side-effect free")
    print("POOL CERTIFICATION MONITOR: OK (read-only five-minute hook, alert-gated, no restart coupling)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
