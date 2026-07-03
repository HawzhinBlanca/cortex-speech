#!/usr/bin/env python3
"""CI-safe unit tests for the P0.2 stale-exe guard logic (check_exe_freshness.py).

These exercise the pure decision core and the binary-marker extractor with synthetic fixtures, so
they run green on any platform (including CI Linux with no Windows exe). The real gate that inspects
the actual built exe is `check_exe_freshness.py` main(), invoked only by `make ship-check-local`.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_exe_freshness import evaluate_freshness, extract_baked_sha, newest_source  # noqa: E402

HEAD = "a" * 40
OTHER = "b" * 40


def test_extract_baked_sha_finds_contiguous_marker() -> None:
    blob = b"\x00\x01padding" + b"CORTEX_BUILD_SHA:" + HEAD.encode() + b"\x00trailing"
    assert extract_baked_sha(blob) == HEAD


def test_extract_baked_sha_handles_unknown() -> None:
    assert extract_baked_sha(b"junk CORTEX_BUILD_SHA:unknown junk") == "unknown"


def test_extract_baked_sha_absent_marker_returns_none() -> None:
    assert extract_baked_sha(b"no marker here at all") is None


def test_fresh_and_head_matched_passes() -> None:
    problems = evaluate_freshness(
        exe_exists=True,
        exe_mtime=2000.0,
        baked_sha=HEAD,
        head_sha=HEAD,
        newest_src_mtime=1000.0,
        newest_src_file="src/App.svelte",
    )
    assert problems == [], problems


def test_stale_exe_is_flagged() -> None:
    problems = evaluate_freshness(
        exe_exists=True,
        exe_mtime=1000.0,
        baked_sha=HEAD,
        head_sha=HEAD,
        newest_src_mtime=2000.0,  # source newer than exe
        newest_src_file="src-tauri/src/pipeline.rs",
    )
    assert any("STALE EXE" in p for p in problems), problems


def test_wrong_sha_is_flagged() -> None:
    problems = evaluate_freshness(
        exe_exists=True,
        exe_mtime=2000.0,
        baked_sha=OTHER,  # built from a different commit
        head_sha=HEAD,
        newest_src_mtime=1000.0,
        newest_src_file="src/App.svelte",
    )
    assert any("NOT HEAD" in p for p in problems), problems


def test_missing_exe_is_flagged() -> None:
    problems = evaluate_freshness(
        exe_exists=False,
        exe_mtime=0.0,
        baked_sha=None,
        head_sha=HEAD,
        newest_src_mtime=1000.0,
        newest_src_file="src/App.svelte",
    )
    assert any("not found" in p for p in problems), problems


def test_unknown_baked_sha_is_flagged() -> None:
    problems = evaluate_freshness(
        exe_exists=True,
        exe_mtime=2000.0,
        baked_sha="unknown",
        head_sha=HEAD,
        newest_src_mtime=1000.0,
        newest_src_file="src/App.svelte",
    )
    assert any("unknown" in p for p in problems), problems


def test_absent_marker_in_old_exe_is_flagged() -> None:
    problems = evaluate_freshness(
        exe_exists=True,
        exe_mtime=2000.0,
        baked_sha=None,  # exe predates the marker
        head_sha=HEAD,
        newest_src_mtime=1000.0,
        newest_src_file="src/App.svelte",
    )
    assert any("marker" in p for p in problems), problems


def test_short_sha_prefix_match_passes() -> None:
    # git may hand back a full SHA while a build baked a shortened one; prefix match both ways.
    problems = evaluate_freshness(
        exe_exists=True,
        exe_mtime=2000.0,
        baked_sha=HEAD[:12],
        head_sha=HEAD,
        newest_src_mtime=1000.0,
        newest_src_file="src/App.svelte",
    )
    assert problems == [], problems


def test_newest_source_picks_latest_file(tmp_path: Path) -> None:
    app = tmp_path
    (app / "src").mkdir()
    (app / "src-tauri" / "src").mkdir(parents=True)
    old = app / "src" / "old.ts"
    new = app / "src-tauri" / "src" / "new.rs"
    old.write_text("x")
    new.write_text("y")
    import os

    os.utime(old, (1000, 1000))
    os.utime(new, (5000, 5000))
    mtime, newest = newest_source(app, ["src", "src-tauri/src"], [])
    assert mtime == 5000.0
    assert newest == new


def _run() -> int:
    failures = 0
    import tempfile

    for name, fn in sorted(globals().items()):
        if not name.startswith("test_") or not callable(fn):
            continue
        try:
            if "tmp_path" in fn.__code__.co_varnames[: fn.__code__.co_argcount]:
                with tempfile.TemporaryDirectory() as d:
                    fn(Path(d))
            else:
                fn()
        except AssertionError as exc:
            failures += 1
            print(f"FAIL {name}: {exc}", flush=True)
        else:
            print(f"ok   {name}", flush=True)
    if failures:
        print(f"\n{failures} freshness-logic test(s) failed", flush=True)
        return 1
    print("\nexe-freshness logic tests passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(_run())
