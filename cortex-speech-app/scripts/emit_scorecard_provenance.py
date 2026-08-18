#!/usr/bin/env python3
"""Emit the scorecard provenance sidecar `promotion_gate.py` requires.

The gate refuses any scorecard whose provenance is not cryptographically bound: "a plausible number
is not proof of which model produced it". Nothing generated that sidecar -- only the gate that
consumes it and the gate's own tests -- so a real verdict could not be produced even with both
scorecards measured. This is the missing emitter.

It never invents a binding. Every challenger field is derived by calling the GATE'S OWN
`load_challenger_run`, so the sidecar agrees with the re-verified run and deployment by construction
rather than by a human copying hashes; every champion field is read from the incumbent's canonical
deployment manifest and re-hashed from the files on disk. The scorecard hash and `norm_basis` are
read back out of the measured TSV, so a sidecar can never describe a different measurement than the
one it sits beside.

Usage::

    python scripts/emit_scorecard_provenance.py \
      --scorecard runs/champion_results.tsv --role champion \
      --deployment runs/incumbent/deployment_manifest.json \
      --challenger-run-record runs/challenger_canary_02/challenger_run.json \
      --eval-manifest docs/eval/gold.tsv
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "scripts"))

import promotion_gate as gate  # noqa: E402

SCHEMA_VERSION = 1


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def scorecard_facts(path: Path) -> tuple[str, str]:
    """Return (sha256, norm_basis) read back from the measured TSV itself."""
    text = path.read_text(encoding="utf-8")
    lines = [line for line in text.splitlines() if line.strip()]
    if len(lines) < 2:
        raise SystemExit(f"scorecard {path} has no data rows")
    header = lines[0].split("\t")
    if "norm_basis" not in header:
        raise SystemExit(f"scorecard {path} has no norm_basis column")
    index = header.index("norm_basis")
    bases = {line.split("\t")[index] for line in lines[1:]}
    if len(bases) != 1:
        raise SystemExit(f"scorecard {path} mixes norm_basis values {sorted(bases)}")
    basis = bases.pop()
    if not basis:
        raise SystemExit(f"scorecard {path} has an empty norm_basis")
    return sha256_of(path), basis


def champion_identities(deployment_path: Path) -> dict[str, Any]:
    """Read the incumbent's identities from its canonical manifest, re-hashing every component.

    Deliberately NOT trusting the manifest's stated hashes: the sidecar must describe the bytes that
    are actually on disk, which is the only thing the served model could have used.
    """
    deployment = json.loads(deployment_path.read_text(encoding="utf-8"))
    if deployment.get("family") != gate.CANONICAL_MODEL_FAMILY:
        raise SystemExit(f"incumbent deployment is not family {gate.CANONICAL_MODEL_FAMILY!r}")
    root = deployment_path.parent
    out: dict[str, Any] = {
        "model_family": deployment["family"],
        "model_id": deployment["model_id"],
        "model_sha256": deployment["manifestSha256"],
    }
    for field, component in (
        ("base_model", "base_checkpoint"),
        ("adapter", "adapter"),
        ("adapter_config", "adapter_config"),
        ("tokenizer", "tokenizer"),
    ):
        value = deployment.get(component)
        if not isinstance(value, dict):
            raise SystemExit(f"incumbent deployment has no {component} component")
        raw = Path(value["path"])
        path = raw if raw.is_absolute() else root / raw
        actual = sha256_of(path)
        if actual != value["sha256"]:
            raise SystemExit(f"incumbent {component} on disk does not match its manifest hash")
        out[f"{field}_id"] = value.get("id") if component == "base_checkpoint" else value["path"]
        out[f"{field}_sha256"] = actual
    return out


def challenger_identities(run_record: Path) -> dict[str, Any]:
    """Derive via the gate's own loader so the sidecar cannot disagree with what the gate re-verifies."""
    binding = gate.load_challenger_run(run_record)
    return {
        "model_family": binding.model_family,
        "model_id": binding.model_id,
        "model_sha256": binding.model_sha256,
        "base_model_id": binding.base_model_id,
        "base_model_sha256": binding.base_model_sha256,
        "adapter_id": binding.adapter_id,
        "adapter_sha256": binding.adapter_sha256,
        "adapter_config_id": binding.adapter_config_id,
        "adapter_config_sha256": binding.adapter_config_sha256,
        "tokenizer_id": binding.tokenizer_id,
        "tokenizer_sha256": binding.tokenizer_sha256,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scorecard", required=True, type=Path)
    parser.add_argument("--role", required=True, choices=["champion", "challenger"])
    parser.add_argument("--deployment", type=Path, help="incumbent deployment manifest (champion role)")
    parser.add_argument("--challenger-run-record", required=True, type=Path)
    parser.add_argument("--eval-manifest", required=True, type=Path)
    parser.add_argument(
        "--normalizer",
        type=Path,
        default=REPO_ROOT / "scripts" / "scorecard_7b.py",
        help="the code that produced norm_basis; hashed into normalizer_sha256",
    )
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()

    scorecard_sha, norm_basis = scorecard_facts(args.scorecard)
    eval_text = args.eval_manifest.read_text(encoding="utf-8")
    eval_rows = len([line for line in eval_text.splitlines() if line.strip()])

    # The challenger run is the shared context of BOTH sidecars: it names the snapshot this
    # comparison adjudicates and the exact trained artifacts under test.
    binding = gate.load_challenger_run(args.challenger_run_record)
    shared = {
        "snapshot_id": binding.snapshot_id,
        "training_run_id": binding.training_run_id,
        "challenger_artifact_manifest_sha256": binding.artifact_manifest_sha256,
        "challenger_deployment_manifest_sha256": binding.deployment_file_sha256,
    }

    if args.role == "challenger":
        identities = challenger_identities(args.challenger_run_record)
    else:
        if not args.deployment:
            raise SystemExit("--deployment (the incumbent's manifest) is required for the champion role")
        identities = champion_identities(args.deployment)

    provenance = {
        "schema_version": SCHEMA_VERSION,
        "role": args.role,
        "scorecard_sha256": scorecard_sha,
        "eval_manifest_sha256": sha256_of(args.eval_manifest),
        "eval_manifest_rows": eval_rows,
        **shared,
        **identities,
        "normalizer_id": f"{args.normalizer.name}:{norm_basis}",
        "normalizer_sha256": sha256_of(args.normalizer),
        "norm_basis": norm_basis,
    }
    missing = [field for field in gate._PROVENANCE_FIELDS if field not in provenance]
    if missing:
        raise SystemExit(f"emitted provenance is missing mandatory field(s) {missing}")

    out = args.out or args.scorecard.with_name(args.scorecard.name + ".provenance.json")
    out.write_bytes(json.dumps(provenance, ensure_ascii=False, indent=2).encode("utf-8") + b"\n")
    print(f"wrote {args.role} provenance -> {out}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
