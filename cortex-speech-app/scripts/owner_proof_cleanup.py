#!/usr/bin/env python3
"""Identity-locked cleanup implementation for owner-proof staging trees."""

from __future__ import annotations

import ctypes
import hashlib
import os
import stat
from pathlib import Path
from typing import Any


def delete_exact_locked_file(
    api: Any,
    path: Path,
    expected_identity: tuple[int, int],
    expected_sha256: str | None = None,
) -> bool:
    """Delete one exact journal file by its retained Windows handle; absence is idempotent."""
    if os.name != "nt":
        raise api.ProofInputError("exact journal deletion requires Windows")
    safe = api.absolute_lexical(path)
    if not os.path.lexists(safe):
        return False
    if expected_sha256 is not None and (
        len(expected_sha256) != 64
        or expected_sha256 != expected_sha256.lower()
        or any(character not in "0123456789abcdef" for character in expected_sha256)
    ):
        raise api.ProofInputError("exact journal digest is invalid")
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel32.CloseHandle.restype = ctypes.c_int
    desired = api._DELETE | api._FILE_READ_ATTRIBUTES | api._FILE_WRITE_ATTRIBUTES
    if expected_sha256 is not None:
        desired |= 0x80000000
    handle, information = api._open_exact_windows_handle(
        kernel32,
        safe,
        is_directory=False,
        desired_access=desired,
        share_mode=api._FILE_SHARE_READ,
        context="publication recovery journal",
    )
    try:
        if api._windows_information_identity(information) != expected_identity or information.links != 1:
            raise api.ProofInputError("publication recovery journal identity or link count changed")
        if expected_sha256 is not None:
            kernel32.ReadFile.argtypes = [
                ctypes.c_void_p,
                ctypes.c_void_p,
                ctypes.c_uint32,
                ctypes.POINTER(ctypes.c_uint32),
                ctypes.c_void_p,
            ]
            kernel32.ReadFile.restype = ctypes.c_int
            digest = hashlib.sha256()
            buffer = ctypes.create_string_buffer(1024 * 1024)
            while True:
                read = ctypes.c_uint32()
                if not kernel32.ReadFile(handle, buffer, len(buffer), ctypes.byref(read), None):
                    raise api.ProofInputError("publication recovery journal cannot be hashed")
                if read.value == 0:
                    break
                digest.update(buffer.raw[: read.value])
            if digest.hexdigest() != expected_sha256:
                raise api.ProofInputError("publication recovery journal digest changed")
        api._clear_readonly_by_handle(kernel32, handle, context="publication recovery journal")
        api._delete_by_handle(kernel32, handle, context="publication recovery journal")
    finally:
        kernel32.CloseHandle(handle)
    if os.path.lexists(safe):
        raise api.ProofInputError("publication recovery journal remained after exact deletion")
    return True


def delete_owned_tree_windows(api: Any, root: Path, expected_root_identity: tuple[int, int]) -> None:
    """Delete only identities captured under an identity-locked transaction root."""

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

    class Disposition(ctypes.Structure):
        _fields_ = [("delete", ctypes.c_int)]

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
    kernel32.SetFileInformationByHandle.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    kernel32.SetFileInformationByHandle.restype = ctypes.c_int
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel32.CloseHandle.restype = ctypes.c_int
    invalid = ctypes.c_void_p(-1).value

    def open_locked(
        path: Path,
        directory: bool,
        namespace_metadata: os.stat_result | None = None,
    ) -> tuple[int, tuple[int, int], Information, os.stat_result]:
        metadata = os.lstat(path) if namespace_metadata is None else namespace_metadata
        handle = kernel32.CreateFileW(
            os.fspath(path),
            0x00010000 | 0x80,
            0x1 | 0x2,
            None,
            3,
            0x00200000 | 0x02000000,
            None,
        )
        if handle in (None, invalid):
            raise api.ProofInputError("owned cleanup entry cannot be identity-locked")
        try:
            information = Information()
            if not kernel32.GetFileInformationByHandle(handle, ctypes.byref(information)):
                raise api.ProofInputError("owned cleanup entry identity cannot be inspected")
            is_directory = bool(information.attributes & 0x10)
            identity = (information.volume, (information.indexHigh << 32) | information.indexLow)
            if information.attributes & api.FILE_ATTRIBUTE_REPARSE_POINT or is_directory != directory:
                raise api.ProofInputError("owned cleanup encountered a reparse or type swap")
            if not api.stat_matches_handle_identity(metadata, int(handle), identity):
                raise api.ProofInputError("owned cleanup entry changed between namespace check and handle lock")
            path_metadata = os.stat(path, follow_symlinks=False)
            if not api.stat_matches_handle_identity(path_metadata, int(handle), identity):
                raise api.ProofInputError("owned cleanup pathname differs from its retained handle")
            return (int(handle), identity, information, metadata)
        except Exception:
            kernel32.CloseHandle(handle)
            raise

    def remove_locked(
        path: Path,
        directory: bool,
        identity_expected: tuple[int, int],
        handle: int,
        information: Information,
        metadata: os.stat_result,
    ) -> None:
        try:
            identity = (information.volume, (information.indexHigh << 32) | information.indexLow)
            if identity != identity_expected:
                raise api.ProofInputError("owned cleanup entry was replaced after ownership capture")
            if directory:
                children: list[tuple[Path, bool, tuple[int, int], int, Information, os.stat_result]] = []
                processed = 0
                try:
                    with os.scandir(path) as entries:
                        for entry in entries:
                            child = os.lstat(entry.path)
                            if stat.S_ISLNK(child.st_mode) or api.metadata_reparse(child):
                                raise api.ProofInputError("owned cleanup encountered a reparse entry")
                            child_is_directory = stat.S_ISDIR(child.st_mode)
                            if not child_is_directory and not stat.S_ISREG(child.st_mode):
                                raise api.ProofInputError("owned cleanup encountered a special entry")
                            child_path = Path(entry.path)
                            child_handle, child_identity, child_information, child_metadata = open_locked(
                                child_path,
                                child_is_directory,
                                child,
                            )
                            children.append(
                                (
                                    child_path,
                                    child_is_directory,
                                    child_identity,
                                    child_handle,
                                    child_information,
                                    child_metadata,
                                )
                            )
                    for child_entry in children:
                        processed += 1
                        remove_locked(*child_entry)
                finally:
                    for child_entry in children[processed:]:
                        kernel32.CloseHandle(child_entry[3])
            else:
                if not stat.S_ISREG(metadata.st_mode):
                    raise api.ProofInputError("owned cleanup encountered a non-regular file")
                if not metadata.st_mode & stat.S_IWRITE:
                    if information.links != 1:
                        raise api.ProofInputError("owned cleanup will not mutate a read-only external hardlink alias")
                    os.chmod(path, stat.S_IWRITE | stat.S_IREAD)
            disposition = Disposition(1)
            if not kernel32.SetFileInformationByHandle(handle, 4, ctypes.byref(disposition), ctypes.sizeof(disposition)):
                raise api.ProofInputError("owned cleanup entry cannot be deleted by identity")
        finally:
            kernel32.CloseHandle(handle)

    root_handle, root_identity, root_information, root_metadata = open_locked(root, True)
    remove_locked(root, True, expected_root_identity, root_handle, root_information, root_metadata)
