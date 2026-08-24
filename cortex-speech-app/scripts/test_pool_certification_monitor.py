#!/usr/bin/env python3
"""Static contract for the five-minute read-only pool certification hook.

The watchdog's process-control branches are drilled separately. This gate protects the properties
that matter specifically to certification: it runs only after liveness succeeds, requests the
read-only review-ready report, never turns a certification failure into a restart, and suppresses
the external success ping when the data plane is unhealthy.
"""

from pathlib import Path


SCRIPT = Path(__file__).resolve().parent / "ops" / "cortex-watchdog.ps1"
ADMIN = Path(__file__).resolve().parents[1] / "src-tauri" / "src" / "bin" / "pool_admin.rs"


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
    admin = ADMIN.read_text(encoding="utf-8")
    required_admin = {
        "certify uses a WAL-consistent disposable copy for FTS5 validation": "Database::open_detached_read_snapshot",
        "ordinary reads use SQLite's source-enforced read-only boundary": "Database::open_read_only",
        "writers retain the guarded live opener": "Database::open_with_retry",
        "unknown commands fail before database access": "command_database_access(command)?",
    }
    missing_admin = [label for label, token in required_admin.items() if token not in admin]
    if missing_admin:
        raise AssertionError(f"pool_admin read-only database boundary is missing: {missing_admin}")
    classifier = admin[admin.index("const DETACHED_READ_COMMANDS") : admin.index("fn value_after")]
    direct_or_writers = classifier[classifier.index("const DIRECT_READ_COMMANDS") :]
    if '"certify"' not in classifier or '"certify"' in direct_or_writers:
        raise AssertionError("pool_admin does not classify certify exclusively as detached read-only")
    database = (ADMIN.parents[1] / "db.rs").read_text(encoding="utf-8")
    if "SQLITE_OPEN_READ_ONLY" not in database or "BEGIN DEFERRED" not in database:
        raise AssertionError("pool_admin's read path is not source-enforced and snapshot-consistent")
    print("POOL CERTIFICATION MONITOR: OK (read-only five-minute hook, alert-gated, no restart coupling)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
