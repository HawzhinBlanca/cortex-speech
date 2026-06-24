#!/usr/bin/env python3
"""Export a fine-tuned HF Wav2Vec2ForCTC model to ONNX so the app's `ort` path can load it.

Uses the legacy TorchScript exporter (dynamo=False, no onnxscript dep). For >2 GB models ONNX
external-data is used automatically (model.onnx graph + sibling weight files). Verify the export
with scripts/verify_onnx_export.py (its CER must match scripts/measure_finetuned_cer.py).

Env: CORTEX_FINETUNED_MODEL (HF export dir) + CORTEX_FINETUNED_ONNX (output model.onnx path).
"""
import os
import sys

import torch
from transformers import AutoModelForCTC


def main() -> int:
    model_dir = os.environ.get("CORTEX_FINETUNED_MODEL", "")
    out = os.environ.get("CORTEX_FINETUNED_ONNX", "")
    if not model_dir or not out:
        print("set CORTEX_FINETUNED_MODEL (HF dir) and CORTEX_FINETUNED_ONNX (output model.onnx)")
        return 2
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    # bf16 weights -> float32 so CPU export/inference matches the float32 audio input.
    model = AutoModelForCTC.from_pretrained(model_dir).float().eval()

    class Wrap(torch.nn.Module):
        def __init__(self, m):
            super().__init__()
            self.m = m

        def forward(self, input_values):
            return self.m(input_values).logits

    dummy = torch.randn(1, 16000)
    torch.onnx.export(
        Wrap(model),
        (dummy,),
        out,
        input_names=["input_values"],
        output_names=["logits"],
        dynamic_axes={"input_values": {1: "samples"}, "logits": {1: "frames"}},
        opset_version=14,
        do_constant_folding=True,
        dynamo=False,
    )
    print(f"exported -> {out} ({os.path.getsize(out)} bytes graph; weights in external-data siblings)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
