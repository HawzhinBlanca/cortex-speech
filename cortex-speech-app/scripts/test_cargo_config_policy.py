#!/usr/bin/env python3
"""cargo config lives at the repository ROOT, where every cwd in use discovers it.

cargo discovers `.cargo/config.toml` from the CURRENT DIRECTORY upward, never from the manifest.
Measured 2026-09-02: `src-tauri/.cargo/config.toml` set `+crt-static` for MSVC (sherpa-onnx's
prebuilt libraries use the static CRT), the release workstation runs cargo from `src-tauri/` and got
it, CI runs cargo from `cortex-speech-app/` with `--manifest-path` and never did — the runner and the
workstation linked with different CRT flags for as long as that file existed. The flag now sits in the
root config next to the test-isolation `[env]` table, and nothing may quietly move a setting back
into a directory some cwd never walks through.
"""

from __future__ import annotations

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
ROOT_CONFIG = REPO_ROOT / ".cargo" / "config.toml"
NESTED_CONFIGS = [
    REPO_ROOT / "cortex-speech-app" / ".cargo" / "config.toml",
    REPO_ROOT / "cortex-speech-app" / "src-tauri" / ".cargo" / "config.toml",
]


def test_root_config_carries_the_msvc_crt_flag() -> None:
    if not ROOT_CONFIG.is_file():
        raise AssertionError(f"{ROOT_CONFIG} is missing — every cargo-launched process loses its config")
    text = ROOT_CONFIG.read_text(encoding="utf-8")
    assert "[target.x86_64-pc-windows-msvc]" in text, "the MSVC target section is gone from the root config"
    assert 'rustflags = ["-C", "target-feature=+crt-static"]' in text, (
        "+crt-static must be set at the root: sherpa-onnx's prebuilt libraries use the static CRT, and "
        "a flag in a nested .cargo/ is invisible to CI's cortex-speech-app/-rooted cargo"
    )


def test_no_nested_cargo_config_shadows_the_root() -> None:
    present = [str(path.relative_to(REPO_ROOT)) for path in NESTED_CONFIGS if path.exists()]
    assert not present, (
        "a nested .cargo/config.toml applies only to cwds beneath it, so the workstation and CI "
        "silently diverge; keep every setting in the root config:\n" + "\n".join(f"- {p}" for p in present)
    )


def main() -> None:
    test_root_config_carries_the_msvc_crt_flag()
    test_no_nested_cargo_config_shadows_the_root()
    print("cargo config policy passed")


if __name__ == "__main__":
    main()
