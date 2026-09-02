#!/usr/bin/env python3
"""Launch policy tests only through the repository-owned pinned interpreter."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

from policy_python import PolicyEnvironmentError, locked_python, validate_locked_launcher


def main() -> int:
    try:
        identity = validate_locked_launcher()
    except PolicyEnvironmentError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 2
    print(
        "Locked Python policy launcher: "
        f"Python {identity.get('python')}, lock {str(identity.get('lockSha256', ''))[:12]}",
        flush=True,
    )
    runner = Path(__file__).resolve().with_name("run_python_policies.py")
    completed = subprocess.run(
        [str(locked_python()), str(runner)],
        cwd=runner.parents[1],
        check=False,
        shell=False,
    )
    return completed.returncode


if __name__ == "__main__":
    raise SystemExit(main())
