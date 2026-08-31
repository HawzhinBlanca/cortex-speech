"""Durable process and evidence primitives for :mod:`verify_10`.

This module is intentionally dependency-free so the verifier can supervise its own
fault drills before the application environment is trusted.
"""

from __future__ import annotations

import contextlib
import ctypes
import hashlib
import json
import math
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


def _durable_replace(source: Path, destination: Path) -> None:
    """Atomically replace ``destination`` and request durable rename metadata.

    ``os.replace`` gives Windows atomic name replacement but no write-through guarantee.  The proof
    pointer is the release-status authority, so returning before NTFS has committed that rename leaves
    a power-loss window in which a caller observed a published verdict that never reached disk.
    ``MOVEFILE_WRITE_THROUGH`` closes that window after the temporary file itself has been fsynced.
    """

    if os.name != "nt":
        os.replace(source, destination)
        _fsync_directory(destination.parent)
        return

    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    move_file_ex = kernel32.MoveFileExW
    move_file_ex.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD]
    move_file_ex.restype = wintypes.BOOL
    movefile_replace_existing = 0x00000001
    movefile_write_through = 0x00000008
    if not move_file_ex(
        str(source),
        str(destination),
        movefile_replace_existing | movefile_write_through,
    ):
        raise OSError(ctypes.get_last_error(), f"durable replace failed for {destination}")


def atomic_write_bytes(path: Path, content: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
    try:
        with temporary.open("xb") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        _durable_replace(temporary, path)
    except OSError as error:
        with contextlib.suppress(OSError):
            temporary.unlink()
        raise EvidenceError(f"atomic evidence write failed for {path}: {error}") from error


def atomic_write_json(path: Path, value: object) -> None:
    try:
        encoded = (
            json.dumps(
                value,
                ensure_ascii=False,
                indent=2,
                sort_keys=True,
                allow_nan=False,
            )
            + "\n"
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise EvidenceError(f"evidence JSON is not canonical or finite for {path}: {error}") from error
    atomic_write_bytes(path, encoded)


def _remove_publication_name_durably(path: Path) -> None:
    """Remove an authority name through a durable same-directory rename.

    A plain unlink has no Windows write-through equivalent.  Renaming the public name to a unique
    quarantine with ``MOVEFILE_WRITE_THROUGH`` first makes the authority disappear durably; failure
    to delete the now-non-authoritative quarantine is harmless and intentionally best-effort.
    """

    quarantine = path.with_name(f".{path.name}.{os.getpid()}.{uuid.uuid4().hex}.rollback")
    _durable_replace(path, quarantine)
    with contextlib.suppress(OSError):
        quarantine.unlink()
        _fsync_directory(quarantine.parent)


def publish_validated_json(
    path: Path,
    value: object,
    validator: Callable[[Path], object],
) -> None:
    """Validate a non-authoritative candidate before atomically publishing it.

    Writing the public pointer and validating it afterward briefly makes malformed or stale bytes
    authoritative.  A crash in that interval preserves the bad pointer.  The candidate lives beside
    the destination so the final rename is same-volume and atomic; only bytes that passed the exact
    consumer validator can replace the prior authority.
    """

    path.parent.mkdir(parents=True, exist_ok=True)
    candidate = path.with_name(f".{path.name}.{os.getpid()}.{uuid.uuid4().hex}.candidate")
    prior = path.read_bytes() if path.is_file() else None
    try:
        atomic_write_json(candidate, value)
        validator(candidate)
        _durable_replace(candidate, path)
        # Re-open the public name as a final filesystem/integration check.  The byte stream is the
        # already-validated candidate; no serialization occurs between validation and publication.
        try:
            validator(path)
        except BaseException as validation_error:
            try:
                if prior is None:
                    _remove_publication_name_durably(path)
                else:
                    atomic_write_bytes(path, prior)
            except BaseException as rollback_error:
                raise EvidenceError(
                    f"published authority failed validation and rollback failed for {path}: "
                    f"validation={validation_error}; rollback={rollback_error}"
                ) from rollback_error
            raise
    finally:
        with contextlib.suppress(OSError):
            candidate.unlink()


def read_json_object(path: Path) -> dict[str, object]:
    def reject_duplicate_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
        value: dict[str, object] = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate key {key!r}")
            value[key] = item
        return value

    try:
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=reject_duplicate_keys,
            parse_constant=lambda token: (_ for _ in ()).throw(
                ValueError(f"non-finite JSON number {token!r}")
            ),
        )
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
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
            text = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
            # Split after the parenthesized comm so an executable name containing spaces cannot
            # shift the field indexes: the remainder starts at field 3 (state).
            fields = text.rpartition(")")[2].split()
            if fields[0] in {"Z", "X", "x"}:
                # A zombie or dead entry keeps its /proc row (and starttime) until the parent
                # reaps it, but the process identity is gone. Windows reports the same condition
                # as "exited" via WaitForSingleObject; treat both sides identically.
                return None
            return fields[19]
        except (OSError, IndexError, UnicodeError):
            return None

    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    open_process = kernel32.OpenProcess
    open_process.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    open_process.restype = wintypes.HANDLE
    wait_for_single_object = kernel32.WaitForSingleObject
    wait_for_single_object.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    wait_for_single_object.restype = wintypes.DWORD
    get_process_times = kernel32.GetProcessTimes
    get_process_times.argtypes = [
        wintypes.HANDLE,
        ctypes.POINTER(wintypes.FILETIME),
        ctypes.POINTER(wintypes.FILETIME),
        ctypes.POINTER(wintypes.FILETIME),
        ctypes.POINTER(wintypes.FILETIME),
    ]
    get_process_times.restype = wintypes.BOOL
    close_handle = kernel32.CloseHandle
    close_handle.argtypes = [wintypes.HANDLE]
    close_handle.restype = wintypes.BOOL
    process_query_limited_information = 0x1000
    synchronize = 0x00100000
    handle = open_process(process_query_limited_information | synchronize, False, pid)
    if not handle:
        error = ctypes.get_last_error()
        if error == 87:  # ERROR_INVALID_PARAMETER: no process with this PID
            return None
        raise LeaseError(f"cannot inspect process {pid} identity: {ctypes.WinError(error)}")
    creation = wintypes.FILETIME()
    exit_time = wintypes.FILETIME()
    kernel = wintypes.FILETIME()
    user = wintypes.FILETIME()
    try:
        wait_object_0 = 0x00000000
        wait_timeout = 0x00000102
        wait_result = wait_for_single_object(handle, 0)
        if wait_result == wait_object_0:
            return None
        if wait_result != wait_timeout:
            raise LeaseError(f"cannot inspect process {pid} liveness (wait status 0x{wait_result:08x})")
        ok = get_process_times(
            handle,
            ctypes.byref(creation),
            ctypes.byref(exit_time),
            ctypes.byref(kernel),
            ctypes.byref(user),
        )
        if not ok:
            raise LeaseError(
                f"cannot inspect process {pid} creation time: {ctypes.WinError(ctypes.get_last_error())}"
            )
        creation_ticks = (creation.dwHighDateTime << 32) | creation.dwLowDateTime
        return str(creation_ticks)
    finally:
        close_handle(handle)


def _atomic_create(path: Path) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    return os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)


class _LeaseGuard:
    """Cross-process serialization for lease heartbeats and takeover's final check.

    On Windows a named kernel mutex leaves no stale filesystem lock. If an owner is killed, the next
    waiter receives ``WAIT_ABANDONED`` *while owning the mutex*, which is exactly the recovery
    behavior needed here. POSIX uses ``flock`` on a stable sidecar; kernel locks likewise disappear
    with the process even though the inert inode remains.
    """

    def __init__(self, lease_path: Path) -> None:
        self.lease_path = lease_path
        self._handle: int | None = None
        self._descriptor: int | None = None

    def __enter__(self) -> "_LeaseGuard":
        if os.name == "nt":
            from ctypes import wintypes

            kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
            create_mutex = kernel32.CreateMutexW
            create_mutex.argtypes = [
                ctypes.POINTER(_SecurityAttributes),
                wintypes.BOOL,
                wintypes.LPCWSTR,
            ]
            create_mutex.restype = wintypes.HANDLE
            wait_for_single_object = kernel32.WaitForSingleObject
            wait_for_single_object.argtypes = [wintypes.HANDLE, wintypes.DWORD]
            wait_for_single_object.restype = wintypes.DWORD
            close_handle = kernel32.CloseHandle
            close_handle.argtypes = [wintypes.HANDLE]
            close_handle.restype = wintypes.BOOL

            identity = hashlib.sha256(
                str(self.lease_path.resolve(strict=False)).lower().encode("utf-8")
            ).hexdigest()
            handle = create_mutex(None, False, f"Local\\CortexVerify10Lease-{identity}")
            if not handle:
                raise LeaseError(f"cannot create verifier lease mutex: {ctypes.WinError(ctypes.get_last_error())}")
            wait_object_0 = 0x00000000
            wait_abandoned = 0x00000080
            wait_timeout = 0x00000102
            result = wait_for_single_object(handle, 120_000)
            if result not in (wait_object_0, wait_abandoned):
                close_handle(handle)
                if result == wait_timeout:
                    raise LeaseError("timed out waiting for verifier lease serialization")
                raise LeaseError(f"verifier lease mutex wait failed with status 0x{result:08x}")
            self._handle = int(handle)
            return self

        import fcntl

        guard_path = self.lease_path.with_suffix(self.lease_path.suffix + ".guard")
        guard_path.parent.mkdir(parents=True, exist_ok=True)
        descriptor = os.open(guard_path, os.O_CREAT | os.O_RDWR, 0o600)
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX)
        except Exception:
            os.close(descriptor)
            raise
        self._descriptor = descriptor
        return self

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> None:
        if os.name == "nt":
            if self._handle is None:
                return
            from ctypes import wintypes

            kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
            release_mutex = kernel32.ReleaseMutex
            release_mutex.argtypes = [wintypes.HANDLE]
            release_mutex.restype = wintypes.BOOL
            close_handle = kernel32.CloseHandle
            close_handle.argtypes = [wintypes.HANDLE]
            close_handle.restype = wintypes.BOOL
            handle = wintypes.HANDLE(self._handle)
            release_mutex(handle)
            close_handle(handle)
            self._handle = None
            return

        if self._descriptor is not None:
            import fcntl

            with contextlib.suppress(OSError):
                fcntl.flock(self._descriptor, fcntl.LOCK_UN)
            os.close(self._descriptor)
            self._descriptor = None


def _before_takeover_final_recheck() -> None:
    """Injection seam for proving a last-moment heartbeat wins over takeover."""


def _finite_number(value: object) -> bool:
    return not isinstance(value, bool) and isinstance(value, (int, float)) and math.isfinite(float(value))


def _heartbeat_age(lease: dict[str, object]) -> float:
    monotonic_heartbeat = lease.get("heartbeatMonotonic")
    if _finite_number(monotonic_heartbeat):
        # monotonic() is shared across processes on the same boot. A negative delta means the record
        # came from a different monotonic epoch or an impossible future; either way, treat it as fresh.
        return max(0.0, time.monotonic() - float(monotonic_heartbeat))
    wall_heartbeat = lease.get("heartbeatUnix")
    if not _finite_number(wall_heartbeat):
        raise LeaseError("existing verifier lease has invalid heartbeatUnix")
    # Legacy leases have only wall time. A backward jump is safe (fresh); forward-jump protection is
    # provided for every lease written by this implementation through heartbeatMonotonic.
    return max(0.0, time.time() - float(wall_heartbeat))


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
        self.started_monotonic = time.monotonic()
        self._last_heartbeat = 0.0

    def _guard(self) -> _LeaseGuard:
        return _LeaseGuard(self.path)

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
            "startedMonotonic": self.started_monotonic,
            "heartbeatUnix": now,
            "heartbeatMonotonic": time.monotonic(),
            "heartbeatUtc": utc_now(),
        }

    def _write(self) -> None:
        with self._guard():
            current = read_json_object(self.path)
            triple = (
                current.get("pid"),
                current.get("processCreationTime"),
                current.get("runToken"),
            )
            if triple != (self.pid, self.creation_time, self.run_token):
                raise LeaseError("verifier lost lease ownership before heartbeat; refusing to overwrite it")
            atomic_write_json(self.path, self._document())
        self._last_heartbeat = time.monotonic()

    def _install_new_unlocked(self) -> None:
        descriptor = _atomic_create(self.path)
        try:
            payload = (json.dumps(self._document(), sort_keys=True) + "\n").encode("utf-8")
            os.write(descriptor, payload)
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        _fsync_directory(self.path.parent)
        self._last_heartbeat = time.monotonic()

    @staticmethod
    def _validated_lease(lease: dict[str, object]) -> tuple[int, str, str]:
        if lease.get("schema") != 1:
            raise LeaseError("existing verifier lease has invalid schema")
        if not isinstance(lease.get("runToken"), str) or not lease["runToken"]:
            raise LeaseError("existing verifier lease has invalid runToken")
        if isinstance(lease.get("pid"), bool) or not isinstance(lease.get("pid"), int):
            raise LeaseError("existing verifier lease has invalid pid")
        if int(lease["pid"]) <= 0:
            raise LeaseError("existing verifier lease has invalid pid")
        if not isinstance(lease.get("processCreationTime"), str) or not lease["processCreationTime"]:
            raise LeaseError("existing verifier lease has invalid processCreationTime")
        if not _finite_number(lease.get("heartbeatUnix")):
            raise LeaseError("existing verifier lease has invalid heartbeatUnix")
        if "heartbeatMonotonic" in lease and not _finite_number(lease.get("heartbeatMonotonic")):
            raise LeaseError("existing verifier lease has invalid heartbeatMonotonic")
        return int(lease["pid"]), str(lease["processCreationTime"]), str(lease["runToken"])

    def _takeover_document(self) -> dict[str, object]:
        return {
            "schema": 1,
            "runToken": self.run_token,
            "pid": self.pid,
            "processCreationTime": self.creation_time,
            "startedUnix": time.time(),
            "startedMonotonic": time.monotonic(),
        }

    @staticmethod
    def _validated_takeover(takeover: dict[str, object]) -> tuple[int, str, str]:
        if takeover.get("schema") != 1:
            raise LeaseError("existing takeover marker has invalid schema; recovery fails closed")
        if not isinstance(takeover.get("runToken"), str) or not takeover["runToken"]:
            raise LeaseError("existing takeover marker has invalid runToken; recovery fails closed")
        if isinstance(takeover.get("pid"), bool) or not isinstance(takeover.get("pid"), int):
            raise LeaseError("existing takeover marker has invalid pid; recovery fails closed")
        if not isinstance(takeover.get("processCreationTime"), str) or not takeover["processCreationTime"]:
            raise LeaseError("existing takeover marker has invalid processCreationTime; recovery fails closed")
        if not _finite_number(takeover.get("startedUnix")):
            raise LeaseError("existing takeover marker has invalid startedUnix; recovery fails closed")
        if not _finite_number(takeover.get("startedMonotonic")):
            raise LeaseError("existing takeover marker has invalid startedMonotonic; recovery fails closed")
        return (
            int(takeover["pid"]),
            str(takeover["processCreationTime"]),
            str(takeover["runToken"]),
        )

    def _recover_or_refuse_takeover_unlocked(self, takeover_path: Path) -> None:
        if not takeover_path.exists():
            return
        takeover = read_json_object(takeover_path)
        owner_pid, owner_creation, owner_token = self._validated_takeover(takeover)
        observed = process_creation_time(owner_pid)
        if observed == owner_creation:
            raise LeaseError(
                f"another verifier start is already auditing the lease "
                f"(pid {owner_pid}, token {owner_token})"
            )
        # No process with the recorded PID+creation identity remains. A reused PID is also safe here:
        # unlike lease takeover, no process is terminated; only the dead contender's marker is removed.
        takeover_path.unlink()

    def _claim_takeover_unlocked(self, takeover_path: Path) -> None:
        self._recover_or_refuse_takeover_unlocked(takeover_path)
        atomic_write_json(takeover_path, self._takeover_document())

    def _owns_takeover_unlocked(self, takeover_path: Path) -> bool:
        if not takeover_path.exists():
            return False
        takeover = read_json_object(takeover_path)
        triple = self._validated_takeover(takeover)
        return triple == (self.pid, self.creation_time, self.run_token)

    def acquire(self) -> str | None:
        """Acquire the lease and return an abandoned token when a stale run was replaced."""

        takeover_path = self.path.with_suffix(self.path.suffix + ".takeover")
        with self._guard():
            # A contender killed after publishing its identity leaves a recoverable marker. Refuse a
            # live exact identity; reclaim only when PID+creation proves that contender is gone.
            self._recover_or_refuse_takeover_unlocked(takeover_path)
            try:
                self._install_new_unlocked()
                return None
            except FileExistsError:
                self._claim_takeover_unlocked(takeover_path)

        try:
            lease = read_json_object(self.path)
            holder_pid, holder_creation, holder_token = self._validated_lease(lease)
            observed_creation = process_creation_time(holder_pid)
            initial_age = _heartbeat_age(lease)
            if observed_creation == holder_creation and initial_age <= STALE_AFTER_SECONDS:
                raise LeaseError(
                    f"verifier {holder_pid} is live "
                    f"(heartbeat {initial_age:.1f}s old, token {holder_token})"
                )
            if observed_creation is not None and observed_creation != holder_creation:
                raise LeaseError("lease PID was reused; holder identity is unknown and takeover fails closed")

            # Tests inject a heartbeat here. In production this is intentionally empty: it represents
            # the scheduling window that used to let a refreshed holder be killed.
            _before_takeover_final_recheck()

            with self._guard():
                if not self._owns_takeover_unlocked(takeover_path):
                    raise LeaseError("takeover ownership changed before the final lease audit")

                # This lock is also taken by heartbeat(). Re-read identity *and freshness* while it is
                # held, then keep it held through termination. Therefore a heartbeat either lands
                # before this read and wins, or cannot race between the read and the kill.
                current = read_json_object(self.path)
                current_identity = self._validated_lease(current)
                if current_identity != (holder_pid, holder_creation, holder_token):
                    raise LeaseError("lease identity changed during takeover; refusing termination")
                current_creation = process_creation_time(holder_pid)
                current_age = _heartbeat_age(current)
                if current_creation == holder_creation and current_age <= STALE_AFTER_SECONDS:
                    raise LeaseError(
                        f"verifier {holder_pid} refreshed its heartbeat during takeover "
                        f"({current_age:.1f}s old, token {holder_token})"
                    )
                if current_creation is not None and current_creation != holder_creation:
                    raise LeaseError("lease PID was reused during takeover; refusing termination")
                if current_creation == holder_creation:
                    _terminate_verified_process_tree(holder_pid, holder_creation)
                    if process_creation_time(holder_pid) == holder_creation:
                        raise LeaseError("verified stale process tree survived takeover")

                self.path.unlink()
                self._install_new_unlocked()
                return holder_token
        finally:
            with self._guard():
                with contextlib.suppress(FileNotFoundError, LeaseError, OSError):
                    if self._owns_takeover_unlocked(takeover_path):
                        takeover_path.unlink()

    def heartbeat(self, *, force: bool = False) -> None:
        if force or time.monotonic() - self._last_heartbeat >= HEARTBEAT_SECONDS:
            self._write()

    def update_gate(self, gate: str | None, child_pid: int | None) -> None:
        self.current_gate = gate
        self.child_pid = child_pid
        self.heartbeat(force=True)

    def release(self) -> None:
        with self._guard():
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
                    def reject_duplicate_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
                        value: dict[str, object] = {}
                        for key, item in pairs:
                            if key in value:
                                raise ValueError(f"duplicate key {key!r}")
                            value[key] = item
                        return value

                    last = json.loads(
                        lines[-1],
                        object_pairs_hook=reject_duplicate_keys,
                        parse_constant=lambda token: (_ for _ in ()).throw(
                            ValueError(f"non-finite JSON number {token!r}")
                        ),
                    )
                    if not isinstance(last, dict) or last.get("runToken") != run_token:
                        raise ValueError("journal tail is bound to another run")
                    sequence = last.get("sequence")
                    if isinstance(sequence, bool) or not isinstance(sequence, int) or sequence < 1:
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
            encoded = json.dumps(
                record,
                ensure_ascii=False,
                sort_keys=True,
                allow_nan=False,
            )
            self.path.parent.mkdir(parents=True, exist_ok=True)
            with self.path.open("a", encoding="utf-8", newline="\n") as handle:
                handle.write(encoded + "\n")
                handle.flush()
                os.fsync(handle.fileno())
        except (OSError, TypeError, ValueError) as error:
            raise EvidenceError(f"cannot append verifier evidence event {event}: {error}") from error
        return record


class _SecurityAttributes(ctypes.Structure):
    _fields_ = [
        ("nLength", ctypes.c_uint32),
        ("lpSecurityDescriptor", ctypes.c_void_p),
        ("bInheritHandle", ctypes.c_int),
    ]


class _BasicLimit(ctypes.Structure):
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


class _IoCounters(ctypes.Structure):
    _fields_ = [
        (name, ctypes.c_ulonglong)
        for name in (
            "ReadOperationCount",
            "WriteOperationCount",
            "OtherOperationCount",
            "ReadTransferCount",
            "WriteTransferCount",
            "OtherTransferCount",
        )
    ]


class _ExtendedLimit(ctypes.Structure):
    _fields_ = [
        ("BasicLimitInformation", _BasicLimit),
        ("IoInfo", _IoCounters),
        ("ProcessMemoryLimit", ctypes.c_size_t),
        ("JobMemoryLimit", ctypes.c_size_t),
        ("PeakProcessMemoryUsed", ctypes.c_size_t),
        ("PeakJobMemoryUsed", ctypes.c_size_t),
    ]


class _ThreadEntry32(ctypes.Structure):
    _fields_ = [
        ("dwSize", ctypes.c_uint32),
        ("cntUsage", ctypes.c_uint32),
        ("th32ThreadID", ctypes.c_uint32),
        ("th32OwnerProcessID", ctypes.c_uint32),
        ("tpBasePri", ctypes.c_long),
        ("tpDeltaPri", ctypes.c_long),
        ("dwFlags", ctypes.c_uint32),
    ]


def _windows_kernel32() -> object:
    """Return kernel32 with every used 64-bit-sensitive signature declared exactly."""

    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    signatures = {
        "CreateJobObjectW": (
            [ctypes.POINTER(_SecurityAttributes), wintypes.LPCWSTR],
            wintypes.HANDLE,
        ),
        "SetInformationJobObject": (
            [wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID, wintypes.DWORD],
            wintypes.BOOL,
        ),
        "AssignProcessToJobObject": ([wintypes.HANDLE, wintypes.HANDLE], wintypes.BOOL),
        "TerminateJobObject": ([wintypes.HANDLE, wintypes.UINT], wintypes.BOOL),
        "CloseHandle": ([wintypes.HANDLE], wintypes.BOOL),
        "CreateToolhelp32Snapshot": ([wintypes.DWORD, wintypes.DWORD], wintypes.HANDLE),
        "Thread32First": ([wintypes.HANDLE, ctypes.POINTER(_ThreadEntry32)], wintypes.BOOL),
        "Thread32Next": ([wintypes.HANDLE, ctypes.POINTER(_ThreadEntry32)], wintypes.BOOL),
        "OpenThread": ([wintypes.DWORD, wintypes.BOOL, wintypes.DWORD], wintypes.HANDLE),
        "ResumeThread": ([wintypes.HANDLE], wintypes.DWORD),
    }
    for name, (argtypes, restype) in signatures.items():
        function = getattr(kernel32, name)
        function.argtypes = argtypes
        function.restype = restype
    return kernel32


class WindowsJob:
    """Kill-on-close Windows Job Object; a no-op process-group wrapper elsewhere."""

    def __init__(self) -> None:
        self.handle: int | None = None
        if os.name != "nt":
            return
        kernel32 = _windows_kernel32()
        handle = kernel32.CreateJobObjectW(None, None)
        if not handle:
            raise OSError(ctypes.get_last_error(), "CreateJobObjectW failed")

        info = _ExtendedLimit()
        info.BasicLimitInformation.LimitFlags = 0x00002000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not kernel32.SetInformationJobObject(handle, 9, ctypes.byref(info), ctypes.sizeof(info)):
            error = ctypes.get_last_error()
            kernel32.CloseHandle(handle)
            raise OSError(error, "SetInformationJobObject failed")
        self.handle = int(handle)

    def assign(self, process: subprocess.Popen[object]) -> None:
        if self.handle is None:
            return
        from ctypes import wintypes

        kernel32 = _windows_kernel32()
        process_handle = wintypes.HANDLE(int(process._handle))  # type: ignore[attr-defined]
        if not kernel32.AssignProcessToJobObject(wintypes.HANDLE(self.handle), process_handle):
            raise OSError(ctypes.get_last_error(), "AssignProcessToJobObject failed")

    def resume(self, process: subprocess.Popen[object]) -> None:
        if self.handle is None:
            return
        from ctypes import wintypes

        kernel32 = _windows_kernel32()
        snapshot = kernel32.CreateToolhelp32Snapshot(0x00000004, 0)  # TH32CS_SNAPTHREAD
        invalid_handle = ctypes.c_void_p(-1).value
        if not snapshot or int(snapshot) == invalid_handle:
            raise OSError(ctypes.get_last_error(), "CreateToolhelp32Snapshot failed")
        thread_ids: list[int] = []
        try:
            entry = _ThreadEntry32()
            entry.dwSize = ctypes.sizeof(entry)
            more = kernel32.Thread32First(snapshot, ctypes.byref(entry))
            while more:
                if entry.th32OwnerProcessID == process.pid:
                    thread_ids.append(int(entry.th32ThreadID))
                entry.dwSize = ctypes.sizeof(entry)
                more = kernel32.Thread32Next(snapshot, ctypes.byref(entry))
        finally:
            kernel32.CloseHandle(snapshot)
        if not thread_ids:
            raise OSError("suspended process has no enumerable primary thread")

        thread_suspend_resume = 0x0002
        resume_failed = 0xFFFFFFFF
        for thread_id in thread_ids:
            thread = kernel32.OpenThread(thread_suspend_resume, False, thread_id)
            if not thread:
                raise OSError(ctypes.get_last_error(), f"OpenThread failed for {thread_id}")
            try:
                previous_count = kernel32.ResumeThread(thread)
                if previous_count == resume_failed:
                    raise OSError(ctypes.get_last_error(), f"ResumeThread failed for {thread_id}")
                if previous_count != 1:
                    raise OSError(
                        f"primary thread {thread_id} had unexpected suspend count {previous_count}; "
                        "refusing an unproven launch"
                    )
            finally:
                kernel32.CloseHandle(thread)

    def terminate(self, exit_code: int = 1) -> None:
        if self.handle is None:
            return
        from ctypes import wintypes

        kernel32 = _windows_kernel32()
        if not kernel32.TerminateJobObject(wintypes.HANDLE(self.handle), exit_code):
            raise OSError(ctypes.get_last_error(), "TerminateJobObject failed")

    def close(self) -> None:
        if self.handle is None:
            return
        from ctypes import wintypes

        handle = self.handle
        kernel32 = _windows_kernel32()
        if not kernel32.CloseHandle(wintypes.HANDLE(handle)):
            raise OSError(ctypes.get_last_error(), "CloseHandle(job) failed")
        self.handle = None


def _cleanup_failed_spawn(
    process: subprocess.Popen[object] | None,
    job: WindowsJob,
    creation_time: str | None,
    *,
    assigned: bool,
) -> None:
    cleanup_errors: list[str] = []
    if process is not None and process.poll() is None:
        if assigned and os.name == "nt":
            try:
                job.terminate(1)
            except Exception as error:
                cleanup_errors.append(f"job termination failed: {error}")
        if process.poll() is None and (
            creation_time is None or process_creation_time(process.pid) == creation_time
        ):
            try:
                process.kill()
            except Exception as error:
                cleanup_errors.append(f"process termination failed: {error}")
    try:
        job.close()
    except Exception as error:
        cleanup_errors.append(f"job close failed: {error}")
    if process is not None:
        deadline = time.monotonic() + 5
        while (
            creation_time is not None
            and process_creation_time(process.pid) == creation_time
            and time.monotonic() < deadline
        ):
            time.sleep(0.05)
        if creation_time is not None and process_creation_time(process.pid) == creation_time:
            # TerminateJobObject and Popen.kill are asynchronous. Retry the exact still-matching PID
            # once, then prove its creation identity disappeared instead of trusting cached poll().
            with contextlib.suppress(Exception):
                process.kill()
            deadline = time.monotonic() + 5
            while (
                process_creation_time(process.pid) == creation_time
                and time.monotonic() < deadline
            ):
                time.sleep(0.05)
            if process_creation_time(process.pid) == creation_time:
                raise RuntimeError(
                    "failed suspended worker survived containment cleanup"
                    + (f" ({'; '.join(cleanup_errors)})" if cleanup_errors else "")
                )
        with contextlib.suppress(subprocess.TimeoutExpired):
            process.wait(timeout=1)
    if cleanup_errors and process is None:
        raise RuntimeError("; ".join(cleanup_errors))


def spawn_isolated(
    argv: list[str],
    *,
    cwd: Path,
    log: TextIO,
    env: dict[str, str] | None = None,
) -> tuple[subprocess.Popen[object], WindowsJob]:
    # Create the kill-on-close boundary *before* CreateProcess. On Windows the worker starts
    # suspended, is assigned while no user instruction can execute, and is resumed only afterward.
    job = WindowsJob()
    process: subprocess.Popen[object] | None = None
    creation_time: str | None = None
    assigned = False
    creationflags = 0
    start_new_session = os.name != "nt"
    if os.name == "nt":
        create_suspended = 0x00000004
        creationflags = subprocess.CREATE_NEW_PROCESS_GROUP | create_suspended
    try:
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
        if os.name == "nt":
            creation_time = process_creation_time(process.pid)
            if creation_time is None:
                raise OSError("cannot bind the suspended worker to an OS process identity")
        job.assign(process)
        assigned = True
        job.resume(process)
    except Exception:
        _cleanup_failed_spawn(process, job, creation_time, assigned=assigned)
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
