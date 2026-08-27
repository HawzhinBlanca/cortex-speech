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

from check_exe_freshness import (  # noqa: E402
    SOURCE_DIRS,
    SOURCE_FILES,
    SOURCE_PREFIXES,
    _source_changed_since,
    evaluate_freshness,
    extract_baked_sha,
    newest_source,
    worktree_source_changes,
    worktree_source_warnings,
)

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


def test_stale_installer_is_flagged_even_when_the_exe_is_fresh() -> None:
    """An MSI/NSIS from an older build is the artifact people actually double-click.

    Found 2026-08-17: both installers under target/release/bundle/ were four days behind the exe,
    and nothing said so — a "finished" installer that ships last week's app.
    """
    problems = evaluate_freshness(
        exe_exists=True,
        exe_mtime=2000.0,
        baked_sha=HEAD,
        head_sha=HEAD,
        newest_src_mtime=1000.0,
        newest_src_file="src/App.svelte",
        stale_installers=[("Cortex_2.1.0_x64_en-US.msi", 1500.0)],
    )
    assert len(problems) == 1, problems
    assert "STALE INSTALLER" in problems[0], problems

    # A rebuilt installer (nothing passed in) leaves the gate green.
    assert (
        evaluate_freshness(
            exe_exists=True,
            exe_mtime=2000.0,
            baked_sha=HEAD,
            head_sha=HEAD,
            newest_src_mtime=1000.0,
            newest_src_file="src/App.svelte",
            stale_installers=[],
        )
        == []
    )


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


def _otherwise_fresh_with_status(status_lines: list[str]) -> list[str]:
    return evaluate_freshness(
        exe_exists=True,
        exe_mtime=2000.0,
        baked_sha=HEAD,
        head_sha=HEAD,
        newest_src_mtime=1000.0,
        newest_src_file="src/App.svelte",
        dirty_source_paths=worktree_source_changes(status_lines, SOURCE_PREFIXES),
    )


def test_dirty_tracked_rust_source_fails_closed() -> None:
    problems = _otherwise_fresh_with_status([" M cortex-speech-app/src-tauri/src/pipeline.rs"])
    assert any("UNCOMMITTED COMPILED SOURCE" in problem for problem in problems), problems


def test_dirty_tracked_frontend_source_fails_closed() -> None:
    problems = _otherwise_fresh_with_status([" M cortex-speech-app/src/App.svelte"])
    assert any("src/App.svelte" in problem for problem in problems), problems


def test_dirty_build_input_fails_closed() -> None:
    problems = _otherwise_fresh_with_status([" M cortex-speech-app/src-tauri/Cargo.toml"])
    assert any("Cargo.toml" in problem for problem in problems), problems


def test_compiled_pilot_focus_contract_fails_closed() -> None:
    problems = _otherwise_fresh_with_status(["?? cortex-speech-app/controlled_pilot_focus.json"])
    assert any("controlled_pilot_focus.json" in problem for problem in problems), problems


def test_vendored_http_source_fails_closed() -> None:
    problems = _otherwise_fresh_with_status(
        [" M cortex-speech-app/src-tauri/vendor/tiny_http_fork/src/client.rs"]
    )
    assert any("tiny_http_fork/src/client.rs" in problem for problem in problems), problems


def test_vendored_http_manifest_fails_closed() -> None:
    problems = _otherwise_fresh_with_status(
        [" M cortex-speech-app/src-tauri/vendor/tiny_http_fork/Cargo.toml"]
    )
    assert any("tiny_http_fork/Cargo.toml" in problem for problem in problems), problems


def test_repo_toolchain_input_fails_closed() -> None:
    problems = _otherwise_fresh_with_status([" M rust-toolchain.toml"])
    assert any("rust-toolchain.toml" in problem for problem in problems), problems


def test_packaged_champion_client_fails_closed() -> None:
    problems = _otherwise_fresh_with_status([" M cortex-speech-app/scripts/cortex_7b_client.py"])
    assert any("cortex_7b_client.py" in problem for problem in problems), problems


def test_tauri_capability_directory_fails_closed() -> None:
    problems = _otherwise_fresh_with_status(
        ["?? cortex-speech-app/src-tauri/capabilities/reviewer.json"]
    )
    assert any("capabilities/reviewer.json" in problem for problem in problems), problems


def test_cross_commit_diff_covers_every_external_compiled_input(tmp_path: Path) -> None:
    """The HEAD-equivalence shortcut must not call these build changes "docs only"."""
    import subprocess

    compiled = {
        "rust-toolchain.toml": "channel = '1.95.0'\n",
        "cortex-speech-app/controlled_pilot_focus.json": "[]\n",
        "cortex-speech-app/src-tauri/vendor/tiny_http_fork/Cargo.toml": "[package]\nname='fixture'\n",
        "cortex-speech-app/src-tauri/vendor/tiny_http_fork/src/client.rs": "const DEADLINE: u64 = 10;\n",
    }
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    subprocess.run(["git", "config", "user.email", "freshness@example.invalid"], cwd=tmp_path, check=True)
    subprocess.run(["git", "config", "user.name", "Freshness Test"], cwd=tmp_path, check=True)
    for relative, content in compiled.items():
        path = tmp_path / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
    subprocess.run(["git", "add", "."], cwd=tmp_path, check=True)
    subprocess.run(["git", "commit", "-qm", "baseline"], cwd=tmp_path, check=True)
    baked = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=tmp_path, check=True, capture_output=True, text=True
    ).stdout.strip()

    for relative, content in compiled.items():
        (tmp_path / relative).write_text(content + "# changed\n", encoding="utf-8")
    subprocess.run(["git", "add", "."], cwd=tmp_path, check=True)
    subprocess.run(["git", "commit", "-qm", "change compiled inputs"], cwd=tmp_path, check=True)

    changed = _source_changed_since(tmp_path, baked, SOURCE_DIRS, SOURCE_FILES)
    assert changed is not None
    assert set(changed) == set(compiled), changed


def test_relevant_untracked_source_fails_closed() -> None:
    problems = _otherwise_fresh_with_status(["?? cortex-speech-app/src/lib/new_runtime.ts"])
    assert any("new_runtime.ts" in problem for problem in problems), problems


def test_docs_only_dirt_remains_nonblocking() -> None:
    problems = _otherwise_fresh_with_status(
        [" M docs/RELEASE_NOTES.md", "?? PROGRESS_LEDGER.md", " M cortex-speech-app/README.md"]
    )
    assert problems == [], problems


def test_unavailable_worktree_status_fails_closed() -> None:
    problems = evaluate_freshness(
        exe_exists=True,
        exe_mtime=2000.0,
        baked_sha=HEAD,
        head_sha=HEAD,
        newest_src_mtime=1000.0,
        newest_src_file="src/App.svelte",
        source_status_available=False,
    )
    assert any("could not inspect" in problem for problem in problems), problems


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


def test_worktree_warns_on_sibling_with_uncommitted_source() -> None:
    # The exact scenario this session hit: the main checkout built the exe, but a sibling worktree
    # carries the real fixes as uncommitted edits. Green-at-HEAD must not hide that.
    worktrees = [
        ("/repo/main", []),  # the checkout being gated — clean
        ("/repo/.claude/worktrees/wt1", [" M cortex-speech-app/src-tauri/src/pipeline.rs"]),
    ]
    warnings = worktree_source_warnings(worktrees, "/repo/main", SOURCE_PREFIXES)
    assert len(warnings) == 1, warnings
    assert "wt1" in warnings[0] and "1 uncommitted" in warnings[0], warnings


def test_worktree_skips_the_gated_checkout_itself() -> None:
    # Uncommitted source in the checkout being gated is the freshness check's own fail-closed job,
    # not a sibling warning — don't double-report it.
    worktrees = [("/repo/main", [" M cortex-speech-app/src/App.svelte"])]
    assert worktree_source_warnings(worktrees, "/repo/main", SOURCE_PREFIXES) == []


def test_worktree_ignores_non_source_dirty() -> None:
    # A sibling dirtied only in docs/ledger/tests is not an unshipped-source risk.
    worktrees = [
        ("/repo/main", []),
        ("/repo/wt2", [" M docs/DEEP_CHECK.md", "?? PROGRESS_LEDGER.md", " M cortex-speech-app/src-tauri/tests/x.rs"]),
    ]
    assert worktree_source_warnings(worktrees, "/repo/main", SOURCE_PREFIXES) == []


def test_worktree_no_warnings_when_all_clean() -> None:
    worktrees = [("/repo/main", []), ("/repo/wt3", [])]
    assert worktree_source_warnings(worktrees, "/repo/main", SOURCE_PREFIXES) == []


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
