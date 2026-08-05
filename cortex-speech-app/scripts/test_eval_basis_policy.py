"""Source policies for the eval-basis honesty fixes (deep-audit H1 + H5, 2026-08-05).

H1: three engines in the pinned scorecard were compared as though identically normalized. They were
not — the stock-300M row came from the Rust harness, whose `wer::normalize_for_metrics` also folds
hamza, strips diacritics and normalizes numbers, while the Python scorecards do NFC+lower+whitespace
only. The fix is structural, not documentary: every scorecard stamps the basis into its artifacts and
`mapsswe_compare.py` refuses to pair two runs whose bases differ, so an un-interpretable cross-basis
p-value can no longer be produced.

H5: `MEASUREMENTS.md` called the gold set "known-disjoint" while `FINAL_READINESS_10.md` §M1.1
requires a *permanent, never-hidden* caveat that training-set overlap with the 7B LoRA is
UNVERIFIABLE (its manifest is on an offline drive). The claim asserted exactly what the plan says
cannot be established.

Each check below was fail-before verified: removing the guard fires the assertion.
"""

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]


def _read(rel: str) -> str:
    return (REPO_ROOT / rel).read_text(encoding="utf-8")


def test_every_scorecard_stamps_its_normalization_basis() -> None:
    """A CER number that cannot be traced to its normalizer is not re-checkable.

    `scorecard_7b.py` can switch basis from an environment variable (`CORTEX_CER_STRIP=1`), so two
    runs of the SAME script produce artifacts that differ by more than a CER point while being
    byte-indistinguishable without the stamp.
    """
    for rel in ("scripts/scorecard_7b.py", "scripts/scorecard_seamless.py"):
        src = _read(rel)
        if "NORM_BASIS" not in src:
            raise AssertionError(f"{rel} no longer defines NORM_BASIS — its numbers become untraceable")
        if "norm_basis" not in src:
            raise AssertionError(f"{rel} no longer writes a norm_basis column into its per-clip TSV")

    seven_b = _read("scripts/scorecard_7b.py")
    if '"norm_basis": NORM_BASIS' not in seven_b:
        raise AssertionError("scorecard_7b.py no longer records norm_basis in its JSON summary")
    # The switch and the stamp must stay wired to the SAME condition, or the stamp lies.
    if "_NORM is norm_fair" not in seven_b:
        raise AssertionError(
            "scorecard_7b.py's NORM_BASIS is no longer derived from the active normalizer, so the "
            "stamp can disagree with the basis actually used"
        )


def test_mapsswe_refuses_a_cross_basis_comparison() -> None:
    """A matched-pairs p-value across two normalizations measures the normalizers too.

    This is not hypothetical: the pinned champion-vs-stock comparison (z=-26.26, p=5.8e-152) paired a
    Python NFC+lower scorecard against the Rust `normalize_for_metrics`. The refusal makes that
    un-producible rather than merely documented.
    """
    # RUN IT, don't grep it. The first version of this check asserted the string "CROSS-BASIS REFUSAL"
    # was present — which also appears in the explanatory COMMENT above the branch, so deleting the
    # actual refusal left the gate green. Its own fail-before caught that. Behaviour is the only
    # thing worth pinning here.
    import subprocess
    import sys
    import tempfile

    rows = "1\t1\t10\t1\t5\t{b}\n2\t2\t10\t1\t5\t{b}\n3\t1\t10\t2\t5\t{b}\n"
    header = "clip_index\tchar_dist\tchar_ref_len\tword_dist\tword_ref_len\tnorm_basis\n"
    with tempfile.TemporaryDirectory() as td:
        strict = Path(td) / "a.tsv"
        fair = Path(td) / "b.tsv"
        legacy = Path(td) / "legacy.tsv"
        strict.write_text(header + rows.format(b="nfc_lower_space_kept"), encoding="utf-8")
        fair.write_text(
            header + rows.format(b="nfc_lower_space_kept_punct_digits_stripped"), encoding="utf-8"
        )
        legacy.write_text(
            "clip_index\tchar_dist\tchar_ref_len\tword_dist\tword_ref_len\n1\t1\t10\t1\t5\n2\t2\t10\t1\t5\n",
            encoding="utf-8",
        )
        script = str(REPO_ROOT / "scripts" / "mapsswe_compare.py")

        def run(x, y):
            return subprocess.run(
                [sys.executable, script, str(x), str(y)], capture_output=True, text=True
            )

        mixed = run(strict, fair)
        if mixed.returncode == 0:
            raise AssertionError(
                "mapsswe_compare.py ACCEPTED two runs scored on different normalization bases and "
                f"printed a p-value that cannot be interpreted. stdout: {mixed.stdout[:300]!r}"
            )

        same = run(strict, strict)
        if same.returncode != 0:
            raise AssertionError(
                f"mapsswe_compare.py refused a SAME-basis comparison — the guard is over-broad. "
                f"stdout: {same.stdout[:300]!r} stderr: {same.stderr[:200]!r}"
            )

        # A legacy TSV cannot prove basis equality; silence would readmit the exact defect.
        unknown = run(legacy, legacy)
        if "basis unknown" not in (unknown.stdout + unknown.stderr).lower():
            raise AssertionError(
                "comparing legacy TSVs (no norm_basis column) did not warn that basis equality is "
                f"unverified. stdout: {unknown.stdout[:300]!r}"
            )


def test_measurements_does_not_claim_identical_normalization() -> None:
    """The three-engine table must not re-assert what the harnesses contradict."""
    src = _read("docs/MEASUREMENTS.md")
    if "identical\nNFC+lower+whitespace (space-KEPT) normalization for all engines" in src:
        raise AssertionError(
            "MEASUREMENTS.md again claims identical normalization for all engines; the stock-300M row "
            "is scored through the Rust normalize_for_metrics (hamza + diacritics + numbers)"
        )
    if "Normalization correction" not in src:
        raise AssertionError("the normalization correction has been removed from MEASUREMENTS.md")
    if "not interpretable as stated" not in src:
        raise AssertionError(
            "MEASUREMENTS.md no longer flags the cross-basis champion-vs-stock p-value; an unsound "
            "significance test would read as sound"
        )


def test_unverifiable_overlap_caveat_is_present_and_disjointness_is_not_claimed() -> None:
    """H5: FINAL_READINESS_10.md §M1.1 requires this caveat to be permanent and never hidden."""
    src = _read("docs/MEASUREMENTS.md")
    # ASSERTION vs CITATION. A bare `known-disjoint` states the claim; a QUOTED `"known-disjoint"` is
    # the correction citing the phrase it retired, which must stay readable or the fix loses its
    # explanation. Banning the substring outright forbids describing the defect — the first version of
    # this guard did exactly that and failed on its own correction text.
    bare = [
        line.strip()
        for line in src.splitlines()
        if "known-disjoint" in line and '"known-disjoint"' not in line
    ]
    if bare:
        raise AssertionError(
            "MEASUREMENTS.md asserts 'known-disjoint' (unquoted), which claims exactly what the plan "
            f"says cannot be established while the LoRA training manifest is offline: {bare[:2]}"
        )
    if "UNVERIFIABLE" not in src:
        raise AssertionError("the unverifiable-overlap caveat is missing from MEASUREMENTS.md")

    plan = _read("docs/FINAL_READINESS_10.md")
    if "unverifiable" not in plan.lower():
        raise AssertionError(
            "FINAL_READINESS_10.md no longer states the overlap caveat this policy enforces — this "
            "gate would be pinning a requirement that no longer exists"
        )


def main() -> None:
    test_every_scorecard_stamps_its_normalization_basis()
    test_mapsswe_refuses_a_cross_basis_comparison()
    test_measurements_does_not_claim_identical_normalization()
    test_unverifiable_overlap_caveat_is_present_and_disjointness_is_not_claimed()
    print("eval-basis policy passed")


if __name__ == "__main__":
    main()
