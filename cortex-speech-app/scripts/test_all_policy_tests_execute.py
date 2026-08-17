#!/usr/bin/env python3
"""The policy that makes a dead policy test impossible.

`run_python_policies.py` runs every `scripts/test_*.py` and counts a zero exit as a pass. A test
function that is DEFINED but never CALLED therefore reports PASS while asserting nothing — the suite
prints a healthy "N policy test scripts passed" and the regression it was written for is unguarded.

This is not hypothetical and not a one-off. Found live on 2026-08-15, both guarding real incidents:

  * test_premium_dataset_policy.py   — test_alignment_only_review_risk_is_tolerated_but_audio_risk_is_not
  * test_rust_runtime_panic_policy.py — test_file_dialog_commands_do_not_block_the_main_thread
                                        (the 2026-07-11 main-thread freeze regression gate)

Adding the two missing calls fixes today. This file fixes tomorrow: any future `def test_` that
nobody invokes fails the suite immediately, naming the file and the function.

Detection is AST-based, not textual, because `def test_x():` contains the substring `test_x()` and a
grep for calls therefore counts every definition as its own caller — which is exactly how a hand
check missed these two earlier the same day.

A file that dispatches by discovery (iterating `globals()`) runs everything it defines by
construction and is exempt; it is the pattern this file recommends.

Exit 0 = every defined policy test is reachable. Exit 1 = at least one asserts nothing.
"""

from __future__ import annotations

import ast
import sys
from pathlib import Path

SCRIPTS_DIR = Path(__file__).resolve().parent


def defined_and_referenced(source: str) -> tuple[set[str], set[str], bool]:
    """(test functions defined, every name referenced, does it dispatch via globals())."""
    tree = ast.parse(source)

    defined = {
        node.name
        for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name.startswith("test_")
    }

    # Every Name load anywhere in the file — NOT only ast.Call. A file may dispatch through a list of
    # function objects (`for t in [test_a, test_b]: t()`), where the reference carries no parentheses.
    referenced: set[str] = set()
    uses_globals = False
    for node in ast.walk(tree):
        if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load):
            referenced.add(node.id)
            if node.id == "globals":
                uses_globals = True
        elif isinstance(node, ast.Attribute):
            referenced.add(node.attr)

    # A name is only "called" if it is referenced somewhere that is not its own definition; the walk
    # above never yields the def's own name as a Load, so a plain set difference is already correct.
    return defined, referenced, uses_globals


def uncalled_tests(path: Path) -> list[str]:
    """Test functions this file defines but never reaches. Empty list means the file is honest."""
    try:
        source = path.read_text(encoding="utf-8")
    except OSError:
        return []
    try:
        defined, referenced, uses_globals = defined_and_referenced(source)
    except SyntaxError:
        # py_compile in run_python_policies.py is the syntax gate; not this file's job to duplicate it.
        return []
    if uses_globals:
        return []
    return sorted(defined - referenced)


def test_this_policy_detects_a_defined_but_uncalled_function():
    src = "def test_a():\n    pass\n\ndef test_b():\n    pass\n\nif __name__ == '__main__':\n    test_a()\n"
    defined, referenced, _ = defined_and_referenced(src)
    assert sorted(defined - referenced) == ["test_b"], "the detector must name the uncalled function"


def test_a_definition_is_not_mistaken_for_its_own_caller():
    """The exact flaw that let both dead tests survive a manual grep check."""
    src = "def test_only(): \n    pass\n"
    defined, referenced, _ = defined_and_referenced(src)
    assert sorted(defined - referenced) == ["test_only"], "`def test_x():` must not count as a call to test_x"


def test_dispatch_through_a_list_of_function_objects_counts_as_called():
    src = "def test_a():\n    pass\n\nif __name__ == '__main__':\n    for t in [test_a]:\n        t()\n"
    defined, referenced, _ = defined_and_referenced(src)
    assert not (defined - referenced), "a reference without parentheses is still a call site"


def test_globals_discovery_files_are_exempt():
    src = "def test_a():\n    pass\n\nfor k, v in globals().items():\n    pass\n"
    _, _, uses_globals = defined_and_referenced(src)
    assert uses_globals, "a file that iterates globals() runs everything it defines"


def test_every_policy_test_in_this_directory_actually_executes():
    """The real gate: scan the suite the sweep depends on."""
    dead: list[str] = []
    scanned = 0
    for path in sorted(SCRIPTS_DIR.glob("test_*.py")):
        scanned += 1
        for name in uncalled_tests(path):
            dead.append(f"{path.name}::{name}")

    # A floor, for the same reason every other gate here has one: if the glob ever stops matching,
    # "no dead tests found" would be a vacuous pass rather than a real result.
    assert scanned >= 20, f"only {scanned} policy test files discovered — the glob is broken, not the suite clean"

    assert not dead, (
        "policy test functions that are DEFINED but never CALLED — they report PASS while asserting "
        "nothing:\n  " + "\n  ".join(dead)
    )
    print(f"  ({scanned} policy test files scanned, every defined test executes)")


if __name__ == "__main__":
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for t in tests:
        t()
        print(f"  ok  {t.__name__}")
    print(f"PASS: every policy test executes ({len(tests)} assertions)")
