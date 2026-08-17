"""Audio that was PROCESSED before import must never be exported as if it were an original.

The owner's pre-import cleaner (`kurdish-audio-cleaner`) separates voice from music with a neural
model, CUTS every non-speech stretch out, re-concatenates what survives with 150 ms pauses, and
normalises the level. Its output is an ordinary WAV. Nothing in the file, and until migration v54
nothing in this database, distinguished it from a raw field recording — so a dataset card would have
described machine-separated audio in exactly the words used for the owner's own microphone captures.

Three pins, one per link in the chain that keeps the claim attached to the audio:

  1. the schema can hold the declaration,
  2. the import records it without anyone having to remember, and
  3. the export prints it.

Break any one and the other two go quiet, which is worse than never having had them: the export
would then assert provenance it no longer checks.
"""

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]


def _read(relative_path: str) -> str:
    return (REPO_ROOT / relative_path).read_text(encoding="utf-8")


def test_the_schema_can_hold_a_processing_declaration() -> None:
    migrations = _read("src-tauri/src/migrations/mod.rs")
    if "source_audio_provenance" not in migrations:
        raise AssertionError("migration v54 must create source_audio_provenance")
    for column in ("processing", "separator_model", "timeline_preserved", "manifest_path"):
        if column not in migrations:
            raise AssertionError(f"source_audio_provenance must carry the {column!r} column")

    db = _read("src-tauri/src/db.rs")
    for symbol in (
        "pub struct SourceAudioProvenance",
        "pub fn upsert_source_audio_provenance",
        "pub fn source_audio_provenance_map",
    ):
        if symbol not in db:
            raise AssertionError(f"db.rs must expose {symbol!r}")


def test_the_import_records_it_without_being_asked() -> None:
    pipeline = _read("src-tauri/src/pipeline.rs")
    if "crate::source_provenance::detect(path)" not in pipeline:
        raise AssertionError(
            "the import must detect processed source audio itself — a declaration that depends on "
            "someone remembering to run a command is not a guarantee"
        )
    if "upsert_source_audio_provenance" not in pipeline:
        raise AssertionError("the import must persist what it detected")

    detector = _read("src-tauri/src/source_provenance.rs")
    if "audio_is_processed" not in detector:
        raise AssertionError(
            "detection must require the manifest's explicit processing claim — inferring it from a "
            "folder name is wrong in both directions: it brands originals as processed, and it misses "
            "a cleaned tree named something else"
        )
    if "fn nothing_is_claimed_without_a_manifest_that_says_so" not in detector:
        raise AssertionError(
            "source_provenance.rs must keep the test proving an unmarked recording, a suggestively "
            "named folder, and an unrelated manifest.json all produce NO claim"
        )


def test_the_export_prints_it() -> None:
    export = _read("src-tauri/src/export.rs")
    for symbol in ("pub processed_audio: Vec<ProcessedAudioNotice>", "struct ProcessedAudioNotice", "processed_audio_notices("):
        if symbol not in export:
            raise AssertionError(f"export.rs must carry {symbol!r} so a dataset card declares processed audio")
    if "timeline_preserved" not in export:
        raise AssertionError(
            "the export notice must state whether the timeline survived: a consumer that trusts an "
            "offset into audio which had its silence cut out will mis-locate every clip"
        )

    tests = _read("src-tauri/src/export_tests.rs")
    if "an_export_declares_which_recordings_were_processed_before_import" not in tests:
        raise AssertionError("export_tests.rs must keep the declaration test")


def main() -> None:
    test_the_schema_can_hold_a_processing_declaration()
    test_the_import_records_it_without_being_asked()
    test_the_export_prints_it()
    print("processed-audio provenance policy passed (3 assertions)")


if __name__ == "__main__":
    main()
