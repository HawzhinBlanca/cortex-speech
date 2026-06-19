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
Defaults to Nawras with the 0-15s / 15-30s / 30-36s VAD slices.
"""
import os, sys, re, json, tempfile, subprocess

os.environ.setdefault("HF_HOME", "/mnt/c/Users/hawzh/.cache/huggingface")
os.environ.setdefault("HF_HUB_OFFLINE", "1")  # use only the local cache; never call the network

AUDIO = sys.argv[1] if len(sys.argv) > 1 else "/mnt/c/Users/hawzh/Desktop/Nawras - KU.wav"
if len(sys.argv) > 2:
    BOUNDS = [tuple(int(x) for x in a.split("-")) for a in sys.argv[2:]]
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
    consensus = o or w or x  # 7B primary
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
