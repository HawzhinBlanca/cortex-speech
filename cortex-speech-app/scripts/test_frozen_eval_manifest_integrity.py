#!/usr/bin/env python3
"""Policy: the committed frozen FLEURS ckb eval manifest must have one row per DISTINCT clip.

The frozen manifest `docs/eval/fleurs_ckb_iq_frozen.rel.tsv` is the reference-only, portable record of
the eval set the pinned CER/WER numbers (MEASUREMENTS.md) were scored on. `scorecard_7b.py` and
`scorecard_stats.py` count EVERY row (N = len(rows), no dedup), so a manifest with duplicate rows
silently inflates N, duplication-weights micro-CER toward the repeated clips, and narrows the bootstrap
CI — a fabricated-precision hazard the one-law forbids.

The original builder named clips `clips/<id>.wav` from the FLEURS *sentence* id, which is SHARED across
multiple recordings; same-id rows carry the identical reference, so they collapsed to exact-duplicate
rows (and clobbered each other on disk). This regression fired at 922 rows / 348 unique. The generator
now disambiguates same-id clips; this policy keeps the committed artifact honest permanently.

Pure stdlib (no numpy/soundfile) so it always runs on every policy runner, unlike the generator's
numpy-gated unit test. Auto-discovered by run_python_policies.py (name starts with `test_`).
"""

from __future__ import annotations

import hashlib
from pathlib import Path

MANIFEST = Path(__file__).resolve().parent.parent / "docs" / "eval" / "fleurs_ckb_iq_frozen.rel.tsv"


def _rows() -> list[str]:
    text = MANIFEST.read_text(encoding="utf-8")
    return [ln for ln in text.splitlines() if "\t" in ln]


def test_manifest_exists_and_nonempty() -> None:
    assert MANIFEST.is_file(), f"frozen eval manifest missing: {MANIFEST}"
    assert _rows(), "frozen eval manifest has no data rows"


def test_every_row_has_a_unique_clip_path() -> None:
    paths = [ln.split("\t", 1)[0] for ln in _rows()]
    dupes = sorted({p for p in paths if paths.count(p) > 1})
    assert len(paths) == len(set(paths)), (
        f"{len(paths) - len(set(paths))} duplicate clip path(s) in the frozen manifest "
        f"(e.g. {dupes[:3]}) — same-sentence-id FLEURS recordings collapsed to one filename. "
        f"N would be inflated ({len(paths)} rows vs {len(set(paths))} distinct clips). "
        "Rebuild with the disambiguating generator, or dedup the committed manifest."
    )


def test_no_exact_duplicate_rows() -> None:
    rows = _rows()
    assert len(rows) == len(set(rows)), (
        f"{len(rows) - len(set(rows))} exact-duplicate row(s) in the frozen manifest — "
        "each would be scored (and weighted) more than once."
    )


def test_sha256_sidecar_matches() -> None:
    sidecar = MANIFEST.with_suffix(MANIFEST.suffix + ".sha256")
    assert sidecar.is_file(), f"missing sha256 sidecar: {sidecar}"
    recorded = sidecar.read_text(encoding="utf-8").split()[0]
    actual = hashlib.sha256(MANIFEST.read_bytes()).hexdigest()
    assert recorded == actual, (
        f"sha256 sidecar is stale: records {recorded[:12]}… but the manifest hashes to "
        f"{actual[:12]}…. Regenerate the sidecar after any manifest edit."
    )


def _run() -> int:
    failures = 0
    for name, fn in sorted(globals().items()):
        if not name.startswith("test_") or not callable(fn):
            continue
        try:
            fn()
        except AssertionError as exc:
            failures += 1
            print(f"FAIL {name}: {exc}", flush=True)
        else:
            print(f"ok   {name}", flush=True)
    if failures:
        print(f"\n{failures} frozen-manifest-integrity test(s) failed", flush=True)
        return 1
    print("\nfrozen-manifest-integrity tests passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(_run())
