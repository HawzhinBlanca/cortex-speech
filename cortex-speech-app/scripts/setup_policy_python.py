#!/usr/bin/env python3
"""Create the isolated, exact Python environment used by proof policies."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import venv
from pathlib import Path

from policy_python import (
    APP_ROOT,
    ENV_ROOT,
    LOCK_PATH,
    STAMP_PATH,
    SUPPORTED_PYTHON,
    PolicyEnvironmentError,
    locked_python,
    sha256_file,
)


def _safe_environment_root() -> Path:
    app_root = APP_ROOT.resolve(strict=True)
    candidate = ENV_ROOT.resolve(strict=False)
    if candidate.parent != app_root or candidate.name != ".policy-python":
        raise PolicyEnvironmentError("refusing an unexpected Python policy environment path")
    if ENV_ROOT.is_symlink():
        raise PolicyEnvironmentError("refusing a symlinked Python policy environment")
    return candidate


def _require_locked_base_interpreter() -> None:
    """The venv inherits THIS interpreter's minor; refuse before building anything from the wrong one.

    `python scripts/setup_policy_python.py` runs under whatever `python` is on PATH — on this
    workstation that has been another agent's 3.11 venv. The gate later refuses the environment, but
    by then a 3.11 venv exists and every policy run on it is an untrusted result.
    """
    observed = (sys.version_info.major, sys.version_info.minor)
    if observed in SUPPORTED_PYTHON:
        return
    (major, minor) = sorted(SUPPORTED_PYTHON)[0]
    launcher = f"py -{major}.{minor}" if os.name == "nt" else f"python{major}.{minor}"
    raise PolicyEnvironmentError(
        f"refusing to build the policy environment from Python {observed[0]}.{observed[1]}; the locked "
        f"interpreter is {major}.{minor} (what CI runs). Re-run as: {launcher} {Path(__file__).name}"
    )


def _reset_environment() -> None:
    _require_locked_base_interpreter()
    target = _safe_environment_root()
    if target.exists():
        shutil.rmtree(target)
    venv.EnvBuilder(with_pip=True, clear=False, symlinks=False).create(target)


def _validate_inside_environment(python_path: Path) -> dict[str, object]:
    probe = (
        "import json,sys; "
        f"sys.path.insert(0, {str(Path(__file__).resolve().parent)!r}); "
        "from policy_python import validate_environment; "
        "print(json.dumps(validate_environment(), sort_keys=True))"
    )
    completed = subprocess.run(
        [str(python_path), "-c", probe],
        cwd=APP_ROOT,
        check=True,
        capture_output=True,
        text=True,
        shell=False,
    )
    return json.loads(completed.stdout)


def _atomic_write_stamp(document: dict[str, object]) -> None:
    STAMP_PATH.parent.mkdir(parents=True, exist_ok=True)
    temporary = STAMP_PATH.with_name(f".{STAMP_PATH.name}.{os.getpid()}.tmp")
    payload = json.dumps(document, indent=2, sort_keys=True) + "\n"
    with temporary.open("w", encoding="utf-8", newline="\n") as target:
        target.write(payload)
        target.flush()
        os.fsync(target.fileno())
    os.replace(temporary, STAMP_PATH)


def main() -> int:
    _reset_environment()
    python_path = locked_python()
    subprocess.run(
        [
            str(python_path),
            "-m",
            "pip",
            "install",
            "--disable-pip-version-check",
            "--no-deps",
            "--only-binary=:all:",
            "--requirement",
            str(LOCK_PATH),
        ],
        cwd=APP_ROOT,
        check=True,
        shell=False,
    )
    identity = _validate_inside_environment(python_path)
    identity.update(
        {
            "pythonSha256": sha256_file(python_path),
            "basePython": f"{sys.version_info.major}.{sys.version_info.minor}",
        }
    )
    _atomic_write_stamp(identity)
    print(
        "Python policy environment ready: "
        f"Python {identity['python']}, lock {str(identity['lockSha256'])[:12]}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
