"""Fail-closed identity checks for the isolated Python policy environment."""

from __future__ import annotations

import hashlib
import importlib.metadata
import json
import os
import re
import sys
from pathlib import Path
from typing import Callable


APP_ROOT = Path(__file__).resolve().parents[1]
LOCK_PATH = Path(__file__).resolve().with_name("policy_requirements.lock")
ENV_ROOT = APP_ROOT / ".policy-python"
STAMP_PATH = ENV_ROOT / "cortex-policy-environment.json"
SUPPORTED_PYTHON = {(3, 11), (3, 12)}
PINNED_DISTRIBUTIONS = {
    "cffi": "2.0.0",
    "click": "8.4.1",
    "colorama": "0.4.6",
    "jiwer": "4.0.0",
    "numpy": "1.26.4",
    "pycparser": "3.0",
    "rapidfuzz": "3.14.5",
    "soundfile": "0.14.0",
    "typing-extensions": "4.15.0",
}
_PIN_RE = re.compile(r"^([A-Za-z0-9_.-]+)==([^\s#]+)$")


class PolicyEnvironmentError(RuntimeError):
    """The interpreter or its locked proof dependencies are not authoritative."""


def canonical_distribution(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def locked_python(env_root: Path = ENV_ROOT) -> Path:
    if os.name == "nt":
        return env_root / "Scripts" / "python.exe"
    return env_root / "bin" / "python"


def parse_lock(path: Path = LOCK_PATH) -> dict[str, str]:
    pins: dict[str, str] = {}
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise PolicyEnvironmentError(f"cannot read Python policy lock: {error}") from error
    for line_no, raw in enumerate(lines, start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        match = _PIN_RE.fullmatch(line)
        if match is None:
            raise PolicyEnvironmentError(
                f"Python policy lock line {line_no} is not an exact name==version pin"
            )
        name = canonical_distribution(match.group(1))
        if name in pins:
            raise PolicyEnvironmentError(f"duplicate Python policy lock entry: {name}")
        pins[name] = match.group(2)
    expected = {canonical_distribution(name): version for name, version in PINNED_DISTRIBUTIONS.items()}
    if pins != expected:
        raise PolicyEnvironmentError(
            "Python policy lock inventory drifted from the reviewed proof contract"
        )
    return pins


def validate_environment(
    *,
    version_getter: Callable[[str], str] = importlib.metadata.version,
    version_info: tuple[int, int] | None = None,
) -> dict[str, object]:
    observed_python = version_info or (sys.version_info.major, sys.version_info.minor)
    if observed_python not in SUPPORTED_PYTHON:
        supported = ", ".join(f"{major}.{minor}" for major, minor in sorted(SUPPORTED_PYTHON))
        raise PolicyEnvironmentError(
            f"Python {observed_python[0]}.{observed_python[1]} is outside the locked policy "
            f"interpreter set ({supported})"
        )
    pins = parse_lock()
    observed: dict[str, str] = {}
    for name, expected in sorted(pins.items()):
        try:
            actual = version_getter(name)
        except importlib.metadata.PackageNotFoundError as error:
            raise PolicyEnvironmentError(
                f"missing locked Python policy distribution {name}=={expected}; "
                "run `npm run setup:python-policies`"
            ) from error
        if actual != expected:
            raise PolicyEnvironmentError(
                f"Python policy distribution {name} is {actual}, expected exactly {expected}; "
                "run `npm run setup:python-policies`"
            )
        observed[name] = actual
    return {
        "schema": 1,
        "python": f"{observed_python[0]}.{observed_python[1]}",
        "lockSha256": sha256_file(LOCK_PATH),
        "distributions": observed,
    }


def validate_locked_launcher() -> dict[str, object]:
    python_path = locked_python()
    if not python_path.is_file() or not STAMP_PATH.is_file():
        raise PolicyEnvironmentError(
            "isolated Python policy environment is absent; run `npm run setup:python-policies`"
        )
    try:
        stamp = json.loads(STAMP_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PolicyEnvironmentError(
            "isolated Python policy environment stamp is unreadable; "
            "run `npm run setup:python-policies`"
        ) from error
    expected = {
        "lockSha256": sha256_file(LOCK_PATH),
        "pythonSha256": sha256_file(python_path),
    }
    for field, value in expected.items():
        if stamp.get(field) != value:
            raise PolicyEnvironmentError(
                f"isolated Python policy environment {field} is stale; "
                "run `npm run setup:python-policies`"
            )
    return stamp
