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
import time
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
    word_clip = []  # P2.1: (word_dist, word_ref_len) for WER, on the SAME normalized pairs as CER
    total_proc_s = 0.0  # P2.1: per-clip processing wall-clock (decode + inference)
    total_audio_s = 0.0  # P2.1: per-clip audio duration -> real RTF (soundfile gives the true length)
    for i, row in enumerate(rows):
        # Accept the 4-field gold manifest (wav/ref/gender/age) as well as a 2-field one.
        wav, ref = row[0], row[1]
        t0 = time.perf_counter()
        audio, sr = sf.read(wav)
        clip_audio_s = len(audio) / sr if sr else 0.0
        if audio.ndim > 1:
            audio = audio.mean(axis=1)
        inputs = processor(audio, sampling_rate=16000, return_tensors="np")
        iv = inputs["input_values"].astype(np.float32)
        logits = sess.run(None, {iname: iv})[0]
        # skip_special_tokens=True to match measure_finetuned_cer.py and the model's integration_guide.md,
        # so <unk>/<s>/</s> argmax frames are dropped (not emitted as literal token strings). Without it
        # this scorecard decoded differently than the 19.77% reference it is published as comparable to.
        hyp = processor.batch_decode(logits.argmax(axis=-1), skip_special_tokens=True)[0]
        total_proc_s += time.perf_counter() - t0
        total_audio_s += clip_audio_s
        r, h = norm(ref), norm(hyp)
        if not r:
            # Zero-reference clip drops out of the ratio-of-sums (numerator AND denominator), matching
            # eval.rs — the old `" "` + ref_len=1 hack inflated micro CER and the bootstrap CI.
            continue
        o = jiwer.process_characters(r, h if h else "")
        per_clip.append((o.substitutions + o.deletions + o.insertions, len(r)))
        ow = jiwer.process_words(r, h if h else "")
        word_clip.append((ow.substitutions + ow.deletions + ow.insertions, len(r.split())))
        if (i + 1) % 50 == 0:
            print(f"  ...{i+1}/{len(rows)}")

    if not per_clip:
        # Every row was empty-reference (dropped from the ratio-of-sums) or the manifest had no
        # tab-separated rows. Fail CLEANLY with an actionable message — otherwise the seeded bootstrap
        # below calls random.randrange(0) and dies with a cryptic ValueError mid-measurement.
        print(
            "no scorable clips: every manifest row was empty-reference or not tab-separated. "
            "Expected lines of 'wav<TAB>ref[<TAB>gender<TAB>age]'; check the gold manifest path/format.",
            file=sys.stderr,
        )
        return 2

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

    # P2.1: micro WER + its own seeded bootstrap (same recipe/seed as CER, on the word arrays), and
    # the real RTF from the summed audio durations. CER path above is untouched (publishes the 21%).
    wdists = np.array([d for d, _ in word_clip], dtype=float)
    wrefs = np.array([r for _, r in word_clip], dtype=float)
    wer = wdists.sum() / max(wrefs.sum(), 1.0)
    wrng = random.Random(42)
    wboots = []
    for _ in range(n_boot):
        wsample = [idx[wrng.randrange(n)] for _ in range(n)]
        wboots.append(wdists[wsample].sum() / max(wrefs[wsample].sum(), 1.0))
    wlo, whi = np.percentile(wboots, [2.5, 97.5])
    rtf_val = total_proc_s / total_audio_s if total_audio_s > 0 else None

    # per-clip dump
    out_tsv = os.path.join(os.path.dirname(manifest), "finetuned_results.tsv")
    with open(out_tsv, "w", encoding="utf-8") as f:
        f.write("char_dist\tchar_ref_len\tword_dist\tword_ref_len\n")
        for (d, r), (wd, wr) in zip(per_clip, word_clip):
            f.write(f"{d}\t{r}\t{wd}\t{wr}\n")

    rtf_str = f"{rtf_val:.3f}" if rtf_val is not None else "n/a"
    print(f"\n=================================================")
    print(f"  FINE-TUNED micro CER = {micro*100:.2f}%   95% CI [{lo*100:.2f}%, {hi*100:.2f}%]   N={n}")
    print(f"  FINE-TUNED micro WER = {wer*100:.2f}%   95% CI [{wlo*100:.2f}%, {whi*100:.2f}%]   N={n}")
    print(f"  RTF = {rtf_str}  ({total_proc_s:.1f}s processing / {total_audio_s:.1f}s audio)")
    print(f"  (stock OmniASR-CTC-300M baseline: 29.40%, N=400)")
    print(f"  per-clip -> {out_tsv}")
    print(f"=================================================")

    # Optional regression gate: fail if micro CER exceeds the baseline CI upper bound by more than a
    # noise margin (default 1.5 absolute pts). Set CORTEX_SCORECARD_BASELINE to docs/finetuned_scorecard_baseline.json.
    baseline_path = os.environ.get("CORTEX_SCORECARD_BASELINE", "")
    if baseline_path:
        import json

        with open(baseline_path, encoding="utf-8") as bf:
            b = json.load(bf)
        margin = float(os.environ.get("CORTEX_SCORECARD_MARGIN_PTS", "1.5"))
        ceiling = float(b["ci_hi"]) + margin
        if micro * 100 > ceiling:
            print(f"REGRESSION: micro CER {micro*100:.2f}% > baseline ci_hi {b['ci_hi']}% + {margin}pt = {ceiling:.2f}%")
            return 1
        print(f"OK vs baseline: {micro*100:.2f}% <= {ceiling:.2f}%")
    return 0


if __name__ == "__main__":
    sys.exit(main())
