#!/usr/bin/env python3
"""Regression tests for the IAA annotation extractor (scripts/build_iaa_annotations.py).

`iaa-kappa-ceiling` (charter item 44) bounds every accuracy number this project may honestly quote.
Its blocker was never only "recruit two annotators": `agreement_kappa.py` reads a TSV of two rater
columns and nothing produced one, so computing kappa meant hand-assembling a file out of SQLite.

The extractor closes that. These tests pin the properties that decide whether the resulting kappa is
TRUSTWORTHY rather than merely printable — every one of them is a way to get a number that looks fine
and means nothing:

  * pooling three raters into two columns invents agreement nobody measured
  * scoring a handful of items yields a kappa whose CI spans the whole scale
  * emitting a file when no item was doubly annotated produces a kappa built from nothing

Runs on the policy runner: pure stdlib, temp DBs, no app and no network.
"""

from __future__ import annotations

import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
EXTRACTOR = SCRIPTS / "build_iaa_annotations.py"
KAPPA = SCRIPTS / "agreement_kappa.py"


def _make_db(path: Path, rows: list[tuple[str, str, str]]) -> None:
    con = sqlite3.connect(path)
    con.execute(
        "CREATE TABLE spot_checks (segment_id TEXT, reviewer TEXT, action TEXT, "
        "submitted_transcript TEXT, expected_transcript TEXT, noticed INTEGER, cer REAL, created_at TEXT)"
    )
    con.executemany(
        "INSERT INTO spot_checks (segment_id, reviewer, action) VALUES (?, ?, ?)", rows
    )
    con.commit()
    con.close()


def _run(db: Path, out: Path, *extra: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(EXTRACTOR), "--db", str(db), "--out", str(out), *extra],
        capture_output=True, text=True, encoding="utf-8", errors="replace",
    )


def test_two_raters_produce_a_tsv_that_kappa_can_score() -> None:
    """The end-to-end contract: extractor output must feed agreement_kappa.py and yield a real number."""
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        db, out = tmp / "db.sqlite", tmp / "iaa.tsv"
        rows = []
        for i in range(40):
            # 32 agreements, 8 disagreements — a deliberately imperfect, realistic set.
            a = "accept" if i % 2 == 0 else "edit"
            b = a if i % 5 else "reject"
            rows += [(f"seg-{i:03d}", "Bahar", a), (f"seg-{i:03d}", "Nasrin", b)]
        _make_db(db, rows)

        result = _run(db, out)
        assert result.returncode == 0, f"extractor failed: {result.stderr or result.stdout}"
        assert out.is_file(), "no TSV written"

        header, *body = out.read_text(encoding="utf-8").strip().splitlines()
        assert header.split("\t")[:2] == ["Bahar", "Nasrin"], f"header must name both raters: {header!r}"
        assert len(body) == 40, f"expected 40 doubly-annotated items, got {len(body)}"

        scored = subprocess.run(
            [sys.executable, str(KAPPA), str(out)],
            capture_output=True, text=True, encoding="utf-8", errors="replace",
        )
        assert scored.returncode == 0, f"agreement_kappa rejected our TSV: {scored.stderr}"
        assert "Cohen's kappa" in scored.stdout, scored.stdout
        assert "N=40 items" in scored.stdout, f"kappa scored the wrong item count: {scored.stdout}"


def test_a_third_rater_is_excluded_by_name_never_pooled() -> None:
    """Cohen's kappa is the 2-rater statistic. Folding a third into two columns invents agreement."""
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        db, out = tmp / "db.sqlite", tmp / "iaa.tsv"
        rows = []
        for i in range(35):
            rows += [(f"seg-{i:03d}", "Bahar", "accept"), (f"seg-{i:03d}", "Nasrin", "accept")]
        # A third reviewer with a much thinner overlap must not displace the real pair.
        rows += [("seg-000", "Zara", "reject"), ("seg-001", "Zara", "reject")]
        _make_db(db, rows)

        result = _run(db, out)
        assert result.returncode == 0, result.stderr
        assert "EXCLUDED" in result.stdout and "Zara" in result.stdout, (
            f"a third rater must be named as excluded, not silently pooled: {result.stdout}"
        )
        header = out.read_text(encoding="utf-8").splitlines()[0]
        assert "Zara" not in header, f"third rater leaked into the scored columns: {header!r}"
        assert header.split("\t")[:2] == ["Bahar", "Nasrin"]


def test_no_overlap_refuses_instead_of_writing_an_empty_file() -> None:
    """Two annotators who never met on an item have measured nothing about each other.

    `--min-items 1` deliberately disarms the OTHER refusal, so this exercises the no-overlap guard
    itself. Without it the min-items floor answers first and the test passes whether or not the
    no-overlap path works at all — which it did, until a mutation run showed the test surviving that
    guard's removal.
    """
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        db, out = tmp / "db.sqlite", tmp / "iaa.tsv"
        _make_db(db, [("seg-a", "Bahar", "accept"), ("seg-b", "Nasrin", "accept")])

        # --min-items 0 fully disarms the floor, so ONLY the zero-overlap guard can answer here.
        # With --min-items 1 the floor replied first and this test passed whether or not the
        # zero-overlap path worked — caught by a mutation run, not by reading it.
        result = _run(db, out, "--min-items", "0")
        assert result.returncode == 2, f"expected refusal (2), got {result.returncode}: {result.stdout}"
        assert not out.exists(), "refusing to measure must not leave a file behind"
        assert "nothing to measure" in (result.stderr + result.stdout), (
            f"the refusal must name the real reason (no overlap): {result.stderr or result.stdout}"
        )


def test_too_few_items_refuses_rather_than_reporting_a_meaningless_kappa() -> None:
    """A kappa over a handful of items has a CI spanning the scale; printing it would be false precision."""
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        db, out = tmp / "db.sqlite", tmp / "iaa.tsv"
        rows = []
        for i in range(4):
            rows += [(f"seg-{i}", "Bahar", "accept"), (f"seg-{i}", "Nasrin", "accept")]
        _make_db(db, rows)

        result = _run(db, out)
        assert result.returncode == 2, f"expected refusal (2), got {result.returncode}: {result.stdout}"
        assert not out.exists(), "a refused run must not leave a TSV"
        assert "--min-items" in (result.stderr + result.stdout), "the refusal must say how to override it"

        # Deliberately lowering the floor is allowed — the guard is against ACCIDENTAL false precision.
        forced = _run(db, out, "--min-items", "1")
        assert forced.returncode == 0, forced.stderr
        assert out.is_file()


def main() -> None:
    test_two_raters_produce_a_tsv_that_kappa_can_score()
    test_a_third_rater_is_excluded_by_name_never_pooled()
    test_no_overlap_refuses_instead_of_writing_an_empty_file()
    test_too_few_items_refuses_rather_than_reporting_a_meaningless_kappa()
    print("IAA annotation extractor policy passed")


if __name__ == "__main__":
    main()
