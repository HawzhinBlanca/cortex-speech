"""The trainer adapter and the challenger wrapper must agree on the evidence contract.

Gate D had no compatible trainer at all: `train_challenger.py` hands an external trainer
`--manifest --out --base --contract` and demands hashed consumption evidence back, while the real
trainer (`train_omni_7b.py`) speaks `--train_jsonl/--output_dir` and emits none of it.
`omni7b_trainer_adapter.py` bridges them.

A bridge is only worth as much as the proof that both ends still line up. The `edit`-rejection defect
(exporter wrote `decision="edit"`, validator admitted only `"accept"`) was exactly this shape: two
halves of one contract drifting apart with no gate watching the seam. So this drives the adapter's
real evidence builder and hands the result straight to the wrapper's real verifier -- no fixtures
hand-written to match, because a hand-written fixture would drift with the code it is meant to catch.

Regression guard: 2026-08-18.
"""

from __future__ import annotations

import hashlib
import json
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "scripts"))

import omni7b_trainer_adapter as adapter  # noqa: E402
import train_challenger as wrapper  # noqa: E402


def _contract(tmp: Path) -> tuple[dict, str]:
    """A contract shaped exactly as `train_challenger.main` writes one, hashed the same way.

    The base is a REAL file: `verify_deployment_manifest` re-opens it, so a placeholder path would
    make this pass for the wrong reason.
    """
    base = tmp / "base_checkpoint.pt"
    base.write_bytes(b"\x00omniasr-7b-base")
    contract = {
        "schema": 1,
        "snapshot": {"id": "a" * 64, "root_sha256": "b" * 64},
        "train_manifest": {
            "path": "training_input/train_manifest.jsonl",
            "sha256": "c" * 64,
            "rows": 403,
            "audio_files_count": 403,
            "input_root_sha256": "d" * 64,
        },
        "base": {
            "id": "omniasr-7b-v2",
            "path": str(base),
            "sha256": wrapper.sha256_of(base),
            "size_bytes": base.stat().st_size,
        },
        "model_card": adapter.OMNIASR_CARD,
        "required_outputs": [wrapper.TRAINER_EVIDENCE_NAME, wrapper.ARTIFACT_MANIFEST_NAME],
    }
    path = tmp / wrapper.CONTRACT_NAME
    wrapper.atomic_write_json(path, contract)
    return contract, wrapper.sha256_of(path)


def _artifacts(out_dir: Path) -> list[dict]:
    """Real files on disk, because the wrapper re-hashes every declared artifact."""
    peft = out_dir / adapter.MODEL_DIR / "final_model"
    peft.mkdir(parents=True, exist_ok=True)
    written = []
    for role, name, payload in (
        ("adapter", "adapter_model.safetensors", b"\x00lora-weights"),
        ("adapter_config", "adapter_config.json", b'{"r":16}'),
        ("tokenizer", "tokenizer.model", b"\x00tokenizer"),
    ):
        path = peft / name
        path.write_bytes(payload)
        written.append(
            {
                "role": role,
                "path": path.relative_to(out_dir).as_posix(),
                "sha256": hashlib.sha256(payload).hexdigest(),
                "size_bytes": len(payload),
            }
        )
    return written


def _emit(tmp: Path, **overrides):
    out_dir = tmp
    contract, contract_sha = _contract(out_dir)
    evidence, manifest = adapter.build_evidence(
        contract=contract,
        contract_sha256=contract_sha,
        rows_consumed=overrides.get("rows_consumed", contract["train_manifest"]["rows"]),
        artifacts=overrides.get("artifacts", _artifacts(out_dir)),
        trainer_identity="train_omni_7b.py fairseq2-peft-lora (sha256 deadbeefdeadbeef)",
        recipe_sha256="f" * 64,
        environment_sha256="1" * 64,
        seed=20260818,
        environment={"torch": "2.8.0+cu128"},
        hyperparameters={"epochs": 1},
    )
    wrapper.atomic_write_json(out_dir / wrapper.TRAINER_EVIDENCE_NAME, evidence)
    wrapper.atomic_write_json(out_dir / wrapper.ARTIFACT_MANIFEST_NAME, manifest)
    return out_dir, contract, contract_sha


def test_adapter_evidence_satisfies_the_wrapper() -> None:
    """The whole point: what the adapter writes, the wrapper accepts."""
    with tempfile.TemporaryDirectory() as raw:
        out_dir, contract, contract_sha = _emit(Path(raw))
        problems, outputs = wrapper.verify_trainer_outputs(out_dir, contract, contract_sha)
        assert problems == [], f"the adapter's own evidence must satisfy the wrapper: {problems}"
        for role in ("adapter", "tokenizer", "adapter_config"):
            assert role in outputs["roles"], f"missing required {role!r} role"


def test_a_deployment_manifest_can_be_built_from_it() -> None:
    """The wrapper's LAST step before 'trained' -- it must find every serving component."""
    with tempfile.TemporaryDirectory() as raw:
        out_dir, contract, contract_sha = _emit(Path(raw))
        _, outputs = wrapper.verify_trainer_outputs(out_dir, contract, contract_sha)
        deployment = wrapper.build_deployment_manifest(contract, contract_sha, outputs, "adapter")
        wrapper.atomic_write_canonical_json(out_dir / wrapper.DEPLOYMENT_MANIFEST_NAME, deployment)
        problems = wrapper.verify_deployment_manifest(out_dir / wrapper.DEPLOYMENT_MANIFEST_NAME, out_dir)
        assert problems == [], f"deployment manifest must verify: {problems}"
        assert deployment["family"] == "omniasr-7b"
        assert deployment["model_card"] == adapter.OMNIASR_CARD, "the model lock must survive into serving"


def test_a_row_count_the_trainer_did_not_report_is_refused() -> None:
    """The adapter attests the TRAINER'S count. If it drifts from the contract, the wrapper must
    catch it -- otherwise a trainer could silently skip rows and still be called trained."""
    with tempfile.TemporaryDirectory() as raw:
        out_dir, contract, contract_sha = _emit(Path(raw), rows_consumed=402)
        problems, _ = wrapper.verify_trainer_outputs(out_dir, contract, contract_sha)
        assert any("train_rows_consumed" in p for p in problems), problems


def test_evidence_files_cannot_be_passed_off_as_model_artifacts() -> None:
    """A trainer that declares its own evidence as the model has produced nothing."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        fake = [
            {
                "role": "adapter",
                "path": wrapper.TRAINER_EVIDENCE_NAME,
                "sha256": "0" * 64,
                "size_bytes": 1,
            }
        ]
        out_dir, contract, contract_sha = _emit(tmp, artifacts=fake)
        problems, _ = wrapper.verify_trainer_outputs(out_dir, contract, contract_sha)
        assert any("evidence file, not a model artifact" in p for p in problems), problems


def test_the_adapter_refuses_a_contract_for_another_model_family() -> None:
    """The owner's model lock, enforced at the bridge: only the pinned OmniASR-7B card may train."""
    source = (REPO_ROOT / "scripts" / "omni7b_trainer_adapter.py").read_text(encoding="utf-8")
    assert 'contract.get("model_card") != OMNIASR_CARD' in source, (
        "the adapter must refuse any contract that does not pin the champion family card"
    )
    assert adapter.OMNIASR_CARD == wrapper.OMNIASR_MODEL_CARD, (
        "adapter and wrapper must pin the SAME champion card, or the lock has a seam"
    )


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"TRAINER ADAPTER CONTRACT: {len(tests)} pins")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
