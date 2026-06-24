#!/usr/bin/env python3
"""Verify an ONNX export of the fine-tuned Wav2Vec2ForCTC matches the transformers model.

Runs the exported ONNX via onnxruntime on a gold manifest (feature extraction via the HF processor,
greedy CTC decode via the processor's tokenizer) and reports micro CER — which must match the
transformers-measured number (see scripts/measure_finetuned_cer.py) to prove the export is correct.

Env: CORTEX_FINETUNED_MODEL (HF dir, for the processor) + CORTEX_FINETUNED_ONNX (path to model.onnx).
Usage: python scripts/verify_onnx_export.py <gold_manifest.tsv>
"""
import os
import sys
import unicodedata

import jiwer
import numpy as np
import onnxruntime as ort
import soundfile as sf
from transformers import AutoProcessor

MODEL = os.environ.get("CORTEX_FINETUNED_MODEL", "")
ONNX = os.environ.get("CORTEX_FINETUNED_ONNX", "")


def norm(s: str) -> str:
    return " ".join(unicodedata.normalize("NFC", s).strip().lower().split())


def main() -> int:
    if not MODEL or not ONNX:
        print("set CORTEX_FINETUNED_MODEL (HF dir) and CORTEX_FINETUNED_ONNX (model.onnx)")
        return 2
    sys.stdout.reconfigure(encoding="utf-8")
    manifest = sys.argv[1]
    processor = AutoProcessor.from_pretrained(MODEL)
    sess = ort.InferenceSession(ONNX, providers=["CPUExecutionProvider"])
    iname = sess.get_inputs()[0].name

    rows = [l.rstrip("\n").split("\t") for l in open(manifest, encoding="utf-8") if "\t" in l]
    total_dist = 0
    total_ref = 0
    for i, (wav, ref) in enumerate(rows):
        audio, sr = sf.read(wav)
        if audio.ndim > 1:
            audio = audio.mean(axis=1)
        inputs = processor(audio, sampling_rate=16000, return_tensors="np")
        iv = inputs["input_values"].astype(np.float32)
        logits = sess.run(None, {iname: iv})[0]
        pred_ids = logits.argmax(axis=-1)
        hyp = processor.batch_decode(pred_ids)[0]
        r, h = norm(ref), norm(hyp)
        o = jiwer.process_characters(r if r else " ", h if h else "")
        total_dist += o.substitutions + o.deletions + o.insertions
        total_ref += len(r) if r else 1
        if (i + 1) % 10 == 0:
            print(f"  ...{i+1}/{len(rows)}")
    cer = total_dist / max(total_ref, 1)
    print(f"\nONNX-exported MMS-CTC-1B micro CER = {cer*100:.2f}%   (N={len(rows)})")
    print("(must match the transformers-measured 19.77% to confirm a correct export)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
