#!/usr/bin/env python3
"""Run a test command and refuse to call it a pass if it executed NOTHING.

WHY. `cargo test` and `vitest` both exit 0 when they match zero tests. Measured 2026-08-03:

    cargo test --lib -- a_filter_that_matches_nothing
      test result: ok. 0 passed; 0 failed; 0 ignored; 1105 filtered out    exit 0
    npx vitest run -t a_filter_that_matches_nothing                        exit 0

So the two largest legs of verify-10 would have reported PASS if test discovery ever broke -- a stray
filter, a renamed module, a cfg that excludes the test tree, a vitest `include` that stops matching.
Green, with nothing behind it.

This is not a hypothetical class in this repo. It has been fixed twice already: `_fn_fuzz_smoke` fails
loud on an empty `cargo fuzz list` rather than sailing through its loop, and the `ignored-real-model`
leg once reported "21 passed" while twelve of those tests asserted nothing because
CORTEX_REAL_AUDIO_DIR was unset. The charter's word for this is a vacuous pass, and it forbids it.

WHAT IT DOES. Runs the command, streams nothing away (stdout and stderr are passed through verbatim so
the leg log is unchanged), then applies two checks:

  1. the command's own exit code must be 0 -- this never masks a real failure;
  2. the reported test count must be >= --min.

THE CHECKER'S OWN VACUITY IS THE SUBTLE PART. If the count pattern does not match at all -- output
format changed, a new harness version, a crash before the summary -- this FAILS rather than skipping the
check. A guard that quietly disables itself when it stops understanding the output is worse than no
guard, because it still looks like protection.

Usage:
    python scripts/assert_ran.py --min 900 --kind cargo  -- cargo test --lib
    python scripts/assert_ran.py --min 100 --kind vitest -- npm test
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

# Both harnesses print a machine-readable summary. Counting PASSED specifically (not "total") is
# deliberate: a run where everything was filtered out reports 0 passed, which is the case being caught.
PATTERNS = {
    # libtest: "test result: ok. 1105 passed; 0 failed; 7 ignored; ..."  (one line per test binary)
    "cargo": re.compile(r"test result:\s+\w+\.\s+(\d+)\s+passed", re.I),
    # vitest: "Tests  245 passed (245)" / "Tests  245 passed | 2 skipped (247)"
    "vitest": re.compile(r"Tests\s+(\d+)\s+passed", re.I),
}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--min", type=int, required=True, help="fail if fewer than this many tests passed")
    ap.add_argument("--kind", choices=sorted(PATTERNS), required=True)
    ap.add_argument("cmd", nargs=argparse.REMAINDER, help="-- followed by the command to run")
    args = ap.parse_args()

    cmd = args.cmd[1:] if args.cmd and args.cmd[0] == "--" else args.cmd
    if not cmd:
        print("assert_ran: no command given", file=sys.stderr)
        return 2

    executable = shutil.which(cmd[0])
    if executable is None:
        print(f"assert_ran: executable not found: {cmd[0]}", file=sys.stderr)
        return 127

    argv = [executable, *cmd[1:]]
    if sys.platform == "win32" and Path(executable).suffix.casefold() in {".bat", ".cmd"}:
        # CreateProcess cannot execute a batch shim directly. Invoke the OS command processor as an
        # explicit argv substep instead of asking Python for an implicit shell. This keeps the
        # verifier's process identity and process-tree containment observable while still supporting
        # npm.cmd on stock Windows installations.
        command_processor = os.environ.get("COMSPEC") or shutil.which("cmd.exe")
        if not command_processor:
            print("assert_ran: Windows command processor not found", file=sys.stderr)
            return 127
        # Use the original command token after `which` has proved exactly what it resolves to. A
        # fully qualified path beginning with a quote has surprising `/s /c` stripping semantics;
        # the PATH-resolved token avoids that ambiguity while still refusing an absent shim above.
        argv = [command_processor, "/d", "/s", "/c", subprocess.list2cmdline(cmd)]

    r = subprocess.run(argv, shell=False, capture_output=True, text=True, errors="replace")
    sys.stdout.write(r.stdout)
    sys.stderr.write(r.stderr)

    if r.returncode != 0:
        print(f"\nassert_ran: command failed (exit {r.returncode}) -- reporting that, not the count.")
        return r.returncode

    # Sum across binaries: `cargo test` prints one summary line per test target.
    counts = [int(m) for m in PATTERNS[args.kind].findall(r.stdout + r.stderr)]
    if not counts:
        print(
            f"\nassert_ran: FAIL - the command exited 0 but no '{args.kind}' test-count line was found.\n"
            f"  This check refuses to pass when it cannot READ the result. The output format may have\n"
            f"  changed, or the run died before printing its summary. Fix the pattern deliberately;\n"
            f"  a guard that silently disables itself is worse than no guard."
        )
        return 1

    total = sum(counts)
    if total < args.min:
        print(
            f"\nassert_ran: FAIL - only {total} test(s) ran, expected at least {args.min}.\n"
            f"  Exit code was 0, so this would have been reported as a PASS. Test discovery is broken\n"
            f"  (a stray filter, a renamed module, a cfg excluding the test tree), or the floor needs\n"
            f"  lowering DELIBERATELY because tests were removed on purpose."
        )
        return 1

    print(f"\nassert_ran: {total} test(s) ran (floor {args.min}) across {len(counts)} binary/binaries.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
