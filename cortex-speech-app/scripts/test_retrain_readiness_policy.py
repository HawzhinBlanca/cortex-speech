#!/usr/bin/env python3
"""Policy: the retrain-readiness report's eligibility SQL never drifts from stats.rs.

`scripts/retrain_readiness.py` answers the one question that spends GPU time — "is there enough new
reviewed material to be worth a fine-tune?" — and it answers it with a HAND-COPY of the eligibility
SQL in `src-tauri/src/stats.rs`. Its own docstring rests the whole safety argument on that: the Rust
rule is pinned against the REAL `export_dataset` output by
`the_dashboards_verified_count_equals_what_the_export_actually_writes`, so a faithful mirror inherits
that pinning and "does NOT invent a third opinion about what counts".

Nothing enforced the mirror. A copy with no gate is a claim, and this is the defect class the ledger
records SIX separate fixes for: a tally that counts rows the export drops. The failure is silent and
one-directional — stats.rs gains a rule (it gained the placeholder axis exactly this way), the Python
keeps the old one, and the owner is told they have more eligible material than the export would ever
publish. They then spend hours of GPU on it.

Compared modulo whitespace, because the Rust literal is indented inside a function and the Python one
is at module scope; every token that decides eligibility must still be identical. Same shape as
`test_couch_page_i18n.py`, which pins the phone's Sorani against the desktop strings it was copied
from, for the same reason.

Verified fail-before: change one token on either side and this fires.
"""

from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

APP = Path(__file__).resolve().parent.parent
STATS_RS = APP / "src-tauri" / "src" / "stats.rs"
MIRROR_PY = APP / "scripts" / "retrain_readiness.py"


def _norm(sql: str) -> str:
    """Whitespace-insensitive, everything else exact."""
    return re.sub(r"\s+", " ", sql).strip()


def _rust_const(src: str, name: str) -> str:
    m = re.search(r'const %s: &str =\s*"(.*?)";' % re.escape(name), src, re.DOTALL)
    assert m, (
        f"stats.rs no longer defines `const {name}: &str` as a single string literal, so this gate "
        f"cannot read the rule it exists to compare against. That is a FAILURE, not a pass: fix the "
        f"extraction here deliberately rather than letting the check quietly become vacuous."
    )
    return m.group(1)


def _rust_placeholder(src: str, effective: str) -> str:
    """The placeholder clause is a `format!` with {EFFECTIVE} interpolated — expand it the same way."""
    m = re.search(r'let placeholder = format!\(\s*"(.*?)"\s*\);', src, re.DOTALL)
    assert m, (
        "stats.rs no longer builds its placeholder clause with `let placeholder = format!(\"...\")`. "
        "This gate cannot verify a rule it cannot find — update the extraction, do not delete the check."
    )
    return m.group(1).replace("{EFFECTIVE}", effective)


def _load_mirror():
    spec = importlib.util.spec_from_file_location("retrain_readiness", MIRROR_PY)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def test_eligibility_sql_matches_stats_rs() -> None:
    src = STATS_RS.read_text(encoding="utf-8")
    mirror = _load_mirror()

    effective = _rust_const(src, "EFFECTIVE")
    expected = {
        "EFFECTIVE": effective,
        "REJECTED": _rust_const(src, "REJECTED"),
        "PLACEHOLDER": _rust_placeholder(src, effective),
    }

    drift = []
    for name, rust_sql in expected.items():
        actual = getattr(mirror, name, None)
        assert isinstance(actual, str), (
            f"retrain_readiness.py no longer defines `{name}` as a string. The report would then be "
            f"answering with some other rule entirely — which is exactly what this gate prevents."
        )
        if _norm(rust_sql) != _norm(actual):
            drift.append(f"  {name}:\n    stats.rs : {_norm(rust_sql)}\n    mirror   : {_norm(actual)}")

    assert not drift, (
        "retrain_readiness.py's eligibility SQL has DRIFTED from stats.rs.\n\n"
        + "\n".join(drift)
        + "\n\nThe report tells the owner how much material a retrain would have, and it is only "
        "trustworthy while it mirrors the rule that is pinned against the real export. Re-copy it "
        "from stats.rs. Do not 'fix' the numbers by editing the mirror to taste — that invents a "
        "third opinion about what counts, which is the tally-counts-rows-the-export-drops defect "
        "this repo has fixed six times."
    )


def test_the_mirror_still_declares_where_it_came_from() -> None:
    """A copied rule with no attribution is indistinguishable from an invented one to the next reader."""
    text = MIRROR_PY.read_text(encoding="utf-8")
    assert "stats.rs" in text, (
        "retrain_readiness.py no longer names stats.rs as the source of its eligibility SQL. The next "
        "person to read it has no way to know it is a mirror rather than an independent rule."
    )


def main() -> None:
    test_eligibility_sql_matches_stats_rs()
    test_the_mirror_still_declares_where_it_came_from()
    print("retrain-readiness eligibility mirror OK: EFFECTIVE, REJECTED and PLACEHOLDER match stats.rs")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        sys.exit(1)
