#!/usr/bin/env python3
"""Publishable CER scorecard for the fine-tuned model via its ONNX export (onnxruntime).

Runs the (int8) ONNX on a gold manifest, computes micro CER + a seed-fixed utterance-bootstrap 95%
CI (Bisani & Ney ratio-of-sums), and emits per-clip results. Same NFC+lower+whitespace normalization
as the stock baseline scorecard so the numbers are comparable.

Env: CORTEX_FINETUNED_MODEL (HF dir, for the processor) + CORTEX_FINETUNED_ONNX (model.onnx).
Usage: python scripts/scorecard_finetuned.py <gold_manifest.tsv> [bootstrap=3000]
"""
import os
import random
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
    n_boot = int(sys.argv[2]) if len(sys.argv) > 2 else 3000

    processor = AutoProcessor.from_pretrained(MODEL)
    sess = ort.InferenceSession(ONNX, providers=["CPUExecutionProvider"])
    iname = sess.get_inputs()[0].name

    rows = [l.rstrip("\n").split("\t") for l in open(manifest, encoding="utf-8") if "\t" in l]
    per_clip = []  # (char_dist, char_ref_len)
    for i, (wav, ref) in enumerate(rows):
        audio, sr = sf.read(wav)
        if audio.ndim > 1:
            audio = audio.mean(axis=1)
        inputs = processor(audio, sampling_rate=16000, return_tensors="np")
        iv = inputs["input_values"].astype(np.float32)
        logits = sess.run(None, {iname: iv})[0]
        hyp = processor.batch_decode(logits.argmax(axis=-1))[0]
        r, h = norm(ref), norm(hyp)
        o = jiwer.process_characters(r if r else " ", h if h else "")
        per_clip.append((o.substitutions + o.deletions + o.insertions, len(r) if r else 1))
        if (i + 1) % 50 == 0:
            print(f"  ...{i+1}/{len(rows)}")

    dists = np.array([d for d, _ in per_clip], dtype=float)
    refs = np.array([r for _, r in per_clip], dtype=float)
    micro = dists.sum() / max(refs.sum(), 1.0)

    rng = random.Random(42)
    n = len(per_clip)
    boots = []
    idx = list(range(n))
    for _ in range(n_boot):
        sample = [idx[rng.randrange(n)] for _ in range(n)]
        sd = dists[sample].sum()
        sr = refs[sample].sum()
        boots.append(sd / max(sr, 1.0))
    lo, hi = np.percentile(boots, [2.5, 97.5])

    # per-clip dump
    out_tsv = os.path.join(os.path.dirname(manifest), "finetuned_results.tsv")
    with open(out_tsv, "w", encoding="utf-8") as f:
        f.write("char_dist\tchar_ref_len\n")
        for d, r in per_clip:
            f.write(f"{d}\t{r}\n")

    print(f"\n=================================================")
    print(f"  FINE-TUNED micro CER = {micro*100:.2f}%   95% CI [{lo*100:.2f}%, {hi*100:.2f}%]   N={n}")
    print(f"  (stock OmniASR-CTC-300M baseline: 29.40%, N=400)")
    print(f"  per-clip -> {out_tsv}")
    print(f"=================================================")
    return 0


if __name__ == "__main__":
    sys.exit(main())
