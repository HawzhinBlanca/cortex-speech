"""Architecture policy for the durable job/interrupted-import store slice."""

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


def test_store_owns_bounded_reads_and_serialized_discard_without_ui_dependencies() -> None:
    store = read("stores/jobs.rs")
    for forbidden in ("tauri", "crate::commands", "crate::http"):
        if forbidden in store:
            raise AssertionError(f"JobStore crossed a forbidden layer: {forbidden}")
    for required in (
        "struct JobStore",
        "runtime: DatabaseRuntime",
        "begin_mutation().map_err(AppError::Other)?",
        "self.runtime.open_read()?.find_interrupted_import_job()",
        'self.lock("discard_interrupted_import").discard_import_job(job_id)',
        "self.runtime.open_read()?.list_recent_jobs(limit)",
        'self.run_tracked(job_id, "export_dataset", "EXPORT_FAILED"',
        'self.run_tracked(job_id, "export_huggingface_dataset", "HF_EXPORT_FAILED"',
        "crate::export::export_dataset(database, path, format)",
        "crate::export::export_huggingface_dataset(database, path, settings)",
    ):
        if required not in store:
            raise AssertionError(f"JobStore lost required database boundary: {required}")


def test_commands_delegate_without_raw_database_authority() -> None:
    source = read("commands.rs")
    signatures = {
        "get_interrupted_import": "pub fn get_interrupted_import(",
        "discard_interrupted_import": "pub fn discard_interrupted_import(",
        "resume_interrupted_import": "pub fn resume_interrupted_import(",
        "get_jobs": "pub async fn get_jobs(",
    }
    for name, signature in signatures.items():
        body = command(source, signature)
        if ".job_store()" not in body:
            raise AssertionError(f"{name} bypasses JobStore")
        for forbidden in ("state.lock_db()", "state.db_arc()", "crate::db::Database", ".connection()"):
            if forbidden in body:
                raise AssertionError(f"{name} regained raw database authority: {forbidden}")

    discard = command(source, signatures["discard_interrupted_import"])
    for required in ('STRICT_RATE_LIMITER.check("discard_interrupted_import")', "validate::validate_identifier(&job_id)"):
        if required not in discard:
            raise AssertionError(f"discard_interrupted_import lost validation: {required}")

    exports = read("commands/export.rs")
    export_signatures = {
        "export_dataset": "pub async fn export_dataset(",
        "export_huggingface_dataset": "pub async fn export_huggingface_dataset(",
    }
    for name, signature in export_signatures.items():
        body = command(exports, signature)
        if ".job_store()" not in body or f"store.{name}" not in body:
            raise AssertionError(f"{name} bypasses the tracked JobStore boundary")
        for forbidden in ("state.lock_db()", "state.db_arc()", ".run_tracked(", "crate::db::Database"):
            if forbidden in body:
                raise AssertionError(f"{name} regained raw database or job-lifecycle authority: {forbidden}")
        if 'STRICT_RATE_LIMITER.check("' + name + '")' not in body or "validate::validate_output_path" not in body:
            raise AssertionError(f"{name} lost rate or output-path validation")


def main() -> None:
    test_store_owns_bounded_reads_and_serialized_discard_without_ui_dependencies()
    test_commands_delegate_without_raw_database_authority()
    print("job-store architecture policy passed")


if __name__ == "__main__":
    main()
