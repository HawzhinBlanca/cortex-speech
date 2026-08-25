"""Fault and trust-boundary regressions for the verify-10 supervisor."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
VERIFY = REPO_ROOT / "scripts" / "verify_10.py"
SUPERVISOR = REPO_ROOT / "scripts" / "verify10_supervisor.py"


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


class Verify10SupervisorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.supervisor = load_module("verify10_supervisor_fault_test", SUPERVISOR)
        cls.verify = load_module("verify10_fault_test", VERIFY)

    def test_registry_is_typed_profiled_explicit_and_below_six_hours(self) -> None:
        gates = self.verify.GATES
        self.assertTrue(gates)
        self.assertEqual(len({gate.id for gate in gates}), len(gates))
        self.assertTrue(all(isinstance(gate, self.verify.GateSpec) for gate in gates))
        self.assertTrue(all(gate.timeout_seconds > 0 for gate in gates))
        self.assertTrue(all(gate.profiles and gate.profiles <= self.verify.PROFILES for gate in gates))
        self.assertTrue(
            all(step.argv and all(isinstance(arg, str) and arg for arg in step.argv) for gate in gates for step in gate.steps)
        )
        full_budget = sum(gate.timeout_seconds for gate in gates if self.verify.PROFILE_FULL in gate.profiles)
        self.assertLessEqual(full_budget, 6 * 60 * 60)
        source = VERIFY.read_text(encoding="utf-8")
        self.assertNotIn("shell=True", source)

    def test_live_lease_refuses_a_concurrent_start(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            lease = Path(temporary) / "lease.json"
            first = self.supervisor.LeaseManager(lease, "a" * 40, "owner-product", "first")
            second = self.supervisor.LeaseManager(lease, "a" * 40, "owner-product", "second")
            first.acquire()
            try:
                with self.assertRaisesRegex(self.supervisor.LeaseError, "is live"):
                    second.acquire()
            finally:
                first.release()

    def test_two_concurrent_starts_have_exactly_one_winner(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            lease = Path(temporary) / "lease.json"
            barrier = threading.Barrier(2)
            release = threading.Event()
            outcomes: list[str] = []

            def contender(token: str) -> None:
                manager = self.supervisor.LeaseManager(lease, "b" * 40, "owner-product", token)
                barrier.wait()
                try:
                    manager.acquire()
                except self.supervisor.LeaseError:
                    outcomes.append("refused")
                    return
                outcomes.append("acquired")
                release.wait(5)
                manager.release()

            threads = [threading.Thread(target=contender, args=(token,)) for token in ("one", "two")]
            for thread in threads:
                thread.start()
            deadline = time.monotonic() + 5
            while len(outcomes) < 2 and time.monotonic() < deadline:
                time.sleep(0.02)
            release.set()
            for thread in threads:
                thread.join(timeout=5)
            self.assertCountEqual(outcomes, ["acquired", "refused"])

    def test_dead_holder_is_replaced_but_pid_reuse_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            lease = Path(temporary) / "lease.json"
            dead_token = "dead-holder"
            lease.write_text(
                json.dumps(
                    {
                        "schema": 1,
                        "runToken": dead_token,
                        "pid": 2_000_000_000,
                        "processCreationTime": "gone",
                        "heartbeatUnix": time.time() - 120,
                    }
                ),
                encoding="utf-8",
            )
            replacement = self.supervisor.LeaseManager(
                lease, "c" * 40, "owner-product", "replacement"
            )
            self.assertEqual(replacement.acquire(), dead_token)
            replacement.release()

            lease.write_text(
                json.dumps(
                    {
                        "schema": 1,
                        "runToken": "reused",
                        "pid": os.getpid(),
                        "processCreationTime": "not-this-process",
                        "heartbeatUnix": time.time() - 120,
                    }
                ),
                encoding="utf-8",
            )
            refused = self.supervisor.LeaseManager(lease, "c" * 40, "owner-product", "new")
            with self.assertRaisesRegex(self.supervisor.LeaseError, "PID was reused"):
                refused.acquire()

    def test_verified_wedged_holder_is_terminated_and_replaced(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            lease = Path(temporary) / "lease.json"
            holder = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(120)"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                creationflags=subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0,
                start_new_session=os.name != "nt",
            )
            try:
                creation = self.supervisor.process_creation_time(holder.pid)
                self.assertIsNotNone(creation)
                lease.write_text(
                    json.dumps(
                        {
                            "schema": 1,
                            "runToken": "wedged",
                            "pid": holder.pid,
                            "processCreationTime": creation,
                            "heartbeatUnix": time.time() - 120,
                        }
                    ),
                    encoding="utf-8",
                )
                replacement = self.supervisor.LeaseManager(
                    lease, "d" * 40, "owner-product", "replacement"
                )
                started = time.monotonic()
                self.assertEqual(replacement.acquire(), "wedged")
                self.assertLess(time.monotonic() - started, 60)
                holder.wait(timeout=5)
                replacement.release()
            finally:
                if holder.poll() is None:
                    holder.kill()
                    holder.wait(timeout=5)

    def test_timeout_kills_hanging_child_and_grandchild(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pids = root / "pids.json"
            log_path = root / "worker.log"
            script = (
                "import json,os,subprocess,sys,time;"
                "g=subprocess.Popen([sys.executable,'-c','import time;time.sleep(120)']);"
                f"open(r'{pids}','w').write(json.dumps([os.getpid(),g.pid]));"
                "time.sleep(120)"
            )
            with log_path.open("w", encoding="utf-8") as log:
                process, job = self.supervisor.spawn_isolated(
                    [sys.executable, "-c", script], cwd=root, log=log
                )
                deadline = time.monotonic() + 5
                while not pids.exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                identities = [
                    (pid, self.supervisor.process_creation_time(pid))
                    for pid in json.loads(pids.read_text(encoding="utf-8"))
                ]
                _return_code, timed_out = self.supervisor.wait_isolated(
                    process, job, timeout=0.2, heartbeat=lambda: None
                )
            self.assertTrue(timed_out)
            deadline = time.monotonic() + 5
            while any(
                creation is not None and self.supervisor.process_creation_time(pid) == creation
                for pid, creation in identities
            ) and time.monotonic() < deadline:
                time.sleep(0.05)
            self.assertTrue(
                all(
                    creation is None or self.supervisor.process_creation_time(pid) != creation
                    for pid, creation in identities
                )
            )

    def test_evidence_write_failure_is_terminal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaises(self.supervisor.EvidenceError):
                journal = self.supervisor.EvidenceJournal(Path(temporary), "token")
                journal.append("run_start")

    def test_gate_worker_isolated_result_is_hash_bound(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lease = self.supervisor.LeaseManager(
                root / "lease.json", "e" * 40, "owner-product", "token"
            )
            lease.acquire()
            try:
                journal = self.supervisor.EvidenceJournal(root / "events.jsonl", "token")
                gate = self.verify._gate_by_id("manifest-alignment")
                status, _seconds, _detail, artifacts = self.verify._run_gate_worker(
                    gate, root / "run", "token", lease, journal
                )
            finally:
                lease.release()
            self.assertEqual(status, self.verify.PASS, _detail)
            self.assertTrue(artifacts)
            self.assertTrue(all(len(str(artifact["sha256"])) == 64 for artifact in artifacts))

    def test_completed_manifest_is_the_only_status_and_latest_pointer_authority(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            original = (
                self.verify.PROOF_ROOT,
                self.verify.LATEST_PROOF,
                self.verify.RUN_LOCK,
                self.verify.LEGACY_RUN_LOCK,
                self.verify.GATES,
            )
            try:
                self.verify.PROOF_ROOT = root / "proofs"
                self.verify.LATEST_PROOF = root / "latest-proof.json"
                self.verify.RUN_LOCK = root / "verify.lease.json"
                self.verify.LEGACY_RUN_LOCK = root / "legacy.lock"
                self.verify.GATES = [self.verify._gate_by_id("manifest-alignment")]
                status_path = root / "STATUS.md"
                code = self.verify.aggregate_main(
                    quick=False,
                    status_md=str(status_path),
                    profile=self.verify.PROFILE_OWNER,
                )
                self.assertEqual(code, 2, "external evidence must keep a one-gate proof incomplete")
                pointer = json.loads(self.verify.LATEST_PROOF.read_text(encoding="utf-8"))
                manifest_path = Path(pointer["manifest"])
                self.assertEqual(pointer["manifestSha256"], self.supervisor.sha256_file(manifest_path))
                manifest = self.verify._validate_completed_manifest(
                    manifest_path, pointer["fullGitSha"], pointer["runToken"]
                )
                self.assertTrue(manifest["complete"])
                status = status_path.read_text(encoding="utf-8")
                self.assertIn(pointer["fullGitSha"], status)
                self.assertIn("owner-product", status)
                with self.assertRaises(self.verify.EvidenceError):
                    self.verify._validate_completed_manifest(
                        manifest_path, "f" * 40, pointer["runToken"]
                    )
            finally:
                (
                    self.verify.PROOF_ROOT,
                    self.verify.LATEST_PROOF,
                    self.verify.RUN_LOCK,
                    self.verify.LEGACY_RUN_LOCK,
                    self.verify.GATES,
                ) = original


if __name__ == "__main__":
    unittest.main()
