#!/usr/bin/env python3
"""Regression proof for the fail-closed Python policy environment."""

from __future__ import annotations

import re
from pathlib import Path

import importlib.metadata

from policy_python import (
    PINNED_DISTRIBUTIONS,
    PolicyEnvironmentError,
    canonical_distribution,
    parse_lock,
    validate_environment,
)


def test_lock_is_exact_and_environment_is_current() -> None:
    expected = {
        canonical_distribution(name): version
        for name, version in PINNED_DISTRIBUTIONS.items()
    }
    assert parse_lock() == expected
    identity = validate_environment()
    assert identity["distributions"] == expected


def test_missing_distribution_is_refused() -> None:
    def missing(name: str) -> str:
        if name == "jiwer":
            raise importlib.metadata.PackageNotFoundError(name)
        return parse_lock()[name]

    try:
        validate_environment(version_getter=missing)
    except PolicyEnvironmentError as error:
        assert "missing locked Python policy distribution jiwer==4.0.0" in str(error)
    else:
        raise AssertionError("a missing metric authority must fail closed")


def test_version_drift_is_refused() -> None:
    pins = parse_lock()

    def drifted(name: str) -> str:
        return "3.1.0" if name == "jiwer" else pins[name]

    try:
        validate_environment(version_getter=drifted)
    except PolicyEnvironmentError as error:
        assert "jiwer is 3.1.0, expected exactly 4.0.0" in str(error)
    else:
        raise AssertionError("metric-authority version drift must fail closed")


def test_unreviewed_python_minor_is_refused() -> None:
    pins = parse_lock()
    for minor in (11, 13):
        try:
            validate_environment(version_getter=pins.__getitem__, version_info=(3, minor))
        except PolicyEnvironmentError as error:
            assert "outside the locked policy interpreter set" in str(error)
        else:
            raise AssertionError(f"Python 3.{minor} must fail closed: only CI's 3.12 is the locked interpreter")


def test_the_locked_minor_is_exactly_what_ci_runs() -> None:
    """3.12 is the only admitted minor, and it is the one every ci.yml job installs."""
    pins = parse_lock()
    identity = validate_environment(version_getter=pins.__getitem__, version_info=(3, 12))
    assert identity["python"] == "3.12"
    workflow = (Path(__file__).resolve().parents[2] / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
    declared = set(re.findall(r"python-version:\s*'?\"?(3\.\d+)", workflow))
    assert declared == {"3.12"}, f"ci.yml installs {sorted(declared)}, the lock admits only 3.12"


def main() -> None:
    test_the_locked_minor_is_exactly_what_ci_runs()
    test_lock_is_exact_and_environment_is_current()
    test_missing_distribution_is_refused()
    test_version_drift_is_refused()
    test_unreviewed_python_minor_is_refused()
    print("Python policy environment regression passed")


if __name__ == "__main__":
    main()
