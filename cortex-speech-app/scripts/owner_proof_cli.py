#!/usr/bin/env python3
"""Thin command-line surface for the owner-proof input authority."""

from __future__ import annotations

import argparse
import hashlib
import os
import sqlite3
import sys
from pathlib import Path
from types import ModuleType


def run_cli(api: ModuleType, argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=api.__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare = commands.add_parser("prepare", help="atomically publish one proof-input container")
    prepare.add_argument("--output-root", type=Path, required=True)
    for name in ("media-mp4", "media-mov", "media-flac", "audiobook-mp3", "scale-db", "campaign-db"):
        prepare.add_argument(f"--{name}", type=Path, required=True)
    validate = commands.add_parser("validate", help="rehash and independently revalidate an existing bundle")
    validate.add_argument("--bundle-root", type=Path, required=True)
    attempt = commands.add_parser("attempt", help="create a fresh per-run writable database attempt")
    attempt.add_argument("--bundle-root", type=Path, required=True)
    attempt.add_argument("--run-token", required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "prepare":
            container = api._absolute_lexical(args.output_root)
            bundle = container / api.BUNDLE_DIR
            existed = os.path.lexists(container)
            result = api.validate_bundle(bundle) if existed else api.prepare_bundle(
                contract_path=api.DEFAULT_CONTRACT,
                sources=api.SourcePaths(
                    args.media_mp4,
                    args.media_mov,
                    args.media_flac,
                    args.audiobook_mp3,
                    args.scale_db,
                    args.campaign_db,
                ),
                output_root=container,
            )
            output = {
                "schema": 1,
                "status": "already-prepared" if existed else "prepared",
                "containerRoot": os.fspath(container),
                "bundleRoot": os.fspath(bundle),
                "manifestSha256": hashlib.sha256(api.canonical_json_bytes(result)).hexdigest(),
                "releaseGitSha": result["releaseGitSha"],
            }
        elif args.command == "validate":
            bundle = api._absolute_lexical(args.bundle_root)
            result = api.validate_bundle(bundle)
            output = {
                "schema": 1,
                "status": "valid",
                "bundleRoot": os.fspath(bundle),
                "manifestSha256": hashlib.sha256(api.canonical_json_bytes(result)).hexdigest(),
                "releaseGitSha": result["releaseGitSha"],
            }
        else:
            output = api.create_attempt(bundle_root=args.bundle_root, run_token=args.run_token)
        sys.stdout.buffer.write(api.canonical_json_bytes(output))
        sys.stdout.buffer.flush()
        return 0
    except (OSError, api.ProofInputError, sqlite3.Error) as error:
        sys.stderr.buffer.write(f"OWNER PROOF INPUTS FAILED: {error}\n".encode("ascii", errors="backslashreplace"))
        sys.stderr.buffer.flush()
        return 1
