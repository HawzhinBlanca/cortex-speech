#!/usr/bin/env python3
"""Policy + unit self-check for quantize_finetuned_onnx.py.

Dependency-free (no onnx/onnxruntime) so it always runs on the policy runner: the quantizer's imports are
local to its functions, so the module imports and `head_nodes_to_exclude` is unit-testable on plain
strings. Also structurally pins the honesty-critical guards (int8 weight type, head exclusion, the
warn-if-head-not-found, and the CER-parity NEXT step) so a future edit can't silently drop them.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import quantize_finetuned_onnx as q  # noqa: E402

SRC = (Path(__file__).resolve().parent / "quantize_finetuned_onnx.py").read_text(encoding="utf-8")


def test_head_nodes_excludes_lm_head_and_classifier() -> None:
    names = [
        "/m/wav2vec2/encoder/layers.0/attention/q_proj/MatMul",
        "/m/lm_head/MatMul",
        "/m/lm_head/Add",
        "encoder/classifier/MatMul",
    ]
    excluded = q.head_nodes_to_exclude(names)
    assert set(excluded) == {"/m/lm_head/MatMul", "/m/lm_head/Add", "encoder/classifier/MatMul"}, excluded


def test_head_nodes_keeps_encoder_and_attention_nodes() -> None:
    # Attention "head" projections must NOT be excluded — only the lm_head/classifier output projection is.
    names = [
        "/m/encoder/layers.5/feed_forward/intermediate/MatMul",
        "/m/encoder/layers.5/attention/out_proj/MatMul",
        "/m/encoder/pos_conv_embed/Conv",
    ]
    assert q.head_nodes_to_exclude(names) == [], "no encoder/attention node is a CTC head"


def test_head_nodes_match_is_case_insensitive_and_null_safe() -> None:
    assert q.head_nodes_to_exclude(["/M/LM_HEAD/MatMul"]) == ["/M/LM_HEAD/MatMul"]
    assert q.head_nodes_to_exclude([None, ""]) == []  # null/empty names never crash


def test_source_pins_int8_and_head_exclusion_guards() -> None:
    assert "QuantType.QInt8" in SRC, "must quantize to int8 (QInt8 weight type)"
    assert "nodes_to_exclude=excluded" in SRC, "must pass the CTC-head exclusion to quantize_dynamic"
    assert "WARNING" in SRC, "must warn loudly when the CTC head cannot be identified"
    assert "verify_onnx_export.py" in SRC, "must point the caller at the CER-parity correctness gate"


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
        print(f"\n{failures} quantize-policy test(s) failed", flush=True)
        return 1
    print("\nquantize-policy tests passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(_run())
