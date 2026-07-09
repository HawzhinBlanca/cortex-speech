#!/usr/bin/env python3
"""M0.7 — fail if PROGRESS_LEDGER.md is stale (has no entry for recent commits).

The 108-commit gap that happened before 07-02 must never happen again. This gate runs
at pre-commit or CI and fails if the ledger's latest entry is >3 commits old.

Implementation note: this measures the gate's stated intent DIRECTLY — the number of commits
since PROGRESS_LEDGER.md itself was last updated in git. An earlier version keyed off today's
CALENDAR date appearing in the ledger, which false-fired at every midnight rollover (a day with no
work still demanded a "date-bump" entry, polluting the ledger with non-work) and, conversely, could
pass while many code commits piled up as long as today's date happened to appear in some old entry.
Counting commits-since-ledger-update is both false-positive-free at rollover AND strictly stricter
where it matters (a burst of code commits with no ledger entry reds regardless of the calendar date).
"""
import os
import subprocess
from pathlib import Path


def test_ledger_staleness(max_commits_lag=3):
    """Fail if PROGRESS_LEDGER.md is >max_commits_lag commits behind HEAD."""
    repo_root = Path(__file__).parent.parent.parent  # go up to git root
    ledger = repo_root / "PROGRESS_LEDGER.md"

    if not ledger.exists():
        print("SKIP: PROGRESS_LEDGER.md not found")
        return

    def git(*args):
        return subprocess.check_output(["git", *args], cwd=repo_root, text=True).strip()

    try:
        # An uncommitted ledger edit (staged or unstaged) means an entry is being added right now —
        # treat as current so the gate never blocks the very commit that updates the ledger.
        if git("status", "--porcelain", "--", "PROGRESS_LEDGER.md"):
            print("[OK] PROGRESS_LEDGER.md: pending edit in working tree (entry being added)")
            return
        last_ledger_commit = git("log", "-1", "--format=%H", "--", "PROGRESS_LEDGER.md")
        if not last_ledger_commit:
            print("SKIP: PROGRESS_LEDGER.md not yet committed")
            return
        commits_since = int(git("rev-list", "--count", f"{last_ledger_commit}..HEAD") or "0")
    except Exception as e:  # noqa: BLE001
        # In CI a git failure must be a HARD error: a silent SKIP inside an overall-green policy run
        # converts the anti-drift gate into a pass exactly where nobody is watching (true-10 audit
        # 2026-07-09). Locally, keep the ergonomic skip but print it in ::warning form so it is
        # visible in gate output rather than scrolling by as an OK-looking line.
        if os.environ.get("GITHUB_ACTIONS"):
            raise AssertionError(f"ledger-staleness gate could not read git history in CI: {e}") from e
        print(f"::warning title=Ledger gate skipped::could not read git history locally: {e}")
        return

    if commits_since > max_commits_lag:
        raise AssertionError(
            f"PROGRESS_LEDGER.md is stale: {commits_since} commits since it was last updated "
            f"(limit is {max_commits_lag}). Please add an entry to the ledger."
        )

    print(f"[OK] PROGRESS_LEDGER.md: up to date ({commits_since} commit(s) since last update)")


if __name__ == "__main__":
    test_ledger_staleness()
    print("PASS: ledger staleness gate green")
