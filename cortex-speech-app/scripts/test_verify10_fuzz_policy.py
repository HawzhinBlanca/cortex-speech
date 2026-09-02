"""Fail-closed policy tests for the verify-10 fuzz leg."""

import importlib.util
import subprocess
import sys
from pathlib import Path
from unittest import mock


VERIFY = Path(__file__).resolve().parents[2] / "scripts" / "verify_10.py"


def load_verify():
    spec = importlib.util.spec_from_file_location("verify_10_fuzz_policy", VERIFY)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def completed(returncode=0, stdout="", stderr=""):
    return subprocess.CompletedProcess([], returncode, stdout, stderr)


class _WindowsCheckout:
    """A fixed Windows-drive checkout path, usable on any host.

    The assertions below prove the WINDOWS branch of _fuzz_cmd translates the checkout
    through WSL's /mnt/<drive> mount; on a POSIX host the module-level SRC_TAURI has no
    drive letter, so the branch under test needs a representative Windows path injected.
    """

    _raw = "C:\\cx\\cortex-speech-app\\src-tauri"

    def __str__(self):
        return self._raw

    def resolve(self):
        return self


def test_windows_fuzz_uses_one_ext4_cache_with_quoted_arguments():
    module = load_verify()
    with mock.patch.object(module.sys, "platform", "win32"), mock.patch.object(
        module, "SRC_TAURI", _WindowsCheckout()
    ):
        command = module._fuzz_cmd("run normalizer -- -max_total_time=30")

    assert command[:4] == ["wsl", "--exec", "bash", "-lc"]
    shell = command[4]
    assert "${XDG_CACHE_HOME:-$HOME/.cache}/cortex-speech/fuzz/" in shell
    assert 'export CARGO_TARGET_DIR="$cache_dir"' in shell
    assert "exec cargo +nightly fuzz run normalizer -- -max_total_time=30" in shell
    assert "/mnt/c/" in shell


def test_fuzz_smoke_builds_all_targets_once_then_runs_every_target():
    module = load_verify()
    requested = []

    def command(argstr):
        requested.append(("cargo", argstr))
        return [argstr]

    def run_command(target):
        requested.append(("binary", target))
        return [target]

    results = [
        completed(stdout="cache\ndiff\n"),
        completed(),
        completed(stderr="#123 DONE cov: 4 ft: 8 corp: 2/2b\n"),
        completed(stderr="#456 DONE cov: 7 ft: 9 corp: 3/3b\n"),
    ]
    with mock.patch.object(module, "_fuzz_cmd", side_effect=command), mock.patch.object(
        module, "_fuzz_run_cmd", side_effect=run_command
    ), mock.patch.object(module.subprocess, "run", side_effect=results):
        assert module._fn_fuzz_smoke()

    assert requested == [
        ("cargo", "list"),
        ("cargo", "build"),
        ("binary", "cache"),
        ("binary", "diff"),
    ]


def test_fuzz_smoke_refuses_success_without_execution_evidence():
    module = load_verify()
    # An exit-zero harness that reports zero iterations is still a vacuous non-run.
    results = [completed(stdout="normalizer\n"), completed(), completed(stderr="#0 DONE\n")]
    with mock.patch.object(module, "_fuzz_cmd", side_effect=lambda arg: [arg]), mock.patch.object(
        module, "_fuzz_run_cmd", side_effect=lambda target: [target]
    ), mock.patch.object(module.subprocess, "run", side_effect=results):
        assert not module._fn_fuzz_smoke()


def test_fuzz_smoke_fails_closed_when_upfront_build_fails():
    module = load_verify()
    requested = []

    def command(argstr):
        requested.append(argstr)
        return [argstr]

    with mock.patch.object(module, "_fuzz_cmd", side_effect=command), mock.patch.object(
        module.subprocess,
        "run",
        side_effect=[completed(stdout="normalizer\n"), completed(returncode=1, stderr="link failed")],
    ):
        assert not module._fn_fuzz_smoke()

    assert requested == ["list", "build"]


def test_windows_fuzz_runs_the_exact_built_binary_without_a_second_cargo_build():
    module = load_verify()
    with mock.patch.object(module.sys, "platform", "win32"):
        command = module._fuzz_run_cmd("normalizer")

    assert command[:4] == ["wsl", "--exec", "bash", "-lc"]
    shell = command[4]
    assert 'binary="$cache_dir/x86_64-unknown-linux-gnu/release/$target"' in shell
    assert 'exec "$binary"' in shell
    assert "detect_odr_violation=0" in shell
    assert "-artifact_prefix=" in shell
    assert 'artifacts="$cache_dir/runtime-artifacts/$target"' in shell
    assert 'corpus="$cache_dir/runtime-corpus/$target"' in shell
    assert 'source_corpus="$fuzz_dir/corpus/$target"' in shell
    assert "cargo run" not in shell

    with mock.patch.object(module.sys, "platform", "win32"):
        try:
            module._fuzz_run_cmd("../escape")
        except ValueError as error:
            assert "unsafe" in str(error)
        else:
            raise AssertionError("unsafe fuzz target name was accepted")


if __name__ == "__main__":
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"verify-10 fuzz policy regressions passed ({len(tests)} assertions)")
