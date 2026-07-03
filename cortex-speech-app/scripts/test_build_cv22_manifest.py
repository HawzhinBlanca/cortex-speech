#!/usr/bin/env python3
"""CI-safe unit tests for build_cv22_ckb_manifest.py (P2.1).

Exercises column detection, the existing-audio filter, WSL path rewriting, tar extraction, and the
SHA sidecar with a synthetic CV22 layout — no real dataset needed, runs on any platform.
"""

from __future__ import annotations

import io
import sys
import tarfile
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from build_cv22_ckb_manifest import build, find_columns, sanitize_reference, to_wsl_path  # noqa: E402


def test_to_wsl_path_rewrites_windows_drive() -> None:
    assert to_wsl_path("C:\\data\\x\\a.mp3") == "/mnt/c/data/x/a.mp3"
    assert to_wsl_path("D:/data/b.mp3") == "/mnt/d/data/b.mp3"
    assert to_wsl_path("/mnt/c/already") == "/mnt/c/already"


def test_sanitize_reference_collapses_tabs_and_newlines() -> None:
    assert sanitize_reference("a\tb\nc  d") == "a b c d"
    assert sanitize_reference("  spaced  ") == "spaced"


def test_find_columns_by_name_not_position() -> None:
    header = ["client_id", "path", "sentence_id", "sentence", "locale"]
    assert find_columns(header) == (1, 3)
    reordered = ["sentence", "path"]
    assert find_columns(reordered) == (1, 0)


def _write_cv22(root: Path, rows: list[tuple[str, str]], make_audio: set[str]) -> None:
    (root / "transcript" / "ckb").mkdir(parents=True)
    (root / "audio" / "ckb" / "test").mkdir(parents=True)
    with open(root / "transcript" / "ckb" / "test.tsv", "w", encoding="utf-8", newline="") as fh:
        fh.write("client_id\tpath\tsentence_id\tsentence\tlocale\n")
        for mp3, sentence in rows:
            fh.write(f"cid\t{mp3}\tsid\t{sentence}\tckb\n")
    for mp3 in make_audio:
        (root / "audio" / "ckb" / "test" / mp3).write_bytes(b"\x00fakeaudio")


def test_build_emits_only_rows_with_existing_audio() -> None:
    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        _write_cv22(
            root,
            rows=[("a.mp3", "سڵاو"), ("missing.mp3", "نیە"), ("c.mp3", "  ")],  # c has empty ref
            make_audio={"a.mp3", "c.mp3"},
        )
        out = root / "manifest.tsv"
        n, skipped = build(root, "test", out, limit=None, wsl=False, extract=False)
        assert n == 1, "only a.mp3 (present audio + non-empty ref) is emitted"
        assert skipped == 2, "missing.mp3 (no audio) and c.mp3 (empty ref) are skipped"
        lines = out.read_text(encoding="utf-8").strip().splitlines()
        assert len(lines) == 1
        path, ref = lines[0].split("\t")
        assert path.endswith("a.mp3")
        assert ref == "سڵاو"
        # SHA sidecar written.
        sha = out.with_suffix(".tsv.sha256").read_text(encoding="utf-8")
        assert "manifest.tsv" in sha and len(sha.split()[0]) == 64


def test_build_limit_caps_rows() -> None:
    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        _write_cv22(root, rows=[("a.mp3", "x"), ("b.mp3", "y")], make_audio={"a.mp3", "b.mp3"})
        n, _ = build(root, "test", root / "m.tsv", limit=1, wsl=False, extract=False)
        assert n == 1, "the --limit cap is honored"


def test_build_extract_unpacks_tar_then_manifests() -> None:
    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        _write_cv22(root, rows=[("packed.mp3", "پاکراو")], make_audio=set())  # audio only inside the tar
        # Build a tar shard containing packed.mp3.
        tar_path = root / "audio" / "ckb" / "test" / "ckb_test_0.tar"
        with tarfile.open(tar_path, "w") as tf:
            data = b"\x00tarred"
            info = tarfile.TarInfo(name="clips/packed.mp3")
            info.size = len(data)
            tf.addfile(info, io.BytesIO(data))
        # Without --extract: no mp3 present -> 0 rows.
        n0, _ = build(root, "test", root / "m0.tsv", limit=None, wsl=False, extract=False)
        assert n0 == 0, "without extraction the tar-packed clip is invisible"
        # With --extract: the clip is unpacked and manifested.
        n1, _ = build(root, "test", root / "m1.tsv", limit=None, wsl=False, extract=True)
        assert n1 == 1, "extraction unpacks the clip so it appears in the manifest"


def _run() -> int:
    failures = 0
    for name, fn in sorted(globals().items()):
        if not name.startswith("test_") or not callable(fn):
            continue
        try:
            fn()
        except AssertionError as exc:
            failures += 1
            print(f"FAIL {name}: {exc}", flush=True)
        else:
            print(f"ok   {name}", flush=True)
    if failures:
        print(f"\n{failures} cv22-manifest test(s) failed", flush=True)
        return 1
    print("\ncv22-manifest tests passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(_run())
