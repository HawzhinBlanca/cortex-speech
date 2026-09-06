#!/usr/bin/env python3
"""Adversarial unit and policy tests for owner proof-input preparation."""

from __future__ import annotations

import hashlib
import ctypes
import io
import json
import os
import shutil
import sqlite3
import stat
import subprocess
import sys
import tempfile
import unittest
import uuid
from pathlib import Path
from unittest import mock

import prepare_owner_proof_inputs as proof
import owner_proof_build as proof_build
import owner_proof_cli as proof_cli
import owner_proof_helper as proof_helper
import owner_proof_platform as proof_platform
import owner_proof_transaction as proof_transaction


if os.name != "nt":
    # protected_roots() documents its POSIX branch as the seam that "keeps synthetic policy
    # tests usable elsewhere": it reads APPDATA/LOCALAPPDATA there, and CI Linux/macOS shells
    # define neither. Point the live-authority sentinels at paths no fixture can live under so
    # every protected-root rejection stays a real, lexical comparison. Windows is untouched —
    # its branch resolves Known Folders and ignores these variables entirely.
    os.environ.setdefault("APPDATA", "/nonexistent-cortex-live/roaming")
    os.environ.setdefault("LOCALAPPDATA", "/nonexistent-cortex-live/local")


GIT_SHA = "a" * 40


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def dacl_descriptor(path: Path) -> bytes:
    if os.name != "nt":
        raise RuntimeError("DACL descriptors are Windows-only")
    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    advapi32.GetFileSecurityW.argtypes = [
        ctypes.c_wchar_p,
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_uint32),
    ]
    advapi32.GetFileSecurityW.restype = ctypes.c_int
    required = ctypes.c_uint32()
    advapi32.GetFileSecurityW(os.fspath(path), 0x4, None, 0, ctypes.byref(required))
    if not required.value:
        raise RuntimeError("DACL descriptor size is unavailable")
    buffer = ctypes.create_string_buffer(required.value)
    if not advapi32.GetFileSecurityW(
        os.fspath(path),
        0x4,
        buffer,
        len(buffer),
        ctypes.byref(required),
    ):
        raise RuntimeError("DACL descriptor cannot be read")
    return bytes(buffer.raw[: required.value])


def effective_dacl_descriptor(path: Path) -> bytes:
    """Compare access semantics while ignoring Windows' informational AUTO_INHERITED bit."""
    descriptor = bytearray(dacl_descriptor(path))
    control = int.from_bytes(descriptor[2:4], "little") & ~0x0400
    descriptor[2:4] = control.to_bytes(2, "little")
    return bytes(descriptor)


def seed_database(path: Path, *, segments: int, distinct_paths: int, campaign: bool) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(path)
    connection.executescript(
        "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, description TEXT NOT NULL);"
        "CREATE TABLE speech_segments(id TEXT PRIMARY KEY, audio_path TEXT NOT NULL);"
        "CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);"
    )
    connection.execute("INSERT INTO schema_migrations VALUES(1, 'fixture schema')")
    for index in range(segments):
        connection.execute(
            "INSERT INTO speech_segments VALUES(?, ?)",
            (f"segment-{index}", f"D:/fixture/audio-{index % distinct_paths}.wav"),
        )
    if campaign:
        connection.execute(
            "INSERT INTO settings VALUES(?, ?)",
            ("review_campaign.sequential_first_pass.v1", '{"fixture":true}'),
        )
    connection.commit()
    connection.close()


class FakeHelper:
    schema_fingerprints: dict[int, str] = {}

    def __init__(self, path: Path, helper_sha256: str, git_sha: str, helper_source_sha256: str):
        self.path = path
        self.helper_sha256 = helper_sha256
        self.git_sha = git_sha
        self.helper_source_sha256 = helper_source_sha256

    def schema_contract(self, *, expected_schema: int) -> dict[str, object]:
        fingerprint = self.schema_fingerprints.get(expected_schema)
        if fingerprint is None:
            raise proof.ProofInputError("fake helper lacks schema authority")
        return {
            "schema": 1,
            "operation": "schema-contract",
            "appGitSha": self.git_sha,
            "helperSourceSha256": self.helper_source_sha256,
            "schemaVersion": expected_schema,
            "schemaFingerprintSha256": fingerprint,
        }

    def inspect(self, database: Path, *, expected_schema: int, campaign: str) -> dict[str, object]:
        inspection = proof.inspect_sqlite_readonly(database)
        if inspection["schemaVersion"] != expected_schema:
            raise proof.ProofInputError("fake helper schema refusal")
        if campaign == "absent" and inspection["campaignAuthorityRows"] != 0:
            raise proof.ProofInputError("fake helper campaign refusal")
        if campaign == "required" and not inspection["sequentialCampaignPresent"]:
            raise proof.ProofInputError("fake helper missing campaign refusal")
        return {
            "schema": 1,
            "operation": "inspect",
            "appGitSha": self.git_sha,
            "helperSourceSha256": self.helper_source_sha256,
            "databaseSha256": digest(database),
            "inspection": inspection,
        }

    def migrate(
        self,
        source_database: Path,
        output_database: Path,
        *,
        staging_root: Path,
        source_sha256: str,
        expected_source_schema: int,
        expected_target_schema: int,
    ) -> dict[str, object]:
        if digest(source_database) != source_sha256 or not output_database.name.endswith(".work.db"):
            raise proof.ProofInputError("fake migration lost source identity")
        if not source_database.is_relative_to(staging_root / "db-authorities"):
            raise proof.ProofInputError("fake migration source escaped authority staging")
        if not output_database.is_relative_to(staging_root / "db-derived") or output_database.exists():
            raise proof.ProofInputError("fake migration escaped staging")
        before = proof.inspect_sqlite_readonly(source_database)
        if before["schemaVersion"] != expected_source_schema:
            raise proof.ProofInputError("fake migration source schema mismatch")
        shutil.copyfile(source_database, output_database)
        connection = sqlite3.connect(output_database)
        for version in range(expected_source_schema + 1, expected_target_schema + 1):
            connection.execute("INSERT INTO schema_migrations VALUES(?, ?)", (version, f"fixture schema {version}"))
        connection.commit()
        connection.close()
        after = proof.inspect_sqlite_readonly(output_database)
        return {
            "schema": 1,
            "operation": "migrate",
            "appGitSha": self.git_sha,
            "helperSourceSha256": self.helper_source_sha256,
            "sourceSha256": source_sha256,
            "resultSha256": digest(output_database),
            "appliedMigrations": list(range(expected_source_schema + 1, expected_target_schema + 1)),
            "before": before,
            "after": after,
        }


class CampaignInjectingHelper(FakeHelper):
    def migrate(self, *args, **kwargs):  # type: ignore[no-untyped-def]
        result = super().migrate(*args, **kwargs)
        database = args[1]
        connection = sqlite3.connect(database)
        connection.execute(
            "INSERT INTO settings VALUES(?, ?)",
            ("review_campaign.sequential_first_pass.v1", '{"forged":true}'),
        )
        connection.commit()
        connection.close()
        result["resultSha256"] = digest(database)
        result["after"] = proof.inspect_sqlite_readonly(database)
        return result


class SegmentDroppingHelper(FakeHelper):
    def migrate(self, *args, **kwargs):  # type: ignore[no-untyped-def]
        result = super().migrate(*args, **kwargs)
        database = args[1]
        connection = sqlite3.connect(database)
        connection.execute("DELETE FROM speech_segments WHERE id=(SELECT id FROM speech_segments LIMIT 1)")
        connection.commit()
        connection.close()
        result["resultSha256"] = digest(database)
        result["after"] = proof.inspect_sqlite_readonly(database)
        return result


class Fixture:
    def __init__(self, root: Path, *, scale_campaign: bool = False, campaign_present: bool = True):
        self.root = root
        source = root / "sources"
        self.mp4 = source / "mp4" / "A1-0001_PODCAST-001.mp4"
        self.mov = source / "mov" / "A1-0001_PODCAST-001.mov"
        self.flac = source / "flac" / "Lamofull00086400_A01.flac"
        self.audiobook = source / "book" / "audiobook-long.mp3"
        self.scale = source / "scale" / "cortex-speech.db"
        self.campaign = source / "campaign" / "cortex-speech.db"
        self.helper = source / "tool" / "owner_proof_db.exe"
        payloads = {
            self.mp4: b"real-mp4-fixture",
            self.mov: b"real-mov-fixture",
            self.flac: b"real-flac-fixture",
            self.audiobook: b"real-long-mp3-fixture",
            self.helper: b"MZ-fake-owner-proof-helper\0CORTEX_BUILD_SHA:" + GIT_SHA.encode("ascii"),
        }
        for path, payload in payloads.items():
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(payload)
        seed_database(self.scale, segments=7, distinct_paths=4, campaign=scale_campaign)
        seed_database(self.campaign, segments=9, distinct_paths=5, campaign=campaign_present)
        source_fingerprint = proof.inspect_sqlite_readonly(self.scale)["schemaFingerprintSha256"]
        campaign_fingerprint = proof.inspect_sqlite_readonly(self.campaign)["schemaFingerprintSha256"]
        target_fixture = source / "target" / "schema2.db"
        target_fixture.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(self.scale, target_fixture)
        target_connection = sqlite3.connect(target_fixture)
        target_connection.execute("INSERT INTO schema_migrations VALUES(2, 'fixture schema 2')")
        target_connection.commit()
        target_connection.close()
        target_fingerprint = proof.inspect_sqlite_readonly(target_fixture)["schemaFingerprintSha256"]
        target_fixture.unlink()
        if source_fingerprint != campaign_fingerprint:
            raise AssertionError("fixture schema-1 authorities must share one exact schema fingerprint")
        FakeHelper.schema_fingerprints = {
            1: str(source_fingerprint),
            2: str(target_fingerprint),
        }
        self.contract = root / "contract.json"
        contract = {
            "schema": 1,
            "bundleId": "cortex-owner-product-proof-inputs-v1",
            "targetPlatform": "windows-11-x64-owner",
            "mediaFileCount": 3,
            "requiredMediaExtensions": [".flac", ".mov", ".mp4"],
            "audiobookMinimumDurationMilliseconds": 60001,
            "helperToolchain": {
                "channel": "1.95.0-x86_64-pc-windows-msvc",
                "cargoBinarySha256": "b" * 64,
                "rustcBinarySha256": "c" * 64,
                "gitBinarySha256": "f" * 64,
                "gitVersion": "git version fixture",
                "cargoConfigSha256": "a" * 64,
                "cargoCommitHash": "d" * 40,
                "rustcCommitHash": "e" * 40,
                "msvcToolsVersion": "14.44.35207",
                "windowsSdkVersion": "10.0.26100.0",
                "clBinarySha256": "1" * 64,
                "linkBinarySha256": "2" * 64,
                "libBinarySha256": "3" * 64,
                "rcBinarySha256": "4" * 64,
                "mtBinarySha256": "5" * 64,
                "msvcTreeSha256": "6" * 64,
                "windowsSdkTreeSha256": "7" * 64,
                "rustRuntimeTreeSha256": "8" * 64,
                "gitRuntimeTreeSha256": "9" * 64,
            },
            "files": [
                self.spec("real-media-mp4", "media/A1-0001_PODCAST-001.mp4", self.mp4),
                self.spec("real-media-mov", "media/A1-0001_PODCAST-001.mov", self.mov),
                self.spec("real-media-flac", "media/Lamofull00086400_A01.flac", self.flac),
                self.spec("long-audiobook-mp3", "audiobook/audiobook-long.mp3", self.audiobook),
                self.spec(
                    "scale-database-authority",
                    "db-authorities/scale-production-derived-schema1.db",
                    self.scale,
                    include_size=False,
                ),
                self.spec(
                    "campaign-database-authority",
                    "db-authorities/current-campaign-exact-schema1.db",
                    self.campaign,
                    include_size=False,
                ),
            ],
            "databaseContracts": {
                "scale": {
                    "authorityRole": "scale-database-authority",
                    "sourceSchemaVersion": 1,
                    "targetSchemaVersion": 2,
                    "segmentCount": 7,
                    "distinctAudioPathCount": 4,
                    "campaignAuthority": "absent",
                    "sourceSchemaFingerprintSha256": source_fingerprint,
                    "targetSchemaFingerprintSha256": target_fingerprint,
                    "derivedRelativePath": "db-derived/scale-current-schema2.db",
                },
                "campaignExact": {
                    "authorityRole": "campaign-database-authority",
                    "schemaVersion": 1,
                    "segmentCount": 9,
                    "distinctAudioPathCount": 5,
                    "campaignAuthority": "required",
                    "schemaFingerprintSha256": campaign_fingerprint,
                },
            },
        }
        self.contract.write_text(json.dumps(contract), encoding="utf-8")
        self.contract_sha256 = proof._canonical_sha256(contract)

    @staticmethod
    def spec(role: str, relative: str, path: Path, *, include_size: bool = True) -> dict[str, object]:
        result: dict[str, object] = {
            "role": role,
            "relativePath": relative,
            "sourceBasename": path.name,
            "sha256": digest(path),
        }
        if include_size:
            result["sizeBytes"] = path.stat().st_size
        return result

    def sources(self) -> proof.SourcePaths:
        return proof.SourcePaths(
            media_mp4=self.mp4,
            media_mov=self.mov,
            media_flac=self.flac,
            audiobook_mp3=self.audiobook,
            scale_db=self.scale,
            campaign_db=self.campaign,
            migration_helper=self.helper,
        )


class OwnerProofInputTests(unittest.TestCase):
    def setUp(self) -> None:
        # resolve(): macOS temp lives under /var -> /private/var; unresolved fixture roots carry a
        # symlink component and the proof-input no-alias guards (correctly) refuse every path.
        self.temporary = Path(tempfile.mkdtemp(prefix="cortex-owner-proof-test-")).resolve()

    def tearDown(self) -> None:
        for path in sorted(self.temporary.rglob("*"), key=lambda item: len(item.parts), reverse=True):
            if path.is_file() and not path.is_symlink():
                try:
                    os.chmod(path, stat.S_IWRITE | stat.S_IREAD)
                except OSError:
                    pass
        if os.name == "nt":
            subprocess.run(
                ["icacls.exe", os.fspath(self.temporary), "/reset", "/T", "/C"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
                shell=False,
            )
        shutil.rmtree(self.temporary)

    @staticmethod
    def factory(helper_type=FakeHelper):  # type: ignore[no-untyped-def]
        return lambda path, helper_hash, git_sha, source_hash: helper_type(path, helper_hash, git_sha, source_hash)

    @unittest.skipUnless(os.name == "nt", "process-tree containment uses Windows Job Objects")
    def test_contained_command_kills_hanging_child_and_grandchild(self) -> None:
        started = self.temporary / "contained-grandchild-started.txt"
        survived = self.temporary / "contained-grandchild-survived.txt"
        grandchild_code = (
            "import pathlib,sys,time; "
            "pathlib.Path(sys.argv[1]).write_text('started',encoding='utf-8'); "
            "time.sleep(5); "
            "pathlib.Path(sys.argv[2]).write_text('survived',encoding='utf-8')"
        )
        child_code = (
            "import subprocess,sys,time; "
            "subprocess.Popen([sys.executable,'-c',sys.argv[1],sys.argv[2],sys.argv[3]]); "
            "time.sleep(60)"
        )
        with self.assertRaises(subprocess.TimeoutExpired):
            proof_build.run_contained(
                [sys.executable, "-c", child_code, grandchild_code, os.fspath(started), os.fspath(survived)],
                cwd=self.temporary,
                env=None,
                timeout=2,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        self.assertTrue(started.is_file())
        import time

        time.sleep(4)
        self.assertFalse(survived.exists())

    @unittest.skipUnless(os.name == "nt", "process-tree containment uses Windows Job Objects")
    def test_contained_command_tree_dies_when_supervisor_is_killed(self) -> None:
        started = self.temporary / "killed-supervisor-grandchild-started.txt"
        survived = self.temporary / "killed-supervisor-grandchild-survived.txt"
        grandchild_code = (
            "import pathlib,sys,time; "
            "pathlib.Path(sys.argv[1]).write_text('started',encoding='utf-8'); "
            "time.sleep(5); "
            "pathlib.Path(sys.argv[2]).write_text('survived',encoding='utf-8')"
        )
        contained_child = (
            "import subprocess,sys,time; "
            "subprocess.Popen([sys.executable,'-c',sys.argv[1],sys.argv[2],sys.argv[3]]); "
            "time.sleep(60)"
        )
        scripts = Path(proof.__file__).parent
        supervisor_code = (
            "import subprocess,sys; from pathlib import Path; "
            "from owner_proof_build import run_contained; "
            "run_contained([sys.executable,'-c',sys.argv[1],sys.argv[2],sys.argv[3],sys.argv[4]],"
            "cwd=Path(sys.argv[5]),env=None,timeout=60,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)"
        )
        environment = dict(os.environ)
        environment["PYTHONPATH"] = os.fspath(scripts)
        supervisor = subprocess.Popen(
            [
                sys.executable,
                "-c",
                supervisor_code,
                contained_child,
                grandchild_code,
                os.fspath(started),
                os.fspath(survived),
                os.fspath(self.temporary),
            ],
            cwd=self.temporary,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            import time

            deadline = time.monotonic() + 5
            while not started.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            self.assertTrue(started.is_file())
            supervisor.kill()
            supervisor.wait(timeout=10)
            time.sleep(4)
            self.assertFalse(survived.exists())
        finally:
            if supervisor.poll() is None:
                supervisor.kill()
                supervisor.wait(timeout=10)

    @unittest.skipUnless(os.name == "nt", "database-helper containment uses Windows Job Objects")
    def test_database_helper_timeout_kills_its_grandchild(self) -> None:
        started = self.temporary / "helper-grandchild-started.txt"
        survived = self.temporary / "helper-grandchild-survived.txt"
        grandchild_code = (
            "import pathlib,sys,time; "
            "pathlib.Path(sys.argv[1]).write_text('started',encoding='utf-8'); "
            "time.sleep(5); "
            "pathlib.Path(sys.argv[2]).write_text('survived',encoding='utf-8')"
        )
        helper_code = (
            "import subprocess,sys,time; "
            "subprocess.Popen([sys.executable,'-c',sys.argv[1],sys.argv[2],sys.argv[3]]); "
            "time.sleep(60)"
        )
        helper_path = Path(sys.executable)
        adapter = proof_helper.SubprocessHelper(
            helper_path,
            digest(helper_path),
            GIT_SHA,
            "b" * 64,
            lambda: dict(os.environ),
        )
        with self.assertRaisesRegex(proof.ProofInputError, "could not complete"):
            adapter._run(
                ["-c", helper_code, grandchild_code, os.fspath(started), os.fspath(survived)],
                timeout=2,
            )
        self.assertTrue(started.is_file())
        import time

        time.sleep(4)
        self.assertFalse(survived.exists())

    def test_database_helper_output_uses_the_strict_typed_json_boundary(self) -> None:
        helper_path = self.temporary / "strict-output-helper.exe"
        helper_path.write_bytes(b"MZ-strict-output-helper")
        helper_source_sha = "b" * 64
        adapter = proof_helper.SubprocessHelper(
            helper_path,
            digest(helper_path),
            GIT_SHA,
            helper_source_sha,
            lambda: dict(os.environ),
        )
        prefix = (
            f'{{"appGitSha":"{GIT_SHA}","helperSourceSha256":"{helper_source_sha}","schema":1,"x":'
        ).encode("ascii")
        invalid_payloads = (
            prefix + (b"[" * 1100) + b"0" + (b"]" * 1100) + b"}\n",
            prefix + b'"\\ud800"}\n',
            prefix + (b"9" * 5000) + b"}\n",
            prefix + b"NaN}\n",
            prefix + b"Infinity}\n",
            prefix + b"-Infinity}\n",
        )
        for payload in invalid_payloads:
            completed = subprocess.CompletedProcess([], 0, payload, b"")
            with (
                mock.patch.object(proof_helper, "run_contained", return_value=completed),
                self.assertRaises(proof.ProofInputError),
            ):
                adapter._run(["schema-contract"], timeout=1)

    def prepare(self, fixture: Fixture, *, helper_type=FakeHelper) -> tuple[Path, dict[str, object]]:  # type: ignore[no-untyped-def]
        output = self.temporary / "bundle"
        manifest = proof.prepare_bundle(
            contract_path=fixture.contract,
            sources=fixture.sources(),
            output_root=output,
            helper_factory=self.factory(helper_type),
            git_sha=GIT_SHA,
            expected_contract_sha256=fixture.contract_sha256,
        )
        return output / proof.BUNDLE_DIR, manifest

    @staticmethod
    def _crash_environment() -> dict[str, str]:
        environment = dict(os.environ)
        environment["PYTHONPATH"] = os.fspath(Path(proof.__file__).parent)
        return environment

    def _crash_prepare(self, fixture: Fixture, output: Path, mode: str) -> subprocess.CompletedProcess[bytes]:
        code = r'''
import json, os, sys
from pathlib import Path
import owner_proof_platform as platform
import prepare_owner_proof_inputs as proof
from test_prepare_owner_proof_inputs import FakeHelper, GIT_SHA

contract_path, output_path = Path(sys.argv[1]), Path(sys.argv[2])
contract = json.loads(contract_path.read_text(encoding="utf-8"))
scale = contract["databaseContracts"]["scale"]
campaign = contract["databaseContracts"]["campaignExact"]
FakeHelper.schema_fingerprints = {
    scale["sourceSchemaVersion"]: scale["sourceSchemaFingerprintSha256"],
    scale["targetSchemaVersion"]: scale["targetSchemaFingerprintSha256"],
    campaign["schemaVersion"]: campaign["schemaFingerprintSha256"],
}
mode = sys.argv[10]
real_publish = platform.OwnedDirectoryLock.publish_no_replace
real_seal = platform.ChildNamespaceSeal.__init__
seal_count = 0
def publish(self, destination, flush, *, preflushed=False):
    if Path(destination) == output_path and mode == "pre-rename":
        os._exit(71)
    result = real_publish(self, destination, flush, preflushed=preflushed)
    if Path(destination) == output_path and mode == "post-rename":
        os._exit(72)
    return result
def seal(self, *args, **kwargs):
    global seal_count
    real_seal(self, *args, **kwargs)
    seal_count += 1
    if mode == "partial-seal" and seal_count == 1:
        os._exit(73)
platform.OwnedDirectoryLock.publish_no_replace = publish
platform.ChildNamespaceSeal.__init__ = seal
proof.prepare_bundle(
    contract_path=contract_path,
    sources=proof.SourcePaths(
        media_mp4=Path(sys.argv[3]), media_mov=Path(sys.argv[4]), media_flac=Path(sys.argv[5]),
        audiobook_mp3=Path(sys.argv[6]), scale_db=Path(sys.argv[7]), campaign_db=Path(sys.argv[8]),
        migration_helper=Path(sys.argv[9]),
    ),
    output_root=output_path,
    helper_factory=lambda path, helper_hash, git_sha, source_hash: FakeHelper(path, helper_hash, git_sha, source_hash),
    git_sha=GIT_SHA,
    expected_contract_sha256=proof._canonical_sha256(contract),
)
'''
        return subprocess.run(
            [
                sys.executable,
                "-c",
                code,
                os.fspath(fixture.contract),
                os.fspath(output),
                os.fspath(fixture.mp4),
                os.fspath(fixture.mov),
                os.fspath(fixture.flac),
                os.fspath(fixture.audiobook),
                os.fspath(fixture.scale),
                os.fspath(fixture.campaign),
                os.fspath(fixture.helper),
                mode,
            ],
            cwd=self.temporary,
            env=self._crash_environment(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
            check=False,
        )

    def _crash_attempt(self, fixture: Fixture, bundle: Path, token: str, mode: str) -> subprocess.CompletedProcess[bytes]:
        code = r'''
import json, os, sys
from pathlib import Path
import owner_proof_platform as platform
import prepare_owner_proof_inputs as proof
from test_prepare_owner_proof_inputs import FakeHelper

contract_path, bundle, token, mode = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3], sys.argv[4]
contract = json.loads(contract_path.read_text(encoding="utf-8"))
scale = contract["databaseContracts"]["scale"]
campaign = contract["databaseContracts"]["campaignExact"]
FakeHelper.schema_fingerprints = {
    scale["sourceSchemaVersion"]: scale["sourceSchemaFingerprintSha256"],
    scale["targetSchemaVersion"]: scale["targetSchemaFingerprintSha256"],
    campaign["schemaVersion"]: campaign["schemaFingerprintSha256"],
}
final = bundle / proof.ATTEMPTS_DIR / token
real_publish = platform.OwnedDirectoryLock.publish_no_replace
def publish(self, destination, flush, *, preflushed=False):
    if Path(destination) == final and mode == "pre-rename":
        os._exit(81)
    result = real_publish(self, destination, flush, preflushed=preflushed)
    if Path(destination) == final and mode == "post-rename":
        os._exit(82)
    return result
platform.OwnedDirectoryLock.publish_no_replace = publish
proof.create_attempt(
    bundle_root=bundle,
    run_token=token,
    helper_factory=lambda path, helper_hash, git_sha, source_hash: FakeHelper(path, helper_hash, git_sha, source_hash),
    expected_contract_sha256=proof._canonical_sha256(contract),
)
'''
        return subprocess.run(
            [sys.executable, "-c", code, os.fspath(fixture.contract), os.fspath(bundle), token, mode],
            cwd=self.temporary,
            env=self._crash_environment(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
            check=False,
        )

    def _crash_prepare_during_recovery(
        self,
        fixture: Fixture,
        output: Path,
    ) -> subprocess.CompletedProcess[bytes]:
        code = r'''
import json, os, sys
from pathlib import Path
import owner_proof_platform as platform
import owner_proof_transaction as transaction
import prepare_owner_proof_inputs as proof
from test_prepare_owner_proof_inputs import FakeHelper, GIT_SHA

contract_path, output_path = Path(sys.argv[1]), Path(sys.argv[2])
contract = json.loads(contract_path.read_text(encoding="utf-8"))
scale = contract["databaseContracts"]["scale"]
campaign = contract["databaseContracts"]["campaignExact"]
FakeHelper.schema_fingerprints = {
    scale["sourceSchemaVersion"]: scale["sourceSchemaFingerprintSha256"],
    scale["targetSchemaVersion"]: scale["targetSchemaFingerprintSha256"],
    campaign["schemaVersion"]: campaign["schemaFingerprintSha256"],
}
real_delete = platform._delete_by_handle
def delete(kernel32, handle, *, context):
    result = real_delete(kernel32, handle, context=context)
    if context == f"publication recovery {transaction.OWNER_JOURNAL_NAME}":
        os._exit(74)
    return result
platform._delete_by_handle = delete
proof.prepare_bundle(
    contract_path=contract_path,
    sources=proof.SourcePaths(
        media_mp4=Path(sys.argv[3]), media_mov=Path(sys.argv[4]), media_flac=Path(sys.argv[5]),
        audiobook_mp3=Path(sys.argv[6]), scale_db=Path(sys.argv[7]), campaign_db=Path(sys.argv[8]),
        migration_helper=Path(sys.argv[9]),
    ),
    output_root=output_path,
    helper_factory=lambda path, helper_hash, git_sha, source_hash: FakeHelper(path, helper_hash, git_sha, source_hash),
    git_sha=GIT_SHA,
    expected_contract_sha256=proof._canonical_sha256(contract),
)
'''
        return subprocess.run(
            [
                sys.executable,
                "-c",
                code,
                os.fspath(fixture.contract),
                os.fspath(output),
                os.fspath(fixture.mp4),
                os.fspath(fixture.mov),
                os.fspath(fixture.flac),
                os.fspath(fixture.audiobook),
                os.fspath(fixture.scale),
                os.fspath(fixture.campaign),
                os.fspath(fixture.helper),
            ],
            cwd=self.temporary,
            env=self._crash_environment(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
            check=False,
        )

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_prepare_is_atomic_path_free_and_hash_preserving(self) -> None:
        fixture = Fixture(self.temporary)
        source_hashes = {role: digest(path) for role, path in fixture.sources().by_role().items()}
        bundle, manifest = self.prepare(fixture)
        self.assertTrue(bundle.is_dir())
        self.assertFalse(any(self.temporary.glob(f"{proof.STAGING_PREFIX}*")))
        raw = (bundle / proof.MANIFEST_NAME).read_text(encoding="utf-8")
        self.assertEqual(raw.encode("utf-8"), proof.canonical_json_bytes(json.loads(raw)))
        self.assertNotIn(str(self.temporary), raw)
        self.assertFalse(manifest["safety"]["sourcePathsPersisted"])
        self.assertEqual(
            manifest["helperSourceSha256"],
            digest(bundle / "tools" / "owner_proof_db.rs"),
        )
        self.assertEqual(manifest["helperBuild"]["mode"], "synthetic-test-override")
        self.assertEqual(source_hashes, {role: digest(path) for role, path in fixture.sources().by_role().items()})
        self.assertEqual(manifest["databases"]["scaleAuthority"]["schemaVersion"], 1)
        self.assertEqual(manifest["databases"]["scaleDerived"]["schemaVersion"], 2)
        self.assertEqual(manifest["databases"]["scaleDerived"]["campaignAuthorityRows"], 0)
        self.assertTrue(manifest["databases"]["campaignExactAuthority"]["sequentialCampaignPresent"])
        proof.validate_bundle(
            bundle,
            helper_factory=self.factory(),
            expected_contract_sha256=fixture.contract_sha256,
        )

    @unittest.skipUnless(os.name == "nt", "publication crash recovery uses Windows ACL identity authority")
    def test_prepare_reconciles_empty_and_owned_preseal_staging(self) -> None:
        for phase in ("empty", "owner"):
            case = self.temporary / phase
            fixture = Fixture(case)
            output = self.temporary / f"{phase}-bundle"
            normalized = proof._normalized_path(output)
            staging = output.parent / proof._deterministic_staging_name(proof.STAGING_PREFIX, normalized)
            staging.mkdir()
            with proof_platform.OwnedDirectoryLock(staging, publish=True) as staging_lock:
                if phase == "owner":
                    proof_transaction.begin_transaction(
                        staging_lock,
                        kind="prepare",
                        normalized_final_path=normalized,
                        release_git_sha=GIT_SHA,
                        run_token=None,
                        flush_directory=proof._fsync_directory,
                    )
                    (staging / "crash-before-plan.bin").write_bytes(b"owned partial work")
            manifest = proof.prepare_bundle(
                contract_path=fixture.contract,
                sources=fixture.sources(),
                output_root=output,
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=fixture.contract_sha256,
            )
            self.assertEqual(manifest["releaseGitSha"], GIT_SHA)
            self.assertFalse(staging.exists())
            proof.validate_bundle(
                output / proof.BUNDLE_DIR,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )

    @unittest.skipUnless(os.name == "nt", "publication crash recovery uses Windows ACL identity authority")
    def test_prepare_process_kills_reconcile_partial_seals_and_publication(self) -> None:
        expected_codes = {"partial-seal": 73, "pre-rename": 71, "post-rename": 72}
        for mode, expected_code in expected_codes.items():
            case = self.temporary / mode
            fixture = Fixture(case)
            output = self.temporary / f"{mode}-bundle"
            crashed = self._crash_prepare(fixture, output, mode)
            self.assertEqual(
                crashed.returncode,
                expected_code,
                crashed.stderr.decode("utf-8", errors="replace"),
            )
            manifest = proof.prepare_bundle(
                contract_path=fixture.contract,
                sources=fixture.sources(),
                output_root=output,
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=fixture.contract_sha256,
            )
            self.assertEqual(manifest["releaseGitSha"], GIT_SHA)
            staging = output.parent / proof._deterministic_staging_name(
                proof.STAGING_PREFIX,
                proof._normalized_path(output),
            )
            self.assertFalse(staging.exists())
            proof.validate_bundle(
                output / proof.BUNDLE_DIR,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )

    @unittest.skipUnless(os.name == "nt", "publication crash recovery uses Windows ACL identity authority")
    def test_prepare_process_kill_after_owner_journal_recovery_leaves_only_reclaimable_root(self) -> None:
        fixture = Fixture(self.temporary / "recovery-owner-journal")
        output = self.temporary / "recovery-owner-journal-bundle"
        initial_crash = self._crash_prepare(fixture, output, "partial-seal")
        self.assertEqual(initial_crash.returncode, 73, initial_crash.stderr.decode("utf-8", errors="replace"))

        recovery_crash = self._crash_prepare_during_recovery(fixture, output)
        self.assertEqual(recovery_crash.returncode, 74, recovery_crash.stderr.decode("utf-8", errors="replace"))
        staging = output.parent / proof._deterministic_staging_name(
            proof.STAGING_PREFIX,
            proof._normalized_path(output),
        )
        self.assertTrue(staging.is_dir())
        self.assertEqual(list(staging.iterdir()), [])

        manifest = proof.prepare_bundle(
            contract_path=fixture.contract,
            sources=fixture.sources(),
            output_root=output,
            helper_factory=self.factory(),
            git_sha=GIT_SHA,
            expected_contract_sha256=fixture.contract_sha256,
        )
        self.assertEqual(manifest["releaseGitSha"], GIT_SHA)
        self.assertFalse(staging.exists())

    @unittest.skipUnless(os.name == "nt", "publication crash recovery uses Windows ACL identity authority")
    def test_attempt_process_kills_are_same_token_idempotent(self) -> None:
        fixture = Fixture(self.temporary)
        bundle, _manifest = self.prepare(fixture)
        for mode, expected_code in (("pre-rename", 81), ("post-rename", 82)):
            token = str(uuid.uuid4())
            crashed = self._crash_attempt(fixture, bundle, token, mode)
            self.assertEqual(
                crashed.returncode,
                expected_code,
                crashed.stderr.decode("utf-8", errors="replace"),
            )
            result = proof.create_attempt(
                bundle_root=bundle,
                run_token=token,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )
            self.assertEqual(Path(result["attemptDirectory"]).name, token)
            staging = bundle / proof.ATTEMPTS_DIR / f"{proof.ATTEMPT_STAGING_PREFIX}{token}.staging"
            self.assertFalse(staging.exists())
            replay = proof.create_attempt(
                bundle_root=bundle,
                run_token=token,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )
            self.assertEqual(replay, result)

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_attempt_requires_exact_prepare_transaction_path_and_journals(self) -> None:
        path_fixture = Fixture(self.temporary / "attempt-container-path")
        path_bundle, _manifest = self.prepare(path_fixture)
        path_container = path_bundle.parent
        path_token = str(uuid.uuid4())
        real_normalized = proof._normalized_path

        def moved_container_path(path: Path) -> str:
            normalized = real_normalized(path)
            if Path(path) == path_container:
                return f"{normalized}-moved"
            return normalized

        with mock.patch.object(proof, "_normalized_path", side_effect=moved_container_path):
            with self.assertRaises(proof.ProofInputError):
                proof.create_attempt(
                    bundle_root=path_bundle,
                    run_token=path_token,
                    helper_factory=self.factory(),
                    expected_contract_sha256=path_fixture.contract_sha256,
                )
        self.assertFalse((path_bundle / proof.ATTEMPTS_DIR / path_token).exists())
        self.assertFalse(
            (path_bundle / proof.ATTEMPTS_DIR / f"{proof.ATTEMPT_STAGING_PREFIX}{path_token}.staging").exists()
        )

        for journal in (proof_transaction.OWNER_JOURNAL_NAME, proof_transaction.SEAL_PLAN_NAME):
            for state in ("missing", "corrupt"):
                case = self.temporary / f"attempt-{journal.strip('.').replace('.', '-')}-{state}"
                fixture = Fixture(case)
                output = case / "bundle"
                proof.prepare_bundle(
                    contract_path=fixture.contract,
                    sources=fixture.sources(),
                    output_root=output,
                    helper_factory=self.factory(),
                    git_sha=GIT_SHA,
                    expected_contract_sha256=fixture.contract_sha256,
                )
                bundle = output / proof.BUNDLE_DIR
                container = bundle.parent
                token = str(uuid.uuid4())
                final = bundle / proof.ATTEMPTS_DIR / token
                staging = bundle / proof.ATTEMPTS_DIR / f"{proof.ATTEMPT_STAGING_PREFIX}{token}.staging"
                if state == "missing":
                    observed = proof_transaction._direct_tree(container)
                    del observed[journal]
                    context = mock.patch.object(proof_transaction, "_direct_tree", return_value=observed)
                else:
                    journal_path = container / journal
                    proof._make_writable(journal_path)
                    journal_path.write_bytes(proof.canonical_json_bytes({"schema": 1}))
                    proof._make_readonly(journal_path)
                    context = mock.patch.object(proof_transaction, "_direct_tree", wraps=proof_transaction._direct_tree)
                with context, self.assertRaises(proof.ProofInputError):
                    proof.create_attempt(
                        bundle_root=bundle,
                        run_token=token,
                        helper_factory=self.factory(),
                        expected_contract_sha256=fixture.contract_sha256,
                    )
                self.assertFalse(final.exists())
                self.assertFalse(staging.exists())

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_attempt_replay_requires_exact_readonly_initial_manifest(self) -> None:
        fixture = Fixture(self.temporary / "attempt-manifest-exact")
        bundle, _manifest = self.prepare(fixture)

        def role(value: dict[str, object]) -> None:
            value["files"][0]["role"] = "forged-role"  # type: ignore[index]

        def initial_hash(value: dict[str, object]) -> None:
            value["files"][0]["initialSha256"] = "0" * 64  # type: ignore[index]

        def initial_size(value: dict[str, object]) -> None:
            value["files"][0]["initialSizeBytes"] += 1  # type: ignore[index,operator]

        def campaign(value: dict[str, object]) -> None:
            value["files"][0]["campaignAuthority"] = "forged"  # type: ignore[index]

        def extra(value: dict[str, object]) -> None:
            value["files"][0]["extra"] = True  # type: ignore[index]

        def duplicate(value: dict[str, object]) -> None:
            value["files"].append(dict(value["files"][0]))  # type: ignore[union-attr,index]

        mutations = (role, initial_hash, initial_size, campaign, extra, duplicate)
        for mutate in mutations:
            token = str(uuid.uuid4())
            created = proof.create_attempt(
                bundle_root=bundle,
                run_token=token,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )
            attempt = Path(created["attemptDirectory"])
            attempt_manifest = attempt / "attempt-manifest.v1.json"
            value = json.loads(attempt_manifest.read_text(encoding="utf-8"))
            mutate(value)
            proof._make_writable(attempt_manifest)
            attempt_manifest.write_bytes(proof.canonical_json_bytes(value))
            proof._make_readonly(attempt_manifest)
            with self.assertRaises(proof.ProofInputError):
                proof.create_attempt(
                    bundle_root=bundle,
                    run_token=token,
                    helper_factory=self.factory(),
                    expected_contract_sha256=fixture.contract_sha256,
                )

        writable_token = str(uuid.uuid4())
        writable = proof.create_attempt(
            bundle_root=bundle,
            run_token=writable_token,
            helper_factory=self.factory(),
            expected_contract_sha256=fixture.contract_sha256,
        )
        writable_manifest = Path(writable["attemptDirectory"]) / "attempt-manifest.v1.json"
        proof._make_writable(writable_manifest)
        with self.assertRaisesRegex(proof.ProofInputError, "manifest is writable"):
            proof.create_attempt(
                bundle_root=bundle,
                run_token=writable_token,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_attempt_replay_requires_both_databases_to_remain_writable(self) -> None:
        fixture = Fixture(self.temporary / "attempt-database-writable")
        bundle, _manifest = self.prepare(fixture)
        for name in ("scale-work.db", "campaign-observation.db"):
            token = str(uuid.uuid4())
            created = proof.create_attempt(
                bundle_root=bundle,
                run_token=token,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )
            database = Path(created["attemptDirectory"]) / name
            proof._make_readonly(database)
            with self.assertRaisesRegex(proof.ProofInputError, "database is read-only"):
                proof.create_attempt(
                    bundle_root=bundle,
                    run_token=token,
                    helper_factory=self.factory(),
                    expected_contract_sha256=fixture.contract_sha256,
                )

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_transaction_plan_missing_entries_and_duplicate_acl_states_fail_typed(self) -> None:
        owner_payload = proof.canonical_json_bytes({"fixture": "owner"})
        duplicate_masks = {
            "schema": 1,
            "ownerJournalSha256": hashlib.sha256(owner_payload).hexdigest(),
            "entries": [
                {
                    "relativePath": ".",
                    "identity": [1, 2],
                    "directory": True,
                    "linkCount": 1,
                    "protectedDaclSha256": "0" * 64,
                    "cumulativeDenyMasks": [2, 2],
                }
            ],
        }
        with self.assertRaises(proof.ProofInputError):
            proof_transaction._parse_plan(duplicate_masks, owner_payload)
        for invalid_masks in ([[]], [{}], [True], ["2"], [2, 1]):
            invalid = json.loads(json.dumps(duplicate_masks))
            invalid["entries"][0]["cumulativeDenyMasks"] = invalid_masks
            with self.subTest(invalid_masks=invalid_masks), self.assertRaises(proof.ProofInputError):
                proof_transaction._parse_plan(invalid, owner_payload)

        fixture = Fixture(self.temporary)
        bundle, _manifest = self.prepare(fixture)
        container = bundle.parent
        observed = proof_transaction._direct_tree(container)
        missing = next(
            relative
            for relative in observed
            if relative not in proof_transaction.PUBLISHED_TRANSACTION_FILES
            and relative not in (proof.BUNDLE_DIR, proof.VERIFY_ROOT_DIR)
        )
        del observed[missing]
        with (
            mock.patch.object(proof_transaction, "_direct_tree", return_value=observed),
            self.assertRaisesRegex(proof.ProofInputError, "missing a planned entry"),
        ):
            proof_transaction.validate_published_transaction(
                container,
                kind="prepare",
                normalized_final_path=proof._normalized_path(container),
                release_git_sha=GIT_SHA,
                run_token=None,
                mutable_descendant_roots=(proof.VERIFY_ROOT_DIR, f"{proof.BUNDLE_DIR}/{proof.ATTEMPTS_DIR}"),
            )

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_published_transaction_rejects_shape_valid_recovery_authority_drift(self) -> None:
        for mutation_index, mutation in enumerate(("fingerprint", "mask")):
            case = self.temporary / f"pm-{mutation_index}"
            fixture = Fixture(case)
            output = case / "bundle"
            proof.prepare_bundle(
                contract_path=fixture.contract,
                sources=fixture.sources(),
                output_root=output,
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=fixture.contract_sha256,
            )
            plan_path = output / proof_transaction.SEAL_PLAN_NAME
            plan = json.loads(plan_path.read_text(encoding="utf-8"))
            root_entry = next(entry for entry in plan["entries"] if entry["relativePath"] == ".")
            if mutation == "fingerprint":
                root_entry["protectedDaclSha256"] = "0" * 64
            else:
                final_mask = root_entry["cumulativeDenyMasks"][-1]
                extra = next(1 << bit for bit in range(32) if not final_mask & (1 << bit))
                root_entry["cumulativeDenyMasks"][-1] = final_mask | extra
            proof._make_writable(plan_path)
            plan_path.write_bytes(proof.canonical_json_bytes(plan))
            proof._make_readonly(plan_path)
            with self.subTest(mutation=mutation), self.assertRaises(proof.ProofInputError):
                proof.validate_bundle(
                    output / proof.BUNDLE_DIR,
                    helper_factory=self.factory(),
                    expected_contract_sha256=fixture.contract_sha256,
                )

        for journal_index, journal in enumerate(
            (proof_transaction.OWNER_JOURNAL_NAME, proof_transaction.SEAL_PLAN_NAME)
        ):
            case = self.temporary / f"jw-{journal_index}"
            fixture = Fixture(case)
            output = case / "bundle"
            proof.prepare_bundle(
                contract_path=fixture.contract,
                sources=fixture.sources(),
                output_root=output,
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=fixture.contract_sha256,
            )
            proof._make_writable(output / journal)
            with self.subTest(journal=journal), self.assertRaisesRegex(proof.ProofInputError, "remain read-only"):
                proof.validate_bundle(
                    output / proof.BUNDLE_DIR,
                    helper_factory=self.factory(),
                    expected_contract_sha256=fixture.contract_sha256,
                )

    def test_cli_lost_response_revalidates_the_fixed_bundle_child(self) -> None:
        container = self.temporary / "cli-container"
        container.mkdir()
        result = {"releaseGitSha": GIT_SHA}
        output = io.BytesIO()
        fake_stdout = type("BinaryStdout", (), {"buffer": output})()
        arguments = [
            "prepare",
            "--output-root",
            os.fspath(container),
            "--media-mp4",
            "unused.mp4",
            "--media-mov",
            "unused.mov",
            "--media-flac",
            "unused.flac",
            "--audiobook-mp3",
            "unused.mp3",
            "--scale-db",
            "unused-scale.db",
            "--campaign-db",
            "unused-campaign.db",
        ]
        with (
            mock.patch.object(proof, "validate_bundle", return_value=result) as validate,
            mock.patch.object(proof_cli.sys, "stdout", fake_stdout),
        ):
            self.assertEqual(proof.main(arguments), 0)
        validate.assert_called_once_with(container / proof.BUNDLE_DIR)
        payload = json.loads(output.getvalue())
        self.assertEqual(payload["containerRoot"], os.fspath(container))
        self.assertEqual(payload["bundleRoot"], os.fspath(container / proof.BUNDLE_DIR))
        self.assertEqual(payload["status"], "already-prepared")

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_fresh_attempts_are_writable_distinct_and_never_overwritten(self) -> None:
        fixture = Fixture(self.temporary)
        bundle, _manifest = self.prepare(fixture)
        token = str(uuid.uuid4())
        result = proof.create_attempt(
            bundle_root=bundle,
            run_token=token,
            helper_factory=self.factory(),
            expected_contract_sha256=fixture.contract_sha256,
        )
        attempt = Path(result["attemptDirectory"])
        scale = attempt / "scale-work.db"
        campaign = attempt / "campaign-observation.db"
        self.assertTrue(os.stat(scale).st_mode & stat.S_IWUSR)
        self.assertTrue(os.stat(campaign).st_mode & stat.S_IWUSR)
        self.assertNotEqual(scale, bundle / "db-derived" / "scale-current-schema2.db")
        connection = sqlite3.connect(campaign)
        self.assertIsNotNone(
            connection.execute(
                "SELECT value FROM settings WHERE key=?",
                ("review_campaign.sequential_first_pass.v1",),
            ).fetchone()
        )
        connection.close()
        replay = proof.create_attempt(
            bundle_root=bundle,
            run_token=token,
            helper_factory=self.factory(),
            expected_contract_sha256=fixture.contract_sha256,
        )
        self.assertEqual(replay, result)
        with scale.open("ab") as target:
            target.write(b"used")
        with self.assertRaises(proof.ProofInputError):
            proof.create_attempt(
                bundle_root=bundle,
                run_token=token,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_traversal_and_non_v4_attempt_tokens_are_refused(self) -> None:
        fixture = Fixture(self.temporary)
        bundle, _manifest = self.prepare(fixture)
        for token in ("../escape", str(uuid.uuid1()), str(uuid.uuid4()).upper()):
            with self.subTest(token=token), self.assertRaises(proof.ProofInputError):
                proof.create_attempt(
                    bundle_root=bundle,
                    run_token=token,
                    helper_factory=self.factory(),
                    expected_contract_sha256=fixture.contract_sha256,
                )

    def test_wrong_hash_and_wrong_filename_fail_before_publication(self) -> None:
        fixture = Fixture(self.temporary)
        fixture.mp4.write_bytes(b"tampered")
        with self.assertRaises(proof.ProofInputError):
            self.prepare(fixture)
        self.assertFalse((self.temporary / "bundle").exists())

        fixture = Fixture(self.temporary / "second")
        wrong = fixture.mp4.with_name("wrong.mp4")
        fixture.mp4.rename(wrong)
        sources = fixture.sources()
        sources = proof.SourcePaths(wrong, sources.media_mov, sources.media_flac, sources.audiobook_mp3, sources.scale_db, sources.campaign_db, sources.migration_helper)
        with self.assertRaises(proof.ProofInputError):
            proof.prepare_bundle(
                contract_path=fixture.contract,
                sources=sources,
                output_root=self.temporary / "second-bundle",
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=fixture.contract_sha256,
            )

    def test_public_release_contract_cannot_be_replaced_by_a_shape_valid_contract(self) -> None:
        fixture = Fixture(self.temporary)
        with self.assertRaises(proof.ProofInputError):
            proof.load_contract(fixture.contract)

    def test_helper_without_exact_release_marker_is_refused(self) -> None:
        fixture = Fixture(self.temporary)
        fixture.helper.write_bytes(b"MZ-helper-without-a-release-marker")
        with self.assertRaises(proof.ProofInputError):
            self.prepare(fixture)

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_contract_wrong_media_count_and_extra_bundle_file_are_refused(self) -> None:
        fixture = Fixture(self.temporary)
        contract = json.loads(fixture.contract.read_text(encoding="utf-8"))
        contract["mediaFileCount"] = 4
        fixture.contract.write_text(json.dumps(contract), encoding="utf-8")
        with self.assertRaises(proof.ProofInputError):
            self.prepare(fixture)

        fixture = Fixture(self.temporary / "valid")
        bundle, _manifest = self.prepare(fixture)
        extra = bundle / "media" / "extra.txt"
        with self.assertRaises(OSError):
            extra.write_text("not declared", encoding="utf-8")
        proof.validate_bundle(
            bundle,
            helper_factory=self.factory(),
            expected_contract_sha256=fixture.contract_sha256,
        )

    def test_json_boundaries_reject_malformed_values_with_typed_errors(self) -> None:
        invalid_payloads = {
            "deep-object": b'{"x":' + (b'{"x":' * 1100) + b"0" + (b"}" * 1100) + b"}\n",
            "deep-list": b'{"x":' + (b"[" * 1100) + b"0" + (b"]" * 1100) + b"}\n",
            "lone-surrogate": b'{"x":"\\ud800"}\n',
            "huge-integer": b'{"x":' + (b"9" * 5000) + b"}\n",
            "nan": b'{"x":NaN}\n',
            "infinity": b'{"x":Infinity}\n',
            "negative-infinity": b'{"x":-Infinity}\n',
        }
        for name, payload in invalid_payloads.items():
            proof_json = self.temporary / f"{name}.json"
            proof_json.write_bytes(payload)
            with self.subTest(loader=name), self.assertRaises(proof.ProofInputError):
                proof._load_json(proof_json)
            proof._make_readonly(proof_json)
            with self.subTest(journal=name), self.assertRaises(proof.ProofInputError):
                proof_transaction._load_canonical_journal(proof_json)

        for value in ({"x": "\ud800"}, {"x": float("nan")}, {"x": float("inf")}):
            with self.subTest(canonical=value), self.assertRaises(proof.ProofInputError):
                proof.canonical_json_bytes(value)

    def test_live_appdata_source_and_output_are_refused(self) -> None:
        live = self.temporary / "roaming"
        fixture = Fixture(live / "cortex-speech")
        authoritative = (live / "cortex-speech", self.temporary / "local" / "CortexSpeech" / "private-production-releases")
        with mock.patch.object(proof, "protected_roots", return_value=authoritative):
            with self.assertRaises(proof.ProofInputError):
                proof.prepare_bundle(
                    contract_path=fixture.contract,
                    sources=fixture.sources(),
                    output_root=self.temporary / "bundle",
                    helper_factory=self.factory(),
                    git_sha=GIT_SHA,
                    expected_contract_sha256=fixture.contract_sha256,
                )

        fixture = Fixture(self.temporary / "safe")
        output_parent = live / "cortex-speech"
        output_parent.mkdir(parents=True, exist_ok=True)
        with mock.patch.object(proof, "protected_roots", return_value=authoritative):
            with self.assertRaises(proof.ProofInputError):
                proof.prepare_bundle(
                    contract_path=fixture.contract,
                    sources=fixture.sources(),
                    output_root=output_parent / "proof",
                    helper_factory=self.factory(),
                    git_sha=GIT_SHA,
                    expected_contract_sha256=fixture.contract_sha256,
                )

    @unittest.skipUnless(os.name == "nt", "protected roots resolve via SHGetKnownFolderPath only on Windows")
    def test_environment_cannot_redirect_windows_known_folder_authority(self) -> None:
        before = proof.protected_roots()
        decoy = self.temporary / "decoy"
        with mock.patch.dict(
            os.environ,
            {"APPDATA": str(decoy / "roaming"), "LOCALAPPDATA": str(decoy / "local")},
            clear=False,
        ):
            self.assertEqual(proof.protected_roots(), before)

    @unittest.skipUnless(os.name == "nt", "\\\\?\\ verbatim and 8.3 short names are Windows path aliases")
    def test_windows_verbatim_and_short_names_share_one_containment_identity(self) -> None:
        protected = proof.protected_roots()[0]
        raw = str(protected)
        slash = "\\"
        verbatim_value = (
            slash * 2 + "?" + slash + "UNC" + slash + raw[2:]
            if raw.startswith(slash * 2)
            else slash * 2 + "?" + slash + raw
        )
        verbatim = Path(verbatim_value)
        self.assertEqual(proof._normalized_path(verbatim), proof._normalized_path(protected))
        with self.assertRaises(proof.ProofInputError):
            proof._reject_protected(verbatim / "alias-child")

        long_path = self.temporary / "Owner Proof Long Alias Directory"
        long_path.mkdir()
        buffer = ctypes.create_unicode_buffer(32768)
        length = ctypes.windll.kernel32.GetShortPathNameW(str(long_path), buffer, len(buffer))
        if not length or "~" not in buffer.value:
            self.skipTest("8.3 aliases are disabled on this volume")
        short_path = Path(buffer.value)
        self.assertEqual(proof._normalized_path(short_path), proof._normalized_path(long_path))
        self.assertTrue(proof._is_within(short_path / "future-child", long_path))

    def test_output_inside_git_worktree_is_refused(self) -> None:
        with self.assertRaises(proof.ProofInputError):
            proof._assert_safe_output_root(proof.REPO_ROOT / "unpublished-owner-proof-bundle-test")

    def test_snapshot_database_and_snapshot_attempt_root_are_refused(self) -> None:
        fixture = Fixture(self.temporary)
        snapshot = self.temporary / "snapshots" / "snapshot_1" / "cortex-speech.db"
        snapshot.parent.mkdir(parents=True)
        shutil.copyfile(fixture.scale, snapshot)
        sources = fixture.sources()
        sources = proof.SourcePaths(sources.media_mp4, sources.media_mov, sources.media_flac, sources.audiobook_mp3, snapshot, sources.campaign_db, sources.migration_helper)
        with self.assertRaises(proof.ProofInputError):
            proof.prepare_bundle(
                contract_path=fixture.contract,
                sources=sources,
                output_root=self.temporary / "bundle",
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=fixture.contract_sha256,
            )
        snapshot_output_parent = self.temporary / "snapshots"
        with self.assertRaises(proof.ProofInputError):
            proof.prepare_bundle(
                contract_path=fixture.contract,
                sources=fixture.sources(),
                output_root=snapshot_output_parent / "proof",
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=fixture.contract_sha256,
            )

    def test_sqlite_sidecars_are_refused(self) -> None:
        fixture = Fixture(self.temporary)
        Path(str(fixture.scale) + "-wal").write_bytes(b"sidecar")
        with self.assertRaises(proof.ProofInputError):
            self.prepare(fixture)

    def test_broken_sidecar_symlink_is_still_present_authority(self) -> None:
        fixture = Fixture(self.temporary)
        sidecar = Path(str(fixture.scale) + "-wal")
        try:
            os.symlink(self.temporary / "missing-target", sidecar)
        except OSError:
            with mock.patch.object(proof.os.path, "lexists", side_effect=lambda path: Path(path) == sidecar):
                with self.assertRaises(proof.ProofInputError):
                    proof._reject_sqlite_sidecars(fixture.scale)
            return
        with self.assertRaises(proof.ProofInputError):
            proof._reject_sqlite_sidecars(fixture.scale)

    def test_atomic_file_publication_never_overwrites_a_racing_destination(self) -> None:
        temporary = self.temporary / "temporary"
        destination = self.temporary / "destination"
        temporary.write_bytes(b"new")
        destination.write_bytes(b"owner")
        with self.assertRaises(proof.ProofInputError):
            proof._publish_file_without_overwrite(temporary, destination)
        self.assertEqual(destination.read_bytes(), b"owner")
        self.assertEqual(temporary.read_bytes(), b"new")

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_derived_name_flush_failure_prevents_root_publication(self) -> None:
        fixture = Fixture(self.temporary / "derived-name-fsync")
        output = self.temporary / "derived-name-fsync-bundle"
        staging = output.parent / proof._deterministic_staging_name(
            proof.STAGING_PREFIX,
            proof._normalized_path(output),
        )
        contract = json.loads(fixture.contract.read_text(encoding="utf-8"))
        derived_relative = contract["databaseContracts"]["scale"]["derivedRelativePath"]
        derived_final = staging / proof.BUNDLE_DIR / Path(derived_relative)
        derived_parent = derived_final.parent
        derived_work = derived_final.with_name(f"{derived_final.stem}.work.db")
        real_fsync = proof._fsync_directory
        observed_after_no_replace: list[bool] = []

        def fail_derived_name_flush(path: Path) -> None:
            if Path(path) == derived_parent:
                observed_after_no_replace.append(derived_final.is_file() and not derived_work.exists())
                raise proof.ProofInputError("injected derived-name durability failure")
            real_fsync(path)

        with mock.patch.object(proof, "_fsync_directory", side_effect=fail_derived_name_flush):
            with self.assertRaisesRegex(proof.ProofInputError, "derived-name durability failure"):
                proof.prepare_bundle(
                    contract_path=fixture.contract,
                    sources=fixture.sources(),
                    output_root=output,
                    helper_factory=self.factory(),
                    git_sha=GIT_SHA,
                    expected_contract_sha256=fixture.contract_sha256,
                )
        self.assertEqual(observed_after_no_replace, [True])
        self.assertFalse(output.exists())
        self.assertFalse(staging.exists())

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_source_and_published_authority_hardlinks_fail_closed(self) -> None:
        fixture = Fixture(self.temporary / "source-hardlink")
        alias = self.temporary / "source-hardlink-alias.mp4"
        os.link(fixture.mp4, alias)
        with self.assertRaises(proof.ProofInputError):
            proof.prepare_bundle(
                contract_path=fixture.contract,
                sources=fixture.sources(),
                output_root=self.temporary / "source-hardlink-bundle",
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=fixture.contract_sha256,
            )

        published_fixture = Fixture(self.temporary / "published-hardlink")
        bundle, _manifest = self.prepare(published_fixture)
        published = bundle / "media" / "A1-0001_PODCAST-001.mp4"
        os.link(published, self.temporary / "published-hardlink-alias.mp4")
        with self.assertRaises(proof.ProofInputError):
            proof.validate_bundle(
                bundle,
                helper_factory=self.factory(),
                expected_contract_sha256=published_fixture.contract_sha256,
            )

    def test_publication_flush_failure_happens_before_final_name(self) -> None:
        fixture = Fixture(self.temporary / "prepare-fsync")
        output = self.temporary / "prepare-fsync-bundle"
        real_fsync = proof._fsync_directory

        def fail_parent_before_publication(path: Path) -> None:
            if Path(path) == output.parent and not output.exists():
                raise proof.ProofInputError("injected parent fsync failure")
            real_fsync(path)

        with mock.patch.object(proof, "_fsync_directory", side_effect=fail_parent_before_publication):
            with self.assertRaises(proof.ProofInputError):
                proof.prepare_bundle(
                    contract_path=fixture.contract,
                    sources=fixture.sources(),
                    output_root=output,
                    helper_factory=self.factory(),
                    git_sha=GIT_SHA,
                    expected_contract_sha256=fixture.contract_sha256,
                )
        self.assertFalse(output.exists())

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_attempt_flush_failure_is_retryable_and_never_publishes(self) -> None:
        fixture = Fixture(self.temporary / "attempt-fsync")
        bundle, _manifest = self.prepare(fixture)
        token = "819abf8d-89cc-4a88-9093-73c44ca12087"  # fake fixture token
        attempts = bundle / proof.ATTEMPTS_DIR
        final = attempts / token
        real_fsync = proof._fsync_attempts_directory

        def fail_parent_before_publication(path: Path) -> None:
            if Path(path) == attempts and not final.exists():
                raise proof.ProofInputError("injected attempt fsync failure")
            real_fsync(path)

        with mock.patch.object(proof, "_fsync_attempts_directory", side_effect=fail_parent_before_publication):
            with self.assertRaises(proof.ProofInputError):
                proof.create_attempt(
                    bundle_root=bundle,
                    run_token=token,
                    helper_factory=self.factory(),
                    expected_contract_sha256=fixture.contract_sha256,
                )
        self.assertFalse(final.exists())
        result = proof.create_attempt(
            bundle_root=bundle,
            run_token=token,
            helper_factory=self.factory(),
            expected_contract_sha256=fixture.contract_sha256,
        )
        self.assertEqual(result["runToken"], token)

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_post_rename_flush_failure_is_explicit_and_reconcilable(self) -> None:
        fixture = Fixture(self.temporary / "post-rename-fsync")
        output = self.temporary / "post-rename-fsync-bundle"
        real_fsync = proof._fsync_directory

        def fail_only_after_rename(path: Path) -> None:
            if Path(path) == output.parent and output.exists():
                raise proof.ProofInputError("injected post-rename durability failure")
            real_fsync(path)

        with mock.patch.object(proof, "_fsync_directory", side_effect=fail_only_after_rename):
            with self.assertRaisesRegex(proof.ProofInputError, "durability is unknown"):
                proof.prepare_bundle(
                    contract_path=fixture.contract,
                    sources=fixture.sources(),
                    output_root=output,
                    helper_factory=self.factory(),
                    git_sha=GIT_SHA,
                    expected_contract_sha256=fixture.contract_sha256,
                )
        self.assertTrue(output.is_dir())
        bundle = output / proof.BUNDLE_DIR
        with mock.patch.object(proof, "_fsync_directory", wraps=real_fsync) as reconciled_flush:
            proof.validate_bundle(
                bundle,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )
        self.assertTrue(any(Path(call.args[0]) == output.parent for call in reconciled_flush.call_args_list))

        token = "151f77d1-b11b-43c1-8dbf-8840e867d0d5"  # fake fixture token
        attempts = bundle / proof.ATTEMPTS_DIR
        final = attempts / token

        real_attempt_fsync = proof._fsync_attempts_directory

        def fail_attempt_after_rename(path: Path) -> None:
            if Path(path) == attempts and final.exists():
                raise proof.ProofInputError("injected attempt durability failure")
            real_attempt_fsync(path)

        with mock.patch.object(proof, "_fsync_attempts_directory", side_effect=fail_attempt_after_rename):
            with self.assertRaisesRegex(proof.ProofInputError, "durability is unknown"):
                proof.create_attempt(
                    bundle_root=bundle,
                    run_token=token,
                    helper_factory=self.factory(),
                    expected_contract_sha256=fixture.contract_sha256,
                )
        with mock.patch.object(
            proof,
            "_fsync_attempts_directory",
            wraps=real_attempt_fsync,
        ) as reconciled_attempt_flush:
            recovered = proof.create_attempt(
                bundle_root=bundle,
                run_token=token,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )
        self.assertTrue(
            any(Path(call.args[0]) == attempts for call in reconciled_attempt_flush.call_args_list)
        )
        self.assertEqual(Path(recovered["attemptDirectory"]), final)

    @unittest.skipUnless(
        os.name == "nt",
        "the swap defense under test is the Windows delete-by-handle cleanup; the POSIX branch is rmtree",
    )
    def test_cleanup_refuses_a_renamed_root_replacement(self) -> None:
        parent = self.temporary / "cleanup-swap"
        parent.mkdir()
        staging = parent / f"{proof.STAGING_PREFIX}owned"
        displaced = parent / "displaced"
        victim = parent / "victim"
        staging.mkdir()
        victim.mkdir()
        keep = victim / "KEEP.txt"
        keep.write_text("owner", encoding="utf-8")
        identity = proof._owned_directory_identity(staging)
        real_delete = proof._delete_owned_tree_windows

        def swap_then_delete(path: Path, expected_identity: tuple[int, int]) -> None:
            os.rename(path, displaced)
            os.rename(victim, path)
            real_delete(path, expected_identity)

        with mock.patch.object(proof, "_delete_owned_tree_windows", side_effect=swap_then_delete):
            with self.assertRaises(proof.ProofInputError):
                proof._remove_owned_staging(staging, parent, proof.STAGING_PREFIX, identity)
        self.assertTrue(displaced.is_dir())
        self.assertEqual((staging / "KEEP.txt").read_text(encoding="utf-8"), "owner")

    @unittest.skipUnless(
        os.name == "nt",
        "_delete_owned_tree_windows is the Windows identity-locked deleter (CreateFileW handles)",
    )
    def test_cleanup_never_deletes_a_child_swapped_after_namespace_check(self) -> None:
        root = self.temporary / "cleanup-child-race"
        root.mkdir()
        owned = root / "owned.txt"
        parked = self.temporary / "parked-owned.txt"
        victim = self.temporary / "victim.txt"
        owned.write_text("OWNED", encoding="utf-8")
        victim.write_text("VICTIM", encoding="utf-8")
        identity = proof._owned_directory_identity(root)
        real_lstat = proof_platform.os.lstat
        swapped = False

        def swap_after_lstat(path: object) -> os.stat_result:
            nonlocal swapped
            metadata = real_lstat(path)
            if Path(path) == owned and not swapped:
                swapped = True
                os.rename(owned, parked)
                os.rename(victim, owned)
            return metadata

        with mock.patch.object(proof_platform.os, "lstat", side_effect=swap_after_lstat):
            with self.assertRaises(proof.ProofInputError):
                proof._delete_owned_tree_windows(root, identity)
        self.assertTrue(root.is_dir())
        self.assertEqual(owned.read_text(encoding="utf-8"), "VICTIM")
        self.assertEqual(parked.read_text(encoding="utf-8"), "OWNED")

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_prepare_holds_staging_identity_across_every_write(self) -> None:
        fixture = Fixture(self.temporary / "staging-write-race")
        output = self.temporary / "staging-write-race-bundle"
        victim = self.temporary / "staging-victim"
        displaced = self.temporary / "displaced-staging"
        victim.mkdir()
        keep = victim / "KEEP.txt"
        keep.write_text("owner", encoding="utf-8")
        real_copy = proof._copy_exact
        attempted = False

        def try_root_swap(source: Path, destination: Path, *, expected_sha256: str | None):
            nonlocal attempted
            if not attempted:
                attempted = True
                staging = next(parent for parent in destination.parents if parent.name.startswith(proof.STAGING_PREFIX))
                try:
                    os.rename(staging, displaced)
                except OSError as error:
                    raise proof.ProofInputError("transaction root lock blocked injected rename") from error
                os.rename(victim, staging)
            return real_copy(source, destination, expected_sha256=expected_sha256)

        with mock.patch.object(proof, "_copy_exact", side_effect=try_root_swap):
            with self.assertRaises(proof.ProofInputError):
                proof.prepare_bundle(
                    contract_path=fixture.contract,
                    sources=fixture.sources(),
                    output_root=output,
                    helper_factory=self.factory(),
                    git_sha=GIT_SHA,
                    expected_contract_sha256=fixture.contract_sha256,
                )
        self.assertTrue(attempted)
        self.assertEqual(keep.read_text(encoding="utf-8"), "owner")
        self.assertFalse((victim / "media").exists())

    @unittest.skipUnless(os.name == "nt", "directory identity locks are Windows-only")
    def test_prepare_locks_each_destination_parent_before_copying(self) -> None:
        fixture = Fixture(self.temporary / "child-write-race")
        output = self.temporary / "child-write-race-bundle"
        victim = self.temporary / "foreign-media"
        parked = self.temporary / "parked-media"
        victim.mkdir()
        keep = victim / "KEEP.txt"
        keep.write_text("owner", encoding="utf-8")
        real_copy = proof._copy_exact
        attempted = False

        def try_child_swap(
            source: Path,
            destination: Path,
            *,
            expected_sha256: str | None,
            source_require_single_link: bool = True,
        ) -> dict[str, object]:
            nonlocal attempted
            if destination.parent.name == "media" and not attempted:
                attempted = True
                try:
                    os.rename(destination.parent, parked)
                except OSError:
                    pass
                else:
                    os.rename(victim, destination.parent)
                    os.rename(destination.parent, victim)
                    os.rename(parked, destination.parent)
                    self.fail("destination parent was replaceable while the proof writer was active")
            return real_copy(
                source,
                destination,
                expected_sha256=expected_sha256,
                source_require_single_link=source_require_single_link,
            )

        with mock.patch.object(proof, "_copy_exact", side_effect=try_child_swap):
            proof.prepare_bundle(
                contract_path=fixture.contract,
                sources=fixture.sources(),
                output_root=output,
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=fixture.contract_sha256,
            )
        self.assertTrue(attempted)
        self.assertEqual(keep.read_text(encoding="utf-8"), "owner")
        self.assertFalse((victim / "A1-0001_PODCAST-001.mp4").exists())

    @unittest.skipUnless(os.name == "nt", "namespace sealing is Windows-only")
    def test_publication_seal_closes_the_child_lock_release_window(self) -> None:
        fixture = Fixture(self.temporary / "publication-seal")
        output = self.temporary / "publication-seal-bundle"
        parked = self.temporary / "parked-media-at-publish"
        real_publish = proof_platform.OwnedDirectoryLock.publish_no_replace
        attempted = False

        def try_swap_after_child_handles_close(
            locked: proof_platform.OwnedDirectoryLock,
            destination: Path,
            flush: object,
            *,
            preflushed: bool = False,
        ) -> None:
            nonlocal attempted
            media = locked.path / proof.BUNDLE_DIR / "media"
            if locked.path.name.startswith(proof.STAGING_PREFIX) and media.exists():
                attempted = True
                with self.assertRaises(OSError):
                    os.rename(media, parked)
            real_publish(locked, destination, flush, preflushed=preflushed)  # type: ignore[arg-type]

        with mock.patch.object(
            proof_platform.OwnedDirectoryLock,
            "publish_no_replace",
            new=try_swap_after_child_handles_close,
        ):
            proof.prepare_bundle(
                contract_path=fixture.contract,
                sources=fixture.sources(),
                output_root=output,
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=fixture.contract_sha256,
            )
        self.assertTrue(attempted)
        self.assertFalse(parked.exists())

    @unittest.skipUnless(os.name == "nt", "publication containers use Windows ACLs")
    def test_publication_container_blocks_bundle_replacement_and_top_level_injection(self) -> None:
        fixture = Fixture(self.temporary / "container-seal")
        bundle, _manifest = self.prepare(fixture)
        container = bundle.parent
        with self.assertRaises(OSError):
            os.rename(bundle, self.temporary / "moved-bundle-child")
        with self.assertRaises(OSError):
            (container / "injected").mkdir()
        with self.assertRaises(OSError):
            (container / "injected.txt").write_text("forged", encoding="utf-8")
        workspace = container / proof.VERIFY_ROOT_DIR / f"{proof.VERIFY_BUILD_PREFIX}fixture"
        workspace.mkdir()
        workspace.rmdir()

    def test_publication_never_suppresses_acl_restoration_failure(self) -> None:
        class FailingSeal:
            def restore(self) -> None:
                raise proof.ProofInputError("fixture ACL restoration failed")

        class FailingRoot:
            path = self.temporary

            def seal_children(self, _permissions: int) -> FailingSeal:
                return FailingSeal()

            def seal_self_deletion(self) -> None:
                raise RuntimeError("fixture seal construction failed")

        with self.assertRaisesRegex(proof.ProofInputError, "fixture ACL restoration failed"):
            proof_platform.publish_sealed_directory(
                FailingRoot(),  # type: ignore[arg-type]
                [],
                [],
                self.temporary / "never-published",
                lambda _path: None,
                seal_root_deletion=True,
            )

    @unittest.skipUnless(os.name == "nt", "DACL rollback ordering is Windows-only")
    def test_failed_publication_keeps_parent_namespace_sealed_until_children_restore(self) -> None:
        root = self.temporary / "rollback-order-staging"
        child = root / "child"
        child.mkdir(parents=True)
        moved = root / "moved-child"
        root_lock = proof_platform.OwnedDirectoryLock(root, publish=True)
        child_lock = proof_platform.OwnedDirectoryLock(child, publish=True)
        real_restore = proof_platform.ChildNamespaceSeal.restore
        attempted = False

        def restore_with_swap_probe(seal: proof_platform.ChildNamespaceSeal) -> None:
            nonlocal attempted
            if seal.path == child and not attempted:
                attempted = True
                with self.assertRaises(OSError):
                    os.rename(child, moved)
            real_restore(seal)

        try:
            with (
                mock.patch.object(root_lock, "publish_no_replace", side_effect=proof.ProofInputError("stop")),
                mock.patch.object(proof_platform.ChildNamespaceSeal, "restore", new=restore_with_swap_probe),
                self.assertRaisesRegex(proof.ProofInputError, "stop"),
            ):
                proof_platform.publish_sealed_directory(
                    root_lock,
                    [child_lock],
                    [],
                    self.temporary / "never-published-order",
                    proof._fsync_directory,
                    seal_root_deletion=True,
                )
        finally:
            root_lock.close()
            child_lock.close()
        self.assertTrue(attempted)
        self.assertTrue(child.is_dir())
        self.assertFalse(moved.exists())

    @unittest.skipUnless(os.name == "nt", "DACL restoration is Windows-only")
    def test_failed_publication_restores_exact_effective_protected_dacls(self) -> None:
        root = self.temporary / "restore-dacl-staging"
        child = root / "tools"
        authority = child / "owner_proof_db.exe"
        child.mkdir(parents=True)
        authority.write_bytes(b"MZ-fixture")
        root_lock = proof_platform.OwnedDirectoryLock(root, publish=True)
        child_lock = proof_platform.OwnedDirectoryLock(child, publish=True)
        with proof_platform.LockedFile(authority, acl_authority=True):
            pass
        before = {path: effective_dacl_descriptor(path) for path in (root, child, authority)}
        try:
            with (
                mock.patch.object(
                    root_lock,
                    "publish_no_replace",
                    side_effect=proof.ProofInputError("injected pre-rename failure"),
                ),
                self.assertRaisesRegex(proof.ProofInputError, "injected pre-rename failure"),
            ):
                proof_platform.publish_sealed_directory(
                    root_lock,
                    [child_lock],
                    [authority],
                    self.temporary / "never-published-dacl",
                    proof._fsync_directory,
                )
        finally:
            root_lock.close()
            child_lock.close()
        self.assertEqual(before, {path: effective_dacl_descriptor(path) for path in before})

    @unittest.skipUnless(os.name == "nt", "directory identity locks are Windows-only")
    def test_standalone_validation_locks_bundle_children_for_the_full_read(self) -> None:
        fixture = Fixture(self.temporary / "validation-locks")
        bundle, _manifest = self.prepare(fixture)
        parked = self.temporary / "parked-tools"
        real_validate = proof._validate_bundle_locked
        attempted = False

        def try_tools_swap(*args: object, **kwargs: object) -> dict[str, object]:
            nonlocal attempted
            attempted = True
            with self.assertRaises(OSError):
                os.rename(bundle / "tools", parked)
            return real_validate(*args, **kwargs)

        with mock.patch.object(proof, "_validate_bundle_locked", side_effect=try_tools_swap):
            proof.validate_bundle(
                bundle,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )
        self.assertTrue(attempted)
        self.assertFalse(parked.exists())

    @unittest.skipUnless(os.name == "nt", "file identity locks are Windows-only")
    def test_locked_file_denies_write_delete_and_rename_until_release(self) -> None:
        authority = self.temporary / "helper.exe"
        authority.write_bytes(b"MZ-identity-locked-helper")
        moved = self.temporary / "moved.exe"
        with proof_platform.LockedFile(authority) as locked:
            with self.assertRaises(OSError):
                authority.open("wb")
            with self.assertRaises(OSError):
                os.rename(authority, moved)
            with self.assertRaises(OSError):
                authority.unlink()
            locked.verify()
        os.rename(authority, moved)
        self.assertTrue(moved.exists())

    @unittest.skipUnless(os.name == "nt", "named mutex recovery is Windows-only")
    def test_prepare_mutex_is_released_when_its_owner_process_is_killed(self) -> None:
        identity = f"owner-proof-test-{uuid.uuid4()}"
        scripts = Path(proof.__file__).parent
        child_code = (
            "import sys,time; from owner_proof_platform import NamedMutex; "
            "mutex=NamedMutex('CortexOwnerProofPrepare',sys.argv[1]); "
            "print('READY',flush=True); time.sleep(60)"
        )
        environment = dict(os.environ)
        environment["PYTHONPATH"] = os.fspath(scripts)
        child = subprocess.Popen(
            [sys.executable, "-c", child_code, identity],
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            self.assertEqual(child.stdout.readline().strip(), "READY")
            with self.assertRaises(proof.ProofInputError):
                proof_platform.NamedMutex("CortexOwnerProofPrepare", identity)
            child.kill()
            child.wait(timeout=10)
            recovered = proof_platform.NamedMutex("CortexOwnerProofPrepare", identity)
            recovered.close()
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=10)
            assert child.stdout is not None and child.stderr is not None
            child.stdout.close()
            child.stderr.close()

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_moved_bundle_inside_git_worktree_cannot_validate_or_create_attempt(self) -> None:
        fixture = Fixture(self.temporary / "moved-bundle")
        bundle, _manifest = self.prepare(fixture)
        with mock.patch.object(proof, "REPO_ROOT", self.temporary):
            with self.assertRaises(proof.ProofInputError):
                proof.validate_bundle(
                    bundle,
                    helper_factory=self.factory(),
                    expected_contract_sha256=fixture.contract_sha256,
                )
            with self.assertRaises(proof.ProofInputError):
                proof.create_attempt(
                    bundle_root=bundle,
                    run_token=str(uuid.uuid4()),
                    helper_factory=self.factory(),
                    expected_contract_sha256=fixture.contract_sha256,
                )

    @unittest.skipUnless(os.name == "nt", "release tool identity is Windows-only")
    def test_pinned_git_legitimate_installed_hardlinks_are_hash_bound_not_rejected(self) -> None:
        contract = proof.load_contract(proof.DEFAULT_CONTRACT)
        toolchain = contract["helperToolchain"]
        # The contract pins the OWNER'S RELEASE TOOLCHAIN by hash, which is the point: a proof
        # bundle may only be built with the exact Git binary the release contract names.
        #
        # Skip ONLY on a hosted CI runner, where that toolchain cannot exist by construction. Do
        # NOT skip merely because the hash differs: on the release workstation a mismatch means the
        # pinned Git drifted from the contract, and that must stay a LOUD FAILURE rather than a
        # quiet skip -- a gate that skips itself when its subject changes is worse than no gate.
        if os.environ.get("GITHUB_ACTIONS") == "true" or os.environ.get("CI") == "true":
            raise unittest.SkipTest(
                "SKIP-ENV: hosted CI runner - the release contract pins the owner workstation's "
                "Git binary by hash, which no runner can have; the refusal there is the pin working"
            )
        git = proof._pinned_git_tool(toolchain)
        self.assertGreaterEqual(os.stat(git).st_nlink, 1)

    def test_schema_fingerprint_is_pinned_not_merely_well_formed(self) -> None:
        fixture = Fixture(self.temporary)
        inspection = proof.inspect_sqlite_readonly(fixture.scale)
        with self.assertRaises(proof.ProofInputError):
            proof._validate_inspection(
                inspection,
                expected_schema=1,
                expected_schema_fingerprint="0" * 64,
                expected_segments=7,
                expected_distinct_paths=4,
                campaign="absent",
            )

    @unittest.skipUnless(os.name == "nt", "the release build environment contract requires Windows (%SystemRoot%, MSVC toolchain roots)")
    def test_release_build_environment_drops_wrapper_and_flag_injection(self) -> None:
        injected = {
            "SystemRoot": r"Z:\attacker-root",
            "RUSTC": "attacker-rustc",
            "RUSTC_WRAPPER": "attacker-wrapper",
            "RUSTFLAGS": "--cfg forged",
            "CARGO_ENCODED_RUSTFLAGS": "forged",
            "CARGO_BUILD_RUSTC_WRAPPER": "forged",
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER": "forged-linker",
            "GIT_DIR": r"Z:\forged-git-dir",
            "GIT_INDEX_FILE": r"Z:\forged-index",
            "SQLITE_TMPDIR": r"Z:\forged-sqlite-temp",
            "RUST_LOG": "trace,private_payload=trace",
        }
        with mock.patch.dict(os.environ, injected, clear=True):
            environment = proof._minimal_windows_build_environment()
            git_environment = proof._git_environment(Path(r"C:\trusted\git.exe"))
            helper_environment = proof._minimal_helper_environment()
        self.assertEqual(environment["SystemRoot"], environment["WINDIR"])
        self.assertTrue(Path(environment["SystemRoot"]).is_absolute())
        for key in injected.keys() - {"SystemRoot"}:
            self.assertNotIn(key, environment)
        self.assertNotEqual(environment["SystemRoot"], injected["SystemRoot"])
        self.assertIsNotNone(git_environment)
        assert git_environment is not None
        self.assertNotIn("GIT_DIR", git_environment)
        self.assertNotIn("GIT_INDEX_FILE", git_environment)
        self.assertNotIn("SQLITE_TMPDIR", helper_environment)
        self.assertNotIn("RUST_LOG", helper_environment)

    @unittest.skipUnless(os.name == "nt", "release rebuild ownership uses Windows mutexes")
    def test_release_validation_rebuilds_helper_and_cleans_owned_workspace(self) -> None:
        parent = self.temporary / "verification-parent"
        container = parent / "container"
        bundle = container / proof.BUNDLE_DIR
        verify_root = container / proof.VERIFY_ROOT_DIR
        bundle.mkdir(parents=True)
        verify_root.mkdir()
        binary_bytes = b"MZ" + f"CORTEX_BUILD_SHA:{GIT_SHA}".encode("ascii")
        binary_hash = hashlib.sha256(binary_bytes).hexdigest()
        build_evidence = {"fixture": "exact-build-evidence"}
        contract = {"helperToolchain": {}}
        manifest = {"releaseGitSha": GIT_SHA, "helperBuild": build_evidence}
        helper_entry = {"sha256": binary_hash, "sizeBytes": len(binary_bytes)}
        manifest_sha256 = hashlib.sha256(proof.canonical_json_bytes(manifest)).hexdigest()
        workspace_key = hashlib.sha256(
            f"{GIT_SHA}\n{manifest_sha256}".encode("ascii")
        ).hexdigest()
        stale_exact = verify_root / f"{proof.VERIFY_BUILD_PREFIX}{workspace_key[:32]}"
        (stale_exact / "orphaned-build").mkdir(parents=True)
        (stale_exact / proof.VERIFY_LEASE_NAME).write_bytes(b"crash-partial-lease")

        def fake_build(staging: Path, *_args: object) -> tuple[object, ...]:
            build_root = staging / ".helper-build-fixture"
            build_root.mkdir()
            binary = build_root / "owner_proof_db.exe"
            binary.write_bytes(binary_bytes)
            identity = proof_platform.owned_directory_identity(build_root)
            return (
                binary,
                proof_platform.LockedFile(binary),
                binary_hash,
                len(binary_bytes),
                build_root,
                identity,
                build_evidence,
            )

        with (
            mock.patch.object(proof, "_git_sha_clean", return_value=GIT_SHA),
            mock.patch.object(proof, "_build_release_helper", side_effect=fake_build),
        ):
            proof._verify_release_helper_rebuild(
                bundle,
                contract,
                manifest,
                helper_entry,
                self.temporary / "git.exe",
            )
        self.assertFalse(stale_exact.exists())
        self.assertFalse(any(verify_root.glob(f"{proof.VERIFY_BUILD_PREFIX}*")))

        with (
            mock.patch.object(proof, "_git_sha_clean", return_value=GIT_SHA),
            mock.patch.object(proof, "_build_release_helper", side_effect=fake_build),
            mock.patch.object(
                proof,
                "_remove_owned_staging",
                side_effect=proof.ProofInputError("injected verification cleanup failure"),
            ),
            self.assertRaisesRegex(proof.ProofInputError, "injected verification cleanup failure"),
        ):
            proof._verify_release_helper_rebuild(
                bundle,
                contract,
                manifest,
                helper_entry,
                self.temporary / "git.exe",
            )
        recovered_mutex = proof_platform.NamedMutex(
            "CortexOwnerProofVerifyBuild",
            proof._normalized_path(bundle),
        )
        recovered_mutex.close()
        with proof_platform.OwnedDirectoryLock(verify_root, pin_namespace=False):
            pass
        stale_identity = proof._owned_directory_identity(stale_exact)
        proof._remove_owned_staging(stale_exact, verify_root, proof.VERIFY_BUILD_PREFIX, stale_identity)

        with (
            mock.patch.object(proof, "_git_sha_clean", return_value=GIT_SHA),
            mock.patch.object(proof, "_build_release_helper", side_effect=fake_build),
            self.assertRaisesRegex(proof.ProofInputError, "independently rebuilt"),
        ):
            proof._verify_release_helper_rebuild(
                bundle,
                contract,
                manifest,
                {"sha256": "0" * 64, "sizeBytes": len(binary_bytes)},
                self.temporary / "git.exe",
            )
        self.assertFalse(any(verify_root.glob(f"{proof.VERIFY_BUILD_PREFIX}*")))

        stale = verify_root / f"{proof.VERIFY_BUILD_PREFIX}stale"
        stale.mkdir()
        with (
            mock.patch.object(proof, "_git_sha_clean", return_value=GIT_SHA),
            mock.patch.object(proof, "_build_release_helper") as build,
            self.assertRaisesRegex(proof.ProofInputError, "unknown entry"),
        ):
            proof._verify_release_helper_rebuild(
                bundle,
                contract,
                manifest,
                helper_entry,
                self.temporary / "git.exe",
            )
        build.assert_not_called()

    def test_alternate_cargo_configuration_is_never_build_authority(self) -> None:
        cargo_home = self.temporary / "cargo-home"
        cargo_home.mkdir()
        config = proof.REPO_ROOT / proof.CARGO_CONFIG_REPO_PATH
        expected = digest(config)
        injected = proof.REPO_ROOT / ".cargo" / "config.toml"
        real_lexists = os.path.lexists

        def config_injection(path: object) -> bool:
            return Path(path) == injected or real_lexists(path)

        with mock.patch.object(proof, "_git_blob_sha256", return_value=expected):
            with mock.patch.object(proof.os.path, "lexists", side_effect=config_injection):
                with self.assertRaises(proof.ProofInputError):
                    proof._require_exact_cargo_configuration(
                        cargo_home=cargo_home,
                        release_sha=GIT_SHA,
                        git=Path(r"C:\trusted\git.exe"),
                        expected_sha256=expected,
                    )

    def test_preexisting_output_is_never_reused_or_overwritten(self) -> None:
        fixture = Fixture(self.temporary)
        output = self.temporary / "bundle"
        output.mkdir()
        sentinel = output / "sentinel"
        sentinel.write_text("owner data", encoding="utf-8")
        with self.assertRaises(proof.ProofInputError):
            self.prepare(fixture)
        self.assertEqual(sentinel.read_text(encoding="utf-8"), "owner data")

    def test_scale_campaign_and_missing_campaign_authority_fail_closed(self) -> None:
        scale_campaign = Fixture(self.temporary / "scale-campaign", scale_campaign=True)
        with self.assertRaises(proof.ProofInputError):
            proof.prepare_bundle(
                contract_path=scale_campaign.contract,
                sources=scale_campaign.sources(),
                output_root=self.temporary / "scale-campaign-bundle",
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=scale_campaign.contract_sha256,
            )
        missing = Fixture(self.temporary / "missing", campaign_present=False)
        with self.assertRaises(proof.ProofInputError):
            proof.prepare_bundle(
                contract_path=missing.contract,
                sources=missing.sources(),
                output_root=self.temporary / "missing-bundle",
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=missing.contract_sha256,
            )

    def test_migration_cannot_introduce_campaign_or_drop_segments(self) -> None:
        for index, helper_type in enumerate((CampaignInjectingHelper, SegmentDroppingHelper)):
            fixture = Fixture(self.temporary / f"case-{index}")
            with self.subTest(helper=helper_type.__name__), self.assertRaises(proof.ProofInputError):
                proof.prepare_bundle(
                    contract_path=fixture.contract,
                    sources=fixture.sources(),
                    output_root=self.temporary / f"bad-bundle-{index}",
                    helper_factory=self.factory(helper_type),
                    git_sha=GIT_SHA,
                    expected_contract_sha256=fixture.contract_sha256,
                )

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_manifest_and_authorities_detect_post_publication_drift(self) -> None:
        fixture = Fixture(self.temporary)
        bundle, _manifest = self.prepare(fixture)
        campaign = bundle / "db-authorities" / "current-campaign-exact-schema1.db"
        proof._make_writable(campaign)
        with campaign.open("ab") as target:
            target.write(b"tamper")
        with self.assertRaises(proof.ProofInputError):
            proof.validate_bundle(
                bundle,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_attempts_entry_is_namespace_sealed_against_replacement(self) -> None:
        fixture = Fixture(self.temporary)
        bundle, _manifest = self.prepare(fixture)
        attempts = bundle / proof.ATTEMPTS_DIR
        with self.assertRaises(OSError):
            attempts.rmdir()
        with self.assertRaises(OSError):
            (bundle / "tools" / "injected.dll").write_bytes(b"untrusted runtime")
        result = proof.create_attempt(
            bundle_root=bundle,
            run_token=str(uuid.uuid4()),
            helper_factory=self.factory(),
            expected_contract_sha256=fixture.contract_sha256,
        )
        attempt = Path(result["attemptDirectory"])
        with self.assertRaises(OSError):
            os.rename(attempt, attempts / "swapped-attempt")
        scale = attempt / "scale-work.db"
        connection = sqlite3.connect(scale)
        connection.execute("INSERT INTO settings(key,value) VALUES('durability-proof','committed')")
        connection.commit()
        connection.close()
        reopened = sqlite3.connect(scale)
        self.assertEqual(
            reopened.execute("SELECT value FROM settings WHERE key='durability-proof'").fetchone(),
            ("committed",),
        )
        reopened.close()
        proof.validate_bundle(
            bundle,
            helper_factory=self.factory(),
            expected_contract_sha256=fixture.contract_sha256,
        )

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_rehashed_manifest_cannot_reauthorize_a_different_media_authority(self) -> None:
        fixture = Fixture(self.temporary)
        bundle, _manifest = self.prepare(fixture)
        media = bundle / "media" / "A1-0001_PODCAST-001.mp4"
        proof._make_writable(media)
        media.write_bytes(b"different-but-rehashed")
        proof._make_readonly(media)
        manifest_path = bundle / proof.MANIFEST_NAME
        proof._make_writable(manifest_path)
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        replacement_hash = digest(media)
        for item in manifest["files"]:
            if item["role"] == "real-media-mp4":
                item["sha256"] = replacement_hash
                item["sizeBytes"] = media.stat().st_size
        for item in manifest["sourcePreservation"]:
            if item["role"] == "real-media-mp4":
                item["declaredSha256"] = replacement_hash
                item["copiedSha256"] = replacement_hash
        manifest_path.write_bytes(proof.canonical_json_bytes(manifest))
        proof._make_readonly(manifest_path)
        with self.assertRaises(proof.ProofInputError):
            proof.validate_bundle(
                bundle,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )

    @unittest.skipUnless(os.name == "nt", "the owner-proof publication transaction is Windows-only (NamedMutex, CreateFileW identity locks, protected DACLs)")
    def test_writable_manifest_is_not_valid_proof(self) -> None:
        fixture = Fixture(self.temporary)
        bundle, _manifest = self.prepare(fixture)
        proof._make_writable(bundle / proof.MANIFEST_NAME)
        with self.assertRaises(proof.ProofInputError):
            proof.validate_bundle(
                bundle,
                helper_factory=self.factory(),
                expected_contract_sha256=fixture.contract_sha256,
            )

    def test_symlinks_are_refused_when_supported(self) -> None:
        fixture = Fixture(self.temporary)
        link = self.temporary / "linked.mp4"
        try:
            os.symlink(fixture.mp4, link)
        except OSError:
            # Windows may reserve symlink creation for Developer Mode/admin. Exercise the same
            # fail-closed branch deterministically by presenting the direct file as a reparse point.
            with mock.patch.object(proof, "_metadata_reparse", return_value=True):
                with self.assertRaises(proof.ProofInputError):
                    proof._assert_safe_existing_file(fixture.mp4, role="media")
            return
        sources = fixture.sources()
        sources = proof.SourcePaths(link, sources.media_mov, sources.media_flac, sources.audiobook_mp3, sources.scale_db, sources.campaign_db, sources.migration_helper)
        with self.assertRaises(proof.ProofInputError):
            proof.prepare_bundle(
                contract_path=fixture.contract,
                sources=sources,
                output_root=self.temporary / "bundle",
                helper_factory=self.factory(),
                git_sha=GIT_SHA,
                expected_contract_sha256=fixture.contract_sha256,
            )

    def test_policy_has_no_campaign_sanitizer_or_authority_delete_command(self) -> None:
        python_source = Path(proof.__file__).read_text(encoding="utf-8")
        rust_source = (
            proof.APP_ROOT / "src-tauri" / "src" / "bin" / "owner_proof_db.rs"
        ).read_text(encoding="utf-8")
        for forbidden in (
            "DELETE FROM settings",
            "DELETE FROM review_campaign_registry",
            "DROP TABLE review_campaign",
            "UPDATE settings SET value",
        ):
            self.assertNotIn(forbidden, python_source)
            self.assertNotIn(forbidden, rust_source)
        self.assertNotIn("--delete", python_source)
        self.assertNotIn("--sanitize", python_source)

    def test_checked_in_real_authority_contract_is_well_formed_and_path_free(self) -> None:
        contract = proof.load_contract(proof.DEFAULT_CONTRACT)
        encoded = json.dumps(contract, ensure_ascii=False)
        self.assertNotRegex(encoded, r"[A-Za-z]:[\\/]")
        self.assertNotIn("AppData", encoded)
        scale = contract["databaseContracts"]["scale"]
        campaign = contract["databaseContracts"]["campaignExact"]
        self.assertEqual(scale["sourceSchemaVersion"], 60)
        self.assertEqual(
            scale["sourceSchemaFingerprintSha256"],
            "80e88a4f9b40ecba46aaee933c98dc7aea54fe8ae58ea3178354409188759cd0",
        )
        self.assertEqual(scale["targetSchemaVersion"], 70)
        self.assertEqual(
            scale["targetSchemaFingerprintSha256"],
            "f542f433eb5f235369ed703d8231c9956f246a1e6470c7d1b46a79c29503257c",
        )
        self.assertEqual(scale["derivedRelativePath"], "db-derived/scale-current-schema70.db")
        self.assertEqual(scale["segmentCount"], 30373)
        self.assertEqual(campaign["schemaVersion"], 65)
        self.assertEqual(
            campaign["schemaFingerprintSha256"],
            "50b62000b8174323221c206bf747a1507cc5e88459bc16a25064ae09b06ecd66",
        )
        self.assertEqual(campaign["segmentCount"], 43774)
        self.assertEqual(proof._canonical_sha256(contract), proof.RELEASE_CONTRACT_SHA256)


if __name__ == "__main__":
    unittest.main(verbosity=2)
