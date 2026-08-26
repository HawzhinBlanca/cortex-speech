"""Architecture policy for fail-closed import publication and metadata backfills."""

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
        'self.lock("publish_champion_import_segments")',
        ".insert_champion_segments_batch(",
        "deployment_sha256,",
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
    if "rollback_segments" in champion or "publish_segments" in champion:
        raise AssertionError("champion drafting must not create or compensate canonical segment rows")
    for required in (
        "segment(s) remain unpublished",
        "no segments were published",
        "ChampionAttempt::Drafted(draft)",
    ):
        if required not in champion:
            raise AssertionError(f"champion pre-publication hard stop lost invariant: {required}")
    publish_call = "import_writes.publish_champion_segments(&prepared, deployment_sha256, Some(&identity))?"
    if pipeline.count(publish_call) != 2:
        raise AssertionError("both import paths must delegate atomic champion publication through ImportWriteStore")
    if pipeline.count("self.run_primary_wsl_pass_for_import(&mut prepared, cancel)?") != 2:
        raise AssertionError("both import paths must complete champion drafting before publication")

    background = method(pipeline, "fn enqueue_background_alignments(")
    if "import_writes.update_alignment_if_unchanged(" not in background:
        raise AssertionError("background alignment lost revision-CAS store delegation")
    if "source_alignment.as_deref()" not in background:
        raise AssertionError("background alignment no longer compares the exact pre-inference metadata")

    champion = method(pipeline, "fn transcribe_with_champion_commit(")
    for required in (
        "let runtime = self.shared_database_runtime(&self.db_path)?;",
        "let db = runtime.open_read()?;",
        "if commit_champion",
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

    database = read("db.rs")
    atomic_publish = method(database, "pub(crate) fn insert_champion_segments_batch(")
    for required in (
        'self.conn.execute("SAVEPOINT champion_import_publish", [])?',
        "self.insert_segments_batch(segments)?;",
        'DELETE FROM segment_hypotheses WHERE segment_id = ?1',
        "self.set_audio_identity(audio_path, identity)?;",
        'self.release_savepoint("champion_import_publish")?',
        'self.cleanup_savepoint_after_error("champion_import_publish")',
    ):
        if required not in atomic_publish:
            raise AssertionError(f"atomic champion publication lost invariant: {required}")
    if "is_placeholder_transcript(&segment.raw_transcript)" not in atomic_publish:
        raise AssertionError("atomic champion publication no longer rejects placeholders")
    if "champion_file_publication_is_atomic_and_never_exposes_placeholders" not in read("stores/import_write.rs"):
        raise AssertionError("atomic champion publication needs an injected-failure Rust regression")

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
