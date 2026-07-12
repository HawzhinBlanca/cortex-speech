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

Multi-GPU: CORTEX_7B_DEVICES="0,1" (default: every GPU nvidia-smi reports; "cpu" forces CPU)
pre-forks ONE WORKER PROCESS per GPU. Each child pins its card via CUDA_VISIBLE_DEVICES, loads a
full model replica (~15-17 GB bf16 fits a single 24 GB card), and accept()s on the SHARED listening
socket — the kernel load-balances connections across children, so N GPUs serve N requests truly in
parallel. Separate processes, not threads, ON PURPOSE: the 7B decode is an autoregressive Python
loop, and two replica THREADS in one interpreter serialize on the GIL — measured 1.10x on 2x
3090 Ti, vs the honest parallelism of processes. A single device (or CPU) runs the same serial
loop inline, exactly like the original single-model server.
"""
import os
import socket
import subprocess
import sys

HOST = os.environ.get("CORTEX_7B_HOST", "127.0.0.1")
PORT = int(os.environ.get("CORTEX_7B_PORT", "8799"))
LANG = os.environ.get("CORTEX_7B_LANG", "ckb_Arab")


def parse_device_indices() -> list:
    """GPU indices to serve on — WITHOUT touching CUDA. The parent must stay CUDA-free: a forked
    child cannot safely inherit an initialized CUDA context, so device discovery goes through
    nvidia-smi and each child initializes CUDA itself after pinning CUDA_VISIBLE_DEVICES."""
    spec = os.environ.get("CORTEX_7B_DEVICES", "").strip()
    if spec.lower() == "cpu":
        return []
    if spec:
        return [int(i) for i in spec.split(",") if i.strip() != ""]
    try:
        out = subprocess.run(["nvidia-smi", "-L"], capture_output=True, text=True, timeout=10)
        if out.returncode == 0:
            return list(range(len([l for l in out.stdout.splitlines() if l.startswith("GPU ")])))
    except Exception:
        pass
    return []


def worker(srv: socket.socket, tag: str) -> None:
    """One full replica: heavy imports, model load, then the proven serial accept loop. Runs in its
    own process per GPU (or inline for single-device/CPU). Everything CUDA lives below this line."""
    import copy
    import json
    import tempfile
    from pathlib import Path

    import torch
    import torchaudio

    # --- model paths (base .pt re-fetchable; LoRA + tokenizer live in ./model) --------------------
    # IMPORTANT: fairseq2's SentencePiece loader URI-encodes the tokenizer path, so a MODEL_DIR
    # containing spaces becomes "%20"-mangled and "cannot be opened". Override with
    # CORTEX_7B_MODEL_DIR pointing at a SPACE-FREE copy of the model/ dir.
    model_dir = Path(os.environ.get("CORTEX_7B_MODEL_DIR", str(Path(__file__).resolve().parent / "model")))
    os.environ.setdefault("FAIRSEQ2_USER_ASSET_DIR", str(model_dir))

    # --- fairseq2 / PEFT-LoRA compatibility patches (verbatim from server.py, the proven recipe) --
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
        print(f"[{tag}] Failed to import core libraries: {e}", flush=True)
        print("Ensure torch, fairseq2, peft, and omnilingual-asr are installed (use the champion venv).")
        os._exit(1)

    BaseLinear.in_features = property(lambda self: self.input_dim)
    BaseLinear.out_features = property(lambda self: self.output_dim)

    orig_post_init = LoraConfig.__post_init__

    def patched_post_init(self):
        orig_post_init(self)
        self._register_custom_module({BaseLinear: LoraLinear})

    LoraConfig.__post_init__ = patched_post_init

    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    dtype = torch.bfloat16 if device.type == "cuda" else torch.float32
    print(f"[{tag}] Device: {device} | Precision: {dtype}", flush=True)

    model_card = "soranivoice_omniASR_LLM_7B_v2_local"
    tokenizer_path = model_dir / "omniASR_tokenizer_written_v2.model"
    checkpoint_path = model_dir / "omniASR-LLM-7B-v2.pt"
    if not checkpoint_path.exists():
        env_path = os.environ.get("BASE_MODEL_PATH")
        if env_path and Path(env_path).exists():
            checkpoint_path = Path(env_path)
        else:
            cache = Path.home() / ".cache" / "fairseq2" / "assets" / "omniASR-LLM-7B-v2" / "omniASR-LLM-7B-v2.pt"
            if cache.exists():
                checkpoint_path = cache
    print(f"[{tag}] Base checkpoint: {checkpoint_path}", flush=True)
    if not checkpoint_path.exists():
        print(f"[{tag}] Error: base omniASR-LLM-7B-v2.pt not found (package, BASE_MODEL_PATH, or ~/.cache).")
        os._exit(1)

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

    print(f"[{tag}] Loading base weights (~30 GB, one minute)...", flush=True)
    base_model = load_model(model_card, device=device, dtype=dtype, config=config_custom)
    if (model_dir / "adapter_model.safetensors").exists():
        print(f"[{tag}] Applying LoRA adapter from {model_dir}...", flush=True)
        peft_model = PeftModel.from_pretrained(base_model, str(model_dir))
        pipeline_model = peft_model.base_model.model
        print(f"[{tag}] LoRA applied.", flush=True)
    else:
        print(f"[{tag}] WARNING: no LoRA adapter found — serving BASE weights only.", flush=True)
        pipeline_model = base_model

    tokenizer = load_tokenizer(model_card)
    pipeline = ASRInferencePipeline(
        model_card=None, model=pipeline_model, tokenizer=tokenizer, device=device, dtype=dtype
    )
    print(f"[{tag}] Pipeline ready.", flush=True)

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

    # listen() HERE, not in the parent: the port must only answer once a replica can actually
    # transcribe — the launcher's READY probe and the app's preflight are TCP connects, and a
    # parent-side listen would make them pass minutes before any model is loaded. The first
    # worker's listen() flips the shared socket to listening; later workers' calls are no-ops.
    srv.listen(16)
    # The proven serial loop: one request at a time PER PROCESS. With N pre-forked workers all
    # accept()ing this same socket, the kernel hands each incoming connection to exactly one free
    # worker — cross-GPU parallelism with zero shared Python state.
    print(f"[{tag}] serving on {HOST}:{PORT} (lang={LANG})", flush=True)
    while True:
        conn, _ = srv.accept()
        try:
            handle(conn)
        except Exception as e:
            print(f"[{tag}] handler error: {e}", flush=True)
        finally:
            conn.close()


def main() -> None:
    indices = parse_device_indices()
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((HOST, PORT))
    # Deliberately NOT listening yet — see the worker's listen() note.

    print("=" * 60, flush=True)
    print("Loading OmniASR-7B Champion (base + Kurdish LoRA)...", flush=True)
    if not indices:
        # Hide every GPU BEFORE worker() derives its device from torch.cuda.is_available() —
        # without this, "cpu" printed a CPU banner while silently loading ~16 GB onto GPU 0
        # (adversarial review 2026-07-12), defeating the documented override (e.g. keeping the
        # server off the cards during a training run).
        os.environ["CUDA_VISIBLE_DEVICES"] = ""
        print("Devices: [cpu] (no CUDA GPU reported / CORTEX_7B_DEVICES=cpu)", flush=True)
        worker(srv, "cpu")
        return
    if len(indices) == 1 or not hasattr(os, "fork"):
        # Single GPU (or a fork-less platform): identical to the original single-model server.
        os.environ["CUDA_VISIBLE_DEVICES"] = str(indices[0])
        print(f"Devices: [cuda GPU {indices[0]}] (single replica)", flush=True)
        worker(srv, f"gpu{indices[0]}")
        return

    print(f"Devices: GPUs {indices} — pre-forking one worker process per GPU", flush=True)
    children = []
    for i in indices:
        pid = os.fork()
        if pid == 0:
            # Child: pin the card BEFORE any CUDA init, then never return.
            os.environ["CUDA_VISIBLE_DEVICES"] = str(i)
            worker(srv, f"gpu{i}")
            os._exit(0)
        children.append(pid)
    srv.close()  # only the workers accept
    print(f"parent: {len(children)} workers forked (pids {children}); serving once all load.", flush=True)
    supervise_workers(set(children), len(children))


def supervise_workers(live, total, reap=None, exit_fn=None):
    """Reap workers as they exit, degrading gracefully. SCALE RESILIENCE: a single worker death (a
    rare OOM / CUDA hiccup over a long session) must NOT take the whole server down — that turns a
    transient blip into a ~10-min full reload. The surviving workers keep accept()ing on the shared
    listen socket, so remaining replicas serve on (at reduced throughput); the parent logs LOUDLY so
    the reduced capacity is never silent (the earlier build deliberately died here to avoid a silent
    half-capacity server — loud logging keeps that honesty while staying ALIVE). Only exit once EVERY
    worker is gone. `live` is the set of child pids; `total` the original count. `reap`/`exit_fn` are
    injectable so the loop is unit-testable without real fork()/os.wait() (Linux-only)."""
    reap = reap or os.wait
    exit_fn = exit_fn or sys.exit
    while live:
        pid, status = reap()
        live.discard(pid)
        if live:
            print(
                f"parent: WORKER {pid} DIED (status {status}) — SERVING DEGRADED on {len(live)} of "
                f"{total} replica(s). Restart the server when convenient to restore full capacity.",
                flush=True,
            )
        else:
            print(f"parent: last worker {pid} exited (status {status}); no replicas left — stopping.", flush=True)
    exit_fn(1)


if __name__ == "__main__":
    main()
