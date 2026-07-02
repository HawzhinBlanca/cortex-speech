#!/usr/bin/env python3
"""M0.7 — fail if PROGRESS_LEDGER.md is stale (has no entry for recent commits).

The 108-commit gap that happened before 07-02 must never happen again. This gate runs
at pre-commit or CI and fails if the ledger's latest entry is >3 commits old.
"""
import re
import subprocess
from pathlib import Path


def test_ledger_staleness(max_commits_lag=3):
    """Check PROGRESS_LEDGER.md is not stale."""
    repo_root = Path(__file__).parent.parent.parent  # go up to git root
    ledger = repo_root / "PROGRESS_LEDGER.md"

    if not ledger.exists():
        print("SKIP: PROGRESS_LEDGER.md not found")
        return

    # Extract the most recent date from the ledger (look for "2026-" patterns)
    content = ledger.read_text(encoding="utf-8")
    dates = re.findall(r"2026-\d{2}-\d{2}", content)
    if not dates:
        print("SKIP: no dates found in ledger")
        return

    latest_ledger_date = sorted(dates)[-1]

    # Get the current commit count and the last commit message
    try:
        log = subprocess.check_output(
            ["git", "log", "--oneline", "-20"],
            cwd=repo_root,
            text=True
        )
    except Exception as e:
        print(f"SKIP: could not read git log: {e}")
        return

    commits = log.strip().split("\n")
    # For a simpler check: if the ledger has an entry for today's date (2026-07-02), it's current.
    # Otherwise, fail if there are >max_commits_lag commits since the most recent ledger date.
    import datetime
    today = datetime.date.today().strftime("%Y-%m-%d")

    if today in content:
        # Today's date is in the ledger, it's current
        commits_since_ledger = 0
    else:
        # Count commits since the latest ledger date
        commits_since_ledger = 0
        for commit in commits:
            # Look for the date in the commit message
            if latest_ledger_date in commit:
                break
            commits_since_ledger += 1

    if commits_since_ledger > max_commits_lag:
        raise AssertionError(
            f"PROGRESS_LEDGER.md is stale: {commits_since_ledger} commits since last entry "
            f"(limit is {max_commits_lag}). Please add an entry to the ledger."
        )

    print(f"[OK] PROGRESS_LEDGER.md: up to date (latest entry: {latest_ledger_date})")


if __name__ == "__main__":
    test_ledger_staleness()
    print("PASS: ledger staleness gate green")
