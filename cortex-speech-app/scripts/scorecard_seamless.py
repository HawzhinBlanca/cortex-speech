#!/usr/bin/env python3
"""SeamlessM4T-v2 baseline scorecard — the charter's required external baseline (M4a / charter line 13;
stock Whisper is explicitly NOT valid for ckb).

Scores facebook/seamless-m4t-v2-large (speech->text, tgt_lang=ckb) on a gold manifest with the SAME
NFC+lower+whitespace (space-KEPT) normalization as every other scorecard here, prints the verbatim
headline (micro CER/WER + seed-42 bootstrap CI via asr_metrics), and writes a per-clip TSV
(char_dist, char_ref_len, word_dist, word_ref_len — manifest row order) compatible with
scorecard_stats.py and pairable with omni7b_results.tsv for MAPSSWE.

Usage (WSL venv with transformers+torch; CPU is fine — GPUs may be held by the warm 7B server):
    python scripts/scorecard_seamless.py <gold_manifest.tsv> <out_results.tsv> [bootstrap=2000]
"""
import os
import sys
import time
import unicodedata

import soundfile as sf
import torch

from asr_metrics import bootstrap_ci, micro, word_pair

MODEL_ID = os.environ.get("CORTEX_SEAMLESS_MODEL", "facebook/seamless-m4t-v2-large")


def norm(s: str) -> str:
    return " ".join(unicodedata.normalize("NFC", s or "").strip().lower().split())


# Stamped into the TSV so a comparison can prove both engines were scored the same way (audit H1).
# This harness has no alternate basis; the constant names the one it uses.
NORM_BASIS = "nfc_lower_space_kept"


def edit_distance(a, b) -> int:
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (0 if ca == cb else 1)))
        prev = cur
    return prev[-1]


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    sys.stdout.reconfigure(encoding="utf-8")
    manifest, out_tsv = sys.argv[1], sys.argv[2]
    n_boot = int(sys.argv[3]) if len(sys.argv) > 3 else 2000

    from transformers import AutoProcessor, SeamlessM4Tv2ForSpeechToText

    print(f"loading {MODEL_ID} (first run downloads ~9 GB)...", flush=True)
    processor = AutoProcessor.from_pretrained(MODEL_ID)
    model = SeamlessM4Tv2ForSpeechToText.from_pretrained(MODEL_ID)
    model.eval()
    print("model ready.", flush=True)

    rows = [l.rstrip("\n").split("\t") for l in open(manifest, encoding="utf-8") if "\t" in l]
    per_clip, word_clip = [], []
    t0 = time.perf_counter()
    with open(out_tsv, "w", encoding="utf-8") as f:
        # clip_index (the manifest row index) is the FIRST column so mapsswe_compare.py can pair two
        # engines' TSVs by CLIP, not by row position. Skipped clips (decode error / empty ref below) drop
        # their row entirely, so without a clip id a comparison silently misaligns when the two engines skip
        # DIFFERENT clips (equal row counts, mismatched clips). Both scorecards iterate the same manifest, so
        # the manifest index is the shared, tab-safe key.
        # norm_basis stamps WHICH normalization produced these counts, so mapsswe_compare.py can refuse
        # a cross-basis pairing (audit H1). This scorecard has no CER_STRIP switch — norm() here is
        # always the strict space-kept basis — but the stamp must be present for the check to work.
        f.write("clip_index\tchar_dist\tchar_ref_len\tword_dist\tword_ref_len\tnorm_basis\n")
        for i, row in enumerate(rows):
            wav, ref = row[0], row[1]
            try:
                audio, sr = sf.read(wav)
                inputs = processor(audios=audio, sampling_rate=sr, return_tensors="pt")
                with torch.no_grad():
                    tokens = model.generate(**inputs, tgt_lang="ckb")
                hyp = processor.decode(tokens[0].tolist(), skip_special_tokens=True)
            except Exception as e:
                print(f"  SKIP {wav}: {e}", flush=True)
                continue
            r, h = norm(ref), norm(hyp)
            if not r:
                continue  # zero-reference clips drop out of the ratio-of-sums (matches eval.rs)
            cd, cr = edit_distance(list(r), list(h)), len(r)
            wd, wr = word_pair(r, h)
            per_clip.append((cd, cr))
            word_clip.append((wd, wr))
            f.write(f"{i}\t{cd}\t{cr}\t{wd}\t{wr}\t{NORM_BASIS}\n")
            if (i + 1) % 25 == 0:
                print(f"  ...{i + 1}/{len(rows)}", flush=True)

    n = len(per_clip)
    if n == 0:
        print("no scorable clips")
        return 1
    cer = micro(per_clip)
    clo, chi = bootstrap_ci(per_clip, n_boot, 42)
    wer = micro(word_clip)
    wlo, whi = bootstrap_ci(word_clip, n_boot, 42)
    total = time.perf_counter() - t0
    print("=" * 60)
    print(f"  SeamlessM4T-v2 micro CER = {cer * 100:.2f}%   95% CI [{clo * 100:.2f}%, {chi * 100:.2f}%]   N={n}")
    print(f"  SeamlessM4T-v2 micro WER = {wer * 100:.2f}%   95% CI [{wlo * 100:.2f}%, {whi * 100:.2f}%]   N={n}")
    print(f"  ({total / max(n, 1):.2f} s/clip; {total:.0f}s total)   per-clip -> {out_tsv}")
    print("=" * 60)
    return 0


if __name__ == "__main__":
    sys.exit(main())
