#!/usr/bin/env python3
"""Narrow platform primitives for the owner-proof evidence boundary."""

from __future__ import annotations

import ctypes
import hashlib
import os
import shutil
import stat
import struct
import sys
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Callable, Collection, Mapping, Sequence


FILE_ATTRIBUTE_REPARSE_POINT = 0x400

_DELETE = 0x00010000
_READ_CONTROL = 0x00020000
_WRITE_DAC = 0x00040000
_FILE_READ_ATTRIBUTES = 0x80
_FILE_WRITE_ATTRIBUTES = 0x100
_FILE_SHARE_READ = 0x1
_FILE_SHARE_WRITE = 0x2
_FILE_SHARE_DELETE = 0x4
_OPEN_EXISTING = 3
_FILE_FLAG_OPEN_REPARSE_POINT = 0x00200000
_FILE_FLAG_BACKUP_SEMANTICS = 0x02000000
_DACL_SECURITY_INFORMATION = 0x4
_PROTECTED_DACL_SECURITY_INFORMATION = 0x80000000
_SE_DACL_PROTECTED = 0x1000
_ACCESS_DENIED_ACE_TYPE = 0x1
_INHERITED_ACE = 0x10
_WIN_WORLD_SID = 1
_FILE_ID_INFO_CLASS = 18


class _WindowsFileTime(ctypes.Structure):
    _fields_ = [("low", ctypes.c_uint32), ("high", ctypes.c_uint32)]


class _WindowsFileInformation(ctypes.Structure):
    _fields_ = [
        ("attributes", ctypes.c_uint32),
        ("creation", _WindowsFileTime),
        ("access", _WindowsFileTime),
        ("write", _WindowsFileTime),
        ("volume", ctypes.c_uint32),
        ("sizeHigh", ctypes.c_uint32),
        ("sizeLow", ctypes.c_uint32),
        ("links", ctypes.c_uint32),
        ("indexHigh", ctypes.c_uint32),
        ("indexLow", ctypes.c_uint32),
    ]


class _WindowsFileIdInformation(ctypes.Structure):
    # FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS FileIdInfo.
    _fields_ = [("volume", ctypes.c_uint64), ("file_id", ctypes.c_ubyte * 16)]


class _WindowsBasicInformation(ctypes.Structure):
    _fields_ = [
        ("creation", ctypes.c_int64),
        ("access", ctypes.c_int64),
        ("write", ctypes.c_int64),
        ("change", ctypes.c_int64),
        ("attributes", ctypes.c_uint32),
    ]


class _WindowsDisposition(ctypes.Structure):
    _fields_ = [("delete", ctypes.c_int)]


@dataclass(frozen=True)
class PublicationRecoveryEntry:
    """One exact object and its ordered cumulative publication-deny ACE states."""

    relative_path: str
    identity: tuple[int, int]
    is_directory: bool
    link_count: int
    protected_dacl_sha256: str
    deny_masks: tuple[int, ...]


class ProofInputError(RuntimeError):
    """A platform boundary could not be proven safe and exact."""


class PublicationDurabilityUnknown(ProofInputError):
    """The exact object was renamed, but final-name durability could not be proven."""


def _read_protected_dacl_bytes(handle: int, *, context: str) -> bytes:
    """Read one non-NULL protected DACL from the retained object handle."""
    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    advapi32.GetSecurityInfo.argtypes = [
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
    ]
    advapi32.GetSecurityInfo.restype = ctypes.c_uint32
    advapi32.GetSecurityDescriptorControl.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_uint16),
        ctypes.POINTER(ctypes.c_uint32),
    ]
    advapi32.GetSecurityDescriptorControl.restype = ctypes.c_int
    kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    kernel32.LocalFree.restype = ctypes.c_void_p
    dacl = ctypes.c_void_p()
    descriptor = ctypes.c_void_p()
    status = advapi32.GetSecurityInfo(
        handle,
        1,
        _DACL_SECURITY_INFORMATION,
        None,
        None,
        ctypes.byref(dacl),
        None,
        ctypes.byref(descriptor),
    )
    if status != 0:
        raise ProofInputError(f"{context} DACL cannot be read ({status})")
    try:
        if not descriptor.value or not dacl.value:
            raise ProofInputError(f"{context} has a NULL DACL")
        control = ctypes.c_uint16()
        revision = ctypes.c_uint32()
        if not advapi32.GetSecurityDescriptorControl(
            descriptor,
            ctypes.byref(control),
            ctypes.byref(revision),
        ):
            raise ProofInputError(f"{context} DACL control cannot be read")
        if not control.value & _SE_DACL_PROTECTED:
            raise ProofInputError(f"{context} DACL is not protected from inherited mutation")
        header = ctypes.string_at(dacl, 8)
        _acl_revision, _sbz1, acl_size, _ace_count, _sbz2 = struct.unpack("<BBHHH", header)
        if acl_size < 8 or acl_size > 0xFFFF:
            raise ProofInputError(f"{context} DACL size is invalid")
        return ctypes.string_at(dacl, acl_size)
    finally:
        if descriptor.value:
            kernel32.LocalFree(descriptor)


def _parse_acl_bytes(payload: bytes, *, context: str) -> tuple[int, tuple[bytes, ...]]:
    if len(payload) < 8:
        raise ProofInputError(f"{context} DACL is truncated")
    revision, _sbz1, acl_size, ace_count, _sbz2 = struct.unpack_from("<BBHHH", payload)
    if acl_size != len(payload) or revision == 0:
        raise ProofInputError(f"{context} DACL header is invalid")
    offset = 8
    aces: list[bytes] = []
    for _index in range(ace_count):
        if offset + 4 > acl_size:
            raise ProofInputError(f"{context} DACL ACE header is truncated")
        _ace_type, _ace_flags, ace_size = struct.unpack_from("<BBH", payload, offset)
        if ace_size < 4 or ace_size % 4 or offset + ace_size > acl_size:
            raise ProofInputError(f"{context} DACL ACE size is invalid")
        aces.append(payload[offset : offset + ace_size])
        offset += ace_size
    return revision, tuple(aces)


def _dacl_fingerprint(revision: int, aces: Sequence[bytes]) -> str:
    digest = hashlib.sha256(b"cortex-protected-dacl-v1\x00" + bytes((revision,)))
    for ace in aces:
        digest.update(struct.pack("<I", len(ace)))
        digest.update(ace)
    return digest.hexdigest()


def _protected_dacl_sha256(handle: int, *, context: str) -> str:
    revision, aces = _parse_acl_bytes(_read_protected_dacl_bytes(handle, context=context), context=context)
    return _dacl_fingerprint(revision, aces)


def _protect_handle_dacl(handle: int, path: Path, *, context: str) -> str:
    """Normalize one tool-owned publication object to a protected, non-NULL DACL."""
    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    advapi32.GetSecurityInfo.argtypes = [
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
    ]
    advapi32.GetSecurityInfo.restype = ctypes.c_uint32
    advapi32.SetSecurityInfo.argtypes = [
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_void_p,
    ]
    advapi32.SetSecurityInfo.restype = ctypes.c_uint32
    advapi32.GetSecurityDescriptorControl.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_uint16),
        ctypes.POINTER(ctypes.c_uint32),
    ]
    advapi32.GetSecurityDescriptorControl.restype = ctypes.c_int
    kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    dacl = ctypes.c_void_p()
    descriptor = ctypes.c_void_p()
    status = advapi32.GetSecurityInfo(
        handle,
        1,
        0x4,
        None,
        None,
        ctypes.byref(dacl),
        None,
        ctypes.byref(descriptor),
    )
    if status != 0:
        raise ProofInputError(f"{context} DACL cannot be read ({status})")
    try:
        if not dacl.value:
            raise ProofInputError(f"{context} has a NULL DACL and cannot become publication authority")
        status = advapi32.SetSecurityInfo(
            handle,
            1,
            _PROTECTED_DACL_SECURITY_INFORMATION | _DACL_SECURITY_INFORMATION,
            None,
            None,
            dacl,
            None,
        )
        if status != 0:
            raise ProofInputError(f"{context} DACL cannot be protected ({status})")
    finally:
        kernel32.LocalFree(descriptor)
    return _protected_dacl_sha256(handle, context=f"{context} normalized")


def absolute_lexical(path: Path) -> Path:
    raw = os.fspath(path)
    if not raw or "\x00" in raw:
        raise ProofInputError("proof-input paths cannot be empty or contain NUL")
    if any(part == ".." for part in path.parts):
        raise ProofInputError("parent traversal is not permitted in proof-input paths")
    return Path(os.path.abspath(raw))


def _canonical_relative_path(value: str) -> str:
    if value == ".":
        return value
    if not value or "\x00" in value or "\\" in value or ":" in value:
        raise ProofInputError("publication recovery path is not one canonical relative path")
    parsed = PurePosixPath(value)
    if parsed.is_absolute() or parsed.as_posix() != value or any(
        part in {"", ".", ".."} or part.endswith((" ", ".")) for part in parsed.parts
    ):
        raise ProofInputError("publication recovery path is not one canonical relative path")
    return value


def _enumerate_direct_tree(root: Path) -> dict[str, tuple[Path, bool, os.stat_result]]:
    """Enumerate an exact direct tree without following aliases or special objects."""
    safe_root = absolute_lexical(root)
    root_metadata = os.lstat(safe_root)
    if stat.S_ISLNK(root_metadata.st_mode) or metadata_reparse(root_metadata) or not stat.S_ISDIR(root_metadata.st_mode):
        raise ProofInputError("publication recovery root is indirect or not a directory")
    observed: dict[str, tuple[Path, bool, os.stat_result]] = {".": (safe_root, True, root_metadata)}
    pending = [safe_root]
    while pending:
        directory = pending.pop()
        with os.scandir(directory) as scanned:
            entries = list(scanned)
        for entry in entries:
            path = Path(entry.path)
            # CPython's Windows DirEntry cache may report zero identity/link fields even
            # though a direct lstat returns the real file identity. Recovery authorities
            # must therefore never use the cached DirEntry stat result.
            metadata = os.lstat(path)
            if stat.S_ISLNK(metadata.st_mode) or metadata_reparse(metadata):
                raise ProofInputError("publication recovery tree contains a symlink or reparse point")
            is_directory = stat.S_ISDIR(metadata.st_mode)
            if not is_directory and not stat.S_ISREG(metadata.st_mode):
                raise ProofInputError("publication recovery tree contains a special object")
            relative = _canonical_relative_path(path.relative_to(safe_root).as_posix())
            folded = relative.casefold()
            if any(existing.casefold() == folded for existing in observed):
                raise ProofInputError("publication recovery tree contains a case-colliding path")
            observed[relative] = (path, is_directory, metadata)
            if is_directory:
                pending.append(path)
    return observed


def metadata_reparse(metadata: os.stat_result) -> bool:
    return bool(getattr(metadata, "st_file_attributes", 0) & FILE_ATTRIBUTE_REPARSE_POINT)


def stable_file_sha256(path: Path) -> str:
    before = os.lstat(path)
    if stat.S_ISLNK(before.st_mode) or metadata_reparse(before) or not stat.S_ISREG(before.st_mode):
        raise ProofInputError("toolchain closure contains an indirect or non-regular file")
    digest = hashlib.sha256()
    with path.open("rb", buffering=0) as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
        opened = os.fstat(source.fileno())
    after = os.lstat(path)
    def state(item: os.stat_result) -> tuple[int, int, int, int]:
        return (item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns)
    if state(before) != state(opened) or state(opened) != state(after):
        raise ProofInputError("toolchain file changed while it was hashed")
    return digest.hexdigest()


def toolchain_tree_sha256(roots: dict[str, Path]) -> str:
    """Hash a versioned compiler/SDK closure without persisting private installation paths."""
    digest = hashlib.sha256()
    for label, root in sorted(roots.items()):
        if not label or "/" in label or "\\" in label:
            raise ProofInputError("toolchain closure label is invalid")
        root = absolute_lexical(root)
        root_metadata = os.lstat(root)
        if metadata_reparse(root_metadata) or not stat.S_ISDIR(root_metadata.st_mode):
            raise ProofInputError("toolchain closure root is not one direct directory")
        digest.update(f"R\0{label}\n".encode("utf-8"))
        pending = [root]
        while pending:
            directory = pending.pop()
            with os.scandir(directory) as scanned:
                entries = sorted(scanned, key=lambda entry: entry.name.casefold())
            for entry in entries:
                path = Path(entry.path)
                metadata = entry.stat(follow_symlinks=False)
                relative = path.relative_to(root).as_posix()
                if stat.S_ISLNK(metadata.st_mode) or metadata_reparse(metadata):
                    raise ProofInputError("toolchain closure contains a symlink or reparse point")
                if stat.S_ISDIR(metadata.st_mode):
                    digest.update(f"D\0{label}/{relative}\n".encode("utf-8"))
                    pending.append(path)
                elif stat.S_ISREG(metadata.st_mode):
                    file_hash = stable_file_sha256(path)
                    digest.update(f"F\0{label}/{relative}\0{metadata.st_size}\0{file_hash}\n".encode("utf-8"))
                else:
                    raise ProofInputError("toolchain closure contains a special filesystem entry")
    return digest.hexdigest()


@dataclass(frozen=True)
class WindowsBuildTools:
    msvc_root: Path
    sdk_include: Path
    sdk_lib: Path
    msvc_bin: Path
    sdk_bin: Path
    cl: Path
    link: Path
    lib: Path
    rc: Path
    mt: Path

    def environment(self, base: dict[str, str]) -> dict[str, str]:
        environment = dict(base)
        msvc_include = self.msvc_root / "include"
        environment.update(
            {
                "PATH": os.pathsep.join((os.fspath(self.msvc_bin), os.fspath(self.sdk_bin))),
                "INCLUDE": os.pathsep.join(
                    os.fspath(path)
                    for path in (
                        msvc_include,
                        self.sdk_include / "ucrt",
                        self.sdk_include / "shared",
                        self.sdk_include / "um",
                        self.sdk_include / "winrt",
                        self.sdk_include / "cppwinrt",
                    )
                ),
                "LIB": os.pathsep.join(
                    os.fspath(path)
                    for path in (
                        self.msvc_root / "lib" / "x64",
                        self.sdk_lib / "ucrt" / "x64",
                        self.sdk_lib / "um" / "x64",
                    )
                ),
                "CC_x86_64_pc_windows_msvc": os.fspath(self.cl),
                "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER": os.fspath(self.link),
            }
        )
        return environment


class LockedToolchainTrees:
    """Retain no-write/no-delete handles for every executable/runtime closure entry."""

    def __init__(self, roots: list[Path]):
        self.handles: list[int] = []  # __del__ -> close() must be safe after a platform refusal
        if os.name != "nt":
            raise ProofInputError("toolchain tree locks require Windows")
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateFileW.argtypes = [
            ctypes.c_wchar_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_void_p,
        ]
        kernel32.CreateFileW.restype = ctypes.c_void_p
        try:
            pending = [absolute_lexical(root) for root in roots]
            while pending:
                path = pending.pop()
                metadata = os.lstat(path)
                directory = stat.S_ISDIR(metadata.st_mode)
                if stat.S_ISLNK(metadata.st_mode) or metadata_reparse(metadata) or (
                    not directory and not stat.S_ISREG(metadata.st_mode)
                ):
                    raise ProofInputError("toolchain lock closure contains an indirect or special entry")
                handle = kernel32.CreateFileW(
                    os.fspath(path),
                    0x80000000 | 0x80,  # GENERIC_READ | FILE_READ_ATTRIBUTES
                    0x1 | (0x2 if directory else 0),  # deny delete everywhere and writes to files
                    None,
                    3,
                    0x00200000 | 0x02000000,
                    None,
                )
                if handle in (None, ctypes.c_void_p(-1).value):
                    raise ProofInputError("toolchain closure cannot be locked against mutation")
                self.handles.append(int(handle))
                if directory:
                    with os.scandir(path) as entries:
                        pending.extend(Path(entry.path) for entry in entries)
        except Exception:
            self.close()
            raise

    def close(self) -> None:
        if not self.handles:
            return
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
        for handle in reversed(self.handles):
            kernel32.CloseHandle(handle)
        self.handles.clear()

    def __enter__(self) -> LockedToolchainTrees:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


class LockedFile:
    """Retain an ordinary file identity while denying writes, deletes, and namespace replacement."""

    def __init__(self, path: Path, *, require_single_link: bool = True, acl_authority: bool = False):
        # __del__ -> close() reads self.handle even when construction was refused; initialize it
        # before the platform refusal so a POSIX caller gets one typed error, not deallocator noise.
        self.handle: int | None = None
        self.protected_dacl_sha256: str | None = None
        if os.name != "nt":
            raise ProofInputError("owner-proof file identity locks require Windows")
        self.path = absolute_lexical(path)

        class FileTime(ctypes.Structure):
            _fields_ = [("low", ctypes.c_uint32), ("high", ctypes.c_uint32)]

        class Information(ctypes.Structure):
            _fields_ = [
                ("attributes", ctypes.c_uint32),
                ("creation", FileTime),
                ("access", FileTime),
                ("write", FileTime),
                ("volume", ctypes.c_uint32),
                ("sizeHigh", ctypes.c_uint32),
                ("sizeLow", ctypes.c_uint32),
                ("links", ctypes.c_uint32),
                ("indexHigh", ctypes.c_uint32),
                ("indexLow", ctypes.c_uint32),
            ]

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateFileW.argtypes = [
            ctypes.c_wchar_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_void_p,
        ]
        kernel32.CreateFileW.restype = ctypes.c_void_p
        kernel32.GetFileInformationByHandle.argtypes = [ctypes.c_void_p, ctypes.POINTER(Information)]
        kernel32.GetFileInformationByHandle.restype = ctypes.c_int
        kernel32.GetFinalPathNameByHandleW.argtypes = [
            ctypes.c_void_p,
            ctypes.c_wchar_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
        ]
        kernel32.GetFinalPathNameByHandleW.restype = ctypes.c_uint32
        handle = kernel32.CreateFileW(
            os.fspath(self.path),
            0x80000000 | 0x80 | (0x00020000 | 0x00040000 if acl_authority else 0),
            0x1,
            None,
            3,
            0x00200000,
            None,
        )
        if handle in (None, ctypes.c_void_p(-1).value):
            raise ProofInputError("proof-input file cannot be identity-locked")
        self.handle = int(handle)
        try:
            information = Information()
            if not kernel32.GetFileInformationByHandle(self.handle, ctypes.byref(information)):
                raise ProofInputError("locked proof-input file identity cannot be inspected")
            if information.attributes & (FILE_ATTRIBUTE_REPARSE_POINT | 0x10):
                raise ProofInputError("locked proof-input authority is indirect or not a regular file")
            if require_single_link and information.links != 1:
                raise ProofInputError("locked proof-input authority must have exactly one filesystem name")
            required = kernel32.GetFinalPathNameByHandleW(self.handle, None, 0, 0)
            if not required or required > 32768:
                raise ProofInputError("locked proof-input final path is unavailable")
            buffer = ctypes.create_unicode_buffer(required + 1)
            written = kernel32.GetFinalPathNameByHandleW(self.handle, buffer, len(buffer), 0)
            if not written or written >= len(buffer):
                raise ProofInputError("locked proof-input final path cannot be read")
            final_path = buffer.value
            if final_path.casefold().startswith("\\\\?\\unc\\"):
                final_path = "\\\\" + final_path[8:]
            elif final_path.casefold().startswith("\\\\?\\"):
                final_path = final_path[4:]
            if os.path.normcase(os.path.abspath(final_path)).rstrip("\\/") != os.path.normcase(self.path).rstrip("\\/"):
                raise ProofInputError("proof-input file resolved through an alias or changed while locking")
            if acl_authority:
                self.protected_dacl_sha256 = _protect_handle_dacl(
                    self.handle,
                    self.path,
                    context="owned publication file",
                )
            self.identity = (information.volume, (information.indexHigh << 32) | information.indexLow)
            self.links = information.links
            self.acl_authority = acl_authority
        except Exception:
            self.close()
            raise

    def verify(self) -> None:
        if self.handle is None:
            raise ProofInputError("proof-input file lock is closed")
        metadata = os.stat(self.path, follow_symlinks=False)
        if not self.matches_stat(metadata) or metadata.st_nlink != self.links:
            raise ProofInputError("locked proof-input file identity changed")

    def matches_stat(self, metadata: os.stat_result) -> bool:
        """Whether an os.stat() result names this locked file, in either Windows identity encoding."""
        return stat_matches_handle_identity(metadata, self.handle, self.identity)

    def close(self) -> None:
        if self.handle is None:
            return
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
        kernel32.CloseHandle(self.handle)
        self.handle = None

    def seal_self_deletion(self) -> ChildNamespaceSeal:
        if not self.acl_authority or self.handle is None:
            raise ProofInputError("locked proof-input file lacks deletion-sealing authority")
        self.verify()
        return ChildNamespaceSeal(self.handle, self.path, 0x00010000)

    def __enter__(self) -> LockedFile:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


class ChildNamespaceSeal:
    """Temporary ownership of an original DACL while a DELETE_CHILD deny ACE is committed or restored."""

    def __init__(self, handle: int, path: Path, permissions: int):
        if os.name != "nt":
            raise ProofInputError("child namespace seals require Windows")

        class Trustee(ctypes.Structure):
            pass

        trustee_pointer = ctypes.POINTER(Trustee)
        Trustee._fields_ = [
            ("multipleTrustee", trustee_pointer),
            ("multipleOperation", ctypes.c_int),
            ("form", ctypes.c_int),
            ("kind", ctypes.c_int),
            ("name", ctypes.c_wchar_p),
        ]

        class ExplicitAccess(ctypes.Structure):
            _fields_ = [
                ("permissions", ctypes.c_uint32),
                ("mode", ctypes.c_int),
                ("inheritance", ctypes.c_uint32),
                ("trustee", Trustee),
            ]

        advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        advapi32.CreateWellKnownSid.argtypes = [
            ctypes.c_int,
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_uint32),
        ]
        advapi32.CreateWellKnownSid.restype = ctypes.c_int
        advapi32.GetSecurityInfo.argtypes = [
            ctypes.c_void_p,
            ctypes.c_int,
            ctypes.c_uint32,
            ctypes.POINTER(ctypes.c_void_p),
            ctypes.POINTER(ctypes.c_void_p),
            ctypes.POINTER(ctypes.c_void_p),
            ctypes.POINTER(ctypes.c_void_p),
            ctypes.POINTER(ctypes.c_void_p),
        ]
        advapi32.GetSecurityInfo.restype = ctypes.c_uint32
        advapi32.SetEntriesInAclW.argtypes = [
            ctypes.c_uint32,
            ctypes.POINTER(ExplicitAccess),
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_void_p),
        ]
        advapi32.SetEntriesInAclW.restype = ctypes.c_uint32
        advapi32.SetSecurityInfo.argtypes = [
            ctypes.c_void_p,
            ctypes.c_int,
            ctypes.c_uint32,
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        advapi32.SetSecurityInfo.restype = ctypes.c_uint32
        advapi32.GetSecurityDescriptorControl.argtypes = [
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_uint16),
            ctypes.POINTER(ctypes.c_uint32),
        ]
        advapi32.GetSecurityDescriptorControl.restype = ctypes.c_int
        advapi32.SetFileSecurityW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_void_p]
        advapi32.SetFileSecurityW.restype = ctypes.c_int
        kernel32.LocalFree.argtypes = [ctypes.c_void_p]
        kernel32.LocalFree.restype = ctypes.c_void_p

        security_descriptor = ctypes.c_void_p()
        old_dacl = ctypes.c_void_p()
        status = advapi32.GetSecurityInfo(
            handle,
            1,
            0x4,
            None,
            None,
            ctypes.byref(old_dacl),
            None,
            ctypes.byref(security_descriptor),
        )
        if status != 0:
            raise ProofInputError(f"owned root DACL cannot be read ({status})")
        if not old_dacl.value:
            kernel32.LocalFree(security_descriptor)
            raise ProofInputError("owned namespace has a NULL DACL and cannot be sealed safely")
        control = ctypes.c_uint16()
        revision = ctypes.c_uint32()
        if not advapi32.GetSecurityDescriptorControl(
            security_descriptor,
            ctypes.byref(control),
            ctypes.byref(revision),
        ):
            kernel32.LocalFree(security_descriptor)
            raise ProofInputError("owned namespace DACL control cannot be read")
        self.handle = handle
        self.path = absolute_lexical(path)
        self.old_dacl = old_dacl
        self.security_descriptor: int | None = int(security_descriptor.value)
        self._advapi32 = advapi32
        self._kernel32 = kernel32
        if not control.value & 0x1000:
            self.close(restore=False)
            raise ProofInputError("owned namespace DACL is not protected from inherited mutation")

        sid_size = ctypes.c_uint32(68)
        sid = ctypes.create_string_buffer(sid_size.value)
        if not advapi32.CreateWellKnownSid(1, None, sid, ctypes.byref(sid_size)):
            self.close(restore=False)
            raise ProofInputError("Everyone SID cannot be constructed for the namespace seal")
        access = ExplicitAccess(
            permissions,
            3,
            0,
            Trustee(None, 0, 0, 5, ctypes.cast(sid, ctypes.c_wchar_p)),
        )
        new_dacl = ctypes.c_void_p()
        status = advapi32.SetEntriesInAclW(1, ctypes.byref(access), old_dacl, ctypes.byref(new_dacl))
        if status != 0:
            self.close(restore=False)
            raise ProofInputError(f"owned root child-namespace seal cannot be built ({status})")
        try:
            status = advapi32.SetSecurityInfo(handle, 1, 0x4, None, None, new_dacl, None)
            if status != 0:
                raise ProofInputError(f"owned root child namespace cannot be sealed ({status})")
        finally:
            kernel32.LocalFree(new_dacl)

    def close(self, *, restore: bool) -> None:
        if self.security_descriptor is None:
            return
        try:
            if restore:
                if not self._advapi32.SetFileSecurityW(
                    os.fspath(self.path),
                    0x4,
                    self.security_descriptor,
                ):
                    raise ProofInputError(
                        f"owned root effective DACL cannot be restored ({ctypes.get_last_error()})"
                    )
        finally:
            self._kernel32.LocalFree(self.security_descriptor)
            self.security_descriptor = None

    def commit(self) -> None:
        self.close(restore=False)

    def restore(self) -> None:
        self.close(restore=True)

    def __del__(self) -> None:
        if getattr(self, "security_descriptor", None) is not None:
            try:
                self.restore()
            except Exception:
                pass


class OwnedDirectoryLock:
    """Hold a directory identity so its pathname cannot be renamed or replaced mid-transaction."""

    def __init__(self, path: Path, *, publish: bool = False, pin_namespace: bool = True):
        if publish and not pin_namespace:
            raise ProofInputError("publication requires a namespace-pinning directory handle")
        self.path = absolute_lexical(path)
        self.publish = publish
        self.pin_namespace = pin_namespace
        self.handle: int | None = None
        self.descriptor: int | None = None
        self.protected_dacl_sha256: str | None = None
        self.links = 0
        if os.name == "nt":
            kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
            kernel32.CreateFileW.argtypes = [
                ctypes.c_wchar_p,
                ctypes.c_uint32,
                ctypes.c_uint32,
                ctypes.c_void_p,
                ctypes.c_uint32,
                ctypes.c_uint32,
                ctypes.c_void_p,
            ]
            kernel32.CreateFileW.restype = ctypes.c_void_p
            handle = kernel32.CreateFileW(
                os.fspath(self.path),
                0x80
                | (0x00010000 if publish else 0)
                | (0x00020000 | 0x00040000 if publish else 0),  # DELETE plus READ_CONTROL/WRITE_DAC for publication
                0x1 | 0x2 | (0 if pin_namespace else 0x4),
                None,
                3,
                0x00200000 | 0x02000000,  # OPEN_REPARSE_POINT | BACKUP_SEMANTICS
                None,
            )
            if handle in (None, ctypes.c_void_p(-1).value):
                raise ProofInputError("owned directory cannot be identity-locked")
            self.handle = int(handle)
        else:
            self.descriptor = os.open(self.path, os.O_RDONLY)
        self.identity = self._identity()
        if publish and self.handle is not None:
            self.protected_dacl_sha256 = _protect_handle_dacl(
                self.handle,
                self.path,
                context="owned publication directory",
            )

    def _identity(self) -> tuple[int, int]:
        if os.name != "nt":
            assert self.descriptor is not None
            metadata = os.fstat(self.descriptor)
            if not stat.S_ISDIR(metadata.st_mode):
                raise ProofInputError("owned directory identity is not a direct directory")
            self.links = metadata.st_nlink
            return (metadata.st_dev, metadata.st_ino)

        class FileTime(ctypes.Structure):
            _fields_ = [("low", ctypes.c_uint32), ("high", ctypes.c_uint32)]

        class Information(ctypes.Structure):
            _fields_ = [
                ("attributes", ctypes.c_uint32),
                ("creation", FileTime),
                ("access", FileTime),
                ("write", FileTime),
                ("volume", ctypes.c_uint32),
                ("sizeHigh", ctypes.c_uint32),
                ("sizeLow", ctypes.c_uint32),
                ("links", ctypes.c_uint32),
                ("indexHigh", ctypes.c_uint32),
                ("indexLow", ctypes.c_uint32),
            ]

        assert self.handle is not None
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.GetFileInformationByHandle.argtypes = [ctypes.c_void_p, ctypes.POINTER(Information)]
        kernel32.GetFileInformationByHandle.restype = ctypes.c_int
        information = Information()
        if not kernel32.GetFileInformationByHandle(self.handle, ctypes.byref(information)):
            raise ProofInputError("owned directory identity cannot be inspected")
        if information.attributes & FILE_ATTRIBUTE_REPARSE_POINT or not information.attributes & 0x10:
            raise ProofInputError("owned directory identity is a reparse point or non-directory")
        self.links = information.links
        return (information.volume, (information.indexHigh << 32) | information.indexLow)

    def verify_path(self) -> None:
        with OwnedDirectoryLock(self.path, pin_namespace=False) as observed:
            identity = observed.identity
        if identity != self.identity:
            raise ProofInputError("owned directory path no longer identifies the locked transaction root")

    def seal_children(self, permissions: int = 0x2 | 0x4 | 0x40) -> ChildNamespaceSeal:
        if not self.publish or self.handle is None:
            raise ProofInputError("owned directory lock lacks child-namespace sealing authority")
        self.verify_path()
        return ChildNamespaceSeal(self.handle, self.path, permissions)

    def seal_self_deletion(self) -> ChildNamespaceSeal:
        if not self.publish or self.handle is None:
            raise ProofInputError("owned directory lock lacks self-deletion sealing authority")
        self.verify_path()
        return ChildNamespaceSeal(self.handle, self.path, 0x00010000)

    def publish_no_replace(
        self,
        destination: Path,
        flush: Callable[[Path], None],
        *,
        preflushed: bool = False,
    ) -> None:
        if not self.publish:
            raise ProofInputError("owned directory lock lacks publication authority")
        if os.path.lexists(destination):
            raise ProofInputError("preexisting proof directory cannot be overwritten")
        if not preflushed:
            flush(self.path)
            flush(self.path.parent)
        if os.name != "nt":
            self.verify_path()
            os.rename(self.path, destination)
            self.path = absolute_lexical(destination)
            try:
                flush(self.path.parent)
            except Exception as error:
                raise PublicationDurabilityUnknown(
                    "proof directory was published but final-name durability is unknown; rerun to reconcile"
                ) from error
            return

        class RenameInformation(ctypes.Structure):
            _fields_ = [
                ("replace", ctypes.c_ubyte),
                ("root", ctypes.c_void_p),
                ("length", ctypes.c_uint32),
                ("name", ctypes.c_wchar * 32768),
            ]

        target = os.fspath(absolute_lexical(destination))
        encoded_length = len(target.encode("utf-16-le"))
        information = RenameInformation(0, None, encoded_length, target)
        assert self.handle is not None
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.SetFileInformationByHandle.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
        kernel32.SetFileInformationByHandle.restype = ctypes.c_int
        size = RenameInformation.name.offset + encoded_length
        if not kernel32.SetFileInformationByHandle(self.handle, 3, ctypes.byref(information), size):
            raise ProofInputError(f"proof directory publication failed ({ctypes.get_last_error()})")
        self.path = absolute_lexical(destination)
        try:
            flush(self.path.parent)
        except Exception as error:
            raise PublicationDurabilityUnknown(
                "proof directory was published but final-name durability is unknown; rerun to reconcile"
            ) from error

    def close(self) -> None:
        if self.handle is not None:
            kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
            kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
            kernel32.CloseHandle.restype = ctypes.c_int
            kernel32.CloseHandle(self.handle)
            self.handle = None
        if self.descriptor is not None:
            os.close(self.descriptor)
            self.descriptor = None

    def __enter__(self) -> OwnedDirectoryLock:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


class NamedMutex:
    """Crash-released per-output ownership; no stale filesystem sentinel survives process death."""

    def __init__(self, namespace: str, identity: str):
        self.handle: int | None = None  # __del__ -> close() must be safe after a platform refusal
        if os.name != "nt":
            raise ProofInputError("owner-proof preparation mutex requires Windows")
        digest = hashlib.sha256(identity.encode("utf-8", errors="strict")).hexdigest()
        self.name = f"Local\\{namespace}-{digest}"
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateMutexW.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_wchar_p]
        kernel32.CreateMutexW.restype = ctypes.c_void_p
        kernel32.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
        kernel32.WaitForSingleObject.restype = ctypes.c_uint32
        handle = kernel32.CreateMutexW(None, 0, self.name)
        if not handle:
            raise ProofInputError(f"proof preparation mutex cannot be created ({ctypes.get_last_error()})")
        self.handle: int | None = int(handle)
        wait = kernel32.WaitForSingleObject(self.handle, 0)
        if wait == 0x102:
            self.close(release=False)
            raise ProofInputError("another proof-input preparation owns the output mutex")
        if wait not in (0, 0x80):
            code = ctypes.get_last_error()
            self.close(release=False)
            raise ProofInputError(f"proof preparation mutex cannot be acquired ({code})")

    def close(self, *, release: bool = True) -> None:
        if self.handle is None:
            return
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.ReleaseMutex.argtypes = [ctypes.c_void_p]
        kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
        if release:
            kernel32.ReleaseMutex(self.handle)
        kernel32.CloseHandle(self.handle)
        self.handle = None

    def __del__(self) -> None:
        self.close()


def owned_directory_identity(path: Path) -> tuple[int, int]:
    with OwnedDirectoryLock(path) as locked:
        return locked.identity


def access_is_denied(path: Path, desired_access: int, *, directory: bool) -> bool:
    """Prove a committed namespace ACL denies one destructive access right to this process."""
    if os.name != "nt":
        raise ProofInputError("namespace access proof requires Windows")
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateFileW.argtypes = [
        ctypes.c_wchar_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
    ]
    kernel32.CreateFileW.restype = ctypes.c_void_p
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    handle = kernel32.CreateFileW(
        os.fspath(absolute_lexical(path)),
        desired_access,
        0x1 | 0x2 | 0x4,
        None,
        3,
        0x00200000 | (0x02000000 if directory else 0),
        None,
    )
    if handle not in (None, ctypes.c_void_p(-1).value):
        kernel32.CloseHandle(handle)
        return False
    code = ctypes.get_last_error()
    if code != 5:
        raise ProofInputError(f"committed namespace access cannot be inspected ({code})")
    return True


def publish_sealed_directory(
    root: OwnedDirectoryLock,
    children: list[OwnedDirectoryLock],
    files: list[Path],
    destination: Path,
    flush: Callable[[Path], None],
    *,
    seal_root_deletion: bool = False,
    root_child_permissions: int = 0x2 | 0x4 | 0x40,
    child_permissions: dict[str, int] | None = None,
    recovery_plan_callback: Callable[[list[PublicationRecoveryEntry]], None] | None = None,
    allowed_unsealed_recovery_paths: Collection[str] = (),
) -> None:
    """Seal every child identity, release descendant handles, then publish the root without a swap gap."""
    if recovery_plan_callback is None and allowed_unsealed_recovery_paths:
        raise ProofInputError("publication recovery metadata requires a recovery-plan callback")
    file_locks: list[LockedFile] = []
    recovery_metadata_locks: list[LockedFile] = []
    recovery_metadata: dict[str, tuple[tuple[int, int], int, str]] = {}
    seal_groups: list[list[ChildNamespaceSeal]] = []

    def restore_seals() -> None:
        ordered = list(reversed(seal_groups))
        for group_index, group in enumerate(ordered):
            restoration_order = list(reversed(group))
            for seal_index, seal in enumerate(restoration_order):
                try:
                    seal.restore()
                except ProofInputError as error:
                    remaining = restoration_order[seal_index + 1 :]
                    for ancestor_group in ordered[group_index + 1 :]:
                        remaining.extend(ancestor_group)
                    for retained in remaining:
                        retained.commit()
                    seal_groups.clear()
                    raise error
        seal_groups.clear()

    def commit_seals() -> None:
        for group in seal_groups:
            for seal in group:
                seal.commit()
        seal_groups.clear()

    try:
        flush(root.path)
        flush(root.path.parent)
        file_locks = [LockedFile(path, acl_authority=True) for path in files]
        resolved_children: list[tuple[OwnedDirectoryLock, str, int]] = []
        for child in children:
            try:
                relative = _canonical_relative_path(child.path.relative_to(root.path).as_posix())
            except ValueError as error:
                raise ProofInputError("publication child escaped its owned root") from error
            permissions = (
                child_permissions[relative]
                if child_permissions is not None and relative in child_permissions
                else 0x2 | 0x40
                if child.path.name == "attempts"
                else 0x2 | 0x4 | 0x40
            )
            resolved_children.append((child, relative, permissions))
        if recovery_plan_callback is not None:
            if root.protected_dacl_sha256 is None:
                raise ProofInputError("publication root lacks a protected-DACL fingerprint")
            assert root.handle is not None
            _require_no_base_everyone_deny(root.handle, context="publication recovery root")
            plan = [
                PublicationRecoveryEntry(
                    relative_path=".",
                    identity=root.identity,
                    is_directory=True,
                    link_count=root.links,
                    protected_dacl_sha256=root.protected_dacl_sha256,
                    deny_masks=(
                        (root_child_permissions, root_child_permissions | _DELETE)
                        if seal_root_deletion
                        else (root_child_permissions,)
                    ),
                )
            ]
            for child, relative, permissions in resolved_children:
                if child.protected_dacl_sha256 is None:
                    raise ProofInputError("publication child lacks a protected-DACL fingerprint")
                assert child.handle is not None
                _require_no_base_everyone_deny(child.handle, context=f"publication recovery {relative}")
                plan.append(
                    PublicationRecoveryEntry(
                        relative_path=relative,
                        identity=child.identity,
                        is_directory=True,
                        link_count=child.links,
                        protected_dacl_sha256=child.protected_dacl_sha256,
                        deny_masks=(permissions, permissions | _DELETE),
                    )
                )
            for locked in file_locks:
                if locked.protected_dacl_sha256 is None:
                    raise ProofInputError("publication file lacks a protected-DACL fingerprint")
                try:
                    relative = _canonical_relative_path(locked.path.relative_to(root.path).as_posix())
                except ValueError as error:
                    raise ProofInputError("publication file escaped its owned root") from error
                assert locked.handle is not None
                _require_no_base_everyone_deny(locked.handle, context=f"publication recovery {relative}")
                plan.append(
                    PublicationRecoveryEntry(
                        relative_path=relative,
                        identity=locked.identity,
                        is_directory=False,
                        link_count=locked.links,
                        protected_dacl_sha256=locked.protected_dacl_sha256,
                        deny_masks=(_DELETE,),
                    )
                )
            plan.sort(key=lambda item: (item.relative_path != ".", item.relative_path.casefold()))
            planned_paths = {item.relative_path for item in plan}
            allowed_values = list(allowed_unsealed_recovery_paths)
            if any(not isinstance(value, str) for value in allowed_values):
                raise ProofInputError("publication recovery metadata path is not text")
            allowed_paths = {_canonical_relative_path(value) for value in allowed_values}
            if (
                len(allowed_paths) != len(allowed_values)
                or "." in allowed_paths
                or allowed_paths & planned_paths
                or len({value.casefold() for value in allowed_paths | planned_paths})
                != len(allowed_paths | planned_paths)
            ):
                raise ProofInputError("publication recovery metadata paths are duplicated or overlap the plan")
            for relative in allowed_paths:
                parent = PurePosixPath(relative).parent.as_posix()
                if parent == ".":
                    parent = "."
                if parent not in planned_paths or not next(
                    item.is_directory for item in plan if item.relative_path == parent
                ):
                    raise ProofInputError("publication recovery metadata path lacks a planned directory parent")
            before_callback = _enumerate_direct_tree(root.path)
            before_paths = set(before_callback)
            if not planned_paths <= before_paths or not before_paths <= planned_paths | allowed_paths:
                raise ProofInputError("publication recovery plan is not the exact pre-callback staging inventory")
            for relative in before_paths & allowed_paths:
                _path, is_directory, metadata = before_callback[relative]
                if is_directory or metadata.st_nlink != 1:
                    raise ProofInputError("publication recovery metadata must be a direct single-link file")
            file_identities = [item.identity for item in plan if not item.is_directory]
            if any(item.link_count != 1 for item in plan if not item.is_directory) or len(file_identities) != len(
                set(file_identities)
            ):
                raise ProofInputError("publication recovery plan contains a hardlinked file")
            recovery_plan_callback(plan)
            after_callback = _enumerate_direct_tree(root.path)
            if set(after_callback) != planned_paths | allowed_paths:
                raise ProofInputError("publication recovery callback did not create the exact metadata inventory")
            plan_by_path = {item.relative_path: item for item in plan}
            root.verify_path()
            assert root.handle is not None
            if _protected_dacl_sha256(root.handle, context="publication recovery root") != plan_by_path[
                "."
            ].protected_dacl_sha256:
                raise ProofInputError("publication recovery callback changed the root DACL")
            for child, relative, _permissions in resolved_children:
                child.verify_path()
                assert child.handle is not None
                if _protected_dacl_sha256(
                    child.handle,
                    context=f"publication recovery {relative}",
                ) != plan_by_path[relative].protected_dacl_sha256:
                    raise ProofInputError("publication recovery callback changed a child DACL")
            for locked in file_locks:
                locked.verify()
                relative = locked.path.relative_to(root.path).as_posix()
                assert locked.handle is not None
                if _protected_dacl_sha256(
                    locked.handle,
                    context=f"publication recovery {relative}",
                ) != plan_by_path[relative].protected_dacl_sha256:
                    raise ProofInputError("publication recovery callback changed a file DACL")
            for relative in sorted(allowed_paths, key=str.casefold):
                path, is_directory, metadata = after_callback[relative]
                if is_directory or metadata.st_nlink != 1:
                    raise ProofInputError("publication recovery metadata must remain a direct single-link file")
                locked = LockedFile(path)
                recovery_metadata_locks.append(locked)
                locked.verify()
                digest = stable_file_sha256(path)
                locked.verify()
                recovery_metadata[relative] = (locked.identity, metadata.st_size, digest)
            flush(root.path)
        root_group: list[ChildNamespaceSeal] = []
        seal_groups.append(root_group)
        root_group.append(root.seal_children(root_child_permissions))
        if seal_root_deletion:
            root_group.append(root.seal_self_deletion())
        for child, _relative, permissions in resolved_children:
            child_group: list[ChildNamespaceSeal] = []
            seal_groups.append(child_group)
            child_group.append(child.seal_children(permissions))
            child_group.append(child.seal_self_deletion())
        for locked in file_locks:
            file_group: list[ChildNamespaceSeal] = []
            seal_groups.append(file_group)
            file_group.append(locked.seal_self_deletion())
        for locked in reversed(file_locks):
            locked.close()
        file_locks.clear()
        for child in reversed(children):
            child.close()
        children.clear()
        for locked in reversed(recovery_metadata_locks):
            locked.close()
        recovery_metadata_locks.clear()
        try:
            root.publish_no_replace(destination, flush, preflushed=True)
        except PublicationDurabilityUnknown:
            commit_seals()
            raise
        except Exception:
            restore_seals()
            raise
        try:
            for relative, (identity, size, digest) in recovery_metadata.items():
                path = root.path.joinpath(*PurePosixPath(relative).parts)
                with LockedFile(path) as locked:
                    if locked.identity != identity or locked.links != 1 or os.lstat(path).st_size != size:
                        raise ProofInputError("published recovery metadata identity or size changed")
                    if stable_file_sha256(path) != digest:
                        raise ProofInputError("published recovery metadata bytes changed")
                    locked.verify()
        except Exception as error:
            commit_seals()
            raise PublicationDurabilityUnknown(
                "proof directory was published but recovery-metadata continuity is unknown; rerun to reconcile"
            ) from error
        commit_seals()
    finally:
        for locked in reversed(recovery_metadata_locks):
            locked.close()
        for locked in reversed(file_locks):
            locked.close()
        if seal_groups:
            restore_seals()


def fsync_directory(path: Path, *, desired_access: int = 0x40000000) -> None:
    if os.name != "nt":
        descriptor = os.open(path, os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        return
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateFileW.argtypes = [
        ctypes.c_wchar_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
    ]
    kernel32.CreateFileW.restype = ctypes.c_void_p
    kernel32.FlushFileBuffers.argtypes = [ctypes.c_void_p]
    kernel32.FlushFileBuffers.restype = ctypes.c_int
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel32.CloseHandle.restype = ctypes.c_int
    handle = kernel32.CreateFileW(
        os.fspath(absolute_lexical(path)),
        desired_access,
        0x1 | 0x2 | 0x4,
        None,
        3,
        0x02000000,
        None,
    )
    if handle in (None, ctypes.c_void_p(-1).value):
        raise ProofInputError(f"proof directory cannot be opened for durability flush ({ctypes.get_last_error()})")
    try:
        if not kernel32.FlushFileBuffers(handle):
            raise ProofInputError(f"proof directory durability flush failed ({ctypes.get_last_error()})")
    finally:
        kernel32.CloseHandle(handle)


def _coerce_recovery_entry(value: PublicationRecoveryEntry | Mapping[str, object]) -> PublicationRecoveryEntry:
    if isinstance(value, PublicationRecoveryEntry):
        entry = value
    elif isinstance(value, Mapping):
        snake = {
            "relative_path",
            "identity",
            "is_directory",
            "link_count",
            "protected_dacl_sha256",
            "deny_masks",
        }
        camel = {
            "relativePath",
            "identity",
            "isDirectory",
            "linkCount",
            "protectedDaclSha256",
            "denyMasks",
        }
        keys = set(value)
        if keys == snake:
            relative = value["relative_path"]
            is_directory = value["is_directory"]
            link_count = value["link_count"]
            fingerprint = value["protected_dacl_sha256"]
            deny_masks = value["deny_masks"]
        elif keys == camel:
            relative = value["relativePath"]
            is_directory = value["isDirectory"]
            link_count = value["linkCount"]
            fingerprint = value["protectedDaclSha256"]
            deny_masks = value["denyMasks"]
        else:
            raise ProofInputError("publication recovery plan entry fields are not exact")
        identity = value["identity"]
        if (
            not isinstance(relative, str)
            or type(is_directory) is not bool
            or type(link_count) is not int
            or not isinstance(fingerprint, str)
            or not isinstance(identity, (list, tuple))
            or len(identity) != 2
            or not isinstance(deny_masks, (list, tuple))
        ):
            raise ProofInputError("publication recovery plan entry has invalid field types")
        if any(type(item) is not int for item in (*identity, *deny_masks)):
            raise ProofInputError("publication recovery plan entry has invalid numeric fields")
        entry = PublicationRecoveryEntry(
            relative_path=relative,
            identity=(identity[0], identity[1]),
            is_directory=is_directory,
            link_count=link_count,
            protected_dacl_sha256=fingerprint,
            deny_masks=tuple(deny_masks),
        )
    else:
        raise ProofInputError("publication recovery plan entry is not an object")
    if not isinstance(entry.relative_path, str):
        raise ProofInputError("publication recovery plan path is not text")
    relative = _canonical_relative_path(entry.relative_path)
    if relative != entry.relative_path:
        raise ProofInputError("publication recovery plan path is not canonical")
    if (
        not isinstance(entry.identity, (list, tuple))
        or len(entry.identity) != 2
        or not isinstance(entry.deny_masks, (list, tuple))
    ):
        raise ProofInputError("publication recovery plan entry has invalid identity or deny states")
    if (
        type(entry.identity[0]) is not int
        or type(entry.identity[1]) is not int
        or entry.identity[0] < 0
        or entry.identity[1] < 0
        or type(entry.is_directory) is not bool
        or type(entry.link_count) is not int
        or entry.link_count < 1
        or not isinstance(entry.protected_dacl_sha256, str)
        or len(entry.protected_dacl_sha256) != 64
        or entry.protected_dacl_sha256 != entry.protected_dacl_sha256.lower()
        or any(character not in "0123456789abcdef" for character in entry.protected_dacl_sha256)
        or not entry.deny_masks
        or len(set(entry.deny_masks)) != len(entry.deny_masks)
        or any(type(mask) is not int or mask <= 0 or mask > 0xFFFFFFFF for mask in entry.deny_masks)
        or any(
            current == previous or current | previous != current
            for previous, current in zip(entry.deny_masks, entry.deny_masks[1:])
        )
    ):
        raise ProofInputError("publication recovery plan entry is invalid")
    return PublicationRecoveryEntry(
        relative_path=entry.relative_path,
        identity=(entry.identity[0], entry.identity[1]),
        is_directory=entry.is_directory,
        link_count=entry.link_count,
        protected_dacl_sha256=entry.protected_dacl_sha256,
        deny_masks=tuple(entry.deny_masks),
    )


def _windows_handle_path(kernel32: ctypes.WinDLL, handle: int, *, context: str) -> Path:
    kernel32.GetFinalPathNameByHandleW.argtypes = [
        ctypes.c_void_p,
        ctypes.c_wchar_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
    ]
    kernel32.GetFinalPathNameByHandleW.restype = ctypes.c_uint32
    required = kernel32.GetFinalPathNameByHandleW(handle, None, 0, 0)
    if not required or required > 32768:
        raise ProofInputError(f"{context} final path is unavailable")
    buffer = ctypes.create_unicode_buffer(required + 1)
    written = kernel32.GetFinalPathNameByHandleW(handle, buffer, len(buffer), 0)
    if not written or written >= len(buffer):
        raise ProofInputError(f"{context} final path cannot be read")
    value = buffer.value
    folded = value.casefold()
    if folded.startswith("\\\\?\\unc\\"):
        value = "\\\\" + value[8:]
    elif folded.startswith("\\\\?\\"):
        value = value[4:]
    return absolute_lexical(Path(value))


def _windows_handle_information(kernel32: ctypes.WinDLL, handle: int, *, context: str) -> _WindowsFileInformation:
    kernel32.GetFileInformationByHandle.argtypes = [ctypes.c_void_p, ctypes.POINTER(_WindowsFileInformation)]
    kernel32.GetFileInformationByHandle.restype = ctypes.c_int
    information = _WindowsFileInformation()
    if not kernel32.GetFileInformationByHandle(handle, ctypes.byref(information)):
        raise ProofInputError(f"{context} identity cannot be inspected")
    return information


def _windows_information_identity(information: _WindowsFileInformation) -> tuple[int, int]:
    return (information.volume, (information.indexHigh << 32) | information.indexLow)


def _windows_handle_file_id_identity(handle: int) -> tuple[int, int] | None:
    """FILE_ID_INFO identity for an open handle, or None when the volume cannot report one."""
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.GetFileInformationByHandleEx.argtypes = [
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_uint32,
    ]
    kernel32.GetFileInformationByHandleEx.restype = ctypes.c_int
    information = _WindowsFileIdInformation()
    if not kernel32.GetFileInformationByHandleEx(
        handle,
        _FILE_ID_INFO_CLASS,
        ctypes.byref(information),
        ctypes.sizeof(information),
    ):
        return None
    return (information.volume, int.from_bytes(bytes(information.file_id), "little"))


def stat_matches_handle_identity(
    metadata: os.stat_result,
    handle: int | None,
    identity: tuple[int, int],
) -> bool:
    """Answer whether os.stat() names the same volume and file as an open handle's identity.

    ``identity`` is ``(dwVolumeSerialNumber, 64-bit file index)`` as GetFileInformationByHandle
    reports it.  CPython >= 3.12 fills ``st_dev``/``st_ino`` on Windows from FILE_ID_INFO instead
    -- a 64-bit volume serial and a 128-bit file id -- so on 3.12 the two encodings name the same
    object with different numbers and a direct tuple comparison refuses every honest file.  Read
    the second encoding back from the same already-trusted handle and accept either one; nothing
    that is not the locked file can satisfy either.
    """
    observed = (metadata.st_dev, metadata.st_ino)
    if observed == identity:
        return True
    if os.name != "nt" or handle is None:
        return False
    extended = _windows_handle_file_id_identity(handle)
    return extended is not None and observed == extended


def _open_exact_windows_handle(
    kernel32: ctypes.WinDLL,
    path: Path,
    *,
    is_directory: bool,
    desired_access: int,
    share_mode: int,
    context: str,
) -> tuple[int, _WindowsFileInformation]:
    kernel32.CreateFileW.argtypes = [
        ctypes.c_wchar_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
    ]
    kernel32.CreateFileW.restype = ctypes.c_void_p
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel32.CloseHandle.restype = ctypes.c_int
    handle = kernel32.CreateFileW(
        os.fspath(absolute_lexical(path)),
        desired_access,
        share_mode,
        None,
        _OPEN_EXISTING,
        _FILE_FLAG_OPEN_REPARSE_POINT | (_FILE_FLAG_BACKUP_SEMANTICS if is_directory else 0),
        None,
    )
    if handle in (None, ctypes.c_void_p(-1).value):
        raise ProofInputError(f"{context} cannot be identity-locked ({ctypes.get_last_error()})")
    locked = int(handle)
    try:
        information = _windows_handle_information(kernel32, locked, context=context)
        observed_directory = bool(information.attributes & 0x10)
        if information.attributes & FILE_ATTRIBUTE_REPARSE_POINT or observed_directory != is_directory:
            raise ProofInputError(f"{context} is indirect or changed type")
        if os.path.normcase(os.fspath(_windows_handle_path(kernel32, locked, context=context))).rstrip(
            "\\/"
        ) != os.path.normcase(os.fspath(absolute_lexical(path))).rstrip("\\/"):
            raise ProofInputError(f"{context} resolved through an alias or changed pathname")
        return locked, information
    except Exception:
        kernel32.CloseHandle(locked)
        raise


def _everyone_sid(advapi32: ctypes.WinDLL) -> ctypes.Array[ctypes.c_char]:
    advapi32.CreateWellKnownSid.argtypes = [
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_uint32),
    ]
    advapi32.CreateWellKnownSid.restype = ctypes.c_int
    size = ctypes.c_uint32(68)
    sid = ctypes.create_string_buffer(size.value)
    if not advapi32.CreateWellKnownSid(_WIN_WORLD_SID, None, sid, ctypes.byref(size)):
        raise ProofInputError("publication recovery cannot construct the Everyone SID")
    return sid


def _is_exact_everyone_deny(
    advapi32: ctypes.WinDLL,
    ace: bytes,
    mask: int,
    everyone_sid: ctypes.Array[ctypes.c_char],
) -> bool:
    if len(ace) < 12:
        return False
    ace_type, ace_flags, ace_size = struct.unpack_from("<BBH", ace)
    if (
        ace_type != _ACCESS_DENIED_ACE_TYPE
        or ace_flags & _INHERITED_ACE
        or ace_size != len(ace)
        or struct.unpack_from("<I", ace, 4)[0] != mask
    ):
        return False
    advapi32.IsValidSid.argtypes = [ctypes.c_void_p]
    advapi32.IsValidSid.restype = ctypes.c_int
    advapi32.GetLengthSid.argtypes = [ctypes.c_void_p]
    advapi32.GetLengthSid.restype = ctypes.c_uint32
    advapi32.EqualSid.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    advapi32.EqualSid.restype = ctypes.c_int
    ace_buffer = ctypes.create_string_buffer(ace)
    sid_pointer = ctypes.c_void_p(ctypes.addressof(ace_buffer) + 8)
    if not advapi32.IsValidSid(sid_pointer):
        return False
    sid_length = advapi32.GetLengthSid(sid_pointer)
    return 0 < sid_length <= len(ace) - 8 and bool(advapi32.EqualSid(sid_pointer, everyone_sid))


def _require_no_base_everyone_deny(handle: int, *, context: str) -> None:
    _revision, aces = _parse_acl_bytes(_read_protected_dacl_bytes(handle, context=context), context=context)
    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    everyone = _everyone_sid(advapi32)
    for ace in aces:
        if len(ace) >= 12:
            ace_type, ace_flags, _ace_size = struct.unpack_from("<BBH", ace)
            mask = struct.unpack_from("<I", ace, 4)[0]
            if (
                ace_type == _ACCESS_DENIED_ACE_TYPE
                and not ace_flags & _INHERITED_ACE
                and _is_exact_everyone_deny(advapi32, ace, mask, everyone)
            ):
                raise ProofInputError(f"{context} already contains an explicit Everyone deny ACE")


def _publication_base_dacl(
    handle: int,
    entry: PublicationRecoveryEntry,
    *,
    allow_missing: bool,
) -> tuple[int, list[bytes], bool]:
    """Validate one live seal in memory and return its exact recorded base DACL."""
    context = f"publication recovery {entry.relative_path}"
    current = _read_protected_dacl_bytes(handle, context=context)
    revision, aces = _parse_acl_bytes(current, context=context)
    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    everyone = _everyone_sid(advapi32)
    remaining = list(aces)
    matching = [
        (index, state_index)
        for index, ace in enumerate(remaining)
        for state_index, mask in enumerate(entry.deny_masks)
        if _is_exact_everyone_deny(advapi32, ace, mask, everyone)
    ]
    if len(matching) > 1:
        raise ProofInputError(f"{context} contains multiple possible publication deny ACEs")
    removed = bool(matching)
    if matching:
        ace_index, state_index = matching[0]
        if not allow_missing and state_index != len(entry.deny_masks) - 1:
            raise ProofInputError(f"{context} contains only a partial publication deny state")
        remaining.pop(ace_index)
    elif not allow_missing:
        raise ProofInputError(f"{context} is missing its committed publication deny ACE")
    if _dacl_fingerprint(revision, remaining) != entry.protected_dacl_sha256:
        raise ProofInputError(f"{context} cannot be restored to its exact pre-seal DACL")
    return revision, remaining, removed


def _remove_publication_denies(
    handle: int,
    entry: PublicationRecoveryEntry,
    *,
    allow_missing: bool,
) -> None:
    context = f"publication recovery {entry.relative_path}"
    revision, remaining, removed = _publication_base_dacl(handle, entry, allow_missing=allow_missing)
    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    if removed:
        acl_size = 8 + sum(len(ace) for ace in remaining)
        if acl_size > 0xFFFF:
            raise ProofInputError(f"{context} restored DACL is too large")
        restored = struct.pack("<BBHHH", revision, 0, acl_size, len(remaining), 0) + b"".join(remaining)
        acl_buffer = ctypes.create_string_buffer(restored)
        advapi32.IsValidAcl.argtypes = [ctypes.c_void_p]
        advapi32.IsValidAcl.restype = ctypes.c_int
        if not advapi32.IsValidAcl(acl_buffer):
            raise ProofInputError(f"{context} restored DACL is invalid")
        advapi32.SetSecurityInfo.argtypes = [
            ctypes.c_void_p,
            ctypes.c_int,
            ctypes.c_uint32,
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        advapi32.SetSecurityInfo.restype = ctypes.c_uint32
        status = advapi32.SetSecurityInfo(
            handle,
            1,
            _PROTECTED_DACL_SECURITY_INFORMATION | _DACL_SECURITY_INFORMATION,
            None,
            None,
            ctypes.cast(acl_buffer, ctypes.c_void_p),
            None,
        )
        if status != 0:
            raise ProofInputError(f"{context} DACL cannot be restored ({status})")
    if _protected_dacl_sha256(handle, context=context) != entry.protected_dacl_sha256:
        raise ProofInputError(f"{context} DACL differs from its exact pre-seal fingerprint")


def path_matches_handle_identity(
    path: Path,
    *,
    is_directory: bool,
    observed: tuple[int, int],
    identity: tuple[int, int],
    context: str,
) -> bool:
    """Bind an os.lstat identity to a handle-encoded identity for the same path.

    Companion to :func:`stat_matches_handle_identity` for callers that scanned a directory
    without retaining a handle.  Opens the exact path and requires BOTH encodings to agree:
    the handle's GetFileInformationByHandle identity must equal ``identity`` and its
    FILE_ID_INFO identity must equal what the scan observed, so the scan, the handle and the
    plan are all pinned to one object.
    """
    if observed == identity:
        return True
    if os.name != "nt":
        return False
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel32.CloseHandle.restype = ctypes.c_int
    handle, information = _open_exact_windows_handle(
        kernel32,
        absolute_lexical(path),
        is_directory=is_directory,
        desired_access=_FILE_READ_ATTRIBUTES,
        share_mode=_FILE_SHARE_READ | _FILE_SHARE_WRITE | _FILE_SHARE_DELETE,
        context=context,
    )
    try:
        if _windows_information_identity(information) != identity:
            return False
        extended = _windows_handle_file_id_identity(handle)
        return extended is not None and observed == extended
    finally:
        kernel32.CloseHandle(handle)


def validate_owned_publication_plan(
    root: Path,
    expected_root_identity: tuple[int, int],
    plan_entries: Sequence[PublicationRecoveryEntry | Mapping[str, object]],
) -> None:
    """Non-mutating proof that every planned identity carries its exact final publication seal."""
    if os.name != "nt":
        raise ProofInputError("published recovery-plan validation requires Windows")
    entries = [_coerce_recovery_entry(value) for value in plan_entries]
    folded: set[str] = set()
    identities: set[tuple[int, int]] = set()
    for entry in entries:
        if entry.relative_path.casefold() in folded or entry.identity in identities:
            raise ProofInputError("published recovery plan contains a duplicate path or identity")
        folded.add(entry.relative_path.casefold())
        identities.add(entry.identity)
    if not entries or entries[0].relative_path != "." or entries[0].identity != expected_root_identity:
        raise ProofInputError("published recovery plan does not bind the exact root identity")
    safe_root = absolute_lexical(root)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel32.CloseHandle.restype = ctypes.c_int
    for entry in entries:
        path = safe_root if entry.relative_path == "." else safe_root.joinpath(*PurePosixPath(entry.relative_path).parts)
        handle, information = _open_exact_windows_handle(
            kernel32,
            path,
            is_directory=entry.is_directory,
            desired_access=_READ_CONTROL | _FILE_READ_ATTRIBUTES,
            share_mode=_FILE_SHARE_READ | _FILE_SHARE_WRITE | _FILE_SHARE_DELETE,
            context=f"published recovery plan {entry.relative_path}",
        )
        try:
            if _windows_information_identity(information) != entry.identity or information.links != entry.link_count:
                raise ProofInputError("published recovery-plan identity or link count changed")
            _publication_base_dacl(handle, entry, allow_missing=False)
        finally:
            kernel32.CloseHandle(handle)


def _clear_readonly_by_handle(kernel32: ctypes.WinDLL, handle: int, *, context: str) -> None:
    kernel32.GetFileInformationByHandleEx.argtypes = [
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_uint32,
    ]
    kernel32.GetFileInformationByHandleEx.restype = ctypes.c_int
    kernel32.SetFileInformationByHandle.argtypes = [
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_uint32,
    ]
    kernel32.SetFileInformationByHandle.restype = ctypes.c_int
    information = _WindowsBasicInformation()
    if not kernel32.GetFileInformationByHandleEx(
        handle,
        0,
        ctypes.byref(information),
        ctypes.sizeof(information),
    ):
        raise ProofInputError(f"{context} attributes cannot be inspected")
    if not information.attributes & 0x1:
        return
    information.attributes &= ~0x1
    if information.attributes == 0:
        information.attributes = 0x80
    if not kernel32.SetFileInformationByHandle(
        handle,
        0,
        ctypes.byref(information),
        ctypes.sizeof(information),
    ):
        raise ProofInputError(f"{context} readonly attribute cannot be cleared")


def _delete_by_handle(kernel32: ctypes.WinDLL, handle: int, *, context: str) -> None:
    kernel32.SetFileInformationByHandle.argtypes = [
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_uint32,
    ]
    kernel32.SetFileInformationByHandle.restype = ctypes.c_int
    disposition = _WindowsDisposition(1)
    if not kernel32.SetFileInformationByHandle(
        handle,
        4,
        ctypes.byref(disposition),
        ctypes.sizeof(disposition),
    ):
        raise ProofInputError(f"{context} cannot be deleted by identity ({ctypes.get_last_error()})")


def recover_owned_publication_staging(
    root: Path,
    expected_root_identity: tuple[int, int],
    plan_entries: Sequence[PublicationRecoveryEntry | Mapping[str, object]],
    recovery_metadata_delete_order: Sequence[str],
) -> None:
    """Reconcile and delete only an exact, journal-bound publication staging identity."""
    if os.name != "nt":
        raise ProofInputError("publication staging recovery requires Windows")
    entries = [_coerce_recovery_entry(value) for value in plan_entries]
    by_path: dict[str, PublicationRecoveryEntry] = {}
    folded_paths: set[str] = set()
    for entry in entries:
        folded = entry.relative_path.casefold()
        if folded in folded_paths:
            raise ProofInputError("publication recovery plan contains a duplicate path")
        folded_paths.add(folded)
        by_path[entry.relative_path] = entry
    if "." not in by_path or not by_path["."].is_directory or by_path["."].identity != expected_root_identity:
        raise ProofInputError("publication recovery plan does not bind the exact root identity")
    for relative, entry in by_path.items():
        if relative == ".":
            continue
        parent = PurePosixPath(relative).parent.as_posix()
        if parent == ".":
            parent = "."
        if parent not in by_path or not by_path[parent].is_directory:
            raise ProofInputError("publication recovery plan has a missing directory ancestor")
    file_entries = [entry for entry in entries if not entry.is_directory]
    if any(entry.link_count != 1 for entry in file_entries) or len({entry.identity for entry in file_entries}) != len(
        file_entries
    ):
        raise ProofInputError("publication recovery plan contains a hardlinked file")
    allowed_values = list(recovery_metadata_delete_order)
    if any(not isinstance(value, str) for value in allowed_values):
        raise ProofInputError("publication recovery metadata path is not text")
    allowed = {_canonical_relative_path(value) for value in allowed_values}
    if (
        len(allowed) != len(allowed_values)
        or "." in allowed
        or allowed & set(by_path)
        or len({value.casefold() for value in allowed | set(by_path)}) != len(allowed | set(by_path))
    ):
        raise ProofInputError("publication recovery metadata paths are duplicated or overlap the plan")
    for relative in allowed:
        parent = PurePosixPath(relative).parent.as_posix()
        if parent == ".":
            parent = "."
        if parent not in by_path or not by_path[parent].is_directory:
            raise ProofInputError("publication recovery metadata path lacks a planned directory parent")

    safe_root = absolute_lexical(root)
    observed = _enumerate_direct_tree(safe_root)
    if set(observed) - (set(by_path) | allowed):
        raise ProofInputError("publication recovery tree contains an unplanned object")
    if "." not in observed:
        raise ProofInputError("publication recovery root disappeared")

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel32.CloseHandle.restype = ctypes.c_int
    acl_handles: dict[str, tuple[int, _WindowsFileInformation]] = {}
    metadata_handles: dict[str, tuple[int, _WindowsFileInformation]] = {}
    delete_handles: dict[str, int] = {}
    authority: dict[str, tuple[bool, tuple[int, int], int]] = {}
    try:
        for relative in sorted(observed, key=lambda value: (value.count("/"), value.casefold())):
            path, observed_directory, metadata = observed[relative]
            entry = by_path.get(relative)
            if entry is None:
                if observed_directory or metadata.st_nlink != 1:
                    raise ProofInputError("publication recovery metadata is not a direct single-link file")
                handle, information = _open_exact_windows_handle(
                    kernel32,
                    path,
                    is_directory=False,
                    desired_access=_FILE_READ_ATTRIBUTES,
                    share_mode=_FILE_SHARE_READ | _FILE_SHARE_WRITE | _FILE_SHARE_DELETE,
                    context=f"publication recovery metadata {relative}",
                )
                metadata_handles[relative] = (handle, information)
                identity = _windows_information_identity(information)
                if not stat_matches_handle_identity(metadata, handle, identity) or information.links != 1:
                    raise ProofInputError("publication recovery metadata identity or link count changed")
                authority[relative] = (False, identity, 1)
                continue
            if observed_directory != entry.is_directory:
                raise ProofInputError("publication recovery object changed type")
            handle, information = _open_exact_windows_handle(
                kernel32,
                path,
                is_directory=entry.is_directory,
                desired_access=_READ_CONTROL | _WRITE_DAC | _FILE_READ_ATTRIBUTES,
                share_mode=_FILE_SHARE_READ | _FILE_SHARE_WRITE | _FILE_SHARE_DELETE,
                context=f"publication recovery {relative}",
            )
            acl_handles[relative] = (handle, information)
            identity = _windows_information_identity(information)
            if (
                identity != entry.identity
                or not stat_matches_handle_identity(metadata, handle, entry.identity)
                or information.links != entry.link_count
                or metadata.st_nlink != entry.link_count
            ):
                raise ProofInputError("publication recovery object identity or link count differs from its plan")
            authority[relative] = (entry.is_directory, entry.identity, entry.link_count)
        if _windows_information_identity(acl_handles["."][1]) != expected_root_identity:
            raise ProofInputError("publication recovery root identity changed")
        identities = [identity for _directory, identity, _links in authority.values()]
        if len(identities) != len(set(identities)):
            raise ProofInputError("publication recovery tree contains an internal hardlink or identity alias")
        for relative, (handle, _information) in acl_handles.items():
            _remove_publication_denies(handle, by_path[relative], allow_missing=True)

        repeated = _enumerate_direct_tree(safe_root)
        if set(repeated) != set(observed):
            raise ProofInputError("publication recovery inventory changed after ACL restoration")
        for relative in sorted(observed, key=lambda value: (value.count("/"), value.casefold())):
            path, _observed_directory, _metadata = repeated[relative]
            is_directory, expected_identity, expected_links = authority[relative]
            handle, information = _open_exact_windows_handle(
                kernel32,
                path,
                is_directory=is_directory,
                desired_access=_DELETE | _FILE_READ_ATTRIBUTES | _FILE_WRITE_ATTRIBUTES,
                share_mode=_FILE_SHARE_READ | _FILE_SHARE_WRITE,
                context=f"publication recovery delete {relative}",
            )
            if _windows_information_identity(information) != expected_identity or information.links != expected_links:
                kernel32.CloseHandle(handle)
                raise ProofInputError("publication recovery delete handle differs from its exact plan")
            delete_handles[relative] = handle
        final_inventory = _enumerate_direct_tree(safe_root)
        if set(final_inventory) != set(observed):
            raise ProofInputError("publication recovery inventory changed before deletion")
        for relative, (handle, _information) in list(acl_handles.items()):
            kernel32.CloseHandle(handle)
            del acl_handles[relative]
        for relative, (handle, _information) in list(metadata_handles.items()):
            kernel32.CloseHandle(handle)
            del metadata_handles[relative]
        # Recovery metadata is the authority that makes a partially deleted tree
        # safe to resume.  Keep it until every planned payload descendant and
        # directory is gone.  The caller orders the metadata so the owner journal
        # is deleted last; consequently, a crash after owner deletion can leave at
        # most an empty, safely reclaimable root.
        payload_delete_order = sorted(
            (relative for relative in delete_handles if relative != "." and relative not in allowed),
            key=lambda value: (value.count("/"), value.casefold()),
            reverse=True,
        )
        delete_order = [
            *payload_delete_order,
            *(relative for relative in allowed_values if relative in delete_handles),
            ".",
        ]
        if len(delete_order) != len(delete_handles) or set(delete_order) != set(delete_handles):
            raise ProofInputError("publication recovery deletion order is not an exact inventory")
        for relative in delete_order:
            handle = delete_handles.pop(relative)
            try:
                if not authority[relative][0]:
                    _clear_readonly_by_handle(kernel32, handle, context=f"publication recovery {relative}")
                _delete_by_handle(kernel32, handle, context=f"publication recovery {relative}")
            finally:
                kernel32.CloseHandle(handle)
        if os.path.lexists(safe_root):
            raise ProofInputError("publication recovery did not remove the exact staging root")
    finally:
        for handle in delete_handles.values():
            kernel32.CloseHandle(handle)
        for handle, _information in metadata_handles.values():
            kernel32.CloseHandle(handle)
        for handle, _information in acl_handles.values():
            kernel32.CloseHandle(handle)


def delete_exact_locked_file(
    path: Path,
    expected_identity: tuple[int, int],
    expected_sha256: str | None = None,
) -> bool:
    """Delete one exact journal file by its retained Windows handle; absence is idempotent."""
    from owner_proof_cleanup import delete_exact_locked_file as delete_impl

    return delete_impl(sys.modules[__name__], path, expected_identity, expected_sha256)


def delete_owned_tree_windows(root: Path, expected_root_identity: tuple[int, int]) -> None:
    """Delete only identities captured under an identity-locked transaction root."""
    from owner_proof_cleanup import delete_owned_tree_windows as delete_impl

    delete_impl(sys.modules[__name__], root, expected_root_identity)


def remove_owned_staging(
    path: Path,
    parent: Path,
    prefix: str,
    expected_identity: tuple[int, int],
    *,
    windows_delete: Callable[[Path, tuple[int, int]], None] = delete_owned_tree_windows,
) -> None:
    try:
        safe = absolute_lexical(path)
        if safe.parent != absolute_lexical(parent) or not safe.name.startswith(prefix):
            raise ProofInputError("owned staging cleanup target is outside its exact namespace")
        metadata = os.lstat(safe)
        if stat.S_ISLNK(metadata.st_mode) or metadata_reparse(metadata) or not stat.S_ISDIR(metadata.st_mode):
            raise ProofInputError("owned staging cleanup target changed type or became indirect")
        if owned_directory_identity(safe) != expected_identity:
            raise ProofInputError("owned staging cleanup target identity changed")
        if os.name == "nt":
            windows_delete(safe, expected_identity)
        elif getattr(shutil.rmtree, "avoids_symlink_attacks", False):
            shutil.rmtree(safe)
        else:
            raise ProofInputError("owned staging cleanup lacks a symlink-safe implementation")
        if os.path.lexists(safe):
            raise ProofInputError("owned staging cleanup did not remove the exact target")
    except FileNotFoundError:
        return
    except ProofInputError:
        raise
    except OSError as error:
        raise ProofInputError("owned staging cleanup failed") from error
