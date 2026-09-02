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

UNITTEST METHODS COUNT TOO (2026-08-25). This scan used to read only TOP-LEVEL `def test_`, so every
`unittest.TestCase` METHOD was invisible to it — ten policy files, among them the 32-test review
compensation gate and the 12-test serving-provenance gate, i.e. the money path and a canon gate.
Deleting their `unittest.main()` tail left them exiting 0 while asserting nothing, and this gate went
on printing a clean scan. A file that defines a TestCase must therefore also dispatch it from a
`__main__` guard, or its methods are reported dead by name.

A file that dispatches by discovery runs everything it defines by construction and is exempt — but
only when it really does: the exemption requires the RESULT of `globals()` to be inspected
(`globals().items()`, `globals()[name]`), not merely the name `globals` appearing somewhere in the
file, which used to exempt a file wholesale on an incidental mention.

Exit 0 = every defined policy test is reachable. Exit 1 = at least one asserts nothing.
"""

from __future__ import annotations

import ast
import sys
from pathlib import Path

SCRIPTS_DIR = Path(__file__).resolve().parent

# Constructing either one from a __main__ guard runs the TestCase methods the loader discovers.
UNITTEST_ENTRYPOINTS = {"main", "TextTestRunner"}


def _is_main_guard(node: ast.AST) -> bool:
    return (
        isinstance(node, ast.If)
        and isinstance(node.test, ast.Compare)
        and isinstance(node.test.left, ast.Name)
        and node.test.left.id == "__name__"
        and any(isinstance(c, ast.Constant) and c.value == "__main__" for c in node.test.comparators)
    )


def _dispatches_via_globals(tree: ast.AST) -> bool:
    """True only when the RESULT of `globals()` is inspected — the discovery-dispatch pattern."""
    for node in ast.walk(tree):
        if isinstance(node, (ast.Attribute, ast.Subscript)):
            value = node.value
            if isinstance(value, ast.Call) and isinstance(value.func, ast.Name) and value.func.id == "globals":
                return True
    return False


def _testcase_test_methods(tree: ast.AST) -> list[str]:
    """`Class.test_method` for every unittest.TestCase subclass defined in the file.

    Transitive: a shared local base class (`class _Base(unittest.TestCase)`) makes its subclasses
    TestCases too, and their methods are just as invisible to a top-level-only scan.
    """
    classes = {node.name: node for node in ast.walk(tree) if isinstance(node, ast.ClassDef)}
    testcases: set[str] = set()
    changed = True
    while changed:
        changed = False
        for name, node in classes.items():
            if name in testcases:
                continue
            for base in node.bases:
                base_name = base.attr if isinstance(base, ast.Attribute) else getattr(base, "id", "")
                if base_name.endswith("TestCase") or base_name in testcases:
                    testcases.add(name)
                    changed = True
                    break
    return sorted(
        f"{name}.{member.name}"
        for name in testcases
        for member in classes[name].body
        if isinstance(member, (ast.FunctionDef, ast.AsyncFunctionDef)) and member.name.startswith("test_")
    )


def _dispatches_unittest(tree: ast.AST) -> bool:
    """A `__main__` guard that reaches `unittest.main()` / `unittest.TextTestRunner()`.

    One hop through a locally defined function, because `if __name__ == "__main__": main()` with
    `def main(): unittest.main()` is the same dispatch written differently.
    """
    def runs_unittest(node: ast.AST) -> bool:
        for sub in ast.walk(node):
            if (
                isinstance(sub, ast.Call)
                and isinstance(sub.func, ast.Attribute)
                and isinstance(sub.func.value, ast.Name)
                and sub.func.value.id == "unittest"
                and sub.func.attr in UNITTEST_ENTRYPOINTS
            ):
                return True
        return False

    called: set[str] = set()
    for guard in (n for n in ast.walk(tree) if _is_main_guard(n)):
        if runs_unittest(guard):
            return True
        for sub in ast.walk(guard):
            if isinstance(sub, ast.Call) and isinstance(sub.func, ast.Name):
                called.add(sub.func.id)
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name in called:
            if runs_unittest(node):
                return True
    return False


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
    for node in ast.walk(tree):
        if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load):
            referenced.add(node.id)
        elif isinstance(node, ast.Attribute):
            referenced.add(node.attr)

    # A name is only "called" if it is referenced somewhere that is not its own definition; the walk
    # above never yields the def's own name as a Load, so a plain set difference is already correct.
    return defined, referenced, _dispatches_via_globals(tree)


def uncalled_tests(path: Path) -> list[str]:
    """Test functions this file defines but never reaches. Empty list means the file is honest."""
    try:
        source = path.read_text(encoding="utf-8")
    except OSError:
        return []
    try:
        tree = ast.parse(source)
    except SyntaxError:
        # py_compile in run_python_policies.py is the syntax gate; not this file's job to duplicate it.
        return []
    defined, referenced, uses_globals = defined_and_referenced(source)

    dead = [] if uses_globals else sorted(defined - referenced)

    methods = _testcase_test_methods(tree)
    if methods and not _dispatches_unittest(tree):
        dead.extend(methods)
    return dead


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


def test_a_bare_globals_mention_does_not_exempt_a_file():
    """The exemption used to fire on ANY reference, waiving dead-test detection for a third of them."""
    src = "def test_a():\n    pass\n\ndef helper(fn=globals):\n    return fn\n"
    _, _, uses_globals = defined_and_referenced(src)
    assert not uses_globals, "naming `globals` is not dispatching through it"


def test_unittest_methods_without_a_dispatch_are_reported_dead():
    """The money-gate hole: strip `unittest.main()` and 32 compensation tests assert nothing."""
    src = (
        "import unittest\n\n"
        "class Money(unittest.TestCase):\n"
        "    def test_rate(self):\n        pass\n"
        "    def test_reversal(self):\n        pass\n"
    )
    tree = ast.parse(src)
    assert _testcase_test_methods(tree) == ["Money.test_rate", "Money.test_reversal"]
    assert not _dispatches_unittest(tree), "no __main__ guard runs these methods"


def test_unittest_methods_with_a_main_guarded_dispatch_are_reachable():
    src = (
        "import unittest\n\n"
        "class Money(unittest.TestCase):\n"
        "    def test_rate(self):\n        pass\n\n"
        "if __name__ == '__main__':\n    unittest.main()\n"
    )
    assert _dispatches_unittest(ast.parse(src)), "a __main__-guarded unittest.main() runs the methods"


def test_a_local_base_class_does_not_hide_its_subclass_methods():
    src = (
        "import unittest\n\n"
        "class _Base(unittest.TestCase):\n    pass\n\n"
        "class Money(_Base):\n    def test_rate(self):\n        pass\n"
    )
    assert _testcase_test_methods(ast.parse(src)) == ["Money.test_rate"]


def test_a_main_function_that_runs_unittest_counts_as_dispatch():
    src = (
        "import unittest\n\n"
        "class Money(unittest.TestCase):\n    def test_rate(self):\n        pass\n\n"
        "def main():\n    unittest.main()\n\n"
        "if __name__ == '__main__':\n    main()\n"
    )
    assert _dispatches_unittest(ast.parse(src)), "one hop through a local main() is the same dispatch"


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
