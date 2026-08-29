#!/usr/bin/env python3
"""Identity-locked subprocess adapter for the bundled owner-proof database helper."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path
from typing import Any, Callable

from owner_proof_build import run_contained
from owner_proof_contract import MAX_JSON_BYTES, parse_json_bytes
from owner_proof_platform import LockedFile, ProofInputError, stable_file_sha256


class SubprocessHelper:
    def __init__(
        self,
        path: Path,
        helper_sha256: str,
        git_sha: str,
        helper_source_sha256: str,
        environment_factory: Callable[[], dict[str, str]],
    ):
        self.path = path
        self.helper_sha256 = helper_sha256
        self.git_sha = git_sha
        self.helper_source_sha256 = helper_source_sha256
        self.environment_factory = environment_factory

    def _run(self, arguments: list[str], *, timeout: int) -> dict[str, Any]:
        with LockedFile(self.path) as file_lock:
            if stable_file_sha256(self.path) != self.helper_sha256:
                raise ProofInputError("bundled migration helper hash changed")
            environment = self.environment_factory()
            try:
                completed = run_contained(
                    [os.fspath(self.path), *arguments],
                    cwd=self.path.parent,
                    env=environment,
                    capture_output=True,
                    timeout=timeout,
                )
            except (OSError, subprocess.SubprocessError) as error:
                raise ProofInputError("owner-proof database helper could not complete") from error
            file_lock.verify()
            if stable_file_sha256(self.path) != self.helper_sha256:
                raise ProofInputError("bundled migration helper changed during execution")
        if len(completed.stdout) > MAX_JSON_BYTES or len(completed.stderr) > MAX_JSON_BYTES:
            raise ProofInputError("owner-proof database helper exceeded bounded output")
        if completed.returncode != 0:
            message = completed.stderr.decode("utf-8", errors="replace").strip()
            raise ProofInputError(f"owner-proof database helper refused the operation: {message[:1000]}")
        value = parse_json_bytes(completed.stdout, context="owner-proof database helper output")
        if (
            not isinstance(value, dict)
            or value.get("schema") != 1
            or value.get("appGitSha") != self.git_sha
            or value.get("helperSourceSha256") != self.helper_source_sha256
        ):
            raise ProofInputError("owner-proof database helper identity does not match the release")
        return value

    def inspect(self, database: Path, *, expected_schema: int, campaign: str) -> dict[str, Any]:
        return self._run(
            [
                "inspect",
                "--db",
                os.fspath(database),
                "--expected-schema",
                str(expected_schema),
                "--campaign",
                campaign,
            ],
            timeout=900,
        )

    def schema_contract(self, *, expected_schema: int) -> dict[str, Any]:
        return self._run(["schema-contract", "--expected-schema", str(expected_schema)], timeout=900)

    def migrate(
        self,
        source_database: Path,
        output_database: Path,
        *,
        staging_root: Path,
        source_sha256: str,
        expected_source_schema: int,
        expected_target_schema: int,
    ) -> dict[str, Any]:
        return self._run(
            [
                "migrate",
                "--source-db",
                os.fspath(source_database),
                "--output-db",
                os.fspath(output_database),
                "--staging-root",
                os.fspath(staging_root),
                "--source-sha256",
                source_sha256,
                "--expected-source-schema",
                str(expected_source_schema),
                "--expected-target-schema",
                str(expected_target_schema),
            ],
            timeout=3600,
        )
