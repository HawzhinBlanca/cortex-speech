#!/usr/bin/env python3
"""Exact-commit Git materialization and native link preflight for owner proof inputs."""

from __future__ import annotations

import hashlib
import ctypes
import os
import re
import subprocess
from pathlib import Path
from typing import Callable, Mapping

from owner_proof_platform import ProofInputError


FULL_GIT_SHA = re.compile(r"[0-9a-f]{40}")


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


class _WindowsKillJob:
    """Assign a suspended process before it can spawn, then kill the full tree on handle close."""

    def __init__(self) -> None:
        from ctypes import wintypes

        self.kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        signatures = {
            "CreateJobObjectW": ([ctypes.c_void_p, wintypes.LPCWSTR], wintypes.HANDLE),
            "SetInformationJobObject": (
                [wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID, wintypes.DWORD],
                wintypes.BOOL,
            ),
            "AssignProcessToJobObject": ([wintypes.HANDLE, wintypes.HANDLE], wintypes.BOOL),
            "CloseHandle": ([wintypes.HANDLE], wintypes.BOOL),
            "CreateToolhelp32Snapshot": ([wintypes.DWORD, wintypes.DWORD], wintypes.HANDLE),
            "Thread32First": ([wintypes.HANDLE, ctypes.POINTER(_ThreadEntry32)], wintypes.BOOL),
            "Thread32Next": ([wintypes.HANDLE, ctypes.POINTER(_ThreadEntry32)], wintypes.BOOL),
            "OpenThread": ([wintypes.DWORD, wintypes.BOOL, wintypes.DWORD], wintypes.HANDLE),
            "ResumeThread": ([wintypes.HANDLE], wintypes.DWORD),
        }
        for name, (argtypes, restype) in signatures.items():
            function = getattr(self.kernel32, name)
            function.argtypes = argtypes
            function.restype = restype
        handle = self.kernel32.CreateJobObjectW(None, None)
        if not handle:
            raise OSError(ctypes.get_last_error(), "owner-proof job object cannot be created")
        limits = _ExtendedLimit()
        limits.BasicLimitInformation.LimitFlags = 0x00002000
        if not self.kernel32.SetInformationJobObject(handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
            error = ctypes.get_last_error()
            self.kernel32.CloseHandle(handle)
            raise OSError(error, "owner-proof kill-on-close job cannot be configured")
        self.handle: int | None = int(handle)

    def assign_and_resume(self, process: subprocess.Popen[object]) -> None:
        from ctypes import wintypes

        assert self.handle is not None
        if not self.kernel32.AssignProcessToJobObject(
            wintypes.HANDLE(self.handle),
            wintypes.HANDLE(int(process._handle)),  # type: ignore[attr-defined]
        ):
            raise OSError(ctypes.get_last_error(), "owner-proof process cannot enter its kill job")
        snapshot = self.kernel32.CreateToolhelp32Snapshot(0x4, 0)
        if not snapshot or int(snapshot) == ctypes.c_void_p(-1).value:
            raise OSError(ctypes.get_last_error(), "owner-proof suspended thread snapshot failed")
        thread_ids: list[int] = []
        try:
            entry = _ThreadEntry32()
            entry.dwSize = ctypes.sizeof(entry)
            more = self.kernel32.Thread32First(snapshot, ctypes.byref(entry))
            while more:
                if entry.th32OwnerProcessID == process.pid:
                    thread_ids.append(int(entry.th32ThreadID))
                entry.dwSize = ctypes.sizeof(entry)
                more = self.kernel32.Thread32Next(snapshot, ctypes.byref(entry))
        finally:
            self.kernel32.CloseHandle(snapshot)
        if len(thread_ids) != 1:
            raise OSError("owner-proof suspended process lacks one exact primary thread")
        thread = self.kernel32.OpenThread(0x2, False, thread_ids[0])
        if not thread:
            raise OSError(ctypes.get_last_error(), "owner-proof primary thread cannot be opened")
        try:
            previous = self.kernel32.ResumeThread(thread)
            if previous != 1:
                raise OSError("owner-proof primary thread has an unexpected suspend count")
        finally:
            self.kernel32.CloseHandle(thread)

    def close(self) -> None:
        if self.handle is None:
            return
        from ctypes import wintypes

        handle = self.handle
        self.handle = None
        if not self.kernel32.CloseHandle(wintypes.HANDLE(handle)):
            raise OSError(ctypes.get_last_error(), "owner-proof job handle cannot be closed")


def run_contained(
    command: list[str],
    *,
    cwd: Path,
    env: Mapping[str, str] | None,
    timeout: int,
    capture_output: bool = False,
    text: bool = False,
    encoding: str | None = None,
    errors: str | None = None,
    stdin: int | None = subprocess.DEVNULL,
    stdout: int | None = None,
    stderr: int | None = None,
) -> subprocess.CompletedProcess[object]:
    """Run one command with a kill-on-close Windows process-tree boundary."""
    if os.name != "nt":
        return subprocess.run(
            command,
            cwd=cwd,
            env=None if env is None else dict(env),
            capture_output=capture_output,
            text=text,
            encoding=encoding,
            errors=errors,
            stdin=stdin,
            stdout=stdout,
            stderr=stderr,
            check=False,
            shell=False,
            timeout=timeout,
        )
    if capture_output and (stdout is not None or stderr is not None):
        raise ValueError("capture_output cannot be combined with explicit output streams")
    job = _WindowsKillJob()
    process: subprocess.Popen[object] | None = None
    try:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env=None if env is None else dict(env),
            stdin=stdin,
            stdout=subprocess.PIPE if capture_output else stdout,
            stderr=subprocess.PIPE if capture_output else stderr,
            text=text,
            encoding=encoding,
            errors=errors,
            shell=False,
            creationflags=subprocess.CREATE_NEW_PROCESS_GROUP | 0x4 | subprocess.CREATE_NO_WINDOW,
        )
        job.assign_and_resume(process)
        try:
            captured_stdout, captured_stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            job.close()
            process.wait(timeout=10)
            raise
        return subprocess.CompletedProcess(command, process.returncode, captured_stdout, captured_stderr)
    except Exception:
        if process is not None and process.poll() is None:
            try:
                job.close()
            finally:
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        raise
    finally:
        job.close()


class GitAuthority:
    def __init__(self, binary: Path | None, repository: Path, minimal_environment: Callable[[], dict[str, str]]):
        self.binary = binary
        self.repository = repository
        self.minimal_environment = minimal_environment

    @property
    def program(self) -> str:
        return os.fspath(self.binary) if self.binary is not None else "git"

    def environment(self) -> dict[str, str] | None:
        if self.binary is None:
            return None
        environment = self.minimal_environment()
        system32 = Path(environment["SystemRoot"]) / "System32"
        environment.update(
            {
                "PATH": os.pathsep.join((os.fspath(self.binary.parent), os.fspath(system32))),
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_CONFIG_GLOBAL": "NUL",
                "GIT_TERMINAL_PROMPT": "0",
            }
        )
        return environment

    def _text(self, arguments: list[str], *, repository: Path, encoding: str = "ascii") -> str:
        try:
            result = run_contained(
                [self.program, *arguments],
                cwd=repository,
                env=self.environment(),
                capture_output=True,
                text=True,
                encoding=encoding,
                errors="strict",
                timeout=30,
            )
        except (OSError, subprocess.SubprocessError, UnicodeError) as error:
            raise ProofInputError("release Git identity cannot be resolved") from error
        if result.returncode != 0:
            raise ProofInputError("release Git identity cannot be resolved")
        return result.stdout

    def clean_sha(self, repository: Path | None = None) -> str:
        root = self.repository if repository is None else repository
        status = self._text(["status", "--porcelain=v1", "--untracked-files=all"], repository=root, encoding="utf-8")
        revision = self._text(["rev-parse", "HEAD"], repository=root).strip()
        if status:
            raise ProofInputError("proof inputs may be prepared only from a clean release tree")
        if FULL_GIT_SHA.fullmatch(revision) is None:
            raise ProofInputError("release Git SHA is not exact")
        return revision

    def tree(self, git_sha: str) -> str:
        if FULL_GIT_SHA.fullmatch(git_sha) is None:
            raise ProofInputError("release Git SHA is not exact")
        tree = self._text(["rev-parse", f"{git_sha}^{{tree}}"], repository=self.repository).strip()
        if FULL_GIT_SHA.fullmatch(tree) is None:
            raise ProofInputError("release source-tree identity cannot be resolved")
        return tree

    def commit_timestamp(self, git_sha: str) -> str:
        if FULL_GIT_SHA.fullmatch(git_sha) is None:
            raise ProofInputError("release Git SHA is not exact")
        timestamp = self._text(["show", "-s", "--format=%ct", git_sha], repository=self.repository).strip()
        if not timestamp.isascii() or not timestamp.isdigit() or timestamp.startswith("0"):
            raise ProofInputError("release commit timestamp cannot be resolved")
        return timestamp

    def blob_sha256(self, git_sha: str, relative: str) -> str:
        if not relative or "\\" in relative or relative.startswith("/") or ".." in Path(relative).parts:
            raise ProofInputError("Git blob path is not one safe relative path")
        try:
            result = run_contained(
                [self.program, "show", f"{git_sha}:{relative}"],
                cwd=self.repository,
                env=self.environment(),
                capture_output=True,
                timeout=30,
            )
        except (OSError, subprocess.SubprocessError) as error:
            raise ProofInputError("release source blob cannot be resolved") from error
        if result.returncode != 0 or len(result.stdout) > 2 * 1024 * 1024:
            raise ProofInputError("release source blob cannot be resolved")
        return hashlib.sha256(result.stdout).hexdigest()

    def materialize(self, build_root: Path, release_sha: str, assert_no_links: Callable[[Path], Path]) -> Path:
        source = build_root / "source"
        commands = (
            [
                self.program,
                "-c",
                "core.hooksPath=NUL",
                "-c",
                "core.autocrlf=false",
                "clone",
                "--no-checkout",
                "--no-hardlinks",
                "--local",
                os.fspath(self.repository),
                os.fspath(source),
            ],
            [
                self.program,
                "-c",
                "core.hooksPath=NUL",
                "-c",
                "core.autocrlf=false",
                "-C",
                os.fspath(source),
                "checkout",
                "--detach",
                "--force",
                release_sha,
            ],
        )
        for command in commands:
            try:
                result = run_contained(
                    command,
                    cwd=build_root,
                    env=self.environment(),
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=300,
                )
            except (OSError, subprocess.SubprocessError) as error:
                raise ProofInputError("exact release source materialization could not complete") from error
            if result.returncode != 0:
                raise ProofInputError("exact release source materialization failed")
        if self.clean_sha(source) != release_sha:
            raise ProofInputError("detached build source is not the exact clean release commit")
        project = assert_no_links(source / "cortex-speech-app" / "src-tauri")
        if not project.is_dir():
            raise ProofInputError("detached build source lacks the Rust application")
        return project


def run_link_preflight(
    rustc: Path,
    linker: Path,
    build_root: Path,
    environment: Mapping[str, str],
    atomic_write: Callable[[Path, bytes], None],
) -> None:
    source = build_root / "owner-proof-link-preflight.rs"
    output = build_root / "owner-proof-link-preflight.exe"
    atomic_write(source, b"fn main() {}\n")
    try:
        result = run_contained(
            [
                os.fspath(rustc),
                os.fspath(source),
                "--crate-name",
                "owner_proof_link_preflight",
                "--target",
                "x86_64-pc-windows-msvc",
                "-C",
                "target-feature=+crt-static",
                "-C",
                f"linker={linker}",
                "-o",
                os.fspath(output),
            ],
            cwd=build_root,
            env=dict(environment),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=180,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise ProofInputError("pinned Rust/MSVC compile-link preflight could not run") from error
    if result.returncode != 0 or not output.is_file():
        raise ProofInputError("pinned Rust/MSVC compile-link preflight failed")
