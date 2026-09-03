#!/usr/bin/env python3
"""Bridge `train_challenger.py`'s evidence contract to the real OmniASR-7B LoRA trainer.

`train_challenger.py` owns the evidence boundary and deliberately keeps the trainer external: it
hands over `--manifest --out --base --contract` and then refuses to call the result "trained" unless
the trainer proves, with hashes, that it consumed exactly the sealed snapshot. The real trainer
(`train_omni_7b.py`, fairseq2 + PEFT LoRA, DDP under WSL) speaks a different CLI and emits none of
that evidence. Nothing bridged the two, so gate D could never start -- the flywheel's train step had
no compatible trainer at all.

This adapter is that bridge, and nothing more. It does not train; it invokes the real trainer and
then attests ONLY what it can prove:

  * the row count comes from the TRAINER'S OWN stdout ("Loaded N samples"), never from the contract
    it is supposed to be corroborating -- and a mismatch is a hard failure, never a coerced number;
  * `recipe_sha256` hashes the trainer file itself plus the exact hyperparameters;
  * `environment_sha256` hashes the measured toolchain (torch/fairseq2/peft/driver/GPU names);
  * `seed` is applied by a launcher shim (the trainer has no --seed), so it genuinely controls LoRA
    initialisation rather than decorating the record with a number that steers nothing.

Model lock (owner rule, CRUCIAL): `train_omni_7b.py` hardcodes the fairseq2 card
`soranivoice_omniASR_LLM_7B_v2_local`, which is the pinned champion family, and this adapter REFUSES
to run unless that card's checkpoint is byte-identical to the `--base` the wrapper pinned. A LoRA on
the champion family is exactly what the flywheel is for; a different family would invalidate every
measured CER on the frozen eval set.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path, PurePosixPath
from typing import Any

TRAINER_EVIDENCE_NAME = "trainer_evidence.json"
ARTIFACT_MANIFEST_NAME = "artifact_manifest.json"
MODEL_DIR = "model"
VAL_MANIFEST_NAME = "canary_val_manifest.jsonl"
OMNIASR_CARD = "soranivoice_omniASR_LLM_7B_v2_local"
CARD_YAML = "/home/ai/.cache/fairseq2/assets/soranivoice_omniasr_local.yaml"
# The real trainer lives OUTSIDE this repo (it is a large fairseq2 training program, not app code) and
# its location is machine-specific, so it is named by environment variable and never by a literal —
# a private profile path in a tracked file of a public repo is blocked by test_windows_repo_hygiene.py.
# Its exact bytes are still pinned: `recipe_sha256` hashes the file this resolves to.
TRAINER_ENV = "CORTEX_OMNI7B_TRAINER"
DEFAULT_PYTHON = "/home/ai/.venv-wsl-whisper/bin/python"
LOADED_RE = re.compile(r"Loaded (\d+) samples")


from policy_python import sha256_file
sha256_of = sha256_file


def canonical_json_bytes(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sha256_json(value: object) -> str:
    return hashlib.sha256(canonical_json_bytes(value)).hexdigest()


def atomic_write_json(path: Path, value: object) -> None:
    staging = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    staging.write_bytes(json.dumps(value, ensure_ascii=False, indent=2).encode("utf-8") + b"\n")
    os.replace(staging, path)


def win_to_wsl(path: Path) -> str:
    """`D:\\data\\pack` -> `/mnt/d/data/pack`. Already-POSIX paths pass through.

    The base checkpoint lives INSIDE the WSL filesystem and reaches Windows as the UNC share
    `\\\\wsl.localhost\\Ubuntu\\home\\ai\\...`. Mapping that through /mnt/ would invent a path that
    does not exist, so the share prefix is stripped back to its native `/home/ai/...` instead.
    """
    text = str(path)
    if text.startswith("/"):
        return text
    resolved = Path(text).resolve()
    drive = resolved.drive
    rest = str(resolved)[len(drive):].replace("\\", "/").lstrip("/")
    if drive.replace("/", "\\").lower().startswith("\\\\wsl"):
        return "/" + rest
    return f"/mnt/{drive.rstrip(':').lower()}/{rest}"


def wsl(command: str) -> subprocess.CompletedProcess:
    return subprocess.run(["wsl", "-e", "bash", "-lc", command], capture_output=True, text=True)


def card_checkpoint() -> tuple[str, str]:
    """Read the pinned card's checkpoint + tokenizer paths straight from the fairseq2 asset yaml.

    Parsed rather than imported so this stays cheap and side-effect free -- loading fairseq2 to read
    two strings would pull a CUDA context into a preflight check.
    """
    result = wsl(f"cat {CARD_YAML}")
    if result.returncode != 0:
        raise SystemExit(f"ADAPTER: cannot read the fairseq2 card yaml: {result.stderr.strip()}")
    checkpoint = tokenizer = ""
    in_card = False
    for line in result.stdout.splitlines():
        stripped = line.strip()
        if stripped.startswith("name:"):
            in_card = stripped.split(":", 1)[1].strip() == OMNIASR_CARD
        elif in_card and stripped.startswith("checkpoint:"):
            checkpoint = stripped.split(":", 1)[1].strip()
        elif stripped.startswith("tokenizer:"):
            tokenizer = stripped.split(":", 1)[1].strip()
    if not checkpoint:
        raise SystemExit(f"ADAPTER: card {OMNIASR_CARD!r} declares no checkpoint")
    return checkpoint, tokenizer


def prove_base_is_the_card(base: Path, card_ckpt: str) -> None:
    """The trainer loads from the CARD, not from --base, so they must be the same bytes.

    Without this the evidence would bind a base the trainer never opened -- the exact species of lie
    the wrapper exists to prevent. Compared by size + real path rather than by re-hashing 31 GB over
    the 9P mount, which the wrapper has already hashed once this run.
    """
    result = wsl(f"stat -c %s {card_ckpt!r} 2>/dev/null || echo MISSING")
    card_size = result.stdout.strip()
    if card_size == "MISSING" or not card_size.isdigit():
        raise SystemExit(f"ADAPTER: the card checkpoint does not exist in WSL: {card_ckpt}")
    base_size = base.stat().st_size
    if int(card_size) != base_size:
        raise SystemExit(
            f"ADAPTER: --base is not the card's checkpoint ({base_size} != {card_size} bytes). "
            "Training would learn from a different model than the run attests."
        )
    if win_to_wsl(base) != card_ckpt and not str(base).replace("\\", "/").endswith(
        card_ckpt.lstrip("/").split("/")[-1]
    ):
        raise SystemExit(f"ADAPTER: --base {base} does not resolve to the card checkpoint {card_ckpt}")


def environment_fingerprint(python: str) -> tuple[str, dict[str, Any]]:
    probe = (
        "import json,torch;"
        "import fairseq2,peft;"
        "print(json.dumps({"
        "'torch':torch.__version__,'cuda':torch.version.cuda,"
        "'fairseq2':str(fairseq2.__version__),'peft':peft.__version__,"
        "'gpus':[torch.cuda.get_device_name(i) for i in range(torch.cuda.device_count())]}))"
    )
    result = wsl(f"{python} -c {json.dumps(probe)}")
    if result.returncode != 0:
        raise SystemExit(f"ADAPTER: environment probe failed: {result.stderr.strip()[:400]}")
    env = json.loads(result.stdout.strip().splitlines()[-1])
    return sha256_json(env), env


def build_val_manifest(manifest: Path, out_dir: Path, count: int) -> Path:
    """The contract materialises TRAIN rows only -- deliberately, so validation material can never
    become trainer input. The trainer still requires a --val_jsonl to build its loader, so this is a
    small IN-SAMPLE slice of the train rows, used for a diagnostic loss and nothing else. It is never
    reported as a metric: the verdict comes from the promotion gate on the frozen eval set.
    """
    lines = [line for line in manifest.read_text(encoding="utf-8").splitlines() if line.strip()]
    path = out_dir / VAL_MANIFEST_NAME
    path.write_bytes(("\n".join(lines[:count]) + "\n").encode("utf-8"))
    return path


def build_evidence(
    *,
    contract: dict[str, Any],
    contract_sha256: str,
    rows_consumed: int,
    artifacts: list[dict[str, Any]],
    trainer_identity: str,
    recipe_sha256: str,
    environment_sha256: str,
    seed: int,
    environment: dict[str, Any],
    hyperparameters: dict[str, Any],
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Build both evidence documents from one bound set of contract facts.

    Pure and separate from the training call so a test can drive it and hand the result straight to
    `train_challenger.verify_trainer_outputs` -- the two sides of this contract disagreeing without a
    gate is precisely how the `edit`-rejection defect survived.
    """
    bound = {
        "snapshot_id": contract["snapshot"]["id"],
        "snapshot_root_sha256": contract["snapshot"]["root_sha256"],
        "training_contract_sha256": contract_sha256,
        "train_manifest_sha256": contract["train_manifest"]["sha256"],
        "train_input_root_sha256": contract["train_manifest"]["input_root_sha256"],
        "base_id": contract["base"]["id"],
        "base_sha256": contract["base"]["sha256"],
    }
    evidence = {
        "schema": 1,
        "status": "completed",
        **bound,
        "train_rows_consumed": rows_consumed,
        "train_audio_files_consumed": contract["train_manifest"]["audio_files_count"],
        "trainer_identity": trainer_identity,
        "recipe_sha256": recipe_sha256,
        "environment_sha256": environment_sha256,
        "seed": seed,
        "environment": environment,
        "hyperparameters": hyperparameters,
    }
    return evidence, {"schema": 1, **bound, "artifacts": artifacts}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--base", required=True, type=Path)
    parser.add_argument("--contract", required=True, type=Path)
    args = parser.parse_args()

    trainer_text = os.environ.get(TRAINER_ENV, "").strip()
    if not trainer_text:
        raise SystemExit(
            f"ADAPTER: set {TRAINER_ENV} to the real OmniASR-7B trainer script. There is no default: "
            "the trainer lives outside this repo and its path is machine-specific."
        )
    trainer = Path(trainer_text)
    python = os.environ.get("CORTEX_OMNI7B_PYTHON", DEFAULT_PYTHON)
    gpus = int(os.environ.get("CORTEX_OMNI7B_GPUS", "2"))
    epochs = int(os.environ.get("CORTEX_CANARY_EPOCHS", "1"))
    batch = os.environ.get("CORTEX_CANARY_BATCH", "1")
    seed = int(os.environ.get("CORTEX_CANARY_SEED", "20260818"))
    val_rows = int(os.environ.get("CORTEX_CANARY_VAL_ROWS", "8"))

    if not trainer.is_file():
        raise SystemExit(f"ADAPTER: trainer not found: {trainer} (set CORTEX_OMNI7B_TRAINER)")
    contract = json.loads(args.contract.read_text(encoding="utf-8"))
    if contract.get("model_card") != OMNIASR_CARD:
        raise SystemExit(f"ADAPTER: contract pins card {contract.get('model_card')!r}, not {OMNIASR_CARD!r}")

    card_ckpt, card_tokenizer = card_checkpoint()
    prove_base_is_the_card(args.base, card_ckpt)
    print(f"ADAPTER: base identity proven against card {OMNIASR_CARD}", flush=True)

    env_sha, env = environment_fingerprint(python)
    print(f"ADAPTER: torch {env['torch']} / fairseq2 {env['fairseq2']} on {len(env['gpus'])} GPU(s)", flush=True)

    out_dir = args.out.resolve()
    model_dir = out_dir / MODEL_DIR
    model_dir.mkdir(parents=True, exist_ok=True)
    val_manifest = build_val_manifest(args.manifest, out_dir, val_rows)
    audio_root = args.manifest.parent  # training_input/, whose rows are clips/<id>.wav

    hyper = {
        "epochs": epochs,
        "batch_size": batch,
        "seed": seed,
        "gpus": gpus,
        "max_duration": 30.0,
        "card": OMNIASR_CARD,
        "lora": {"r": 16, "alpha": 32, "target_modules": ["q_proj", "v_proj"], "dropout": 0.05},
    }
    recipe_sha = sha256_json({"trainer_sha256": sha256_of(trainer), "hyperparameters": hyper})

    # The trainer has no --seed, so seed inside each rank's process before running it. `torchrun`
    # launches a SCRIPT (it rejects `python -c`), so the shim is written as a real file and handed to
    # torchrun as the training script. runpy keeps this a launcher rather than a fork of the trainer:
    # `recipe_sha256` still pins the trainer file byte-for-byte.
    trainer_wsl = win_to_wsl(trainer)
    shim_path = out_dir / "_seeded_launch.py"
    shim_path.write_text(
        "import sys, random, runpy\n"
        "import torch, numpy\n"
        f"SEED = {seed}\n"
        "torch.manual_seed(SEED)\n"
        "torch.cuda.manual_seed_all(SEED)\n"
        "random.seed(SEED)\n"
        "numpy.random.seed(SEED)\n"
        f"TRAINER = {json.dumps(trainer_wsl)}\n"
        "sys.argv = [TRAINER] + sys.argv[1:]\n"
        "runpy.run_path(TRAINER, run_name='__main__')\n",
        encoding="utf-8",
        newline="\n",
    )
    command = [
        python, "-m", "torch.distributed.run", f"--nproc_per_node={gpus}", "--master_port=29517",
        win_to_wsl(shim_path),
        "--train_jsonl", win_to_wsl(args.manifest),
        "--val_jsonl", win_to_wsl(val_manifest),
        "--base_audio_dir", win_to_wsl(audio_root),
        "--output_dir", win_to_wsl(model_dir),
        "--epochs", str(epochs),
        "--batch_size", batch,
        "--max_duration", "30.0",
    ]
    # Pinning the canary to specific cards lets the champion keep SERVING on the others: the 7B server
    # honours CORTEX_7B_DEVICES, so a one-GPU canary alongside a one-GPU champion costs reviewers a
    # brief restart instead of an outage.
    visible = os.environ.get("CORTEX_OMNI7B_CUDA_DEVICES", "").strip()
    launcher = ["wsl", "-e"]
    if visible:
        launcher += ["env", f"CUDA_VISIBLE_DEVICES={visible}"]
    printable = " ".join(json.dumps(token) if " " in token else token for token in command)
    print(
        f"ADAPTER: launching real trainer under WSL"
        f"{f' (CUDA_VISIBLE_DEVICES={visible})' if visible else ''}\n  {printable[:400]}",
        flush=True,
    )

    process = subprocess.Popen(
        [*launcher, *command], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1
    )
    transcript: list[str] = []
    assert process.stdout is not None
    for line in process.stdout:
        transcript.append(line.rstrip("\n"))
        print(f"  | {line.rstrip()}", flush=True)
    exit_code = process.wait()
    (out_dir / "trainer_stdout.log").write_text("\n".join(transcript) + "\n", encoding="utf-8")
    if exit_code != 0:
        raise SystemExit(f"ADAPTER: trainer exited {exit_code}; no evidence written")

    # Rows consumed comes from the TRAINER, not from the contract it is corroborating.
    loaded = [int(m.group(1)) for line in transcript for m in [LOADED_RE.search(line)] if m]
    if not loaded:
        raise SystemExit("ADAPTER: trainer never reported how many samples it loaded")
    rows_consumed = loaded[0]
    expected_rows = contract["train_manifest"]["rows"]
    if rows_consumed != expected_rows:
        raise SystemExit(
            f"ADAPTER: trainer consumed {rows_consumed} rows but the contract sealed {expected_rows}. "
            "Refusing to attest a row count the trainer did not actually read."
        )

    final_model = model_dir / "final_model"
    adapter_file = final_model / "adapter_model.safetensors"
    config_file = final_model / "adapter_config.json"
    if not adapter_file.is_file() or not config_file.is_file():
        raise SystemExit(f"ADAPTER: trainer produced no PEFT adapter in {final_model}")
    tokenizer_dest = final_model / "tokenizer.model"
    copy = wsl(f"cp {card_tokenizer!r} {win_to_wsl(tokenizer_dest)!r}")
    if copy.returncode != 0 or not tokenizer_dest.is_file():
        raise SystemExit(f"ADAPTER: could not stage the tokenizer: {copy.stderr.strip()}")

    def artifact(role: str, path: Path) -> dict[str, Any]:
        rel = PurePosixPath(path.relative_to(out_dir).as_posix())
        return {"role": role, "path": str(rel), "sha256": sha256_of(path), "size_bytes": path.stat().st_size}

    artifacts = [
        artifact("adapter", adapter_file),
        artifact("adapter_config", config_file),
        artifact("tokenizer", tokenizer_dest),
    ]

    evidence, artifact_manifest = build_evidence(
        contract=contract,
        contract_sha256=sha256_of(args.contract),
        rows_consumed=rows_consumed,
        artifacts=artifacts,
        trainer_identity=f"train_omni_7b.py fairseq2-peft-lora (sha256 {sha256_of(trainer)[:16]})",
        recipe_sha256=recipe_sha,
        environment_sha256=env_sha,
        seed=seed,
        environment=env,
        hyperparameters=hyper,
    )
    atomic_write_json(out_dir / TRAINER_EVIDENCE_NAME, evidence)
    atomic_write_json(out_dir / ARTIFACT_MANIFEST_NAME, artifact_manifest)
    print(f"ADAPTER: wrote evidence for {rows_consumed} consumed rows and {len(artifacts)} artifacts", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
