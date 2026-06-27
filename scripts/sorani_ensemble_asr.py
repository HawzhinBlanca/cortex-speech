#!/usr/bin/env python3
"""
Sorani (ckb) diverse-ensemble ASR.

Runs three architecturally-distinct engines on the same audio and fuses them:
  1. OmniASR-LLM-7B-v2   (fairseq2 / Meta omnilingual — encoder + LLM decoder)
  2. Whisper-medium-ckb  (transformers — encoder/decoder seq2seq)
  3. Central-Kurdish-XLSR (transformers — wav2vec2 CTC)

Because the three architectures fail differently, their *agreement* is a real
confidence signal (unlike a hard-coded 0.90/0.95). For each segment we report
every engine's transcript plus a mean pairwise character-agreement score, and
pick the 7B as the consensus primary while flagging low-agreement segments for
human review.

Usage:
  python3 sorani_ensemble_asr.py <audio.wav> [start_ms-end_ms ...]
The audio path is required; bounds default to the 0-15s / 15-30s / 30-36s VAD slices.
Set HF_HOME in the environment if your model cache is not at the platform default.
"""
import os, sys, re, json, tempfile, subprocess

# Use only the local HF cache; never call the network. HF_HOME is intentionally NOT hardcoded here —
# it defaults to the platform standard (~/.cache/huggingface) and is overridable via the environment,
# so this tracked script never bakes in a private profile path (repo hygiene policy).
os.environ.setdefault("HF_HUB_OFFLINE", "1")

if len(sys.argv) < 2:
    sys.exit("usage: python3 sorani_ensemble_asr.py <audio.wav> [start_ms-end_ms ...]")
AUDIO = sys.argv[1]


def _parse_bound(a):
    parts = a.split("-")
    if len(parts) != 2:
        sys.exit(f"bad bound '{a}': expected <start_ms>-<end_ms>")
    try:
        return (int(parts[0]), int(parts[1]))
    except ValueError:
        sys.exit(f"bad bound '{a}': start and end must be integers (milliseconds)")


if len(sys.argv) > 2:
    BOUNDS = [_parse_bound(a) for a in sys.argv[2:]]
else:
    BOUNDS = [(0, 15000), (15000, 30000), (30000, 36000)]

WHISPER_CKB = "roseman/whisper-medium-ckb"
XLSR_CKB = "Akashpb13/Central_kurdish_xlsr"


def normalize_ckb(text):
    text = re.sub(r'[كڪڬ]', 'ک', text)
    text = re.sub(r'[يے]', 'ی', text)
    text = text.replace('ى', 'ی').replace('ـ', '')
    text = re.sub(r'[ً-ٰٟ]', '', text)
    return re.sub(r'\s+', ' ', text).strip()


def cer(a, b):
    """Character error rate between two strings (Levenshtein / len(a))."""
    a, b = list(a), list(b)
    if not a and not b:
        return 0.0
    if not a:
        return 1.0
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[-1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1] / max(len(a), 1)


def agreement(texts):
    """Mean pairwise character agreement (1 - CER) across present transcripts."""
    present = [t for t in texts if t]
    if len(present) < 2:
        return 0.0
    sims, n = 0.0, 0
    for i in range(len(present)):
        for j in range(i + 1, len(present)):
            sims += 1.0 - min(cer(present[i], present[j]), 1.0)
            n += 1
    return sims / n


def _align_to_anchor(anchor, other):
    """Word-level alignment of `other` onto `anchor`; returns, per anchor slot,
    the aligned word from `other` ('' = deletion). Insertions in other are dropped."""
    n, m = len(anchor), len(other)
    dp = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n + 1):
        dp[i][0] = i
    for j in range(m + 1):
        dp[0][j] = j
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            c = 0 if anchor[i - 1] == other[j - 1] else 1
            dp[i][j] = min(dp[i - 1][j] + 1, dp[i][j - 1] + 1, dp[i - 1][j - 1] + c)
    aligned = [''] * n
    i, j = n, m
    while i > 0 and j > 0:
        c = 0 if anchor[i - 1] == other[j - 1] else 1
        if dp[i][j] == dp[i - 1][j - 1] + c:
            aligned[i - 1] = other[j - 1]; i -= 1; j -= 1
        elif dp[i][j] == dp[i - 1][j] + 1:
            i -= 1            # deletion: anchor slot unmatched by other
        else:
            j -= 1            # insertion in other: dropped (anchor is longest)
    return aligned


def rover_fuse(hyps):
    """ROVER-style confusion-network majority vote over the diverse hypotheses.
    Anchor = longest hyp (captures every content slot, e.g. a year the primary
    dropped); each other hyp aligns to it and votes per slot; the majority word
    wins, ties prefer a non-empty token. Equal votes (a strong primary does not
    get extra weight) is what lets two agreeing voters restore content the
    primary missed."""
    from collections import Counter
    toks = [h.split() for h in hyps if h.strip()]
    if not toks:
        return ""
    if len(toks) == 1:
        return " ".join(toks[0])
    anchor = max(toks, key=len)
    cols = [Counter() for _ in anchor]
    for t in toks:
        aligned = anchor if t is anchor else _align_to_anchor(anchor, t)
        for k, word in enumerate(aligned):
            cols[k][word] += 1
    out = []
    for c in cols:
        best = max(c.items(), key=lambda kv: (kv[1], kv[0] != ''))
        if best[0]:
            out.append(best[0])
    return " ".join(out)


# --- prep audio: 16k mono, sliced ---
import soundfile as sf
print("Converting audio to 16k mono...", flush=True)
_t = tempfile.NamedTemporaryFile(suffix=".wav", delete=False); _t.close()
subprocess.run(["ffmpeg", "-y", "-i", AUDIO, "-ac", "1", "-ar", "16000", "-f", "wav", _t.name],
               check=True, capture_output=True)
audio, sr = sf.read(_t.name, dtype="float32"); os.remove(_t.name)
slices = []
for (s, e) in BOUNDS:
    seg = audio[int(s * 16):int(e * 16)]
    tf = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    sf.write(tf.name, seg, 16000); tf.close()
    slices.append(tf.name)
print(f"{len(slices)} segments @16k from {os.path.basename(AUDIO)}", flush=True)

results = {i: {"bounds": BOUNDS[i]} for i in range(len(slices))}


def free():
    import torch, gc
    gc.collect()
    if torch.cuda.is_available():
        torch.cuda.empty_cache()


# --- Engine 1: OmniASR-7B (fairseq2) ---
try:
    print("\n[1/3] OmniASR-7B v2 ...", flush=True)
    from omnilingual_asr.models.inference.pipeline import ASRInferencePipeline
    pipe7b = ASRInferencePipeline(model_card="omniASR_LLM_7B_v2")
    txts = pipe7b.transcribe(slices, lang=["ckb_Arab"] * len(slices), batch_size=16)
    for i, t in enumerate(txts):
        results[i]["omniasr_7b"] = normalize_ckb(t)
    del pipe7b; free()
    print("    OK", flush=True)
except Exception as e:
    print("    FAILED:", e, flush=True)

# --- Engine 2: Whisper-medium-ckb (transformers seq2seq) ---
try:
    print("\n[2/3] Whisper-medium-ckb ...", flush=True)
    import torch
    from transformers import pipeline as hf_pipeline
    asr = hf_pipeline("automatic-speech-recognition", model=WHISPER_CKB,
                      device=0 if torch.cuda.is_available() else -1,
                      torch_dtype=torch.float16)
    for i, p in enumerate(slices):
        out = asr(p)
        results[i]["whisper_ckb"] = normalize_ckb(out.get("text", ""))
    del asr; free()
    print("    OK", flush=True)
except Exception as e:
    print("    FAILED:", e, flush=True)

# --- Engine 3: Central-Kurdish-XLSR (wav2vec2 CTC) ---
try:
    print("\n[3/3] Central-Kurdish-XLSR ...", flush=True)
    import torch
    from transformers import pipeline as hf_pipeline
    asr = hf_pipeline("automatic-speech-recognition", model=XLSR_CKB,
                      device=0 if torch.cuda.is_available() else -1)
    for i, p in enumerate(slices):
        out = asr(p)
        results[i]["xlsr_ckb"] = normalize_ckb(out.get("text", ""))
    del asr; free()
    print("    OK", flush=True)
except Exception as e:
    print("    FAILED:", e, flush=True)

for f in slices:
    try: os.remove(f)
    except Exception: pass

# --- fuse + report ---
print("\n================= ENSEMBLE RESULTS =================", flush=True)
for i in range(len(slices)):
    r = results[i]
    o = r.get("omniasr_7b", ""); w = r.get("whisper_ckb", ""); x = r.get("xlsr_ckb", "")
    conf = agreement([o, w, x])
    consensus = rover_fuse([o, w, x])  # ROVER majority vote across the diverse engines
    r["agreement_confidence"] = round(conf, 3)
    r["consensus"] = consensus
    flag = "REVIEW" if conf < 0.6 else "OK"
    print(f"\n--- Segment {i+1} {r['bounds']}  agreement={conf:.2f}  [{flag}] ---", flush=True)
    print("  OmniASR-7B :", o, flush=True)
    print("  Whisper-ckb:", w, flush=True)
    print("  XLSR-ckb   :", x, flush=True)
    print("  CONSENSUS  :", consensus, flush=True)

print("\n__JSON__=" + json.dumps(results, ensure_ascii=False), flush=True)
print("__DONE__", flush=True)
