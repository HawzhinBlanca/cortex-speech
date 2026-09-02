#!/usr/bin/env python3
"""Fail-closed Windows icon quality regression using only the Python standard library."""

from __future__ import annotations

import json
import struct
import unittest
from pathlib import Path


APP = Path(__file__).resolve().parents[1]
TAURI_CONFIG = APP / "src-tauri" / "tauri.conf.json"
SOURCE = APP / "src-tauri" / "assets" / "couch-icon.png"
PNG = APP / "src-tauri" / "icons" / "icon.png"
ICO = APP / "src-tauri" / "icons" / "icon.ico"
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
REQUIRED_WINDOWS_SIZES = {16, 24, 32, 48, 64, 256}


def png_dimensions(path: Path) -> tuple[int, int]:
    payload = path.read_bytes()
    if len(payload) < 24 or payload[:8] != PNG_SIGNATURE or payload[12:16] != b"IHDR":
        raise AssertionError(f"{path} is not a canonical PNG with an IHDR header")
    return struct.unpack(">II", payload[16:24])


def ico_entries(path: Path) -> list[dict[str, int]]:
    payload = path.read_bytes()
    if len(payload) < 6:
        raise AssertionError("Windows ICO is truncated")
    reserved, image_type, count = struct.unpack_from("<HHH", payload, 0)
    if reserved != 0 or image_type != 1 or count <= 0:
        raise AssertionError("Windows ICO header is malformed")
    directory_end = 6 + 16 * count
    if directory_end > len(payload):
        raise AssertionError("Windows ICO directory is truncated")
    entries: list[dict[str, int]] = []
    for index in range(count):
        width, height, colors, reserved_byte, planes, bits, size, offset = struct.unpack_from(
            "<BBBBHHII", payload, 6 + 16 * index
        )
        width = 256 if width == 0 else width
        height = 256 if height == 0 else height
        if reserved_byte != 0 or width != height or colors != 0:
            raise AssertionError(f"Windows ICO directory entry {index} is malformed")
        # PNG-compressed ICO frames conventionally encode planes as either zero or one.
        if planes not in {0, 1} or bits < 32 or size <= 0 or offset < directory_end:
            raise AssertionError(f"Windows ICO directory entry {index} has weak metadata")
        if offset + size > len(payload):
            raise AssertionError(f"Windows ICO image {index} points outside the file")
        entries.append(
            {"width": width, "height": height, "bits": bits, "size": size, "offset": offset}
        )
    return entries


class WindowsIconPolicyTests(unittest.TestCase):
    def test_committed_source_and_runtime_png_are_real_high_resolution_assets(self) -> None:
        self.assertEqual(png_dimensions(SOURCE), (512, 512))
        self.assertEqual(png_dimensions(PNG), (512, 512))
        self.assertGreater(SOURCE.stat().st_size, 1_000)
        self.assertGreater(PNG.stat().st_size, 1_000)

    def test_ico_contains_every_required_windows_resolution(self) -> None:
        entries = ico_entries(ICO)
        sizes = {entry["width"] for entry in entries}
        self.assertEqual(sizes, REQUIRED_WINDOWS_SIZES)
        self.assertEqual(len(entries), len(REQUIRED_WINDOWS_SIZES))
        self.assertGreater(ICO.stat().st_size, 5_000)

    def test_tauri_and_nsis_reference_the_verified_assets(self) -> None:
        config = json.loads(TAURI_CONFIG.read_text(encoding="utf-8"))
        bundle = config["bundle"]
        self.assertEqual(bundle["icon"], ["icons/icon.png", "icons/icon.ico"])
        self.assertEqual(bundle["windows"]["nsis"]["installerIcon"], "icons/icon.ico")


if __name__ == "__main__":
    unittest.main(verbosity=2)
