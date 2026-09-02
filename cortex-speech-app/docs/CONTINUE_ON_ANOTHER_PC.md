# Continue Cortex on another PC (agent handoff)

Goal: a fresh Claude Code on a second machine reproduces this exact working setup and continues the
work. Reuses the existing setup docs — this only adds the parts that are **not** in git (the private
champion model + the WSL server) and the handoff of open tasks. No hardcoded user paths: **find things
by name.**

## 0. Prereqs on the new PC
- Node 20+, Rust stable (`rustup`), Python 3.12.
- For the champion engine: WSL2 (Ubuntu) + an NVIDIA GPU with ≥24 GB VRAM and CUDA 12 drivers.
- Full build gate + rules: read `RELEASE.md` and the repo `CLAUDE.md` first.

## 1. Clone
```
git clone https://github.com/HawzhinBlanca/cortex-speech.git
cd cortex-speech/cortex-speech-app
```

## 2. Public assets (from the internet, SHA-pinned — already scripted)
```
npm ci
python scripts/fetch_models.py          # required VAD + ONNX Runtime support only
python scripts/fetch_models.py --check  # verify support + any optional ASR already present
```

## 3. The sole production ASR champion (NOT public, NOT in git)
The champion = base OmniASR-7B checkpoint + a Kurdish LoRA adapter + a warm WSL server. Locate each by
filename (run in WSL so it sees both Linux and `/mnt/*` Windows drives):
```
# base 30 GB checkpoint (re-fetchable via the omnilingual_asr/fairseq2 asset store on first server run)
find / /mnt -name 'omniASR-LLM-7B-v2.pt' 2>/dev/null
# the LoRA adapter + tokenizer + server (the private artifact — copy this dir over if not found)
find / /mnt -name 'adapter_model.safetensors' 2>/dev/null      # -> OmniASR_7B_Champion/adapter_weights/
find / /mnt -name 'omniASR_tokenizer_written_v2.model' 2>/dev/null
find / /mnt -name 'cortex_7b_server.py' 2>/dev/null            # tracked copy is at cortex-speech-app/scripts/
find / /mnt -name 'cortex_7b_client.py' 2>/dev/null            # tracked copy is at cortex-speech-app/scripts/
```
If the adapter/server/tokenizer aren't found, copy the whole `OmniASR_7B_Champion/` folder + the
`cortex_7b_server.py` from this PC. The base `.pt` re-downloads itself; the LoRA is yours to carry.

## 4. WSL Python env for the server
Create a venv with: `torch` (CUDA 12 build), `fairseq2`, `omnilingual_asr`, `peft`, `soundfile`,
`numpy`, `torchaudio`. (`cortex_7b_server.py`'s imports are the authoritative list.)
- Pitfall from this session: newer `datasets` wants `torchcodec`, whose default wheel needs CUDA 13
  (`libnvrtc.so.13`) and breaks on CUDA-12 envs. Don't fight it — for FLEURS decode use
  `load_dataset(..., ).cast_column("audio", Audio(decode=False))` + `soundfile`.

## 5. Start the champion server (keep it running)
```
CORTEX_7B_PORT=8799 python cortex_7b_server.py     # binds 127.0.0.1:8799 INSIDE WSL, loads ~30 GB
# verify (inside WSL): timeout 2 bash -c 'exec 3<>/dev/tcp/127.0.0.1/8799' && echo UP
```

## 6. Point the app at the champion (`%APPDATA%\cortex-speech\settings.json`)
```
"asr_model_size": "WSL7B",
"external_asr_script_path": "<WSL path to cortex_7b_client.py from step 3>",
"use_finetuned_asr": false,
"cloud_llm_opt_in": false, "jury_cloud_opt_in": false
```
Policy (already enforced in code): 7B is the ONLY engine that becomes the transcript; if it's down the
app offers only a champion repair/retry path — it never drops to a smaller or cloud model.

## 7. Build the app (frontend BEFORE cargo — non-negotiable)
```
npm run build
cargo build --release --manifest-path src-tauri/Cargo.toml
python scripts/check_exe_freshness.py     # must be GREEN
```

## 8. Verify it works like here
```
CORTEX_AUDIO="<path to any Kurdish .wav>" node e2e_real_app.cjs   # fails on blank/placeholder transcript
python scripts/build_review_page.py --manifest run.jsonl --out review.html --embed-audio
```

## 9. Continue the open work (state: `PROGRESS_LEDGER.md`, newest entry)
Two tasks were in flight; pick them up and record results in the ledger (that + git is how we
coordinate across machines — there is no shared live session):
1. **FLEURS-ckb clean CER** — measure the champion on FLEURS-ckb (`ckb_iq` test) to confirm the
   CV22 number (5.04% CER, but train/test disjointness unverified) and retain any smaller-model
   comparison as an explicitly offline diagnostic. Decode via `Audio(decode=False)`+soundfile, then
   `scripts/scorecard_7b.py <manifest> 2000`.
2. **Ask-dialog verify** — with the 7B server DOWN, drive the exe and confirm it shows the
   champion repair/retry action (and `transcribe_segment` rejects with `E_ASR_7B_UNAVAILABLE`),
   with no smaller-model action or silent downgrade.

## Rules (do not skip)
Honesty law + gates in `CLAUDE.md`: never fabricate a metric; nothing "done" until USER-OBSERVABLE or
MEASURED on real audio; commit on a branch, Conventional Commits; never put a private profile path in a
tracked file (`scripts/test_windows_repo_hygiene.py` enforces it — that's why this doc searches by name).
