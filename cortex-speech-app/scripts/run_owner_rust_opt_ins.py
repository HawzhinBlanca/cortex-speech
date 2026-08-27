#!/usr/bin/env python3
"""Run every owner-scoped Rust opt-in test without permitting a vacuous skip."""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path


APP_ROOT = Path(__file__).resolve().parents[1]
REPO_ROOT = APP_ROOT.parent
MANIFEST = APP_ROOT / "src-tauri" / "Cargo.toml"
MEDIA_ENV = "CORTEX_OWNER_REAL_MEDIA_DIR"
AUDIOBOOK_ENV = "CORTEX_OWNER_AUDIOBOOK_MP3"
SCALE_DB_ENV = "CORTEX_OWNER_SCALE_DB"

REAL_MEDIA_TESTS = (
    "champion_selected_refuses_to_downgrade_to_the_finetuned_model",
    "test_decode_any_supported_audio",
    "test_decode_flac_small",
    "test_decode_large_flac",
    "test_decode_mov_batch",
    "test_decode_mov_small",
    "test_decode_mp4_batch_small",
    "test_decode_mp4_small",
    "test_pipeline_import_supported_audio_directory",
    "test_pipeline_process_single_supported_audio",
    "test_vad_on_real_kurdish_audio",
)
AUDIOBOOK_TESTS = (
    "audiobook_mp3_decode_and_chunk_plan",
    "audiobook_mp3_fingerprint_reimport_not_duplicate",
    "streaming_decoder_matches_the_direct_decoder_on_real_long_audio",
)
SCALE_TEST = "export_tests::hf_export_at_real_corpus_scale"
_PASS_SUMMARY = re.compile(r"test result: ok\. 1 passed; 0 failed; 0 ignored;")
_SKIP_OUTPUT = re.compile(r"\bskip(?:ped|ping)?\b", re.IGNORECASE)


class OwnerOptInError(RuntimeError):
    pass


def _required_path(variable: str, *, directory: bool) -> Path:
    raw = os.environ.get(variable, "").strip()
    if not raw:
        raise OwnerOptInError(f"{variable} is not configured")
    path = Path(raw).expanduser().resolve(strict=True)
    if directory and not path.is_dir():
        raise OwnerOptInError(f"{variable} is not a directory")
    if not directory and not path.is_file():
        raise OwnerOptInError(f"{variable} is not a file")
    return path


def _validate_media_inputs() -> tuple[Path, Path]:
    media = _required_path(MEDIA_ENV, directory=True)
    files = [path for path in media.rglob("*") if path.is_file()]
    if not (3 <= len(files) <= 25):
        raise OwnerOptInError(
            f"{MEDIA_ENV} must be a curated 3-25-file proof directory, observed {len(files)} files"
        )
    extensions = {path.suffix.casefold() for path in files}
    missing = {".flac", ".mov", ".mp4"} - extensions
    if missing:
        raise OwnerOptInError(
            f"{MEDIA_ENV} lacks mandatory real decoder fixtures: {', '.join(sorted(missing))}"
        )
    if not (media / "A1-0001_PODCAST-001.mp4").is_file():
        raise OwnerOptInError(
            f"{MEDIA_ENV} must contain A1-0001_PODCAST-001.mp4 for the real Kurdish VAD test"
        )
    audiobook = _required_path(AUDIOBOOK_ENV, directory=False)
    if audiobook.suffix.casefold() != ".mp3":
        raise OwnerOptInError(f"{AUDIOBOOK_ENV} must name a real long-form MP3")
    return media, audiobook


def _run_exact(target_args: list[str], selector: str, environment: dict[str, str]) -> None:
    command = [
        "cargo",
        "test",
        "--manifest-path",
        str(MANIFEST),
        *target_args,
        selector,
        "--",
        "--ignored",
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ]
    completed = subprocess.run(
        command,
        cwd=REPO_ROOT,
        env=environment,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
        shell=False,
    )
    output = (completed.stdout or "") + (completed.stderr or "")
    print(output, end="" if output.endswith("\n") else "\n", flush=True)
    if completed.returncode != 0:
        raise OwnerOptInError(f"Rust opt-in {selector} exited {completed.returncode}")
    if not _PASS_SUMMARY.search(output):
        raise OwnerOptInError(f"Rust opt-in {selector} did not prove exactly one passing test")
    if _SKIP_OUTPUT.search(output):
        raise OwnerOptInError(f"Rust opt-in {selector} emitted skip output")


def run_media() -> None:
    media, audiobook = _validate_media_inputs()
    environment = os.environ.copy()
    environment["CORTEX_REAL_AUDIO_DIR"] = str(media)
    environment["CORTEX_AUDIOBOOK_MP3"] = str(audiobook)
    for selector in REAL_MEDIA_TESTS:
        _run_exact(["--test", "real_audio"], selector, environment)
    for selector in AUDIOBOOK_TESTS:
        _run_exact(["--test", "audiobook_smoke"], selector, environment)
    print(f"OWNER RUST MEDIA OPT-INS: {len(REAL_MEDIA_TESTS) + len(AUDIOBOOK_TESTS)} passed")


def run_scale() -> None:
    database = _required_path(SCALE_DB_ENV, directory=False)
    environment = os.environ.copy()
    environment["CORTEX_SCALE_DB"] = str(database)
    _run_exact(["--lib"], SCALE_TEST, environment)
    print("OWNER RUST SCALE OPT-IN: 1 passed")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("suite", choices=("media", "scale"))
    args = parser.parse_args()
    try:
        run_media() if args.suite == "media" else run_scale()
    except (OSError, OwnerOptInError, subprocess.SubprocessError) as error:
        print(f"OWNER RUST OPT-INS FAILED: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
