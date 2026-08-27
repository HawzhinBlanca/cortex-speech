# WSL_DR_RUNBOOK.md — disaster recovery for the 7B training/serving environment (P5.4 / M5.4)

The WSL side (champion serving + retrain) is a single point of failure outside the app's own
backup/snapshot machinery. This runbook records what exists, what is irreplaceable vs re-buildable,
and the exact recovery procedure. **Environment facts below were re-probed live on 2026-08-20**
(external review finding: this runbook still described `/root/cortex_env`, an env the runtime no
longer uses — a recovery drill following it would have rebuilt the wrong environment at the wrong
path). Steps marked *(verify on WSL)* are standard procedures not yet exercised end-to-end here.

## What lives where (verified 2026-08-20)

| Asset | Location (WSL) | Size | Replaceable? |
|---|---|---|---|
| Python venv (SERVING) | `/home/ai/.venv-wsl-whisper` (Python 3.12.13) | small | **Rebuildable** from `scripts/wsl7b_requirements.lock` (committed, 158 pins, frozen from THIS venv) |
| fairseq2 asset cache | `/root/.cache/fairseq2` | **59 GB** | **Re-downloadable** (slow — hours on a fast line); grows over time |
| 7B champion adapter weights | `Kurdish_ASR_Model_Export/OmniASR_7B_Champion/` next to the repo (Windows side, reached via `/mnt/c/...`) | GBs | **IRREPLACEABLE** if the original training run is lost — back these up off-machine |
| Server script | `Kurdish_ASR_Model_Export/OmniASR_7B_Champion/scripts/cortex_7b_server.py` | tiny | In the export folder; also copy into any backup |
| Client (app side) | `cortex-speech-app/scripts/cortex_7b_client.py` | tiny | In git |

## Pinned environment (frozen from the live SERVING venv, 2026-08-20)

The complete, version-controlled lock is **`cortex-speech-app/scripts/wsl7b_requirements.lock`**
(158 pins — `pip freeze` of `/home/ai/.venv-wsl-whisper`). Headline pins:

```
python        3.12.13  (Ubuntu 26.04 LTS)
distro        Ubuntu 26.04 LTS
GPUs          2x NVIDIA GeForce RTX 3090 Ti
venv          /home/ai/.venv-wsl-whisper   <- the path the app and start helper default to
```

Keep the lock current: after any WSL env change,
`wsl -- /home/ai/.venv-wsl-whisper/bin/pip freeze > cortex-speech-app/scripts/wsl7b_requirements.lock`
and commit it — the lock in git IS the recovery source, not a note beside a backup.

## Backup priorities (do these BEFORE a disaster)

1. **Adapter weights + server script** (`OmniASR_7B_Champion/`) → copy to the offline F: corpus
   drive (or any off-machine location). This is the only truly irreplaceable asset.
2. **`pip freeze` snapshot** of `cortex_env` (command above) → stored with the adapter backup.
3. The fairseq2 cache and the venv itself are NOT worth backing up (59 GB of re-downloadable
   assets; a venv rebuilds in minutes).

## Recovery procedure

### A. Venv lost/corrupted *(verify on WSL)*
```bash
python3 -m venv /home/ai/.venv-wsl-whisper
/home/ai/.venv-wsl-whisper/bin/pip install -r /mnt/c/<repo>/cortex-speech-app/scripts/wsl7b_requirements.lock
```
Torch must see the GPUs:
`/home/ai/.venv-wsl-whisper/bin/python -c "import torch; print(torch.cuda.is_available())"` → `True`.
If not, install the CUDA-enabled torch wheel matching the driver.

### B. fairseq2 cache lost *(verify on WSL)*
Nothing to restore by hand — fairseq2 re-downloads model assets into `~/.cache/fairseq2` on first
load. Expect the first server start to take hours on a fresh cache; subsequent starts are normal.

### C. Adapter weights lost
Restore `OmniASR_7B_Champion/` from the off-machine backup (priority 1 above). If no backup exists
and the original training run is gone, the champion is LOST — a full retrain (RETRAIN_RUNBOOK.md)
is the only path back. This is why backup priority 1 exists.

### D. Whole WSL distro lost *(verify on WSL)*
Reinstall the distro, then A + B (C only if the Windows-side export folder was also lost —
it usually survives, since it lives on the Windows filesystem).

## Smoke test (after ANY recovery)

1. Start the server the way the app does: `powershell -File cortex-speech-app/scripts/start_7b_server.ps1`
   (it passes the app's own champion pointer; the server refuses to serve an unverified deployment)
2. From Windows, the app's client must transcribe: the WSL preflight in the app (or
   `python scripts/cortex_7b_client.py` with a test clip) returns a non-empty Sorani transcript.
3. In-app: import one small file with the WSL 7B engine selected — the import must NOT fall back
   (the F2 loud-downgrade warning must not appear).

## Champion pointer integration (P5.2)

The app writes `champion.json` into its data dir at every startup (id + checkpoint path + SHA per
family). The server currently hardcodes `ADAPTER = f"{EXPORT}/adapter_weights"`; the documented
one-line change (RETRAIN_RUNBOOK step 7) makes it read the pointer with a fallback — after which a
promotion swaps the serving adapter on next server start. Until that change lands, recovery only
needs the hardcoded path to exist again.
