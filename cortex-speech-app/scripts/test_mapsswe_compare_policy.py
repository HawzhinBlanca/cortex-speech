"""Unit test for mapsswe_compare.mapsswe — a matched-pairs significance p-value must never be fabricated.

The MAPSSWE tool's stdout is pasted into the ledger as a REAL measured significance result (the honesty law).
A constant NON-zero per-clip error gap gives sample variance 0; the old code returned (mean, inf, 0.0),
printing "p = 0.000e+00 (SIGNIFICANT p<0.05)" — a fabricated maximally-significant verdict. The correct
fallback (the two-sided sign test, p = 2*0.5^n) says such a uniform gap over a handful of segments is NOT
significant. Mirrors the canonical Rust reference significance.rs:182-193 and its test
mapsswe_constant_nonzero_gap_is_not_falsely_significant. Fail-before: reverting mapsswe's var==0 branch to
`inf, 0.0` makes the constant-gap p 0.0 and these asserts fire.
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from mapsswe_compare import mapsswe  # noqa: E402


def test_mapsswe_significance() -> None:
    # Constant NON-zero gap -> two-sided sign test p = 2*0.5^n, NOT a fabricated 0.0.
    for n, expect_p in ((2, 0.5), (3, 0.25), (4, 0.125)):
        _mean, _z, p = mapsswe([1] * n)
        assert abs(p - expect_p) < 1e-12, f"n={n}: p={p}, expected sign-test {expect_p}"
        assert p >= 0.05, f"n={n}: a uniform gap over {n} segments must NOT be 'significant'"
    # Magnitude, not sign, drives the sign-test p (a negative constant gap behaves the same).
    assert abs(mapsswe([-2, -2, -2])[2] - 0.25) < 1e-12

    # All-zero diffs (tied) -> p = 1.0.
    assert mapsswe([0, 0, 0]) == (0.0, 0.0, 1.0)

    # n < 2 -> no ZeroDivisionError on /(n-1), non-significant.
    assert mapsswe([2])[2] == 1.0
    assert mapsswe([])[2] == 1.0

    # A real, non-degenerate consistent gap still yields a normal-approx p in (0, 0.05) — significant.
    _m, _z, p = mapsswe([1, 0, 1, 0, 1, 0, 1, 0])
    assert 0.0 < p < 0.05, f"a consistent gap over 8 segments should be significant, got p={p}"

    # A symmetric set around zero is not significant.
    assert mapsswe([1, -1, 1, -1])[2] > 0.05


def main() -> None:
    test_mapsswe_significance()
    print("mapsswe significance policy passed")


if __name__ == "__main__":
    main()
