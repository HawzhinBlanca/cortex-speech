#!/usr/bin/env python3
"""Measure the fine-tuned MMS-CTC-1B Sorani model's micro CER on a gold manifest.

Loads the consolidated HF Wav2Vec2ForCTC model per the export integration guide, transcribes each
clip, and computes micro CER (sum char edit-distance / sum ref length) against the verified
references, using the SAME NFC+lower+whitespace normalization the baseline scorecard used so the
number is directly comparable to the stock OmniASR-CTC-300M baseline (29.40% CER).

Usage: python scripts/measure_finetuned_cer.py <gold_manifest.tsv>
"""
import os
import sys
import unicodedata

import jiwer
import soundfile as sf
import torch
from transformers import AutoModelForCTC, AutoProcessor

# Path to the fine-tuned HF Wav2Vec2ForCTC export directory (config.json + model.safetensors + ...).
MODEL = os.environ.get("CORTEX_FINETUNED_MODEL", "")


def norm(s: str) -> str:
    return " ".join(unicodedata.normalize("NFC", s).strip().lower().split())


def main() -> int:
    manifest = sys.argv[1]
    sys.stdout.reconfigure(encoding="utf-8")
    if not MODEL:
        print("set CORTEX_FINETUNED_MODEL to the fine-tuned model export directory")
        return 2
    print(f"loading {MODEL} ...")
    processor = AutoProcessor.from_pretrained(MODEL)
    # The weights are saved in bfloat16; cast to float32 so CPU conv1d matches the float32 audio
    # input (CPU has no fast bf16 conv path anyway).
    model = AutoModelForCTC.from_pretrained(MODEL).float()
    model.eval()
    print(f"loaded. torch={torch.__version__}, params={sum(p.numel() for p in model.parameters())/1e9:.2f}B")

    rows = [l.rstrip("\n").split("\t") for l in open(manifest, encoding="utf-8") if "\t" in l]
    total_dist = 0
    total_ref = 0
    examples = []
    for i, (wav, ref) in enumerate(rows):
        audio, sr = sf.read(wav)
        if audio.ndim > 1:
            audio = audio.mean(axis=1)
        inputs = processor(audio, sampling_rate=16000, return_tensors="pt")
        key = "input_values" if "input_values" in inputs else "input_features"
        with torch.no_grad():
            logits = model(inputs[key]).logits
        pred_ids = torch.argmax(logits, dim=-1)
        hyp = processor.batch_decode(pred_ids, skip_special_tokens=True)[0]
        r, h = norm(ref), norm(hyp)
        o = jiwer.process_characters(r if r else " ", h if h else "")
        total_dist += o.substitutions + o.deletions + o.insertions
        total_ref += len(r) if r else 1
        if i < 10:
            examples.append((ref, hyp))
        if (i + 1) % 10 == 0:
            print(f"  ...{i+1}/{len(rows)} clips")

    cer = total_dist / max(total_ref, 1)
    print(f"\n=================================================")
    print(f"  MMS-CTC-1B (fine-tuned) micro CER = {cer*100:.2f}%   (N={len(rows)})")
    print(f"  baseline (stock OmniASR-CTC-300M)  = 29.40%")
    print(f"=================================================\n")
    print("=== examples (ref -> hyp) ===")
    for ref, hyp in examples:
        print(f"ref: {ref}\nhyp: {hyp}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
