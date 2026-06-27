#!/usr/bin/env python3
"""Probe: does constraining OmniASR-CTC decoding to Kurdish-script tokens fix the language-lock loss?

Runs the model.int8.onnx directly (input = raw 16k audio, output = [T, 9812] CTC logits), does a
greedy CTC decode (a) unconstrained and (b) masked to {blank, space, Arabic-script tokens}, and
compares CER vs the reference. If (b) << (a), constrained decoding is a real, no-retrain fix.

Usage: python scripts/constrained_decode_probe.py <model.onnx> <tokens.txt> <manifest.tsv>
"""
import sys
import unicodedata
import wave
from collections import Counter

import numpy as np
import onnxruntime as ort
import jiwer


def load_tokens(path):
    id2tok = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line:
                continue
            tok, idstr = line.rsplit(" ", 1)
            id2tok[int(idstr)] = tok
    n = max(id2tok) + 1
    return [id2tok.get(i, "") for i in range(n)]


def is_arabic(tok):
    return any("؀" <= c <= "ۿ" or "ݐ" <= c <= "ݿ" for c in tok)


def read_wav_f32(path):
    w = wave.open(path, "rb")
    pcm = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32) / 32768.0
    w.close()
    return pcm


def greedy_ctc(logits, blank, toks, keep=None):
    if keep is not None:
        masked = np.full_like(logits, -1e30)
        masked[:, keep] = logits[:, keep]
        logits = masked
    ids = logits.argmax(axis=-1)
    out, prev = [], -1
    for i in ids:
        if i != prev and i != blank:
            out.append(toks[i])
        prev = i
    return "".join(out)


def norm(s):
    return " ".join(unicodedata.normalize("NFC", s).strip().lower().split())


def cer(ref, hyp):
    r, h = norm(ref), norm(hyp)
    if not r:
        return 0.0 if not h else 1.0
    return jiwer.cer(r, h)


def main():
    model, tokens, manifest = sys.argv[1], sys.argv[2], sys.argv[3]
    toks = load_tokens(tokens)
    sess = ort.InferenceSession(model, providers=["CPUExecutionProvider"])
    iname = sess.get_inputs()[0].name

    rows = [l.rstrip("\n").split("\t") for l in open(manifest, encoding="utf-8") if "\t" in l]
    all_logits, refs = [], []
    for path, ref in rows:
        pcm = read_wav_f32(path)
        logits = sess.run(None, {iname: pcm[None, :]})[0][0]  # [T, 9812]
        all_logits.append(logits)
        refs.append(ref)

    # blank = most frequent argmax across all frames (CTC emits blank most of the time)
    cnt = Counter()
    for lg in all_logits:
        cnt.update(lg.argmax(axis=-1).tolist())
    blank = cnt.most_common(1)[0][0]

    space_ids = [i for i, t in enumerate(toks) if t == " "]
    arab_ids = [i for i, t in enumerate(toks) if is_arabic(t)]
    keep = sorted(set([blank] + space_ids + arab_ids))

    sys.stdout.reconfigure(encoding="utf-8")
    print(f"blank id (empirical) = {blank} -> token {toks[blank]!r}")
    print(f"kept tokens for constrained decode: {len(keep)} (1 blank + {len(space_ids)} space + {len(arab_ids)} Arabic)")

    # micro CER both ways
    def micro(constrained):
        import jiwer as J

        td = tr = 0
        examples = []
        for lg, ref in zip(all_logits, refs):
            hyp = greedy_ctc(lg, blank, toks, keep if constrained else None)
            examples.append((ref, hyp))
            r, h = norm(ref), norm(hyp)
            if not r:
                # Zero-reference clip drops out of the ratio-of-sums (numerator AND denominator), matching
                # eval.rs and measure_finetuned_cer.py / scorecard_finetuned.py so this probe's CER stays
                # comparable to the published 19.77% scorecard. The old `r if r else " "` + `tr += 1` hack
                # injected the full hypothesis insertion distance against a phantom 1-char denominator.
                continue
            o = J.process_characters(r, h if h else "")
            td += o.substitutions + o.deletions + o.insertions
            tr += len(r)
        return td / max(tr, 1), examples

    cer_u, ex_u = micro(False)
    cer_c, ex_c = micro(True)
    print(f"\nUNCONSTRAINED micro CER = {cer_u * 100:.2f}%")
    print(f"CONSTRAINED  micro CER = {cer_c * 100:.2f}%   (Kurdish-token-masked decode)")
    print(f"=> change: {(cer_u - cer_c) * 100:+.2f} pts")
    print("\n=== examples (ref | unconstrained | constrained) ===")
    for (ref, hu), (_, hc) in list(zip(ex_u, ex_c))[:10]:
        print(f"ref: {ref}\n  unc: {hu}\n  con: {hc}\n")


if __name__ == "__main__":
    main()
