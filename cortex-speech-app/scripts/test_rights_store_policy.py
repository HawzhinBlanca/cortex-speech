"""Architecture policy for the recording-rights/provenance store slice."""

from pathlib import Path


ROOT = Path(__file__).resolve().parents[1] / "src-tauri" / "src"


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def command(source: str, signature: str) -> str:
    start = source.find(signature)
    if start < 0:
        raise AssertionError(f"missing command boundary `{signature}`")
    end = source.find("\n#[tauri::command]", start + len(signature))
    return source[start:] if end < 0 else source[start:end]


def test_store_owns_serialized_writes_and_bounded_reads_without_ui_dependencies() -> None:
    store = read("stores/rights.rs")
    for forbidden in ("tauri", "crate::commands", "crate::http"):
        if forbidden in store:
            raise AssertionError(f"RightsStore crossed a forbidden layer: {forbidden}")
    for required in (
        "struct RightsStore",
        "runtime: DatabaseRuntime",
        # Repointed 2026-08-30: writes now begin a restore-generation-gated mutation and take the
        # lock through it, replacing the bare `self.lock("op")` these pins used to name.
        "self.runtime.begin_mutation().map_err(AppError::Other)?",
        ".set_recording_rights(audio_path, rights)",
        ".revoke_recording(audio_path)",
        "self.runtime.open_read()?.list_recording_rights()",
    ):
        if required not in store:
            raise AssertionError(f"RightsStore lost required database boundary: {required}")


def test_commands_validate_and_map_without_raw_database_authority() -> None:
    source = read("commands/segments_write.rs").split("#[cfg(test)]", 1)[0]
    required = {
        "set_recording_rights": (
            'STRICT_RATE_LIMITER.check("set_recording_rights")',
            "validate::validate_file_path(&audio_path)",
            ".rights_store()",
            ".declare_recording(",
        ),
        "revoke_recording_consent": (
            'STRICT_RATE_LIMITER.check("revoke_recording_consent")',
            "validate::validate_file_path(&audio_path)",
            ".rights_store()",
            ".revoke_recording(",
        ),
        "list_recording_rights": (
            'RATE_LIMITER.check("list_recording_rights")',
            ".rights_store()",
            ".list_recordings()",
        ),
    }
    for name, needles in required.items():
        body = command(source, f"pub fn {name}(")
        for needle in needles:
            if needle not in body:
                raise AssertionError(f"{name} lost validation/delegation: {needle}")
        for forbidden in ("state.lock_db()", "state.db_arc()", "crate::db::Database", ".connection()"):
            if forbidden in body:
                raise AssertionError(f"{name} regained raw database authority: {forbidden}")


def main() -> None:
    test_store_owns_serialized_writes_and_bounded_reads_without_ui_dependencies()
    test_commands_validate_and_map_without_raw_database_authority()
    print("rights-store architecture policy passed")


if __name__ == "__main__":
    main()
