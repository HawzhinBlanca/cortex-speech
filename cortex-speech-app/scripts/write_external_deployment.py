#!/usr/bin/env python3
"""Write the schema-2 FUSED deployment manifest for a model trained outside the Cortex flywheel.

The ``external_offline`` provenance kind (see ``cortex_7b_server.py`` / ``deployment.rs``) pins three
SHA-256s: the training manifest, the parent checkpoint the model was trained from, and an evidence
document. This tool computes every one of them -- and the checkpoint and tokenizer identities -- by
hashing the real files it is handed. It never accepts a hash on the command line, so nothing in the
manifest can be typed in rather than measured.

It writes two files into the manifest's directory:

* the canonical deployment manifest (``--out``), byte-for-byte the contract both verifiers accept;
* ``external_offline_record.json``: which file produced each pinned hash (path, size, sha256), so the
  owner can re-hash the same inputs later. The manifest alone carries only the hashes.

It registers nothing and promotes nothing. Registration is a separate, explicit step, and no
automatic path may promote an ``external_offline`` model.

Usage (run where the checkpoint bytes are, e.g. inside WSL)::

    python3 scripts/write_external_deployment.py \
      --checkpoint /home/ai/.../kurdish_omniasr_7b_v12_c750.pt \
      --tokenizer  /home/ai/cortex_v12_candidate/omniASR_tokenizer_written_v2.model \
      --parent-checkpoint /home/ai/.../kurdish_omniasr_7b_v11_champion.pt \
      --training-manifest /mnt/c/.../v12_train.jsonl \
      --evidence /mnt/c/.../release/kurdish-omniasr-7b-v12/SHA256SUMS \
      --out /home/ai/cortex_v12_candidate/deployment_manifest.json
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Sequence

import bootstrap_legacy_champion as legacy
from cortex_7b_server import external_offline_model_id

RECORD_NAME = "external_offline_record.json"


class ExternalDeploymentError(ValueError):
    """The requested manifest would not describe the given files exactly."""


def build(
    *,
    checkpoint: Path,
    tokenizer: Path,
    parent_checkpoint: Path,
    training_manifest: Path,
    evidence: Path,
    out: Path,
    checkpoint_id: str | None = None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Hash every input and return (manifest, record). Writes nothing."""
    if not out.name or out.name in (".", "..") or out.name == RECORD_NAME:
        raise ExternalDeploymentError("--out must name a manifest file")
    out_parent = out.expanduser().parent.resolve(strict=True)
    hashed = {}
    for role, path in (
        ("checkpoint", checkpoint),
        ("tokenizer", tokenizer),
        ("parent_checkpoint", parent_checkpoint),
        ("training_manifest", training_manifest),
        ("evidence", evidence),
    ):
        hashed[role] = legacy._stable_file(path, role)
    paths = [resolved for resolved, _, _ in hashed.values()]
    if len(set(paths)) != len(paths):
        raise ExternalDeploymentError("every input must be a different file")
    if hashed["checkpoint"][2] == hashed["parent_checkpoint"][2]:
        raise ExternalDeploymentError("the checkpoint and its parent have the same bytes; a model is not its own parent")
    if {out_parent / out.name, out_parent / RECORD_NAME} & set(paths):
        raise ExternalDeploymentError("the outputs must not overwrite an input")

    ckpt_path, ckpt_size, ckpt_sha = hashed["checkpoint"]
    tok_path, tok_size, tok_sha = hashed["tokenizer"]
    evidence_hashes = {
        "training_manifest_sha256": hashed["training_manifest"][2],
        "parent_checkpoint_sha256": hashed["parent_checkpoint"][2],
        "evidence_sha256": hashed["evidence"][2],
    }
    manifest: dict[str, Any] = {
        "schema": 2,
        "model_id": external_offline_model_id(evidence_hashes),
        "family": legacy.FAMILY,
        "model_card": legacy.MODEL_CARD,
        "language": legacy.LANGUAGE,
        "runtime": dict(legacy.RUNTIME),
        "base_checkpoint": {
            "id": checkpoint_id or ckpt_path.stem,
            **legacy._component(legacy._base_path(ckpt_path, out_parent), ckpt_size, ckpt_sha),
        },
        "tokenizer": legacy._component(
            legacy._relative_component(tok_path, out_parent, "tokenizer"), tok_size, tok_sha
        ),
        "server_compatibility": {"protocol": legacy.PROTOCOL, "version": legacy.PROTOCOL_VERSION},
        "provenance": {"kind": "external_offline", "training_provenance": "external_unverified", **evidence_hashes},
    }
    manifest["manifestSha256"] = legacy._sha256_bytes(legacy._canonical_json_bytes(manifest))
    record = {
        "schema": 1,
        "modelVersionId": manifest["model_id"],
        "manifestSha256": manifest["manifestSha256"],
        "deploymentSha256": legacy._sha256_bytes(legacy.manifest_file_bytes(manifest)),
        "inputs": {
            role: {"path": str(path), "sha256": sha, "size_bytes": size}
            for role, (path, size, sha) in hashed.items()
        },
    }
    return manifest, record


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Write a fused external_offline OmniASR-7B deployment manifest")
    parser.add_argument("--checkpoint", type=Path, required=True, help="the fused fairseq2 checkpoint (.pt)")
    parser.add_argument("--tokenizer", type=Path, required=True, help="tokenizer, inside the --out directory")
    parser.add_argument("--parent-checkpoint", type=Path, required=True, help="checkpoint it was trained from")
    parser.add_argument("--training-manifest", type=Path, required=True, help="the exact training manifest")
    parser.add_argument("--evidence", type=Path, required=True, help="the evidence document (e.g. release SHA256SUMS)")
    parser.add_argument("--out", type=Path, required=True, help="deployment manifest output path")
    parser.add_argument("--checkpoint-id", help="base_checkpoint.id (default: the checkpoint file stem)")
    parser.add_argument("--replace", action="store_true", help="atomically replace non-identical existing outputs")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    out = args.out.expanduser()
    try:
        manifest, record = build(
            checkpoint=args.checkpoint,
            tokenizer=args.tokenizer,
            parent_checkpoint=args.parent_checkpoint,
            training_manifest=args.training_manifest,
            evidence=args.evidence,
            out=out,
            checkpoint_id=args.checkpoint_id,
        )
        content = legacy.manifest_file_bytes(manifest)
        status = legacy._atomic_publish(out, content, replace=args.replace)
        record_status = legacy._atomic_publish(
            out.parent / RECORD_NAME, legacy._canonical_json_bytes(record) + b"\n", replace=args.replace
        )
    except (ExternalDeploymentError, legacy.BootstrapError, OSError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2
    print(
        json.dumps(
            {
                "deploymentSha256": record["deploymentSha256"],
                "manifestSha256": manifest["manifestSha256"],
                "modelVersionId": manifest["model_id"],
                "path": str(out.resolve(strict=True)),
                "provenance": manifest["provenance"],
                "record": str((out.parent / RECORD_NAME).resolve(strict=True)),
                "status": status,
                "recordStatus": record_status,
            },
            sort_keys=True,
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
