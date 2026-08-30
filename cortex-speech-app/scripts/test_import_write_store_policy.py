"""Architecture policy for fail-closed import publication and metadata backfills."""

from pathlib import Path

from _db_policy_util import database_surface
import re


ROOT = Path(__file__).resolve().parents[1] / "src-tauri" / "src"


def read(relative: str) -> str:
    if relative == "db.rs":
        return database_surface(ROOT)
    return (ROOT / relative).read_text(encoding="utf-8")


def method(source: str, signature: str) -> str:
    start = source.find(signature)
    if start < 0:
        raise AssertionError(f"missing pipeline method `{signature}`")
    next_method = re.search(
        r"^    (?:(?:pub(?:\(crate\)|\(super\))?)\s+)?(?:async\s+)?fn ",
        source[start + len(signature) :],
        flags=re.MULTILINE,
    )
    if next_method is None:
        return source[start:]
    end = start + len(signature) + next_method.start()
    return source[start:end]


def test_store_owns_import_publication_rollback_and_revision_guarded_alignment() -> None:
    store = read("stores/import_write.rs")
    for forbidden in ("tauri", "crate::commands", "crate::http"):
        if forbidden in store:
            raise AssertionError(f"ImportWriteStore crossed a forbidden layer: {forbidden}")
    for required in (
        "struct ImportWriteStore",
        "runtime: DatabaseRuntime",
        "self.runtime.begin_mutation().map_err(AppError::Other)",
        "self.runtime.lock_after_mutation(mutation)",
        'self.lock_after_mutation("publish_import_segments", &mutation)',
        ".insert_segments_with_provenance_batch(segments, provenance)",
        'self.lock_after_mutation("publish_import_segments_with_identity", &mutation)',
        ".insert_segments_with_audio_identity_and_provenance_batch(segments, identity, provenance)",
        'self.lock_after_mutation("publish_champion_import_segments", &mutation)',
        ".insert_champion_segments_with_provenance_batch(",
        "deployment_sha256,",
        'self.lock_after_mutation("rollback_import_segments", &mutation).delete_segments_batch(segment_ids)',
        'self.lock_after_mutation("update_import_alignment", &mutation)',
        'self.lock_after_mutation("upsert_import_source_transcript", &mutation).upsert_source_transcript(record)',
        'self.lock_after_mutation("upsert_import_source_provenance", &mutation).upsert_source_audio_provenance(record)',
        'self.lock_after_mutation("record_import_loop0_shadow", &mutation).record_loop0_shadow(segment_id, memory_fired)',
        'self.lock_after_mutation("update_machine_speaker", &mutation).update_speaker_id(segment_id, Some(speaker_id))',
        'self.lock_after_mutation("insert_import_hypothesis", &mutation).insert_hypothesis(hypothesis)',
        'self.lock_after_mutation("commit_import_champion_transcript", &mutation)',
    ):
        if required not in store:
            raise AssertionError(f"ImportWriteStore lost required authority: {required}")


def test_pipeline_delegates_import_segment_writes_without_raw_writer_calls() -> None:
    pipeline_root = read("pipeline.rs")
    pipeline_import = read("pipeline/import_flow.rs")
    pipeline_source_reference = read("pipeline/source_reference.rs")
    pipeline_transcription = read("pipeline/transcription.rs")
    pipeline = "\n".join((pipeline_root, pipeline_import, pipeline_source_reference, pipeline_transcription))
    for module in ("mod import_flow;", "mod source_reference;", "mod transcription;"):
        if module not in pipeline_root:
            raise AssertionError(f"pipeline composition root lost `{module}`")
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
    if "import_writes.publish_segments(&segments, source_provenance)?" not in persist:
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
    if pipeline.count("import_writes.publish_champion_segments(") != 2:
        raise AssertionError("both import paths must delegate atomic champion publication through ImportWriteStore")
    if pipeline.count("self.run_primary_wsl_pass_for_import(&mut prepared, cancel)?") != 2:
        raise AssertionError("both import paths must complete champion drafting before publication")
    if pipeline.count("publish_segments_with_identity(&prepared, &identity,") != 2:
        raise AssertionError("both compatibility import paths must atomically publish rows with scoped identity")
    if "let source_provenance = crate::source_provenance::detect(path);" not in pipeline:
        raise AssertionError("import lost its pre-decode preprocessing-provenance snapshot")
    preflight = method(pipeline, "fn process_single_file_under_source_lease(")
    if ".upsert_source_audio_provenance(" in preflight:
        raise AssertionError("preprocessing provenance must not publish before segment truth")
    single_file = method(pipeline, "pub fn import_single_file_with_events(")
    seal = single_file.find("crate::media::seal_import_source(path)")
    duration = single_file.find("audio::get_duration_ms(path)?")
    if seal < 0 or duration < 0 or seal > duration:
        raise AssertionError("single-file import must seal the exact path before its first decoder probe")
    if "self.process_single_file_under_source_lease(" not in single_file or "&source_lease," not in single_file:
        raise AssertionError("single-file import does not retain its source lease through publication")

    media = read("media.rs")
    for required in (
        "struct ImportMediaSourceLease",
        "_path_guards: Vec<std::fs::File>",
        "fn seal_import_parent_chain(path: &Path)",
        ".share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)",
        "replacement cannot swap an ancestor directory while path-reopening inference is live",
    ):
        if required not in media:
            raise AssertionError(f"Windows import source-path lease lost invariant: {required}")
    windows_chain = method(media, "fn seal_import_parent_chain(path: &Path)")
    if "const FILE_SHARE_DELETE" in windows_chain or ".share_mode(FILE_SHARE_DELETE" in windows_chain:
        raise AssertionError("import parent handles must exclude Windows FILE_SHARE_DELETE")

    background = method(pipeline, "fn enqueue_background_alignments(")
    if "import_writes.update_alignment_if_unchanged(" not in background:
        raise AssertionError("background alignment lost revision-CAS store delegation")
    if "source_alignment.as_deref()" not in background:
        raise AssertionError("background alignment no longer compares the exact pre-inference metadata")
    if "crate::quality::effective_transcript(&s)" not in background:
        raise AssertionError("background alignment must time the exact authoritative review projection")

    champion = method(pipeline, "fn transcribe_draft_only(")
    for required in (
        "let runtime = self.shared_database_runtime(&self.db_path)?;",
        "let db = runtime.open_read()?;",
    ):
        if required not in champion:
            raise AssertionError(f"champion drafting lost bounded read authority: {required}")
    if "Database::open(&self.db_path)" in champion:
        raise AssertionError("champion drafting regained an independent raw database connection")
    if ".connection()" in champion:
        raise AssertionError("champion drafting regained a raw database connection escape")
    for forbidden in (
        "ImportWriteStore",
        ".commit_bound_champion_transcript_if_unreviewed(",
        ".insert_hypothesis(",
        "populate_hypotheses_reusing_primary(",
    ):
        if forbidden in champion:
            raise AssertionError(f"draft-only inference regained a pre-publication write: {forbidden}")
    if "segment_queries.resolve_transcription_segment(&audio_path_str, alignment_json)?" not in champion:
        raise AssertionError("champion drafting bypasses the bounded segment query store")
    import_draft = method(pipeline, "fn transcribe_import_draft_only(")
    if "self.transcribe_draft_only(Some(segment_id), audio_path, alignment_json, cancel)" not in import_draft:
        raise AssertionError("import inference must delegate to the side-effect-free draft primitive")

    database = read("db.rs")
    atomic_publish = method(database, "pub(crate) fn insert_champion_segments_batch(")
    for required in (
        'self.conn.execute("SAVEPOINT champion_import_publish", [])?',
        "self.insert_segments_batch(segments)?;",
        'DELETE FROM segment_hypotheses WHERE segment_id = ?1',
        "self.set_audio_identity_for_segments(audio_path, &segment_ids, identity)?;",
        'self.release_savepoint("champion_import_publish")?',
        'self.cleanup_savepoint_after_error("champion_import_publish")',
    ):
        if required not in atomic_publish:
            raise AssertionError(f"atomic champion publication lost invariant: {required}")
    if "is_placeholder_transcript(&segment.raw_transcript)" not in atomic_publish:
        raise AssertionError("atomic champion publication no longer rejects placeholders")
    if "champion_file_publication_is_atomic_and_never_exposes_placeholders" not in read("stores/import_write.rs"):
        raise AssertionError("atomic champion publication needs an injected-failure Rust regression")

    provenance_publish = method(database, "pub(crate) fn insert_champion_segments_with_provenance_batch(")
    for required in (
        'self.conn.execute("SAVEPOINT champion_import_source_publish", [])?',
        "self.insert_champion_segments_batch(segments, deployment_sha256, identity)?;",
        "self.upsert_source_audio_provenance(provenance)?;",
        'self.release_savepoint("champion_import_source_publish")?',
        'self.cleanup_savepoint_after_error("champion_import_source_publish")',
    ):
        if required not in provenance_publish:
            raise AssertionError(f"champion+provenance publication lost invariant: {required}")
    store_tests = read("stores/import_write.rs")
    for required in (
        "CREATE TRIGGER fail_import_source_provenance",
        "a provenance failure must roll back rows, champion hypotheses and recording identity",
        "source_audio_provenance(audio_path).unwrap().is_none()",
    ):
        if required not in store_tests:
            raise AssertionError(f"provenance rollback proof lost invariant: {required}")

    scoped_publish = method(database, "pub(crate) fn insert_segments_with_audio_identity_batch(")
    for required in (
        'self.conn.execute("SAVEPOINT import_identity_publish", [])?',
        "self.insert_segments_batch(segments)?;",
        "self.set_audio_identity_for_segments(audio_path, &segment_ids, identity)?;",
        'self.release_savepoint("import_identity_publish")?',
        'self.cleanup_savepoint_after_error("import_identity_publish")',
    ):
        if required not in scoped_publish:
            raise AssertionError(f"scoped import identity publication lost invariant: {required}")
    path_identity = method(database, "pub fn set_audio_identity(")
    if "self.ensure_audio_identity_compatible(audio_path, identity)?;" not in path_identity:
        raise AssertionError("legacy path-wide identity backfill can rebind changed source bytes")

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
