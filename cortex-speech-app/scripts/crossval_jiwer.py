#!/usr/bin/env python3
"""External jiwer cross-validation of Cortex's Rust WER/CER (blueprint M1.1).

Reads scripts/crossval_vectors.json - this repo's ACTUAL normalized strings + WER/CER,
emitted by the `emit_crossval_vectors` test in src-tauri/src/wer.rs - and recomputes
WER/CER with the independent `jiwer` library on the SAME (Rust-normalized) input.

Because the input is already Rust-normalized, this isolates the metric MATH (edit distance
+ aggregation) from normalization, making it a genuine INDEPENDENT check rather than the
old self-referential fixture (whose "expected" values came from Rust itself).

Asserts |jiwer - rust| <= TOL for every case jiwer can compute. Empty-reference cases are
version-sensitive in jiwer (v3 raised, v4 returns 1.0) and are reported but not hard-failed.

Run:        python scripts/crossval_jiwer.py
Requires:   pip install jiwer
Regenerate vectors:
    CORTEX_EMIT_CROSSVAL=1 cargo test --manifest-path src-tauri/Cargo.toml --lib \
        emit_crossval_vectors -- --ignored --nocapture
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

TOL = 1e-6
JIWER_VERSION = "4.0.0"
VECTORS = Path(__file__).resolve().parent / "crossval_vectors.json"

# Windows consoles default to cp1252 and crash on Sorani output; force UTF-8.
try:
    sys.stdout.reconfigure(encoding="utf-8")
except (AttributeError, ValueError):
    pass


def main() -> int:
    try:
        import jiwer
    except ImportError:
        print("FAIL: jiwer not installed. Run: pip install jiwer", file=sys.stderr)
        return 2
    if not VECTORS.exists():
        print(f"FAIL: {VECTORS} missing - regenerate with the emit_crossval_vectors Rust test.",
              file=sys.stderr)
        return 2

    try:
        import importlib.metadata as _md
        ver = _md.version("jiwer")
    except Exception as error:
        print(f"FAIL: cannot prove jiwer distribution identity: {error}", file=sys.stderr)
        return 2
    if ver != JIWER_VERSION:
        print(
            f"FAIL: jiwer version drift: observed {ver}, expected exactly {JIWER_VERSION}",
            file=sys.stderr,
        )
        return 2

    rows = json.loads(VECTORS.read_text(encoding="utf-8"))
    print(f"jiwer {ver} | {len(rows)} vectors | tol={TOL:g}")
    print(f"  {'rust_wer':>9} {'jiwer_wer':>10} {'rust_cer':>9} {'jiwer_cer':>10}  status  (ref -> hyp)")

    failures = []
    noted = 0
    for r in rows:
        nr, nh = r["norm_reference"], r["norm_hypothesis"]
        rw, rc = r["rust_wer"], r["rust_cer"]
        label = f"{nr!r} -> {nh!r}"
        # Empty reference is mathematically degenerate (division by ref length 0). Rust
        # deliberately clamps WER/CER to 1.0; jiwer returns the raw insertion count. That is a
        # documented convention divergence, not a metric-math discrepancy -> note, don't fail.
        if nr.strip() == "":
            try:
                jwt = f"{float(jiwer.wer(nr, nh)):.4f}"
                jct = f"{float(jiwer.cer(nr, nh)):.4f}"
            except Exception as e:
                jwt = jct = type(e).__name__
            noted += 1
            print(f"  {rw:>9.4f} {jwt:>10} {rc:>9.4f} {jct:>10}  noted   {label}  [empty-ref: Rust clamps 1.0, jiwer raw]")
            continue
        jw = float(jiwer.wer(nr, nh))
        jc = float(jiwer.cer(nr, nh))
        ok = abs(jw - rw) <= TOL and abs(jc - rc) <= TOL
        if not ok:
            failures.append((label, rw, jw, rc, jc))
        print(f"  {rw:>9.4f} {jw:>10.4f} {rc:>9.4f} {jc:>10.4f}  {'OK' if ok else 'MISMATCH'}   {label}")

    print()
    computable = len(rows) - noted
    if failures:
        print(f"CROSSVAL FAILED: {len(failures)} case(s) where Rust disagrees with jiwer > {TOL:g}:")
        for label, rw, jw, rc, jc in failures:
            print(f"  {label}: wer rust={rw} jiwer={jw} | cer rust={rc} jiwer={jc}")
        return 1
    print(f"CROSSVAL PASSED: Rust WER/CER == jiwer within {TOL:g} on all {computable} computable "
          f"case(s) ({noted} empty-ref case(s) noted, not failed).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
