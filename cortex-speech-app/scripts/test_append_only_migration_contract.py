#!/usr/bin/env python3
"""Lock each post-production migration to one exact, append-only Rust source block.

Migrations 1..65 retain their independent byte-identical production-prefix proof in
``test_historical_migration_prefix.py``. This contract starts at v66 and deliberately hashes each
migration separately, so appending v70 does not require blessing any rewrite of v66..69.
"""

from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any


APP_ROOT = Path(__file__).resolve().parents[1]
MIGRATIONS = APP_ROOT / "src-tauri" / "src" / "migrations" / "mod.rs"
CONTRACT = Path(__file__).with_name("append_only_migration_contract.v1.json")
CONTRACT_SOURCE = "src-tauri/src/migrations/mod.rs"
CATALOG_START = "pub static MIGRATIONS: &[Migration] = &["
CATALOG_END = re.compile(r"(?m)^\];$")
MIGRATION_HEADER = re.compile(
    r'^    Migration \{\n'
    r"        version: (?P<version>[0-9]+),\n"
    r'        description: "(?P<description>(?:\\.|[^"\\])*)",',
    re.MULTILINE,
)
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
LOCKED_VERSIONS = [66, 67, 68, 69]
TOP_LEVEL_KEYS = {"schema", "source", "normalization", "algorithm", "migrations"}
MIGRATION_KEYS = {"version", "description", "sourceBlockSha256"}


@dataclass(frozen=True)
class SourceMigration:
    version: int
    description: str
    source_block: str

    @property
    def sha256(self) -> str:
        return hashlib.sha256(self.source_block.encode("utf-8")).hexdigest()


def _normalize_source(raw: bytes) -> str:
    try:
        source = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise AssertionError("migration catalog is not valid UTF-8") from error
    source = source.replace("\r\n", "\n")
    assert "\r" not in source, "migration catalog contains unsupported bare carriage returns"
    return source


def extract_source_migrations(source: str) -> list[SourceMigration]:
    """Extract exact LF-normalized top-level migration blocks from the Rust catalog."""

    try:
        catalog_start = source.index(CATALOG_START)
    except ValueError as error:
        raise AssertionError("canonical Rust migration catalog is missing") from error
    end_match = CATALOG_END.search(source, catalog_start)
    assert end_match is not None, "canonical Rust migration catalog terminator is missing"
    catalog = source[catalog_start : end_match.start()]
    headers = list(MIGRATION_HEADER.finditer(catalog))
    assert headers, "migration catalog contains no canonical Migration blocks"

    migrations: list[SourceMigration] = []
    for index, header in enumerate(headers):
        block_end = headers[index + 1].start() if index + 1 < len(headers) else len(catalog)
        # The separator before a future migration is not part of either migration's authority.
        # Canonicalize only trailing newlines; every byte from `Migration {` through `},` is hashed.
        source_block = catalog[header.start() : block_end].rstrip("\n") + "\n"
        assert source_block.endswith("    },\n"), (
            f"migration {header.group('version')} does not end at a canonical top-level block"
        )
        migrations.append(
            SourceMigration(
                version=int(header.group("version")),
                description=header.group("description"),
                source_block=source_block,
            )
        )

    versions = [migration.version for migration in migrations]
    assert versions == list(range(1, versions[-1] + 1)), (
        f"migration catalog is not one ordered contiguous append-only sequence: {versions}"
    )
    return migrations


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise AssertionError(f"migration contract contains duplicate key {key!r}")
        result[key] = value
    return result


def load_contract(path: Path = CONTRACT) -> dict[str, Any]:
    try:
        contract = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=_reject_duplicate_keys,
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AssertionError(f"cannot read immutable migration contract: {error}") from error
    assert isinstance(contract, dict), "migration contract must be a JSON object"
    assert set(contract) == TOP_LEVEL_KEYS, "migration contract top-level fields changed"
    assert type(contract["schema"]) is int and contract["schema"] == 1
    assert contract["source"] == CONTRACT_SOURCE
    assert contract["normalization"] == "utf8-lf"
    assert contract["algorithm"] == "sha256"
    assert isinstance(contract["migrations"], list)

    entries = contract["migrations"]
    assert len(entries) == len(LOCKED_VERSIONS)
    for entry in entries:
        assert isinstance(entry, dict)
        assert set(entry) == MIGRATION_KEYS, "migration contract entry fields changed"
        assert type(entry["version"]) is int
        assert isinstance(entry["description"], str) and entry["description"]
        assert isinstance(entry["sourceBlockSha256"], str)
        assert SHA256.fullmatch(entry["sourceBlockSha256"])
    assert [entry["version"] for entry in entries] == LOCKED_VERSIONS
    return contract


def validate_contract(source: str, contract: dict[str, Any]) -> list[SourceMigration]:
    migrations = extract_source_migrations(source)
    by_version = {migration.version: migration for migration in migrations}
    assert len(by_version) == len(migrations), "migration catalog contains duplicate versions"

    for expected in contract["migrations"]:
        version = expected["version"]
        actual = by_version.get(version)
        assert actual is not None, f"locked migration {version} is missing"
        assert actual.description == expected["description"], (
            f"migration {version} description changed: expected {expected['description']!r}, "
            f"got {actual.description!r}"
        )
        assert actual.sha256 == expected["sourceBlockSha256"], (
            f"migration {version} source block changed: "
            f"expected {expected['sourceBlockSha256']}, got {actual.sha256}"
        )
    return migrations


def _current_source() -> str:
    return _normalize_source(MIGRATIONS.read_bytes())


def _assert_rejected(source: str, contract: dict[str, Any]) -> None:
    try:
        validate_contract(source, contract)
    except AssertionError:
        return
    raise AssertionError("mutated migration source unexpectedly satisfied the immutable contract")


def test_append_only_migrations_66_through_69_match_individual_source_hashes() -> None:
    contract = load_contract()
    assert (APP_ROOT / contract["source"]).resolve() == MIGRATIONS.resolve()
    validate_contract(_current_source(), contract)


def test_contract_rejects_body_description_and_order_mutation() -> None:
    source = _current_source()
    contract = load_contract()

    body_mutation = source.replace(
        "CREATE TABLE review_drafts (",
        "CREATE TABLE review_drafts_mutated (",
        1,
    )
    assert body_mutation != source
    _assert_rejected(body_mutation, contract)

    journal_body_mutation = source.replace(
        "CREATE TABLE desktop_review_legacy_actions_v1 (",
        "CREATE TABLE desktop_review_legacy_actions_v1_mutated (",
        1,
    )
    assert journal_body_mutation != source
    _assert_rejected(journal_body_mutation, contract)

    description_mutation = source.replace(
        "Add non-authoritative crash-safe desktop review drafts",
        "Add mutable desktop review drafts",
        1,
    )
    assert description_mutation != source
    _assert_rejected(description_mutation, contract)

    migrations = extract_source_migrations(source)
    migration_66 = next(item.source_block for item in migrations if item.version == 66)
    migration_67 = next(item.source_block for item in migrations if item.version == 67)
    ordered_pair = migration_66 + migration_67
    assert source.count(ordered_pair) == 1
    order_mutation = source.replace(ordered_pair, migration_67 + migration_66, 1)
    _assert_rejected(order_mutation, contract)


def test_future_append_does_not_redefine_locked_migration_blocks() -> None:
    source = _current_source()
    contract = load_contract()
    catalog_end = CATALOG_END.search(source, source.index(CATALOG_START))
    assert catalog_end is not None
    future = (
        "    Migration {\n"
        "        version: 70,\n"
        '        description: "Append-only parser sentinel",\n'
        '        up_sql: "SELECT 1;",\n'
        '        down_sql: Some("SELECT 1;"),\n'
        "    },\n"
    )
    appended = source[: catalog_end.start()] + future + source[catalog_end.start() :]
    migrations = validate_contract(appended, contract)
    assert migrations[-1].version == 70


def main() -> None:
    test_append_only_migrations_66_through_69_match_individual_source_hashes()
    test_contract_rejects_body_description_and_order_mutation()
    test_future_append_does_not_redefine_locked_migration_blocks()
    hashes = {
        migration.version: migration.sha256
        for migration in extract_source_migrations(_current_source())
        if migration.version in LOCKED_VERSIONS
    }
    print(f"append-only migration source blocks 66-69 are immutable: {hashes}")


if __name__ == "__main__":
    main()
