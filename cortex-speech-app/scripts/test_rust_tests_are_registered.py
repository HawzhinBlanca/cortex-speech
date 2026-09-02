"""A Rust test that is not registered is counted as passing while asserting nothing.

WHY THIS EXISTS: on 2026-08-30 an edit left TWO `#[test]` attributes stacked on one function. Rust
applies the first and treats the second as a duplicate, so the attribute that should have belonged to
the NEXT function was consumed. That next test -- a real one, covering the queue ordering this whole
release exists to change -- silently stopped running. `cargo test` reported a clean pass, and the
same test name simply appeared twice in the output.

The compiler does warn (`duplicate_macro_attributes`, then `dead_code`), but nothing in this repo
turns warnings into failures, so the only thing standing between that bug and production was somebody
reading scrollback.

This is the Rust twin of `test_all_policy_tests_execute.py`, which exists because the identical
failure mode -- a test file that runs zero assertions and reports success -- has already happened on
the Python side.

The check is deliberately narrow so it has no false positives: an attribute line that is followed by
another attribute line carrying the SAME attribute is always a mistake. A legitimate stack
(`#[test]` then `#[should_panic]`, or `#[tokio::test]` then `#[ignore]`) uses different attributes and
is left alone.

Run: python scripts/test_rust_tests_are_registered.py
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent / "src-tauri" / "src"

# Attributes that REGISTER a test. Stacking one of these twice always means a lost registration.
REGISTRARS = {"#[test]", "#[tokio::test]", "#[test_case]"}
ATTR = re.compile(r"^\s*(#\[[^\]]+\])\s*$")


def duplicated_registrars(path: Path) -> list[str]:
    hits: list[str] = []
    previous: str | None = None
    previous_line = 0
    for number, raw in enumerate(path.read_text(encoding="utf-8", errors="ignore").splitlines(), 1):
        match = ATTR.match(raw)
        if not match:
            previous = None
            continue
        attribute = match.group(1)
        if attribute == previous and attribute in REGISTRARS:
            hits.append(f"{path.name}:{previous_line}-{number}: {attribute} stacked on itself")
        previous, previous_line = attribute, number
    return hits


def test_no_registrar_is_stacked_on_itself() -> None:
    failures: list[str] = []
    scanned = 0
    for path in sorted(SRC.rglob("*.rs")):
        scanned += 1
        failures.extend(duplicated_registrars(path))
    if failures:
        listed = "\n".join(f"  - {failure}" for failure in failures)
        raise AssertionError(
            "A test-registering attribute is stacked on itself. Rust consumes the duplicate, so the "
            "NEXT function loses its own attribute and stops running while still reporting success:\n"
            + listed
        )
    print(f"RUST TEST REGISTRATION: OK - no stacked registrar across {scanned} source files")


def test_gate_actually_bites() -> None:
    """Anti-vacuity: the detector must fire on the exact shape that shipped."""
    import tempfile

    with tempfile.TemporaryDirectory() as directory:
        bad = Path(directory) / "bad.rs"
        bad.write_text("    #[test]\n    #[test]\n    fn a() {}\n", encoding="utf-8")
        if not duplicated_registrars(bad):
            raise AssertionError("the detector missed a stacked #[test] - this gate proves nothing")
        good = Path(directory) / "good.rs"
        good.write_text("    #[test]\n    #[should_panic]\n    fn a() {}\n", encoding="utf-8")
        if duplicated_registrars(good):
            raise AssertionError("a legitimate attribute stack was flagged - this gate blocks clean code")
    print("gate bites: a stacked #[test] is caught, #[test] + #[should_panic] is not")


if __name__ == "__main__":
    try:
        test_gate_actually_bites()
        test_no_registrar_is_stacked_on_itself()
    except AssertionError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        raise SystemExit(1)
