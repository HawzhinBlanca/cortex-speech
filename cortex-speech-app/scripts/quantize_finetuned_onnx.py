#!/usr/bin/env python3
"""Quantize the fine-tuned Wav2Vec2ForCTC ONNX export to int8 (dynamic) — in-tree + reproducible.

The app ships the fine-tuned ckb checkpoint as an int8 `model.int8.onnx`, but that int8 was produced
OUT OF BAND: `export_finetuned_onnx.py` emits fp32 only, and re-quantizing a newly-retrained champion had
no committed script (fragile; see memory `finetuned-mms-checkpoint-location`). This closes that gap.

It performs onnxruntime DYNAMIC int8 quantization but DELIBERATELY LEAVES THE CTC OUTPUT HEAD IN FP32:
quantizing the final classifier projection is the well-documented cause of a large WER/CER hit, whereas
int8-dynamic on the encoder matches fp32 accuracy closely. If it cannot identify the head to exclude it
WARNS LOUDLY rather than silently shipping a possibly-degraded model.

Correctness is NOT proven here. It is proven by re-running `scripts/verify_onnx_export.py` on the int8
output and confirming its micro-CER matches the fp32/transformers number. That CER-parity check is the
real gate; this script only produces the candidate.

Env: CORTEX_FINETUNED_ONNX (fp32 model.onnx IN) + optional CORTEX_FINETUNED_ONNX_INT8 (int8 OUT; defaults
to a `.int8.onnx` sibling).
Run:      python scripts/quantize_finetuned_onnx.py
Verify:   CORTEX_FINETUNED_ONNX=<int8 out> python scripts/verify_onnx_export.py <gold_manifest.tsv>
          (the int8 micro-CER MUST match the fp32 number, or the quantization is bad)
"""
from __future__ import annotations

import os
import sys

# The CTC classifier head is kept in fp32 (quantizing it materially hurts CER). wav2vec2/MMS name it
# `lm_head`; other CTC heads use `classifier`. Matched against the ONNX node-name path the legacy
# exporter emits (e.g. "/m/lm_head/MatMul"). Pure + dependency-free so it is unit-tested without
# onnxruntime installed (the policy runner has no scientific stack).
HEAD_MARKERS = ("lm_head", "classifier")


def head_nodes_to_exclude(node_names) -> list:
    """Return the subset of ONNX node names that belong to the CTC output head (to keep in fp32)."""
    return [n for n in node_names if any(m in (n or "").lower() for m in HEAD_MARKERS)]


def _quantize(in_path: str, out_path: str, exclude_head: bool = True) -> list:
    """Dynamic-int8 quantize in_path -> out_path, excluding CTC-head nodes. Returns excluded names."""
    import onnx  # local imports keep the module (and head_nodes_to_exclude) importable without the deps
    from onnxruntime.quantization import QuantType, quantize_dynamic

    excluded: list = []
    if exclude_head:
        model = onnx.load(in_path, load_external_data=False)
        excluded = head_nodes_to_exclude([node.name for node in model.graph.node])
        if not excluded:
            print(
                "WARNING: could not identify the CTC output head by name — it may be quantized. The "
                "verify_onnx_export.py CER-parity check is now the ONLY guard against a bad int8.",
                file=sys.stderr,
            )
        else:
            print(f"keeping {len(excluded)} CTC-head node(s) in fp32: {excluded}")
    quantize_dynamic(in_path, out_path, weight_type=QuantType.QInt8, nodes_to_exclude=excluded)
    return excluded


def main() -> int:
    src = os.environ.get("CORTEX_FINETUNED_ONNX", "")
    if not src:
        print("set CORTEX_FINETUNED_ONNX (fp32 model.onnx); optional CORTEX_FINETUNED_ONNX_INT8 (output)")
        return 2
    if not os.path.isfile(src):
        print(f"input not found: {src}", file=sys.stderr)
        return 2
    out = os.environ.get("CORTEX_FINETUNED_ONNX_INT8", "")
    if not out:
        base, ext = os.path.splitext(src)
        out = f"{base}.int8{ext}"
    try:
        import onnx  # noqa: F401
        from onnxruntime.quantization import quantize_dynamic  # noqa: F401
    except ImportError:
        print("error: `pip install onnx onnxruntime` to quantize", file=sys.stderr)
        return 2
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    _quantize(src, out, exclude_head=True)
    size = os.path.getsize(out) if os.path.isfile(out) else 0
    print(f"quantized -> {out} ({size} bytes)")
    print(f"NEXT (prove it): CORTEX_FINETUNED_ONNX={out} python scripts/verify_onnx_export.py <gold_manifest.tsv>")
    print("      the int8 micro-CER MUST match the fp32/transformers number, or the quantization is bad.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
