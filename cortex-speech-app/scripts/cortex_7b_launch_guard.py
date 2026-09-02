#!/usr/bin/env python3
"""Own one champion startup until the launcher proves pointer-bound readiness.

PowerShell 5.1 does not reliably execute ``finally`` after Ctrl+C.  A detached ``wsl.exe`` can
therefore keep loading a model after the launcher has been interrupted.  This small stdlib-only
guard runs inside that WSL session, requires a fresh launcher heartbeat until an explicit READY
transfer, and owns the exact child process group for cleanup.
"""

from __future__ import annotations

import argparse
from collections import deque
import ctypes
import os
from pathlib import Path
import re
import secrets
import signal
import subprocess
import sys
import threading
import time


STATE_DIR_ENV = "CORTEX_7B_LAUNCH_STATE_DIR"
DEFAULT_HEARTBEAT_TIMEOUT_SECONDS = 45.0
MAX_TAIL_BYTES = 64 * 1024
MAX_LOG_SEGMENT_BYTES = 2 * 1024 * 1024
LOG_READ_CHUNK_BYTES = 64 * 1024
MAX_RETAINED_INACTIVE_ATTEMPTS = 16
STALE_ATTEMPT_MAX_AGE_SECONDS = 7 * 24 * 60 * 60
PRUNE_SAFETY_AGE_SECONDS = 5 * 60
_PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
_ERROR_INVALID_PARAMETER = 87
_ATTEMPT_FILE = re.compile(
    r"^(?P<token>[0-9a-f]{32})\.(?:heartbeat|ready|stop|pid|log|log\.1)$"
)


def _validate_token(value: str) -> str:
    if len(value) != 32 or value != value.lower() or any(char not in "0123456789abcdef" for char in value):
        raise ValueError("launch token must be exactly 32 lowercase hexadecimal characters")
    return value


def _state_root() -> Path:
    configured = os.environ.get(STATE_DIR_ENV)
    root = Path(configured) if configured else Path.home() / ".cache" / "cortex-speech" / "champion-launch"
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if root.is_symlink() or not root.is_dir():
        raise ValueError(f"champion launch state root is not a real directory: {root}")
    try:
        root.chmod(0o700)
    except OSError:
        # Windows test hosts do not implement POSIX mode bits. Production runs inside WSL.
        if os.name != "nt":
            raise
    return root


def _paths(token: str) -> dict[str, Path]:
    root = _state_root()
    return {
        "heartbeat": root / f"{token}.heartbeat",
        "ready": root / f"{token}.ready",
        "stop": root / f"{token}.stop",
        "pid": root / f"{token}.pid",
        "log": root / f"{token}.log",
        "log_previous": root / f"{token}.log.1",
    }


def _atomic_write(path: Path, text: str) -> None:
    temp = path.with_name(f".{path.name}.{os.getpid()}.{secrets.token_hex(8)}.tmp")
    try:
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        if hasattr(os, "O_CLOEXEC"):
            flags |= os.O_CLOEXEC
        descriptor = os.open(temp, flags, 0o600)
        with os.fdopen(descriptor, "w", encoding="ascii", newline="\n") as handle:
            handle.write(text)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temp, path)
    finally:
        try:
            temp.unlink()
        except FileNotFoundError:
            pass


def _unlink(path: Path) -> None:
    try:
        path.unlink()
    except FileNotFoundError:
        pass


def _normalize_exit_code(code: int) -> int:
    return code if code >= 0 else 128 + min(127, abs(code))


def _terminate_child(child: subprocess.Popen[bytes], grace_seconds: float = 15.0) -> None:
    if child.poll() is not None:
        return
    try:
        if os.name == "nt":
            child.terminate()
        else:
            os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        child.wait(timeout=grace_seconds)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        if os.name == "nt":
            child.kill()
        else:
            os.killpg(child.pid, signal.SIGKILL)
    except ProcessLookupError:
        return
    try:
        child.wait(timeout=5.0)
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError("champion child process group survived SIGKILL") from exc


class _BoundedAttemptLog:
    """Two bounded log segments written by one drain thread and guard-status callers.

    The child never owns a log-file descriptor: it writes to a pipe that is continuously drained.
    Rotation can therefore close/replace files without leaving the child writing to an unlinked,
    disk-consuming inode. Log I/O failure is deliberately swallowed after recording the exception;
    draining continues so a full or unwritable evidence directory cannot deadlock the live child.
    """

    def __init__(self, path: Path, previous_path: Path):
        self.path = path
        self.previous_path = previous_path
        self._lock = threading.Lock()
        self.last_error: OSError | None = None

    @staticmethod
    def _write_all(descriptor: int, data: bytes) -> None:
        remaining = memoryview(data)
        while remaining:
            written = os.write(descriptor, remaining)
            if written <= 0:
                raise OSError("bounded launch log write made no progress")
            remaining = remaining[written:]

    def append_bytes(self, data: bytes) -> bool:
        if not data:
            return True
        # A single caller can never defeat the cap. The pipe pump uses smaller fixed chunks, while
        # this also protects status/error callers from an accidentally enormous message.
        if len(data) > MAX_LOG_SEGMENT_BYTES:
            data = data[-MAX_LOG_SEGMENT_BYTES:]
        with self._lock:
            try:
                try:
                    size = self.path.stat().st_size
                except FileNotFoundError:
                    size = 0
                if size > MAX_LOG_SEGMENT_BYTES or size + len(data) > MAX_LOG_SEGMENT_BYTES:
                    if self.path.exists():
                        os.replace(self.path, self.previous_path)
                    size = 0
                flags = os.O_WRONLY | os.O_CREAT | os.O_APPEND
                if hasattr(os, "O_CLOEXEC"):
                    flags |= os.O_CLOEXEC
                descriptor = os.open(self.path, flags, 0o600)
                try:
                    self._write_all(descriptor, data)
                finally:
                    os.close(descriptor)
                self.last_error = None
                return True
            except OSError as exc:
                self.last_error = exc
                return False

    def append_text(self, message: str) -> bool:
        return self.append_bytes((message.rstrip("\n") + "\n").encode("utf-8", "replace"))


def _append_log(path: Path, message: str) -> None:
    """Best-effort bounded error logging for failures outside an active supervisor."""
    _BoundedAttemptLog(path, path.with_name(path.name + ".1")).append_text(message)


def _windows_process_is_live(pid: int) -> bool:
    """Probe liveness with a real process handle, never with ``os.kill``.

    On Windows ``signal 0`` IS ``CTRL_C_EVENT``, so ``os.kill(pid, 0)`` reaches
    ``GenerateConsoleCtrlEvent`` rather than ``OpenProcess``.  Measured 2026-09-01 on this
    repository's pinned interpreter: against a LIVE pid it succeeds, meaning the "probe"
    actually generates a console Ctrl+C that every process on that console can receive; and
    against a dead pid its failure code depends on the console host -- ``WinError 87`` with no
    console attached, but ``WinError 11`` under the cmd.exe console ``npm`` creates.  A guard
    that keyed on 87 therefore reported every dead pid as live under CI and pruned nothing.
    ``OpenProcess`` answers the question that was actually being asked.
    """
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.argtypes = (ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong)
    kernel32.OpenProcess.restype = ctypes.c_void_p
    kernel32.CloseHandle.argtypes = (ctypes.c_void_p,)
    handle = kernel32.OpenProcess(_PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        # ERROR_INVALID_PARAMETER is Windows' "no such process".  Access denied and every other
        # ambiguity stay fail-closed/live so cleanup leaks a stale log rather than deleting a
        # live one.  ponytail: a pid whose handle is still held by an exited process's parent
        # reads live; that is the same conservative leak PID reuse already produces.
        return ctypes.get_last_error() != _ERROR_INVALID_PARAMETER
    kernel32.CloseHandle(handle)
    return True


def _pid_is_live_or_unknown(path: Path) -> bool:
    """Return false only when an exact published PID is definitely no longer alive."""
    try:
        raw = path.read_text(encoding="ascii").strip()
        pid = int(raw)
        if pid <= 0 or str(pid) != raw:
            return True
    except FileNotFoundError:
        return False
    except (OSError, ValueError):
        return True
    if os.name == "nt":
        return _windows_process_is_live(pid)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        # Permission and platform ambiguity must leak a stale log rather than delete a live one.
        return True
    return True


def prune_stale_attempts(root: Path, now: float | None = None) -> int:
    """Bound inactive-attempt storage without ever pruning a live or newly starting attempt."""
    current_time = time.time() if now is None else now
    attempts: dict[str, list[Path]] = {}
    try:
        entries = list(root.iterdir())
    except OSError:
        return 0
    for path in entries:
        match = _ATTEMPT_FILE.fullmatch(path.name)
        if match is not None:
            attempts.setdefault(match.group("token"), []).append(path)

    inactive: list[tuple[float, str, list[Path]]] = []
    for token, files in attempts.items():
        pid_path = root / f"{token}.pid"
        if pid_path.exists() and _pid_is_live_or_unknown(pid_path):
            continue
        try:
            newest_mtime = max(path.lstat().st_mtime for path in files)
        except OSError:
            continue
        inactive.append((max(0.0, current_time - newest_mtime), token, files))

    # Keep the newest bounded set even when old, except that the absolute seven-day limit wins.
    inactive.sort(key=lambda item: item[0])
    keep_tokens = {token for _age, token, _files in inactive[:MAX_RETAINED_INACTIVE_ATTEMPTS]}
    removed = 0
    for age, token, files in inactive:
        if age < PRUNE_SAFETY_AGE_SECONDS:
            continue
        if age < STALE_ATTEMPT_MAX_AGE_SECONDS and token in keep_tokens:
            continue
        # Recheck immediately before deletion. PID reuse produces a conservative leak, never a
        # false deletion; a new guard also has a five-minute safety window before count pruning.
        pid_path = root / f"{token}.pid"
        if pid_path.exists() and _pid_is_live_or_unknown(pid_path):
            continue
        for path in files:
            try:
                path.unlink()
                removed += 1
            except FileNotFoundError:
                pass
            except OSError:
                # Stale cleanup is maintenance, never launch authority.
                pass

    # Atomic-write debris has no token authority. Only remove old regular temp files, never a fresh
    # publication still being fsynced by a concurrent guard/signal process.
    for path in entries:
        if not (path.name.startswith(".") and path.name.endswith(".tmp")):
            continue
        try:
            if path.is_file() and not path.is_symlink() and current_time - path.stat().st_mtime >= STALE_ATTEMPT_MAX_AGE_SECONDS:
                path.unlink()
                removed += 1
        except (FileNotFoundError, OSError):
            pass
    return removed


def _heartbeat_age(path: Path) -> float:
    try:
        raw = path.read_text(encoding="ascii")
        sent_ns = int(raw)
    except (OSError, ValueError):
        return float("inf")
    now_ns = time.monotonic_ns()
    if sent_ns <= 0 or sent_ns > now_ns:
        return float("inf")
    return (now_ns - sent_ns) / 1_000_000_000


def signal_state(token: str, state: str) -> int:
    paths = _paths(_validate_token(token))
    if state == "ready" and not paths["pid"].is_file():
        raise ValueError("cannot transfer READY ownership before the launch guard publishes its child PID")
    _atomic_write(paths[state], str(time.monotonic_ns()))
    return 0


def _file_signature(path: Path) -> tuple[int, int, int] | None:
    try:
        metadata = path.stat()
        return metadata.st_ino, metadata.st_size, metadata.st_mtime_ns
    except (FileNotFoundError, OSError):
        return None


def _read_file_tail(path: Path, limit: int) -> bytes:
    if limit <= 0:
        return b""
    try:
        size = path.stat().st_size
        with path.open("rb") as handle:
            handle.seek(max(0, size - limit))
            return handle.read(limit)
    except (FileNotFoundError, OSError):
        return b""


def read_tail_lines(token: str, lines: int) -> list[str]:
    if not (1 <= lines <= 100):
        raise ValueError("tail line count must be from 1 through 100")
    paths = _paths(_validate_token(token))
    ordered = (paths["log_previous"], paths["log"])
    raw = b""
    # Rotation is atomic but can occur between reading the two segments. Retry a changed snapshot so
    # the displayed tail is recent and bounded; continuous rotation still returns the final attempt.
    for _attempt in range(3):
        before = tuple(_file_signature(path) for path in ordered)
        current = _read_file_tail(paths["log"], MAX_TAIL_BYTES)
        previous = _read_file_tail(paths["log_previous"], MAX_TAIL_BYTES - len(current))
        raw = previous + current
        after = tuple(_file_signature(path) for path in ordered)
        if before == after:
            break
    return [line.decode("utf-8", "replace") for line in deque(raw.splitlines(), maxlen=lines)]


def show_tail(token: str, lines: int) -> int:
    for line in read_tail_lines(token, lines):
        print(line)
    return 0


def supervise(token: str, heartbeat_timeout: float, command: list[str]) -> int:
    token = _validate_token(token)
    if not (0.1 <= heartbeat_timeout <= 300.0):
        raise ValueError("heartbeat timeout must be between 0.1 and 300 seconds")
    if command and command[0] == "--":
        command = command[1:]
    if not command or any(not isinstance(part, str) or not part for part in command):
        raise ValueError("launch guard requires a non-empty child command")

    paths = _paths(token)
    prune_stale_attempts(paths["log"].parent)
    if any(path.exists() for path in paths.values()):
        raise ValueError("launch token already has live or ambiguous state")
    _atomic_write(paths["heartbeat"], str(time.monotonic_ns()))
    attempt_log = _BoundedAttemptLog(paths["log"], paths["log_previous"])

    child: subprocess.Popen[bytes] | None = None
    drain_thread: threading.Thread | None = None
    drain_done = threading.Event()
    stop_signal: int | None = None

    def request_stop(signum: int, _frame: object) -> None:
        nonlocal stop_signal
        stop_signal = signum

    previous_handlers: dict[int, object] = {}
    guarded_signals = [signal.SIGINT, signal.SIGTERM]
    if hasattr(signal, "SIGHUP"):
        guarded_signals.append(signal.SIGHUP)
    for signum in guarded_signals:
        previous_handlers[signum] = signal.getsignal(signum)
        signal.signal(signum, request_stop)

    try:
        environment = os.environ.copy()
        environment["CORTEX_7B_LAUNCH_TOKEN"] = token
        child = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            env=environment,
            close_fds=True,
            bufsize=0,
            start_new_session=(os.name != "nt"),
        )
        if child.stdout is None:
            raise RuntimeError("champion child output pipe was not created")

        def drain_output() -> None:
            try:
                while True:
                    try:
                        chunk = os.read(child.stdout.fileno(), LOG_READ_CHUNK_BYTES)
                    except InterruptedError:
                        continue
                    except OSError:
                        return
                    if not chunk:
                        return
                    # Disk/log rotation errors never stop the pipe drain or block the child.
                    attempt_log.append_bytes(chunk)
            finally:
                drain_done.set()

        drain_thread = threading.Thread(
            target=drain_output,
            name=f"cortex-launch-log-{token[:8]}",
            daemon=True,
        )
        drain_thread.start()
        _atomic_write(paths["pid"], str(child.pid))
        ownership_transferred = False
        while True:
            code = child.poll()
            if code is not None:
                return _normalize_exit_code(code)
            if stop_signal is not None:
                attempt_log.append_text(f"launch guard received signal {stop_signal}; stopping child")
                _terminate_child(child)
                return 128 + stop_signal
            if paths["stop"].is_file():
                attempt_log.append_text("launch guard received an exact stop request; stopping child")
                _terminate_child(child)
                return 125
            if not ownership_transferred and paths["ready"].is_file():
                ownership_transferred = True
                _unlink(paths["heartbeat"])
                _unlink(paths["ready"])
                attempt_log.append_text("launch guard: pointer-bound READY ownership transferred")
            if not ownership_transferred and _heartbeat_age(paths["heartbeat"]) > heartbeat_timeout:
                attempt_log.append_text("launch guard: launcher heartbeat expired before READY; stopping child")
                _terminate_child(child)
                return 124
            time.sleep(0.1)
    finally:
        if child is not None and child.poll() is None:
            _terminate_child(child)
        if child is not None and child.stdout is not None:
            # Normally EOF wakes the drain immediately. A descendant with an inherited write handle
            # cannot hold guard shutdown forever; close only our read side after a bounded drain.
            if not drain_done.wait(timeout=2.0):
                child.stdout.close()
            if drain_thread is not None:
                drain_thread.join(timeout=1.0)
            try:
                child.stdout.close()
            except OSError:
                pass
        _unlink(paths["pid"])
        _unlink(paths["heartbeat"])
        _unlink(paths["ready"])
        _unlink(paths["stop"])
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="operation", required=True)

    supervise_parser = subparsers.add_parser("supervise")
    supervise_parser.add_argument("--token", required=True)
    supervise_parser.add_argument(
        "--heartbeat-timeout",
        type=float,
        default=DEFAULT_HEARTBEAT_TIMEOUT_SECONDS,
    )
    supervise_parser.add_argument("command", nargs=argparse.REMAINDER)

    signal_parser = subparsers.add_parser("signal")
    signal_parser.add_argument("--token", required=True)
    signal_parser.add_argument("--state", required=True, choices=("heartbeat", "ready", "stop"))

    tail_parser = subparsers.add_parser("tail")
    tail_parser.add_argument("--token", required=True)
    tail_parser.add_argument("--lines", type=int, default=15, choices=range(1, 101))
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if args.operation == "supervise":
            return supervise(args.token, args.heartbeat_timeout, args.command)
        if args.operation == "signal":
            return signal_state(args.token, args.state)
        if args.operation == "tail":
            return show_tail(args.token, args.lines)
        raise ValueError(f"unsupported launch-guard operation {args.operation!r}")
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as exc:
        message = f"champion launch guard: {exc}"
        token = getattr(args, "token", None)
        if isinstance(token, str):
            try:
                _append_log(_paths(_validate_token(token))["log"], message)
            except (OSError, ValueError):
                pass
        print(message, file=sys.stderr, flush=True)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
