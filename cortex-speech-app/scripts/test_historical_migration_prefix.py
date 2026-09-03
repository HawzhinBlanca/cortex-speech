#!/usr/bin/env python3
"""Prove that the append-only schema-1..65 production migration catalog never changed.

The integration line may append migrations, but rewriting an already deployed migration changes
what a fresh install means without changing its recorded version.  Hash the exact normalized source
prefix inherited from production commit bd581ef; newline normalization makes the proof independent
of Git's Windows checkout policy while every other byte remains authoritative.
"""

from __future__ import annotations

import hashlib
import re
from pathlib import Path


MIGRATIONS = Path(__file__).resolve().parents[1] / "src-tauri" / "src" / "migrations" / "mod.rs"
PREFIX_START = "pub static MIGRATIONS: &[Migration] = &["
FIRST_APPEND_ONLY_MIGRATION = "    Migration {\n        version: 66,"
PRODUCTION_1_TO_65_SHA256 = "60b41b8452a490d40a62232f868ca0e2c2ff82ab095874359fa393a176f8ba7f"


def historical_prefix(source: str) -> str:
    try:
        start = source.index(PREFIX_START)
        end = source.index(FIRST_APPEND_ONLY_MIGRATION, start)
    except ValueError as error:
        raise AssertionError("migration catalog or append-only v66 boundary is missing") from error
    return source[start:end]


def test_production_migrations_1_through_65_are_byte_identical() -> None:
    # read_text performs universal-newline normalization; encode back to UTF-8 for a platform-stable
    # digest of the exact Rust source contract.
    prefix = historical_prefix(MIGRATIONS.read_text(encoding="utf-8"))
    versions = [int(value) for value in re.findall(r"Migration\s*\{\s*version:\s*(\d+)\s*,", prefix)]
    assert versions == list(range(1, 66)), f"historical migration sequence changed: {versions}"
    actual = hashlib.sha256(prefix.encode("utf-8")).hexdigest()
    assert actual == PRODUCTION_1_TO_65_SHA256, (
        "migrations 1-65 differ from the byte-identical bd581ef production catalog: "
        f"expected {PRODUCTION_1_TO_65_SHA256}, got {actual}"
    )


def main() -> None:
    test_production_migrations_1_through_65_are_byte_identical()
    print("historical migration prefix 1-65 is byte-identical to bd581ef")


if __name__ == "__main__":
    main()
