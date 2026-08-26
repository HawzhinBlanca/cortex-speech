"""Architecture policy for import segment publication, rollback and metadata backfills."""

from pathlib import Path


ROOT = Path(__file__).resolve().parents[1] / "src-tauri" / "src"


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def method(source: str, signature: str) -> str:
    start = source.find(signature)
    if start < 0:
        raise AssertionError(f"missing pipeline method `{signature}`")
    end = source.find("\n    fn ", start + len(signature))
    return source[start:] if end < 0 else source[start:end]


def test_store_owns_import_publication_rollback_and_revision_guarded_alignment() -> None:
    store = read("stores/import_write.rs")
    for forbidden in ("tauri", "crate::commands", "crate::http"):
        if forbidden in store:
            raise AssertionError(f"ImportWriteStore crossed a forbidden layer: {forbidden}")
    for required in (
        "struct ImportWriteStore",
        "runtime: DatabaseRuntime",
        "begin_mutation().map_err(AppError::Other)?",
        'self.lock("publish_import_segments").insert_segments_batch(segments)',
        'self.lock("rollback_import_segments").delete_segments_batch(segment_ids)',
        'self.lock("update_import_alignment").update_segment_alignment_if_unchanged(',
        'self.lock("upsert_import_source_transcript").upsert_source_transcript(record)',
        'self.lock("upsert_import_source_provenance").upsert_source_audio_provenance(record)',
        'self.lock("set_import_audio_identity").set_audio_identity(audio_path, identity)',
        'self.lock("record_import_loop0_shadow").record_loop0_shadow(segment_id, memory_fired)',
        'self.lock("update_machine_speaker").update_speaker_id(segment_id, Some(speaker_id))',
        'self.lock("insert_import_hypothesis").insert_hypothesis(hypothesis)',
        'self.lock("commit_import_champion_transcript").commit_champion_transcript_if_unreviewed(',
    ):
        if required not in store:
            raise AssertionError(f"ImportWriteStore lost required authority: {required}")


def test_pipeline_delegates_import_segment_writes_without_raw_writer_calls() -> None:
    pipeline = read("pipeline.rs")
    for forbidden in (
        "db.insert_segments_batch(",
        "db.delete_segments_batch(",
        "db.update_segment_alignment(",
        "db.upsert_source_transcript(",
        "db.upsert_source_audio_provenance(",
        "db.set_audio_identity(",
        "db.record_loop0_shadow(",
        "db.update_speaker_id(",
        "db.insert_hypothesis(",
        "db.commit_champion_transcript_if_unreviewed(",
    ):
        if forbidden in pipeline:
            raise AssertionError(f"pipeline regained a raw migrated import writer: {forbidden}")

    persist = method(pipeline, "fn persist_segments(")
    if "import_writes.publish_segments(&segments)?" not in persist:
        raise AssertionError("import publication bypasses ImportWriteStore")

    champion = method(pipeline, "fn run_primary_wsl_pass_for_import(")
    if champion.count("import_writes.rollback_segments(&import_ids)") != 4:
        raise AssertionError("every champion cancel/halt rollback must delegate through ImportWriteStore")

    background = method(pipeline, "fn enqueue_background_alignments(")
    if "import_writes.update_alignment_if_unchanged(" not in background:
        raise AssertionError("background alignment lost revision-CAS store delegation")
    if "source_alignment.as_deref()" not in background:
        raise AssertionError("background alignment no longer compares the exact pre-inference metadata")

    champion = method(pipeline, "pub fn transcribe(")
    for required in (
        "let runtime = self.shared_database_runtime(&self.db_path)?;",
        "let db = runtime.open_read()?;",
        "let updated = import_writes",
        ".commit_champion_transcript_if_unreviewed(",
    ):
        if required not in champion:
            raise AssertionError(f"champion transcription lost serialized store authority: {required}")
    if "Database::open(&self.db_path)" in champion:
        raise AssertionError("champion transcription regained an independent raw database connection")
    if ".connection()" in champion:
        raise AssertionError("champion transcription regained a raw database connection escape")
    if "segment_queries.resolve_transcription_segment(&audio_path_str, alignment_json)?" not in champion:
        raise AssertionError("champion transcription bypasses the bounded segment query store")

    for required in (
        "database_runtime: Arc<Mutex<Option<crate::database_runtime::DatabaseRuntime>>>",
        "fn shared_database_runtime(",
        "if database_path != self.db_path",
        "*slot = Some(runtime.clone());",
        "let import_writes = self.import_write_store(db.path())?;",
    ):
        if required not in pipeline:
            raise AssertionError(f"pipeline lost the production shared-runtime boundary: {required}")
    if "database_runtime.clone()," not in read("lib.rs"):
        raise AssertionError("desktop startup no longer injects the exact AppState runtime")


def main() -> None:
    test_store_owns_import_publication_rollback_and_revision_guarded_alignment()
    test_pipeline_delegates_import_segment_writes_without_raw_writer_calls()
    print("import-write store architecture policy passed")


if __name__ == "__main__":
    main()
