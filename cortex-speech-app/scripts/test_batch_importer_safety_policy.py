"""Source-level pins for the headless production importer's safety boundary.

The executable's Rust tests perform the adversarial runtime proof. These policy checks ensure the
proof and the guarded call sites remain wired into the shipped binary instead of surviving as dead
helpers while a future refactor quietly restores APPDATA-relative or partial-success behavior.
"""

from pathlib import Path


APP = Path(__file__).resolve().parents[1]
IMPORTER = APP / "src-tauri" / "src" / "bin" / "batch_importer.rs"


def source() -> str:
    return IMPORTER.read_text(encoding="utf-8")


def test_relocated_profiles_require_a_minted_identity_before_any_database_open() -> None:
    text = source()
    validation = text[text.index("fn isolated_import_data_dir(") : text.index("fn collect_prepared_wavs(")]
    main = text[text.index("fn main()") : text.index("#[cfg(test)]")]

    assert "cortex-batch-import-staging-profile" in text
    assert "CORTEX_IMPORT_STAGING_TOKEN" in text
    assert "STAGING_TOKEN_ENV" in validation
    assert "canonical_profile" in validation
    assert "sqlite_application_id_from_header" in validation
    assert "SQLite format 3\\0" in text
    assert 'std::env::var_os("APPDATA")' not in text
    assert main.index("isolated_import_data_dir(") < main.index("InstanceLock::try_lock")
    assert main.index("isolated_import_data_dir(") < main.index("Database::open_with_retry")
    assert "supplied_relocated_live_profile_is_refused_without_touching_database" in text
    assert "only_the_exact_minted_staging_identity_is_admitted" in text
    assert "profile_database_bytes(&relocated_live), before" in text


def test_staging_identity_can_only_be_minted_onto_a_new_profile() -> None:
    text = source()
    mint = text[text.index("fn mint_import_staging_profile(") : text.index("fn collect_prepared_wavs(")]

    assert "--init-staging-profile" in text
    assert mint.count(".create_new(true)") >= 2  # database and sentinel
    assert "refusing to attach an import-staging identity to an existing path" in mint
    assert "pragma_update(None, \"application_id\"" in mint
    assert "isolated_import_data_dir(" in mint  # self-validation before publication to the caller


def test_mixed_success_is_never_a_success_exit() -> None:
    text = source()
    verdict = text[text.index("fn require_complete_import(") : text.index("fn main()")]
    main = text[text.index("fn main()") : text.index("#[cfg(test)]")]

    assert "failed > 0 || succeeded != total" in verdict
    assert "remain durably committed for resume" in verdict
    assert "require_complete_import(total, succeeded, failed, &target_dir)?;" in main
    assert main.index("import_directory_with_agent_run_id") < main.index("require_complete_import(")
    assert "any_partial_or_inconsistent_import_tally_is_an_incomplete_exit" in text
    assert "(3, 2, 1)" in text and "(3, 2, 0)" in text and "(3, 3, 1)" in text


def main() -> None:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"BATCH IMPORTER SAFETY POLICY: {len(tests)} regressions passed")


if __name__ == "__main__":
    main()
