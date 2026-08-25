"""Durable process and evidence primitives for :mod:`verify_10`.

This module is intentionally dependency-free so the verifier can supervise its own
fault drills before the application environment is trusted.
"""

from __future__ import annotations

import contextlib
import ctypes
import hashlib
import json
import os
import signal
import subprocess
import sys
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterator, TextIO


HEARTBEAT_SECONDS = 5.0
STALE_AFTER_SECONDS = 30.0
GRACEFUL_STOP_SECONDS = 15.0


class EvidenceError(RuntimeError):
    """The verifier can no longer prove what it did and must fail closed."""


class LeaseError(RuntimeError):
    """A verifier lease is live, malformed, or cannot be taken over safely."""


def utc_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _fsync_directory(path: Path) -> None:
    if os.name == "nt":
        return
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def atomic_write_bytes(path: Path, content: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
    try:
        with temporary.open("xb") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        _fsync_directory(path.parent)
    except OSError as error:
        with contextlib.suppress(OSError):
            temporary.unlink()
        raise EvidenceError(f"atomic evidence write failed for {path}: {error}") from error


def atomic_write_json(path: Path, value: object) -> None:
    atomic_write_bytes(
        path,
        (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode("utf-8"),
    )


def read_json_object(path: Path) -> dict[str, object]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise LeaseError(f"cannot read verifier lease {path}: {error}") from error
    if not isinstance(value, dict):
        raise LeaseError(f"verifier lease {path} is not a JSON object")
    return value


def process_creation_time(pid: int) -> str | None:
    """Return an OS-issued process start identity, never a wall-clock estimate."""

    if pid <= 0:
        return None
    if os.name != "nt":
        try:
            fields = Path(f"/proc/{pid}/stat").read_text(encoding="ascii").split()
            return fields[21]
        except (OSError, IndexError, UnicodeError):
            return None

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    process_query_limited_information = 0x1000
    handle = kernel32.OpenProcess(process_query_limited_information, False, pid)
    if not handle:
        return None
    creation = ctypes.c_ulonglong()
    exit_time = ctypes.c_ulonglong()
    kernel = ctypes.c_ulonglong()
    user = ctypes.c_ulonglong()
    try:
        exit_code = ctypes.c_uint32()
        if not kernel32.GetExitCodeProcess(handle, ctypes.byref(exit_code)) or exit_code.value != 259:
            return None
        ok = kernel32.GetProcessTimes(
            handle,
            ctypes.byref(creation),
            ctypes.byref(exit_time),
            ctypes.byref(kernel),
            ctypes.byref(user),
        )
        return str(creation.value) if ok else None
    finally:
        kernel32.CloseHandle(handle)


def _atomic_create(path: Path) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    return os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)


def _terminate_verified_process_tree(pid: int, creation_time: str) -> None:
    if process_creation_time(pid) != creation_time:
        raise LeaseError("lease holder identity changed before takeover; refusing to terminate")
    if os.name == "nt":
        subprocess.run(
            ["taskkill", "/PID", str(pid), "/T"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=GRACEFUL_STOP_SECONDS,
        )
    else:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(pid, signal.SIGTERM)
    deadline = time.monotonic() + GRACEFUL_STOP_SECONDS
    while process_creation_time(pid) == creation_time and time.monotonic() < deadline:
        time.sleep(0.2)
    if process_creation_time(pid) != creation_time:
        return
    if os.name == "nt":
        subprocess.run(
            ["taskkill", "/PID", str(pid), "/T", "/F"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=10,
        )
    else:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(pid, signal.SIGKILL)


@dataclass
class LeaseManager:
    path: Path
    full_sha: str
    profile: str
    run_token: str
    current_gate: str | None = None
    child_pid: int | None = None

    def __post_init__(self) -> None:
        self.pid = os.getpid()
        creation = process_creation_time(self.pid)
        if creation is None:
            raise LeaseError("cannot obtain this verifier process creation time")
        self.creation_time = creation
        self.started_unix = time.time()
        self._last_heartbeat = 0.0

    def _document(self) -> dict[str, object]:
        now = time.time()
        return {
            "schema": 1,
            "runToken": self.run_token,
            "pid": self.pid,
            "processCreationTime": self.creation_time,
            "fullGitSha": self.full_sha,
            "profile": self.profile,
            "currentGate": self.current_gate,
            "childPid": self.child_pid,
            "startedUnix": self.started_unix,
            "heartbeatUnix": now,
            "heartbeatUtc": utc_now(),
        }

    def _write(self) -> None:
        atomic_write_json(self.path, self._document())
        self._last_heartbeat = time.monotonic()

    def _install_new(self) -> None:
        descriptor = _atomic_create(self.path)
        try:
            payload = (json.dumps(self._document(), sort_keys=True) + "\n").encode("utf-8")
            os.write(descriptor, payload)
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        _fsync_directory(self.path.parent)
        self._last_heartbeat = time.monotonic()

    def acquire(self) -> str | None:
        """Acquire the lease and return an abandoned token when a stale run was replaced."""

        try:
            self._install_new()
            return None
        except FileExistsError:
            pass

        takeover = self.path.with_suffix(self.path.suffix + ".takeover")
        try:
            takeover_fd = _atomic_create(takeover)
        except FileExistsError as error:
            raise LeaseError("another verifier start is already auditing the existing lease") from error
        try:
            lease = read_json_object(self.path)
            required = {
                "runToken": str,
                "pid": int,
                "processCreationTime": str,
                "heartbeatUnix": (int, float),
            }
            for field, expected in required.items():
                if not isinstance(lease.get(field), expected):
                    raise LeaseError(f"existing verifier lease has invalid {field}")
            holder_pid = int(lease["pid"])
            holder_creation = str(lease["processCreationTime"])
            holder_token = str(lease["runToken"])
            observed_creation = process_creation_time(holder_pid)
            age = time.time() - float(lease["heartbeatUnix"])
            if observed_creation == holder_creation and age <= STALE_AFTER_SECONDS:
                raise LeaseError(
                    f"verifier {holder_pid} is live (heartbeat {age:.1f}s old, token {holder_token})"
                )
            if observed_creation is not None and observed_creation != holder_creation:
                raise LeaseError("lease PID was reused; holder identity is unknown and takeover fails closed")
            if observed_creation == holder_creation:
                # Re-read immediately before termination: PID, creation time and run token must all
                # still be the exact values audited above.
                current = read_json_object(self.path)
                triple = (
                    current.get("pid"),
                    current.get("processCreationTime"),
                    current.get("runToken"),
                )
                if triple != (holder_pid, holder_creation, holder_token):
                    raise LeaseError("lease identity changed during takeover; refusing termination")
                _terminate_verified_process_tree(holder_pid, holder_creation)
                if process_creation_time(holder_pid) == holder_creation:
                    raise LeaseError("verified stale process tree survived takeover")
            with contextlib.suppress(FileNotFoundError):
                self.path.unlink()
            self._install_new()
            return holder_token
        finally:
            os.close(takeover_fd)
            with contextlib.suppress(OSError):
                takeover.unlink()

    def heartbeat(self, *, force: bool = False) -> None:
        if force or time.monotonic() - self._last_heartbeat >= HEARTBEAT_SECONDS:
            self._write()

    def update_gate(self, gate: str | None, child_pid: int | None) -> None:
        self.current_gate = gate
        self.child_pid = child_pid
        self.heartbeat(force=True)

    def release(self) -> None:
        try:
            current = read_json_object(self.path)
        except LeaseError:
            return
        triple = (
            current.get("pid"),
            current.get("processCreationTime"),
            current.get("runToken"),
        )
        if triple == (self.pid, self.creation_time, self.run_token):
            with contextlib.suppress(OSError):
                self.path.unlink()


class EvidenceJournal:
    """Append-only, fsynced event journal with a monotonic per-run sequence."""

    def __init__(self, path: Path, run_token: str) -> None:
        self.path = path
        self.run_token = run_token
        self.sequence = 0
        if path.exists():
            try:
                lines = [line for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
                if lines:
                    last = json.loads(lines[-1])
                    if not isinstance(last, dict) or last.get("runToken") != run_token:
                        raise ValueError("journal tail is bound to another run")
                    sequence = last.get("sequence")
                    if not isinstance(sequence, int) or sequence < 1:
                        raise ValueError("journal tail has no valid sequence")
                    self.sequence = sequence
            except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
                raise EvidenceError(f"cannot resume evidence journal {path}: {error}") from error

    def append(self, event: str, **fields: object) -> dict[str, object]:
        self.sequence += 1
        record = {
            "schema": 1,
            "sequence": self.sequence,
            "runToken": self.run_token,
            "event": event,
            "at": utc_now(),
            **fields,
        }
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            with self.path.open("a", encoding="utf-8", newline="\n") as handle:
                handle.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")
                handle.flush()
                os.fsync(handle.fileno())
        except OSError as error:
            raise EvidenceError(f"cannot append verifier evidence event {event}: {error}") from error
        return record


class WindowsJob:
    """Kill-on-close Windows Job Object; a no-op process-group wrapper elsewhere."""

    def __init__(self) -> None:
        self.handle: int | None = None
        if os.name != "nt":
            return
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        handle = kernel32.CreateJobObjectW(None, None)
        if not handle:
            raise OSError(ctypes.get_last_error(), "CreateJobObjectW failed")

        class BasicLimit(ctypes.Structure):
            _fields_ = [
                ("PerProcessUserTimeLimit", ctypes.c_longlong),
                ("PerJobUserTimeLimit", ctypes.c_longlong),
                ("LimitFlags", ctypes.c_uint32),
                ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t),
                ("ActiveProcessLimit", ctypes.c_uint32),
                ("Affinity", ctypes.c_size_t),
                ("PriorityClass", ctypes.c_uint32),
                ("SchedulingClass", ctypes.c_uint32),
            ]

        class IoCounters(ctypes.Structure):
            _fields_ = [(name, ctypes.c_ulonglong) for name in (
                "ReadOperationCount", "WriteOperationCount", "OtherOperationCount",
                "ReadTransferCount", "WriteTransferCount", "OtherTransferCount",
            )]

        class ExtendedLimit(ctypes.Structure):
            _fields_ = [
                ("BasicLimitInformation", BasicLimit),
                ("IoInfo", IoCounters),
                ("ProcessMemoryLimit", ctypes.c_size_t),
                ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t),
                ("PeakJobMemoryUsed", ctypes.c_size_t),
            ]

        info = ExtendedLimit()
        info.BasicLimitInformation.LimitFlags = 0x00002000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not kernel32.SetInformationJobObject(handle, 9, ctypes.byref(info), ctypes.sizeof(info)):
            error = ctypes.get_last_error()
            kernel32.CloseHandle(handle)
            raise OSError(error, "SetInformationJobObject failed")
        self.handle = handle

    def assign(self, process: subprocess.Popen[object]) -> None:
        if self.handle is None:
            return
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        if not kernel32.AssignProcessToJobObject(self.handle, process._handle):  # type: ignore[attr-defined]
            raise OSError(ctypes.get_last_error(), "AssignProcessToJobObject failed")

    def close(self) -> None:
        if self.handle is not None:
            ctypes.WinDLL("kernel32", use_last_error=True).CloseHandle(self.handle)
            self.handle = None


def spawn_isolated(
    argv: list[str],
    *,
    cwd: Path,
    log: TextIO,
    env: dict[str, str] | None = None,
) -> tuple[subprocess.Popen[object], WindowsJob]:
    creationflags = 0
    start_new_session = os.name != "nt"
    if os.name == "nt":
        creationflags = subprocess.CREATE_NEW_PROCESS_GROUP
    process = subprocess.Popen(
        argv,
        cwd=str(cwd),
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=subprocess.STDOUT,
        shell=False,
        creationflags=creationflags,
        start_new_session=start_new_session,
    )
    job = WindowsJob()
    try:
        job.assign(process)
    except Exception:
        with contextlib.suppress(Exception):
            process.kill()
        job.close()
        raise
    return process, job


def terminate_isolated(process: subprocess.Popen[object], job: WindowsJob) -> None:
    if process.poll() is not None:
        job.close()
        return
    if os.name == "nt":
        with contextlib.suppress(OSError):
            process.send_signal(signal.CTRL_BREAK_EVENT)
    else:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGTERM)
    deadline = time.monotonic() + GRACEFUL_STOP_SECONDS
    while process.poll() is None and time.monotonic() < deadline:
        time.sleep(0.1)
    job.close()
    if process.poll() is None:
        with contextlib.suppress(Exception):
            process.kill()
    with contextlib.suppress(subprocess.TimeoutExpired):
        process.wait(timeout=5)


def wait_isolated(
    process: subprocess.Popen[object],
    job: WindowsJob,
    *,
    timeout: float,
    heartbeat: Callable[[], None],
) -> tuple[int | None, bool]:
    deadline = time.monotonic() + timeout
    while process.poll() is None:
        heartbeat()
        if time.monotonic() >= deadline:
            terminate_isolated(process, job)
            return process.poll(), True
        time.sleep(0.2)
    code = process.returncode
    job.close()
    return code, False


@contextlib.contextmanager
def acquired_lease(manager: LeaseManager) -> Iterator[str | None]:
    abandoned = manager.acquire()
    try:
        yield abandoned
    finally:
        manager.release()
