#!/usr/bin/env python3
"""Warm OmniASR-7B Champion server (TCP line protocol on 127.0.0.1:8799).

The authoritative base+LoRA+tokenizer load recipe serving the protocol that
`cortex_7b_client.py` and `scripts/scorecard_7b.py` expect. COMMITTED to the repo
(owner ship decision 2026-07-10: ship = personal use, truly reliable) so the default
engine is recoverable from a clean checkout — previously this file lived only on one
machine, a bus-factor-1 risk. All machine-specific paths come from env vars; the
~30 GB base checkpoint is re-fetchable (Meta omniASR-LLM-7B-v2) and the Kurdish LoRA
+ tokenizer live in the model dir pointed to by CORTEX_7B_MODEL_DIR.

Protocol (newline-delimited JSON, one request per connection):
  ->  {"audio_path": "/mnt/c/...wav", "start_ms": 1200, "end_ms": 4800}   (start/end optional)
  <-  {"transcript": "..."}                       on success (empty string is legitimate: silent clip)
  <-  {"error": "..."}                             on any failure (nothing fabricated)

Run INSIDE WSL with the champion venv (torch, fairseq2, peft, omnilingual-asr):
  CORTEX_7B_MODEL_DIR=/path/without/spaces CORTEX_7B_PORT=8799 python cortex_7b_server.py
"""
import os
import sys
import copy
import json
import socket
import tempfile
from pathlib import Path

import torch
import torchaudio

# --- model paths (base .pt re-fetchable; LoRA + tokenizer live in ./model) ------------------------
# IMPORTANT: fairseq2's SentencePiece loader URI-encodes the tokenizer path, so a MODEL_DIR containing
# spaces becomes "%20"-mangled and "cannot be opened". Override with CORTEX_7B_MODEL_DIR pointing at a
# SPACE-FREE copy of the model/ dir.
MODEL_DIR = Path(os.environ.get("CORTEX_7B_MODEL_DIR", str(Path(__file__).resolve().parent / "model")))
os.environ.setdefault("FAIRSEQ2_USER_ASSET_DIR", str(MODEL_DIR))

HOST = os.environ.get("CORTEX_7B_HOST", "127.0.0.1")
PORT = int(os.environ.get("CORTEX_7B_PORT", "8799"))
LANG = os.environ.get("CORTEX_7B_LANG", "ckb_Arab")

# --- fairseq2 / PEFT-LoRA compatibility patches (verbatim from server.py, the proven recipe) ------
try:
    from fairseq2.nn.projection import Linear as BaseLinear
    from peft.tuners.lora import Linear as LoraLinear
    from peft import PeftModel, LoraConfig
    from fairseq2.models.hub import load_model
    from fairseq2.data.tokenizers import load_tokenizer
    from fairseq2.assets import get_asset_store, load_in_memory_asset_metadata
    from fairseq2.runtime.dependency import get_dependency_resolver
    from fairseq2.runtime.config_registry import get_config
    from omnilingual_asr.models.wav2vec2_llama import Wav2Vec2LlamaConfig
    from omnilingual_asr.models.inference.pipeline import ASRInferencePipeline
except ImportError as e:
    print(f"Failed to import core libraries: {e}", flush=True)
    print("Ensure torch, fairseq2, peft, and omnilingual-asr are installed (use the champion venv).")
    sys.exit(1)

BaseLinear.in_features = property(lambda self: self.input_dim)
BaseLinear.out_features = property(lambda self: self.output_dim)

orig_post_init = LoraConfig.__post_init__
def patched_post_init(self):
    orig_post_init(self)
    self._register_custom_module({BaseLinear: LoraLinear})
LoraConfig.__post_init__ = patched_post_init

print("=" * 60, flush=True)
print("Loading OmniASR-7B Champion (base + Kurdish LoRA)...", flush=True)
device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
dtype = torch.bfloat16 if torch.cuda.is_available() else torch.float32
print(f"Device: {device} | Precision: {dtype}", flush=True)

model_card = "soranivoice_omniASR_LLM_7B_v2_local"
tokenizer_path = MODEL_DIR / "omniASR_tokenizer_written_v2.model"
checkpoint_path = MODEL_DIR / "omniASR-LLM-7B-v2.pt"
if not checkpoint_path.exists():
    env_path = os.environ.get("BASE_MODEL_PATH")
    if env_path and Path(env_path).exists():
        checkpoint_path = Path(env_path)
    else:
        cache = Path.home() / ".cache" / "fairseq2" / "assets" / "omniASR-LLM-7B-v2" / "omniASR-LLM-7B-v2.pt"
        if cache.exists():
            checkpoint_path = cache
print(f"Base checkpoint: {checkpoint_path}", flush=True)
if not checkpoint_path.exists():
    print("Error: base omniASR-LLM-7B-v2.pt not found (package, BASE_MODEL_PATH, or ~/.cache).")
    sys.exit(1)

store = get_asset_store()
store._metadata_providers = [p for p in store._metadata_providers if getattr(p, "_source", "") != "sorani_custom"]
provider = load_in_memory_asset_metadata(
    "sorani_custom",
    [
        {
            "name": "soranivoice_omniASR_tokenizer_written_v2_local",
            "tokenizer_family": "char_tokenizer",
            "tokenizer": str(tokenizer_path.resolve()),
        },
        {
            "name": model_card,
            "model_family": "wav2vec2_llama",
            "model_arch": "7b",
            "checkpoint": str(checkpoint_path.resolve()),
            "tokenizer_ref": "soranivoice_omniASR_tokenizer_written_v2_local",
        },
    ],
)
store._metadata_providers.append(provider)

resolver = get_dependency_resolver()
base_config = get_config(resolver, Wav2Vec2LlamaConfig, "7b")
config_custom = copy.deepcopy(base_config)
config_custom.llama_config.vocab_size = 10288
config_custom.wav2vec2_asr_config.target_vocab_size = 10288

print("Loading base weights (~30 GB, one minute)...", flush=True)
base_model = load_model(model_card, device=device, dtype=dtype, config=config_custom)
if (MODEL_DIR / "adapter_model.safetensors").exists():
    print(f"Applying LoRA adapter from {MODEL_DIR}...", flush=True)
    peft_model = PeftModel.from_pretrained(base_model, str(MODEL_DIR))
    pipeline_model = peft_model.base_model.model
    print("LoRA applied.", flush=True)
else:
    print("WARNING: no LoRA adapter found — serving BASE weights only.", flush=True)
    pipeline_model = base_model

tokenizer = load_tokenizer(model_card)
pipeline = ASRInferencePipeline(
    model_card=None, model=pipeline_model, tokenizer=tokenizer, device=device, dtype=dtype
)
print("Pipeline ready.", flush=True)
print("=" * 60, flush=True)


def transcribe(audio_path: str, start_ms=None, end_ms=None) -> str:
    """Resample to 16 kHz mono (OmniASR requirement), optionally clip [start_ms,end_ms), transcribe."""
    wav, sr = torchaudio.load(audio_path)  # (channels, samples)
    if start_ms is not None and end_ms is not None and end_ms > start_ms:
        a = max(0, int(start_ms / 1000.0 * sr))
        b = min(wav.size(1), int(end_ms / 1000.0 * sr))
        wav = wav[:, a:b]
    if sr != 16000:
        wav = torchaudio.transforms.Resample(orig_freq=sr, new_freq=16000)(wav)
        sr = 16000
    if wav.size(0) > 1:
        wav = torch.mean(wav, dim=0, keepdim=True)
    tmp = tempfile.NamedTemporaryFile(delete=False, suffix=".wav")
    tmp.close()
    try:
        torchaudio.save(tmp.name, wav, 16000)
        out = pipeline.transcribe([tmp.name], lang=[LANG], batch_size=1)
        return out[0] if out else ""
    finally:
        try:
            os.remove(tmp.name)
        except OSError:
            pass


def handle(conn: socket.socket) -> None:
    buf = b""
    while not buf.endswith(b"\n"):
        d = conn.recv(65536)
        if not d:
            break
        buf += d
    if not buf.strip():
        return
    try:
        req = json.loads(buf.decode("utf-8").strip())
        text = transcribe(req["audio_path"], req.get("start_ms"), req.get("end_ms"))
        reply = {"transcript": text}
    except Exception as e:  # never fabricate — report the failure to the caller
        reply = {"error": str(e)}
    conn.sendall((json.dumps(reply, ensure_ascii=False) + "\n").encode("utf-8"))


def main() -> None:
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((HOST, PORT))
    srv.listen(8)
    print(f"OmniASR-7B server listening on {HOST}:{PORT} (lang={LANG})", flush=True)
    # ponytail: serial accept loop — one warm model, callers (scorecard/app) send one request at a
    # time. Add threading only if concurrent decode is ever needed (a single GPU serializes anyway).
    while True:
        conn, _ = srv.accept()
        try:
            handle(conn)
        except Exception as e:
            print(f"handler error: {e}", flush=True)
        finally:
            conn.close()


if __name__ == "__main__":
    main()
