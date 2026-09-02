#!/usr/bin/env python3
"""Gate D: snapshot -> trained artifact -> verdict must be complete and byte-linked."""

from __future__ import annotations

import json
import math
from pathlib import Path, PurePosixPath
from typing import Any

from train_challenger import (
    inspect_pack,
    is_sha256,
    sha256_of,
    verify_base_checkpoint,
    verify_deployment_manifest,
    verify_materialized_train_input,
    verify_trainer_outputs,
)

REPO_ROOT = Path(__file__).resolve().parents[1]
RUNS_DIR = REPO_ROOT.parent / "runs"
REQUIRED_SCRIPTS = [
    ("train_challenger.py", "sealed snapshot to verified model artifact"),
    ("build_eval_slices.py", "protected evaluation groups"),
    ("promotion_gate.py", "measured verdict"),
]


def _run_file(run_dir: Path, metadata: object, label: str, problems: list[str]) -> Path | None:
    if not isinstance(metadata, dict):
        problems.append(f"no {label} metadata")
        return None
    raw = metadata.get("path")
    if not isinstance(raw, str) or not raw:
        problems.append(f"{label} has no path")
        return None
    rel = PurePosixPath(raw)
    if (
        raw != rel.as_posix()
        or "\\" in raw
        or rel.is_absolute()
        or any(part in {"", ".", ".."} or ":" in part for part in rel.parts)
    ):
        problems.append(f"{label} path is not canonical and relative")
        return None
    try:
        root = run_dir.resolve(strict=True)
        path = root.joinpath(*rel.parts).resolve(strict=True)
        path.relative_to(root)
    except (FileNotFoundError, OSError, RuntimeError, ValueError):
        problems.append(f"{label} file is missing or escapes run directory")
        return None
    if not path.is_file():
        problems.append(f"{label} path is not a regular file")
        return None
    digest = metadata.get("sha256")
    if not is_sha256(digest):
        problems.append(f"{label} has no canonical sha256")
    elif sha256_of(path) != digest:
        problems.append(f"{label} file hash differs from run record")
    return path


def audit_run(record: dict[str, Any], run_dir: Path | None = None) -> list[str]:
    """Find every way a run can claim training without reproducible bytes."""
    problems: list[str] = []
    status = record.get("status")
    if status not in {"prepared", "trained", "failed"}:
        problems.append(f"unknown status {status!r}")
    snapshot_id = record.get("snapshot_id")
    if not is_sha256(snapshot_id):
        problems.append("no snapshot_id in canonical sha256 form — nothing can prove what this run learned from")
    if status == "prepared" and record.get("trainer_exit_code") == 0:
        problems.append("status 'prepared' but trainer reported success — one of the two is wrong")
    if status != "trained":
        return problems

    if record.get("schema") != 2:
        problems.append("trained run is not evidence schema 2")
    if not record.get("trainer_command"):
        problems.append("status 'trained' with no trainer_command — training could not have happened")
    if record.get("trainer_exit_code") != 0:
        problems.append(f"status 'trained' with trainer exit {record.get('trainer_exit_code')!r}")
    for key in ("snapshot_root_sha256", "base_sha256", "artifact_manifest_sha256", "training_run_id"):
        if not is_sha256(record.get(key)):
            problems.append(f"trained run has no canonical {key}")
    if not isinstance(record.get("base_id"), str) or not record["base_id"]:
        problems.append("trained run has no exact base_id")
    if not isinstance(record.get("model_id"), str) or not record["model_id"]:
        problems.append("trained run has no model_id")
    for key in ("train_manifest", "training_contract", "trainer_evidence", "artifact_manifest", "deployment_manifest"):
        if not isinstance(record.get(key), dict):
            problems.append(f"trained run has no {key} evidence")
    if not isinstance(record.get("artifacts"), list) or not record["artifacts"]:
        problems.append("trained run declares no real artifacts")
    if run_dir is None:
        return problems

    expected_train_bytes: bytes | None = None
    pack_evidence: dict[str, Any] | None = None
    pack_dir = record.get("pack_dir")
    if not isinstance(pack_dir, str) or not Path(pack_dir).is_absolute():
        problems.append("trained run pack_dir is not an absolute recorded root")
    elif is_sha256(snapshot_id):
        pack_problems, pack = inspect_pack(
            Path(pack_dir),
            snapshot_id,
            {
                "name": "finetune-pack",
                "status": "sealed",
                "config": record.get("snapshot_selection"),
            },
        )
        problems.extend(f"snapshot root: {problem}" for problem in pack_problems)
        if pack.get("snapshot_root_sha256") != record.get("snapshot_root_sha256"):
            problems.append("snapshot root hash does not match run record")
        if not pack_problems:
            pack_evidence = pack
            expected_train_bytes = ("\n".join(pack["train_lines"]) + "\n").encode("utf-8")

    base_problems, base = verify_base_checkpoint(
        str(record.get("base_checkpoint") or ""),
        str(record.get("base_id") or ""),
        str(record.get("base_sha256") or ""),
    )
    problems.extend(f"base: {problem}" for problem in base_problems)

    train_path = _run_file(run_dir, record.get("train_manifest"), "train_manifest", problems)
    if isinstance(record.get("train_manifest"), dict):
        problems.extend(
            f"train input: {problem}"
            for problem in verify_materialized_train_input(run_dir, record["train_manifest"], pack_evidence)
        )
    if train_path is not None:
        if expected_train_bytes is not None and train_path.read_bytes() != expected_train_bytes:
            problems.append("materialized train_manifest is not the exact train split of the sealed snapshot")
        try:
            rows = [json.loads(line) for line in train_path.read_text(encoding="utf-8").splitlines() if line]
        except (OSError, UnicodeError, ValueError) as error:
            problems.append(f"train_manifest is unreadable: {error}")
            rows = []
        if not rows:
            problems.append("materialized train_manifest has zero rows")
        elif any(not isinstance(row, dict) or row.get("split") != "train" for row in rows):
            problems.append("materialized trainer input contains validation/test or malformed rows")
        expected_rows = record.get("train_manifest", {}).get("rows")
        if expected_rows != len(rows):
            problems.append(f"train_manifest row count changed ({len(rows)} != {expected_rows!r})")

    contract_path = _run_file(run_dir, record.get("training_contract"), "training_contract", problems)
    contract: dict[str, Any] | None = None
    if contract_path is not None:
        try:
            loaded = json.loads(contract_path.read_text(encoding="utf-8"))
            if not isinstance(loaded, dict):
                raise ValueError("top level is not an object")
            contract = loaded
        except (OSError, UnicodeError, ValueError) as error:
            problems.append(f"training_contract is unreadable: {error}")
    if contract is not None:
        if contract.get("schema") != 1:
            problems.append("training_contract schema is not 1")
        contract_sha = sha256_of(contract_path)
        if contract.get("snapshot", {}).get("id") != snapshot_id:
            problems.append("training_contract snapshot id differs from run")
        if contract.get("snapshot", {}).get("root_sha256") != record.get("snapshot_root_sha256"):
            problems.append("training_contract snapshot root differs from run")
        if contract.get("base", {}).get("sha256") != record.get("base_sha256"):
            problems.append("training_contract base differs from run")
        if contract.get("train_manifest") != record.get("train_manifest"):
            problems.append("training_contract train manifest differs from run")
        if base and contract.get("base", {}).get("path") != base.get("path"):
            problems.append("training_contract base path is not the canonical base")
        output_problems, outputs = verify_trainer_outputs(run_dir, contract, contract_sha)
        problems.extend(f"trainer output: {problem}" for problem in output_problems)
        if outputs.get("trainer_evidence_sha256") != record.get("trainer_evidence", {}).get("sha256"):
            problems.append("trainer evidence hash differs from run record")
        if outputs.get("artifact_manifest_sha256") != record.get("artifact_manifest_sha256"):
            problems.append("artifact manifest hash differs from run record")
        if outputs.get("artifacts") != record.get("artifacts"):
            problems.append("verified artifact set differs from run record")

    _run_file(run_dir, record.get("trainer_evidence"), "trainer_evidence", problems)
    _run_file(run_dir, record.get("artifact_manifest"), "artifact_manifest", problems)
    deployment_path = _run_file(run_dir, record.get("deployment_manifest"), "deployment_manifest", problems)
    if deployment_path is not None:
        problems.extend(verify_deployment_manifest(deployment_path, run_dir))
        try:
            deployment = json.loads(deployment_path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, ValueError):
            deployment = {}
        provenance = deployment.get("provenance", {})
        if provenance.get("snapshot", {}).get("id") != snapshot_id:
            problems.append("deployment manifest snapshot id differs from run")
        if provenance.get("snapshot", {}).get("root_sha256") != record.get("snapshot_root_sha256"):
            problems.append("deployment manifest snapshot root differs from run")
        if provenance.get("training_run_id") != record.get("training_run_id"):
            problems.append("deployment manifest training_run_id differs from run")
        if deployment.get("model_id") != record.get("model_id"):
            problems.append("deployment manifest model_id differs from run")
        if deployment.get("manifestSha256") != record.get("deployment_manifest", {}).get("manifestSha256"):
            problems.append("deployment manifest canonical self-hash differs from run record")
        source = provenance.get("source_evidence", {})
        if source.get("training_contract_sha256") != record.get("training_contract", {}).get("sha256"):
            problems.append("deployment source does not link the training contract")
        if source.get("trainer_evidence_sha256") != record.get("trainer_evidence", {}).get("sha256"):
            problems.append("deployment source does not link trainer evidence")
        if source.get("artifact_manifest_sha256") != record.get("artifact_manifest_sha256"):
            problems.append("deployment source does not link artifact manifest")
        if provenance.get("trainer", {}).get("command") != record.get("trainer_command"):
            problems.append("deployment trainer command differs from run")
    return problems


def audit_verdict(verdict: dict[str, Any]) -> list[str]:
    """Check the verdict's own measurement claims; chain linkage is checked in main."""
    problems: list[str] = []
    decision = verdict.get("verdict")
    if decision not in {"PROMOTE", "REJECT", "INVALID"}:
        return [f"unknown verdict {decision!r}"]
    if decision == "INVALID":
        return ["INVALID is not a completed measured verdict"]
    champion, challenger = verdict.get("champion_cer"), verdict.get("challenger_cer")
    valid_number = lambda value: (
        isinstance(value, (int, float))
        and not isinstance(value, bool)
        and math.isfinite(float(value))
        and value >= 0
    )
    if not valid_number(champion) or not valid_number(challenger):
        problems.append(f"{decision} with no finite measured CER on one side")
    elif decision == "PROMOTE" and challenger >= champion:
        problems.append(f"PROMOTE while challenger is not better ({champion} -> {challenger})")
    paired = verdict.get("paired_clips")
    if not isinstance(paired, int) or isinstance(paired, bool) or paired <= 0:
        problems.append(f"{decision} with no paired clips (positive paired_clips required) — nothing was compared")
    slices = verdict.get("slices", [])
    if not isinstance(slices, list):
        problems.append(f"{decision} slices are not a list")
        slices = []
    for entry in slices:
        if not isinstance(entry, dict):
            problems.append(f"{decision} has a malformed slice")
            continue
        before, after = entry.get("champion_cer"), entry.get("challenger_cer")
        if decision == "PROMOTE" and valid_number(before) and valid_number(after) and after > before:
            problems.append(f"PROMOTE while slice {entry.get('slice')!r} regressed ({before} -> {after})")
    return problems


def completion_reasons(trained: int, verdicts: int, linked: int, unverdicted: int) -> list[str]:
    reasons: list[str] = []
    if trained == 0:
        reasons.append("zero verified trained challengers")
    if verdicts == 0:
        reasons.append("zero measured verdicts")
    if trained and verdicts and linked == 0:
        reasons.append("no verdict is byte-linked to a trained challenger")
    if unverdicted:
        reasons.append(f"{unverdicted} trained challenger(s) have no linked verdict")
    return reasons


def main() -> int:
    scripts_dir = REPO_ROOT / "scripts"
    missing = [(name, why) for name, why in REQUIRED_SCRIPTS if not (scripts_dir / name).is_file()]
    if missing:
        print("CHALLENGER LOOP: FAIL", flush=True)
        for name, why in missing:
            print(f"  - scripts/{name} is missing — {why}", flush=True)
        return 1

    run_paths = sorted(RUNS_DIR.glob("challenger_*/challenger_run.json")) if RUNS_DIR.is_dir() else []
    verdict_paths = sorted(RUNS_DIR.glob("**/promotion_verdict.json")) if RUNS_DIR.is_dir() else []
    problems: list[str] = []
    trained_records: list[tuple[Path, dict[str, Any]]] = []
    for path in run_paths:
        try:
            record = json.loads(path.read_text(encoding="utf-8"))
            if not isinstance(record, dict):
                raise ValueError("top level is not an object")
        except (OSError, UnicodeError, ValueError) as error:
            problems.append(f"{path}: unreadable run record: {error}")
            continue
        run_problems = audit_run(record, path.parent)
        problems.extend(f"{path.parent.name}: {problem}" for problem in run_problems)
        if record.get("status") == "trained" and not run_problems:
            trained_records.append((path, record))

    verdict_records: list[tuple[Path, dict[str, Any]]] = []
    for path in verdict_paths:
        try:
            verdict = json.loads(path.read_text(encoding="utf-8"))
            if not isinstance(verdict, dict):
                raise ValueError("top level is not an object")
        except (OSError, UnicodeError, ValueError) as error:
            problems.append(f"{path}: unreadable verdict: {error}")
            continue
        problems.extend(f"{path.parent.name}: {problem}" for problem in audit_verdict(verdict))
        verdict_records.append((path, verdict))

    linked_run_ids: set[str] = set()
    for path, verdict in verdict_records:
        snapshot_id = verdict.get("snapshot_id")
        artifact_hash = verdict.get("challenger_artifact_manifest_sha256")
        deployment_hash = verdict.get("challenger_deployment_manifest_sha256")
        matches = [
            record
            for _, record in trained_records
            if record.get("snapshot_id") == snapshot_id
            and record.get("artifact_manifest_sha256") == artifact_hash
            and record.get("deployment_manifest", {}).get("sha256") == deployment_hash
        ]
        if not is_sha256(snapshot_id):
            problems.append(f"{path.parent.name}: verdict has no canonical snapshot_id")
        if not is_sha256(artifact_hash):
            problems.append(f"{path.parent.name}: verdict has no challenger_artifact_manifest_sha256")
        if not is_sha256(deployment_hash):
            problems.append(f"{path.parent.name}: verdict has no challenger_deployment_manifest_sha256")
        if len(matches) != 1:
            problems.append(f"{path.parent.name}: verdict does not identify exactly one verified trained artifact")
        else:
            linked_run_ids.add(matches[0]["training_run_id"])

    if problems:
        print("CHALLENGER LOOP: FAIL", flush=True)
        for problem in problems:
            print(f"  - {problem}", flush=True)
        return 1
    incomplete = completion_reasons(
        len(trained_records),
        len(verdict_records),
        len(linked_run_ids),
        len(trained_records) - len(linked_run_ids),
    )
    if incomplete:
        print("CHALLENGER LOOP: INCOMPLETE", flush=True)
        for reason in incomplete:
            print(f"  - {reason}", flush=True)
        return 1
    print(
        f"CHALLENGER LOOP: OK ({len(trained_records)} verified train(s), "
        f"{len(verdict_records)} byte-linked verdict(s))",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
