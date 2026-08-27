//! Unit tests for `db.rs`, split out via `#[path]` (Week-4 decomposition) to keep db.rs itself
//! under the 3-4k-line target. Included from db.rs as `#[cfg(test)] #[path = "db_tests.rs"] mod tests;`
//! so `super::*` still resolves to the `db` module. Tests are UNCHANGED — only relocated.

use super::*;

const TEST_AUDIO_CONTENT_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_AUDIO_CONTENT_HASH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn make_db() -> Database {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    db
}

#[test]
fn detached_read_snapshot_cannot_mutate_its_source_database() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("read-only.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        db.initialize().unwrap();
    }
    let snapshot = Database::open_detached_read_snapshot(path.to_str().unwrap()).unwrap();
    assert_eq!(crate::migrations::validate_applied_history(snapshot.connection()).unwrap(), 67);
    snapshot.connection().execute("INSERT INTO settings(key,value) VALUES('must-not-write','x')", []).unwrap();
    assert_eq!(snapshot.integrity_check().unwrap(), "ok", "FTS5 validation runs on the writable private copy");
    drop(snapshot);
    let source = Database::open(path.to_str().unwrap()).unwrap();
    let persisted: i64 = source
        .connection()
        .query_row("SELECT COUNT(*) FROM settings WHERE key='must-not-write'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(persisted, 0, "writes to the certification snapshot must never reach its source file");
}

#[test]
fn direct_read_only_connection_measures_the_source_but_cannot_write_it() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("query-only.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        db.initialize().unwrap();
    }
    let reader = Database::open_read_only(path.to_str().unwrap()).unwrap();
    assert_eq!(crate::migrations::validate_applied_history(reader.connection()).unwrap(), 67);
    assert!(reader.connection().execute("INSERT INTO settings(key,value) VALUES('must-not-write','x')", []).is_err());
    drop(reader);

    let source = Database::open(path.to_str().unwrap()).unwrap();
    let persisted: i64 = source
        .connection()
        .query_row("SELECT COUNT(*) FROM settings WHERE key='must-not-write'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(persisted, 0);
}

fn make_segment(id: &str, audio_path: &str) -> SpeechSegment {
    SpeechSegment {
        id: id.to_string(),
        audio_path: audio_path.to_string(),
        raw_transcript: "test".to_string(),
        duration_ms: 1000,
        ..SpeechSegment::default()
    }
}

#[test]
fn legacy_verified_bit_writer_is_disabled_at_schema_v60() {
    let db = make_db();
    db.insert_segment(&make_segment("verify-disabled", "/verify-disabled.wav")).unwrap();
    let error = db.update_verified("verify-disabled", true).unwrap_err();
    assert!(matches!(error, AppError::Validation(_)), "{error}");
    assert!(error.to_string().contains("legacy batch verify/unverify is disabled"), "{error}");
    let row = db.get_segment_by_id("verify-disabled").unwrap().unwrap();
    assert!(!row.verified && row.human_decision.is_none());
    let effects: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM human_decision_effect_events WHERE segment_id = 'verify-disabled'",
            [],
            |result| result.get(0),
        )
        .unwrap();
    assert_eq!(effects, 0, "a refused legacy writer must publish no pseudo-review authority");
}

fn make_hidden_check_segment(id: &str, audio_path: &str, expected: &str) -> SpeechSegment {
    SpeechSegment {
        id: id.to_string(),
        audio_path: audio_path.to_string(),
        raw_transcript: expected.to_string(),
        verdict_transcript: Some(expected.to_string()),
        verdict: Some("human_accept".into()),
        human_decision: Some("accept".into()),
        verified: true,
        duration_ms: 1_000,
        ..SpeechSegment::default()
    }
}

fn test_source_span(segment_id: &str, duration_ms: i64) -> (i64, i64) {
    let digest = blake3::hash(segment_id.as_bytes());
    let mut offset_bytes = [0_u8; 8];
    offset_bytes.copy_from_slice(&digest.as_bytes()[..8]);
    let source_start_ms = (u64::from_le_bytes(offset_bytes) % 1_000_000_000) as i64;
    (source_start_ms, source_start_ms + duration_ms)
}

fn ensure_test_audio_content_hash(db: &Database, segment_id: &str) -> String {
    let duration_ms: i64 = db
        .connection()
        .query_row("SELECT duration_ms FROM speech_segments WHERE id = ?1", [segment_id], |row| row.get(0))
        .unwrap();
    let (source_start_ms, _) = test_source_span(segment_id, duration_ms);
    db.connection()
        .execute(
            "UPDATE speech_segments
                SET audio_content_hash = ?2,
                    alignment_json = COALESCE(
                        alignment_json,
                        json_object('source_start_ms', ?3, 'source_end_ms', ?3 + duration_ms)
                    )
              WHERE id = ?1
                AND NULLIF(TRIM(COALESCE(audio_content_hash, '')), '') IS NULL",
            params![segment_id, TEST_AUDIO_CONTENT_HASH, source_start_ms],
        )
        .unwrap();
    db.segment_audio_content_hash(segment_id)
        .unwrap()
        .expect("playback fixture must have a server-row audio content hash")
}

fn full_playback_proof(db: &Database, segment_id: &str, reviewer: &str) -> PlaybackDecisionProof {
    let audio_content_hash = ensure_test_audio_content_hash(db, segment_id);
    let segment_revision = db.segment_review_revision(segment_id).unwrap().unwrap();
    let (source_start_ms, source_end_ms) = db.segment_source_span(segment_id).unwrap().unwrap();
    let receipt = PlaybackReceipt {
        segment_id: segment_id.to_string(),
        segment_revision,
        audio_content_hash: audio_content_hash.clone(),
        reviewer: Some(reviewer.to_string()),
        session_id: Some("hidden-test".into()),
        started_at_ms: 1_700_000_000_000,
        played_ms: 1_000,
        clip_duration_ms: 1_000,
        source_start_ms: None,
        source_end_ms: None,
    };
    assert!(db.record_playback_receipt_if_at_revision(&receipt, segment_revision).unwrap());
    PlaybackDecisionProof {
        segment_revision,
        audio_content_hash,
        source_start_ms,
        source_end_ms,
        authority_session_id: None,
        source_lease: None,
    }
}

/// Real policy-4 Couch evidence for tests that exercise the production proof-bearing writer. The
/// temporary source stays alive through the decision transaction so the verified source lease can
/// be rechecked instead of relying on the legacy policy-3 test fixture above.
struct CanonicalPolicy4Playback {
    proof: PlaybackDecisionProof,
    _source: tempfile::TempDir,
}

impl std::ops::Deref for CanonicalPolicy4Playback {
    type Target = PlaybackDecisionProof;

    fn deref(&self) -> &Self::Target {
        &self.proof
    }
}

fn canonical_policy4_phone_playback(db: &Database, segment_id: &str, reviewer: &str) -> CanonicalPolicy4Playback {
    let duration_ms: i64 = db
        .connection()
        .query_row("SELECT duration_ms FROM speech_segments WHERE id=?1", [segment_id], |row| row.get(0))
        .unwrap();
    assert!(duration_ms > 0, "a policy-4 playback fixture needs a positive clip duration");

    let source = tempfile::tempdir().unwrap();
    let source_path = source.path().join("canonical-policy4.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&source_path, spec).unwrap();
    for sample in 0..duration_ms * 16 {
        writer.write_sample::<i16>(((sample % 257) - 128) as i16).unwrap();
    }
    writer.finalize().unwrap();
    let content_hash = crate::export_bundle::current_canonical_pcm_blake3(&source_path).unwrap();
    db.connection()
        .execute(
            "UPDATE speech_segments
                SET audio_path=?2,
                    audio_content_hash=?3,
                    alignment_json=json_object('source_start_ms', 0, 'source_end_ms', ?4)
              WHERE id=?1",
            params![segment_id, source_path.to_string_lossy(), content_hash, duration_ms],
        )
        .unwrap();

    let segment_revision = db.segment_review_revision(segment_id).unwrap().unwrap();
    let session_binding_sha256 = "c".repeat(64);
    let issued_at_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let authority = CouchPlaybackAttemptAuthority {
        playback_receipt_id: uuid::Uuid::new_v4().to_string(),
        media_grant_id: uuid::Uuid::new_v4().to_string(),
        client_attempt_id: uuid::Uuid::new_v4().to_string(),
        session_binding_sha256: session_binding_sha256.clone(),
        reviewer: reviewer.to_string(),
        segment_id: segment_id.to_string(),
        segment_revision,
        audio_content_hash: content_hash.clone(),
        source_path,
        clip_duration_ms: duration_ms,
        source_start_ms: 0,
        source_end_ms: duration_ms,
        issued_at_ms,
        expires_at_ms: issued_at_ms + 60_000,
    };
    let receipt = db
        .finalize_couch_playback_attempt_v1(
            &authority,
            &[DesktopPlaybackInterval { start_ms: 0, end_ms: duration_ms }],
            duration_ms,
        )
        .unwrap();
    let proof = db
        .couch_playback_proof_v4(
            segment_id,
            segment_revision,
            &content_hash,
            reviewer,
            &session_binding_sha256,
            &receipt.playback_receipt_id,
        )
        .unwrap()
        .expect("canonical policy-4 receipt must resolve to its exact source lease");
    CanonicalPolicy4Playback { proof, _source: source }
}

fn latest_human_effect_id(db: &Database, segment_id: &str) -> i64 {
    db.connection()
        .query_row(
            "SELECT id FROM human_decision_effect_events WHERE segment_id = ?1 ORDER BY id DESC LIMIT 1",
            [segment_id],
            |row| row.get(0),
        )
        .expect("human decision must publish its immutable effect")
}

fn record_test_phone_decision(
    db: &Database,
    segment_id: &str,
    decision: &str,
    corrected_transcript: Option<&str>,
    reviewer: &str,
) {
    ensure_test_audio_content_hash(db, segment_id);
    let revision = db.segment_review_revision(segment_id).unwrap().unwrap();
    assert!(
        db.record_phone_human_decision_by_at_revision(segment_id, decision, corrected_transcript, reviewer, revision,)
            .unwrap()
            .is_some(),
        "the attributed phone decision must win its revision CAS"
    );
}

/// Migration v49 / audit #6. The DEFAULT must fail closed: an existing library has no recorded
/// rights, and "nothing recorded" must never read as "permission granted".
#[test]
fn undeclared_rights_are_unknown_and_never_permit_redistribution() {
    let db = make_db();
    db.insert_segments_batch(&[make_segment("r-unknown", "/a.wav")]).unwrap();

    let rights = db.rights_for_segment("r-unknown").unwrap();
    assert_eq!(rights, RecordingRights::default(), "a legacy row records nothing");
    assert_eq!(rights.disposition(), RightsDisposition::Unknown);
    assert!(!rights.permits_redistribution(), "unknown rights MUST NOT permit republishing a voice");
    assert!(!rights.is_revoked(), "unknown is not the same as withdrawn");

    // A missing segment must also fail closed rather than error into a permissive default.
    let absent = db.rights_for_segment("no-such-segment").unwrap();
    assert!(!absent.permits_redistribution());
}

/// A licence alone is NOT consent to republish someone's voice: `permitted_use` must name it.
#[test]
fn a_licence_without_a_permitted_use_is_private_only() {
    let db = make_db();
    db.insert_segments_batch(&[make_segment("r-priv", "/b.wav")]).unwrap();
    db.set_recording_rights(
        "/b.wav",
        &RecordingRights {
            license: Some("CC-BY-4.0".into()),
            consent_basis: Some("explicit_consent".into()),
            permitted_use: Some("train".into()), // train, but NOT redistribute
            attribution: Some("Speaker A".into()),
            source: Some("owner recording 2026-08".into()),
            revoked_at: None,
        },
    )
    .unwrap();

    let rights = db.rights_for_segment("r-priv").unwrap();
    assert_eq!(rights.disposition(), RightsDisposition::PrivateOnly);
    assert!(!rights.permits_redistribution(), "'train' does not imply 'redistribute'");

    // Naming it flips exactly one thing.
    db.set_recording_rights(
        "/b.wav",
        &RecordingRights { permitted_use: Some("train,redistribute".into()), ..rights.clone() },
    )
    .unwrap();
    assert!(db.rights_for_segment("r-priv").unwrap().permits_redistribution());
}

/// Rights are declared per RECORDING: one call covers every clip cut from that file, and touches no
/// other recording's clips.
#[test]
fn declaring_rights_covers_every_segment_of_that_recording_and_no_others() {
    let db = make_db();
    db.insert_segments_batch(&[
        make_segment("s1", "/same.wav"),
        make_segment("s2", "/same.wav"),
        make_segment("s3", "/other.wav"),
    ])
    .unwrap();

    let n = db
        .set_recording_rights(
            "/same.wav",
            &RecordingRights {
                license: Some("CC-BY-4.0".into()),
                consent_basis: Some("public_dataset_licence".into()),
                permitted_use: Some("redistribute".into()),
                ..RecordingRights::default()
            },
        )
        .unwrap();

    assert_eq!(n, 2, "both clips of the recording are covered by one declaration");
    assert!(db.rights_for_segment("s1").unwrap().permits_redistribution());
    assert!(db.rights_for_segment("s2").unwrap().permits_redistribution());
    assert_eq!(
        db.rights_for_segment("s3").unwrap().disposition(),
        RightsDisposition::Unknown,
        "a different recording is untouched — consent does not spread across files"
    );
}

/// Withdrawal outranks everything, and re-declaring a licence must not resurrect it.
#[test]
fn revocation_outranks_a_full_licence_and_survives_a_rights_rewrite() {
    let db = make_db();
    db.insert_segments_batch(&[make_segment("r-rev", "/c.wav")]).unwrap();
    let full = RecordingRights {
        license: Some("CC-BY-4.0".into()),
        consent_basis: Some("explicit_consent".into()),
        permitted_use: Some("train,redistribute".into()),
        ..RecordingRights::default()
    };
    db.set_recording_rights("/c.wav", &full).unwrap();
    assert!(db.rights_for_segment("r-rev").unwrap().permits_redistribution());

    assert_eq!(db.revoke_recording("/c.wav").unwrap(), 1);
    let revoked = db.rights_for_segment("r-rev").unwrap();
    assert_eq!(revoked.disposition(), RightsDisposition::Revoked);
    assert!(revoked.is_revoked());
    assert!(!revoked.permits_redistribution(), "a withdrawn recording is never redistributable");

    // THE POINT: re-declaring the same full rights must not un-revoke it. A withdrawal that a later
    // metadata edit could silently undo is not a withdrawal.
    db.set_recording_rights("/c.wav", &full).unwrap();
    assert!(
        db.rights_for_segment("r-rev").unwrap().is_revoked(),
        "re-declaring a licence resurrected a withdrawn recording"
    );
}

#[test]
fn per_segment_processing_provenance_round_trips_and_stays_unknown_for_legacy_rows() {
    // P0.4 (H3): denoised/diarized are persisted per segment (Migration v41) so a future export reads
    // stored per-segment truth instead of recomputing from export-day model loadability. A fresh in-memory
    // DB applies v41, so these round-trips also prove the migration ran. DISTINCT true/false values (never
    // equal within a row) make the assertions catch a positional SELECT/map_row/INSERT column swap; None
    // must persist as SQL NULL — "not recorded" — never a fabricated false (the honesty-law posture).
    let db = make_db();

    // IMPORT path — insert_segments_batch (what pipeline.rs::persist_segments actually calls).
    let mut imported = make_segment("prov-import", "/a.wav");
    imported.denoised = Some(true);
    imported.diarized = Some(false);
    imported.vad_backend = Some("silero".to_string()); // v42
    db.insert_segments_batch(std::slice::from_ref(&imported)).unwrap();
    let got = db.get_segment_by_id("prov-import").unwrap().expect("imported segment persisted");
    assert_eq!(got.denoised, Some(true), "denoised must round-trip true via the batch import path");
    assert_eq!(got.diarized, Some(false), "diarized must round-trip false — NOT the denoised value (positional guard)");
    assert_eq!(got.vad_backend.as_deref(), Some("silero"), "vad_backend must round-trip via the batch import path");

    // RESTORE path — insert_segment_full must be lossless (opposite values from the import row).
    let mut restored = make_segment("prov-restore", "/b.wav");
    restored.denoised = Some(false);
    restored.diarized = Some(true);
    restored.vad_backend = Some("energy".to_string());
    db.insert_segment_full(&restored).unwrap();
    let got = db.get_segment_by_id("prov-restore").unwrap().expect("restored segment persisted");
    assert_eq!(got.denoised, Some(false), "denoised must round-trip false via insert_segment_full");
    assert_eq!(got.diarized, Some(true), "diarized must round-trip true via insert_segment_full");
    assert_eq!(got.vad_backend.as_deref(), Some("energy"), "vad_backend must round-trip via insert_segment_full");

    // NOT-RECORDED — a row that never set the fields (a legacy pre-v41/v42 row reads identically: NULL).
    let legacy = make_segment("prov-legacy", "/c.wav");
    assert_eq!(legacy.denoised, None, "default construction leaves provenance unrecorded");
    assert_eq!(legacy.vad_backend, None, "default construction leaves vad_backend unrecorded");
    db.insert_segment(&legacy).unwrap();
    let got = db.get_segment_by_id("prov-legacy").unwrap().expect("legacy segment persisted");
    assert_eq!(got.denoised, None, "unrecorded denoising must persist as NULL/None, never a fabricated false");
    assert_eq!(got.diarized, None, "unrecorded diarization must persist as NULL/None, never a fabricated false");
    assert_eq!(got.vad_backend, None, "unrecorded vad_backend must persist as NULL/None");
}

#[test]
fn the_speaker_change_measurement_survives_every_write_that_is_not_about_it() {
    // Migration v47. The score is measured by a whole-library pass that takes minutes; anything that
    // silently drops it costs that pass again, and nothing would say so — the clip just stops being
    // flagged and goes back to looking like ordinary work on the phone.
    //
    // So `speaker_change_score` is deliberately ABSENT from `insert_segment`'s column list: that path's
    // ON CONFLICT DO UPDATE would otherwise write the caller's `None` over a real measurement on every
    // ordinary edit. It IS in `insert_segment_full`, which runs as a fresh INSERT after a delete and
    // must be lossless. This pins both halves of that asymmetry.
    let db = make_db();
    let seg = make_segment("sc-keep", "/a.wav");
    db.insert_segment(&seg).unwrap();
    assert_eq!(
        db.get_segment_by_id("sc-keep").unwrap().unwrap().speaker_change_score,
        None,
        "an unmeasured clip is NULL — not measured, never a fabricated 'one speaker'"
    );

    db.set_speaker_change_score("sc-keep", 0.4121).unwrap();
    assert_eq!(db.get_segment_by_id("sc-keep").unwrap().unwrap().speaker_change_score, Some(0.4121));

    // The edit path re-upserts the row it read BEFORE the measurement landed — the ordinary case, since
    // nothing outside the probe carries this field.
    db.insert_segment(&seg).unwrap();
    assert_eq!(
        db.get_segment_by_id("sc-keep").unwrap().unwrap().speaker_change_score,
        Some(0.4121),
        "an ordinary edit-path upsert must not wipe the measurement it knows nothing about"
    );

    // ...and the restore path carries it, with a DIFFERENT value so a positional column swap cannot
    // pass by coincidence.
    let mut restored = make_segment("sc-restore", "/b.wav");
    restored.speaker_change_score = Some(0.7530);
    db.insert_segment_full(&restored).unwrap();
    assert_eq!(
        db.get_segment_by_id("sc-restore").unwrap().unwrap().speaker_change_score,
        Some(0.7530),
        "insert_segment_full is the lossless restore path and must persist the score"
    );
}

#[test]
fn insert_segment_rejects_unc_audio_path_ntlm_leak_guard() {
    // P1.1: validate_segment is the shared DB write boundary for merge_dataset_json AND every insert
    // path, so a UNC/network audio_path must be rejected here — otherwise a renderer-planted
    // `\\attacker\share\clip.wav` reaches the row and drives the SMB redirector (NTLM forced-auth leak)
    // the moment any later exists()/decode touches it. A plain local path still inserts. UNC is a
    // Windows concept, so the rejection assertion is windows-gated; the accept path runs everywhere.
    let db = make_db();
    db.insert_segment(&make_segment("ok-1", "/a/local.wav")).expect("a plain local audio_path must still insert");
    #[cfg(windows)]
    {
        let err = db.insert_segment(&make_segment("unc-1", r"\\attacker\share\clip.wav")).unwrap_err();
        assert!(format!("{err}").contains("UNC"), "a UNC audio_path must be rejected at the write boundary: {err}");
        assert!(db.get_segment_by_id("unc-1").unwrap().is_none(), "the rejected UNC row must never be persisted");
    }
}

#[test]
fn update_normalized_transcript_is_targeted_and_avoids_the_whole_row_clobber() {
    // iter-88: batch_normalize used a read-modify-write + whole-row insert_segment upsert. A concurrent
    // write to the SAME segment (e.g. a background aligner / 7B pass on the pipeline's own connection)
    // landing between the re-read and the upsert was silently CLOBBERED. The targeted update writes
    // ONLY normalized_transcript, so a concurrent edit to any other column survives.
    let db = make_db();
    let seg = make_segment("n1", "/a.wav");
    db.insert_segment(&seg).unwrap();

    // Snapshot the row as the OLD batch_normalize re-read it (annotated is None here).
    let stale = db.get_segment_by_id("n1").unwrap().unwrap();
    assert_eq!(stale.annotated_transcript, None);

    // A concurrent human edit lands AFTER that snapshot (targeted write to a different column).
    db.connection()
        .execute("UPDATE speech_segments SET annotated_transcript = 'human fix' WHERE id = 'n1'", [])
        .unwrap();

    // NEW targeted path: writes ONLY normalized_transcript -> the concurrent edit SURVIVES.
    assert!(db.update_normalized_transcript("n1", "NORMALIZED").unwrap(), "row found and updated");
    let after = db.get_segment_by_id("n1").unwrap().unwrap();
    assert_eq!(after.normalized_transcript.as_deref(), Some("NORMALIZED"));
    assert_eq!(
        after.annotated_transcript.as_deref(),
        Some("human fix"),
        "targeted update must not clobber a concurrent edit to another column"
    );
    // A missing row reports false, not an error.
    assert!(!db.update_normalized_transcript("ghost", "x").unwrap());

    // The schema-v60 generic machine upsert is now also review-safe: even a stale full row cannot
    // name or erase the concurrent human annotation.
    let mut whole_row = stale.clone();
    whole_row.normalized_transcript = Some("NORMALIZED".to_string());
    db.insert_segment(&whole_row).unwrap();
    let preserved = db.get_segment_by_id("n1").unwrap().unwrap();
    assert_eq!(
        preserved.annotated_transcript.as_deref(),
        Some("human fix"),
        "the schema-v60 machine upsert must preserve review-owned truth even from a stale snapshot"
    );
}

#[test]
fn dropping_speech_segments_cascade_deletes_children_so_strict_recreate_needs_fk_off() {
    // Week-2's LAST open item is the STRICT conversion of speech_segments. SQLite cannot ALTER a
    // table to STRICT, so the only path is the recreate: new STRICT table -> copy -> DROP old ->
    // RENAME. This test proves WHY the naive recreate (the v38 decision_verdicts pattern) is
    // DATA-DESTROYING for THIS table and must not be shipped inside a normal migration.
    //
    // speech_segments is an FK PARENT of seven child tables: FIVE are ON DELETE CASCADE
    // (segment_hypotheses, agent_examples, decision_log, decision_verdicts, loop0_shadow_log) and
    // two are ON DELETE SET NULL (correction_memory.source_segment, corrections.segment_id). This
    // test probes decision_verdicts, a CASCADE child, whose rows are DELETED (not nulled). With
    // foreign_keys=ON (the app default, Database::open db.rs:246), `DROP TABLE speech_segments`
    // performs an implicit DELETE of every parent row, which FIRES ON DELETE CASCADE and wipes the
    // child rows. apply_migration runs up_sql inside `unchecked_transaction()`, and
    // `PRAGMA foreign_keys=OFF` is a NO-OP inside a transaction — so a v39 migration literally
    // cannot turn the cascade off. The correct conversion must run with foreign_keys OFF *outside*
    // a transaction (SQLite's 12-step recreate); see docs/STRICT_SPEECH_SEGMENTS_PLAN.md.
    let db = make_db(); // foreign_keys=ON
    db.insert_segment(&make_segment("seg-1", "/a.wav")).unwrap();
    db.write_legacy_machine_verdict_for_test("seg-1", "auto_accept", Some("t"), None, None, Some(0.9), false).unwrap();
    let child_before: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM decision_verdicts WHERE segment_id='seg-1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(child_before, 1, "precondition: the child verdict row exists via the real write path");

    // Faithfully reproduce a naive STRICT-recreate migration's DROP, inside a transaction exactly
    // as apply_migration would run it.
    let conn = db.connection();
    let tx = conn.unchecked_transaction().unwrap();
    tx.execute_batch("DROP TABLE speech_segments").unwrap();
    tx.commit().unwrap();

    let child_after: i64 =
        db.connection().query_row("SELECT COUNT(*) FROM decision_verdicts", [], |r| r.get(0)).unwrap();
    assert_eq!(
        child_after, 0,
        "DROP TABLE speech_segments with foreign_keys=ON cascade-deleted the child verdict rows: \
             the naive STRICT recreate is DATA-DESTROYING and must run with foreign_keys OFF, outside a txn"
    );
}

#[test]
fn schema60_refuses_every_machine_jury_writer_before_mutation() {
    let registrations = include_str!("lib.rs");
    for command in ["commands::run_t0_gate", "commands::run_jury_pipeline", "commands::run_t2_for_segment"] {
        assert!(
            registrations.contains(command),
            "registered jury command disappeared from the policy inventory: {command}"
        );
    }
    let command_pipeline = include_str!("commands.rs");
    let direct_commands = include_str!("commands/jury.rs");
    let jury_router = include_str!("jury/mod.rs");
    for source in [command_pipeline, direct_commands, jury_router] {
        assert!(!source.contains("write_machine_jury_verdict"), "no registered jury route may bypass the v60 guard");
    }
    assert!(
        command_pipeline.matches(".write_segment_verdict(").count() >= 8,
        "every T1/T2 pipeline branch must terminate at the shared v60 refusal"
    );
    assert_eq!(
        direct_commands.matches(".write_segment_verdict(").count(),
        1,
        "the direct T2 endpoint must terminate at the shared v60 refusal"
    );
    assert_eq!(
        jury_router.matches("db.write_segment_verdict(").count(),
        1,
        "the T0 jury router must terminate at the shared v60 refusal"
    );

    let db = make_db();
    db.insert_segment(&make_segment("machine-verdict-disabled", "/a.wav")).unwrap();

    let snapshot = || {
        db.connection()
            .query_row(
                "SELECT review_revision, verdict, verdict_transcript, jury_transcript,
                        rationale, evidence_json, agreement_score, escalated,
                        (SELECT COUNT(*) FROM decision_verdicts WHERE segment_id = speech_segments.id)
                   FROM speech_segments
                  WHERE id = 'machine-verdict-disabled'",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<f64>>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                },
            )
            .unwrap()
    };
    let before = snapshot();

    // T1/T2 and the pipeline's direct jury branches all terminate at this database guard.
    let direct_error = db
        .write_segment_verdict(
            "machine-verdict-disabled",
            "jury_accept",
            Some("machine candidate"),
            Some("machine consensus"),
            Some(r#"{"source":"jury"}"#),
            Some(0.9),
            false,
        )
        .unwrap_err();
    assert!(direct_error.to_string().contains("disabled at schema v60"), "{direct_error}");
    assert_eq!(snapshot(), before, "the shared T1/T2 database boundary must not mutate state or metrics");

    // T0 uses the public jury helper, which must reach the same refusal before pseudo-learning.
    let t0_error = crate::jury::write_verdict(
        &db,
        "machine-verdict-disabled",
        crate::jury::Verdict::AutoAccept,
        Some("machine candidate"),
        Some("machine consensus"),
        Some(r#"{"source":"t0"}"#),
        Some(0.9),
    )
    .unwrap_err();
    assert!(t0_error.to_string().contains("disabled at schema v60"), "{t0_error}");
    assert_eq!(snapshot(), before, "T0 refusal must leave the segment and decision metrics byte-for-byte unchanged");
}

#[test]
fn schema60_generic_insert_boundaries_reject_review_truth_atomically_and_preserve_existing_authority() {
    let db = make_db();

    let mut forged_single = make_segment("forged-single", "/forged-single.wav");
    forged_single.annotated_transcript = Some("renderer answer".into());
    forged_single.verified = true;
    let error = db.insert_segment(&forged_single).unwrap_err();
    assert!(error.to_string().contains("review-owned field"), "{error}");
    assert!(db.get_segment_by_id(&forged_single.id).unwrap().is_none());

    let neutral_batch = make_segment("neutral-batch", "/neutral-batch.wav");
    let mut forged_batch = make_segment("forged-batch", "/forged-batch.wav");
    forged_batch.human_decision = Some("edit".into());
    forged_batch.reviewed_by = Some("renderer".into());
    let error = db.insert_segments_batch(&[neutral_batch.clone(), forged_batch]).unwrap_err();
    assert!(error.to_string().contains("review-owned field"), "{error}");
    assert!(
        db.get_segment_by_id(&neutral_batch.id).unwrap().is_none(),
        "prevalidation must reject the complete batch before its first insert"
    );

    let mut forged_full = make_segment("forged-full", "/forged-full.wav");
    forged_full.verdict = Some("human_accept".into());
    forged_full.verdict_transcript = Some("renderer answer".into());
    let error = db.insert_segment_full(&forged_full).unwrap_err();
    assert!(error.to_string().contains("review-owned field"), "{error}");
    assert!(db.get_segment_by_id(&forged_full.id).unwrap().is_none());

    let mut existing = make_segment("machine-upsert", "/machine-upsert.wav");
    existing.confidence = Some(0.4);
    db.insert_segment(&existing).unwrap();
    db.finalize_human_review("machine-upsert", "accept", Some("test"), Some(123), None).unwrap();
    let reviewed = db.get_segment_by_id("machine-upsert").unwrap().unwrap();

    let mut machine_refresh = make_segment("machine-upsert", "/machine-upsert.wav");
    machine_refresh.raw_transcript = "new machine draft".into();
    machine_refresh.speaker_id = Some("speaker-2".into());
    machine_refresh.confidence = Some(0.95);
    db.insert_segment_full(&machine_refresh).unwrap();
    let after = db.get_segment_by_id("machine-upsert").unwrap().unwrap();
    assert!(review_owned_projection_matches(&reviewed, &after));
    assert_eq!(after.raw_transcript, "new machine draft");
    assert_eq!(after.speaker_id.as_deref(), Some("speaker-2"));
    assert_eq!(after.confidence, Some(0.95));
}

#[test]
fn disk_full_rolls_back_a_batch_insert_atomically() {
    // Week-2 disk-full FAULT DRILL. `PRAGMA max_page_count` caps the DB file size; exceeding it
    // returns SQLITE_FULL — the exact error a full disk raises — so it's a PORTABLE, deterministic
    // disk-full injection (no VFS shim, no real full disk). The property under test: a batch insert
    // that hits SQLITE_FULL MID-BATCH must roll back the WHOLE batch (SAVEPOINT batch_insert), never
    // leave a torn partial batch, keep prior committed rows, and keep the DB consistent.
    let db = make_db();
    db.insert_segment(&make_segment("base-1", "/a.wav")).unwrap();

    // Cap at the current size + a tiny headroom so a large batch blows past it after a few rows.
    let cur_pages: i64 = db.connection().query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    db.connection().execute_batch(&format!("PRAGMA max_page_count = {}", cur_pages + 4)).unwrap();

    // 500 fat rows (each ~2 KB) — far more than 4 pages of headroom, so the insert (incl. its FTS
    // trigger writes) hits SQLITE_FULL partway through the batch.
    let big: Vec<SpeechSegment> = (0..500)
        .map(|i| {
            let mut s = make_segment(&format!("full-{i}"), "/a.wav");
            s.raw_transcript = "پڕ".repeat(1000);
            s
        })
        .collect();
    let err = db.insert_segments_batch(&big).unwrap_err();
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("full") || matches!(err, AppError::Database(_)),
        "a disk-full mid-batch must surface as an error, not silent success: {err}"
    );

    // Lift the cap (0 = unlimited) and verify atomic rollback + consistency.
    db.connection().execute_batch("PRAGMA max_page_count = 0").unwrap();
    let leaked: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM speech_segments WHERE id LIKE 'full-%'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(leaked, 0, "SQLITE_FULL mid-batch must roll back the ENTIRE batch — no torn partial insert");
    assert!(db.get_segment_by_id("base-1").unwrap().is_some(), "the pre-batch committed row survives disk-full");
    assert_eq!(db.integrity_check().unwrap(), "ok", "DB stays consistent after a disk-full rollback");
    // The connection is usable again once space is available.
    db.insert_segment(&make_segment("after-1", "/a.wav")).unwrap();
    assert!(db.get_segment_by_id("after-1").unwrap().is_some(), "writes resume after the disk frees up");
}

#[test]
fn redecision_undo_restores_the_database_owned_prior_decision_exactly() {
    let db = make_db();
    let mut owner = make_segment("s1", "/a.wav");
    owner.annotated_transcript = Some("owner gold کە کە".into());
    owner.verified = true;
    db.insert_legacy_segment_fixture(&owner).unwrap();
    ensure_test_audio_content_hash(&db, "s1");
    db.record_human_decision("s1", "edit", Some("owner gold کە کە"), None).unwrap();
    let prior = db.get_segment_by_id("s1").unwrap().unwrap();
    db.record_human_decision("s1", "edit", Some("gemini text خۆ"), None).unwrap();
    let second_effect = latest_human_effect_id(&db, "s1");
    let outcome = db.undo_human_decision(second_effect, None, "00000000-0000-4000-8000-000000000201").unwrap();
    assert!(matches!(outcome, HumanDecisionUndoOutcome::Applied { .. }));
    let restored = db.get_segment_by_id("s1").unwrap().unwrap();
    assert_eq!(restored.human_decision, prior.human_decision);
    assert_eq!(restored.verdict, prior.verdict);
    assert_eq!(restored.verdict_transcript, prior.verdict_transcript);
    assert_eq!(restored.annotated_transcript, prior.annotated_transcript);
    assert_eq!(restored.verified, prior.verified);
    assert!(db.clear_human_decision("s1").is_err(), "snapshot-free legacy clear remains disabled");
}

#[test]
fn intelligence_report_joins_shadow_and_verdicts_against_human_decisions() {
    // True-10 audit: the C5/C4 read side. Over-trigger = would-fire + human accepted the
    // ORIGINAL text unchanged (the memory would have corrupted a correct transcript). C4
    // precision = of T0 auto-accepts a human later reviewed, confirmed vs contradicted.
    let db = make_db();
    for id in ["ot", "edited", "unreviewed", "t0-ok", "t0-bad", "t1"] {
        db.insert_segment(&make_segment(id, "/audio/i.wav")).unwrap();
    }
    // LOOP-0 shadow: 'ot' would fire but the human accepted the original -> OVER-TRIGGER.
    db.record_loop0_shadow("ot", true).unwrap();
    db.record_loop0_shadow("edited", true).unwrap();
    db.record_loop0_shadow("unreviewed", true).unwrap();
    db.connection().execute("UPDATE speech_segments SET human_decision='accept' WHERE id='ot'", []).unwrap();
    db.connection().execute("UPDATE speech_segments SET human_decision='edit' WHERE id='edited'", []).unwrap();
    // C4: two T0 accepts (one confirmed, one contradicted) + one T1 escalation.
    db.record_decision_verdict("t0-ok", "auto_accept", false).unwrap();
    db.record_decision_verdict("t0-bad", "jury_accept", false).unwrap();
    db.record_decision_verdict("t1", "escalated", true).unwrap();
    db.connection().execute("UPDATE speech_segments SET human_decision='accept' WHERE id='t0-ok'", []).unwrap();
    db.connection().execute("UPDATE speech_segments SET human_decision='reject' WHERE id='t0-bad'", []).unwrap();

    let report = db.intelligence_report().unwrap();
    let loop0 = &report["loop0Shadow"];
    assert_eq!(loop0["totalObservations"], 3);
    assert_eq!(loop0["wouldFire"], 3);
    assert_eq!(loop0["firedButHumanAcceptedOriginal"], 1, "'ot' is the one over-trigger");
    assert_eq!(loop0["firedAndHumanEdited"], 1);
    assert_eq!(loop0["firedAndHumanRejected"], 0);
    let c4 = &report["autoAcceptPrecision"];
    assert_eq!(c4["t0Accepts"], 2);
    assert_eq!(c4["t1Escalations"], 1);
    assert_eq!(c4["t0HumanConfirmed"], 1);
    assert_eq!(c4["t0HumanContradicted"], 1);
}

#[test]
fn write_segment_verdict_is_atomic_with_its_decision_log() {
    // Pre-v60 compatibility audit: the legacy verdict UPDATE and decision_verdicts INSERT are one
    // invariant. Fault-inject the second statement by dropping its table: the whole write must
    // FAIL and the verdict UPDATE must ROLL BACK — never a verdict without its C4 denominator row.
    let db = make_db();
    db.insert_segment(&make_segment("atom", "/audio/s.wav")).unwrap();
    db.conn.execute("DELETE FROM schema_migrations WHERE version = 60", []).unwrap();
    db.conn.execute_batch("DROP TABLE decision_verdicts").unwrap();

    let result = db.write_segment_verdict("atom", "escalated", None, None, None, Some(0.4), true);
    assert!(result.is_err(), "a failed decision-log insert must fail the whole verdict write");

    let seg = db.get_segment_by_id("atom").unwrap().expect("segment still present");
    assert_eq!(seg.verdict, None, "the verdict UPDATE must roll back with the failed decision log");
    assert!(!seg.escalated, "escalated must roll back too");
}

#[test]
fn suspect_first_ranks_escalated_by_real_confidence_not_recency() {
    // True-10 audit: escalated rows used to carry agreement_score=None, collapsing the second
    // sort key (COALESCE(agreement_score, 0.5) ASC) to a constant — "suspect-first" silently
    // degraded to recency. With the jury now persisting the IRT confidence on escalation, the
    // most-doubted clip genuinely ranks first; a legacy None row slots at the 0.5 midpoint.
    let db = make_db();
    for id in ["confident", "shaky", "legacy"] {
        db.insert_segment(&make_segment(id, "/audio/s.wav")).unwrap();
    }
    db.write_legacy_machine_verdict_for_test("confident", "escalated", None, None, None, Some(0.9), true).unwrap();
    db.write_legacy_machine_verdict_for_test("shaky", "escalated", None, None, None, Some(0.2), true).unwrap();
    db.write_legacy_machine_verdict_for_test("legacy", "escalated", None, None, None, None, true).unwrap();

    let ordered: Vec<String> = db.get_segments_suspect_first(None).unwrap().into_iter().map(|s| s.id).collect();
    assert_eq!(
        ordered,
        vec!["shaky".to_string(), "legacy".to_string(), "confident".to_string()],
        "lowest jury confidence first; a legacy None row sits at the 0.5 midpoint"
    );
}

#[test]
fn segment_ids_for_audio_path_returns_only_that_files_segments() {
    // P3.2 resume fix: on import-resume, already-imported files are folded back into the jury
    // batch by their segment ids. This pins that the lookup returns exactly (and only) the
    // segments of the requested source file, so a resumed import re-adjudicates the right set.
    let db = make_db();
    db.insert_segment(&make_segment("a1", "/audio/one.wav")).unwrap();
    db.insert_segment(&make_segment("a2", "/audio/one.wav")).unwrap();
    db.insert_segment(&make_segment("b1", "/audio/two.wav")).unwrap();

    let ids = db.segment_ids_for_audio_path("/audio/one.wav").unwrap();
    assert_eq!(ids, vec!["a1".to_string(), "a2".to_string()], "only one.wav's segments, in insert order");
    assert_eq!(db.segment_ids_for_audio_path("/audio/two.wav").unwrap(), vec!["b1".to_string()]);
    assert!(db.segment_ids_for_audio_path("/audio/missing.wav").unwrap().is_empty(), "unknown path -> empty");
}

#[test]
fn model_abilities_round_trip_and_upsert() {
    let db = make_db();
    assert!(db.load_model_abilities().unwrap().is_empty(), "no abilities before any learning run");
    let mut a = std::collections::HashMap::new();
    a.insert("omniasr-wsl-7b".to_string(), 1.5);
    a.insert("omniasr-ctc-300m".to_string(), -0.5);
    a.insert("nan-model".to_string(), f64::NAN); // must be dropped (non-finite)
    db.save_model_abilities(&a).unwrap();
    let loaded = db.load_model_abilities().unwrap();
    assert_eq!(loaded.len(), 2, "the NaN ability must not be stored");
    assert!((loaded["omniasr-wsl-7b"] - 1.5).abs() < 1e-9);
    // Upsert overwrites the prior value for the same model.
    db.save_model_abilities(&std::collections::HashMap::from([("omniasr-wsl-7b".to_string(), 2.2)])).unwrap();
    assert!((db.load_model_abilities().unwrap()["omniasr-wsl-7b"] - 2.2).abs() < 1e-9);
}

// The jury db-lock fix (with_jury_db) opens a SECOND, dedicated connection from Database::path so
// the global AppState db Mutex isn't held across cloud T2 network calls. This pins the assumption
// it relies on: path() round-trips, and a dedicated connection to the same file sees the primary's
// committed rows AND its own writes are visible back to the primary (SQLite WAL coexistence).
#[test]
fn dedicated_connection_via_path_shares_committed_data() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("jury.db");
    let path_str = path.to_string_lossy().to_string();

    let primary = Database::open(&path_str).unwrap();
    primary.initialize().unwrap();
    assert_eq!(primary.path(), path_str, "path() must return the opened path");
    primary.insert_segment(&make_segment("s1", "/s1.wav")).unwrap();

    // A dedicated connection opened from primary.path() (the with_jury_db pattern) sees the row.
    let dedicated = Database::open(primary.path()).unwrap();
    assert!(
        dedicated.get_segment_by_id("s1").unwrap().is_some(),
        "dedicated connection must see rows committed by the primary connection"
    );

    // A targeted write through the dedicated connection persists to the same file and is visible
    // back to the primary. Machine jury verdicts are intentionally disabled at schema v60+.
    assert!(dedicated.update_normalized_transcript("s1", "hi").unwrap());
    let seen = primary.get_segment_by_id("s1").unwrap().unwrap();
    assert_eq!(seen.normalized_transcript.as_deref(), Some("hi"));
}

#[test]
fn spot_check_candidates_respect_their_limit_and_need_a_wrong_draft() {
    // Migration v44. Two properties:
    //
    // (a) `limit` is honoured EXACTLY, including zero. The check used to sit after the push, so
    //     asking for none returned one — which silently hands a spot check to a caller that decided
    //     it did not want any. Found by a fail-before revert that failed to fail.
    //
    // (b) a candidate needs a human answer AND a raw draft that DIFFERS from it. A clip whose draft
    //     is already correct cannot tell a reviewer who listened from one who tapped accept — both
    //     hand back the same text — so including it would quietly dilute every score toward "fine".
    let db = make_db();
    let audio_dir = tempfile::tempdir().unwrap();
    let audio_path = |id: &str| {
        let path = audio_dir.path().join(format!("{id}.wav"));
        std::fs::write(&path, b"playable fixture").unwrap();
        path.to_string_lossy().into_owned()
    };
    let plant = |id: &str, raw: &str, answer: Option<&str>| {
        let mut s = make_segment(id, &audio_path(id));
        s.raw_transcript = raw.to_string();
        db.insert_segment(&s).unwrap();
        ensure_test_audio_content_hash(&db, id);
        if let Some(a) = answer {
            db.finalize_human_review(id, "edit", Some(a), None, None).unwrap();
        }
    };
    plant("sc-wrong-1", "دەقی هەڵە", Some("دەقی ڕاست"));
    plant("sc-wrong-2", "هەڵەی دوو", Some("ڕاستی دوو"));
    plant("sc-already-right", "دەقی ڕاست", Some("دەقی ڕاست")); // draft == answer: no trap
    plant("sc-no-answer", "دەقی بێ وەڵام", None); // machine-only: not an answer key

    // A PHONE reviewer's fresh correction. It must never become an answer key, or the next reviewer
    // is graded against a peer's guess and marked wrong for disagreeing with it.
    //
    // `reviewed_by` is what identifies it, and setting it here is the whole point: this fixture used
    // to express "peer" as `is_gold = false`, which nothing in the app ever sets to true — so the
    // test passed against a query that could never match ANYTHING in production. It now carries the
    // column production actually writes (`record_human_decision_by` sets it unconditionally to the
    // deciding reviewer's name).
    {
        let mut peer = make_segment("sc-peer-edit", &audio_path("peer"));
        peer.raw_transcript = "دەقی هەڵە".into();
        db.insert_segment(&peer).unwrap();
        record_test_phone_decision(&db, "sc-peer-edit", "edit", Some("وەڵامی هاوکار"), "Hemn");
    }
    // The OWNER's own desktop verification: not flagged gold either, but `reviewed_by` is NULL
    // because the desktop path passes no annotator. This is the case that makes the mechanism
    // reachable at all — without it the candidate set is empty in every real installation.
    {
        let mut owner = make_segment("sc-owner-edit", &audio_path("owner"));
        owner.raw_transcript = "دەقی هەڵەی سێ".into();
        db.insert_segment(&owner).unwrap();
        ensure_test_audio_content_hash(&db, "sc-owner-edit");
        db.finalize_human_review("sc-owner-edit", "edit", Some("ڕاستی سێ"), None, None).unwrap();
    }

    let ids = |limit: usize| -> Vec<String> {
        db.list_spot_check_candidates(limit, "Sara", &std::collections::HashSet::new(), None, None)
            .unwrap()
            .into_iter()
            .map(|(s, _)| s.id)
            .collect()
    };
    assert!(ids(0).is_empty(), "a limit of zero must return NOTHING, not one");
    assert_eq!(ids(1).len(), 1, "a limit of one returns exactly one");
    let all = ids(10);
    assert_eq!(all.len(), 3, "the two gold traps plus the owner-verified one qualify, got {all:?}");
    assert!(!all.contains(&"sc-already-right".to_string()), "a correct draft catches nobody");
    assert!(!all.contains(&"sc-no-answer".to_string()), "a clip with no human answer is not an answer key");
    assert!(
        !all.contains(&"sc-peer-edit".to_string()),
        "a peer's fresh correction must never be used to grade another reviewer"
    );
    assert!(
        all.contains(&"sc-owner-edit".to_string()),
        "the owner's own verified answer IS an answer key — without this the mechanism never fires"
    );

    // A hidden check must be BLIND. Historical review_events survive later desktop corrections and
    // migrations that clear reviewed_by, so reviewed_by=NULL alone cannot prove this reviewer has
    // never heard the clip. Re-testing a clip Sara already reviewed measures memory/disagreement,
    // not whether she listened to a genuinely unseen check.
    db.connection()
        .execute(
            "INSERT INTO review_events
                 (segment_id, reviewer, action, source, timestamp_ms)
             VALUES ('sc-owner-edit', 'sArA', 'accept', 'legacy', 1)",
            [],
        )
        .unwrap();
    let unseen_after_history = ids(10);
    assert!(
        !unseen_after_history.contains(&"sc-owner-edit".to_string()),
        "a clip previously reviewed by the same person is not a blind quality check"
    );
    let unseen_for_other: Vec<String> = db
        .list_spot_check_candidates(10, "Hemn", &std::collections::HashSet::new(), None, None)
        .unwrap()
        .into_iter()
        .map(|(s, _)| s.id)
        .collect();
    assert!(
        unseen_for_other.contains(&"sc-owner-edit".to_string()),
        "one person's prior exposure must not consume another person's blind key"
    );

    // Voice focus applies to the whole paid queue, including hidden checks. Filter BEFORE `limit`:
    // the focused key sorts after other candidates, so filtering a pre-limited result would return
    // zero and silently turn QC off for the batch.
    let focus = std::collections::HashSet::from(["sc-wrong-2".to_string()]);
    let focused: Vec<String> = db
        .list_spot_check_candidates(1, "Sara", &std::collections::HashSet::new(), None, Some(&focus))
        .unwrap()
        .into_iter()
        .map(|(s, _)| s.id)
        .collect();
    assert_eq!(focused, vec!["sc-wrong-2".to_string()]);

    // The expected text is the HUMAN answer, never the raw draft — grading against the draft would
    // score a blind accept as perfect. Asserted against the row that came back rather than a
    // hardcoded string: the answer key must be right for EVERY candidate, not just whichever one
    // happens to sort first.
    for (seg, expected) in
        db.list_spot_check_candidates(10, "Sara", &std::collections::HashSet::new(), None, None).unwrap()
    {
        assert_ne!(expected, seg.raw_transcript, "{} was graded against its own draft", seg.id);
        assert_eq!(
            Some(expected.as_str()),
            seg.verdict_transcript.as_deref(),
            "{} must be graded against its human verdict",
            seg.id
        );
    }

    // A TRAP ALREADY ANSWERED MUST NOT COME BACK to the same reviewer. Selection is deterministic
    // (id ASC), so without this every batch drew the identical first-N: after one batch the reviewer
    // answers from memory rather than by listening, and because record_spot_check upserts on
    // (segment_id, reviewer) the memorised attempt OVERWRITES the one honest measurement. The score
    // then drifts upward the longer someone works, which is worse than no score at all.
    db.record_spot_check("sc-wrong-1", "Sara", "edit", "دەقی ڕاست", "دەقی ڕاست").unwrap();
    let after = ids(10);
    assert!(!after.contains(&"sc-wrong-1".to_string()), "Sara must not be re-tested on a trap she has answered");
    assert!(after.contains(&"sc-wrong-2".to_string()), "her remaining traps are still available");

    // Per REVIEWER, not global: two people meeting the same clip independently is the entire basis of
    // the agreement sample, so Sara's answer must not consume Hemn's.
    let hemn: Vec<String> = db
        .list_spot_check_candidates(10, "Hemn", &std::collections::HashSet::new(), None, None)
        .unwrap()
        .into_iter()
        .map(|(s, _)| s.id)
        .collect();
    assert!(hemn.contains(&"sc-wrong-1".to_string()), "one reviewer's answer must not exhaust another's pool");
}

#[test]
fn the_agreement_sample_pairs_two_raters_and_never_hides_a_third() {
    // P2.4. Inter-annotator agreement needs the SAME clip answered by two people independently — and
    // spot checks already provide exactly that, because they are deliberately not leased. So the
    // overlap is a side effect that already exists; what was missing was only the export.
    //
    // The output feeds scripts/agreement_kappa.py, which is already unit-tested against the textbook
    // kappa=0.40 example. No kappa is computed here: a second implementation would be an unverified
    // copy of a verified one.
    let db = make_db();
    for id in ["a1", "a2", "a3", "solo"] {
        db.insert_segment(&make_segment(id, &format!("/{id}.wav"))).unwrap();
        ensure_test_audio_content_hash(&db, id);
    }
    assert!(db.agreement_sample().unwrap().is_none(), "no double-review yet means NO sample, not an empty one");

    // Sara and Hemn both answer a1..a3; they agree on two and differ on one.
    for (id, sara, hemn) in [("a1", "accept", "accept"), ("a2", "edit", "edit"), ("a3", "accept", "reject")] {
        db.record_spot_check(id, "Sara", sara, "x", "x").unwrap();
        db.record_spot_check(id, "Hemn", hemn, "y", "x").unwrap();
    }
    // Only Sara saw this one, so it cannot appear in a PAIRED sample.
    db.record_spot_check("solo", "Sara", "accept", "x", "x").unwrap();

    let s = db.agreement_sample().unwrap().expect("two raters overlap");
    assert_eq!(s.items, 3, "exactly the three clips BOTH answered");
    assert!(s.other_reviewers.is_empty());
    let lines: Vec<&str> = s.tsv.lines().collect();
    assert_eq!(lines[0], format!("{}\t{}", s.rater_a, s.rater_b), "header names the two raters");
    assert_eq!(lines.len(), 4, "header + one row per shared clip, and NOT the unpaired one");
    assert!(lines[1..].iter().all(|l| l.split('\t').count() == 2), "two label columns, as the script expects");

    // A THIRD reviewer must never be silently folded in: Cohen's kappa takes exactly two, and quietly
    // averaging three raters into one number is precisely the fabrication the honesty law forbids.
    for id in ["a1", "a2"] {
        db.record_spot_check(id, "Ali", "reject", "z", "x").unwrap();
    }
    let s = db.agreement_sample().unwrap().expect("still a pair");
    assert_eq!(s.items, 3, "Sara/Hemn share the most clips, so they stay the reported pair");
    assert_eq!(s.other_reviewers, vec!["Ali".to_string()], "and the excluded rater is NAMED, not dropped");

    // Determinism: the same data must yield the same pair and the same bytes, or two kappa numbers
    // computed a day apart would not be comparable and nobody would know why.
    assert_eq!(db.agreement_sample().unwrap().unwrap().tsv, s.tsv);
}

#[test]
fn a_named_reviewer_is_recorded_only_at_the_attributed_phone_boundary() {
    // Migration v43. Multi-reviewer Couch Review means several named people decide clips at once, so a
    // decision that does not say WHO made it is unattributable — an audit gap, and the missing substrate
    // for any later inter-annotator agreement study. Four properties, each of which broke a real design:
    //
    //   1. an attributed decision stores the name,
    //   2. an UNattributed one (the owner's desktop, one human, no token naming them) stores SQL NULL
    //      rather than a fabricated "owner" — a provenance column that invents values is worse than none,
    //   3. re-deciding REPLACES the name (a stale reviewer must never be left credited for someone
    //      else's verdict — this is why the UPDATE sets reviewed_by unconditionally, not via COALESCE),
    //   4. undo clears it, because the attribution belongs to the decision being retracted.
    let db = make_db();
    for id in ["att-phone", "att-desktop", "att-redecide"] {
        db.insert_segment(&make_segment(id, &format!("/{id}.wav"))).unwrap();
    }
    let reviewed_by = |id: &str| db.get_segment_by_id(id).unwrap().unwrap().reviewed_by;

    let unsupported = db.record_human_decision_by("att-phone", "accept", None, None, Some("Sara")).unwrap_err();
    assert!(
        unsupported.to_string().contains("anonymous desktop decision boundary"),
        "a named desktop write must fail with an actionable boundary error: {unsupported}"
    );
    assert_eq!(db.get_segment_by_id("att-phone").unwrap().unwrap().human_decision, None);
    assert_eq!(
        db.connection()
            .query_row::<i64, _, _>(
                "SELECT COUNT(*) FROM human_decision_effect_events WHERE segment_id='att-phone'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        0,
        "the rejected named-desktop call must publish no partial effect"
    );
    record_test_phone_decision(&db, "att-phone", "accept", None, "Sara");
    assert_eq!(reviewed_by("att-phone").as_deref(), Some("Sara"), "an attributed decision names its reviewer");

    db.record_human_decision("att-desktop", "accept", None, None).unwrap();
    assert_eq!(reviewed_by("att-desktop"), None, "an unattributed decision stores NULL, never a made-up name");

    record_test_phone_decision(&db, "att-redecide", "accept", None, "Sara");
    record_test_phone_decision(&db, "att-redecide", "edit", Some("ڕاستکراوە"), "Hemn");
    assert_eq!(reviewed_by("att-redecide").as_deref(), Some("Hemn"), "the CURRENT decision's author wins");
    db.record_human_decision("att-redecide", "accept", None, None).unwrap();
    assert_eq!(reviewed_by("att-redecide"), None, "a desktop re-review clears the previous reviewer's name");

    assert!(db.clear_human_decision("att-phone").is_err(), "legacy snapshot-free clear must stay disabled");
    assert_eq!(reviewed_by("att-phone").as_deref(), Some("Sara"), "a refused unsafe clear must not erase attribution");
}

#[test]
fn reviewed_rows_refuse_whole_row_and_asr_upserts_and_preserve_attribution() {
    // WHOLE-ROW CLOBBER — the recurring defect family in this file. `insert_segment_full` rewrites EVERY
    // column from a snapshot, and the couch's own undo path uses it. A `reviewed_by` missing from that
    // statement's column list would silently revert to NULL on any restore, stripping the attribution off
    // rows that still carry the decision. `insert_segment`'s 17-column subset deliberately OMITS it, the
    // same way it omits human_decision, so an ASR-only re-write must LEAVE it intact, not clear it.
    let db = make_db();
    db.insert_segment(&make_segment("rt-1", "/rt-1.wav")).unwrap();
    record_test_phone_decision(&db, "rt-1", "accept", None, "Sara");

    // Paid reviewed rows are no longer restored by renderer-owned whole-row snapshots. Even an
    // apparently identical UPSERT names immutable audio fields in its UPDATE clause, so it must be
    // refused rather than reopening a clobber path around exact effect-based Undo.
    let snapshot = db.get_segment_by_id("rt-1").unwrap().unwrap();
    assert_eq!(snapshot.reviewed_by.as_deref(), Some("Sara"));
    let error = db.insert_segment_full(&snapshot).unwrap_err();
    assert!(
        error.to_string().contains("review-owned field")
            || error.to_string().contains("paid policy-4 source identity is immutable"),
        "{error}"
    );
    assert_eq!(
        db.get_segment_by_id("rt-1").unwrap().unwrap().reviewed_by.as_deref(),
        Some("Sara"),
        "insert_segment_full must persist reviewed_by — dropping it is the whole-row-clobber bug"
    );

    // The ASR upsert also names paid identity fields. A corrected/redecoded clip is a new segment
    // version; it cannot overwrite the immutable reviewed source in place.
    let mut asr_only = make_segment("rt-1", "/rt-1.wav");
    asr_only.raw_transcript = "re-decoded".to_string();
    let asr_error = db.insert_segment(&asr_only).unwrap_err();
    assert!(asr_error.to_string().contains("paid policy-4 source identity is immutable"), "{asr_error}");
    assert_eq!(
        db.get_segment_by_id("rt-1").unwrap().unwrap().reviewed_by.as_deref(),
        Some("Sara"),
        "a refused ASR upsert must leave the human attribution intact"
    );
    assert_ne!(db.get_segment_by_id("rt-1").unwrap().unwrap().raw_transcript, "re-decoded");
}

#[test]
fn historical_decision_verdict_fixture_records_all_machine_classes() {
    // Frozen pre-v60 C4 history must remain readable with the same classification vocabulary even
    // though schema-v60 production jury writers are disabled.
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    for id in ["aa", "je", "es", "hv"] {
        db.insert_segment(&make_segment(id, &format!("/{id}.wav"))).unwrap();
    }

    db.record_decision_verdict("aa", "auto_accept", false).unwrap();
    db.record_decision_verdict("je", "jury_edit", false).unwrap();
    db.record_decision_verdict("es", "escalated", true).unwrap();
    db.record_decision_verdict("hv", "human_accept", false).unwrap();

    let verdict_of = |id: &str| -> Option<String> {
        db.connection()
            .query_row("SELECT auto_accept_verdict FROM decision_verdicts WHERE segment_id = ?1", [id], |r| r.get(0))
            .ok()
    };
    assert_eq!(verdict_of("aa").as_deref(), Some("T0_ACCEPT"), "auto_accept is a T0 auto-resolution");
    assert_eq!(verdict_of("je").as_deref(), Some("T0_ACCEPT"), "jury_edit is a T0 auto-resolution");
    assert_eq!(verdict_of("es").as_deref(), Some("T1_ESCALATE"), "escalated is T1");
    assert_eq!(verdict_of("hv"), None, "human-decided segment gets no machine verdict row");
}

#[test]
fn v35_repairs_divergent_segments_fts_so_segment_writes_succeed() {
    // Regression (real-app import failure, 2026-07-10): a 4-column segments_fts (missing audio_path)
    // left by a mis-ordered init, while the segments_ai/ad/au triggers reference audio_path, makes
    // EVERY segment INSERT fail ("table segments_fts has no column named audio_path"). The import
    // transaction then rolls back and VAD "produces 0 segments" — the app cannot ingest any audio.
    // v35 rebuilds the FTS shadow table to the authoritative 6-column shape.
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();

    // Reproduce the broken divergent state: drop the correct FTS table and recreate migration v1's
    // 4-column copy under the (audio_path-referencing) triggers that initialize() created.
    db.connection()
        .execute_batch(
            "DROP TABLE IF EXISTS segments_fts;
                 CREATE VIRTUAL TABLE segments_fts USING fts5(
                     id, raw_transcript, normalized_transcript, annotated_transcript,
                     content='speech_segments', content_rowid='rowid');",
        )
        .unwrap();
    assert!(
        db.insert_segment(&make_segment("broken", "/a.wav")).is_err(),
        "a 4-column segments_fts under audio_path triggers must reject segment inserts (the real bug)"
    );

    // Exercise v35's exact repair SQL, not a synthetic half-rewind of every later migration. Deleting
    // only history rows from a HEAD database leaves later schema (for example v57's compensation
    // columns) in place and is not a state a real upgrade can produce; replaying v35..HEAD over that
    // hybrid correctly fails on duplicate later DDL and obscures this regression's actual contract.
    let repair = crate::migrations::MIGRATIONS.iter().find(|migration| migration.version == 35).unwrap();
    db.connection().execute_batch(repair.up_sql).unwrap();
    assert!(
        crate::migrations::run_migrations(&db).unwrap().is_empty(),
        "repairing FTS directly must not alter the already-complete migration history"
    );

    // After v35, the write the import path depends on succeeds again.
    db.insert_segment(&make_segment("fixed", "/b.wav"))
        .expect("segment INSERT must succeed after v35 rebuilds segments_fts with audio_path");
}

#[test]
fn search_does_not_match_the_audio_path_column() {
    // Round-23 #7: a token that appears ONLY in the file path must NOT return the segment — search
    // is over transcript content, not folder/file names.
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let mut path_only = make_segment("p1", "/recordings/kurdistan/interview.wav");
    path_only.raw_transcript = "hello world".to_string(); // "kurdistan" is ONLY in the path
    db.insert_segment(&path_only).unwrap();
    let mut text_hit = make_segment("t1", "/recordings/a/b.wav");
    text_hit.raw_transcript = "this is about kurdistan today".to_string();
    db.insert_segment(&text_hit).unwrap();

    let ids: Vec<String> = db.search_segments("kurdistan").unwrap().into_iter().map(|s| s.id).collect();
    assert_eq!(ids, vec!["t1".to_string()], "only the transcript match may return, not the path match: {ids:?}");
}

#[test]
fn transient_integrity_messages_are_not_classified_as_corruption() {
    // Round-20: a transient page-read message from PRAGMA integrity_check must NOT be treated as
    // corruption (which would quarantine a healthy db = silent data loss). It aborts startup instead.
    assert!(integrity_result_looks_transient("unable to get the page 42. error code=8"));
    assert!(integrity_result_looks_transient("disk I/O error"));
    assert!(integrity_result_looks_transient("database is locked"));
    assert!(integrity_result_looks_transient("out of memory"));
    // Genuine structural-corruption findings are NOT transient -> they still quarantine.
    assert!(!integrity_result_looks_transient("row 5 missing from index idx_foo"));
    assert!(!integrity_result_looks_transient("*** in database main *** Page 9: btreeInitPage() returns error"));
    assert!(!integrity_result_looks_transient("wrong # of entries in index"));
}

#[test]
fn stored_transcripts_are_nfc_canonicalized() {
    // Arabic "آ" (U+0622) can arrive decomposed as Alef (U+0627) + combining madda
    // (U+0653). Stored non-canonically it fragments FTS/dedup/WER. The write boundary
    // must store the composed NFC form regardless of the input form.
    let db = make_db();
    let decomposed = "\u{0627}\u{0653}\u{0628}"; // ا + ◌ٓ + ب  (NFD of "آب")
    let composed = "\u{0622}\u{0628}"; // آب (NFC)
    assert_ne!(decomposed, composed, "fixture must actually differ before NFC");

    let mut seg = make_segment("nfc1", "/a.wav");
    seg.raw_transcript = decomposed.to_string();
    seg.annotated_transcript = Some(decomposed.to_string());
    db.insert_legacy_segment_fixture(&seg).unwrap();

    let stored = db.get_segment_by_audio_path("/a.wav").unwrap().unwrap();
    assert_eq!(stored.raw_transcript, composed, "raw_transcript must be stored NFC-composed");
    assert_eq!(stored.annotated_transcript.as_deref(), Some(composed), "annotated must be NFC too");
}

#[test]
fn asr_and_consensus_updates_store_nfc_so_search_still_matches() {
    // The two UPDATE paths that feed the FTS-indexed raw_transcript (the WSL 7B refinement and the
    // machine-consensus batch) must NFC-canonicalize like the insert path, or a decomposed update
    // silently drops the segment out of search.
    let db = make_db();
    let decomposed = "\u{0627}\u{0653}\u{0628}"; // NFD of "آب"
    let composed = "\u{0622}\u{0628}"; // NFC

    // update_asr_transcript_if_unreviewed
    db.insert_segment(&make_segment("u1", "/u1.wav")).unwrap();
    assert!(db
        .update_asr_transcript_if_unreviewed(
            "u1",
            decomposed,
            Some(decomposed),
            Some(0.9),
            Some("heuristic"),
            Some("omniasr-wsl-7b"),
            false,
        )
        .unwrap());
    let s1 = db.get_segment_by_audio_path("/u1.wav").unwrap().unwrap();
    assert_eq!(s1.raw_transcript, composed, "ASR-update raw_transcript must be stored NFC");
    assert_eq!(s1.confidence_source.as_deref(), Some("heuristic"));
    assert_eq!(s1.model_version_id.as_deref(), Some("omniasr-wsl-7b"));
    assert!(!s1.cloud_call);
    assert!(db.search_segments(composed).unwrap().iter().any(|s| s.id == "u1"), "NFC query must find it");

    // update_segment_consensus_batch
    db.insert_segment(&make_segment("u2", "/u2.wav")).unwrap();
    let updates = vec![("u2".to_string(), decomposed.to_string(), decomposed.to_string(), 0.8)];
    assert_eq!(db.update_segment_consensus_batch(&updates).unwrap(), 1);
    let s2 = db.get_segment_by_audio_path("/u2.wav").unwrap().unwrap();
    assert_eq!(s2.raw_transcript, composed, "consensus-batch raw_transcript must be stored NFC");
    assert!(db.search_segments(composed).unwrap().iter().any(|s| s.id == "u2"), "NFC query must find it");
}

#[test]
fn wsl_refinement_must_not_overwrite_a_verified_transcript() {
    // A human who clicked "Verify"/"Verify selected" (batch_verify -> update_verified) sets ONLY
    // verified=1, leaving human_decision/verdict NULL. The background WSL-7B refinement loop
    // (update_asr_transcript_if_unreviewed) must SKIP such a row — otherwise it silently overwrites a
    // human-verified transcript with unapproved machine text while the row still exports as human-verified
    // GOLD. Sibling of consensus_batch_preserves_human_reviewed_transcripts; guards the verified=1 hole the
    // human_decision/verdict-only guard missed (found by adversarial hunt-4).
    let db = make_db();
    let mut seg = make_segment("ver-1", "/v.wav");
    seg.raw_transcript = "human re-transcribed then verified".to_string();
    seg.normalized_transcript = Some("human re-transcribed then verified".to_string());
    db.insert_segment(&seg).expect("insert");

    // Human verifies via the same single-column path batch_verify uses (verified only; decision/verdict NULL).
    assert!(db.update_verified_for_test("ver-1", true).unwrap());
    let locked = db.get_segment_by_id("ver-1").unwrap().unwrap();
    assert!(locked.verified);
    assert!(locked.human_decision.is_none(), "verify leaves human_decision NULL — the race precondition");

    // The WSL-7B loop reaches the same segment and tries to write fresh 7B text.
    let updated = db
        .update_asr_transcript_if_unreviewed(
            "ver-1",
            "unapproved 7b machine text",
            Some("unapproved 7b machine text"),
            Some(0.9),
            Some("external_provider"),
            Some("omniasr-wsl-7b"),
            false,
        )
        .unwrap();
    assert!(!updated, "a verified row must be SKIPPED, not overwritten");

    let after = db.get_segment_by_id("ver-1").unwrap().unwrap();
    assert_eq!(after.raw_transcript, "human re-transcribed then verified", "verified transcript must be intact");
    assert!(after.verified, "verified flag must stay set");

    // Sanity: an UNVERIFIED segment is still refined normally (the guard only protects verified/reviewed rows).
    db.insert_segment(&make_segment("unver-1", "/u.wav")).unwrap();
    assert!(
        db.update_asr_transcript_if_unreviewed(
            "unver-1",
            "7b text",
            None,
            Some(0.9),
            Some("external_provider"),
            Some("omniasr-wsl-7b"),
            false,
        )
        .unwrap(),
        "an unverified row must still be updated"
    );
}

#[test]
fn insert_hypothesis_stores_nfc_so_jury_agreement_is_not_normalization_fragile() {
    // The jury scores inter-engine agreement by exact surface word-equality. If two engines emit the
    // same Sorani word in different normalization forms (NFD vs NFC), a real consensus would be
    // mis-scored as a disagreement and spuriously escalated. insert_hypothesis must NFC-canonicalize
    // every vote (local 300M/1B/WSL-7B and cloud Scribe), exactly like the segment write paths.
    let db = make_db();
    let decomposed = "\u{0627}\u{0653}\u{0628}"; // ا + ◌ٓ + ب  (NFD of "آب")
    let composed = "\u{0622}\u{0628}"; // آب (NFC)
    assert_ne!(decomposed, composed, "fixture must actually differ before NFC");

    db.insert_segment(&make_segment("h1", "/h1.wav")).unwrap();
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: "h1".to_string(),
        model_id: "engine-nfd".to_string(),
        transcript: decomposed.to_string(),
        confidence: Some(0.9),
    })
    .unwrap();
    let hyps = db.get_hypotheses_for_segment("h1").unwrap();
    assert_eq!(hyps.len(), 1, "exactly one hypothesis stored");
    assert_eq!(hyps[0].transcript, composed, "hypothesis vote must be stored NFC-composed, not NFD");
}

#[test]
fn replacing_with_champion_removes_stale_votes_and_rolls_back_as_one_unit() {
    let db = make_db();
    db.insert_segment(&make_segment("champion-hyp", "/champion.wav")).unwrap();
    for (model_id, transcript) in
        [("omniasr-ctc-300m", "old 300m"), ("finetuned-mms-ckb", "old mms"), ("scribe-v1", "old cloud")]
    {
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: "champion-hyp".to_string(),
            model_id: model_id.to_string(),
            transcript: transcript.to_string(),
            confidence: Some(0.9),
        })
        .unwrap();
    }

    let champion = SegmentHypothesis {
        segment_id: "champion-hyp".to_string(),
        model_id: "omniasr-wsl-7b".to_string(),
        transcript: "champion only".to_string(),
        confidence: Some(0.98),
    };
    db.replace_hypotheses_with(&champion).unwrap();
    let stored = db.get_hypotheses_for_segment("champion-hyp").unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].model_id, "omniasr-wsl-7b");
    assert_eq!(stored[0].transcript, "champion only");

    // Force the INSERT half to fail after DELETE. The previous champion must survive, proving the
    // replacement is atomic and cannot erase provenance on a partial write.
    db.conn
        .execute_batch(
            "CREATE TRIGGER reject_blocked_hypothesis
             BEFORE INSERT ON segment_hypotheses
             WHEN NEW.model_id = 'blocked-model'
             BEGIN SELECT RAISE(ABORT, 'injected hypothesis failure'); END;",
        )
        .unwrap();
    let rejected = SegmentHypothesis { model_id: "blocked-model".to_string(), ..champion };
    assert!(db.replace_hypotheses_with(&rejected).is_err());
    let after_failure = db.get_hypotheses_for_segment("champion-hyp").unwrap();
    assert_eq!(after_failure.len(), 1);
    assert_eq!(after_failure[0].model_id, "omniasr-wsl-7b");
    assert_eq!(after_failure[0].transcript, "champion only");
}

#[test]
fn champion_commit_atomically_updates_transcript_provenance_and_sole_hypothesis() {
    let db = make_db();
    let mut segment = make_segment("champion-commit", "/champion-commit.wav");
    segment.raw_transcript = "old draft".to_string();
    segment.normalized_transcript = Some("old normalized".to_string());
    segment.confidence = Some(0.2);
    segment.confidence_source = Some("heuristic".to_string());
    segment.model_version_id = Some("omniasr-ctc-300m".to_string());
    db.insert_segment(&segment).unwrap();
    for model_id in ["omniasr-ctc-300m", "finetuned-mms-ckb", "scribe-v1"] {
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: segment.id.clone(),
            model_id: model_id.to_string(),
            transcript: format!("stale {model_id}"),
            confidence: Some(0.2),
        })
        .unwrap();
    }

    let decomposed = "\u{0627}\u{0653}\u{0628}";
    let composed = "\u{0622}\u{0628}";
    let champion = SegmentHypothesis {
        segment_id: segment.id.clone(),
        model_id: "omniasr-wsl-7b".to_string(),
        transcript: decomposed.to_string(),
        confidence: Some(0.98),
    };
    assert!(db
        .commit_champion_transcript_if_unreviewed(&champion, None, Some(decomposed), Some("external_provider"), true,)
        .unwrap());

    let stored = db.get_segment_by_id(&segment.id).unwrap().unwrap();
    assert_eq!(stored.raw_transcript, composed);
    assert_eq!(stored.normalized_transcript.as_deref(), Some(composed));
    assert_eq!(stored.confidence, Some(0.98));
    assert_eq!(stored.confidence_source.as_deref(), Some("external_provider"));
    assert_eq!(stored.model_version_id.as_deref(), Some("omniasr-wsl-7b"));
    assert!(stored.cloud_call, "cloud refinement provenance must commit with the transcript");

    let hypotheses = db.get_hypotheses_for_segment(&segment.id).unwrap();
    assert_eq!(hypotheses.len(), 1, "all stale optional-engine votes must be removed");
    assert_eq!(hypotheses[0].model_id, "omniasr-wsl-7b");
    assert_eq!(hypotheses[0].transcript, composed);
    assert_eq!(hypotheses[0].confidence, Some(0.98));
    let hypothesis_version: String = db
        .connection()
        .query_row("SELECT model_version_id FROM segment_hypotheses WHERE segment_id = ?1", [&segment.id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(hypothesis_version, "omniasr-wsl-7b");
}

#[test]
fn bound_champion_commit_refuses_cross_segment_and_source_drift_without_side_effects() {
    let db = make_db();
    let alignment_a = r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":1}"#;
    let alignment_b = r#"{"source_start_ms":1000,"source_end_ms":2000,"chunk_index":1,"chunk_count":2}"#;
    let mut segment_a = make_segment("bound-a", "/recording-a.wav");
    segment_a.raw_transcript = "incumbent a".into();
    segment_a.alignment_json = Some(alignment_a.into());
    let mut segment_b = make_segment("bound-b", "/recording-b.wav");
    segment_b.raw_transcript = "incumbent b".into();
    segment_b.alignment_json = Some(alignment_b.into());
    db.insert_segments_batch(&[segment_a.clone(), segment_b.clone()]).unwrap();
    let hash_a = "a".repeat(64);
    let hash_b = "b".repeat(64);
    db.connection()
        .execute(
            "UPDATE speech_segments SET audio_content_hash=CASE id WHEN 'bound-a' THEN ?1 ELSE ?2 END
             WHERE id IN ('bound-a','bound-b')",
            rusqlite::params![hash_a, hash_b],
        )
        .unwrap();
    for (id, transcript) in [("bound-a", "prior vote a"), ("bound-b", "prior vote b")] {
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: id.into(),
            model_id: "prior-model".into(),
            transcript: transcript.into(),
            confidence: Some(0.2),
        })
        .unwrap();
    }

    let snapshot_a = db.champion_transcription_source_snapshot("bound-a").unwrap().unwrap();
    let wrong_segment = SegmentHypothesis {
        segment_id: "bound-b".into(),
        model_id: "omniasr-wsl-7b".into(),
        transcript: "audio from a must never land on b".into(),
        confidence: Some(0.99),
    };
    let error = db
        .commit_bound_champion_transcript_if_unreviewed(
            &wrong_segment,
            None,
            None,
            Some("external_provider"),
            false,
            &snapshot_a,
        )
        .unwrap_err();
    assert!(error.to_string().contains("E_TRANSCRIPTION_SOURCE_MISMATCH"), "{error}");

    // Simulate an authoritative relink/source-epoch change while inference for A is in flight.
    db.connection()
        .execute(
            "UPDATE speech_segments
             SET audio_path='/replacement.wav', alignment_json=?2, audio_content_hash=?3
             WHERE id=?1",
            rusqlite::params!["bound-a", alignment_b, "c".repeat(64)],
        )
        .unwrap();
    let stale_a = SegmentHypothesis {
        segment_id: "bound-a".into(),
        model_id: "omniasr-wsl-7b".into(),
        transcript: "stale a result".into(),
        confidence: Some(0.99),
    };
    let error = db
        .commit_bound_champion_transcript_if_unreviewed(
            &stale_a,
            None,
            None,
            Some("external_provider"),
            false,
            &snapshot_a,
        )
        .unwrap_err();
    assert!(error.to_string().contains("E_TRANSCRIPTION_SOURCE_CHANGED"), "{error}");

    let stored_a = db.get_segment_by_id("bound-a").unwrap().unwrap();
    let stored_b = db.get_segment_by_id("bound-b").unwrap().unwrap();
    assert_eq!(stored_a.raw_transcript, "incumbent a");
    assert_eq!(stored_b.raw_transcript, "incumbent b");
    assert_eq!(db.get_hypotheses_for_segment("bound-a").unwrap()[0].transcript, "prior vote a");
    assert_eq!(db.get_hypotheses_for_segment("bound-b").unwrap()[0].transcript, "prior vote b");
}

#[test]
fn champion_commit_persists_the_exact_returned_registry_identity() {
    let db = make_db();
    let deployment_sha256 = "a".repeat(64);
    crate::registry::register_candidate(
        &db,
        &crate::registry::NewModelVersion {
            id: "omniasr-7b-returned-a".to_string(),
            family: "omniasr-7b".to_string(),
            model_card_name: Some("facebook/omniasr-llm-7b".to_string()),
            checkpoint_sha256: deployment_sha256.clone(),
            checkpoint_path: "/test/deployment-a.json".to_string(),
            source: "cortex-finetuned".to_string(),
            license: "test-only".to_string(),
        },
    )
    .unwrap();
    crate::registry::promote_to_champion(&db, "omniasr-7b-returned-a").unwrap();

    let mut segment = make_segment("identity-persist", "/identity-persist.wav");
    segment.raw_transcript = "incumbent draft".to_string();
    segment.model_version_id = Some("incumbent-model".to_string());
    db.insert_segment(&segment).unwrap();
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: segment.id.clone(),
        model_id: "incumbent-model".to_string(),
        transcript: "incumbent vote".to_string(),
        confidence: Some(0.2),
    })
    .unwrap();

    // These are the exact identity fields returned by the out-of-process ASR reply. The commit
    // must bind the model id to the current registry row carrying the same deployment digest before
    // either transcript table is changed.
    let returned = SegmentHypothesis {
        segment_id: segment.id.clone(),
        model_id: "omniasr-7b-returned-a".to_string(),
        transcript: "returned champion transcript".to_string(),
        confidence: Some(0.97),
    };
    assert!(db
        .commit_champion_transcript_if_unreviewed(
            &returned,
            Some(&deployment_sha256),
            Some("returned normalized transcript"),
            Some("external_provider"),
            false,
        )
        .unwrap());

    let stored = db.get_segment_by_id(&segment.id).unwrap().unwrap();
    assert_eq!(stored.model_version_id.as_deref(), Some(returned.model_id.as_str()));
    assert_eq!(stored.raw_transcript, returned.transcript);
    let hypotheses = db.get_hypotheses_for_segment(&segment.id).unwrap();
    assert_eq!(hypotheses.len(), 1);
    assert_eq!(hypotheses[0].model_id, returned.model_id);

    // `speech_segments.model_version_id` is the durable link to the immutable registry identity;
    // prove the stored transcript resolves to the exact deployment SHA supplied by the ASR reply.
    let persisted_identity: (String, String, String) = db
        .connection()
        .query_row(
            "SELECT s.model_version_id, h.model_version_id, mv.checkpoint_sha256
             FROM speech_segments s
             JOIN segment_hypotheses h ON h.segment_id = s.id
             JOIN model_versions mv ON mv.id = s.model_version_id
             WHERE s.id = ?1",
            [&segment.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        persisted_identity,
        (returned.model_id.clone(), returned.model_id, deployment_sha256),
        "both transcript records must resolve to the exact returned deployment identity"
    );
}

#[test]
fn champion_commit_refuses_without_writes_if_champion_rotated_after_asr_reply() {
    let db = make_db();
    let deployment_a_sha256 = "a".repeat(64);
    let deployment_b_sha256 = "b".repeat(64);
    for (id, sha, path) in [
        ("omniasr-7b-race-a", deployment_a_sha256.as_str(), "/test/deployment-race-a.json"),
        ("omniasr-7b-race-b", deployment_b_sha256.as_str(), "/test/deployment-race-b.json"),
    ] {
        crate::registry::register_candidate(
            &db,
            &crate::registry::NewModelVersion {
                id: id.to_string(),
                family: "omniasr-7b".to_string(),
                model_card_name: Some("facebook/omniasr-llm-7b".to_string()),
                checkpoint_sha256: sha.to_string(),
                checkpoint_path: path.to_string(),
                source: "cortex-finetuned".to_string(),
                license: "test-only".to_string(),
            },
        )
        .unwrap();
    }
    crate::registry::promote_to_champion(&db, "omniasr-7b-race-a").unwrap();

    let mut segment = make_segment("identity-race", "/identity-race.wav");
    segment.raw_transcript = "untouched transcript".to_string();
    segment.normalized_transcript = Some("untouched normalized".to_string());
    segment.confidence = Some(0.31);
    segment.confidence_source = Some("incumbent_source".to_string());
    segment.model_version_id = Some("incumbent-model".to_string());
    db.insert_segment(&segment).unwrap();
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: segment.id.clone(),
        model_id: "incumbent-model".to_string(),
        transcript: "untouched vote".to_string(),
        confidence: Some(0.31),
    })
    .unwrap();

    // Inference returned while A was champion. Before its reply reaches the commit boundary, a
    // promotion atomically rotates the registry to B.
    let stale_reply = SegmentHypothesis {
        segment_id: segment.id.clone(),
        model_id: "omniasr-7b-race-a".to_string(),
        transcript: "stale in-flight transcript".to_string(),
        confidence: Some(0.99),
    };
    crate::registry::promote_to_champion(&db, "omniasr-7b-race-b").unwrap();
    let changes_before: i64 = db.connection().query_row("SELECT total_changes()", [], |row| row.get(0)).unwrap();

    let error = db
        .commit_champion_transcript_if_unreviewed(
            &stale_reply,
            Some(&deployment_a_sha256),
            Some("stale normalized transcript"),
            Some("external_provider"),
            true,
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("MODEL_IDENTITY_CHANGED"),
        "a champion rotation must fail closed with a stable identity error: {error}"
    );
    let changes_after: i64 = db.connection().query_row("SELECT total_changes()", [], |row| row.get(0)).unwrap();
    assert_eq!(
        changes_after, changes_before,
        "the rejected stale reply must execute zero INSERT/UPDATE/DELETE statements"
    );

    let stored = db.get_segment_by_id(&segment.id).unwrap().unwrap();
    assert_eq!(stored.raw_transcript, "untouched transcript");
    assert_eq!(stored.normalized_transcript.as_deref(), Some("untouched normalized"));
    assert_eq!(stored.confidence, Some(0.31));
    assert_eq!(stored.confidence_source.as_deref(), Some("incumbent_source"));
    assert_eq!(stored.model_version_id.as_deref(), Some("incumbent-model"));
    assert!(!stored.cloud_call);
    let hypotheses = db.get_hypotheses_for_segment(&segment.id).unwrap();
    assert_eq!(hypotheses.len(), 1);
    assert_eq!(hypotheses[0].model_id, "incumbent-model");
    assert_eq!(hypotheses[0].transcript, "untouched vote");

    let current_champion: (String, String) = db
        .connection()
        .query_row(
            "SELECT id, checkpoint_sha256 FROM model_versions
             WHERE family = 'omniasr-7b' AND status = 'champion'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(current_champion, ("omniasr-7b-race-b".to_string(), deployment_b_sha256));
}

#[test]
fn champion_commit_cas_miss_preserves_human_owned_rows_and_existing_votes() {
    let db = make_db();
    for id in ["champion-verified", "champion-decision", "champion-verdict"] {
        let mut segment = make_segment(id, &format!("/{id}.wav"));
        segment.raw_transcript = format!("human-owned {id}");
        segment.normalized_transcript = Some(format!("human-owned {id}"));
        segment.confidence = Some(0.4);
        segment.confidence_source = Some("existing_source".to_string());
        segment.model_version_id = Some("existing-model".to_string());
        db.insert_segment(&segment).unwrap();
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: id.to_string(),
            model_id: "existing-model".to_string(),
            transcript: format!("existing vote {id}"),
            confidence: Some(0.4),
        })
        .unwrap();
    }
    db.connection().execute("UPDATE speech_segments SET verified = 1 WHERE id = 'champion-verified'", []).unwrap();
    db.connection()
        .execute("UPDATE speech_segments SET human_decision = 'edit' WHERE id = 'champion-decision'", [])
        .unwrap();
    db.connection()
        .execute("UPDATE speech_segments SET verdict = 'human_accept' WHERE id = 'champion-verdict'", [])
        .unwrap();

    for id in ["champion-verified", "champion-decision", "champion-verdict"] {
        let champion = SegmentHypothesis {
            segment_id: id.to_string(),
            model_id: "omniasr-wsl-7b".to_string(),
            transcript: "late champion draft".to_string(),
            confidence: Some(0.99),
        };
        assert!(
            !db.commit_champion_transcript_if_unreviewed(
                &champion,
                None,
                Some("late normalized draft"),
                Some("external_provider"),
                true,
            )
            .unwrap(),
            "{id} must return a CAS miss"
        );

        let stored = db.get_segment_by_id(id).unwrap().unwrap();
        assert_eq!(stored.raw_transcript, format!("human-owned {id}"));
        assert_eq!(stored.normalized_transcript.as_deref(), Some(format!("human-owned {id}").as_str()));
        assert_eq!(stored.confidence, Some(0.4));
        assert_eq!(stored.confidence_source.as_deref(), Some("existing_source"));
        assert_eq!(stored.model_version_id.as_deref(), Some("existing-model"));
        assert!(!stored.cloud_call);
        let hypotheses = db.get_hypotheses_for_segment(id).unwrap();
        assert_eq!(hypotheses.len(), 1, "CAS miss must not clean up votes for {id}");
        assert_eq!(hypotheses[0].model_id, "existing-model");
        assert_eq!(hypotheses[0].transcript, format!("existing vote {id}"));
    }
}

#[test]
fn champion_commit_rolls_back_transcript_and_hypotheses_when_hypothesis_insert_fails() {
    let db = make_db();
    let mut segment = make_segment("champion-rollback", "/champion-rollback.wav");
    segment.raw_transcript = "previous transcript".to_string();
    segment.normalized_transcript = Some("previous normalized".to_string());
    segment.confidence = Some(0.3);
    segment.confidence_source = Some("previous_source".to_string());
    segment.model_version_id = Some("previous-model".to_string());
    db.insert_segment(&segment).unwrap();
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: segment.id.clone(),
        model_id: "previous-model".to_string(),
        transcript: "previous vote".to_string(),
        confidence: Some(0.3),
    })
    .unwrap();
    db.connection()
        .execute_batch(
            "CREATE TRIGGER reject_champion_commit_hypothesis
             BEFORE INSERT ON segment_hypotheses
             WHEN NEW.model_id = 'omniasr-wsl-7b'
             BEGIN SELECT RAISE(ABORT, 'injected champion hypothesis failure'); END;",
        )
        .unwrap();

    let champion = SegmentHypothesis {
        segment_id: segment.id.clone(),
        model_id: "omniasr-wsl-7b".to_string(),
        transcript: "new champion transcript".to_string(),
        confidence: Some(0.99),
    };
    assert!(db
        .commit_champion_transcript_if_unreviewed(
            &champion,
            None,
            Some("new champion normalized"),
            Some("external_provider"),
            true,
        )
        .is_err());

    let stored = db.get_segment_by_id(&segment.id).unwrap().unwrap();
    assert_eq!(stored.raw_transcript, "previous transcript");
    assert_eq!(stored.normalized_transcript.as_deref(), Some("previous normalized"));
    assert_eq!(stored.confidence, Some(0.3));
    assert_eq!(stored.confidence_source.as_deref(), Some("previous_source"));
    assert_eq!(stored.model_version_id.as_deref(), Some("previous-model"));
    assert!(!stored.cloud_call);
    let hypotheses = db.get_hypotheses_for_segment(&segment.id).unwrap();
    assert_eq!(hypotheses.len(), 1, "the deleted prior vote must be restored by rollback");
    assert_eq!(hypotheses[0].model_id, "previous-model");
    assert_eq!(hypotheses[0].transcript, "previous vote");

    db.connection().execute_batch("DROP TRIGGER reject_champion_commit_hypothesis").unwrap();
    assert!(db
        .commit_champion_transcript_if_unreviewed(
            &champion,
            None,
            Some("new champion normalized"),
            Some("external_provider"),
            true,
        )
        .unwrap());
}

#[test]
fn machine_verdict_never_overwrites_a_human_decision() {
    // The jury (machine) write runs on a separate connection and may land AFTER a curator decided the
    // same segment mid-run. The human is authoritative: a late write_segment_verdict must be a no-op,
    // never reverting the human verdict/transcript or re-escalating an accepted segment.
    let db = make_db();
    db.insert_segment(&make_segment("hv1", "/hv1.wav")).unwrap();
    db.record_human_decision("hv1", "accept", None, None).unwrap();

    assert!(
        db.write_segment_verdict("hv1", "jury_accept", Some("machine consensus"), Some("r"), None, Some(0.9), false)
            .is_err(),
        "schema-v60+ must reject the late machine writer before it can touch human truth"
    );

    let seg = db.get_segment_by_id("hv1").unwrap().unwrap();
    assert_eq!(seg.verdict.as_deref(), Some("human_accept"), "machine verdict clobbered the human decision");
    assert_eq!(seg.human_decision.as_deref(), Some("accept"), "human_decision must be preserved");
    assert!(!seg.escalated, "a human-accepted segment must not be re-escalated by a late machine write");

    // Schema-v60+ disables the same writer for fresh rows too; human review is the only authority.
    db.insert_segment(&make_segment("hv2", "/hv2.wav")).unwrap();
    assert!(db.write_segment_verdict("hv2", "jury_accept", Some("machine"), None, None, Some(0.8), false).is_err());
    let seg2 = db.get_segment_by_id("hv2").unwrap().unwrap();
    assert_eq!(seg2.verdict, None, "a disabled machine writer must leave an open row untouched");
}

#[test]
fn exact_human_effect_undo_restores_the_prior_machine_state() {
    let db = make_db();
    db.insert_segment(&make_segment("cl1", "/cl1.wav")).unwrap();
    ensure_test_audio_content_hash(&db, "cl1");
    db.write_legacy_machine_verdict_for_test(
        "cl1",
        "jury_accept",
        Some("machine"),
        Some("machine rationale"),
        None,
        Some(0.8),
        false,
    )
    .unwrap();
    let prior = db.get_segment_by_id("cl1").unwrap().unwrap();
    db.record_human_decision("cl1", "edit", Some("human gold"), None).unwrap();
    assert_eq!(db.get_segment_by_id("cl1").unwrap().unwrap().verdict.as_deref(), Some("human_edit"));

    let effect_id = latest_human_effect_id(&db, "cl1");
    assert!(matches!(
        db.undo_human_decision(effect_id, None, "00000000-0000-4000-8000-000000000202").unwrap(),
        HumanDecisionUndoOutcome::Applied { .. }
    ));
    let restored = db.get_segment_by_id("cl1").unwrap().unwrap();
    assert_eq!(restored.human_decision, prior.human_decision);
    assert_eq!(restored.verdict, prior.verdict);
    assert_eq!(restored.verdict_transcript, prior.verdict_transcript);
    assert_eq!(restored.escalated, prior.escalated);
}

#[test]
fn human_decision_undo_refuses_rationale_drift() {
    let db = make_db();
    db.insert_segment(&make_segment("undo-rationale-cas", "/undo-rationale-cas.wav")).unwrap();
    db.write_legacy_machine_verdict_for_test(
        "undo-rationale-cas",
        "jury_accept",
        Some("machine"),
        Some("server rationale"),
        None,
        Some(0.8),
        false,
    )
    .unwrap();
    db.record_human_decision("undo-rationale-cas", "accept", None, Some(1)).unwrap();
    let effect_id = latest_human_effect_id(&db, "undo-rationale-cas");
    db.connection()
        .execute(
            "UPDATE speech_segments SET rationale = 'out-of-band rationale'
              WHERE id = 'undo-rationale-cas'",
            [],
        )
        .unwrap();
    assert!(matches!(
        db.undo_human_decision(effect_id, None, "00000000-0000-4000-8000-000000000205").unwrap(),
        HumanDecisionUndoOutcome::Conflict { .. }
    ));
    let kept = db.get_segment_by_id("undo-rationale-cas").unwrap().unwrap();
    assert_eq!(kept.human_decision.as_deref(), Some("accept"));
    assert_eq!(kept.rationale.as_deref(), Some("out-of-band rationale"));
}

#[test]
fn exact_review_flag_effect_undo_is_idempotent_and_conflict_safe() {
    let db = make_db();
    db.insert_segment(&make_segment("fl1", "/fl1.wav")).unwrap();
    let commit = db
        .record_review_flag("fl1", "Flagged for second-pass adjudication", "00000000-0000-4000-8000-000000000901")
        .unwrap();
    let flagged = commit.segment;
    assert!(flagged.escalated && flagged.verdict.as_deref() == Some("escalated"));

    assert!(matches!(
        db.undo_review_flag(commit.effect_event_id, "00000000-0000-4000-8000-000000000203").unwrap(),
        HumanFlagUndoOutcome::Applied { .. }
    ));
    let un = db.get_segment_by_id("fl1").unwrap().unwrap();
    assert!(!un.escalated, "escalated flag must be cleared (inverse of flag)");
    assert_eq!(un.verdict, None, "the 'escalated' verdict must be cleared");
    assert_eq!(un.rationale, None, "the flag rationale must be cleared");
    assert!(matches!(
        db.undo_review_flag(commit.effect_event_id, "00000000-0000-4000-8000-000000000203").unwrap(),
        HumanFlagUndoOutcome::AlreadyApplied { .. }
    ));

    // A later human decision wins; the stale flag undo is a no-mutation conflict.
    db.insert_segment(&make_segment("fl2", "/fl2.wav")).unwrap();
    let stale = db.record_review_flag("fl2", "flag", "00000000-0000-4000-8000-000000000902").unwrap();
    db.record_human_decision("fl2", "accept", None, None).unwrap();
    assert!(matches!(
        db.undo_review_flag(stale.effect_event_id, "00000000-0000-4000-8000-000000000204").unwrap(),
        HumanFlagUndoOutcome::Conflict { .. }
    ));
    let kept = db.get_segment_by_id("fl2").unwrap().unwrap();
    assert_eq!(kept.human_decision.as_deref(), Some("accept"), "a human-decided row must be untouched");
}

#[test]
fn generic_review_flags_cannot_forge_or_overwrite_the_technical_unusable_namespace() {
    let db = make_db();
    db.insert_segment(&make_segment("reserved-technical-flag", "/reserved.wav")).unwrap();
    let forged = db
        .record_review_flag(
            "reserved-technical-flag",
            "technical_unusable:v1:decodeFailed",
            "00000000-0000-4000-8000-000000000921",
        )
        .expect_err("free-form flags must not mint a structured technical exclusion");
    assert!(forged.to_string().contains("namespace is reserved"), "{forged}");

    let base_revision = db.segment_review_revision("reserved-technical-flag").unwrap().unwrap();
    let source = db.technical_unusable_source_snapshot("reserved-technical-flag").unwrap().unwrap();
    db.mark_segment_technically_unusable_after_verified_failure(
        "reserved-technical-flag",
        base_revision,
        "permissionDenied",
        &source.source_path_sha256,
        source.audio_content_hash.as_deref(),
        "00000000-0000-4000-8000-000000000922",
    )
    .unwrap();
    let overwrite = db
        .record_review_flag("reserved-technical-flag", "generic concern", "00000000-0000-4000-8000-000000000923")
        .expect_err("a generic flag must not remove an active technical export exclusion");
    assert!(overwrite.to_string().contains("undo its exact effect first"), "{overwrite}");
    let row = db.get_segment_by_id("reserved-technical-flag").unwrap().unwrap();
    assert_eq!(crate::quality::technical_unusable_reason(&row), Some("permissionDenied"));
}

#[test]
fn review_flag_requires_clean_or_immutable_legacy_human_baseline() {
    let forged = make_db();
    forged.insert_segment(&make_segment("flag-unbound", "/flag-unbound.wav")).unwrap();
    forged
        .connection()
        .execute(
            "UPDATE speech_segments
                SET verified = 1, annotated_transcript = 'forged unbound truth'
              WHERE id = 'flag-unbound'",
            [],
        )
        .unwrap();
    let error = forged
        .record_review_flag("flag-unbound", "must not launder this row", "00000000-0000-4000-8000-000000000903")
        .unwrap_err();
    assert!(
        error.to_string().contains("no immutable legacy or decision-effect authority"),
        "an effect must not snapshot unbound human truth as its baseline: {error}"
    );
    let unchanged = forged.get_segment_by_id("flag-unbound").unwrap().unwrap();
    assert_eq!(unchanged.verdict, None);
    assert!(!unchanged.escalated);
    let effect_count: i64 = forged
        .connection()
        .query_row("SELECT COUNT(*) FROM review_flag_effect_events WHERE segment_id = 'flag-unbound'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(effect_count, 0, "a refused flag must not leave partial effect evidence");

    // Each optional human-authority marker is independently sufficient to reject a first flag.
    // These cases pin the Rust-1.81-compatible Option checks; none may be treated as a clean
    // baseline merely because the other two markers are absent.
    for (suffix, column) in [("decision", "human_decision"), ("reviewer", "reviewed_by"), ("corrected", "corrected_at")]
    {
        let db = make_db();
        let segment_id = format!("flag-unbound-{suffix}");
        db.insert_segment(&make_segment(&segment_id, "/flag-unbound-marker.wav")).unwrap();
        db.connection()
            .execute(&format!("UPDATE speech_segments SET {column} = 'unbound authority' WHERE id = ?1"), [&segment_id])
            .unwrap();
        let prior = db.get_segment_by_id(&segment_id).unwrap().unwrap();
        assert!(
            !Database::flag_human_baseline_is_authorized_on(db.connection(), &prior).unwrap(),
            "{column} must independently make the baseline unauthorized"
        );
        let error = db
            .record_review_flag(
                &segment_id,
                "must reject every unbound marker",
                match suffix {
                    "decision" => "00000000-0000-4000-8000-000000000911",
                    "reviewer" => "00000000-0000-4000-8000-000000000912",
                    _ => "00000000-0000-4000-8000-000000000913",
                },
            )
            .expect_err("an unsnapshotted human-authority marker must reject the first flag");
        let message = error.to_string();
        if suffix == "decision" {
            assert!(message.contains("already has a human decision"), "unexpected refusal: {error}");
        } else {
            assert!(
                message.contains("no immutable legacy or decision-effect authority"),
                "{column} unexpectedly authorized a first flag: {error}"
            );
        }
    }

    let legacy = make_db();
    assert_eq!(crate::migrations::rollback(&legacy, 8).unwrap(), vec![67, 66, 65, 64, 63, 62, 61, 60]);
    let mut legacy_reviewed = make_segment("flag-legacy", "/flag-legacy.wav");
    legacy_reviewed.verified = true;
    legacy_reviewed.annotated_transcript = Some("immutable legacy truth".into());
    legacy.insert_segment_full(&legacy_reviewed).unwrap();
    assert_eq!(crate::migrations::run_migrations(&legacy).unwrap(), vec![60, 61, 62, 63, 64, 65, 66, 67]);
    let commit = legacy
        .record_review_flag("flag-legacy", "legacy row needs adjudication", "00000000-0000-4000-8000-000000000904")
        .expect("an exact immutable pre-v60 reviewed baseline remains flaggable");
    assert!(commit.segment.escalated);
    assert_eq!(commit.segment.annotated_transcript.as_deref(), Some("immutable legacy truth"));
    assert!(commit.segment.verified);
}

#[test]
fn search_segments_tie_order_is_deterministic_by_id() {
    let db = make_db();
    // Insert in non-sorted id order; all share the search token.
    for id in ["seg_m", "seg_a", "seg_z"] {
        let mut s = make_segment(id, &format!("/{id}.wav"));
        s.raw_transcript = "uniquesearchtoken body".to_string();
        db.insert_segment(&s).unwrap();
    }
    // Pin all rows' created_at to ONE value so the tie is GUARANTEED. created_at is stamped by
    // the column default datetime('now') at 1-second resolution, so if these near-instant inserts
    // straddle a one-second clock tick they no longer tie: ORDER BY created_at DESC (the primary
    // sort key) reorders them and the id-tiebreaker assertions below flake (~1-in-N runs, under
    // load). Forcing equal timestamps isolates the `id ASC` tiebreaker that is actually under test.
    db.conn.execute("UPDATE speech_segments SET created_at = '2026-01-01 00:00:00'", []).unwrap();
    let by_search: Vec<String> = db.search_segments("uniquesearchtoken").unwrap().into_iter().map(|s| s.id).collect();
    assert_eq!(by_search, vec!["seg_a", "seg_m", "seg_z"], "tied search results must order by id");

    let by_ids: Vec<String> = db
        .get_segments_by_ids(&["seg_z".into(), "seg_a".into(), "seg_m".into()])
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(by_ids, vec!["seg_a", "seg_m", "seg_z"], "tied id-batch results must order by id");
}

#[test]
fn get_segments_by_ids_handles_more_than_one_sqlite_param_chunk() {
    // 1200 ids spans >2 of the 500-id chunks. A single IN(?,?,…) of this size would overflow the
    // SQLite bound-parameter cap on older builds; the chunked fetch must return every row, with the
    // global (created_at DESC, id ASC) order preserved across chunk boundaries.
    let db = make_db();
    let n = 1200usize;
    for i in 0..n {
        let id = format!("seg_{i:05}");
        db.insert_segment(&make_segment(&id, &format!("/{id}.wav"))).unwrap();
    }
    // Pin created_at so the id ASC tiebreaker is what orders the result deterministically.
    db.conn.execute("UPDATE speech_segments SET created_at = '2024-01-01 00:00:00'", []).unwrap();
    let ids: Vec<String> = (0..n).map(|i| format!("seg_{i:05}")).collect();
    let got = db.get_segments_by_ids(&ids).unwrap();
    assert_eq!(got.len(), n, "every requested id must come back across all chunks");
    let got_ids: Vec<String> = got.into_iter().map(|s| s.id).collect();
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(got_ids, expected, "rows must be globally ordered by id ASC across chunk boundaries");
}

#[test]
fn write_boundary_rejects_invalid_segments() {
    let db = make_db();

    // Empty id, negative duration, and an unknown split are all rejected with a
    // clean AppError::Validation — never silently persisted to corrupt later math.
    let mut s = make_segment("", "/a.wav");
    assert!(matches!(db.insert_segment(&s), Err(AppError::Validation(_))), "empty id");

    s = make_segment("s1", "/a.wav");
    s.duration_ms = -1;
    assert!(matches!(db.insert_segment(&s), Err(AppError::Validation(_))), "negative duration");

    s = make_segment("s2", "/a.wav");
    s.split = Some("trainn".to_string());
    assert!(matches!(db.insert_segment(&s), Err(AppError::Validation(_))), "bogus split");

    // A valid segment (known split) inserts fine.
    s = make_segment("s3", "/a.wav");
    s.split = Some("validation".to_string());
    db.insert_segment(&s).expect("valid segment should insert");

    // A batch containing ANY invalid segment is rejected atomically — the savepoint
    // rolls back, so even the valid sibling does not persist.
    let good = make_segment("b1", "/b1.wav");
    let mut bad = make_segment("b2", "/b2.wav");
    bad.duration_ms = -10;
    assert!(db.insert_segments_batch(&[good, bad]).is_err(), "batch with an invalid segment must fail");
    assert!(
        db.get_segment_by_audio_path("/b1.wav").unwrap().is_none(),
        "the whole batch must roll back, including the valid segment"
    );

    // After a failed batch the connection must NOT hold an open savepoint/transaction — otherwise
    // the next command would run inside a stale transaction and a later rollback could silently
    // discard committed writes (round-17: release_savepoint cleans up even if the commit fails).
    assert!(db.conn.is_autocommit(), "a failed batch must leave no open transaction on the connection");
    db.insert_segment(&make_segment("after", "/after.wav")).expect("a write after a failed batch must commit");
    assert!(db.get_segment_by_audio_path("/after.wav").unwrap().is_some());
}

#[test]
fn get_segment_by_audio_path_returns_match() {
    let db = make_db();
    let seg = make_segment("s1", "/data/audio/file1.wav");
    db.insert_segment(&seg).unwrap();

    let found = db.get_segment_by_audio_path("/data/audio/file1.wav").unwrap();
    assert!(found.is_some(), "should find segment by audio_path");
    assert_eq!(found.unwrap().id, "s1");
}

#[test]
fn get_segment_by_audio_path_returns_none_when_absent() {
    let db = make_db();
    let found = db.get_segment_by_audio_path("/does/not/exist.wav").unwrap();
    assert!(found.is_none(), "should return None for unknown path");
}

#[test]
fn open_with_retry_quarantines_db_when_integrity_check_fails_after_open() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("recover.db");
    {
        let db = Database::open(path.to_str().expect("db path")).expect("open db");
        db.initialize().expect("initialize db");
        for i in 0..2000 {
            let mut segment = make_segment(&format!("corrupt-{i}"), &format!("/audio/{i}.wav"));
            segment.raw_transcript = "x".repeat(1000);
            db.insert_segment(&segment).expect("insert segment");
        }
        db.wal_checkpoint().expect("checkpoint");
    }

    let mut bytes = std::fs::read(&path).expect("read db");
    assert!(bytes.len() > 4096 + 64, "fixture database should span multiple pages");
    for byte in &mut bytes[4096..4096 + 64] {
        *byte = 0xFF;
    }
    std::fs::write(&path, bytes).expect("corrupt db page");

    {
        let corrupt = Database::open(path.to_str().expect("db path")).expect("corrupt db should still open");
        let integrity = corrupt.integrity_check().expect("integrity result");
        assert_ne!(integrity.trim(), "ok", "fixture must reproduce a post-open integrity failure");
    }

    let recovered = Database::open_with_retry(path.to_str().expect("db path")).expect("recover database");
    recovered.initialize().expect("initialize recovered db");

    assert_eq!(recovered.integrity_check().expect("integrity after recovery").trim(), "ok");
    assert_eq!(recovered.segment_count().expect("fresh segment count"), 0);
    assert!(
        std::fs::read_dir(tmp.path())
            .expect("read temp dir")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with("recover.corrupt.")),
        "corrupt database should be retained as a quarantine file"
    );
}

#[test]
fn on_disk_boot_applies_all_migrations_and_survives_restart() {
    // The real boot path (open_with_retry -> initialize) on a FILE-backed database, which the
    // :memory: migration tests never exercise: WAL, persistence across a close, and a second
    // open that must migrate nothing and still pass integrity_check. This is the end-to-end
    // smoke test that the continual-learning schema (v20..v23) actually applies on a genuine
    // app restart, not just in memory.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cortex-speech.db");
    let path_str = path.to_str().expect("db path");
    let head = crate::migrations::MIGRATIONS.iter().map(|m| m.version).max().expect("migrations");

    // First boot: open, migrate to head, persist, close.
    {
        let db = Database::open_with_retry(path_str).expect("first open");
        db.initialize().expect("first initialize");
        assert_eq!(crate::migrations::get_current_version(&db).expect("version"), head);
        assert_eq!(db.integrity_check().expect("integrity").trim(), "ok");
        db.wal_checkpoint().expect("checkpoint");
    }

    // Second boot (simulated restart): the persisted schema is already at head, so initialize
    // migrates nothing, and the new continual-learning tables + provenance column are present.
    let db = Database::open_with_retry(path_str).expect("reopen");
    db.initialize().expect("reopen initialize");
    assert_eq!(crate::migrations::get_current_version(&db).expect("version after restart"), head);
    assert_eq!(db.integrity_check().expect("integrity after restart").trim(), "ok");

    let conn = db.connection();
    for table in ["correction_memory", "corrections", "model_versions", "adapters"] {
        let exists: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1", [table], |r| r.get(0))
            .expect("table query");
        assert_eq!(exists, 1, "{table} must exist after an on-disk restart");
    }
    let has_stamp: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('segment_hypotheses') WHERE name='model_version_id'",
            [],
            |r| r.get(0),
        )
        .expect("stamp query");
    assert_eq!(has_stamp, 1, "the model_version_id provenance stamp must persist across a restart");
}

#[test]
fn corrupt_backup_path_avoids_same_second_collision() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("recover.db");
    let timestamp = 1_781_573_888;
    let first = path.with_extension(format!("corrupt.{timestamp}"));
    std::fs::write(&first, "already quarantined").expect("seed existing quarantine");

    let selected = unique_corrupt_backup_path(&path, timestamp);

    assert_eq!(selected.file_name().unwrap().to_string_lossy(), "recover.corrupt.1781573888.1");
    assert!(!selected.exists());
}

#[test]
fn recover_database_at_quarantines_sqlite_sidecars() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("recover.db");
    std::fs::write(&path, "main").expect("seed db");
    std::fs::write(sqlite_sidecar_path(&path, "-wal"), "wal").expect("seed wal");
    std::fs::write(sqlite_sidecar_path(&path, "-shm"), "shm").expect("seed shm");

    recover_database_at(path.to_str().expect("db path")).expect("recover database");

    assert!(!path.exists());
    assert!(!sqlite_sidecar_path(&path, "-wal").exists());
    assert!(!sqlite_sidecar_path(&path, "-shm").exists());

    let quarantine = std::fs::read_dir(tmp.path())
        .expect("read temp dir")
        .flatten()
        .map(|entry| entry.path())
        .find(|entry| entry.file_name().unwrap().to_string_lossy().starts_with("recover.corrupt."))
        .expect("main quarantine file");

    assert_eq!(std::fs::read_to_string(&quarantine).expect("read quarantined main"), "main");
    assert_eq!(std::fs::read_to_string(sqlite_sidecar_path(&quarantine, "-wal")).expect("read quarantined wal"), "wal");
    assert_eq!(std::fs::read_to_string(sqlite_sidecar_path(&quarantine, "-shm")).expect("read quarantined shm"), "shm");
}

#[test]
fn migration_v13_creates_audio_path_index() {
    let db = make_db();
    // Verify idx_segments_audio_path index exists after migrations.
    let count: i32 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_segments_audio_path'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "idx_segments_audio_path index should exist");
}

#[test]
fn fts_index_searches_inserted_segments_and_tracks_batch_delete() {
    let db = make_db();
    let mut first = make_segment("fts-1", "/data/audio/fts-1.wav");
    first.raw_transcript = "chiman reliable transcript".to_string();
    let mut second = make_segment("fts-2", "/data/audio/fts-2.wav");
    second.raw_transcript = "chiman retained transcript".to_string();

    db.insert_segment(&first).expect("insert first");
    db.insert_segment(&second).expect("insert second");

    let before_delete = db.search_segments("chiman").expect("search before delete");
    assert_eq!(before_delete.len(), 2, "FTS should index inserted transcripts");

    db.delete_segments_batch(&["fts-1".to_string()]).expect("batch delete");

    let after_delete = db.search_segments("chiman").expect("search after delete");
    assert_eq!(after_delete.len(), 1, "FTS should track batch deletes");
    assert_eq!(after_delete[0].id, "fts-2");
}

#[test]
fn vacuum_rebuilds_fts_and_leaves_search_working() {
    // VACUUM cannot be wrapped in a transaction with its compensating FTS rebuild, and in practice
    // this SQLite build PRESERVES speech_segments' rowids across VACUUM (verified: delete row 1 of
    // [1,2,3,4] then VACUUM leaves [2,3,4], not [1,2,3]), so the external-content FTS does not
    // actually desync — the rebuild inside vacuum() is defensive. This guards the observable
    // contract that matters: vacuum() succeeds and search still returns the right rows afterward.
    let db = make_db();
    let mut first = make_segment("vac-1", "/data/audio/vac-1.wav");
    first.raw_transcript = "chiman vacuumable transcript".to_string();
    let mut second = make_segment("vac-2", "/data/audio/vac-2.wav");
    second.raw_transcript = "chiman surviving transcript".to_string();
    db.insert_segment(&first).expect("insert first");
    db.insert_segment(&second).expect("insert second");
    db.delete_segments_batch(&["vac-1".to_string()]).expect("batch delete");

    db.vacuum().expect("vacuum must succeed");

    let hits = db.search_segments("chiman").expect("search after vacuum");
    assert_eq!(hits.len(), 1, "search must still work after VACUUM + FTS rebuild");
    assert_eq!(hits[0].id, "vac-2");
}

#[test]
fn search_treats_fts5_metacharacters_as_literal_text_not_query_syntax() {
    // Regression for the hardening-audit HIGH finding: FTS5 parses the bound value as a query,
    // so ordinary punctuation used to raise a hard error (unterminated string / no such column /
    // fts5: syntax error) and surface a confusing toast on every such keystroke.
    let db = make_db();
    let mut seg = make_segment("repro-1", "/a.wav");
    seg.raw_transcript = "hello world foo bar".to_string();
    db.insert_segment(&seg).expect("insert");

    // Each of these errored BEFORE the fix; all must now be Ok (results or empty), never Err.
    // The control-char cases (NUL etc.) are the regression a proptest later surfaced: an interior
    // NUL survived split_whitespace and made SQLite/FTS5 raise a hard error.
    for q in
        ["\"hello", "foo:bar", "*", "(", "NEAR(a b", "a AND", "OR", "^", "-foo", ")", "\0", "a\0b", "\u{1b}", "\u{7f}"]
    {
        assert!(db.search_segments(q).is_ok(), "query {q:?} must not error");
    }
    // A real token still finds the segment, and a quote next to it doesn't break matching.
    assert_eq!(db.search_segments("hello").unwrap().len(), 1, "literal token still matches");
    assert_eq!(db.search_segments("\"hello\"").unwrap().len(), 1, "quoted token matches too");
    // Whitespace-only input is an empty result, not an error.
    assert!(db.search_segments("   ").unwrap().is_empty(), "blank query -> empty, not error");
}

#[test]
fn search_segments_never_errors_on_arbitrary_input() {
    use proptest::prelude::*;
    // Property generalization of the metacharacter regression above: for ANY user input the
    // search box must return Ok (results or empty), never an FTS5 syntax Err or a panic. The
    // example test samples known-bad punctuation; this covers the infinite input space.
    let db = make_db();
    let mut seg = make_segment("prop-1", "/a.wav");
    seg.raw_transcript = "hello world foo bar".to_string();
    db.insert_segment(&seg).expect("insert");

    proptest!(|(q in ".*")| {
        prop_assert!(db.search_segments(&q).is_ok(), "search must not error on input {q:?}");
    });
}

#[test]
fn insert_segment_accepts_arbitrary_transcript_text_and_keeps_search_queryable() {
    use proptest::prelude::*;
    // Write-path sibling of the search robustness property: user annotations/corrections are
    // free text, so persisting ANY transcript body must not error and must not corrupt the FTS
    // index it feeds (a later search must still return Ok, never an indexing-time syntax error).
    proptest!(|(body in ".*")| {
        let db = make_db();
        let mut seg = make_segment("prop-w", "/w.wav");
        seg.raw_transcript = body.clone();
        prop_assert!(db.insert_segment(&seg).is_ok(), "insert must not error on body {body:?}");
        prop_assert!(db.search_segments("hello").is_ok(), "search must stay Ok after body {body:?}");
    });
}

#[test]
fn consensus_batch_preserves_human_reviewed_transcripts() {
    // Hardening-audit MEDIUM (silent data loss): the consensus refinery overwrote human-corrected
    // transcripts because update_segment_consensus_batch lacked the human-review guard that every
    // other transcript-write path (e.g. update_asr_transcript_if_unreviewed) enforces.
    let db = make_db();
    let mut locked = make_segment("locked-1", "/a.wav");
    locked.raw_transcript = "human corrected text".to_string();
    locked.normalized_transcript = Some("human corrected text".to_string());
    db.insert_segment(&locked).expect("insert locked");
    db.conn
        .execute("UPDATE speech_segments SET verdict='human_edit', human_decision='edit' WHERE id='locked-1'", [])
        .expect("lock as human-reviewed");

    // The refinery's batch write tries to replace the locked segment with machine consensus.
    db.update_segment_consensus_batch(&[(
        "locked-1".to_string(),
        "machine consensus text".to_string(),
        "machine consensus text".to_string(),
        0.9,
    )])
    .expect("consensus batch");

    let after = db.get_segment_by_id("locked-1").unwrap().expect("segment exists");
    assert_eq!(after.raw_transcript, "human corrected text", "human correction must NOT be clobbered");
    assert_eq!(after.normalized_transcript.as_deref(), Some("human corrected text"));

    // An UNREVIEWED segment is still refined normally (the guard only protects human-locked rows).
    let mut fresh = make_segment("fresh-1", "/b.wav");
    fresh.raw_transcript = "old asr".to_string();
    db.insert_segment(&fresh).expect("insert fresh");
    db.update_segment_consensus_batch(&[(
        "fresh-1".to_string(),
        "new consensus".to_string(),
        "new consensus".to_string(),
        0.8,
    )])
    .expect("consensus batch 2");
    assert_eq!(
        db.get_segment_by_id("fresh-1").unwrap().unwrap().raw_transcript,
        "new consensus",
        "an unreviewed segment is still refined"
    );
}

#[test]
fn batch_transcription_update_preserves_human_review_and_never_writes_annotation() {
    // Round-9 audit HIGH (lost update): batch_transcribe wrote the whole STALE snapshot back via
    // insert_segment, reverting a concurrent human verify/edit. The guarded targeted write must
    // (a) refuse to touch a human-verified/reviewed row, (b) never revert `verified`, (c) seed the
    // annotation only when still empty, and (d) preserve an existing human annotation (COALESCE).
    let db = make_db();

    // (a)+(b): a human verified + annotated this row AFTER the batch prefetched it.
    let mut verified = make_segment("verified-1", "/a.wav");
    verified.raw_transcript = "old asr".to_string();
    db.insert_segment(&verified).expect("insert verified");
    db.conn
        .execute("UPDATE speech_segments SET verified=1, annotated_transcript='human gold' WHERE id='verified-1'", [])
        .expect("mark verified");
    let updated = db
        .update_batch_transcription_if_unreviewed(
            "verified-1",
            "fresh asr",
            Some("fresh asr"),
            Some(0.9),
            Some("heuristic"),
            Some("omniasr-wsl-7b"),
            false,
        )
        .expect("update verified");
    assert!(!updated, "a verified row must be skipped, not updated");
    let after = db.get_segment_by_id("verified-1").unwrap().unwrap();
    assert!(after.verified, "verified flag must NOT be reverted by the batch");
    assert_eq!(after.annotated_transcript.as_deref(), Some("human gold"), "human annotation preserved");
    assert_eq!(after.raw_transcript, "old asr", "human-owned row's raw must not be clobbered");

    // (c): a fresh unreviewed row IS updated — and annotated_transcript stays EMPTY. The old
    // behaviour ("annotation seeded when empty") wrote the machine draft into the human-only field,
    // where the couch/editor precedence served it forever over every later champion re-draft — the
    // 348-row 2026-08-12 incident. Machine text lands in raw/normalized ONLY.
    let mut fresh = make_segment("fresh-1", "/b.wav");
    fresh.raw_transcript = "old".to_string();
    fresh.annotated_transcript = None;
    db.insert_segment(&fresh).expect("insert fresh");
    let updated = db
        .update_batch_transcription_if_unreviewed(
            "fresh-1",
            "new asr",
            Some("new asr"),
            Some(0.8),
            Some("heuristic"),
            Some("omniasr-wsl-7b"),
            false,
        )
        .expect("update fresh");
    assert!(updated, "an unreviewed row is updated");
    let after = db.get_segment_by_id("fresh-1").unwrap().unwrap();
    assert_eq!(after.raw_transcript, "new asr");
    assert_eq!(after.annotated_transcript, None, "machine text must NEVER enter the human-only field");
    assert_eq!(after.confidence_source.as_deref(), Some("heuristic"));
    assert_eq!(after.model_version_id.as_deref(), Some("omniasr-wsl-7b"));
    assert!(!after.cloud_call);

    // (d): an unverified row the user annotated (without verifying) keeps that annotation; only
    // the ASR fields refresh — the batch write never mentions the annotated column at all.
    let mut annotated = make_segment("annot-1", "/c.wav");
    annotated.raw_transcript = "old".to_string();
    annotated.annotated_transcript = Some("user typed".to_string());
    db.insert_legacy_segment_fixture(&annotated).expect("insert historical annotated fixture");
    let updated = db
        .update_batch_transcription_if_unreviewed(
            "annot-1",
            "new asr",
            Some("new asr"),
            Some(0.7),
            Some("real_posterior"),
            Some("omniasr-wsl-7b"),
            false,
        )
        .expect("update annotated");
    assert!(updated, "an unverified annotated row still refreshes ASR");
    let after = db.get_segment_by_id("annot-1").unwrap().unwrap();
    assert_eq!(after.annotated_transcript.as_deref(), Some("user typed"), "existing annotation preserved");
    assert_eq!(after.raw_transcript, "new asr", "raw ASR refreshed on an unverified row");
    assert_eq!(after.confidence_source.as_deref(), Some("real_posterior"));
    assert_eq!(after.model_version_id.as_deref(), Some("omniasr-wsl-7b"));
}

#[test]
fn consensus_batch_never_touches_a_flag_verified_row() {
    // Audit #24: the guard checked human_decision/verdict but not `verified`, unlike its sibling
    // update_batch_transcription_if_unreviewed — so a clip the human deliberately closed out with
    // the verify flag (no decision recorded) still had its transcripts rewritten by machine
    // consensus.
    let db = make_db();
    let mut s = make_segment("cv-1", "/cv.wav");
    s.raw_transcript = "دەقی داخراو".to_string();
    db.insert_segment(&s).unwrap();
    db.update_verified_for_test("cv-1", true).unwrap();

    let changed = db
        .update_segment_consensus_batch(&[("cv-1".to_string(), "دەقی مەکینە".to_string(), "norm".to_string(), 0.9)])
        .unwrap();
    assert_eq!(changed, 0, "a verified row must be skipped by machine consensus");
    let after = db.get_segment_by_id("cv-1").unwrap().unwrap();
    assert_eq!(after.raw_transcript, "دەقی داخراو", "the closed-out transcript must survive");
}

#[test]
fn human_edit_does_not_write_no_op_correction_ledger_row() {
    // Round-9 audit LOW: when the model was already right (no candidate differs from the fix),
    // wrong_side falls back to raw_transcript, so the corrections ledger used to record a row whose
    // raw_hypothesis == human_fix. A real (resolvable) audio file is required so the ledger hash
    // resolves — otherwise the ledger insert is skipped for an unrelated reason and the bug hides.
    let db = make_db();
    let tmp = tempfile::tempdir().expect("tmp");
    let audio = tmp.path().join("clip.wav");
    std::fs::write(&audio, b"RIFFxxxxWAVEfmt ").expect("write audio");
    let audio_path = audio.to_string_lossy().to_string();

    let mut seg = make_segment("noop-1", &audio_path);
    seg.raw_transcript = "hello world".to_string();
    db.insert_segment(&seg).expect("insert");
    ensure_test_audio_content_hash(&db, "noop-1");

    // A no-op edit: the corrected text equals the raw ASR (up to the learning key).
    db.record_human_decision("noop-1", "edit", Some("hello world"), None).expect("record no-op edit");

    let ledger_rows: i64 =
        db.conn.query_row("SELECT COUNT(*) FROM corrections WHERE segment_id='noop-1'", [], |r| r.get(0)).unwrap();
    assert_eq!(ledger_rows, 0, "a no-op edit must NOT append a corrections-ledger row");

    // A genuine correction on the same kind of row DOES record a ledger entry.
    let mut seg2 = make_segment("real-1", &audio_path);
    seg2.raw_transcript = "helo wrld".to_string();
    db.insert_segment(&seg2).expect("insert real");
    ensure_test_audio_content_hash(&db, "real-1");
    db.record_human_decision("real-1", "edit", Some("hello world"), None).expect("record real edit");
    let real_rows: i64 =
        db.conn.query_row("SELECT COUNT(*) FROM corrections WHERE segment_id='real-1'", [], |r| r.get(0)).unwrap();
    assert_eq!(real_rows, 1, "a genuine correction still records a ledger row");
}

#[test]
fn merge_dataset_json_does_not_count_human_protected_rows_as_updated() {
    // Hardening-audit LOW: the guarded merge UPDATE correctly skips human-reviewed rows, but the
    // 'updated' counter incremented regardless of rows-affected — over-reporting to the UI.
    let db = make_db();
    let mut seg = make_segment("merge-1", "/a.wav");
    seg.raw_transcript = "original".to_string();
    db.insert_segment(&seg).expect("insert");
    db.conn
        .execute("UPDATE speech_segments SET verdict='human_accept' WHERE id='merge-1'", [])
        .expect("lock as human-reviewed");

    let incoming = vec![SpeechSegment {
        id: "merge-1".to_string(),
        audio_path: "/a.wav".to_string(),
        raw_transcript: "incoming".to_string(),
        duration_ms: 1000,
        ..SpeechSegment::default()
    }];
    let json = serde_json::to_string(&incoming).expect("serialize");
    let (created, updated) = db.merge_dataset_json(&json).expect("merge");
    assert_eq!((created, updated), (0, 0), "a guard-skipped human-locked row must not count as updated");
    assert_eq!(
        db.get_segment_by_id("merge-1").unwrap().unwrap().raw_transcript,
        "original",
        "the locked row is genuinely unchanged"
    );
}

#[test]
fn merge_dataset_json_does_not_overwrite_a_verified_only_row() {
    // A human who clicked "Verify"/"Verify selected" (batch_verify -> update_verified) sets ONLY verified=1,
    // leaving human_decision/verdict NULL. A pasted-dataset merge must NOT overwrite such a row — otherwise it
    // silently replaces the human's reviewed transcript with imported machine text (and, if the import carries
    // verified=true, ships unapproved text as human-verified GOLD). Sibling of
    // wsl_refinement_must_not_overwrite_a_verified_transcript; the verified=0 hole the merge guard missed
    // (found by adversarial hunt-7). update_asr_transcript_if_unreviewed / update_batch_transcription_if_
    // unreviewed already carry this clause.
    let db = make_db();
    let mut seg = make_segment("merge-ver", "/v.wav");
    seg.raw_transcript = "human verified original".to_string();
    db.insert_segment(&seg).expect("insert");
    assert!(db.update_verified_for_test("merge-ver", true).unwrap());
    let locked = db.get_segment_by_id("merge-ver").unwrap().unwrap();
    assert!(locked.verified && locked.human_decision.is_none(), "verify leaves decision NULL (the precondition)");

    let incoming = vec![SpeechSegment {
        id: "merge-ver".to_string(),
        audio_path: "/v.wav".to_string(),
        raw_transcript: "incoming machine text".to_string(),
        verified: false, // an import that would silently UN-verify + replace the human's work
        duration_ms: 1000,
        ..SpeechSegment::default()
    }];
    let (created, updated) = db.merge_dataset_json(&serde_json::to_string(&incoming).unwrap()).expect("merge");
    assert_eq!((created, updated), (0, 0), "a verified row must be skipped, not overwritten");
    let after = db.get_segment_by_id("merge-ver").unwrap().unwrap();
    assert_eq!(after.raw_transcript, "human verified original", "verified transcript must be intact");
    assert!(after.verified, "verified flag must stay set");

    // Schema v60 cannot accept renderer-authored verification even for a new id. A verified row needs
    // immutable legacy/effect authority; JSON import can prove neither.
    let fresh = vec![SpeechSegment {
        id: "merge-new".to_string(),
        audio_path: "/n.wav".to_string(),
        raw_transcript: "brand new".to_string(),
        verified: true,
        duration_ms: 1000,
        ..SpeechSegment::default()
    }];
    let err = db.merge_dataset_json(&serde_json::to_string(&fresh).unwrap()).unwrap_err();
    assert!(err.to_string().contains("review-owned field(s) verified"), "unexpected error: {err}");
    assert!(db.get_segment_by_id("merge-new").unwrap().is_none(), "refusal must create no unbound human row");
}

#[test]
fn merge_dataset_json_v60_accepts_machine_only_insert_and_update() {
    let db = make_db();
    let incoming = vec![SpeechSegment {
        id: "merge-machine".to_string(),
        created_at: Some("2026-08-22 01:02:03".to_string()),
        audio_path: "/machine.wav".to_string(),
        raw_transcript: "machine draft one".to_string(),
        normalized_transcript: Some("machine normalized one".to_string()),
        duration_ms: 1_000,
        confidence: Some(0.7),
        confidence_source: Some("real_posterior".to_string()),
        model_version_id: Some("omniasr-7b-champion".to_string()),
        ..SpeechSegment::default()
    }];
    assert_eq!(
        db.merge_dataset_json(&serde_json::to_string(&incoming).unwrap()).unwrap(),
        (1, 0),
        "machine-only new rows remain importable"
    );
    let created = db.get_segment_by_id("merge-machine").unwrap().unwrap();
    assert_eq!(created.raw_transcript, "machine draft one");
    assert_eq!(created.created_at.as_deref(), Some("2026-08-22 01:02:03"));
    assert!(!created.verified);
    assert!(created.annotated_transcript.is_none());
    assert!(created.human_decision.is_none());

    let replacement = vec![SpeechSegment {
        id: "merge-machine".to_string(),
        audio_path: "/machine-v2.wav".to_string(),
        raw_transcript: "machine draft two".to_string(),
        normalized_transcript: Some("machine normalized two".to_string()),
        duration_ms: 1_001,
        confidence: Some(0.8),
        confidence_source: Some("real_posterior".to_string()),
        model_version_id: Some("omniasr-7b-champion".to_string()),
        ..SpeechSegment::default()
    }];
    assert_eq!(
        db.merge_dataset_json(&serde_json::to_string(&replacement).unwrap()).unwrap(),
        (0, 1),
        "machine-only existing rows remain updateable"
    );
    let updated = db.get_segment_by_id("merge-machine").unwrap().unwrap();
    assert_eq!(updated.raw_transcript, "machine draft two");
    assert_eq!(updated.audio_path, "/machine-v2.wav");
    assert_eq!(updated.created_at.as_deref(), Some("2026-08-22 01:02:03"), "merge updates preserve row identity");
    assert!(updated.annotated_transcript.is_none());
    assert!(!updated.verified);
}

#[test]
fn merge_dataset_json_v60_rejects_mixed_review_payload_before_any_insert_or_update() {
    let db = make_db();
    let mut existing = make_segment("merge-existing", "/existing.wav");
    existing.raw_transcript = "original machine draft".to_string();
    db.insert_segment(&existing).unwrap();

    let payload = vec![
        SpeechSegment {
            id: "merge-existing".to_string(),
            audio_path: "/replacement.wav".to_string(),
            raw_transcript: "replacement machine draft".to_string(),
            duration_ms: 1_000,
            ..SpeechSegment::default()
        },
        SpeechSegment {
            id: "merge-forged-review".to_string(),
            audio_path: "/forged.wav".to_string(),
            raw_transcript: "machine draft".to_string(),
            annotated_transcript: Some("renderer-authored human answer".to_string()),
            verified: true,
            human_decision: Some("edit".to_string()),
            reviewed_by: Some("forged-reviewer".to_string()),
            duration_ms: 1_000,
            ..SpeechSegment::default()
        },
    ];
    let err = db.merge_dataset_json(&serde_json::to_string(&payload).unwrap()).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("Dataset merge refused atomically"), "unexpected error: {message}");
    assert!(message.contains("annotatedTranscript") && message.contains("verified") && message.contains("reviewedBy"));
    assert_eq!(
        db.get_segment_by_id("merge-existing").unwrap().unwrap().raw_transcript,
        "original machine draft",
        "the valid-looking update must not run before a later forged row is rejected"
    );
    assert!(
        db.get_segment_by_id("merge-forged-review").unwrap().is_none(),
        "the forged insert must leave no partial row"
    );
}

#[test]
fn consensus_batch_counts_only_rows_actually_changed() {
    // Round-2 audit LOW: the refinery reported updates.len() (attempted), not rows changed, so a
    // guard-skipped human-locked segment was over-counted. The method now returns rows-affected.
    let db = make_db();
    let mut locked = make_segment("c-lock", "/a.wav");
    locked.raw_transcript = "orig".to_string();
    db.insert_segment(&locked).expect("insert locked");
    db.conn.execute("UPDATE speech_segments SET verdict='human_accept' WHERE id='c-lock'", []).expect("lock");
    db.insert_segment(&make_segment("c-fresh", "/b.wav")).expect("insert fresh");

    let changed = db
        .update_segment_consensus_batch(&[
            ("c-lock".to_string(), "new".to_string(), "new".to_string(), 0.9),
            ("c-fresh".to_string(), "new2".to_string(), "new2".to_string(), 0.9),
        ])
        .expect("batch");
    assert_eq!(changed, 1, "only the unlocked row counts; the human-locked one is skipped");
}

#[test]
fn fts_search_matches_sorani_codepoint_variants() {
    let db = make_db();
    // The canonical normalized_transcript uses Kurdish Keheh (ک U+06A9) + Yeh
    // (ی U+06CC). raw_transcript is deliberately non-matching Latin so the test
    // isolates whether a query typed with the Arabic Kaf/Yeh variant still
    // matches the canonical normalized text.
    let mut seg = make_segment("fts-var", "/data/audio/fts-var.wav");
    seg.raw_transcript = "zzz".to_string();
    seg.normalized_transcript = Some("کوردی".to_string());
    db.insert_segment(&seg).expect("insert segment");

    // Query uses Arabic Kaf (ك U+0643) + Arabic Yeh (ي U+064A) — distinct codepoints.
    let hits = db.search_segments("كوردي").expect("variant search");
    assert_eq!(hits.len(), 1, "a variant-typed query must match the canonical normalized transcript");
    assert_eq!(hits[0].id, "fts-var");
}

#[test]
fn human_edit_learning_uses_agent_proposal_before_raw_asr() {
    let db = make_db();
    let mut seg = make_segment("learn-agent", "/data/audio/learn-agent.wav");
    seg.raw_transcript = "raw wrong transcript".to_string();
    seg.normalized_transcript = Some("normalized wrong transcript".to_string());
    db.insert_segment(&seg).expect("insert segment");
    ensure_test_audio_content_hash(&db, "learn-agent");
    db.write_legacy_machine_verdict_for_test(
        "learn-agent",
        "jury_accept",
        Some("agent proposed transcript"),
        Some("agent rationale"),
        None,
        Some(0.81),
        false,
    )
    .expect("write agent verdict");

    db.record_human_decision("learn-agent", "edit", Some("human corrected transcript"), None)
        .expect("record human edit");

    let fresh = db.get_segment_by_id("learn-agent").expect("load segment").expect("segment exists");
    assert_eq!(fresh.human_decision.as_deref(), Some("edit"));
    assert_eq!(fresh.verdict.as_deref(), Some("human_edit"));
    assert_eq!(fresh.verdict_transcript.as_deref(), Some("human corrected transcript"));
    assert!(!fresh.escalated);

    let (wrong, fix): (String, String) = db
        .connection()
        .query_row(
            "SELECT wrong_transcript, human_fix FROM agent_examples WHERE segment_id = ?1",
            params!["learn-agent"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("learning example exists");
    assert_eq!(wrong, "agent proposed transcript");
    assert_eq!(fix, "human corrected transcript");
}

#[test]
fn human_edit_skips_learning_pair_when_proposal_matches_fix() {
    let db = make_db();
    let mut seg = make_segment("learn-same", "/data/audio/learn-same.wav");
    seg.raw_transcript = "same text".to_string();
    db.insert_segment(&seg).expect("insert segment");
    ensure_test_audio_content_hash(&db, "learn-same");
    db.write_legacy_machine_verdict_for_test(
        "learn-same",
        "jury_accept",
        Some("same   text"),
        None,
        None,
        Some(0.9),
        false,
    )
    .expect("write agent verdict");

    db.record_human_decision("learn-same", "edit", Some("same text"), None).expect("record human edit");

    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM agent_examples WHERE segment_id = ?1", params!["learn-same"], |row| row.get(0))
        .expect("count examples");
    assert_eq!(count, 0);
}

#[test]
fn record_human_decision_uses_stored_pcm_hash_for_corrections_ledger() {
    let db = make_db();
    // The correction identity is the server-owned canonical decoded-PCM hash already stored on
    // the row. It must never be recomputed from mutable file bytes at decision time.
    let tmp = tempfile::tempdir().expect("tempdir");
    let audio = tmp.path().join("clip.wav");
    std::fs::write(&audio, b"RIFF....fake-audio-bytes").expect("write audio");

    let mut seg = make_segment("led-1", audio.to_str().expect("audio path"));
    seg.raw_transcript = "wrong text".to_string();
    db.insert_segment(&seg).expect("insert segment");
    let expected_hash = ensure_test_audio_content_hash(&db, "led-1");
    std::fs::write(&audio, b"different bytes after import").expect("mutate source after canonical import");
    // The agent verdict the human is about to override (captured into jury_verdict).
    db.write_legacy_machine_verdict_for_test("led-1", "jury_accept", Some("agent guess"), None, None, Some(0.7), false)
        .expect("write agent verdict");

    db.record_human_decision("led-1", "edit", Some("right text"), None).expect("record edit");

    let (segment_id, hash, raw_hyp, fix, jury, mv): (
        Option<String>,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    ) = db
        .connection()
        .query_row(
            "SELECT segment_id, audio_content_hash, raw_hypothesis, human_fix, jury_verdict, model_version_id
                 FROM corrections WHERE segment_id = ?1",
            params!["led-1"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .expect("a corrections ledger row must exist after an edit");
    assert_eq!(segment_id.as_deref(), Some("led-1"));
    assert_eq!(hash, expected_hash, "the ledger must use the stored canonical PCM content hash exactly");
    assert!(!raw_hyp.is_empty(), "raw_hypothesis must record what the model produced");
    assert_eq!(fix, "right text");
    assert_eq!(jury.as_deref(), Some("jury_accept"), "jury_verdict captures the pre-override agent verdict");
    assert_eq!(mv.as_deref(), Some("unknown@pre-registry"), "model_version_id provenance is stamped");
}

#[test]
fn non_edit_decision_writes_no_corrections_ledger_row() {
    let db = make_db();
    let tmp = tempfile::tempdir().expect("tempdir");
    let audio = tmp.path().join("clip.wav");
    std::fs::write(&audio, b"bytes").expect("write audio");
    let mut seg = make_segment("led-acc", audio.to_str().expect("path"));
    seg.raw_transcript = "ok text".to_string();
    db.insert_segment(&seg).expect("insert segment");

    db.record_human_decision("led-acc", "accept", None, None).expect("record accept");

    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM corrections WHERE segment_id = ?1", params!["led-acc"], |r| r.get(0))
        .expect("count");
    assert_eq!(count, 0, "an accept (non-edit) decision records no correction");
}

#[test]
fn edit_without_stored_pcm_identity_is_refused_atomically() {
    // An edit is durable training data. Missing server-owned PCM identity must fail closed rather
    // than record an unbindable verdict or silently omit its correction provenance.
    let db = make_db();
    let mut seg = make_segment("led-missing", "/nonexistent/gone.wav");
    seg.raw_transcript = "wrong".to_string();
    db.insert_segment(&seg).expect("insert segment");

    let err = db
        .record_human_decision("led-missing", "edit", Some("right"), None)
        .expect_err("edit without canonical server identity must be refused");
    assert!(err.to_string().contains("canonical server-owned PCM content hash"));

    let fresh = db.get_segment_by_id("led-missing").expect("load").expect("exists");
    assert!(fresh.human_decision.is_none(), "a refused edit must leave the row untouched");
    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM corrections WHERE segment_id = ?1", params!["led-missing"], |r| r.get(0))
        .expect("count");
    assert_eq!(count, 0, "a refused edit must mint no correction row");
}

#[test]
fn edit_populates_correction_memory_with_substitution() {
    let db = make_db();
    let mut seg = make_segment("mem-1", "/data/audio/mem-1.wav");
    seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
    db.insert_segment(&seg).expect("insert");
    ensure_test_audio_content_hash(&db, "mem-1");
    db.record_human_decision("mem-1", "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");

    let (wrong, human, hits): (String, String, i64) = db
        .connection()
        .query_row(
            "SELECT wrong_token, human_token, hit_count
               FROM effective_correction_memory_v60 WHERE source_segment = ?1",
            params!["mem-1"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("a correction memory row must exist after a substituting edit");
    assert_eq!(wrong, "باش");
    assert_eq!(human, "خراپ");
    assert_eq!(hits, 0, "a freshly captured memory starts at hit_count 0");
}

#[test]
fn repeated_correction_bumps_hit_count_not_duplicates() {
    let db = make_db();
    for id in ["mem-a", "mem-b"] {
        let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
        seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&seg).expect("insert");
        ensure_test_audio_content_hash(&db, id);
        db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
    }
    let (rows, max_hits): (i64, i64) = db
        .connection()
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(hit_count), 0) FROM effective_correction_memory_v60
                 WHERE wrong_token = 'باش' AND human_token = 'خراپ'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("query");
    assert_eq!(rows, 1, "the same correction must upsert, not duplicate");
    assert_eq!(max_hits, 1, "a second independent confirmation bumps hit_count to 1");
}

#[test]
fn a_single_edit_repeating_one_confusion_counts_as_one_capture_not_a_confirmation() {
    // hit_count tracks INDEPENDENT (cross-segment) confirmations — the anti-one-off guard. A SINGLE edit
    // on a SINGLE segment whose sentence repeats the same confusion ("باش"→"خراپ" twice) must capture ONE
    // memory at hit_count 0 (a fresh capture), NOT hit_count 1 — otherwise a lone self-repeating edit fakes
    // a second confirmation and can clear min_hits on its own. (Regression for the within-correction
    // duplicate double-count.)
    let db = make_db();
    let mut seg = make_segment("mem-rep", "/data/audio/mem-rep.wav");
    seg.raw_transcript = "ئەو باش بوو ئەو باش بوو".to_string();
    db.insert_segment(&seg).expect("insert");
    ensure_test_audio_content_hash(&db, "mem-rep");
    db.record_human_decision("mem-rep", "edit", Some("ئەو خراپ بوو ئەو خراپ بوو"), None).expect("edit");

    let (rows, max_hits): (i64, i64) = db
        .connection()
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(hit_count), 0) FROM effective_correction_memory_v60
                 WHERE wrong_token = 'باش' AND human_token = 'خراپ'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("query");
    assert_eq!(rows, 1, "a repeated confusion in ONE edit must capture a single memory, not duplicate");
    assert_eq!(max_hits, 0, "one edit is one capture (hit_count 0), never a self-made 'independent' confirmation");
}

#[test]
fn gold_edit_does_not_populate_correction_memory() {
    let db = make_db();
    let mut seg = make_segment("mem-gold", "/data/audio/mem-gold.wav");
    seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
    db.insert_segment(&seg).expect("insert");
    ensure_test_audio_content_hash(&db, "mem-gold");
    db.connection().execute("UPDATE speech_segments SET is_gold = 1 WHERE id = 'mem-gold'", []).expect("mark gold");
    db.record_human_decision("mem-gold", "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");

    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM correction_memory WHERE source_segment = 'mem-gold'", [], |r| r.get(0))
        .expect("count");
    assert_eq!(count, 0, "gold-segment edits must not populate LOOP-0 memory (eval-leak guard)");
}

#[test]
fn load_correction_memories_returns_captured_entries() {
    let db = make_db();
    let mut seg = make_segment("lm-1", "/data/audio/lm-1.wav");
    seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
    db.insert_segment(&seg).expect("insert");
    ensure_test_audio_content_hash(&db, "lm-1");
    db.record_human_decision("lm-1", "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");

    let mems = db.load_correction_memories().expect("load");
    assert_eq!(mems.len(), 1);
    assert_eq!(mems[0].wrong_token, "باش");
    assert_eq!(mems[0].human_token, "خراپ");
    assert!(
        (mems[0].confidence - 0.5).abs() < 1e-9,
        "a freshly captured memory starts at the Beta(1,1) prior 0.5 (no firing-outcome evidence yet)"
    );
    assert_eq!(mems[0].hit_count, 0);
}

#[test]
fn loop0_round_trips_capture_to_fire_through_the_database() {
    // The whole LOOP 0 minus the live-decode wiring. The same confusion is corrected on TWO
    // segments so hit_count reaches 1 and clears the anti-one-off guard (a single correction,
    // hit_count 0, deliberately does NOT fire — covered by unconfirmed_memory_does_not_fire).
    let db = make_db();
    for id in ["lm-2a", "lm-2b"] {
        let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
        seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&seg).expect("insert");
        ensure_test_audio_content_hash(&db, id);
        db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
    }

    let mems = db.load_correction_memories().expect("load");
    assert_eq!(mems.len(), 1, "the repeated correction upserts to a single memory");
    assert_eq!(mems[0].hit_count, 1, "confirmed twice -> hit_count 1, past the anti-one-off guard");

    let out =
        crate::corrections::apply_memories("ئەو ساڵە باش بوو", &mems, &crate::corrections::FiringConfig::default());
    assert_eq!(out, "ئەو ساڵە خراپ بوو", "capture x2 -> DB -> load -> fire reproduces the human fix");
}

/// Confirming edits of the SAME confusion must lift a memory's evidence-based confidence from the
/// neutral prior up past tau_conf — the "a confirmed memory's confidence rises" half of the audit fix.
#[test]
fn confirmed_memory_confidence_rises_above_tau_conf() {
    let db = make_db();
    let tau = crate::corrections::FiringConfig::default().tau_conf;

    // The first edit CAPTURES the memory at the 0.5 prior — below tau_conf, so it cannot fire yet.
    let mut seg0 = make_segment("cf-0", "/data/audio/cf-0.wav");
    seg0.raw_transcript = "ئەو ساڵە باش بوو".to_string();
    db.insert_segment(&seg0).expect("insert");
    ensure_test_audio_content_hash(&db, "cf-0");
    db.record_human_decision("cf-0", "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
    let fresh = db.load_correction_memories().expect("load")[0].confidence;
    assert!(fresh < tau, "a freshly captured memory sits at the 0.5 prior, below tau_conf: {fresh}");

    // Each further human edit of the same confusion is an independent confirmation -> confidence climbs.
    for id in ["cf-1", "cf-2"] {
        let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
        seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&seg).expect("insert");
        ensure_test_audio_content_hash(&db, id);
        db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
    }
    let confirmed = db.load_correction_memories().expect("load")[0].confidence;
    assert!(confirmed > tau, "confirming edits must raise confidence above tau_conf: {confirmed}");
}

/// A memory that first earns confidence, then repeatedly over-triggers on drafts the human ACCEPTS
/// as-is, must decay back below tau_conf — the anti-poisoning "one bad memory decays" half of the fix.
#[test]
fn overridden_memory_confidence_decays_below_tau_conf() {
    let db = make_db();
    let tau = crate::corrections::FiringConfig::default().tau_conf;

    // Earn confidence with confirming edits of the same confusion (distinct segments).
    for id in ["bad-1", "bad-2", "bad-3"] {
        let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
        seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&seg).expect("insert");
        ensure_test_audio_content_hash(&db, id);
        db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
    }
    let confident = db.load_correction_memories().expect("load")[0].confidence;
    assert!(confident > tau, "after confirming edits the memory clears tau_conf: {confident}");

    // Now humans keep ACCEPTING the original in that slot -> every would-fire is an over-trigger.
    for id in ["ovr-1", "ovr-2", "ovr-3"] {
        let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
        seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&seg).expect("insert");
        db.record_human_decision(id, "accept", None, None).expect("accept");
    }
    let decayed = db.load_correction_memories().expect("load")[0].confidence;
    assert!(decayed < tau, "repeated over-triggers must decay confidence below tau_conf: {decayed}");
}

/// A confirm/override event stamps `last_fired_at` (previously never written) and increments the
/// firing-outcome counters; a gold segment's decision must NOT touch either (eval-leak guard).
#[test]
fn confidence_evidence_stamps_last_fired_at_and_skips_gold() {
    let db = make_db();
    // Capture on the first edit, then a second same-confusion edit lands a confirm.
    for id in ["lf-1", "lf-2"] {
        let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
        seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&seg).expect("insert");
        ensure_test_audio_content_hash(&db, id);
        db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
    }
    let (fired_set, confirms): (i64, i64) = db
        .connection()
        .query_row(
            "SELECT last_fired_at IS NOT NULL, confirm_count
               FROM effective_correction_memory_v60 WHERE wrong_token = 'باش'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("query");
    assert_eq!(fired_set, 1, "a confirm event stamps last_fired_at");
    assert_eq!(confirms, 1, "the second same-confusion edit records exactly one confirm");

    // A gold segment whose text the memory would fire on must leave the evidence untouched.
    let mut gold = make_segment("lf-gold", "/data/audio/lf-gold.wav");
    gold.raw_transcript = "ئەو ساڵە باش بوو".to_string();
    db.insert_segment(&gold).expect("insert");
    db.connection().execute("UPDATE speech_segments SET is_gold = 1 WHERE id = 'lf-gold'", []).expect("mark gold");
    db.record_human_decision("lf-gold", "accept", None, None).expect("accept");
    let overrides: i64 = db
        .connection()
        .query_row("SELECT override_count FROM effective_correction_memory_v60 WHERE wrong_token = 'باش'", [], |r| {
            r.get(0)
        })
        .expect("query");
    assert_eq!(overrides, 0, "a gold-segment decision must not update firing-outcome evidence (eval-leak guard)");
}

#[test]
fn loop0_shadow_log_records_would_fire_flag() {
    // P1.3: each shadow observation persists a row with its memory_fired flag (the C5 data source).
    let db = make_db();
    db.insert_segment(&make_segment("sh-1", "/data/audio/sh-1.wav")).expect("insert");
    db.record_loop0_shadow("sh-1", true).expect("shadow true");
    db.record_loop0_shadow("sh-1", false).expect("shadow false");

    let rows: Vec<i64> = db
        .connection()
        .prepare("SELECT memory_fired FROM loop0_shadow_log WHERE segment_id = 'sh-1' ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows, vec![1, 0], "both shadow observations persist with their flags");
}

#[test]
fn a_segment_with_loop0_over_trigger_evidence_cannot_be_deleted() {
    // C5 survivor-bias guard: cleanup must not erase the over-trigger evidence that the gate reads —
    // otherwise the gate would look safer than reality. Schema v60 therefore refuses the delete and
    // preserves both the reviewed segment and its durable observation.
    let db = make_db();
    let mut seg = make_segment("ov-1", "/data/audio/ov-1.wav");
    seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
    db.insert_segment(&seg).expect("insert");
    // Shadow says a memory WOULD fire; the human then accepted the original -> an OVER-TRIGGER.
    db.record_loop0_shadow("ov-1", true).expect("shadow");
    db.connection().execute("UPDATE speech_segments SET human_decision='accept' WHERE id='ov-1'", []).expect("hd");

    let ot_before =
        db.intelligence_report().expect("report")["loop0Shadow"]["firedButHumanAcceptedOriginal"].as_i64().unwrap();
    assert_eq!(ot_before, 1, "the over-trigger is counted while the segment exists");

    let err = db.delete_segment("ov-1").expect_err("review evidence must make the segment append-only");
    assert!(err.to_string().contains("durable review authority"), "unexpected refusal: {err}");
    assert!(db.get_segment_by_id("ov-1").unwrap().is_some(), "the reviewed segment remains authoritative");

    let ot_after =
        db.intelligence_report().expect("report")["loop0Shadow"]["firedButHumanAcceptedOriginal"].as_i64().unwrap();
    assert_eq!(ot_after, 1, "the refused delete cannot erase the over-trigger evidence");
}

#[test]
fn restore_rejects_a_corrupt_source_before_overwriting_the_live_db() {
    let tmp = tempfile::TempDir::new().unwrap();
    let live_path = tmp.path().join("live.db");
    let mut live = Database::open(live_path.to_str().unwrap()).unwrap();
    live.initialize().unwrap();

    // A healthy snapshot restores fine.
    let good_path = tmp.path().join("good.db");
    {
        let good = Database::open(good_path.to_str().unwrap()).unwrap();
        good.initialize().unwrap();
    }
    live.restore(&good_path).expect("a healthy snapshot restores");

    // A garbage source is REJECTED (integrity/open failure) — no partial overwrite of the live DB.
    let bad_path = tmp.path().join("bad.db");
    std::fs::write(&bad_path, b"this is not a sqlite database at all").unwrap();
    assert!(live.restore(&bad_path).is_err(), "a corrupt snapshot must be rejected");
    assert!(live.segment_count().is_ok(), "the live database is still usable after a rejected restore");
}

#[test]
fn restore_staging_does_not_create_sidecars_beside_a_frozen_snapshot() {
    let tmp = tempfile::TempDir::new().unwrap();
    let snapshot_path = tmp.path().join("manifest-bound snapshot ڕ.db");
    {
        let snapshot = Database::open(snapshot_path.to_str().unwrap()).unwrap();
        snapshot.initialize().unwrap();
        snapshot.insert_segment(&make_segment("frozen-source", "/frozen.wav")).unwrap();
        snapshot.wal_checkpoint().unwrap();
    }
    let wal = sqlite_sidecar_path(&snapshot_path, "-wal");
    let shm = sqlite_sidecar_path(&snapshot_path, "-shm");
    let _ = std::fs::remove_file(&wal);
    let _ = std::fs::remove_file(&shm);
    assert!(!wal.exists() && !shm.exists(), "fixture must start as one manifest-bound DB file");

    let staged = Database::stage_restore_source(&snapshot_path).expect("frozen snapshot stages");
    assert!(staged.get_segment_by_id("frozen-source").unwrap().is_some());
    assert!(!wal.exists(), "read-only restore preflight must not create a WAL beside its source");
    assert!(!shm.exists(), "read-only restore preflight must not create shared memory beside its source");
}

#[test]
fn restore_refuses_a_snapshot_from_a_newer_schema_without_clobbering_the_live_db() {
    // Restore copies pages directly, bypassing run_migrations' startup forward-compat guard. A
    // snapshot at a schema NEWER than this build must be refused, or the app would silently operate a
    // future schema with stale semantics — and the current library must survive the refusal intact.
    let tmp = tempfile::TempDir::new().unwrap();

    let live_path = tmp.path().join("live.db");
    let mut live = Database::open(live_path.to_str().unwrap()).unwrap();
    live.initialize().unwrap();
    live.insert_segment(&make_segment("keep-me", "/a.wav")).unwrap();

    // A snapshot that is healthy but records a schema version one past what this build knows.
    let future_path = tmp.path().join("future.db");
    let future_version = crate::migrations::max_supported_version() + 1;
    {
        let future = Database::open(future_path.to_str().unwrap()).unwrap();
        future.initialize().unwrap();
        future
            .connection()
            .execute(
                "INSERT INTO schema_migrations (version, description, applied_at) \
                     VALUES (?1, 'from-a-newer-build', datetime('now'))",
                params![future_version],
            )
            .unwrap();
    }

    let err = live.restore(&future_path).unwrap_err();
    assert!(
        format!("{err}").contains("newer than this build supports"),
        "a newer-schema snapshot must be refused: {err}"
    );
    // The live library is untouched — the segment is still there.
    assert!(
        live.get_segment_by_id("keep-me").unwrap().is_some(),
        "refusing a newer-schema restore must NOT clobber the current library"
    );

    // A same-version snapshot still restores fine (the fence only blocks strictly-newer schemas).
    let same_path = tmp.path().join("same.db");
    {
        let same = Database::open(same_path.to_str().unwrap()).unwrap();
        same.initialize().unwrap();
    }
    live.restore(&same_path).expect("a current-schema snapshot must still restore");
}

#[test]
fn restore_refuses_incomplete_migration_history_without_clobbering_the_live_db() {
    let tmp = tempfile::TempDir::new().unwrap();
    let live_path = tmp.path().join("live-history.db");
    let mut live = Database::open(live_path.to_str().unwrap()).unwrap();
    live.initialize().unwrap();
    live.insert_segment(&make_segment("keep-history", "/keep.wav")).unwrap();

    let damaged_path = tmp.path().join("damaged-history.db");
    {
        let damaged = Database::open(damaged_path.to_str().unwrap()).unwrap();
        damaged.initialize().unwrap();
        damaged.connection().execute("DELETE FROM schema_migrations WHERE version = 23", []).unwrap();
    }

    let error = live.restore(&damaged_path).expect_err("an incomplete snapshot history must be refused");
    assert!(error.to_string().contains("missing=[23]"), "unexpected restore error: {error}");
    assert!(
        live.get_segment_by_id("keep-history").unwrap().is_some(),
        "restore preflight must reject before overwriting any live page"
    );
}

#[test]
fn restore_stages_pending_migrations_and_foreign_keys_before_overwriting_live_data() {
    let tmp = tempfile::TempDir::new().unwrap();
    let live_path = tmp.path().join("live-staged.db");
    let mut live = Database::open(live_path.to_str().unwrap()).unwrap();
    live.initialize().unwrap();
    live.insert_segment(&make_segment("keep-staged", "/keep.wav")).unwrap();

    // A valid v57 history with an unauthorized orphan makes pending v58 fail. That failure must
    // happen in the isolated candidate, before a single live page is replaced.
    let migration_fail_path = tmp.path().join("migration-fail.db");
    {
        let candidate = Database::open(migration_fail_path.to_str().unwrap()).unwrap();
        candidate.initialize().unwrap();
        assert_eq!(crate::migrations::rollback(&candidate, 10).unwrap(), vec![67, 66, 65, 64, 63, 62, 61, 60, 59, 58]);
        candidate
            .connection()
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 INSERT INTO segment_hypotheses
                    (segment_id, model_id, transcript, model_version_id)
                 VALUES ('unauthorized-orphan', 'unknown-model', 'x', 'unknown-model');
                 PRAGMA foreign_keys=ON;",
            )
            .unwrap();
    }
    let migration_error =
        live.restore(&migration_fail_path).expect_err("a candidate whose pending migration fails must be refused");
    assert!(
        migration_error.to_string().contains("migration v58 source set"),
        "unexpected staged migration error: {migration_error}"
    );
    assert!(live.get_segment_by_id("keep-staged").unwrap().is_some());

    // A HEAD snapshot has no pending migration to expose an invalid FK, so the staged post-migration
    // foreign-key check is independently load-bearing.
    let fk_fail_path = tmp.path().join("fk-fail.db");
    {
        let candidate = Database::open(fk_fail_path.to_str().unwrap()).unwrap();
        candidate.initialize().unwrap();
        candidate
            .connection()
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 INSERT INTO segment_hypotheses
                    (segment_id, model_id, transcript, model_version_id)
                 VALUES ('head-orphan', 'unknown-model', 'x', 'unknown-model');
                 PRAGMA foreign_keys=ON;",
            )
            .unwrap();
    }
    let fk_error = live.restore(&fk_fail_path).expect_err("a HEAD candidate with broken FKs must be refused");
    assert!(fk_error.to_string().contains("foreign-key violation"), "unexpected FK error: {fk_error}");
    assert!(
        live.get_segment_by_id("keep-staged").unwrap().is_some(),
        "every failed staged restore must preserve the original live database"
    );

    // Exact migration rows are still not a schema: a required table can be dropped while SQLite
    // integrity and FK checks remain green. The current-release schema contract must catch that too.
    let schema_fail_path = tmp.path().join("schema-fail.db");
    {
        let candidate = Database::open(schema_fail_path.to_str().unwrap()).unwrap();
        candidate.initialize().unwrap();
        candidate.connection().execute("DROP TABLE jobs", []).unwrap();
    }
    let schema_error =
        live.restore(&schema_fail_path).expect_err("a HEAD candidate missing a required schema object must be refused");
    let schema_message = schema_error.to_string();
    assert!(
        schema_message.contains("missing=") && schema_message.contains("\"jobs\""),
        "unexpected schema error: {schema_error}"
    );
    assert!(live.get_segment_by_id("keep-staged").unwrap().is_some());
}

#[test]
fn restore_of_an_older_snapshot_migrates_it_forward_to_head() {
    // Restore copies pages directly, so an OLDER snapshot would leave the live DB behind HEAD and the
    // running app would hit "no such column/table" until the next startup. restore() must re-migrate.
    let tmp = tempfile::TempDir::new().unwrap();
    let head = crate::migrations::max_supported_version();

    let live_path = tmp.path().join("live.db");
    let mut live = Database::open(live_path.to_str().unwrap()).unwrap();
    live.initialize().unwrap();

    // Synthesize a genuinely OLD snapshot: roll back to BEFORE the jobs migration (v37 created the
    // `jobs` table) by deleting every migration record from v37 up AND reverting the SCHEMA those
    // migrations produced — so it looks like it came from a build behind v37. Keyed on the
    // jobs-migration version (37), NOT HEAD, so adding newer migrations (e.g. the v38 STRICT pilot)
    // can't decouple "rolled-back version" from "dropped table": re-migration on restore must re-run
    // v37 (recreating jobs) and everything after.
    //
    // The schema revert must cover EVERY un-recorded migration whose up_sql is not re-runnable
    // against an already-migrated schema. v37 (jobs) and v38 (STRICT recreate: CREATE->copy->drop->
    // rename) both re-run fine, but v39's `RENAME COLUMN ood_score -> signal_anomaly_score` does NOT:
    // re-running it on a schema that already renamed the column fails with "no such column:
    // ood_score". So the synthesis renames it back, making this a faithful pre-v39 snapshot rather
    // than a records-only fake.
    const JOBS_MIGRATION: i64 = 37;
    let old_path = tmp.path().join("old.db");
    {
        let old = Database::open(old_path.to_str().unwrap()).unwrap();
        old.initialize().unwrap();
        // First use the real reversible down paths for every schema newer than v56. Merely deleting
        // their version rows leaves v57's compensation columns/tables and v58's evidence archives in
        // place, which is not an old snapshot at all: replay then (correctly) fails on duplicate schema.
        // These migrations are safely reversible on this intentionally empty fixture. v56 is handled
        // explicitly in the pre-v37 schema synthesis below, alongside the older rename migrations.
        let post_v56 = crate::migrations::MIGRATIONS.iter().filter(|migration| migration.version > 56).count();
        let reverted = crate::migrations::rollback(&old, post_v56).unwrap();
        let expected_reverted: Vec<i64> = crate::migrations::MIGRATIONS
            .iter()
            .rev()
            .filter(|migration| migration.version > 56)
            .map(|migration| migration.version)
            .collect();
        assert_eq!(reverted, expected_reverted);
        assert_eq!(crate::migrations::get_current_version(&old).unwrap(), 56);
        old.connection()
            .execute_batch(&format!(
                // Same reasoning as the ood_score rename directly above, for v52: a genuine pre-v37
                // snapshot carries `agent_confidence`, because v52 (which renames it to
                // agreement_score) had not run yet. Without renaming it back, this synthesis produces
                // a database with HEAD's column names and an old version number — and the replay then
                // fails inside v40, whose INSERT…SELECT names `agent_confidence` explicitly. That
                // failure would be an artifact of the synthesis, not of the upgrade path: a REAL old
                // snapshot replays v40 while the column still has its old name, and only reaches v52
                // afterwards.
                "DROP TABLE IF EXISTS jobs; \
                     ALTER TABLE speech_segments RENAME COLUMN signal_anomaly_score TO ood_score; \
                     ALTER TABLE speech_segments RENAME COLUMN agreement_score TO agent_confidence; \
                     ALTER TABLE review_events DROP COLUMN duration_ms; \
                     DELETE FROM schema_migrations WHERE version >= {JOBS_MIGRATION};"
            ))
            .unwrap();
        let old_ver = crate::migrations::get_current_version(&old).unwrap();
        assert!(old_ver < head, "the synthesized snapshot must be behind HEAD (got v{old_ver}, head v{head})");
        let has_jobs: i64 = old
            .connection()
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='jobs'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(has_jobs, 0, "the old snapshot must genuinely lack the v37 jobs table");
        let pre_v39_col: i64 = old
            .connection()
            .query_row("SELECT COUNT(*) FROM pragma_table_info('speech_segments') WHERE name='ood_score'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(pre_v39_col, 1, "the old snapshot must genuinely carry the pre-v39 ood_score column");
    }

    live.restore(&old_path).expect("an older snapshot restores");

    // After restore the live DB is migrated forward to HEAD and the v37 table exists + is usable.
    assert_eq!(
        crate::migrations::get_current_version(&live).unwrap(),
        head,
        "a restored older snapshot must be migrated up to HEAD in place"
    );
    let has_jobs: i64 = live
        .connection()
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='jobs'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(has_jobs, 1, "the post-restore migration must recreate the v37 jobs table");
    live.create_or_get_job("after-restore", "export_dataset", None, None)
        .expect("the migrated-in jobs table must be usable right after restore");

    // Idempotent: restoring an already-HEAD snapshot re-migrates as a no-op and still succeeds.
    let head_path = tmp.path().join("head.db");
    {
        let h = Database::open(head_path.to_str().unwrap()).unwrap();
        h.initialize().unwrap();
    }
    live.restore(&head_path).expect("a HEAD snapshot restores idempotently");
    assert_eq!(crate::migrations::get_current_version(&live).unwrap(), head);
}

#[test]
fn audio_health_detects_missing_and_relink_repoints_by_basename() {
    // P3.3: a moved/renamed source file becomes "missing"; pointing relink at the new folder
    // repoints every segment on that path (basename match).
    let tmp = tempfile::TempDir::new().unwrap();
    let src = tmp.path().join("clip.wav");
    std::fs::write(&src, b"audio").unwrap();
    let db = make_db();
    for id in ["a", "b"] {
        db.insert_segment(&make_segment(id, &src.to_string_lossy())).unwrap();
    }
    assert_eq!(db.audio_health().unwrap().missing_files, 0, "present file -> healthy");

    // Owner reorganizes: move the file to a new folder (same name).
    let newdir = tmp.path().join("moved");
    std::fs::create_dir(&newdir).unwrap();
    let moved = newdir.join("clip.wav");
    std::fs::rename(&src, &moved).unwrap();

    let health = db.audio_health().unwrap();
    assert_eq!(health.total_files, 1, "two segments, one distinct source file");
    assert_eq!(health.missing_files, 1, "the moved file is missing");

    let result = db.relink_audio(&newdir).unwrap();
    assert_eq!(result.relinked, 1);
    assert_eq!(result.still_missing, 0);
    assert_eq!(db.audio_health().unwrap().missing_files, 0, "relinked -> healthy");
    assert_eq!(
        db.get_segment_by_id("a").unwrap().unwrap().audio_path,
        moved.to_string_lossy().to_string(),
        "both segments repointed to the found file"
    );
}

#[test]
fn relink_leaves_unmatched_paths_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = make_db();
    db.insert_segment(&make_segment("x", "/gone/missing.wav")).unwrap();
    let result = db.relink_audio(tmp.path()).unwrap(); // no clip named missing.wav in the dir
    assert_eq!(result.relinked, 0);
    assert_eq!(result.still_missing, 1, "an unmatched missing path stays missing");
}

#[test]
fn relink_refuses_ambiguous_basename_collisions() {
    // P3.3 ambiguity guard: two DISTINCT missing sources share a basename. A single found file of
    // that name cannot be known to be the right one for both — relink must refuse both (leaving
    // them missing) rather than silently repoint segments to the WRONG recording's audio.
    let tmp = tempfile::TempDir::new().unwrap();
    let db = make_db();
    db.insert_segment(&make_segment("from_a", "/old/folderA/interview.wav")).unwrap();
    db.insert_segment(&make_segment("from_b", "/old/folderB/interview.wav")).unwrap();
    // The owner points relink at a folder that happens to contain ONE interview.wav.
    std::fs::write(tmp.path().join("interview.wav"), b"audio").unwrap();

    let result = db.relink_audio(tmp.path()).unwrap();
    assert_eq!(result.relinked, 0, "ambiguous basename -> nothing relinked");
    assert_eq!(result.still_missing, 2, "both colliding sources stay missing, not mis-linked");
    // Neither segment was repointed — the original (missing) paths are untouched.
    assert_eq!(db.get_segment_by_id("from_a").unwrap().unwrap().audio_path, "/old/folderA/interview.wav");
    assert_eq!(db.get_segment_by_id("from_b").unwrap().unwrap().audio_path, "/old/folderB/interview.wav");
}

#[test]
fn sorani_paths_and_fts_search_are_robust() {
    // P3.7: a real Sorani corpus has Arabic-script filenames with spaces and long paths. Storage,
    // round-trip, and (transcript-scoped) FTS search must all survive them.
    let db = make_db();
    let long_dir = format!("D:/media/کوردی ساؤند سامپڵز/{}", "پارچە ".repeat(40));
    let audio_path = format!("{long_dir}/گەشتی مێژوویی.wav");
    assert!(audio_path.chars().count() > 200, "path is >200 chars: {}", audio_path.chars().count());
    let mut seg = make_segment("sr-1", &audio_path);
    seg.raw_transcript = "ئەمە دەقێکی کوردی گرنگە".to_string();
    db.insert_segment(&seg).unwrap();

    // The long non-ASCII path round-trips byte-identical.
    let got = db.get_segment_by_id("sr-1").unwrap().unwrap();
    assert_eq!(got.audio_path, audio_path, "non-ASCII long path round-trips intact");
    assert_eq!(got.raw_transcript, "ئەمە دەقێکی کوردی گرنگە");

    // FTS finds the segment by a Sorani transcript word (unicode61 tokenizer).
    let by_transcript = db.search_segments("کوردی").unwrap();
    assert!(by_transcript.iter().any(|s| s.id == "sr-1"), "FTS finds the Sorani segment by transcript content");

    // A token present only in the PATH (not the transcript) must NOT match — search is
    // transcript-scoped, so a folder name never yields a false positive.
    let by_path_token = db.search_segments("سامپڵز").unwrap();
    assert!(!by_path_token.iter().any(|s| s.id == "sr-1"), "a path-only token does not match");
}

#[test]
fn begin_import_job_is_atomic_reap_never_survives_a_failed_insert() {
    // Write-path audit (Week 2): reap + INSERT + retention are one invariant. Fault-inject the
    // INSERT with a RAISE trigger: the reap must ROLL BACK — a prior crash must never be marked
    // 'abandoned' without the new running job that justified abandoning it (the resume prompt
    // would otherwise find nothing to offer).
    let db = make_db();
    let crashed = db.begin_import_job("C:/audio/crashed", 4).unwrap();
    // Simulate the crash: the job stays 'running' (a real crash never completes it).
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_import_insert BEFORE INSERT ON import_jobs
                 BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .unwrap();

    let result = db.begin_import_job("C:/audio/new", 2);
    assert!(result.is_err(), "a failed job INSERT must fail the whole begin");

    // The crashed job must still be 'running' — reap rolled back — so the resume prompt still works.
    let found = db.find_interrupted_import_job().unwrap().expect("crashed job still resumable");
    assert_eq!(found.id, crashed, "the prior crash must remain the resumable interruption");
}

#[test]
fn transition_job_rejects_a_concurrently_changed_state() {
    // Write-path audit (Week 2): the UPDATE is now a compare-and-swap on the validated state.
    // HONEST SCOPE: the CAS's own branch (a flip landing BETWEEN the fn's read and its UPDATE) is
    // not injectable single-threaded through the public fn — this test flips the state BEFORE the
    // call, which the fn's fresh read catches in the validation branch instead. It still pins the
    // end-to-end contract the CAS also serves: a state changed out from under a caller is rejected
    // with no silent write. The CAS WHERE-clause itself is structural (state = the validated value).
    let db = make_db();
    let job = db.create_or_get_job("drill-job", "import", None, Some(10)).unwrap();
    db.transition_job(&job.id, crate::jobs::JobState::Running, None).unwrap();
    // "The other connection" cancels it via raw SQL (bypassing the API, as a racer effectively would).
    db.conn.execute("UPDATE jobs SET state = 'cancelled' WHERE id = ?1", params![job.id]).unwrap();
    // A stale-validated edge (running -> succeeded) must now be rejected...
    let err = db.transition_job(&job.id, crate::jobs::JobState::Succeeded, None).unwrap_err();
    assert!(err.to_string().contains("cancelled") || err.to_string().contains("illegal"), "{err}");
    // ...and the cancelled state must be untouched.
    assert_eq!(db.get_job(&job.id).unwrap().unwrap().state, crate::jobs::JobState::Cancelled);
}

#[test]
fn import_journal_records_progress_and_finds_interruption() {
    // P3.2: a running job with some files done is the interruption to resume; completing clears it.
    let db = make_db();
    assert!(db.find_interrupted_import_job().unwrap().is_none(), "no job -> no interruption");

    let job = db.begin_import_job("C:/audio", 3).unwrap();
    db.mark_import_file_done(&job, "C:/audio/a.wav").unwrap();
    db.mark_import_file_done(&job, "C:/audio/b.wav").unwrap();
    db.mark_import_file_done(&job, "C:/audio/a.wav").unwrap(); // idempotent

    let found = db.find_interrupted_import_job().unwrap().unwrap();
    assert_eq!(found.id, job);
    assert_eq!(found.dir, "C:/audio");
    assert_eq!(found.total_files, 3);
    assert_eq!(found.completed_paths.len(), 2, "idempotent mark -> 2 distinct completed files");

    db.complete_import_job(&job).unwrap();
    assert!(db.find_interrupted_import_job().unwrap().is_none(), "completed job is not an interruption");
}

#[test]
fn begin_import_job_reaps_prior_running_crashes() {
    // P3.2 robustness: a crash leaves a job 'running' forever. When a new import begins, that stale
    // crash must be reaped so (a) it can't accumulate across repeated crashes and (b) resuming the
    // NEW job never leaves an old crash lurking to prompt a spurious resume later.
    let db = make_db();
    let crashed = db.begin_import_job("C:/audio/first", 5).unwrap();
    db.mark_import_file_done(&crashed, "C:/audio/first/a.wav").unwrap();
    // No complete/discard — simulate a crash. A second import starts.
    let fresh = db.begin_import_job("C:/audio/second", 2).unwrap();

    // Exactly one 'running' job now, and it is the fresh one — the crash was reaped, not surfaced.
    let running: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM import_jobs WHERE status = 'running'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(running, 1, "only the active import is 'running'; the prior crash is abandoned");
    let found = db.find_interrupted_import_job().unwrap().unwrap();
    assert_eq!(found.id, fresh, "the interruption is the fresh import, never the reaped crash");
    assert_eq!(found.dir, "C:/audio/second");
}

#[test]
fn resume_handoff_copy_failure_rolls_back_to_the_original_running_journal() {
    // Deterministic equivalent of a kill after the in-process resume claim and during successor
    // construction: fail the completed-path copy after the transaction has retired the old row and
    // inserted the new one. The savepoint must restore the exact old journal, not leave zero authority.
    let db = make_db();
    let crashed = db.begin_import_job("C:/audio", 2).unwrap();
    db.mark_import_file_done(&crashed, "C:/audio/a.wav").unwrap();
    db.connection()
        .execute_batch(
            "CREATE TRIGGER fail_resume_journal_copy
             BEFORE INSERT ON import_job_files
             WHEN EXISTS (
                 SELECT 1 FROM import_jobs WHERE id = NEW.job_id AND status = 'running'
             )
             BEGIN SELECT RAISE(ABORT, 'injected successor copy failure'); END;",
        )
        .unwrap();

    let error = db.handoff_import_job_for_resume(&crashed).unwrap_err();
    assert!(error.to_string().contains("successor copy failure"), "unexpected fault: {error}");

    let recovered = db.find_interrupted_import_job().unwrap().expect("the original journal must survive");
    assert_eq!(recovered.id, crashed);
    assert_eq!(recovered.completed_paths, vec!["C:/audio/a.wav"]);
    let running: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM import_jobs WHERE status = 'running'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(running, 1, "a failed handoff must retain exactly one resumable journal");
}

#[test]
fn resume_handoff_survives_a_kill_before_worker_entry_without_duplicate_progress() {
    // Close and reopen immediately after the atomic handoff, before `continue_import_job` models the
    // worker entry. This is the exact process-kill seam that used to erase the old journal in the
    // command handler before the worker created its replacement.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("resume-kill.db");
    let successor;
    {
        let db = Database::open(path.to_string_lossy().as_ref()).unwrap();
        db.initialize().unwrap();
        let crashed = db.begin_import_job("C:/audio", 3).unwrap();
        db.mark_import_file_done(&crashed, "C:/audio/a.wav").unwrap();
        db.mark_import_file_done(&crashed, "C:/audio/b.wav").unwrap();
        successor = db.handoff_import_job_for_resume(&crashed).unwrap();

        let claimed = db.find_interrupted_import_job().unwrap().expect("successor is durable before spawn");
        assert_eq!(claimed.id, successor);
        assert_eq!(claimed.completed_paths.len(), 2);
        let running: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM import_jobs WHERE status = 'running'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(running, 1);
    }

    let reopened = Database::open(path.to_string_lossy().as_ref()).unwrap();
    let before_worker = reopened.find_interrupted_import_job().unwrap().expect("kill left a resumable successor");
    assert_eq!(before_worker.id, successor);
    assert_eq!(
        before_worker.completed_paths.iter().collect::<std::collections::HashSet<_>>().len(),
        2,
        "successor copy must not duplicate completed paths"
    );

    reopened.continue_import_job(&successor, "C:/audio", 4).unwrap();
    // Re-journaling an adopted file is idempotent, which is the no-duplicate resume path used after
    // source/champion authority independently proves that its segment rows should be skipped.
    reopened.mark_import_file_done(&successor, "C:/audio/a.wav").unwrap();
    let admitted = reopened.find_interrupted_import_job().unwrap().unwrap();
    assert_eq!(admitted.id, successor);
    assert_eq!(admitted.total_files, 4);
    assert_eq!(admitted.completed_paths.len(), 2, "worker entry/re-journal must not duplicate progress rows");
}

#[test]
fn discard_import_job_removes_it_and_its_files() {
    let db = make_db();
    let job = db.begin_import_job("C:/x", 1).unwrap();
    db.mark_import_file_done(&job, "C:/x/a.wav").unwrap();
    db.discard_import_job(&job).unwrap();
    assert!(db.find_interrupted_import_job().unwrap().is_none());
    let files: i64 = db.connection().query_row("SELECT COUNT(*) FROM import_job_files", [], |r| r.get(0)).unwrap();
    assert_eq!(files, 0, "discard removed the job's file rows");
}

#[test]
fn source_transcript_upsert_roundtrips_latest_reference() {
    let db = make_db();
    let first = SourceTranscriptRecord {
        audio_path: "/audio/long.wav".to_string(),
        model_id: "gemini-2.5-pro".to_string(),
        audio_content_hash: Some("hash-v1".to_string()),
        audio_size_bytes: Some(123),
        transcript_path: "/refs/long.txt".to_string(),
        transcript_text: "first transcript".to_string(),
        created_at: None,
    };
    db.upsert_source_transcript(&first).expect("insert source transcript");

    let mut second = first.clone();
    second.transcript_path = "/refs/long-v2.txt".to_string();
    second.transcript_text = "improved transcript".to_string();
    db.upsert_source_transcript(&second).expect("update source transcript");

    let loaded = db
        .get_source_transcript("/audio/long.wav", "gemini-2.5-pro")
        .expect("load source transcript")
        .expect("source transcript exists");
    assert_eq!(loaded.transcript_path, "/refs/long-v2.txt");
    assert_eq!(loaded.transcript_text, "improved transcript");
    assert_eq!(loaded.audio_content_hash.as_deref(), Some("hash-v1"));
    assert_eq!(loaded.audio_size_bytes, Some(123));

    let latest = db
        .get_latest_source_transcript_for_audio("/audio/long.wav")
        .expect("load latest source transcript")
        .expect("latest source transcript exists");
    assert_eq!(latest.model_id, "gemini-2.5-pro");
    assert_eq!(latest.transcript_text, "improved transcript");

    let flash = SourceTranscriptRecord {
        audio_path: "/audio/long.wav".to_string(),
        model_id: "gemini-2.5-flash".to_string(),
        audio_content_hash: Some("hash-v1".to_string()),
        audio_size_bytes: Some(123),
        transcript_path: "/refs/long-flash.txt".to_string(),
        transcript_text: "flash transcript".to_string(),
        created_at: None,
    };
    db.upsert_source_transcript(&flash).expect("insert second model source transcript");

    let all = db.get_source_transcripts_for_audio("/audio/long.wav").expect("load all source transcripts");
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|record| record.model_id == "gemini-2.5-pro"));
    assert!(all.iter().any(|record| record.model_id == "gemini-2.5-flash"));
}

#[test]
fn escalation_write_with_no_confidence_preserves_the_persisted_irt_confidence() {
    // True-10 audit 2026-07-09 (suspect-first regression): run_t0_gate persists the real IRT
    // confidence on an escalated verdict, then the T1/T2 escalation paths (cloud-off,
    // audio-prep failure, no-majority) re-write "escalated" with agreement_score=None moments
    // later. The unconditional overwrite NULLed the confidence and both suspect-first orderings
    // (COALESCE(agreement_score, 0.5)) collapsed to recency. COALESCE in the UPDATE must keep
    // the earlier signal; a caller WITH a signal must still win.
    let db = make_db();
    db.insert_segment(&make_segment("esc", "/audio/esc.wav")).unwrap();
    db.write_legacy_machine_verdict_for_test("esc", "escalated", None, None, None, Some(0.83), true).unwrap();
    db.write_legacy_machine_verdict_for_test("esc", "escalated", None, Some("cloud off"), None, None, true).unwrap();
    let seg = db.get_segment_by_id("esc").unwrap().unwrap();
    assert_eq!(seg.agreement_score, Some(0.83), "a None re-write must not destroy the IRT confidence");
    // A later write that CARRIES a signal still replaces it.
    db.write_legacy_machine_verdict_for_test("esc", "escalated", None, None, None, Some(0.41), true).unwrap();
    let seg = db.get_segment_by_id("esc").unwrap().unwrap();
    assert_eq!(seg.agreement_score, Some(0.41));
}

#[test]
fn c4_precision_refuses_deleting_a_contradicted_auto_accept() {
    // A reviewed clip and the machine decision it contradicted are durable authority. Refusing both
    // single and batch deletion keeps the C4 numerator and denominator from drifting optimistically.
    let db = make_db();
    db.insert_segment(&make_segment("good", "/audio/g.wav")).unwrap();
    db.insert_segment(&make_segment("bad", "/audio/b.wav")).unwrap();
    // Two T0 auto-accepts; the human confirms one and contradicts the other.
    db.write_legacy_machine_verdict_for_test("good", "auto_accept", Some("ok"), None, None, Some(0.9), false).unwrap();
    db.write_legacy_machine_verdict_for_test("bad", "auto_accept", Some("wrong"), None, None, Some(0.9), false)
        .unwrap();
    db.record_human_decision("good", "accept", None, None).unwrap();
    db.record_human_decision("bad", "reject", None, None).unwrap();

    let before = db.intelligence_report().unwrap();
    assert_eq!(before["autoAcceptPrecision"]["t0HumanContradicted"], 1);
    assert_eq!(before["autoAcceptPrecision"]["t0HumanConfirmed"], 1);

    let err = db.delete_segment("bad").expect_err("contradicted reviewed clip must be append-only");
    assert!(err.to_string().contains("durable review authority"), "unexpected refusal: {err}");

    let after = db.intelligence_report().unwrap();
    assert_eq!(
        after["autoAcceptPrecision"]["t0HumanContradicted"], 1,
        "the refused delete must not erase the contradiction (survivor bias)"
    );
    assert_eq!(after["autoAcceptPrecision"]["t0Accepts"], 2, "the T0 denominator survives too");
    let err = db
        .delete_segments_batch(&["good".to_string()])
        .expect_err("batch deletion must enforce the same durable-authority boundary");
    assert!(err.to_string().contains("durable review authority"), "unexpected refusal: {err}");
    let final_report = db.intelligence_report().unwrap();
    assert_eq!(final_report["autoAcceptPrecision"]["t0HumanConfirmed"], 1);
    assert_eq!(final_report["autoAcceptPrecision"]["t0Accepts"], 2);
    assert!(db.get_segment_by_id("good").unwrap().is_some());
    assert!(db.get_segment_by_id("bad").unwrap().is_some());
}

#[test]
fn duplicate_batch_ids_cannot_double_archive_loop0_or_c4_evidence() {
    let db = make_db();
    db.insert_segment(&make_segment("duplicate-evidence", "/audio/duplicate-evidence.wav")).unwrap();
    db.record_loop0_shadow("duplicate-evidence", true).unwrap();
    db.connection()
        .execute(
            "INSERT INTO decision_verdicts(segment_id, auto_accept_verdict, verdict_computed_at)
             VALUES ('duplicate-evidence', 'T0_ACCEPT', datetime('now'))",
            [],
        )
        .unwrap();

    let loop0_before: (i64, i64, i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT total_observations, would_fire, fired_human_accepted,
                    fired_human_edited, fired_human_rejected
               FROM loop0_evidence_archive WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    let c4_before: (i64, i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT t0_accepts, t1_escalations, t0_human_confirmed, t0_human_contradicted
               FROM c4_evidence_archive WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();

    let id = "duplicate-evidence".to_string();
    let error = db.delete_segments_batch(&[id.clone(), id]).expect_err("duplicate ids must fail before archival");
    assert!(error.to_string().contains("duplicate ids before evidence archival"), "unexpected refusal: {error}");
    assert!(db.get_segment_by_id("duplicate-evidence").unwrap().is_some());
    assert_eq!(
        db.connection()
            .query_row("SELECT COUNT(*) FROM loop0_shadow_log WHERE segment_id = 'duplicate-evidence'", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    assert_eq!(
        db.connection()
            .query_row("SELECT COUNT(*) FROM decision_verdicts WHERE segment_id = 'duplicate-evidence'", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    let loop0_after: (i64, i64, i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT total_observations, would_fire, fired_human_accepted,
                    fired_human_edited, fired_human_rejected
               FROM loop0_evidence_archive WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    let c4_after: (i64, i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT t0_accepts, t1_escalations, t0_human_confirmed, t0_human_contradicted
               FROM c4_evidence_archive WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(loop0_after, loop0_before, "a refused request cannot mutate LOOP-0 archive counters");
    assert_eq!(c4_after, c4_before, "a refused request cannot mutate C4 archive counters");
}

#[test]
fn shadow_metrics_count_distinct_segments_not_observations() {
    // True-10 audit 2026-07-09: a re-processed segment accumulates several shadow rows, but C5
    // reasons about distinct events — one clip, one human decision, at most one over-trigger.
    let db = make_db();
    db.insert_segment(&make_segment("re", "/audio/re.wav")).unwrap();
    db.record_loop0_shadow("re", true).unwrap();
    db.record_loop0_shadow("re", true).unwrap();
    db.record_loop0_shadow("re", false).unwrap();
    db.record_human_decision("re", "accept", None, None).unwrap();
    let report = db.intelligence_report().unwrap();
    assert_eq!(report["loop0Shadow"]["totalObservations"], 1, "one segment, not three rows");
    assert_eq!(report["loop0Shadow"]["wouldFire"], 1);
    assert_eq!(report["loop0Shadow"]["firedButHumanAcceptedOriginal"], 1, "one physical over-trigger event, not two");
    // The per-segment semantics survive attempted cleanup because the reviewed row is append-only.
    let err = db.delete_segment("re").expect_err("reviewed shadow evidence must refuse deletion");
    assert!(err.to_string().contains("durable review authority"), "unexpected refusal: {err}");
    let after = db.intelligence_report().unwrap();
    assert_eq!(after["loop0Shadow"]["firedButHumanAcceptedOriginal"], 1);
    assert_eq!(after["loop0Shadow"]["totalObservations"], 1);
}

#[test]
fn c3_calibration_count_excludes_human_rejected_verified_clips() {
    // A "mark bad" clip is verified=1 with human_decision='reject'/verdict='human_reject' and its
    // annotated_transcript intact. The C3 conformal-calibration progress count (verifiedWithReference)
    // must EXCLUDE it — matching is_human_rejected / export_dataset — or it overstates how close the user
    // is to the T0 auto-accept threshold by counting discarded bad-audio clips as calibration samples.
    let db = make_db();
    let mut good = make_segment("cal-good", "/audio/cg.wav");
    good.verified = true;
    good.annotated_transcript = Some("دەقی باش".to_string());
    good.snr_db = Some(20.0);
    db.insert_legacy_segment_fixture(&good).unwrap();
    let mut bad = make_segment("cal-bad", "/audio/cb.wav");
    bad.verified = true;
    bad.annotated_transcript = Some("دەقی خراپ".to_string());
    bad.snr_db = Some(20.0);
    db.insert_legacy_segment_fixture(&bad).unwrap();
    // markBad keeps verified=true and sets the reject decision.
    db.conn
        .execute("UPDATE speech_segments SET human_decision='reject', verdict='human_reject' WHERE id='cal-bad'", [])
        .unwrap();

    let report = db.intelligence_report().unwrap();
    let total: i64 = report["conformalCalibration"]["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["verifiedWithReference"].as_i64().unwrap())
        .sum();
    assert_eq!(total, 1, "only the accepted verified-with-reference clip counts; the rejected one is excluded");
}

// ── Durable jobs accessors (migration v37 + crate::jobs) ──
use crate::jobs::JobState;

#[test]
fn create_or_get_job_is_idempotent_on_the_key() {
    let db = make_db();
    let a = db.create_or_get_job("job-a", "import", Some("dedupe-1"), Some(10)).unwrap();
    assert_eq!(a.state, JobState::Queued);
    assert_eq!(a.total, Some(10));
    // Re-issuing the SAME key returns the ORIGINAL job (no duplicate row), even with a different id.
    let again = db.create_or_get_job("job-b", "import", Some("dedupe-1"), Some(99)).unwrap();
    assert_eq!(again.id, "job-a", "same key must return the first job, not create a second");
    assert_eq!(again.total, Some(10), "original params preserved");
    let count: i64 = db.conn.query_row("SELECT COUNT(*) FROM jobs WHERE kind='import'", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1, "the idempotent re-issue must not have inserted a second row");
}

#[test]
fn null_key_jobs_are_never_deduped() {
    let db = make_db();
    db.create_or_get_job("n1", "export", None, None).unwrap();
    db.create_or_get_job("n2", "export", None, None).unwrap();
    let count: i64 = db.conn.query_row("SELECT COUNT(*) FROM jobs WHERE kind='export'", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 2, "two null-key jobs must both persist");
}

#[test]
fn transition_job_enforces_the_lifecycle_and_stamps_times() {
    let db = make_db();
    db.create_or_get_job("j", "transcribe", None, None).unwrap();

    // Legal: queued -> running stamps started_at; progress updates land.
    db.transition_job("j", JobState::Running, None).unwrap();
    db.update_job_progress("j", 3, 0.3).unwrap();
    let mid = db.get_job("j").unwrap().unwrap();
    assert_eq!(mid.state, JobState::Running);
    assert_eq!(mid.completed, 3);
    assert!((mid.progress - 0.3).abs() < 1e-9);
    let started: Option<String> =
        db.conn.query_row("SELECT started_at FROM jobs WHERE id='j'", [], |r| r.get(0)).unwrap();
    assert!(started.is_some(), "started_at stamped on first running");

    // Legal: running -> failed records the error_code and stamps finished_at.
    db.transition_job("j", JobState::Failed, Some("MODEL_UNAVAILABLE")).unwrap();
    let done = db.get_job("j").unwrap().unwrap();
    assert_eq!(done.state, JobState::Failed);
    assert_eq!(done.error_code.as_deref(), Some("MODEL_UNAVAILABLE"));
    let finished: Option<String> =
        db.conn.query_row("SELECT finished_at FROM jobs WHERE id='j'", [], |r| r.get(0)).unwrap();
    assert!(finished.is_some(), "finished_at stamped on terminal");

    // Illegal: a terminal job cannot transition again — rejected, not written.
    let err = db.transition_job("j", JobState::Succeeded, None).unwrap_err();
    assert!(format!("{err}").contains("illegal job transition"), "double-complete must be rejected: {err}");
    assert_eq!(db.get_job("j").unwrap().unwrap().state, JobState::Failed, "state unchanged after illegal move");
}

#[test]
fn progress_is_clamped_to_the_check_range() {
    let db = make_db();
    db.create_or_get_job("p", "eval", None, None).unwrap();
    db.transition_job("p", JobState::Running, None).unwrap();
    // A caller passing 1.4 (e.g. off-by-one on the denominator) must not violate the CHECK constraint.
    db.update_job_progress("p", 7, 1.4).unwrap();
    assert!((db.get_job("p").unwrap().unwrap().progress - 1.0).abs() < 1e-9, "over-range progress clamps to 1.0");
    db.update_job_progress("p", 0, -0.5).unwrap();
    assert!((db.get_job("p").unwrap().unwrap().progress).abs() < 1e-9, "negative progress clamps to 0.0");
}

#[test]
fn orphaned_running_jobs_are_reaped_as_interrupted_on_startup() {
    let db = make_db();
    // Simulate a crash: one job left running, one already finished, one still queued.
    db.create_or_get_job("crashed", "import", None, None).unwrap();
    db.transition_job("crashed", JobState::Running, None).unwrap();
    db.create_or_get_job("clean", "import", None, None).unwrap();
    db.transition_job("clean", JobState::Running, None).unwrap();
    db.transition_job("clean", JobState::Succeeded, None).unwrap();
    db.create_or_get_job("waiting", "import", None, None).unwrap();

    let reaped = db.mark_orphaned_running_jobs_failed().unwrap();
    assert_eq!(reaped, 1, "only the still-running job is a crash residue");

    let crashed = db.get_job("crashed").unwrap().unwrap();
    assert_eq!(crashed.state, JobState::Failed);
    assert_eq!(crashed.error_code.as_deref(), Some("INTERRUPTED"));
    assert_eq!(db.get_job("clean").unwrap().unwrap().state, JobState::Succeeded, "finished job untouched");
    assert_eq!(db.get_job("waiting").unwrap().unwrap().state, JobState::Queued, "queued job untouched");
}

#[test]
fn get_job_returns_none_for_a_missing_id() {
    let db = make_db();
    assert!(db.get_job("does-not-exist").unwrap().is_none());
}

#[test]
fn run_tracked_marks_succeeded_and_returns_the_work_value() {
    let db = make_db();
    let out = db.run_tracked("t-ok", "export_dataset", "EXPORT_FAILED", |_d| Ok(42_i32)).unwrap();
    assert_eq!(out, 42);
    let job = db.get_job("t-ok").unwrap().unwrap();
    assert_eq!(job.state, JobState::Succeeded);
    assert!(job.error_code.is_none());
}

#[test]
fn run_tracked_marks_failed_with_code_and_propagates_the_original_error() {
    let db = make_db();
    let err = db
        .run_tracked::<()>("t-err", "export_dataset", "EXPORT_FAILED", |_d| {
            Err(AppError::Other("disk full while writing parquet".into()))
        })
        .unwrap_err();
    // The ORIGINAL work error propagates, not a job-bookkeeping error.
    assert!(format!("{err}").contains("disk full"), "must surface the work error: {err}");
    let job = db.get_job("t-err").unwrap().unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.error_code.as_deref(), Some("EXPORT_FAILED"));
}

#[test]
fn run_tracked_gives_work_a_usable_db_handle() {
    // The closure receives &Database so the bracketed op can actually touch the library.
    let db = make_db();
    db.insert_segment(&make_segment("seg-in-job", "/a.wav")).unwrap();
    let n =
        db.run_tracked("t-db", "count", "COUNT_FAILED", |d| Ok(d.get_segment_by_id("seg-in-job")?.is_some())).unwrap();
    assert!(n, "work closure could read the DB it was handed");
    assert_eq!(db.get_job("t-db").unwrap().unwrap().state, JobState::Succeeded);
}

#[test]
fn list_recent_jobs_returns_newest_first_and_respects_limit() {
    let db = make_db();
    for i in 0..3 {
        db.create_or_get_job(&format!("lj{i}"), "export_dataset", None, None).unwrap();
    }
    let all = db.list_recent_jobs(10).unwrap();
    assert_eq!(all.len(), 3);
    let limited = db.list_recent_jobs(2).unwrap();
    assert_eq!(limited.len(), 2, "limit honored");
}

#[test]
fn merge_dataset_json_v60_refuses_unbound_review_provenance_on_new_rows() {
    // A lossless renderer-owned reviewed-row import was legitimate before v60. It is now a provenance
    // bypass: no playback-bound decision/flag effect can authorize these fields. Refuse rather than
    // silently dropping the claimed human work or trusting it without immutable authority.
    let db = make_db();

    let incoming = vec![SpeechSegment {
        id: "merge-new-gold".to_string(),
        created_at: Some("2026-01-02 03:04:05".to_string()),
        audio_path: "/gold.wav".to_string(),
        raw_transcript: "دەقی زێڕین".to_string(),
        annotated_transcript: Some("دەقی زێڕینی ڕاستکراوە".to_string()),
        duration_ms: 1500,
        verified: true,
        verdict: Some("human_edit".to_string()),
        verdict_transcript: Some("دەقی زێڕینی ڕاستکراوە".to_string()),
        rationale: Some("reviewer corrected one word".to_string()),
        human_decision: Some("edit".to_string()),
        corrected_at: Some("2026-01-02 03:05:00".to_string()),
        is_gold: true,
        alignment_quality: Some("word_aligner".to_string()),
        ..SpeechSegment::default()
    }];
    let json = serde_json::to_string(&incoming).expect("serialize");
    let err = db.merge_dataset_json(&json).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("Dataset merge refused atomically"), "unexpected error: {message}");
    for field in [
        "annotatedTranscript",
        "verified",
        "verdict",
        "verdictTranscript",
        "rationale",
        "humanDecision",
        "correctedAt",
        "isGold",
    ] {
        assert!(message.contains(field), "error must identify rejected field {field}: {message}");
    }
    assert!(db.get_segment_by_id("merge-new-gold").unwrap().is_none(), "refused merge must create no row");
}

#[test]
fn consensus_batch_restamps_confidence_source_for_the_score_it_writes() {
    // PROVENANCE: update_segment_consensus_batch overwrites `confidence` with an IRT-consensus score,
    // but its SET list omitted `confidence_source` — so a row whose decoder wrote "real_posterior" kept
    // that tag on a number that is no longer a decoder posterior. conformal.rs branches on the exact
    // "real_posterior" token when counting calibration coverage, so the stale tag actively inflated the
    // real-posterior calibration count with IRT scores. The batch must stamp the source it wrote.
    let db = make_db();
    let mut seg = make_segment("cons-prov", "/a.wav");
    seg.confidence = Some(0.42);
    seg.confidence_source = Some("real_posterior".to_string());
    db.insert_segment(&seg).expect("insert");

    let changed = db
        .update_segment_consensus_batch(&[(
            "cons-prov".to_string(),
            "دەقی کۆدەنگی".to_string(),
            "دەقی کۆدەنگی".to_string(),
            0.87,
        )])
        .expect("consensus batch");
    assert_eq!(changed, 1);

    let row = db.get_segment_by_id("cons-prov").unwrap().unwrap();
    assert_eq!(row.confidence, Some(0.87), "the consensus confidence was written");
    assert_eq!(
        row.confidence_source.as_deref(),
        Some("irt_consensus"),
        "confidence_source must describe the number actually stored, not the pre-consensus decoder"
    );
}

#[test]
fn update_segment_alignment_writes_timings_and_quality_together() {
    // Timings + quality marker are one atomic UPDATE (single statement). Written separately, the
    // marker could fail after the timings committed — and quality.rs raises the review-risk reason
    // only when the marker is present, so unmarked heuristic timings read as trustworthy alignment.
    let db = make_db();
    let seg = make_segment("align-atomic", "/a.wav");
    db.insert_segment(&seg).expect("insert");

    db.update_segment_alignment("align-atomic", r#"{"words":[]}"#, "energy_heuristic").expect("update");

    let row = db.get_segment_by_id("align-atomic").unwrap().unwrap();
    assert_eq!(row.alignment_json.as_deref(), Some(r#"{"words":[]}"#));
    assert_eq!(
        row.alignment_quality.as_deref(),
        Some("energy_heuristic"),
        "the quality marker must land with the timings it describes"
    );
}

#[test]
fn every_segment_write_rejects_structurally_invalid_alignment_metadata() {
    let db = make_db();
    let mut seg = make_segment("bad-alignment", "/a.wav");
    seg.alignment_json = Some(r#"{"source_start_ms":1000,"source_end_ms":100}"#.into());
    assert!(db.insert_segment(&seg).is_err(), "the shared insert boundary must reject reversed bounds");
    assert!(db.get_segment_by_id("bad-alignment").unwrap().is_none(), "a rejected row must not persist");

    let valid = make_segment("alignment-update", "/a.wav");
    db.insert_segment(&valid).unwrap();
    assert!(
        db.update_segment_alignment(
            "alignment-update",
            r#"{"words":[{"word":"x","start":2.0,"end":1.0,"confidence":0.9}]}"#,
            "ctc_forced",
        )
        .is_err(),
        "the targeted alignment writer must enforce the same schema"
    );
    let row = db.get_segment_by_id("alignment-update").unwrap().unwrap();
    assert!(row.alignment_json.is_none(), "a rejected targeted update must leave the row unchanged");
}

#[test]
fn phone_decision_rolls_back_every_side_effect_when_finalization_fails() {
    let db = make_db();
    db.insert_segment(&make_segment("phone-atomic", "/a.wav")).unwrap();
    ensure_test_audio_content_hash(&db, "phone-atomic");
    db.connection()
        .execute_batch(
            "CREATE TRIGGER fail_phone_finalize BEFORE UPDATE ON speech_segments
             WHEN NEW.verified = 1 BEGIN SELECT RAISE(ABORT, 'injected finalization failure'); END;",
        )
        .unwrap();

    let result = db.record_phone_human_decision_by("phone-atomic", "edit", Some("ڕاستکراوە"), "Sara");
    assert!(result.is_err(), "the injected finalization failure must propagate");

    let row = db.get_segment_by_id("phone-atomic").unwrap().unwrap();
    assert!(!row.verified);
    assert!(row.human_decision.is_none(), "verdict must roll back with finalization");
    assert!(row.reviewed_by.is_none(), "attribution must roll back with finalization");
    let examples: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM agent_examples WHERE segment_id = 'phone-atomic'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(examples, 0, "learning side effects must roll back with finalization");
}

#[test]
fn schema_v60_legacy_phone_finalizer_refuses_half_written_human_truth_without_mutation() {
    let db = make_db();
    db.insert_segment(&make_segment("legacy-half-written", "/legacy.wav")).unwrap();
    db.connection()
        .execute(
            "UPDATE speech_segments
                SET human_decision='edit', verdict='human_edit',
                    verdict_transcript='human correction', reviewed_by='Sara'
              WHERE id='legacy-half-written'",
            [],
        )
        .unwrap();
    let before = db.get_segment_by_id_with_revision("legacy-half-written").unwrap().unwrap();
    assert!(!before.0.verified && before.0.annotated_transcript.is_none());

    let error = db
        .finalize_phone_human_decision_at_revision("legacy-half-written", Some("human correction"), before.1)
        .unwrap_err();
    assert!(error.to_string().contains("offline repair lane"), "unexpected refusal: {error}");

    let after = db.get_segment_by_id_with_revision("legacy-half-written").unwrap().unwrap();
    assert_eq!(after.1, before.1, "refusal must not advance revision");
    assert!(!after.0.verified && after.0.annotated_transcript.is_none());
    let effects: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM human_decision_effect_events WHERE segment_id='legacy-half-written'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(effects, 0, "legacy refusal must neither invent nor partially write an effect");
}

#[test]
fn relink_refuses_a_candidate_already_owned_by_a_present_segment() {
    // The ambiguity guard counted basename collisions only among the MISSING paths. A missing
    // recording whose basename matches a file that a STILL-PRESENT segment already owns (a different
    // recording that happens to share the name) was silently repointed onto that other recording's
    // audio — transcript/audio mispairing, the exact wrong-audio hazard the guard exists to prevent.
    // A candidate file already owned by another library entry must be refused, not guessed.
    let db = make_db();
    let dir = tempfile::tempdir().unwrap();

    // Present segment B owns <dir>/interview.wav.
    let present_path = dir.path().join("interview.wav");
    std::fs::write(&present_path, b"present recording bytes").unwrap();
    let mut present = make_segment("seg-present", present_path.to_str().unwrap());
    present.raw_transcript = "recording B".to_string();
    db.insert_segment(&present).expect("insert present");

    // Missing segment A points at a DIFFERENT recording that shares the basename.
    let missing = make_segment("seg-missing", "/moved/away/interview.wav");
    db.insert_segment(&missing).expect("insert missing");

    let result = db.relink_audio(dir.path()).unwrap();

    assert_eq!(result.relinked, 0, "a candidate owned by a present segment must be refused");
    let a = db.get_segment_by_id("seg-missing").unwrap().unwrap();
    assert_eq!(
        a.audio_path, "/moved/away/interview.wav",
        "the missing segment must NOT be repointed onto another recording's audio"
    );
    assert_eq!(result.still_missing, 1, "the refused path stays honestly missing");
}

/// v50 end-to-end: the fingerprint survives the DB round trip, including the top bit.
///
/// `fp as i64` / `as u64` are BIT-CASTS. A numeric conversion would saturate or reject a fingerprint
/// above i64::MAX — half of all possible values — so this pins a u64 with the high bit set. Half the
/// corpus silently failing to dedup would be invisible without it.
#[test]
fn audio_fingerprint_round_trips_through_sqlite_including_the_high_bit() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    for id in ["c1", "c2"] {
        db.insert_segment(&SpeechSegment {
            id: id.into(),
            audio_path: "/audio/rec.wav".into(),
            ..SpeechSegment::default()
        })
        .unwrap();
    }
    db.insert_segment(&SpeechSegment {
        id: "other".into(),
        audio_path: "/audio/other.wav".into(),
        ..SpeechSegment::default()
    })
    .unwrap();

    let identity = crate::fingerprint::AudioIdentity {
        spectral: 0xF000_0000_0000_00FF, // high bit set — past i64::MAX
        content: "a".repeat(64),
    };
    let updated = db.set_audio_identity("/audio/rec.wav", &identity).unwrap();
    assert_eq!(updated, 2, "every chunk of the recording is stamped, keyed on audio_path");

    let loaded = db.load_audio_identities().unwrap();
    assert_eq!(loaded.len(), 1, "DISTINCT: one recording, not one row per chunk");
    assert_eq!(loaded[0].spectral, identity.spectral, "the high bit must survive the round trip");
    assert_eq!(
        loaded[0].content.as_deref(),
        Some(identity.content.as_str()),
        "v51: the content hash travels WITH the bucket, or a restart loses the ability to reject"
    );
    assert_eq!(loaded[0].audio_path, "/audio/rec.wav");

    // The untouched recording stays NULL and is simply absent — not defaulted to 0, which register
    // deliberately refuses to store as a bucket key.
    assert!(!loaded.iter().any(|r| r.audio_path == "/audio/other.wav"), "a NULL fingerprint must not appear");
}

#[test]
fn scoped_audio_identity_publication_refuses_same_path_replacement_and_rolls_back_new_rows() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let audio_path = r"C:\Audio\voice.wav";
    db.insert_segment(&SpeechSegment {
        id: "older".into(),
        audio_path: audio_path.into(),
        raw_transcript: "older transcript".into(),
        ..SpeechSegment::default()
    })
    .unwrap();
    let older_identity = crate::fingerprint::AudioIdentity { spectral: 11, content: "a".repeat(64) };
    db.set_audio_identity(audio_path, &older_identity).unwrap();

    let replacement = SpeechSegment {
        id: "replacement".into(),
        audio_path: audio_path.into(),
        raw_transcript: "replacement transcript".into(),
        ..SpeechSegment::default()
    };
    let replacement_identity = crate::fingerprint::AudioIdentity { spectral: 22, content: "b".repeat(64) };
    let error = db.insert_segments_with_audio_identity_batch(&[replacement], &replacement_identity).unwrap_err();
    assert!(error.to_string().contains("SOURCE_IDENTITY_DRIFT"), "unexpected refusal: {error}");
    assert!(db.get_segment_by_id("replacement").unwrap().is_none(), "the conflicting publication must roll back");
    assert_eq!(
        db.segment_audio_content_hash("older").unwrap().as_deref(),
        Some(older_identity.content.as_str()),
        "the older segment must retain its original byte identity"
    );

    let direct_error = db.set_audio_identity(audio_path, &replacement_identity).unwrap_err();
    assert!(direct_error.to_string().contains("SOURCE_IDENTITY_DRIFT"));
    assert_eq!(db.segment_audio_content_hash("older").unwrap().as_deref(), Some(older_identity.content.as_str()));
}

#[test]
fn scoped_audio_identity_never_rebinds_human_rows_and_allows_unchanged_replay() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    let stored_path = r"C:\Audio\Human.wav";
    let identity = crate::fingerprint::AudioIdentity { spectral: 33, content: "c".repeat(64) };
    db.insert_segment(&SpeechSegment {
        id: "human".into(),
        audio_path: stored_path.into(),
        raw_transcript: "human baseline".into(),
        ..SpeechSegment::default()
    })
    .unwrap();
    db.set_audio_identity(stored_path, &identity).unwrap();
    db.connection()
        .execute(
            "UPDATE speech_segments
                SET verified = 1, human_decision = 'accept', reviewed_by = 'owner'
              WHERE id = 'human'",
            [],
        )
        .unwrap();

    let alias_path = r"c:/audio/HUMAN.wav";
    let changed = SpeechSegment {
        id: "changed-alias".into(),
        audio_path: alias_path.into(),
        raw_transcript: "new bytes".into(),
        ..SpeechSegment::default()
    };
    let changed_identity = crate::fingerprint::AudioIdentity { spectral: 44, content: "d".repeat(64) };
    let error = db.insert_segments_with_audio_identity_batch(&[changed], &changed_identity).unwrap_err();
    assert!(error.to_string().contains("SOURCE_IDENTITY_DRIFT"), "case/separator alias must conflict: {error}");
    assert!(db.get_segment_by_id("changed-alias").unwrap().is_none());

    let unchanged = SpeechSegment {
        id: "unchanged-replay".into(),
        audio_path: stored_path.into(),
        raw_transcript: "same recording, new source operation".into(),
        ..SpeechSegment::default()
    };
    db.insert_segments_with_audio_identity_batch(&[unchanged], &identity).unwrap();
    let retained = db.get_segment_by_id("human").unwrap().unwrap();
    assert!(retained.verified);
    assert_eq!(retained.human_decision.as_deref(), Some("accept"));
    assert_eq!(db.segment_audio_content_hash("human").unwrap().as_deref(), Some(identity.content.as_str()));
    assert_eq!(db.segment_audio_content_hash("unchanged-replay").unwrap().as_deref(), Some(identity.content.as_str()));
}

/// The streamed active-learning queue must rank identically to collect-then-sort.
///
/// P1.3, last site. `get_active_learning_queue` materialised the whole corpus as full records to compute
/// one threshold and return at most `limit` clips. It now makes ONE streaming pass — the tally
/// accumulates the certificate while every unverified row's nonconformity is captured alongside it —
/// then sorts the light `(id, score)` pairs and hydrates only what it returns.
///
/// This is a MEMORY change, not a ranking change, and the distinction is the whole risk: the order here
/// decides which clip a reviewer is asked to judge first. So the streamed order is compared against the
/// original algorithm computed directly from a collected `Vec`, on the same corpus, including the
/// stable-sort tie behaviour that keeps equal-uncertainty clips in corpus order.
///
/// The SELECTION RULE itself is untouched and still naive — ranking by distance to a single threshold is
/// P1.4's problem and needs the Gold Marathon's calibration split. Fixing the memory shape does not make
/// the ranking good; it makes it affordable.
#[test]
fn streamed_active_learning_ranking_matches_collect_then_sort() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();

    // Mixed verified/unverified with varied confidence + ctc_score, so nonconformity actually spreads
    // and ties genuinely occur (every 5th clip shares a score with another).
    for i in 0..40 {
        db.insert_legacy_segment_fixture(&SpeechSegment {
            id: format!("s{i:03}"),
            audio_path: format!("/audio/{}.wav", i / 4),
            raw_transcript: format!("دەق {i}"),
            annotated_transcript: (i % 3 != 0).then(|| format!("دەقی {i}")),
            verified: i % 4 == 0,
            confidence: Some(0.3 + ((i % 5) as f64) / 10.0),
            ctc_score: Some(-1.0 - ((i % 5) as f64)),
            confidence_source: Some("real_posterior".into()),
            ..SpeechSegment::default()
        })
        .unwrap();
    }

    let (target_error, confidence_level, limit) = (0.05_f64, 0.95_f64, 7_usize);

    // ── Reference: the original algorithm, over a collected Vec. ──
    let collected = db.get_segments(None).unwrap();
    let q_hat_ref =
        crate::quality::conformal::calibrate_and_certify(&collected, target_error, confidence_level).threshold;
    let mut ref_pairs: Vec<(SpeechSegment, f64)> = collected
        .into_iter()
        .filter(|s| !s.verified)
        .map(|s| {
            let score = crate::quality::conformal::compute_nonconformity_score(&s);
            (s, -(score - q_hat_ref).abs())
        })
        .collect();
    ref_pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let expected: Vec<String> = ref_pairs.into_iter().take(limit).map(|(s, _)| s.id).collect();

    // ── Streamed: one pass, light pairs, hydrate the tail. ──
    let mut tally = crate::quality::conformal::ConformalTally::default();
    let mut scored: Vec<(String, f64)> = Vec::new();
    db.for_each_segment(None, |seg| {
        if !seg.verified {
            scored.push((seg.id.clone(), crate::quality::conformal::compute_nonconformity_score(&seg)));
        }
        tally.push(&seg);
    })
    .unwrap();
    let q_hat = tally.finish(target_error, confidence_level).threshold;
    assert_eq!(q_hat, q_hat_ref, "the streamed certificate must produce the same threshold");
    scored.sort_by(|a, b| {
        let (ua, ub) = (-(a.1 - q_hat).abs(), -(b.1 - q_hat).abs());
        ub.partial_cmp(&ua).unwrap_or(std::cmp::Ordering::Equal)
    });
    let actual: Vec<String> = scored.into_iter().take(limit).map(|(id, _)| id).collect();

    assert_eq!(actual, expected, "streamed ranking diverged from collect-then-sort");

    // Non-vacuity: comparing two empty or trivially-ordered queues would prove nothing.
    assert_eq!(actual.len(), limit, "the fixture must produce a full page of candidates");
    assert!(
        actual.iter().collect::<std::collections::BTreeSet<_>>().len() == limit,
        "the queue must not repeat a clip"
    );
}

/// Migration v52 must RENAME the agreement column, never recreate it — the values have to survive.
///
/// P1.2. `agent_confidence` became `agreement_score` because the old name invited exactly the reading
/// the jury rejects: every recognizer can confidently agree on the same garbage, so a HIGH value is
/// compatible with a completely wrong transcript. That is a naming fix, and a naming fix that silently
/// dropped a corpus of agreement scores would be a catastrophic way to achieve it — the suspect-first
/// ordering reads this column, so a wiped column degrades the review queue to recency with nothing
/// reporting it (the exact failure 6028824's predecessor had).
///
/// Exercised through the real migration chain on a fresh database, and asserted on a value written and
/// read back by the production path.
#[test]
fn renaming_the_agreement_column_preserves_its_values() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap(); // runs every migration, v52 included

    db.insert_segment(&SpeechSegment {
        id: "s1".into(),
        audio_path: "/audio/a.wav".into(),
        raw_transcript: "دەق".into(),
        ..SpeechSegment::default()
    })
    .unwrap();
    // write_segment_verdict is the production path — insert_segment silently drops jury fields.
    db.write_legacy_machine_verdict_for_test("s1", "escalated", None, None, None, Some(0.73), true).unwrap();

    let back = db.get_segment_by_id("s1").unwrap().unwrap();
    assert_eq!(back.agreement_score, Some(0.73), "the agreement value must survive the rename");

    // The new name is the one the schema actually carries, and the old one is gone — a column left
    // behind under both names would let two code paths drift onto different columns.
    let cols: Vec<String> = db
        .connection()
        .prepare("SELECT name FROM pragma_table_info('speech_segments')")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(cols.iter().any(|c| c == "agreement_score"), "agreement_score must exist after v52");
    assert!(
        !cols.iter().any(|c| c == "agent_confidence"),
        "agent_confidence must be GONE, not duplicated — two names for one number is how they drift"
    );
}

/// The SQL prefilter in `PendingWork::Transcript` must be a SUPERSET of `is_placeholder_transcript`.
///
/// P1.3 replaced a whole-library read + Rust filter with a SQL narrow + the SAME Rust filter. That is
/// only safe while SQL never excludes a row Rust would have accepted. The failure mode if they drift is
/// the nasty kind: a new placeholder string added to the Rust list would make those clips invisible to
/// the 7B driver forever, and nothing would report an error — the backlog would just silently look
/// empty. So the two are compared here directly, through real SQLite, rather than by eye.
///
/// The reverse direction is deliberately NOT asserted: SQL is allowed to over-select (a short real
/// transcript, a line starting with '['), because Rust rejects those a moment later at no cost.
#[test]
fn sql_placeholder_prefilter_is_a_superset_of_the_rust_predicate() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();

    // Every string the Rust authority calls "awaiting 7B", plus real transcripts that must NOT be.
    let awaiting =
        ["", "   ", "[Pending WSL 7B ASR]", "[ASR unavailable: model load failed]", "n/a", "N/A", "null", "NULL"];
    let real = ["سڵاو، چۆنی باشی؟", "This is a real transcript that a human wrote."];

    for (i, text) in awaiting.iter().chain(real.iter()).enumerate() {
        db.insert_segment(&SpeechSegment {
            id: format!("s{i:02}"),
            raw_transcript: (*text).to_string(),
            audio_path: format!("/audio/{i}.wav"),
            ..SpeechSegment::default()
        })
        .unwrap();
    }

    let selected: std::collections::HashSet<String> =
        db.get_pending_segments(PendingWork::Transcript).unwrap().into_iter().map(|s| s.raw_transcript).collect();

    for text in awaiting {
        assert!(
            crate::quality::is_placeholder_transcript(text) || text.trim().is_empty(),
            "fixture {text:?} is not actually a placeholder — fix the fixture, not the assertion"
        );
        assert!(
            selected.contains(text),
            "SQL prefilter DROPPED {text:?}, which the Rust authority treats as awaiting 7B. Those \
             clips would never be transcribed again and nothing would report it. Widen the WHERE in \
             PendingWork::Transcript."
        );
    }
    for text in real {
        assert!(!crate::quality::is_placeholder_transcript(text));
    }
    // And the narrowing is real: a genuine transcript is not dragged in as work.
    assert!(!selected.contains(real[0]), "SQL must still narrow — a real transcript is not a target");
}

/// The two score backfills must ask for their backlog, not the library.
#[test]
fn pending_work_selects_only_rows_missing_the_score() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    for id in ["done", "todo"] {
        db.insert_segment(&SpeechSegment {
            id: id.into(),
            raw_transcript: "real text".into(),
            audio_path: format!("/audio/{id}.wav"),
            ..SpeechSegment::default()
        })
        .unwrap();
    }
    db.connection()
        .execute("UPDATE speech_segments SET ctc_score = 0.9, signal_anomaly_score = 0.1 WHERE id = 'done'", [])
        .unwrap();

    for work in [PendingWork::CtcScore, PendingWork::SignalAnomaly] {
        let pending = db.get_pending_segments(work).unwrap();
        assert_eq!(pending.len(), 1, "{work:?}: only the unfinished row");
        assert_eq!(pending[0].id, "todo", "{work:?}: and it is the right one");
    }
}

/// A v50-era row — bucket present, content hash NULL — must load, and must be UNABLE to reject.
///
/// This is the honest coverage gap v51 leaves behind, and it has to be a real database fact rather than
/// a comment: rehydrating such a row as if it proved identity would restore exactly the false-reject
/// bug the content hash exists to kill.
#[test]
fn a_v50_era_row_loads_with_no_content_hash_and_cannot_reject() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    db.insert_segment(&SpeechSegment {
        id: "legacy".into(),
        audio_path: "/audio/legacy.wav".into(),
        ..SpeechSegment::default()
    })
    .unwrap();
    // Exactly what v50 wrote: the bucket column only.
    db.connection()
        .execute("UPDATE speech_segments SET audio_fingerprint = 4242 WHERE audio_path = '/audio/legacy.wav'", [])
        .unwrap();

    let loaded = db.load_audio_identities().unwrap();
    assert_eq!(loaded.len(), 1, "the row still participates — it is loaded, not skipped");
    assert_eq!(loaded[0].content, None, "and it is honestly unhashed");

    let map = crate::fingerprint::AudioFingerprint::new();
    assert_eq!(map.rehydrate(loaded), 1);
    let pcm: Vec<i16> = (0..16_000).map(|i| (i as i16).wrapping_mul(13)).collect();
    // Whatever bucket this audio lands in, an unhashed entry may never be the reason it is refused.
    assert!(
        !map.check_duplicate(&pcm, 16000, Some(std::path::Path::new("/audio/fresh.wav"))),
        "an unhashed legacy row must never reject a legitimate import"
    );
}

/// Folding the three corpus-wide statistics from a STREAM must give byte-identical answers to
/// collecting the corpus first.
///
/// P1.3. `dataset_analytics.rs` stopped calling `get_segments(None)` and now folds
/// `Database::for_each_segment` into TrainingGradeTally / ConformalTally / AnnotationDriftTally. Every
/// existing test for those statistics exercises the SLICE entry point, so they prove the tallies are
/// right without proving the thing that actually changed: that streaming sees the same rows, in the
/// same order, and reaches the same result. A silent divergence here would move the Insights dashboard
/// and the readiness verdict off the export's rule while every unit test stayed green.
///
/// Compared as serialized JSON so a newly added field is covered automatically rather than needing this
/// assertion to be remembered.
#[test]
fn streaming_the_corpus_statistics_equals_collecting_them_first() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();

    // Deliberately varied, so the membership rules each tally applies actually branch: verified and
    // not, human-rejected, annotated and bare, real_posterior and heuristic confidence, and one with no
    // confidence signal at all (which conformal must count separately rather than score).
    for i in 0..24 {
        let verified = i % 2 == 0;
        let annotated = i % 3 != 0;
        db.insert_legacy_segment_fixture(&SpeechSegment {
            id: format!("s{i:03}"),
            audio_path: format!("/audio/{}.wav", i / 4),
            raw_transcript: format!("خاڵی ژمارە {i}"),
            annotated_transcript: annotated.then(|| format!("خاڵی ژمارەی {i}")),
            verified,
            confidence: (i % 7 != 0).then(|| 0.5 + (i as f64 % 5.0) / 12.0),
            ctc_score: (i % 5 != 0).then(|| -1.0 - (i as f64 % 3.0)),
            snr_db: Some(3.0 + (i as f64 % 20.0)),
            clipping_ratio: Some((i as f64 % 4.0) / 20.0),
            confidence_source: Some(if i % 4 == 0 { "real_posterior".into() } else { "heuristic".into() }),
            human_decision: (i % 8 == 0).then(|| "reject".to_string()),
            ..SpeechSegment::default()
        })
        .unwrap();
    }

    let collected = db.get_segments(None).unwrap();
    assert_eq!(collected.len(), 24, "the fixture must actually be in the database");

    // A comparison of two EMPTY results is a vacuous pass, so the fixture is asserted to exercise each
    // statistic non-trivially before the equivalence assertions below are allowed to mean anything.
    let grade_probe = crate::quality::training_grade_breakdown(&collected);
    let cert_probe = crate::quality::conformal::calibrate_and_certify(&collected, 0.05, 0.95);
    let drift_probe = crate::scorecard::annotation_drift_scorecard(&collected, Default::default());
    assert!(grade_probe.summary.total_segments > 0, "fixture graded nothing");
    assert!(
        grade_probe.reason_counts.values().sum::<usize>() > 0,
        "fixture produced no grading reasons — the reason-count fold would be untested"
    );
    assert!(cert_probe.total_certified > 0, "fixture certified nothing — the threshold fold would be untested");
    assert!(
        cert_probe.calibration_heuristic + cert_probe.calibration_real_posterior > 0,
        "fixture calibrated on nothing"
    );
    assert!(drift_probe.num_segments > 0, "fixture had no annotated clips — the drift fold would be untested");

    // 1. Training-grade breakdown — the readiness verdict the export gates on.
    let mut tally = crate::quality::TrainingGradeTally::default();
    db.for_each_segment(None, |seg| tally.push(&seg)).unwrap();
    assert_eq!(
        serde_json::to_value(tally.finish()).unwrap(),
        serde_json::to_value(crate::quality::training_grade_breakdown(&collected)).unwrap(),
        "streamed training-grade breakdown diverged from the collected one"
    );

    // 2. Conformal certificate — the one that made TWO passes, the second gated on the first's
    //    threshold. Certified id ORDER is part of the value, so this also pins that the captured
    //    certify-side input preserved corpus order.
    let mut tally = crate::quality::conformal::ConformalTally::default();
    db.for_each_segment(None, |seg| tally.push(&seg)).unwrap();
    assert_eq!(
        serde_json::to_value(tally.finish(0.05, 0.95)).unwrap(),
        serde_json::to_value(crate::quality::conformal::calibrate_and_certify(&collected, 0.05, 0.95)).unwrap(),
        "streamed conformal certificate diverged from the collected one"
    );

    // 3. Annotation-drift scorecard — seeded bootstrap, so equality here also proves the two saw the
    //    same clips in the same order (a reordering would resample differently).
    let mut tally = crate::scorecard::AnnotationDriftTally::default();
    db.for_each_segment(None, |seg| tally.push(&seg)).unwrap();
    assert_eq!(
        serde_json::to_value(tally.finish(Default::default())).unwrap(),
        serde_json::to_value(crate::scorecard::annotation_drift_scorecard(&collected, Default::default())).unwrap(),
        "streamed annotation-drift scorecard diverged from the collected one"
    );
}

/// The suspect-first SQL and the jury veto must agree on what "poor audio" IS, at the boundary.
///
/// P1.2. Both used to carry their own hand-typed `< 5.0` / `> 0.1`, so moving the jury's threshold left
/// the review queue ordering on the old rule — the queue would quietly stop leading with the clips the
/// gate had just started distrusting, and no test would notice. They now share
/// `quality::POOR_AUDIO_*`; this proves the SHARING is real rather than two constants that happen to be
/// equal today, by exercising values placed exactly on either side of the boundary.
#[test]
fn suspect_first_sql_and_the_jury_veto_agree_on_poor_audio() {
    use crate::quality::{has_poor_audio, POOR_AUDIO_CLIPPING_RATIO, POOR_AUDIO_SNR_DB};

    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();

    // (id, snr, clipping) straddling both thresholds, plus the unmeasured case.
    let cases: [(&str, Option<f64>, Option<f64>); 6] = [
        ("snr_below", Some(POOR_AUDIO_SNR_DB - 0.5), None),
        ("snr_at", Some(POOR_AUDIO_SNR_DB), None), // `<` is strict: AT the threshold is NOT poor
        ("snr_above", Some(POOR_AUDIO_SNR_DB + 0.5), None),
        ("clip_above", None, Some(POOR_AUDIO_CLIPPING_RATIO + 0.05)),
        ("clip_at", None, Some(POOR_AUDIO_CLIPPING_RATIO)), // `>` is strict: AT is NOT poor
        ("unmeasured", None, None),                         // absence of a measurement is never evidence of bad audio
    ];
    for (id, snr, clip) in cases {
        db.insert_segment(&SpeechSegment {
            id: id.into(),
            audio_path: format!("/audio/{id}.wav"),
            snr_db: snr,
            clipping_ratio: clip,
            ..SpeechSegment::default()
        })
        .unwrap();
    }

    // Ask SQLite the same question the ORDER BY asks, using the very string the queue orders by.
    let case_expr = SUSPECT_FIRST_ORDER
        .split("(CASE")
        .nth(1)
        .and_then(|rest| rest.split(") ASC").next())
        .expect("the suspect-first order must still contain its poor-audio CASE arm");
    let sql = format!("SELECT id, (CASE{case_expr}) FROM speech_segments");
    let mut stmt = db.connection().prepare(&sql).unwrap();
    let sql_says: Vec<(String, i64)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(sql_says.len(), 6);

    for (id, sql_rank) in sql_says {
        let (_, snr, clip) = cases.iter().find(|(c, _, _)| *c == id).copied().unwrap();
        // The CASE yields 0 for poor audio (sorts first), 1 otherwise.
        let sql_poor = sql_rank == 0;
        assert_eq!(
            sql_poor,
            has_poor_audio(snr, clip),
            "{id}: the suspect-first SQL and quality::has_poor_audio disagree (snr={snr:?}, clip={clip:?}). \
             A drift here shows a reassuring badge on a clip the jury refused to trust."
        );
    }
}

/// Suspect-first must put the clips the jury DISTRUSTED first, not last (external review #2).
///
/// `agreement_score` on an escalated row is model AGREEMENT. When the audio is bad, every recognizer
/// can confidently agree on the same garbage — which is precisely why has_hard_distrust_veto refuses
/// to auto-accept it. Ordering on agreement alone therefore sent the least trustworthy clips to the
/// BACK of a riskiest-first queue.
#[test]
fn suspect_first_ranks_poor_audio_ahead_of_high_agreement() {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    // agreement_score and `escalated` are written by write_segment_verdict, the real path — NOT by
    // insert_segment, which silently drops them. A fixture that cannot reproduce the production shape
    // proves nothing (same trap as the human_decision fixture in 3a08d01).
    let mk = |id: &str, snr: f64, clip: f64| SpeechSegment {
        id: id.into(),
        audio_path: format!("/audio/{id}.wav"),
        snr_db: Some(snr),
        clipping_ratio: Some(clip),
        ..SpeechSegment::default()
    };
    // The exact shape the review described: noisy audio, 0.97 agreement.
    db.insert_segment(&mk("noisy", 2.0, 0.0)).unwrap();
    db.insert_segment(&mk("clipped", 30.0, 0.5)).unwrap();
    // Clean audio the models genuinely disagreed about.
    db.insert_segment(&mk("disputed", 30.0, 0.0)).unwrap();
    // Clean audio, high agreement — the genuinely least suspect row.
    db.insert_segment(&mk("clean", 30.0, 0.0)).unwrap();
    for (id, conf) in [("noisy", 0.97), ("clipped", 0.95), ("disputed", 0.20), ("clean", 0.99)] {
        db.write_legacy_machine_verdict_for_test(id, "escalated", None, None, None, Some(conf), true).unwrap();
    }

    let order: Vec<String> = db.get_segments_suspect_first(None).unwrap().into_iter().map(|s| s.id).collect();

    let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
    assert!(pos("noisy") < pos("disputed"), "poor-SNR audio must outrank a disagreement: {order:?}");
    assert!(pos("clipped") < pos("disputed"), "clipped audio must outrank a disagreement: {order:?}");
    assert!(pos("disputed") < pos("clean"), "within clean audio, low agreement still comes first: {order:?}");
    assert_eq!(order.last().unwrap(), "clean", "the least suspect clip is last: {order:?}");
}

#[test]
fn review_queue_never_serves_a_clip_the_champion_has_not_drafted() {
    // MEASURED 2026-08-14: an interrupted import left 36 rows carrying `[Pending WSL 7B ASR]`, and
    // the queue's only filter was `verified = 0`, so a reviewer could be handed one. `api_decision`
    // already refuses to VERIFY `[...]` text, so the reviewer would hit a 400 — but the worse path is
    // the one that succeeds: they type the transcript themselves, the clip is finished without the
    // champion ever drafting it, and it has no baseline for any CER measurement, permanently.
    let db = make_db();
    // Real files on disk: the queue also refuses a clip whose audio is missing, so a fixture with a
    // made-up path would be filtered for the WRONG reason and this test would pass vacuously.
    let audio = tempfile::tempdir().unwrap();
    let wav = |name: &str| {
        let p = audio.path().join(name);
        std::fs::write(&p, b"RIFF").unwrap();
        p.to_string_lossy().to_string()
    };
    let mut drafted = make_segment("drafted", &wav("a.wav"));
    drafted.raw_transcript = "دەقێکی ڕاستەقینە".to_string();
    let mut pending = make_segment("pending", &wav("b.wav"));
    pending.raw_transcript = "[Pending WSL 7B ASR]".to_string();
    let mut unavailable = make_segment("unavailable", &wav("c.wav"));
    unavailable.raw_transcript = "[ASR unavailable: server down]".to_string();
    let mut blank = make_segment("blank", &wav("d.wav"));
    blank.raw_transcript = "   ".to_string();
    for seg in [&drafted, &pending, &unavailable, &blank] {
        db.insert_segment(seg).unwrap();
    }

    let served = db.pending_segment_ids().unwrap();

    assert!(served.contains(&"drafted".to_string()), "a real draft must still be served: {served:?}");
    for hidden in ["pending", "unavailable", "blank"] {
        assert!(!served.contains(&hidden.to_string()), "{hidden} must never reach a reviewer: {served:?}");
    }
}

#[test]
fn review_queue_serves_the_oldest_work_first() {
    // MEASURED 2026-08-14: importing 27 hours of new podcast audio put 6,823 fresh clips in front of
    // the 537 remaining clips of the original corpus, because the queue was newest-first. The owner's
    // instruction was the opposite — finish the old material before starting the new dialect — and a
    // review queue is FIFO anyway: adding audio must never delay work already in progress.
    let db = make_db();
    let audio = tempfile::tempdir().unwrap();
    for (id, created) in
        [("old-1", "2026-08-01 10:00:00"), ("mid-1", "2026-08-10 10:00:00"), ("new-1", "2026-08-14 10:00:00")]
    {
        let path = audio.path().join(format!("{id}.wav"));
        std::fs::write(&path, b"RIFF").unwrap();
        let mut seg = make_segment(id, &path.to_string_lossy());
        seg.raw_transcript = "دەقی ڕاست".to_string();
        seg.created_at = Some(created.to_string());
        db.insert_segment_full(&seg).unwrap();
    }

    let served = db.pending_segment_ids().unwrap();

    assert_eq!(
        served,
        vec!["old-1".to_string(), "mid-1".to_string(), "new-1".to_string()],
        "the queue must hand out the oldest pending clip first: {served:?}"
    );
}

#[test]
fn a_spot_check_is_never_served_in_a_dialect_the_reviewer_cannot_judge() {
    // Spot checks are injected on a path of their own, AFTER the queue's dialect filter — so they
    // were the one remaining way to hand someone a dialect they do not speak, and the most damaging:
    // a check is SCORED, so an honest reviewer fails a test they could not have passed and is
    // recorded looking exactly like someone tapping "looks good" without listening.
    let db = make_db();
    let audio = tempfile::tempdir().unwrap();
    let mut ids = Vec::new();
    for (id, file) in [("haw", "KBHP-EP01.wav"), ("sor", "zar-01.wav")] {
        // Real files: the candidate list refuses a key whose audio is gone, for its own good reason.
        let path = if id == "haw" {
            audio.path().join("sorani-hawleri").join(file)
        } else {
            audio.path().join("Kurdish Corpora").join("sorani").join("ZarPodcast").join(file)
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"RIFF").unwrap();
        let mut seg = make_segment(id, path.to_str().unwrap());
        seg.raw_transcript = "دەقی هەڵە".into();
        db.insert_segment(&seg).unwrap();
        ensure_test_audio_content_hash(&db, id);
        // Through the real decision path, so the answer key lands where a human edit leaves it.
        db.finalize_human_review(id, "edit", Some("دەقی ڕاست"), None, None).unwrap();
        ids.push(id);
    }
    let candidates = |allowed: Option<&[String]>| -> Vec<String> {
        db.list_spot_check_candidates(10, "Nasrin", &std::collections::HashSet::new(), allowed, None)
            .unwrap()
            .into_iter()
            .map(|(s, _)| s.id)
            .collect()
    };
    assert_eq!(candidates(None).len(), 2, "unrestricted: both clips are usable keys");
    let sorani_only = vec![crate::dialect::SORANI.to_string()];
    assert_eq!(
        candidates(Some(&sorani_only)),
        vec!["sor".to_string()],
        "a Sorani-only reviewer must be graded on Sorani only"
    );
}

#[test]
fn a_snapshot_of_a_real_sized_library_does_not_take_minutes() {
    // MEASURED 2026-08-17: `backup` paced itself at 5 pages per 250 ms — the rusqlite doc example,
    // copied without arithmetic. That is 80 KB/s, so the owner's 84 MB library took ~18 minutes per
    // snapshot. `take_snapshot` runs synchronously at startup, so EVERY launch held the reviewer
    // port shut for a quarter of an hour, and the 10-minute snapshot timer meant a copy was almost
    // always running against the live database.
    //
    // The size assertion is the load-bearing half: a handful of pages finishes fast under either
    // pacing, so without it this test would pass while the bug was fully present.
    let db = make_db();
    let mut seg = make_segment("bulk", "/audio/bulk.wav");
    seg.raw_transcript = "ک".repeat(4000); // ~4 KB of text per row, so page count climbs quickly
    for i in 0..600 {
        seg.id = format!("bulk-{i}");
        db.insert_segment(&seg).unwrap();
    }
    let pages: i64 = db.connection().query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    assert!(pages >= 1000, "the fixture must be big enough for pacing to matter, got {pages} pages");

    // Bound to a name: a temporary TempDir is dropped at the end of the statement that made it, which
    // deletes the directory out from under the backup.
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("snap.db");
    let started = std::time::Instant::now();
    db.backup(&dest).unwrap();
    let elapsed = started.elapsed();

    // Old pacing needed >= pages/5 * 250 ms — at least 50 s for 1000 pages. New pacing is well under
    // a second. The budget is deliberately loose so a busy CI box cannot flake it; anything near the
    // old behaviour misses it by orders of magnitude.
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "backing up {pages} pages took {elapsed:?} — the per-step pacing has regressed"
    );
}

#[test]
fn reviewed_audio_ms_counts_each_clip_once_per_reviewer() {
    // This is full AUDIO-ACTIVITY progress, not money. A network retry or re-decision of the same
    // clip must not inflate it; accept/edit/reject all mean the reviewer judged the clip. Weighted
    // compensation is a separate immutable projection.
    let db = make_db();
    for (id, ms) in [("a", 9000), ("b", 21000)] {
        let mut seg = make_segment(id, &format!("/audio/{id}.wav"));
        seg.duration_ms = ms;
        db.insert_segment(&seg).unwrap();
        ensure_test_audio_content_hash(&db, id);
    }
    let revision = db.segment_review_revision("a").unwrap().unwrap();
    let revision =
        db.record_phone_human_decision_by_at_revision("a", "accept", Some("test"), "Rezan", revision).unwrap().unwrap();
    db.record_phone_human_decision_by_at_revision("a", "edit", Some("دەقی دەستکاریکراو"), "Rezan", revision)
        .unwrap()
        .unwrap(); // same clip again — a re-decision, not new activity
    let revision = db.segment_review_revision("b").unwrap().unwrap();
    db.record_phone_human_decision_by_at_revision("b", "reject", None, "Rezan", revision).unwrap().unwrap(); // judged activity; payable credit is separately weighted to 10%
    let revision = db.segment_review_revision("a").unwrap().unwrap();
    db.record_phone_human_decision_by_at_revision("a", "accept", Some("دەقی دەستکاریکراو"), "Zana", revision)
        .unwrap()
        .unwrap(); // another reviewer on the same clip counts for HER

    assert_eq!(
        db.reviewed_audio_ms("Rezan").unwrap(),
        30_000,
        "both judged clips count as activity; the repeat on clip a is not double-counted"
    );
    assert_eq!(db.reviewed_audio_ms("Zana").unwrap(), 9_000);
    assert_eq!(db.reviewed_audio_ms("Nobody").unwrap(), 0, "a reviewer with no work owes no rows");

    // PAY SURVIVES DELETION (2026-08-20 hunt): the owner pruning a reviewed clip must not shrink
    // the total for work that was genuinely done — the event snapshots the duration it was paid
    // against (v56), so the number the owner pays on is append-only in practice.
    assert!(
        db.delete_segment("a").is_err(),
        "effect-bound review evidence is append-only, so deleting its source segment must fail closed"
    );
    assert_eq!(db.reviewed_audio_ms("Rezan").unwrap(), 30_000, "failed deletion cannot shrink review progress");
    assert_eq!(db.reviewed_audio_ms("Zana").unwrap(), 9_000, "nor another reviewer's progress");
}

fn record_payable_edit(db: &Database, segment_id: &str, reviewer: &str, duration_ms: i64) -> i64 {
    let mut segment = make_segment(segment_id, &format!("/{segment_id}.wav"));
    segment.raw_transcript = "هەڵە".into();
    segment.duration_ms = duration_ms;
    db.insert_segment(&segment).unwrap();
    ensure_test_audio_content_hash(db, segment_id);
    let revision = db.segment_review_revision(segment_id).unwrap().unwrap();
    db.record_phone_human_decision_by_at_revision(segment_id, "edit", Some("ڕاست"), reviewer, revision)
        .unwrap()
        .unwrap();
    db.connection()
        .query_row(
            "SELECT id FROM review_compensation_ledger WHERE segment_id = ?1 ORDER BY id DESC LIMIT 1",
            [segment_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn review_compensation_policy_arithmetic_and_action_weights_are_exact() {
    assert_eq!(review_pay_basis_points("edit").unwrap(), 10_000);
    assert_eq!(review_pay_basis_points("accept").unwrap(), 1_000);
    assert_eq!(review_pay_basis_points("reject").unwrap(), 1_000);
    assert_eq!(review_pay_basis_points("skip").unwrap(), 0);
    assert_eq!(review_pay_basis_points("undo").unwrap(), 0);
    assert!(review_pay_basis_points("Accept").is_err(), "action names are a closed, case-sensitive contract");

    // At 18,000 IQD/hour the authorized policy is exact down to one millisecond: 5,000
    // micro-IQD for an edit and 500 for accept/reject. Nothing is rounded per clip.
    assert_eq!(review_pay_entitlement_micro_iqd(1, 10_000).unwrap(), 5_000);
    assert_eq!(review_pay_entitlement_micro_iqd(1, 1_000).unwrap(), 500);
    assert_eq!(review_pay_entitlement_micro_iqd(3_600_000, 10_000).unwrap(), 18_000_000_000);
    assert_eq!(review_pay_entitlement_micro_iqd(3_600_000, 1_000).unwrap(), 1_800_000_000);
    assert_eq!(review_pay_entitlement_micro_iqd(3_600_000, 0).unwrap(), 0);

    assert!(review_pay_entitlement_micro_iqd(-1, 1_000).is_err());
    assert!(review_pay_entitlement_micro_iqd(1, -1).is_err());
    assert!(review_pay_entitlement_micro_iqd(1, 10_001).is_err());
    assert!(
        review_pay_entitlement_micro_iqd(1, 1).is_err(),
        "a future policy that cannot represent one millisecond exactly must fail closed"
    );
    assert!(
        review_pay_entitlement_micro_iqd(i64::MAX, 10_000).is_err(),
        "an entitlement outside the persisted i64 domain must never wrap"
    );
}

#[test]
fn review_compensation_records_the_owner_authorized_action_schedule() {
    let db = make_db();
    let mut edit = make_segment("pay-edit", "/edit.wav");
    edit.raw_transcript = "هەڵە".into();
    db.insert_segment(&edit).unwrap();
    ensure_test_audio_content_hash(&db, "pay-edit");
    let revision = db.segment_review_revision("pay-edit").unwrap().unwrap();
    db.record_phone_human_decision_by_at_revision("pay-edit", "edit", Some("ڕاست"), "Sara", revision).unwrap().unwrap();

    seed_for_provenance(&db, "pay-accept", "دەقی مۆدێل");
    let revision = db.segment_review_revision("pay-accept").unwrap().unwrap();
    db.record_phone_human_decision_by_at_revision("pay-accept", "accept", Some("دەقی مۆدێل"), "Sara", revision)
        .unwrap()
        .unwrap();

    db.insert_segment(&make_segment("pay-reject", "/reject.wav")).unwrap();
    ensure_test_audio_content_hash(&db, "pay-reject");
    let revision = db.segment_review_revision("pay-reject").unwrap().unwrap();
    db.record_phone_human_decision_by_at_revision("pay-reject", "reject", None, "Sara", revision).unwrap().unwrap();

    db.insert_segment(&make_segment("pay-skip", "/skip.wav")).unwrap();
    ensure_test_audio_content_hash(&db, "pay-skip");
    db.record_review_event("pay-skip", "Sara", "skip", "test", 1_700_000_000_000).unwrap();

    let rows: Vec<(String, i64, i64, i64)> = db
        .connection()
        .prepare(
            "SELECT compensation_action, rate_basis_points, entitlement_micro_iqd, delta_micro_iqd
               FROM review_compensation_ledger WHERE reviewer = 'Sara' ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("edit".into(), 10_000, 5_000_000, 5_000_000),
            ("accept".into(), 1_000, 500_000, 500_000),
            ("reject".into(), 1_000, 500_000, 500_000),
            ("skip".into(), 0, 0, 0),
        ]
    );
    let summary = db.review_compensation_summary("sara").unwrap();
    assert_eq!(summary.policy_version, REVIEW_PAY_POLICY_VERSION);
    assert_eq!(summary.earned_micro_iqd, 6_000_000);
    assert_eq!(summary.legacy_events_pending_reconciliation, 0);
}

#[test]
fn provenance_reclassification_never_changes_the_semantic_accept_rate() {
    let db = make_db();
    seed_for_provenance(&db, "pay-reclass", "دەقی مۆدێل");
    let human_text = "دەقی مرۆڤ کە هیچ مۆدێلێک نەنووسیویەتی";
    // This is the real re-review case: the phone serves a previous human's text and this reviewer
    // accepts it unchanged. It is semantically an accept for pay, but still human-authored corpus
    // provenance because no ASR hypothesis contains it.
    db.connection()
        .execute("UPDATE speech_segments SET annotated_transcript = ?1 WHERE id = 'pay-reclass'", [human_text])
        .unwrap();
    let served_revision = db.segment_review_revision("pay-reclass").unwrap().unwrap();

    db.record_phone_human_decision_by_at_revision("pay-reclass", "accept", Some(human_text), "Sara", served_revision)
        .unwrap()
        .expect("fresh phone decision must win its CAS");

    let event: (String, String) = db
        .connection()
        .query_row(
            "SELECT action, compensation_action FROM review_events WHERE segment_id = 'pay-reclass'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(event, ("edit".into(), "accept".into()), "corpus provenance and performed action stay distinct");

    let ledger: (String, String, i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT compensation_action, effective_decision, rate_basis_points,
                    entitlement_micro_iqd, delta_micro_iqd
               FROM review_compensation_ledger WHERE segment_id = 'pay-reclass'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(ledger, ("accept".into(), "edit".into(), 1_000, 500_000, 500_000));
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 500_000);
}

#[test]
fn redecisions_adjust_to_one_current_entitlement_and_skip_never_retracts_it() {
    let db = make_db();
    let mut segment = make_segment("pay-redecision", "/pay-redecision.wav");
    segment.duration_ms = 2_000;
    segment.raw_transcript = "دەقی مۆدێل".into();
    db.insert_segment(&segment).unwrap();
    ensure_test_audio_content_hash(&db, "pay-redecision");
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: "pay-redecision".into(),
        model_id: "omniasr-wsl-7b".into(),
        transcript: "دەقی مۆدێل".into(),
        confidence: None,
    })
    .unwrap();
    let revision = db.segment_review_revision("pay-redecision").unwrap().unwrap();
    let revision = db
        .record_phone_human_decision_by_at_revision("pay-redecision", "accept", Some("دەقی مۆدێل"), "Sara", revision)
        .unwrap()
        .unwrap();
    let revision = db
        .record_phone_human_decision_by_at_revision(
            "pay-redecision",
            "edit",
            Some("دەقی دەستکاریکراوی یەکەم"),
            "SARA",
            revision,
        )
        .unwrap()
        .unwrap();
    let revision = db
        .record_phone_human_decision_by_at_revision("pay-redecision", "reject", None, "sara", revision)
        .unwrap()
        .unwrap();
    db.record_phone_human_decision_by_at_revision(
        "pay-redecision",
        "edit",
        Some("دەقی دەستکاریکراوی دووەم"),
        "Sara",
        revision,
    )
    .unwrap()
    .unwrap();
    db.record_review_event("pay-redecision", "Sara", "skip", "test", 1_700_000_000_000).unwrap();

    let deltas: Vec<(i64, i64)> = db
        .connection()
        .prepare("SELECT delta_micro_iqd, delta_corrected_ms FROM review_compensation_ledger ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(deltas, vec![(1_000_000, 0), (9_000_000, 2_000), (-9_000_000, -2_000), (9_000_000, 2_000), (0, 0)]);
    let summary = db.review_compensation_summary("SaRa").unwrap();
    assert_eq!(summary.earned_micro_iqd, 10_000_000, "redecisions project one current money entitlement");
    assert_eq!(
        summary.corrected_audio_ms, 2_000,
        "the ledger projects the latest payable decision, never the sum of every retry"
    );
}

#[test]
fn a_spot_check_retry_cannot_mint_a_second_event_or_change_the_first_action() {
    let db = make_db();
    let mut segment = make_segment("pay-spot-retry", "/pay-spot-retry.wav");
    segment.duration_ms = 2_000;
    db.insert_segment(&segment).unwrap();
    ensure_test_audio_content_hash(&db, "pay-spot-retry");

    db.record_spot_check("pay-spot-retry", "Sara", "edit", "ڕاست", "ڕاست").unwrap();
    db.record_spot_check("pay-spot-retry", "Sara", "reject", "جیاواز", "ڕاست").unwrap();

    let persisted_action: String = db
        .connection()
        .query_row(
            "SELECT action FROM spot_checks WHERE segment_id = 'pay-spot-retry' AND reviewer = 'Sara'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted_action, "edit", "the immutable first answer wins");
    let counts: (i64, i64) = db
        .connection()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM review_events WHERE segment_id = 'pay-spot-retry'),
                    (SELECT COUNT(*) FROM review_compensation_ledger WHERE segment_id = 'pay-spot-retry')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (1, 1));
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 10_000_000);
}

#[test]
fn compensation_failure_rolls_back_its_review_event() {
    let db = make_db();
    let mut segment = make_segment("pay-atomic", "/pay-atomic.wav");
    segment.raw_transcript = "هەڵە".into();
    db.insert_segment(&segment).unwrap();
    ensure_test_audio_content_hash(&db, "pay-atomic");
    let served_revision = db.segment_review_revision("pay-atomic").unwrap().unwrap();
    db.connection()
        .execute_batch(
            "CREATE TRIGGER fail_compensation_insert
             BEFORE INSERT ON review_compensation_ledger
             BEGIN SELECT RAISE(ABORT, 'injected compensation failure'); END;",
        )
        .unwrap();

    assert!(db
        .record_phone_human_decision_by_at_revision("pay-atomic", "edit", Some("ڕاست"), "Sara", served_revision,)
        .is_err());
    let event_count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM review_events WHERE segment_id = 'pay-atomic'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(event_count, 0, "pay and its audit event are one commit");
    let unchanged = db.get_segment_by_id("pay-atomic").unwrap().unwrap();
    assert!(unchanged.human_decision.is_none());
    assert!(!unchanged.verified);
    assert_eq!(db.segment_review_revision("pay-atomic").unwrap(), Some(served_revision));

    db.connection().execute_batch("DROP TRIGGER fail_compensation_insert;").unwrap();
    db.record_phone_human_decision_by_at_revision("pay-atomic", "edit", Some("ڕاست"), "Sara", served_revision)
        .unwrap()
        .unwrap();
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 5_000_000);
}

#[test]
fn a_short_source_span_can_never_mint_compensation_for_a_ten_times_longer_duration() {
    let db = make_db();
    let mut segment = make_segment("pay-duration-drift", "/pay-duration-drift.wav");
    segment.duration_ms = 10_000;
    db.insert_segment(&segment).unwrap();
    db.connection()
        .execute(
            "UPDATE speech_segments
                SET audio_content_hash = ?2,
                    alignment_json = json_object('source_start_ms', 0, 'source_end_ms', 1000)
              WHERE id = ?1",
            params!["pay-duration-drift", TEST_AUDIO_CONTENT_HASH],
        )
        .unwrap();
    let revision = db.segment_review_revision("pay-duration-drift").unwrap().unwrap();
    let error = db
        .record_phone_human_decision_by_at_revision_with_operation(
            "pay-duration-drift",
            "edit",
            Some("human truth"),
            "Sara",
            revision,
            "77777777-7777-4777-8777-777777777777",
            &review_operation_payload_hash("pay-duration-drift", "edit", "human truth", "Sara"),
        )
        .unwrap_err();
    assert!(error.to_string().contains("Couch authority span disagrees with duration"), "{error}");
    let row = db.get_segment_by_id("pay-duration-drift").unwrap().unwrap();
    assert!(row.human_decision.is_none() && !row.verified, "failed pay identity must roll back the decision");
    for table in ["review_events", "review_compensation_ledger", "human_decision_effect_events"] {
        let count: i64 =
            db.connection().query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0, "failed duration identity must not leave {table} evidence");
    }
}

#[test]
fn review_operation_receipt_exactly_identifies_the_committed_phone_work() {
    let db = make_db();
    let mut segment = make_segment("pay-operation", "/pay-operation.wav");
    segment.raw_transcript = "هەڵە".into();
    db.insert_segment(&segment).unwrap();
    ensure_test_audio_content_hash(&db, "pay-operation");
    let revision = db.segment_review_revision("pay-operation").unwrap().unwrap();
    let operation_id = "123e4567-e89b-42d3-a456-426614174000";
    let payload_hash = review_operation_payload_hash("pay-operation", "edit", "ڕاست", "Sara");
    db.record_phone_human_decision_by_at_revision_with_operation(
        "pay-operation",
        "edit",
        Some("ڕاست"),
        "Sara",
        revision,
        operation_id,
        &payload_hash,
    )
    .unwrap()
    .unwrap();

    let receipt = db.review_operation(operation_id).unwrap().expect("committed operation has a durable receipt");
    assert_eq!(receipt.operation_id, operation_id);
    assert_eq!(receipt.operation_payload_hash, payload_hash);
    assert_eq!(receipt.segment_id, "pay-operation");
    assert_eq!(receipt.reviewer, "Sara");
    assert_eq!(receipt.action, "edit");
    assert_eq!(receipt.compensation_action, "edit");
    let ledger_event_id: i64 = db
        .connection()
        .query_row(
            "SELECT review_event_id FROM review_compensation_ledger WHERE segment_id = 'pay-operation'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipt.review_event_id, ledger_event_id);
    assert!(
        db.review_operation("123e4567-e89b-42d3-a456-426614174999").unwrap().is_none(),
        "a valid but unknown UUID is an exact miss"
    );
}

#[test]
fn duplicate_review_operation_uuid_is_rejected_and_rolls_back_the_other_payload() {
    let db = make_db();
    for id in ["pay-operation-first", "pay-operation-second"] {
        let mut segment = make_segment(id, &format!("/{id}.wav"));
        segment.raw_transcript = "هەڵە".into();
        db.insert_segment(&segment).unwrap();
        ensure_test_audio_content_hash(&db, id);
    }
    let operation_id = "223e4567-e89b-42d3-a456-426614174000";
    let first_hash = review_operation_payload_hash("pay-operation-first", "edit", "ڕاستی یەکەم", "Sara");
    let revision = db.segment_review_revision("pay-operation-first").unwrap().unwrap();
    db.record_phone_human_decision_by_at_revision_with_operation(
        "pay-operation-first",
        "edit",
        Some("ڕاستی یەکەم"),
        "Sara",
        revision,
        operation_id,
        &first_hash,
    )
    .unwrap()
    .unwrap();

    let second_revision = db.segment_review_revision("pay-operation-second").unwrap().unwrap();
    let second_hash = review_operation_payload_hash("pay-operation-second", "edit", "ڕاستی دووەم", "Sara");
    assert!(
        db.record_phone_human_decision_by_at_revision_with_operation(
            "pay-operation-second",
            "edit",
            Some("ڕاستی دووەم"),
            "Sara",
            second_revision,
            operation_id,
            &second_hash,
        )
        .is_err(),
        "one UUID can name exactly one immutable payload"
    );
    let second = db.get_segment_by_id("pay-operation-second").unwrap().unwrap();
    assert!(second.human_decision.is_none(), "the conflicting verdict must roll back with its event");
    assert!(!second.verified);
    assert_eq!(db.segment_review_revision("pay-operation-second").unwrap(), Some(second_revision));
    let second_events: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM review_events WHERE segment_id = 'pay-operation-second'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(second_events, 0);
    let receipt = db.review_operation(operation_id).unwrap().unwrap();
    assert_eq!(receipt.segment_id, "pay-operation-first");
    assert_eq!(receipt.operation_payload_hash, first_hash);
}

#[test]
fn invalid_review_operation_uuid_or_hash_fails_before_any_write() {
    let db = make_db();
    db.insert_segment(&make_segment("pay-operation-invalid", "/pay-operation-invalid.wav")).unwrap();
    let revision = db.segment_review_revision("pay-operation-invalid").unwrap().unwrap();
    let valid_id = "323e4567-e89b-42d3-a456-426614174000";
    let valid_hash = "a".repeat(64);
    for (operation_id, payload_hash) in [
        ("not-a-uuid", valid_hash.as_str()),
        ("323E4567-E89B-42D3-A456-426614174000", valid_hash.as_str()),
        (valid_id, "short"),
        (valid_id, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        (valid_id, "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg"),
    ] {
        assert!(
            db.record_phone_human_decision_by_at_revision_with_operation(
                "pay-operation-invalid",
                "accept",
                Some("test"),
                "Sara",
                revision,
                operation_id,
                payload_hash,
            )
            .is_err(),
            "invalid identity must fail: {operation_id} / {payload_hash}"
        );
    }
    assert!(db.review_operation("not-a-uuid").is_err());
    let event_count: i64 =
        db.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
    assert_eq!(event_count, 0);
    let row = db.get_segment_by_id("pay-operation-invalid").unwrap().unwrap();
    assert!(row.human_decision.is_none());
    assert_eq!(db.segment_review_revision("pay-operation-invalid").unwrap(), Some(revision));
}

#[test]
fn controlled_review_action_cap_is_atomic_for_verdict_skip_event_and_compensation() {
    let db = make_db();
    for id in ["pilot-sara-1", "pilot-sara-2", "pilot-hemn-skip", "pilot-hemn-2"] {
        db.insert_segment(&make_segment(id, &format!("/{id}.wav"))).unwrap();
        ensure_test_audio_content_hash(&db, id);
    }
    let limit = ReviewDecisionLimit::new(0, 2, vec![("Sara".into(), 1), ("Hemn".into(), 1)]).unwrap();
    let sara_proof = canonical_policy4_phone_playback(&db, "pilot-sara-1", "Sara");
    let sara_revision = sara_proof.segment_revision;
    db.record_phone_human_decision_by_at_revision_with_operation_limit(
        "pilot-sara-1",
        "accept",
        Some("test"),
        "Sara",
        sara_revision,
        &sara_proof,
        "423e4567-e89b-42d3-a456-426614174001",
        &review_operation_payload_hash("pilot-sara-1", "accept", "test", "Sara"),
        "accept",
        "test",
        Some(&limit),
    )
    .unwrap()
    .unwrap();

    let refused_proof = canonical_policy4_phone_playback(&db, "pilot-sara-2", "Sara");
    let refused_revision = refused_proof.segment_revision;
    let refused = db
        .record_phone_human_decision_by_at_revision_with_operation_limit(
            "pilot-sara-2",
            "edit",
            Some("different"),
            "Sara",
            refused_revision,
            &refused_proof,
            "423e4567-e89b-42d3-a456-426614174002",
            &review_operation_payload_hash("pilot-sara-2", "edit", "different", "Sara"),
            "edit",
            "different",
            Some(&limit),
        )
        .unwrap_err();
    assert!(refused.to_string().contains(REVIEW_PILOT_LIMIT_REACHED));
    let untouched = db.get_segment_by_id("pilot-sara-2").unwrap().unwrap();
    assert!(!untouched.verified && untouched.human_decision.is_none());
    assert!(db.review_operation("423e4567-e89b-42d3-a456-426614174002").unwrap().is_none());

    db.record_review_event_with_operation_limit(
        "pilot-hemn-skip",
        "Hemn",
        "skip",
        "couch",
        1,
        "423e4567-e89b-42d3-a456-426614174003",
        &review_operation_payload_hash("pilot-hemn-skip", "skip", "", "Hemn"),
        "skip",
        "",
        Some(&limit),
    )
    .unwrap();
    let hemn_proof = canonical_policy4_phone_playback(&db, "pilot-hemn-2", "Hemn");
    let hemn_revision = hemn_proof.segment_revision;
    let refused = db
        .record_phone_human_decision_by_at_revision_with_operation_limit(
            "pilot-hemn-2",
            "accept",
            Some("test"),
            "Hemn",
            hemn_revision,
            &hemn_proof,
            "423e4567-e89b-42d3-a456-426614174004",
            &review_operation_payload_hash("pilot-hemn-2", "accept", "test", "Hemn"),
            "accept",
            "test",
            Some(&limit),
        )
        .unwrap_err();
    assert!(refused.to_string().contains(REVIEW_PILOT_LIMIT_REACHED));

    let progress = db.review_decision_progress(&limit).unwrap();
    assert_eq!(progress.total_review_actions, 2);
    assert_eq!(progress.by_reviewer.get("Sara"), Some(&1));
    assert_eq!(progress.by_reviewer.get("Hemn"), Some(&1));
    let (events, ledger): (i64, i64) = db
        .connection()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM review_events), (SELECT COUNT(*) FROM review_compensation_ledger)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((events, ledger), (2, 2), "each authorized action and pay consequence commits once");
}

#[test]
fn paid_corpus_write_rechecks_a_trigger_disabled_missing_playback_receipt_inside_its_transaction() {
    let db = make_db();
    db.insert_segment(&make_segment("corpus-proof", "/corpus-proof.wav")).unwrap();
    let stale = canonical_policy4_phone_playback(&db, "corpus-proof", "Sara");
    let revision = stale.segment_revision;
    db.connection().execute("DROP TRIGGER playback_receipts_v67_policy4_immutable_delete", []).unwrap();
    db.connection().execute("DELETE FROM playback_receipts WHERE segment_id='corpus-proof'", []).unwrap();

    let refused = db
        .record_phone_human_decision_by_at_revision_with_operation_limit(
            "corpus-proof",
            "accept",
            Some("test"),
            "Sara",
            revision,
            &stale,
            "923e4567-e89b-42d3-a456-426614174001",
            &review_operation_payload_hash("corpus-proof", "accept", "test", "Sara"),
            "accept",
            "test",
            None,
        )
        .unwrap();
    assert!(refused.is_none(), "vanished playback evidence must behave as an atomic state conflict");
    let empty: (i64, i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT verified,
                    (SELECT COUNT(*) FROM review_events WHERE segment_id='corpus-proof'),
                    (SELECT COUNT(*) FROM review_compensation_ledger WHERE segment_id='corpus-proof'),
                    (SELECT COUNT(*) FROM review_events
                      WHERE segment_id='corpus-proof' AND operation_id IS NOT NULL)
               FROM speech_segments WHERE id='corpus-proof'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(empty, (0, 0, 0, 0), "verdict, event, ledger, and operation receipt must all stay absent");

    let current = canonical_policy4_phone_playback(&db, "corpus-proof", "Sara");
    let committed = db
        .record_phone_human_decision_by_at_revision_with_operation_limit(
            "corpus-proof",
            "accept",
            Some("test"),
            "Sara",
            current.segment_revision,
            &current,
            "923e4567-e89b-42d3-a456-426614174002",
            &review_operation_payload_hash("corpus-proof", "accept", "test", "Sara"),
            "accept",
            "test",
            None,
        )
        .unwrap();
    assert!(committed.is_some(), "current raw playback evidence authorizes the exact paid write");
}

#[test]
fn controlled_review_cap_serializes_the_last_slot_across_database_connections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pilot-race.db");
    let path_text = path.to_string_lossy().to_string();
    let setup = Database::open(&path_text).unwrap();
    setup.initialize().unwrap();
    let mut playback = Vec::new();
    for id in ["pilot-race-a", "pilot-race-b"] {
        setup.insert_segment(&make_segment(id, &format!("/{id}.wav"))).unwrap();
        playback.push((id, canonical_policy4_phone_playback(&setup, id, "Sara")));
    }
    drop(setup);

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for (index, (id, proof)) in playback.into_iter().enumerate() {
        let barrier = barrier.clone();
        let path_text = path_text.clone();
        workers.push(std::thread::spawn(move || {
            let db = Database::open(&path_text).unwrap();
            let revision = proof.segment_revision;
            let limit = ReviewDecisionLimit::new(0, 1, vec![("Sara".into(), 1)]).unwrap();
            barrier.wait();
            db.record_phone_human_decision_by_at_revision_with_operation_limit(
                id,
                "accept",
                Some("test"),
                "Sara",
                revision,
                &proof,
                &format!("523e4567-e89b-42d3-a456-42661417400{index}"),
                &review_operation_payload_hash(id, "accept", "test", "Sara"),
                "accept",
                "test",
                Some(&limit),
            )
        }));
    }
    barrier.wait();
    let results: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_ref().is_err_and(|error| error.to_string().contains(REVIEW_PILOT_LIMIT_REACHED)))
            .count(),
        1,
        "BEGIN IMMEDIATE must make exactly one racing connection lose the final slot"
    );
    let db = Database::open(&path_text).unwrap();
    let events: i64 = db.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
    assert_eq!(events, 1);
}

#[test]
fn pilot_hidden_keys_are_policy_bound_idempotent_quota_limited_and_immutable() {
    let db = make_db();
    let policy = "a".repeat(64);
    let other_policy = "b".repeat(64);
    let first = vec!["pilot-hidden-b".to_string(), "pilot-hidden-a".to_string(), "pilot-hidden-a".to_string()];
    assert_eq!(
        db.reserve_review_pilot_hidden_keys(&policy, 0, "  Sara  ", &first, 2).unwrap(),
        vec!["pilot-hidden-a", "pilot-hidden-b"]
    );
    assert_eq!(db.review_pilot_hidden_keys(&policy, 0, "sARA").unwrap(), vec!["pilot-hidden-a", "pilot-hidden-b"]);
    assert_eq!(
        db.reserve_review_pilot_hidden_keys(&policy, 0, "SARA", &first, 2).unwrap(),
        vec!["pilot-hidden-a", "pilot-hidden-b"],
        "a lost-response retry must return the complete durable set"
    );
    assert!(db
        .reserve_review_pilot_hidden_keys(&policy, 0, "Sara", &["pilot-hidden-c".into()], 2)
        .unwrap_err()
        .to_string()
        .contains("reviewer quota"));
    assert_eq!(db.review_pilot_hidden_keys(&policy, 0, "Sara").unwrap().len(), 2);

    // Once any grant binds a baseline, a policy-file replacement cannot mint a fresh budget there.
    let rebound = db
        .reserve_review_pilot_hidden_keys(&other_policy, 0, "Sara", &["other-policy-key".into()], 2)
        .unwrap_err()
        .to_string();
    assert!(rebound.contains("another policy identity"), "unexpected policy binding error: {rebound}");
    assert!(db.review_pilot_hidden_keys(&other_policy, 0, "Sara").unwrap().is_empty());

    for invalid in [
        db.reserve_review_pilot_hidden_keys("BAD", 0, "Sara", &[], 2),
        db.reserve_review_pilot_hidden_keys(&policy, -1, "Sara", &[], 2),
        db.reserve_review_pilot_hidden_keys(&policy, 0, " ", &[], 2),
        db.reserve_review_pilot_hidden_keys(&policy, 0, "Sara", &[], 1),
        db.reserve_review_pilot_hidden_keys(&policy, 0, "Sara", &["../bad".into()], 2),
    ] {
        assert!(matches!(invalid, Err(AppError::Validation(_))));
    }

    // Duplicate SQL retries are no-ops, while any semantic mutation is physically refused.
    assert_eq!(
        db.connection()
            .execute(
                "INSERT OR IGNORE INTO review_pilot_hidden_keys
                    (policy_sha256, after_review_event_id, reviewer, segment_id)
                 VALUES (?1, 0, 'sara', 'pilot-hidden-a')",
                [&policy],
            )
            .unwrap(),
        0
    );
    let update = db
        .connection()
        .execute(
            "UPDATE review_pilot_hidden_keys SET segment_id='changed'
              WHERE policy_sha256=?1 AND after_review_event_id=0 AND reviewer='Sara' AND segment_id='pilot-hidden-a'",
            [&policy],
        )
        .unwrap_err()
        .to_string();
    assert!(update.contains("append-only"), "unexpected UPDATE guard: {update}");
    let delete = db
        .connection()
        .execute("DELETE FROM review_pilot_hidden_keys WHERE policy_sha256=?1 AND after_review_event_id=0", [&policy])
        .unwrap_err()
        .to_string();
    assert!(delete.contains("append-only"), "unexpected DELETE guard: {delete}");

    db.reserve_review_pilot_hidden_keys(&policy, 0, "Hemn", &["hemn-a".into(), "hemn-b".into()], 2).unwrap();
    let global =
        db.reserve_review_pilot_hidden_keys(&policy, 0, "Ali", &["global-fifth".into()], 2).unwrap_err().to_string();
    assert!(global.contains("global quota"), "unexpected global quota error: {global}");

    let exposed_db = make_db();
    exposed_db
        .connection()
        .execute(
            "INSERT INTO review_events
                 (segment_id, reviewer, action, source, timestamp_ms)
             VALUES ('already-heard', 'sArA', 'accept', 'legacy', 1)",
            [],
        )
        .unwrap();
    let exposed = exposed_db
        .reserve_review_pilot_hidden_keys(&policy, 0, "Sara", &["already-heard".into()], 2)
        .unwrap_err()
        .to_string();
    assert!(exposed.contains("already seen"), "unexpected blind-key exposure error: {exposed}");
}

#[test]
fn pilot_spot_result_requires_exact_reservation_and_is_atomic_and_idempotent() {
    let db = make_db();
    let policy = "c".repeat(64);
    for id in ["pilot-result", "pilot-skip", "pilot-unreserved"] {
        db.insert_legacy_segment_fixture(&make_hidden_check_segment(id, &format!("/{id}.wav"), "ڕاست")).unwrap();
        ensure_test_audio_content_hash(&db, id);
    }
    db.reserve_review_pilot_hidden_keys(&policy, 0, "Sara", &["pilot-result".into(), "pilot-skip".into()], 2).unwrap();
    assert!(!db.review_pilot_hidden_key_resolved(&policy, 0, "Sara", "pilot-result").unwrap());

    let operation_id = "623e4567-e89b-42d3-a456-426614174001";
    let sara_proofs =
        [full_playback_proof(&db, "pilot-result", "Sara"), full_playback_proof(&db, "pilot-unreserved", "Sara")];
    for (candidate_policy, baseline, segment_id) in [
        ("e".repeat(64), 0, "pilot-result"),
        (policy.clone(), 1, "pilot-result"),
        (policy.clone(), 0, "pilot-unreserved"),
    ] {
        let proof = if segment_id == "pilot-result" { &sara_proofs[0] } else { &sara_proofs[1] };
        let error = db
            .record_pilot_spot_check_with_operation(
                &candidate_policy,
                baseline,
                segment_id,
                "Sara",
                "edit",
                "ڕاست",
                "ڕاست",
                Some(proof),
                operation_id,
                &review_operation_payload_hash(segment_id, "edit", "ڕاست", "Sara"),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("no durable reservation"), "unexpected authorization error: {error}");
    }
    let empty_counts: (i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM spot_checks),
                    (SELECT COUNT(*) FROM review_events),
                    (SELECT COUNT(*) FROM review_compensation_ledger)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(empty_counts, (0, 0, 0), "authorization refusal must roll back every consequence");

    let lowercase_proof = full_playback_proof(&db, "pilot-result", "sARA");
    db.record_pilot_spot_check_with_operation(
        &policy,
        0,
        "pilot-result",
        "sARA",
        "edit",
        "ڕاست",
        "ڕاست",
        Some(&lowercase_proof),
        operation_id,
        &review_operation_payload_hash("pilot-result", "edit", "ڕاست", "sARA"),
    )
    .unwrap();
    db.record_pilot_spot_check_with_operation(
        &policy,
        0,
        "pilot-result",
        "sARA",
        "edit",
        "ڕاست",
        "ڕاست",
        None,
        operation_id,
        &review_operation_payload_hash("pilot-result", "edit", "ڕاست", "sARA"),
    )
    .unwrap();
    let committed_counts: (i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM spot_checks WHERE segment_id='pilot-result'),
                    (SELECT COUNT(*) FROM review_events WHERE segment_id='pilot-result'),
                    (SELECT COUNT(*) FROM review_compensation_ledger WHERE segment_id='pilot-result')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(committed_counts, (1, 1, 1), "retry and case-only re-pair must commit exactly once");
    assert!(db.review_pilot_hidden_key_resolved(&policy, 0, "Sara", "pilot-result").unwrap());

    db.record_review_event_with_operation(
        "pilot-skip",
        "SARA",
        "skip",
        "couch",
        2,
        "623e4567-e89b-42d3-a456-426614174002",
        &review_operation_payload_hash("pilot-skip", "skip", "", "SARA"),
    )
    .unwrap();
    assert!(db.review_pilot_hidden_key_resolved(&policy, 0, "Sara", "pilot-skip").unwrap());
    assert!(!db.review_pilot_hidden_key_resolved(&policy, 0, "Sara", "pilot-unreserved").unwrap());
}

#[test]
fn hidden_judgement_requires_current_playback_inside_its_atomic_write() {
    let db = make_db();
    db.insert_legacy_segment_fixture(&make_hidden_check_segment("hidden-proof", "/hidden-proof.wav", "ڕاست")).unwrap();

    let missing = db
        .record_spot_check_with_operation(
            "hidden-proof",
            "Sara",
            "edit",
            "ڕاست",
            "ڕاست",
            None,
            "823e4567-e89b-42d3-a456-426614174001",
            &review_operation_payload_hash("hidden-proof", "edit", "ڕاست", "Sara"),
        )
        .unwrap_err()
        .to_string();
    assert!(missing.contains("E_NO_PLAYBACK_EVIDENCE"), "unexpected missing-proof error: {missing}");

    let stale = full_playback_proof(&db, "hidden-proof", "Sara");
    db.connection().execute("DROP TRIGGER speech_segments_v60_paid_identity_immutable_update", []).unwrap();
    db.connection()
        .execute(
            "UPDATE speech_segments SET audio_content_hash = ?1 WHERE id = 'hidden-proof'",
            [OTHER_AUDIO_CONTENT_HASH],
        )
        .unwrap();
    let moved = db
        .record_spot_check_with_operation(
            "hidden-proof",
            "Sara",
            "edit",
            "ڕاست",
            "ڕاست",
            Some(&stale),
            "823e4567-e89b-42d3-a456-426614174002",
            &review_operation_payload_hash("hidden-proof", "edit", "ڕاست", "Sara"),
        )
        .unwrap_err()
        .to_string();
    assert!(moved.contains(PLAYBACK_EVIDENCE_CHANGED), "unexpected stale-proof error: {moved}");
    let empty: (i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM spot_checks WHERE segment_id='hidden-proof'),
                    (SELECT COUNT(*) FROM review_events WHERE segment_id='hidden-proof'),
                    (SELECT COUNT(*) FROM review_compensation_ledger WHERE segment_id='hidden-proof')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(empty, (0, 0, 0), "every stale-proof consequence must roll back");

    let current = full_playback_proof(&db, "hidden-proof", "Sara");
    db.record_spot_check_with_operation(
        "hidden-proof",
        "Sara",
        "edit",
        "ڕاست",
        "ڕاست",
        Some(&current),
        "823e4567-e89b-42d3-a456-426614174003",
        &review_operation_payload_hash("hidden-proof", "edit", "ڕاست", "Sara"),
    )
    .unwrap();
    let committed: (i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM spot_checks WHERE segment_id='hidden-proof'),
                    (SELECT COUNT(*) FROM review_events WHERE segment_id='hidden-proof'),
                    (SELECT COUNT(*) FROM review_compensation_ledger WHERE segment_id='hidden-proof')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(committed, (1, 1, 1), "score, audit event, and credit must share one commit");
}

#[test]
fn hidden_result_rechecks_the_canonical_answer_key_and_skip_remains_zero_credit() {
    let db = make_db();
    db.insert_legacy_segment_fixture(&make_hidden_check_segment("hidden-key", "/hidden-key.wav", "کۆن")).unwrap();
    db.connection()
        .execute("UPDATE speech_segments SET verdict_transcript = 'نوێ' WHERE id = 'hidden-key'", [])
        .unwrap();
    let proof = full_playback_proof(&db, "hidden-key", "Sara");
    let stale_key = db
        .record_spot_check_with_operation(
            "hidden-key",
            "Sara",
            "edit",
            "کۆن",
            "کۆن",
            Some(&proof),
            "823e4567-e89b-42d3-a456-426614174004",
            &review_operation_payload_hash("hidden-key", "edit", "کۆن", "Sara"),
        )
        .unwrap_err()
        .to_string();
    assert!(stale_key.contains(HIDDEN_ANSWER_KEY_CHANGED), "unexpected stale-key error: {stale_key}");

    db.record_spot_check_with_operation(
        "hidden-key",
        "Sara",
        "skip",
        "",
        "نوێ",
        None,
        "823e4567-e89b-42d3-a456-426614174005",
        &review_operation_payload_hash("hidden-key", "skip", "", "Sara"),
    )
    .unwrap();
    let (event_count, ledger_count, basis_points, earned): (i64, i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM review_events WHERE segment_id='hidden-key'),
                    COUNT(*), MAX(rate_basis_points), COALESCE(SUM(delta_micro_iqd), 0)
               FROM review_compensation_ledger
              WHERE segment_id='hidden-key'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!((event_count, ledger_count), (1, 1));
    assert_eq!((basis_points, earned), (0, 0), "skip is quality telemetry, never paid judgement");
}

#[test]
fn pilot_reservation_backfills_completed_v58_checks_before_minting_fresh_keys() {
    let db = make_db();
    let policy = "f".repeat(64);
    for (index, id) in ["completed-hidden-a", "completed-hidden-b"].into_iter().enumerate() {
        db.insert_legacy_segment_fixture(&make_hidden_check_segment(id, &format!("/{id}.wav"), "ڕاست")).unwrap();
        let reviewer = if index == 0 { "Sara" } else { "sARA" };
        let proof = full_playback_proof(&db, id, reviewer);
        db.record_spot_check_with_operation(
            id,
            reviewer,
            "edit",
            "ڕاست",
            "ڕاست",
            Some(&proof),
            &format!("723e4567-e89b-42d3-a456-42661417400{index}"),
            &review_operation_payload_hash(id, "edit", "ڕاست", reviewer),
        )
        .unwrap();
    }

    let error =
        db.reserve_review_pilot_hidden_keys(&policy, 0, "Sara", &["would-be-third".into()], 2).unwrap_err().to_string();
    assert!(error.contains("reviewer quota"), "completed keys must consume the lifetime budget: {error}");
    assert!(db.review_pilot_hidden_keys(&policy, 0, "Sara").unwrap().is_empty(), "failed union is atomic");
    assert_eq!(
        db.reserve_review_pilot_hidden_keys(&policy, 0, "Sara", &[], 2).unwrap(),
        vec!["completed-hidden-a", "completed-hidden-b"],
        "the next safe call must durably backfill both completed session-era keys"
    );
}

#[test]
fn pilot_hidden_key_reservation_serializes_the_final_two_slots_across_connections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pilot-hidden-race.db");
    let path_text = path.to_string_lossy().to_string();
    let setup = Database::open(&path_text).unwrap();
    setup.initialize().unwrap();
    drop(setup);
    let policy = "9".repeat(64);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let mut workers = Vec::new();
    for id in ["race-hidden-a", "race-hidden-b", "race-hidden-c"] {
        let barrier = barrier.clone();
        let path_text = path_text.clone();
        let policy = policy.clone();
        workers.push(std::thread::spawn(move || {
            let db = Database::open(&path_text).unwrap();
            barrier.wait();
            db.reserve_review_pilot_hidden_keys(&policy, 0, "Sara", &[id.to_string()], 2)
        }));
    }
    barrier.wait();
    let results: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 2);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let db = Database::open(&path_text).unwrap();
    assert_eq!(db.review_pilot_hidden_keys(&policy, 0, "Sara").unwrap().len(), 2);
}

#[test]
fn schema_v58_upgrades_through_v67_without_reinterpreting_live_baseline_863() {
    let db = make_db();
    assert_eq!(crate::migrations::rollback(&db, 9).unwrap(), vec![67, 66, 65, 64, 63, 62, 61, 60, 59]);
    db.connection()
        .execute(
            "INSERT INTO review_events
                (id, segment_id, reviewer, action, compensation_action, source, timestamp_ms, duration_ms)
             VALUES (863, 'legacy-before-pilot', 'Owner', 'accept', 'accept', 'legacy', 0, 0)",
            [],
        )
        .unwrap();
    assert_eq!(crate::migrations::run_migrations(&db).unwrap(), vec![59, 60, 61, 62, 63, 64, 65, 66, 67]);
    let policy = "8".repeat(64);
    assert_eq!(
        db.reserve_review_pilot_hidden_keys(&policy, 863, "Sara", &["baseline-863-hidden".into()], 2).unwrap(),
        vec!["baseline-863-hidden"]
    );
}

#[test]
fn undo_and_its_signed_reversal_are_atomic_and_idempotent() {
    let db = make_db();
    let mut previous = make_segment("pay-undo", "/pay-undo.wav");
    previous.raw_transcript = "هەڵە".into();
    db.insert_segment(&previous).unwrap();
    ensure_test_audio_content_hash(&db, "pay-undo");
    let served_revision = db.segment_review_revision("pay-undo").unwrap().unwrap();
    let decided_revision = db
        .record_phone_human_decision_by_at_revision("pay-undo", "edit", Some("ڕاست"), "Sara", served_revision)
        .unwrap()
        .unwrap();
    let effect_id = latest_human_effect_id(&db, "pay-undo");
    assert_eq!(db.segment_review_revision("pay-undo").unwrap(), Some(decided_revision));
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 5_000_000);

    db.connection()
        .execute_batch(
            "CREATE TRIGGER fail_compensation_undo
             BEFORE INSERT ON review_compensation_ledger
             WHEN new.compensation_action = 'undo'
             BEGIN SELECT RAISE(ABORT, 'injected reversal failure'); END;",
        )
        .unwrap();
    let undo_operation = "00000000-0000-4000-8000-000000000101";
    assert!(db.undo_human_decision(effect_id, Some("Sara"), undo_operation).is_err());
    let still_decided = db.get_segment_by_id("pay-undo").unwrap().unwrap();
    assert_eq!(still_decided.human_decision.as_deref(), Some("edit"));
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 5_000_000);

    db.connection().execute_batch("DROP TRIGGER fail_compensation_undo;").unwrap();
    assert!(matches!(
        db.undo_human_decision(effect_id, Some("Sara"), undo_operation).unwrap(),
        HumanDecisionUndoOutcome::Applied { .. }
    ));
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 0);

    let entries: Vec<(String, i64, i64, Option<String>, String)> = db
        .connection()
        .prepare(
            "SELECT compensation_action, delta_micro_iqd, delta_corrected_ms, reverses_entry_id, entry_id
               FROM review_compensation_ledger WHERE segment_id = 'pay-undo' ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, "edit");
    assert_eq!(entries[0].1, 5_000_000);
    assert_eq!(entries[0].2, 1_000);
    assert_eq!(entries[1].0, "undo");
    assert_eq!(entries[1].1, -5_000_000);
    assert_eq!(entries[1].2, -1_000);
    assert_eq!(entries[1].3.as_deref(), Some(entries[0].4.as_str()));

    assert!(
        matches!(
            db.undo_human_decision(effect_id, Some("Sara"), undo_operation).unwrap(),
            HumanDecisionUndoOutcome::AlreadyApplied { .. }
        ),
        "the same operation UUID is an idempotent success and cannot append another reversal"
    );
    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM review_compensation_ledger WHERE segment_id = 'pay-undo'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn phone_undo_refuses_a_shadowed_canonical_alias_entitlement_then_unwinds_latest_first() {
    let db = make_db();
    for segment_id in ["alias-a", "alias-b"] {
        db.insert_segment(&make_segment(segment_id, &format!("/{segment_id}.wav"))).unwrap();
        ensure_test_audio_content_hash(&db, segment_id);
    }
    let shared_alignment: String = db
        .connection()
        .query_row("SELECT alignment_json FROM speech_segments WHERE id = 'alias-a'", [], |row| row.get(0))
        .unwrap();
    db.connection()
        .execute("UPDATE speech_segments SET alignment_json = ?1 WHERE id = 'alias-b'", [shared_alignment])
        .unwrap();

    let proof_a = canonical_policy4_phone_playback(&db, "alias-a", "Sara");
    let op_a = "00000000-0000-4000-8000-000000000401";
    let commit_a = db
        .record_phone_human_decision_by_at_revision_with_operation_limit(
            "alias-a",
            "accept",
            Some("test"),
            "Sara",
            proof_a.segment_revision,
            &proof_a,
            op_a,
            &review_operation_payload_hash("alias-a", "accept", "test", "Sara"),
            "accept",
            "test",
            None,
        )
        .unwrap()
        .unwrap();

    let proof_b = canonical_policy4_phone_playback(&db, "alias-b", "Sara");
    let op_b = "00000000-0000-4000-8000-000000000402";
    let commit_b = db
        .record_phone_human_decision_by_at_revision_with_operation_limit(
            "alias-b",
            "edit",
            Some("corrected"),
            "Sara",
            proof_b.segment_revision,
            &proof_b,
            op_b,
            &review_operation_payload_hash("alias-b", "edit", "corrected", "Sara"),
            "edit",
            "corrected",
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 5_000_000);

    let shadowed = db.undo_human_decision(commit_a.effect_event_id, Some("Sara"), op_a).unwrap_err();
    assert!(shadowed.to_string().contains("newer active entitlement mutation"), "{shadowed}");
    assert_eq!(
        db.get_segment_by_id("alias-a").unwrap().unwrap().human_decision.as_deref(),
        Some("accept"),
        "a refused alias undo must roll the segment update back"
    );
    assert_eq!(
        db.connection()
            .query_row("SELECT COUNT(*) FROM human_decision_effect_reversals", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 5_000_000);

    assert!(matches!(
        db.undo_human_decision(commit_b.effect_event_id, Some("Sara"), op_b).unwrap(),
        HumanDecisionUndoOutcome::Applied { .. }
    ));
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 500_000);
    assert!(matches!(
        db.undo_human_decision(commit_a.effect_event_id, Some("Sara"), op_a).unwrap(),
        HumanDecisionUndoOutcome::Applied { .. }
    ));
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 0);
}

#[test]
fn undo_of_a_redecision_reverses_only_that_decisions_delta() {
    let db = make_db();
    seed_for_provenance(&db, "pay-undo-redecision", "دەقی مۆدێل");
    let initial_revision = db.segment_review_revision("pay-undo-redecision").unwrap().unwrap();
    let accept_revision = db
        .record_phone_human_decision_by_at_revision(
            "pay-undo-redecision",
            "accept",
            Some("دەقی مۆدێل"),
            "Sara",
            initial_revision,
        )
        .unwrap()
        .unwrap();
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 500_000);

    let _edit_revision = db
        .record_phone_human_decision_by_at_revision(
            "pay-undo-redecision",
            "edit",
            Some("دەقی دەستکاریکراو"),
            "Sara",
            accept_revision,
        )
        .unwrap()
        .unwrap();
    let edit_effect_id = latest_human_effect_id(&db, "pay-undo-redecision");
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 5_000_000);
    let restored_accept_revision =
        match db.undo_human_decision(edit_effect_id, Some("Sara"), "00000000-0000-4000-8000-000000000102").unwrap() {
            HumanDecisionUndoOutcome::Applied { restored_revision, .. } => restored_revision,
            other => panic!("expected applied edit undo, got {other:?}"),
        };
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 500_000);

    // Exercise the opposite signed adjustment too: undoing edit -> reject must add back the 90%
    // reduction, not erase the earlier edit entitlement.
    let edit_revision = db
        .record_phone_human_decision_by_at_revision(
            "pay-undo-redecision",
            "edit",
            Some("دەقی دەستکاریکراو"),
            "Sara",
            restored_accept_revision,
        )
        .unwrap()
        .unwrap();
    let _reject_revision = db
        .record_phone_human_decision_by_at_revision("pay-undo-redecision", "reject", None, "Sara", edit_revision)
        .unwrap()
        .unwrap();
    let reject_effect_id = latest_human_effect_id(&db, "pay-undo-redecision");
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 500_000);
    assert!(matches!(
        db.undo_human_decision(reject_effect_id, Some("Sara"), "00000000-0000-4000-8000-000000000103",).unwrap(),
        HumanDecisionUndoOutcome::Applied { .. }
    ));
    assert_eq!(db.review_compensation_summary("Sara").unwrap().earned_micro_iqd, 5_000_000);

    let entries: Vec<(String, i64, i64, Option<String>, String)> = db
        .connection()
        .prepare(
            "SELECT compensation_action, delta_micro_iqd, delta_corrected_ms, reverses_entry_id, entry_id
               FROM review_compensation_ledger WHERE segment_id = 'pay-undo-redecision' ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let actions_and_deltas: Vec<(&str, i64)> = entries.iter().map(|entry| (entry.0.as_str(), entry.1)).collect();
    assert_eq!(
        actions_and_deltas,
        vec![
            ("accept", 500_000),
            ("edit", 4_500_000),
            ("undo", -4_500_000),
            ("edit", 4_500_000),
            ("reject", -4_500_000),
            ("undo", 4_500_000),
        ]
    );
    let corrected_deltas: Vec<i64> = entries.iter().map(|entry| entry.2).collect();
    assert_eq!(corrected_deltas, vec![0, 1_000, -1_000, 1_000, -1_000, 1_000]);
    assert_eq!(entries[2].3.as_deref(), Some(entries[1].4.as_str()));
    assert_eq!(entries[5].3.as_deref(), Some(entries[4].4.as_str()));
    assert_eq!(db.review_compensation_summary("Sara").unwrap().corrected_audio_ms, 1_000);
}

#[test]
fn paid_audio_identity_is_immutable_and_effect_bound_segment_deletion_is_refused() {
    let db = make_db();
    db.insert_segment(&make_segment("pay-delete-control", "/pay-delete-control.wav")).unwrap();
    assert_eq!(
        db.connection()
            .execute("UPDATE speech_segments SET duration_ms=1234 WHERE id='pay-delete-control'", [])
            .unwrap(),
        1,
        "an authority-free row must remain repairable"
    );

    let mut segment = make_segment("pay-delete", "/pay-delete.wav");
    segment.duration_ms = 1_234;
    segment.raw_transcript = "هەڵە".into();
    db.insert_segment(&segment).unwrap();
    let playback = canonical_policy4_phone_playback(&db, "pay-delete", "Sara");
    let revision = playback.segment_revision;
    db.record_phone_human_decision_by_at_revision_with_operation_limit(
        "pay-delete",
        "edit",
        Some("ڕاست"),
        "Sara",
        revision,
        &playback,
        "00000000-0000-4000-8000-000000000501",
        &review_operation_payload_hash("pay-delete", "edit", "ڕاست", "Sara"),
        "edit",
        "ڕاست",
        None,
    )
    .unwrap()
    .unwrap();
    let earned = 1_234 * 5_000;
    let summary = db.review_compensation_summary("Sara").unwrap();
    assert_eq!(summary.earned_micro_iqd, earned);
    assert_eq!(summary.corrected_audio_ms, 1_234);

    let mutation = db
        .connection()
        .execute("UPDATE speech_segments SET duration_ms = 999999 WHERE id = 'pay-delete'", [])
        .unwrap_err();
    assert!(mutation.to_string().contains("paid policy-4 source identity is immutable"), "{mutation}");
    assert_eq!(db.get_segment_by_id("pay-delete").unwrap().unwrap().duration_ms, 1_234);
    assert_eq!(
        db.review_compensation_summary("Sara").unwrap().earned_micro_iqd,
        earned,
        "a refused identity mutation must leave the paid duration snapshot authoritative"
    );
    assert!(
        db.delete_segment("pay-delete").is_err(),
        "deleting an effect-bound correction would destroy immutable provenance and must be refused"
    );
    assert!(db.get_segment_by_id("pay-delete").unwrap().is_some());
    let summary = db.review_compensation_summary("Sara").unwrap();
    assert_eq!(summary.earned_micro_iqd, earned);
    assert_eq!(summary.corrected_audio_ms, 1_234);
    let snapshots: (i64, i64) = db
        .connection()
        .query_row(
            "SELECT (SELECT duration_ms FROM review_events WHERE segment_id = 'pay-delete'),
                    (SELECT duration_ms FROM review_compensation_ledger WHERE segment_id = 'pay-delete')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(snapshots, (1_234, 1_234));
}

#[test]
fn deletion_is_allowed_only_for_authority_free_segments() {
    let assert_refused = |db: &Database, id: &str| {
        let error = db.delete_segment(id).expect_err("durable review authority must block deletion");
        assert!(matches!(error, AppError::Validation(_)), "{id}: {error}");
        assert!(
            error.to_string().contains("durable review authority"),
            "{id}: deletion refusal must be explicit: {error}"
        );
        assert!(db.get_segment_by_id(id).unwrap().is_some(), "{id}: refused deletion must preserve the row");
    };

    let db = make_db();
    db.insert_segment(&make_segment("delete-clean", "/delete-clean.wav")).unwrap();
    db.delete_segment("delete-clean").expect("an authority-free segment remains deletable");
    assert!(db.get_segment_by_id("delete-clean").unwrap().is_none());

    db.insert_segment(&make_segment("delete-flag", "/delete-flag.wav")).unwrap();
    db.record_review_flag("delete-flag", "human escalation evidence", "00000000-0000-4000-8000-000000000401").unwrap();
    assert_refused(&db, "delete-flag");

    insert_playback_segment(&db, "delete-playback", 1_000);
    let playback_revision = db.segment_review_revision("delete-playback").unwrap().unwrap();
    db.record_playback_receipt_raw(&receipt(
        "delete-playback",
        playback_revision,
        TEST_AUDIO_CONTENT_HASH,
        1_000,
        1_000,
    ))
    .unwrap();
    assert_refused(&db, "delete-playback");

    db.insert_segment(&make_segment("delete-pay", "/delete-pay.wav")).unwrap();
    ensure_test_audio_content_hash(&db, "delete-pay");
    db.record_review_event_with_operation(
        "delete-pay",
        "Sara",
        "skip",
        "couch",
        1,
        "00000000-0000-4000-8000-000000000402",
        &review_operation_payload_hash("delete-pay", "skip", "", "Sara"),
    )
    .unwrap();
    assert_refused(&db, "delete-pay");

    db.insert_segment(&make_segment("delete-spot", "/delete-spot.wav")).unwrap();
    db.connection()
        .execute(
            "INSERT INTO spot_checks
                (segment_id, reviewer, action, submitted_transcript, expected_transcript, noticed, cer)
             VALUES ('delete-spot', 'Sara', 'edit', 'right', 'right', 1, 0.0)",
            [],
        )
        .unwrap();
    assert_refused(&db, "delete-spot");

    let legacy = make_db();
    assert_eq!(crate::migrations::rollback(&legacy, 8).unwrap(), vec![67, 66, 65, 64, 63, 62, 61, 60]);
    let mut reviewed = make_segment("delete-legacy", "/delete-legacy.wav");
    reviewed.verified = true;
    reviewed.human_decision = Some("accept".into());
    reviewed.verdict = Some("human_accept".into());
    reviewed.verdict_transcript = Some("legacy truth".into());
    reviewed.annotated_transcript = Some("legacy truth".into());
    legacy.insert_segment_full(&reviewed).unwrap();
    assert_eq!(crate::migrations::run_migrations(&legacy).unwrap(), vec![60, 61, 62, 63, 64, 65, 66, 67]);
    assert_refused(&legacy, "delete-legacy");

    db.insert_segment(&make_segment("delete-batch-clean", "/delete-batch-clean.wav")).unwrap();
    db.insert_segment(&make_segment("delete-batch-reviewed", "/delete-batch-reviewed.wav")).unwrap();
    db.connection().execute("UPDATE speech_segments SET verified = 1 WHERE id = 'delete-batch-reviewed'", []).unwrap();
    let batch_error =
        db.delete_segments_batch(&["delete-batch-clean".into(), "delete-batch-reviewed".into()]).unwrap_err();
    assert!(matches!(batch_error, AppError::Validation(_)), "{batch_error}");
    assert!(db.get_segment_by_id("delete-batch-clean").unwrap().is_some(), "batch refusal is atomic");
    assert!(db.get_segment_by_id("delete-batch-reviewed").unwrap().is_some());
}

#[test]
fn settlement_is_exact_case_insensitive_and_retry_idempotent() {
    let db = make_db();
    let through = record_payable_edit(&db, "pay-settle-once", "Sara", 1_234);
    let exact = 1_234 * 5_000;

    let first = db.record_review_compensation_settlement("  Sara  ", through, "  payout-001  ").unwrap();
    assert_eq!(first.reviewer, "Sara");
    assert_eq!(first.from_ledger_id_exclusive, 0);
    assert_eq!(first.through_ledger_id_inclusive, through);
    assert_eq!(first.allocated_micro_iqd, exact);
    assert_eq!(first.payout_reference, "payout-001");
    let summary = db.review_compensation_summary("sARA").unwrap();
    assert_eq!(summary.earned_micro_iqd, exact);
    assert_eq!(summary.settled_micro_iqd, exact);
    assert_eq!(summary.outstanding_micro_iqd, 0);

    let retry = db.record_review_compensation_settlement("sARA", through, "payout-001").unwrap();
    assert_eq!(retry, first, "a lost-response retry returns the durable settlement it already created");
    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM review_compensation_settlements", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);

    let later = record_payable_edit(&db, "pay-settle-later", "Sara", 1_000);
    assert!(
        db.record_review_compensation_settlement("Sara", later, "payout-001").is_err(),
        "reusing a payout reference for a different boundary is a mismatch, never a retry"
    );
    assert!(
        db.record_review_compensation_settlement("Hemn", through, "payout-001").is_err(),
        "the same external reference cannot be rebound to another reviewer"
    );
    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM review_compensation_settlements", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn settlement_rejects_overlap_and_the_database_recomputes_every_amount() {
    let db = make_db();
    let first_through = record_payable_edit(&db, "pay-settle-range-1", "Sara", 1_000);
    db.record_review_compensation_settlement("Sara", first_through, "payout-range-1").unwrap();
    assert!(
        db.record_review_compensation_settlement("Sara", first_through, "payout-overlap").is_err(),
        "a second reference cannot allocate an already-settled interval"
    );

    let second_through = record_payable_edit(&db, "pay-settle-range-2", "Sara", 1_000);
    let second = db.record_review_compensation_settlement("sara", second_through, "payout-range-2").unwrap();
    assert_eq!(second.from_ledger_id_exclusive, first_through);
    assert_eq!(second.allocated_micro_iqd, 5_000_000);

    let third_through = record_payable_edit(&db, "pay-settle-range-3", "Sara", 2_000);
    let forged = db.connection().execute(
        "INSERT INTO review_compensation_settlements
            (settlement_id, policy_version, reviewer, from_ledger_id_exclusive,
             through_ledger_id_inclusive, allocated_micro_iqd, payout_reference)
         VALUES ('forged-settlement', ?1, 'SARA', ?2, ?3, 1, 'forged-payout')",
        rusqlite::params![REVIEW_PAY_POLICY_VERSION, second_through, third_through],
    );
    assert!(forged.is_err(), "the SQL trigger must reject even a direct forged amount");
    let third = db.record_review_compensation_settlement("Sara", third_through, "payout-range-3").unwrap();
    assert_eq!(third.allocated_micro_iqd, 10_000_000);
    let summary = db.review_compensation_summary("SARA").unwrap();
    assert_eq!(summary.settled_micro_iqd, 20_000_000);
    assert_eq!(summary.outstanding_micro_iqd, 0);
}

#[test]
fn a_negative_redecision_adjustment_is_settled_exactly() {
    let db = make_db();
    let edit_through = record_payable_edit(&db, "pay-negative-settlement", "Sara", 1_000);
    let initial = db.record_review_compensation_settlement("Sara", edit_through, "payout-positive").unwrap();
    assert_eq!(initial.allocated_micro_iqd, 5_000_000);

    let revision = db.segment_review_revision("pay-negative-settlement").unwrap().unwrap();
    db.record_phone_human_decision_by_at_revision("pay-negative-settlement", "reject", None, "sARA", revision)
        .unwrap()
        .unwrap();
    let reject_through: i64 = db
        .connection()
        .query_row(
            "SELECT id FROM review_compensation_ledger
              WHERE segment_id = 'pay-negative-settlement' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let before_adjustment = db.review_compensation_summary("Sara").unwrap();
    assert_eq!(before_adjustment.earned_micro_iqd, 500_000);
    assert_eq!(before_adjustment.corrected_audio_ms, 0);
    assert_eq!(before_adjustment.settled_micro_iqd, 5_000_000);
    assert_eq!(before_adjustment.outstanding_micro_iqd, -4_500_000);

    let adjustment =
        db.record_review_compensation_settlement("SARA", reject_through, "payout-negative-adjustment").unwrap();
    assert_eq!(adjustment.from_ledger_id_exclusive, edit_through);
    assert_eq!(adjustment.allocated_micro_iqd, -4_500_000);
    let after_adjustment = db.review_compensation_summary("sara").unwrap();
    assert_eq!(after_adjustment.earned_micro_iqd, 500_000);
    assert_eq!(after_adjustment.settled_micro_iqd, 500_000);
    assert_eq!(after_adjustment.outstanding_micro_iqd, 0);
}

#[test]
fn review_queue_never_serves_a_clip_whose_audio_file_is_gone() {
    // MEASURED 2026-08-15: three staging folders under SoraniVoice_PC_ ceased to exist, taking the
    // audio for 1,031 clips (7% of the library) with them. 536 were still pending and — because this
    // queue is oldest-first and that was the OLDEST material — they sat at the head of the queue, so
    // every reviewer who opened a link was handed unplayable clips first. The rows are perfectly
    // well-formed, so nothing that reads the database can see it; only the disk can.
    //
    // A reviewer who cannot listen can only guess at text they never heard, and this is a VERBATIM
    // corpus: an unheard "looks good" is worse than no decision at all.
    let db = make_db();
    let audio = tempfile::tempdir().unwrap();
    let present = audio.path().join("present.wav");
    std::fs::write(&present, b"RIFF").unwrap();

    let mut playable = make_segment("playable", &present.to_string_lossy());
    playable.raw_transcript = "دەقی ڕاست".to_string();
    let mut orphaned = make_segment("orphaned", &audio.path().join("deleted.wav").to_string_lossy());
    orphaned.raw_transcript = "دەقی ڕاست".to_string();
    for seg in [&playable, &orphaned] {
        db.insert_segment(seg).unwrap();
    }

    let served = db.pending_segment_ids().unwrap();

    assert_eq!(served, vec!["playable".to_string()], "only clips a reviewer can actually hear: {served:?}");
}

#[test]
fn segment_pages_use_stable_keysets_and_lightweight_rows() {
    let db = make_db();
    // PINNED timestamps, newest-first by construction. `insert_segment` stamps created_at = now at
    // one-second resolution, so five rapid inserts USUALLY share one second and the id tiebreak
    // yields a,b,c,d,e — but when the wall clock ticks mid-insert the rows split across two seconds
    // and "newest" reorders them. That is exactly how this test failed twice and passed twice across
    // four otherwise-identical sweep runs on 2026-08-16. A test about ORDERING must own its clock.
    for (id, created) in [
        ("a", "2026-01-01 10:00:05"),
        ("b", "2026-01-01 10:00:04"),
        ("c", "2026-01-01 10:00:03"),
        ("d", "2026-01-01 10:00:02"),
        ("e", "2026-01-01 10:00:01"),
    ] {
        let mut segment = make_segment(id, &format!("/{id}.wav"));
        segment.alignment_json = Some(r#"{"version":1,"words":[]}"#.into());
        // Keep the row deleted below authority-free. A different row carries the large review
        // payload so the page still proves that projection is lightweight.
        segment.evidence_json = (id == "b").then(|| r#"{"large":"payload"}"#.into());
        segment.created_at = Some(created.to_string());
        if segment.evidence_json.is_some() {
            db.insert_legacy_segment_fixture(&segment).unwrap();
        } else {
            db.insert_segment_full(&segment).unwrap();
        }
    }

    let first = db.get_segments_page(None, None, "newest", 2, None).unwrap();
    assert_eq!(first.items.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(first.total, 5);
    assert_eq!(first.revisions.len(), first.items.len());
    for item in &first.items {
        assert_eq!(
            first.revisions.get(&item.id).copied(),
            db.segment_review_revision(&item.id).unwrap(),
            "each lightweight row carries its database-owned compare-and-swap revision"
        );
    }
    assert!(first.items.iter().all(|s| s.alignment_json.is_none() && s.evidence_json.is_none()));

    // This id would sort ahead of the continuation point, but was inserted after the frozen anchor.
    db.insert_segment(&make_segment("00-new", "/new.wav")).unwrap();
    db.delete_segment("a").unwrap();
    let second = db.get_segments_page(None, None, "newest", 2, first.next_cursor.as_deref()).unwrap();
    assert_eq!(second.items.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["c", "d"]);
    assert_eq!(second.total, 5, "cursor total remains the anchored walk's original membership");
    let third = db.get_segments_page(None, None, "newest", 2, second.next_cursor.as_deref()).unwrap();
    assert_eq!(third.items.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["e"]);
    assert!(third.next_cursor.is_none());
}

#[test]
fn segment_page_cursor_is_opaque_versioned_and_scope_bound() {
    let db = make_db();
    db.insert_segments_batch(&[make_segment("a", "/a.wav"), make_segment("b", "/b.wav")]).unwrap();
    let first = db.get_segments_page(None, None, "newest", 1, None).unwrap();
    let cursor = first.next_cursor.as_deref().unwrap();
    assert!(!cursor.chars().all(|c| c.is_ascii_digit()));
    assert!(db.get_segments_page(None, None, "oldest", 1, Some(cursor)).is_err());
    assert!(db.get_segments_page(Some(true), None, "newest", 1, Some(cursor)).is_err());
    assert!(db.get_segments_page(None, None, "newest", 1, Some("not_a_cursor")).is_err());
}

#[test]
fn escalation_review_pages_are_versioned_complete_filtered_and_cursor_bound() {
    let db = make_db();
    for (index, id) in ["e1", "e2", "e3", "e4", "pending", "decided"].iter().enumerate() {
        let mut segment = make_segment(id, &format!("/{id}.wav"));
        segment.alignment_json = Some(format!(
            r#"{{"source_start_ms":{},"source_end_ms":{},"chunk_index":0,"chunk_count":1}}"#,
            index * 1_000,
            (index + 1) * 1_000
        ));
        db.insert_segment(&segment).unwrap();
    }
    for (index, id) in ["e1", "e2", "e3", "e4", "decided"].iter().enumerate() {
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET escalated = 1, verdict = 'escalated', rationale = 'needs review',
                        evidence_json = '{\"source\":\"test\"}', agreement_score = ?2
                  WHERE id = ?1",
                rusqlite::params![id, (index + 1) as f64 / 10.0],
            )
            .unwrap();
    }
    db.connection().execute("UPDATE speech_segments SET human_decision = 'accept' WHERE id = 'decided'", []).unwrap();
    db.connection().execute("UPDATE speech_segments SET verified = 1 WHERE id = 'e4'", []).unwrap();

    let first = db.get_escalation_review_page(2, None, None).unwrap();
    assert_eq!(
        first.total, 4,
        "pending and already-decided rows are outside escalation scope; escalation itself remains authoritative even on a legacy verified row"
    );
    assert_eq!(first.items.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["e1", "e2"]);
    assert!(first.items.iter().all(|row| row.alignment_json.is_some() && row.evidence_json.is_some()));
    assert!(first.items.iter().all(|row| first.revisions.contains_key(&row.id)));

    let cursor = first.next_cursor.as_deref().expect("four rows require a continuation");
    let second = db.get_escalation_review_page(2, Some(cursor), None).unwrap();
    assert_eq!(second.items.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["e3", "e4"]);
    assert!(second.next_cursor.is_none());
    assert!(
        db.get_segments_page(Some(false), None, "suspectFirst", 2, Some(cursor)).is_err(),
        "an escalation cursor must not be redeemable against the pending/library scope"
    );

    let focus: std::collections::HashSet<String> = ["e1", "e4"].iter().map(|id| id.to_string()).collect();
    let focused = db.get_escalation_review_page(10, None, Some(&focus)).unwrap();
    assert_eq!(focused.total, 2);
    assert!(focused.focus_narrowed);
    assert_eq!(focused.items.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["e1", "e4"]);
}

#[test]
fn desktop_review_page_narrows_to_the_voice_focus_but_the_library_does_not() {
    // Owner report 2026-08-20: with a voice focus active, the phones were narrowed but the DESKTOP
    // review queue still played guests — the focus lived only on the couch path. The review page
    // reads through get_segments_page_focused; this pins that the allow-list governs its rows AND
    // its total, that the unfocused wrapper still serves the whole library, and that a cursor minted
    // under one focus set dies when the set changes (its total was computed under the old list).
    let db = make_db();
    db.insert_segments_batch(&[
        make_segment("host-1", "/h1.wav"),
        make_segment("guest-1", "/g1.wav"),
        make_segment("host-2", "/h2.wav"),
    ])
    .unwrap();
    let focus: std::collections::HashSet<String> = ["host-1", "host-2"].iter().map(|s| s.to_string()).collect();

    let page = db.get_segments_page_focused(None, None, "oldest", 10, None, Some(&focus)).unwrap();
    let mut ids: Vec<&str> = page.items.iter().map(|s| s.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["host-1", "host-2"], "a guest clip must never enter the focused queue");
    assert_eq!(page.total, 2, "the total counts the focused queue, not the library");

    let library = db.get_segments_page(None, None, "oldest", 10, None).unwrap();
    assert_eq!(library.total, 3, "the queue narrows, the library does not");

    // A cursor from a focused walk is scope-bound to that exact id set.
    let first = db.get_segments_page_focused(None, None, "oldest", 1, None, Some(&focus)).unwrap();
    let cursor = first.next_cursor.as_deref().expect("two focused rows leave a second page");
    let edited: std::collections::HashSet<String> = ["host-1"].iter().map(|s| s.to_string()).collect();
    assert!(
        db.get_segments_page_focused(None, None, "oldest", 1, Some(cursor), Some(&edited)).is_err(),
        "a cursor must not survive an edit to the focus set"
    );
    assert!(
        db.get_segments_page(None, None, "oldest", 1, Some(cursor)).is_err(),
        "a focused cursor must not be redeemable against the full library"
    );
    assert!(
        db.get_segments_page_focused(None, None, "oldest", 1, Some(cursor), Some(&focus)).is_ok(),
        "the same set keeps paging"
    );
}

#[test]
fn resume_set_lists_exactly_the_files_this_directory_already_holds() {
    // What a re-run of a directory import passes as `resume_completed`. If this under-reports, the
    // re-run persists those files a SECOND time under the same audio_path (the 2026-08-14 shape:
    // one folder re-import doubled 494 already-reviewed clips). If it over-reports, real work is
    // skipped and never imported at all.
    let db = make_db();
    for (id, path) in [
        ("a", r"D:\Set\wavs\lamo_000001.wav"),
        ("b", r"D:\Set\wavs\lamo_000002.wav"),
        ("c", r"D:\Set\wavs_other\lamo_000003.wav"), // a SIBLING dir sharing the prefix's start
        ("d", r"D:\Elsewhere\lamo_000004.wav"),
    ] {
        db.insert_segment(&make_segment(id, path)).unwrap();
    }

    let mut found = db.audio_paths_with_segments_under(r"D:\Set\wavs\").unwrap();
    found.sort();
    assert_eq!(
        found,
        vec![r"D:\Set\wavs\lamo_000001.wav".to_string(), r"D:\Set\wavs\lamo_000002.wav".to_string()],
        "only files under the asked-for directory — the sibling `wavs_other` must not be adopted"
    );

    assert!(
        db.audio_paths_with_segments_under(r"D:\Nothing\").unwrap().is_empty(),
        "a directory the library has never seen resumes nothing"
    );
    // Separator and case are cosmetic on Windows; a path differing only in those is the SAME file
    // and must still be recognised, or the re-run imports it a second time.
    assert_eq!(db.audio_paths_with_segments_under(r"d:/set/wavs/").unwrap().len(), 2);
}

#[test]
fn the_escalation_queue_obeys_the_voice_focus() {
    // The Inbox is a SERVING PATH — it plays these clips, mints playback receipts for them and
    // records verdicts against them — so the focus governs it exactly as it governs the review page.
    // Review 2026-08-20: narrowing the review page still left this queue handing out the guest clips
    // the focus exists to skip, which is the complaint that started the whole thread.
    let db = make_db();
    for id in ["host-1", "guest-1", "host-2"] {
        let mut s = make_segment(id, &format!("/{id}.wav"));
        s.escalated = true;
        db.insert_legacy_segment_fixture(&s).unwrap();
    }
    let focus: std::collections::HashSet<String> = ["host-1", "host-2"].iter().map(|s| s.to_string()).collect();

    let all = db.get_escalation_queue(10, None).unwrap();
    assert_eq!(all.len(), 3, "unfocused, the whole escalation backlog is served");

    let narrowed = db.get_escalation_queue(10, Some(&focus)).unwrap();
    let mut ids: Vec<&str> = narrowed.iter().map(|s| s.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["host-1", "host-2"], "a guest clip must never reach the Inbox while a focus is active");

    let ghost: std::collections::HashSet<String> = ["nobody"].iter().map(|s| s.to_string()).collect();
    assert!(
        db.get_escalation_queue(10, Some(&ghost)).unwrap().is_empty(),
        "a focus naming nothing in the backlog serves nothing, never everything"
    );
}

#[test]
fn every_segment_sort_walks_each_row_exactly_once() {
    let db = make_db();
    for (index, id) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
        let mut segment = make_segment(id, &format!("/{id}.wav"));
        segment.created_at = Some(format!("2026-08-{:02}T00:00:00Z", index + 1));
        segment.duration_ms = 1000 + index as i64 * 100;
        segment.verified = index % 2 == 0;
        segment.confidence = Some(0.2 + index as f64 * 0.1);
        segment.ctc_score = Some(-4.0 + index as f64 * 0.2);
        segment.escalated = index == 3;
        segment.agreement_score = Some(0.3 + index as f64 * 0.1);
        segment.snr_db = Some(if index == 2 { 2.0 } else { 20.0 });
        db.insert_legacy_segment_fixture(&segment).unwrap();
    }

    for sort in ["newest", "oldest", "duration", "verified", "confidence", "activeLearning", "suspectFirst"] {
        let mut cursor = None;
        let mut ids = Vec::new();
        loop {
            let page = db.get_segments_page(None, None, sort, 2, cursor.as_deref()).unwrap();
            ids.extend(page.items.into_iter().map(|row| row.id));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), 5, "{sort} must return all rows: {ids:?}");
        assert_eq!(unique.len(), 5, "{sort} must not duplicate rows: {ids:?}");
    }
}

#[test]
fn review_revision_changes_on_every_update_even_inside_one_second() {
    let db = make_db();
    db.insert_segment(&make_segment("revision", "/revision.wav")).unwrap();
    let first = db.segment_review_revision("revision").unwrap().unwrap();

    // Both writes ordinarily receive the same second-resolution updated_at. The review fence must
    // still observe each one, including a metadata writer that deliberately does not touch updated_at.
    db.connection().execute("UPDATE speech_segments SET speaker_id = 'A' WHERE id = 'revision'", []).unwrap();
    let second = db.segment_review_revision("revision").unwrap().unwrap();
    db.set_speaker_change_score("revision", 0.42).unwrap();
    let third = db.segment_review_revision("revision").unwrap().unwrap();

    assert_eq!(second, first + 1);
    assert_eq!(third, second + 1);
    let (_, paired) = db.get_segment_by_id_with_revision("revision").unwrap().unwrap();
    assert_eq!(paired, third, "row and revision are returned from one result row");
}

#[test]
fn phone_decision_revision_cas_has_no_side_effects_on_a_stale_row() {
    let db = make_db();
    let mut segment = make_segment("decision-cas", "/decision-cas.wav");
    segment.raw_transcript = "هەڵە".into();
    db.insert_segment(&segment).unwrap();
    ensure_test_audio_content_hash(&db, "decision-cas");
    let served_revision = db.segment_review_revision("decision-cas").unwrap().unwrap();

    db.update_speaker_id("decision-cas", Some("SPEAKER_01")).unwrap();
    let current_revision = db.segment_review_revision("decision-cas").unwrap().unwrap();
    assert!(current_revision > served_revision);

    let stale = db
        .record_phone_human_decision_by_at_revision("decision-cas", "edit", Some("ڕاست"), "Sara", served_revision)
        .unwrap();
    assert!(stale.is_none(), "a stale decision is a clean CAS miss");
    let untouched = db.get_segment_by_id("decision-cas").unwrap().unwrap();
    assert!(!untouched.verified);
    assert!(untouched.human_decision.is_none());
    let examples: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM agent_examples WHERE segment_id = 'decision-cas'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(examples, 0, "a CAS miss must not mint learning data");

    let applied = db
        .record_phone_human_decision_by_at_revision("decision-cas", "edit", Some("ڕاست"), "Sara", current_revision)
        .unwrap();
    assert!(applied.is_some());
}

#[test]
fn phone_undo_rolls_back_every_effect_when_reversal_insert_fails_then_retries_cleanly() {
    let db = make_db();
    let mut segment = make_segment("undo-atomic", "/undo-atomic.wav");
    segment.raw_transcript = "هەڵە".into();
    db.insert_segment(&segment).unwrap();
    ensure_test_audio_content_hash(&db, "undo-atomic");
    let served_revision = db.segment_review_revision("undo-atomic").unwrap().unwrap();
    let decided_revision = db
        .record_phone_human_decision_by_at_revision("undo-atomic", "edit", Some("ڕاست"), "Sara", served_revision)
        .unwrap()
        .unwrap();
    let effect_id = latest_human_effect_id(&db, "undo-atomic");
    let operation_id = "00000000-0000-4000-8000-000000000104";

    db.connection()
        .execute_batch(
            "CREATE TRIGGER fail_effect_reversal
             BEFORE INSERT ON human_decision_effect_reversals
             BEGIN SELECT RAISE(ABORT, 'injected effect reversal failure'); END;",
        )
        .unwrap();
    assert!(db.undo_human_decision(effect_id, Some("Sara"), operation_id).is_err());

    let still_decided = db.get_segment_by_id("undo-atomic").unwrap().unwrap();
    assert!(still_decided.verified, "row restore must roll back with the failed effect reversal");
    assert_eq!(still_decided.reviewed_by.as_deref(), Some("Sara"));
    assert_eq!(db.segment_review_revision("undo-atomic").unwrap(), Some(decided_revision));
    assert_eq!(
        db.connection()
            .query_row("SELECT COUNT(*) FROM effective_human_decision_effects_v60 WHERE id = ?1", [effect_id], |row| {
                row.get::<_, i64>(0)
            },)
            .unwrap(),
        1
    );

    db.connection().execute_batch("DROP TRIGGER fail_effect_reversal;").unwrap();
    assert!(matches!(
        db.undo_human_decision(effect_id, Some("Sara"), operation_id).unwrap(),
        HumanDecisionUndoOutcome::Applied { .. }
    ));
    let restored = db.get_segment_by_id("undo-atomic").unwrap().unwrap();
    assert!(!restored.verified);
    assert!(restored.human_decision.is_none());
    assert!(restored.reviewed_by.is_none());
    assert_eq!(
        db.connection()
            .query_row("SELECT COUNT(*) FROM effective_human_decision_effects_v60 WHERE id = ?1", [effect_id], |row| {
                row.get::<_, i64>(0)
            },)
            .unwrap(),
        0
    );
}
#[test]
fn alignment_cas_never_overwrites_concurrent_boundary_metadata() {
    let db = make_db();
    let original = r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":2,"words":[]}"#;
    let concurrent = r#"{"source_start_ms":1000,"source_end_ms":2000,"chunk_index":1,"chunk_count":2,"words":[]}"#;
    let inferred = r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":2,"words":[{"word":"x","start":0.0,"end":0.4}]}"#;
    let mut segment = make_segment("align-cas", "/align-cas.wav");
    segment.alignment_json = Some(original.into());
    db.insert_segment(&segment).unwrap();

    // Simulate the boundary editor winning while forced alignment is still running.
    db.update_segment_alignment("align-cas", concurrent, "energy_heuristic").unwrap();
    let changed =
        db.update_segment_alignment_if_unchanged("align-cas", Some(original), inferred, "ctc_forced").unwrap();
    assert!(!changed, "stale inference must lose the compare-and-swap");
    let row = db.get_segment_by_id("align-cas").unwrap().unwrap();
    assert_eq!(row.alignment_json.as_deref(), Some(concurrent));
    assert_eq!(row.alignment_quality.as_deref(), Some("energy_heuristic"));

    let applied =
        db.update_segment_alignment_if_unchanged("align-cas", Some(concurrent), inferred, "ctc_forced").unwrap();
    assert!(applied);
    let row = db.get_segment_by_id("align-cas").unwrap().unwrap();
    assert_eq!(row.alignment_json.as_deref(), Some(inferred));
    assert_eq!(row.alignment_quality.as_deref(), Some("ctc_forced"));
}

// ---------------------------------------------------------------------------------------------
// Authoritative decision classification (accept-provenance).
//
// `accept` asserts something checkable: an ASR engine produced this exact text and a human approved
// it unchanged. On a RE-review the displayed text is a previous human's correction, so honouring the
// renderer's word would launder human authorship into machine provenance.
// ---------------------------------------------------------------------------------------------

fn seed_for_provenance(db: &Database, id: &str, champion_text: &str) {
    let mut seg = make_segment(id, "/a/clip.wav");
    seg.raw_transcript = champion_text.to_string();
    db.insert_segment(&seg).unwrap();
    ensure_test_audio_content_hash(db, id);
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: id.to_string(),
        model_id: "omniasr-wsl-7b".into(),
        transcript: champion_text.to_string(),
        confidence: None,
    })
    .unwrap();
}

#[test]
fn accepting_the_champions_own_text_stays_an_accept() {
    let db = make_db();
    seed_for_provenance(&db, "acc-1", "کاک لە ئەمە شتێکی تر");
    db.record_human_decision("acc-1", "accept", Some("کاک لە ئەمە شتێکی تر"), None).unwrap();
    let seg = db.get_segment_by_id("acc-1").unwrap().unwrap();
    assert_eq!(seg.human_decision.as_deref(), Some("accept"), "a genuine ASR accept must stay an accept");
}

#[test]
fn whitespace_alone_never_demotes_a_real_accept() {
    // The gate compares on words, not spacing; the backend must agree or it reclassifies honest accepts.
    let db = make_db();
    seed_for_provenance(&db, "acc-2", "کاک لە ئەمە شتێکی تر");
    db.record_human_decision("acc-2", "accept", Some("  کاک   لە ئەمە\tشتێکی تر  "), None).unwrap();
    assert_eq!(db.get_segment_by_id("acc-2").unwrap().unwrap().human_decision.as_deref(), Some("accept"));
}

#[test]
fn confirming_a_previous_humans_correction_is_recorded_as_human_authored() {
    // The exact 2026-08-18 regression on segment 82681df2: five humans edited the clip, the sixth
    // pressed Accept, and the row then claimed the champion had produced a name and punctuation that
    // appear in none of its hypotheses.
    let db = make_db();
    seed_for_provenance(&db, "reg-82681df2", "کاک لە ئەمە شتێکی تر یەعنی");
    db.record_human_decision("reg-82681df2", "edit", Some("کاک لامۆ، شتێکی تر، یەعنی"), None).unwrap();

    // A later reviewer approves what they see — which is Rezan's text, not the engine's.
    db.record_human_decision("reg-82681df2", "accept", Some("کاک لامۆ، شتێکی تر، یەعنی"), None).unwrap();

    let seg = db.get_segment_by_id("reg-82681df2").unwrap().unwrap();
    assert_eq!(
        seg.human_decision.as_deref(),
        Some("edit"),
        "approving a previous human's text is human-authored confirmation, never an ASR accept"
    );
    assert_eq!(
        seg.verdict.as_deref(),
        Some("human_edit"),
        "the verdict must carry the same human authorship as the decision"
    );
    // `annotated_transcript` is human-only and written by the UI's separate field update; the
    // decision path owns `verdict_transcript`, which is the COALESCE-preferred gold source.
    assert_eq!(
        seg.verdict_transcript.as_deref(),
        Some("کاک لامۆ، شتێکی تر، یەعنی"),
        "the human text is carried forward verbatim, never invented or reverted"
    );
}

#[test]
fn an_accept_of_text_no_engine_produced_is_never_an_accept() {
    let db = make_db();
    seed_for_provenance(&db, "acc-3", "کاک لە ئەمە شتێکی تر");
    db.record_human_decision("acc-3", "accept", Some("تەواو جیاواز و دەستکاریکراو"), None).unwrap();
    let seg = db.get_segment_by_id("acc-3").unwrap().unwrap();
    assert_eq!(
        seg.human_decision.as_deref(),
        Some("edit"),
        "a renderer claiming 'accept' for text no engine emitted must not be taken at its word"
    );
}

#[test]
fn an_auxiliary_engines_output_still_counts_as_an_accept() {
    // The champion is not the only traceable hypothesis: accepting the fine-tuned engine's output is
    // still accepting an ASR transcript, and must not be demoted to a human edit.
    let db = make_db();
    seed_for_provenance(&db, "acc-4", "دەقی چامپیۆن");
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: "acc-4".into(),
        model_id: "finetuned-mms-ckb".into(),
        transcript: "دەقی مۆدێلی تر".into(),
        confidence: None,
    })
    .unwrap();
    db.record_human_decision("acc-4", "accept", Some("دەقی مۆدێلی تر"), None).unwrap();
    assert_eq!(db.get_segment_by_id("acc-4").unwrap().unwrap().human_decision.as_deref(), Some("accept"));
}

#[test]
fn a_reject_is_never_reclassified() {
    let db = make_db();
    seed_for_provenance(&db, "rej-1", "دەقی چامپیۆن");
    db.record_human_decision("rej-1", "reject", None, None).unwrap();
    assert_eq!(db.get_segment_by_id("rej-1").unwrap().unwrap().human_decision.as_deref(), Some("reject"));
}

/// Durability of an acknowledged human decision, and what it costs.
///
/// The connection runs `synchronous=NORMAL`; under WAL that is durable against a process crash but
/// not necessarily against power loss. Everything else this app writes is reproducible — a human
/// decision is not. So the decision commit alone escalates to FULL.
#[test]
fn a_human_decision_commit_is_fsynced_and_the_connection_returns_to_normal() {
    let db = make_db();
    seed_for_provenance(&db, "dur-1", "دەقی چامپیۆن");

    let before: i64 = db.conn.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
    assert_eq!(before, 1, "precondition: the connection runs NORMAL (1)");

    db.record_human_decision("dur-1", "accept", Some("دەقی چامپیۆن"), None).unwrap();

    let after: i64 = db.conn.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
    assert_eq!(after, 1, "the escalation must not leak: other writes stay on NORMAL");
    assert_eq!(
        db.get_segment_by_id("dur-1").unwrap().unwrap().human_decision.as_deref(),
        Some("accept"),
        "the decision itself must still land"
    );
}

/// Measure, do not assume: reviewer throughput has to survive the fsync.
#[test]
#[ignore = "mandatory verify-10 gate runs this wall-clock benchmark in isolation from parallel fsync tests"]
fn the_durability_cost_per_decision_is_measured_not_assumed() {
    use std::time::Instant;
    let db = make_db();
    const N: usize = 40;
    for i in 0..N {
        seed_for_provenance(&db, &format!("perf-{i}"), "دەقی چامپیۆن");
    }
    let start = Instant::now();
    let mut decision_seconds = Vec::with_capacity(N);
    for i in 0..N {
        let decision_start = Instant::now();
        db.record_human_decision(&format!("perf-{i}"), "accept", Some("دەقی چامپیۆن"), None).unwrap();
        decision_seconds.push(decision_start.elapsed().as_secs_f64());
    }
    let per_decision = start.elapsed().as_secs_f64() / N as f64;
    decision_seconds.sort_by(f64::total_cmp);
    let p95_index = (N * 95).div_ceil(100).saturating_sub(1);
    let p95 = decision_seconds[p95_index];
    println!(
        "MEASURED: mean {:.1} ms, P95 {:.1} ms per durable human decision (n={N})",
        per_decision * 1000.0,
        p95 * 1000.0
    );
    // A reviewer decides a clip every few seconds at best. Anything under a quarter second is
    // invisible to them; this bound catches an accidental fsync-per-row regression, not normal jitter.
    assert!(
        per_decision < 0.25,
        "a durable decision cost {:.1} ms — that is slow enough for a reviewer to feel",
        per_decision * 1000.0
    );
    assert!(p95 <= 0.5, "durable decision P95 {:.1} ms exceeds the declared 500 ms workstation budget", p95 * 1000.0);
}

// ---------------------------------------------------------------------------------------------
// Playback evidence. The decision surfaces used to gate on `audioError` — the ABSENCE of a failure,
// which is not the presence of listening. These pin what counts as having heard a clip.
// ---------------------------------------------------------------------------------------------

fn receipt(segment: &str, revision: i64, content_hash: &str, played: i64, total: i64) -> PlaybackReceipt {
    let (source_start_ms, source_end_ms) = test_source_span(segment, total);
    PlaybackReceipt {
        segment_id: segment.to_string(),
        segment_revision: revision,
        audio_content_hash: content_hash.to_string(),
        reviewer: Some("Sara".into()),
        session_id: Some("sess-1".into()),
        started_at_ms: 1_700_000_000_000,
        played_ms: played,
        clip_duration_ms: total,
        source_start_ms: Some(source_start_ms),
        source_end_ms: Some(source_end_ms),
    }
}

fn insert_playback_segment(db: &Database, id: &str, duration_ms: i64) {
    let mut segment = make_segment(id, "/a/clip.wav");
    segment.duration_ms = duration_ms;
    db.insert_segment(&segment).unwrap();
    ensure_test_audio_content_hash(db, id);
}

/// The receipt's identity fields are the SERVER's to state, whichever surface minted it.
///
/// Found by the 2026-08-19 bug hunt, verified certain: the desktop mint command passed the
/// renderer's clipDurationMs straight through, and both desktop surfaces report the WHOLE source
/// file's duration (403 of 414 clips share one recording) — so an honest full listen of a 10s clip
/// scored ~0.004 and was refused, while a lying client could shrink the clip to mint coverage 1.0.
/// The phone path resolved the denominator server-side; the desktop did not. One resolving front
/// door now serves every surface, so a caller cannot get it wrong again.
#[test]
fn a_receipt_is_measured_against_the_rows_own_clip_length() {
    let db = make_db();
    let mut seg = make_segment("pb-den", "/a/clip.wav");
    seg.duration_ms = 10_000;
    db.insert_segment(&seg).unwrap();
    ensure_test_audio_content_hash(&db, "pb-den");

    // The client claims the clip is 100ms and that it played all 100ms.
    db.record_playback_receipt(&receipt("pb-den", 0, OTHER_AUDIO_CONTENT_HASH, 100, 100)).unwrap();
    let (total, coverage): (i64, f64) = db
        .connection()
        .query_row(
            "SELECT clip_duration_ms, coverage_ratio FROM playback_receipts WHERE segment_id = 'pb-den'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(total, 10_000, "the denominator is the row's duration, not the client's claim");
    assert!(coverage < 0.02, "100ms of a 10s clip is not a listen, got {coverage}");
}

/// A segment identifier, path, or spectral bucket is not exact audio identity. Missing decoded-PCM
/// content hashes fail closed at mint.
#[test]
fn a_row_without_an_audio_content_hash_cannot_mint_playback_evidence() {
    let db = make_db();
    db.insert_segment(&make_segment("pb-fp", "/a/clip.wav")).unwrap();
    let error = db
        .record_playback_receipt(&receipt("pb-fp", 7, OTHER_AUDIO_CONTENT_HASH, 1_000, 1_000))
        .expect_err("client claims cannot replace missing server audio identity");
    assert!(error.to_string().contains("server-derived audio content hash"));
    let raw_error = db
        .record_playback_receipt_raw(&receipt("pb-fp", 0, "   ", 1_000, 1_000))
        .expect_err("even the internal writer must reject identity-free evidence");
    assert!(raw_error.to_string().contains("canonical server-derived decoded-PCM BLAKE3 hash"));
    assert!(
        !db.record_playback_receipt_if_at_revision(&receipt("pb-fp", 0, OTHER_AUDIO_CONTENT_HASH, 1_000, 1_000), 0,)
            .unwrap(),
        "the atomic phone mint must write nothing when the server row has no content hash"
    );
    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM playback_receipts WHERE segment_id = 'pb-fp'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

/// Listening is PERSONAL: someone else's ears are not your evidence.
///
/// Found by the hunt, verified certain: the guard matched receipts by segment+revision+content hash
/// only, so reviewer A's full listen let reviewer B's blind verdict through — on the phone, a clip
/// A skipped after hearing goes to B's queue with A's receipt still valid for it.
#[test]
fn someone_elses_listening_is_not_your_evidence() {
    let db = make_db();
    db.insert_segment(&make_segment("pb-who", "/a/clip.wav")).unwrap();
    let content_hash = ensure_test_audio_content_hash(&db, "pb-who");
    db.record_playback_receipt(&receipt("pb-who", 0, OTHER_AUDIO_CONTENT_HASH, 1_000, 1_000)).unwrap(); // by Sara

    let revision = db.segment_review_revision("pb-who").unwrap().unwrap_or(0);
    assert!(db.has_sufficient_playback_evidence("pb-who", revision, &content_hash, Some("Sara")).unwrap());
    assert!(
        !db.has_sufficient_playback_evidence("pb-who", revision, &content_hash, Some("Hemn")).unwrap(),
        "Sara's listen must not evidence Hemn's verdict"
    );
    assert!(
        !db.has_sufficient_playback_evidence("pb-who", revision, &content_hash, None).unwrap(),
        "an anonymous desktop check must not ride a named phone receipt"
    );
}

#[test]
fn a_noncanonical_decision_proof_is_never_an_authorization_capability() {
    let db = make_db();
    insert_playback_segment(&db, "pb-proof-shape", 1_000);
    let revision = db.segment_review_revision("pb-proof-shape").unwrap().unwrap();
    db.record_playback_receipt(&receipt("pb-proof-shape", revision, TEST_AUDIO_CONTENT_HASH, 900, 1_000)).unwrap();

    assert!(!db.has_sufficient_playback_evidence("pb-proof-shape", revision, "424242", Some("Sara")).unwrap());
    assert!(!db
        .has_sufficient_playback_evidence(
            "pb-proof-shape",
            revision,
            &TEST_AUDIO_CONTENT_HASH.to_uppercase(),
            Some("Sara"),
        )
        .unwrap());
}

#[test]
fn desktop_policy4_receipt_is_exact_interval_authority_and_replays_idempotently() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("policy4.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&source, spec).unwrap();
    for _ in 0..6_400 {
        writer.write_sample(700_i16).unwrap();
    }
    writer.finalize().unwrap();
    let db = make_db();
    let mut segment = make_segment("pb-policy4", source.to_str().unwrap());
    segment.duration_ms = 400;
    segment.alignment_json =
        Some(r#"{"source_start_ms":0,"source_end_ms":400,"chunk_index":0,"chunk_count":1}"#.to_string());
    db.insert_segment(&segment).unwrap();
    let content_hash = crate::export_bundle::current_canonical_pcm_blake3(&source).unwrap();
    db.connection()
        .execute(
            "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
            rusqlite::params![segment.id, content_hash],
        )
        .unwrap();
    let revision = db.segment_review_revision(&segment.id).unwrap().unwrap();
    let media_grant_id = uuid::Uuid::new_v4().to_string();
    let client_attempt_id = uuid::Uuid::new_v4().to_string();
    let wrong_bytes = "b".repeat(64);
    let wrong_binding = db
        .begin_desktop_playback_session_v1(
            &segment.id,
            revision,
            &media_grant_id,
            &client_attempt_id,
            &source,
            &wrong_bytes,
            None,
        )
        .expect_err("a live grant for different decoded audio must not issue authority");
    assert!(wrong_binding.to_string().contains("different audio bytes"), "{wrong_binding}");
    let session = db
        .begin_desktop_playback_session_v1(
            &segment.id,
            revision,
            &media_grant_id,
            &client_attempt_id,
            &source,
            &content_hash,
            None,
        )
        .expect("the live grant/source/clip identity must issue a session");
    for _ in 0..10_000 {
        let replayed_session = db
            .begin_desktop_playback_session_v1(
                &segment.id,
                revision,
                &media_grant_id,
                &client_attempt_id,
                &source,
                &content_hash,
                None,
            )
            .expect("an exact client-attempt retry must return its original session");
        assert_eq!(replayed_session, session);
    }
    let issued_sessions: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM desktop_playback_sessions_v4 WHERE client_attempt_id=?1",
            [&client_attempt_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(issued_sessions, 1, "10,000 exact begin retries must stay one durable/live attempt");
    let intervals = [DesktopPlaybackInterval { start_ms: 0, end_ms: 340 }];
    // At the declared maximum 2x rate, 340 ms of unique media requires at least 170 ms of server
    // elapsed time. The policy intentionally has zero fixed grace: otherwise short clips could mint
    // an 85% receipt immediately. Leave margin for millisecond clock granularity on Windows CI.
    std::thread::sleep(std::time::Duration::from_millis(220));
    let first = db
        .finalize_desktop_playback_session_v1(
            &session.playback_receipt_id,
            &media_grant_id,
            &source,
            &content_hash,
            &intervals,
        )
        .expect("85% at a plausible server elapsed time must finalize");
    assert_eq!(first.unique_played_ms, 340);
    assert!((first.coverage_ratio - 0.85).abs() < f64::EPSILON);
    let changed_grant = db
        .finalize_desktop_playback_session_v1(
            &session.playback_receipt_id,
            &media_grant_id,
            &source,
            &wrong_bytes,
            &intervals,
        )
        .expect_err("finalization must re-check the immutable grant's decoded bytes");
    assert!(changed_grant.to_string().contains("immutable media grant"), "{changed_grant}");

    let replay = db
        .finalize_desktop_playback_session_v1(
            &session.playback_receipt_id,
            &media_grant_id,
            &source,
            &content_hash,
            &intervals,
        )
        .expect("an exact lost-response replay must return the same immutable receipt");
    assert_eq!(replay, first);
    let recovered_without_live_grant = db
        .replay_finalized_desktop_playback_receipt_v1(&session.playback_receipt_id, &media_grant_id, &intervals)
        .expect("immutable receipt recovery query succeeds")
        .expect("an already-finalized exact union survives expiry of its media grant");
    assert_eq!(recovered_without_live_grant, first);
    let changed_recovery = db
        .replay_finalized_desktop_playback_receipt_v1(
            &session.playback_receipt_id,
            &media_grant_id,
            &[DesktopPlaybackInterval { start_ms: 0, end_ms: 341 }],
        )
        .expect_err("grant-free recovery still refuses an altered interval union");
    assert!(changed_recovery.to_string().contains("different interval union"), "{changed_recovery}");
    let changed_grant_recovery = db
        .replay_finalized_desktop_playback_receipt_v1(
            &session.playback_receipt_id,
            &uuid::Uuid::new_v4().to_string(),
            &intervals,
        )
        .expect_err("exact replay remains bound to the originally issued media grant identity");
    assert!(changed_grant_recovery.to_string().contains("different media grant"), "{changed_grant_recovery}");
    let changed_replay = db
        .finalize_desktop_playback_session_v1(
            &session.playback_receipt_id,
            &media_grant_id,
            &source,
            &content_hash,
            &[DesktopPlaybackInterval { start_ms: 0, end_ms: 341 }],
        )
        .expect_err("one receipt identity cannot authorize a different interval union");
    assert!(changed_replay.to_string().contains("different interval union"), "{changed_replay}");

    let proof = db
        .desktop_playback_proof_v4(
            &segment.id,
            session.segment_revision,
            &content_hash,
            &session.playback_receipt_id,
            None,
        )
        .unwrap()
        .expect("the exact policy-4 receipt must authorize its current clip");
    let rollback_error = crate::migrations::rollback(&db, 1)
        .expect_err("a finalized policy-4 receipt is durable evidence and cannot be downgraded away");
    assert!(rollback_error.to_string().contains("CHECK constraint failed"), "{rollback_error}");
    assert_eq!(crate::migrations::get_current_version(&db).unwrap(), 67);
    assert_eq!(proof.authority_session_id.as_deref(), Some(session.playback_receipt_id.as_str()));
    assert!(db
        .desktop_playback_proof_v4(
            &segment.id,
            session.segment_revision,
            &content_hash,
            &uuid::Uuid::new_v4().to_string(),
            None,
        )
        .unwrap()
        .is_none());

    let operation_id = "77777777-7777-4777-8777-777777777777";
    let committed = db
        .finalize_desktop_review_v1_with_playback(
            &segment.id,
            session.segment_revision,
            "accept",
            Some("test"),
            &proof,
            operation_id,
        )
        .expect("the decision transaction must re-check the exact receipt and commit");
    let persisted_authority: (Option<i64>, Option<String>) = db
        .connection()
        .query_row(
            "SELECT desktop_review_contract_version, playback_authority_session_id
               FROM human_decision_effect_events WHERE id=?1",
            [committed.effect_event_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(persisted_authority.0, Some(1));
    assert_eq!(persisted_authority.1.as_deref(), Some(session.playback_receipt_id.as_str()));
    let changed_receipt = db
        .replay_desktop_review_v1_and_clear_draft(
            &segment.id,
            session.segment_revision,
            "accept",
            Some("test"),
            &uuid::Uuid::new_v4().to_string(),
            operation_id,
        )
        .expect_err("the same operation UUID with a different receipt is a payload conflict");
    assert!(changed_receipt.to_string().contains("different canonical payload"), "{changed_receipt}");
}

#[test]
fn desktop_policy4_cancel_is_exact_idempotent_and_cannot_touch_finalized_or_consumed_authority() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("policy4-cancel.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&source, spec).unwrap();
    for _ in 0..6_400 {
        writer.write_sample(500_i16).unwrap();
    }
    writer.finalize().unwrap();
    let db = make_db();
    let mut segment = make_segment("pb-policy4-cancel", source.to_str().unwrap());
    segment.duration_ms = 400;
    segment.alignment_json =
        Some(r#"{"source_start_ms":0,"source_end_ms":400,"chunk_index":0,"chunk_count":1}"#.to_string());
    db.insert_segment(&segment).unwrap();
    let content_hash = crate::export_bundle::current_canonical_pcm_blake3(&source).unwrap();
    db.connection()
        .execute("UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1", params![segment.id, content_hash])
        .unwrap();
    let revision = db.segment_review_revision(&segment.id).unwrap().unwrap();

    db.set_playback_test_clock(1_000_000, 10_000);
    let cancelled_attempt_id = uuid::Uuid::new_v4().to_string();
    let cancelled_grant_id = uuid::Uuid::new_v4().to_string();
    let cancelled = db
        .begin_desktop_playback_session_v1(
            &segment.id,
            revision,
            &cancelled_grant_id,
            &cancelled_attempt_id,
            &source,
            &content_hash,
            None,
        )
        .unwrap();

    let wrong_attempt = db
        .cancel_desktop_playback_session_v1(&cancelled.playback_receipt_id, &uuid::Uuid::new_v4().to_string())
        .expect_err("a stale renderer must not cancel another attempt's exact authority");
    assert!(wrong_attempt.to_string().contains("E_PLAYBACK_CANCEL_IDENTITY_MISMATCH"), "{wrong_attempt}");
    assert!(db
        .connection()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM desktop_playback_sessions_v4 WHERE playback_receipt_id=?1)",
            [&cancelled.playback_receipt_id],
            |row| row.get::<_, bool>(0),
        )
        .unwrap());

    assert!(db.cancel_desktop_playback_session_v1(&cancelled.playback_receipt_id, &cancelled_attempt_id).unwrap());
    assert!(
        !db.cancel_desktop_playback_session_v1(&cancelled.playback_receipt_id, &cancelled_attempt_id).unwrap(),
        "an exact cancellation replay is a successful no-op"
    );
    let cancelled_finalize = db
        .finalize_desktop_playback_session_v1(
            &cancelled.playback_receipt_id,
            &cancelled_grant_id,
            &source,
            &content_hash,
            &[DesktopPlaybackInterval { start_ms: 0, end_ms: 340 }],
        )
        .expect_err("retired authority cannot later mint a receipt");
    assert!(cancelled_finalize.to_string().contains("missing or was never issued"), "{cancelled_finalize}");

    let final_attempt_id = uuid::Uuid::new_v4().to_string();
    let final_grant_id = uuid::Uuid::new_v4().to_string();
    let finalized = db
        .begin_desktop_playback_session_v1(
            &segment.id,
            revision,
            &final_grant_id,
            &final_attempt_id,
            &source,
            &content_hash,
            None,
        )
        .unwrap();
    db.set_playback_test_clock(1_000_220, 10_220);
    db.finalize_desktop_playback_session_v1(
        &finalized.playback_receipt_id,
        &final_grant_id,
        &source,
        &content_hash,
        &[DesktopPlaybackInterval { start_ms: 0, end_ms: 340 }],
    )
    .unwrap();
    let proof = db
        .desktop_playback_proof_v4(&segment.id, revision, &content_hash, &finalized.playback_receipt_id, None)
        .unwrap()
        .unwrap();
    let committed = db
        .finalize_desktop_review_v1_with_playback(
            &segment.id,
            revision,
            "accept",
            Some("test"),
            &proof,
            &uuid::Uuid::new_v4().to_string(),
        )
        .unwrap();

    for _ in 0..2 {
        let immutable = db
            .cancel_desktop_playback_session_v1(&finalized.playback_receipt_id, &final_attempt_id)
            .expect_err("finalized and consumed evidence is never cancellable, including replay");
        assert!(immutable.to_string().contains("E_PLAYBACK_SESSION_FINALIZED"), "{immutable}");
    }
    let immutable_counts: (i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT
                 EXISTS(SELECT 1 FROM desktop_playback_sessions_v4 WHERE playback_receipt_id=?1),
                 EXISTS(SELECT 1 FROM playback_receipts WHERE authority_session_id=?1 AND policy_version=4),
                 EXISTS(SELECT 1 FROM human_decision_effect_events WHERE id=?2 AND playback_authority_session_id=?1)",
            params![finalized.playback_receipt_id, committed.effect_event_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(immutable_counts, (1, 1, 1));
    db.clear_playback_test_clock();
}

#[test]
fn desktop_policy4_lost_cancellations_cannot_exhaust_global_or_segment_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("policy4-capacity.wav");
    std::fs::write(&source, b"RIFF-policy4-capacity-test-source").unwrap();
    let db = make_db();

    // Simulate 65 browse-only selections whose renderer teardown never reaches the backend. The
    // 65th issuance must reclaim one oldest never-finalized row instead of locking review for 30m.
    for index in 0..65_i64 {
        let segment_id = format!("pb-policy4-browse-{index:02}");
        let mut segment = make_segment(&segment_id, source.to_str().unwrap());
        segment.duration_ms = 1_000;
        db.insert_segment(&segment).unwrap();
        let content_hash = ensure_test_audio_content_hash(&db, &segment_id);
        let revision = db.segment_review_revision(&segment_id).unwrap().unwrap();
        db.set_playback_test_clock(1_000_000 + index, 10_000 + u64::try_from(index).unwrap());
        db.begin_desktop_playback_session_v1(
            &segment_id,
            revision,
            &uuid::Uuid::new_v4().to_string(),
            &uuid::Uuid::new_v4().to_string(),
            &source,
            &content_hash,
            None,
        )
        .expect("ordinary browsing must never exhaust the global attempt bound");
    }
    let live_after_browse: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM desktop_playback_sessions_v4 session
              WHERE NOT EXISTS (SELECT 1 FROM playback_receipts receipt
                                 WHERE receipt.authority_session_id=session.playback_receipt_id)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(live_after_browse, MAX_LIVE_DESKTOP_PLAYBACK_SESSIONS);

    // A→B→A→B→A is the smaller real-world repro for the per-segment bound. The third A replaces
    // only A's oldest never-finalized authority; B and all immutable rows remain untouched.
    for segment_id in ["pb-policy4-revisit-a", "pb-policy4-revisit-b"] {
        let mut segment = make_segment(segment_id, source.to_str().unwrap());
        segment.duration_ms = 1_000;
        db.insert_segment(&segment).unwrap();
        ensure_test_audio_content_hash(&db, segment_id);
    }
    for (offset, segment_id) in [
        "pb-policy4-revisit-a",
        "pb-policy4-revisit-b",
        "pb-policy4-revisit-a",
        "pb-policy4-revisit-b",
        "pb-policy4-revisit-a",
    ]
    .into_iter()
    .enumerate()
    {
        let revision = db.segment_review_revision(segment_id).unwrap().unwrap();
        db.set_playback_test_clock(1_001_000 + offset as i64, 11_000 + offset as u64);
        db.begin_desktop_playback_session_v1(
            segment_id,
            revision,
            &uuid::Uuid::new_v4().to_string(),
            &uuid::Uuid::new_v4().to_string(),
            &source,
            TEST_AUDIO_CONTENT_HASH,
            None,
        )
        .expect("revisiting a clip must replace stale unfinalized authority instead of refusing playback");
    }
    let (global_live, live_a, live_b): (i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM desktop_playback_sessions_v4 session
                   WHERE NOT EXISTS (SELECT 1 FROM playback_receipts receipt
                                      WHERE receipt.authority_session_id=session.playback_receipt_id)),
                 (SELECT COUNT(*) FROM desktop_playback_sessions_v4 session
                   WHERE session.segment_id='pb-policy4-revisit-a'
                     AND NOT EXISTS (SELECT 1 FROM playback_receipts receipt
                                      WHERE receipt.authority_session_id=session.playback_receipt_id)),
                 (SELECT COUNT(*) FROM desktop_playback_sessions_v4 session
                   WHERE session.segment_id='pb-policy4-revisit-b'
                     AND NOT EXISTS (SELECT 1 FROM playback_receipts receipt
                                      WHERE receipt.authority_session_id=session.playback_receipt_id))",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(global_live, MAX_LIVE_DESKTOP_PLAYBACK_SESSIONS);
    assert_eq!((live_a, live_b), (2, 2));
    db.clear_playback_test_clock();
}

#[test]
fn desktop_policy4_rejects_instant_scalar_equivalent_inflation_without_partial_rows() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("policy4-long.wav");
    std::fs::write(&source, b"RIFF-policy4-long-test-source").unwrap();
    let db = make_db();
    let mut segment = make_segment("pb-policy4-instant", source.to_str().unwrap());
    segment.duration_ms = 10_000;
    db.insert_segment(&segment).unwrap();
    let content_hash = ensure_test_audio_content_hash(&db, &segment.id);
    let revision = db.segment_review_revision(&segment.id).unwrap().unwrap();
    let media_grant_id = uuid::Uuid::new_v4().to_string();
    let client_attempt_id = uuid::Uuid::new_v4().to_string();
    let session = db
        .begin_desktop_playback_session_v1(
            &segment.id,
            revision,
            &media_grant_id,
            &client_attempt_id,
            &source,
            &content_hash,
            None,
        )
        .unwrap();

    let error = db
        .finalize_desktop_playback_session_v1(
            &session.playback_receipt_id,
            &media_grant_id,
            &source,
            &content_hash,
            &[DesktopPlaybackInterval { start_ms: 0, end_ms: 8_500 }],
        )
        .expect_err("8.5 seconds cannot be minted immediately from a renderer counter");
    assert!(error.to_string().contains("E_PLAYBACK_TIME_IMPLAUSIBLE"), "{error}");
    let counts: (i64, i64) = db
        .connection()
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM desktop_playback_intervals_v4 WHERE playback_receipt_id=?1),
                 (SELECT COUNT(*) FROM playback_receipts WHERE authority_session_id=?1)",
            [&session.playback_receipt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (0, 0), "a refused forged counter must leave no partial evidence");
}

#[test]
fn desktop_policy4_never_finalizes_a_subthreshold_receipt_at_the_one_ms_span_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("policy4-threshold.wav");
    std::fs::write(&source, b"RIFF-policy4-threshold-test-source").unwrap();
    let db = make_db();
    let mut segment = make_segment("pb-policy4-threshold", source.to_str().unwrap());
    segment.duration_ms = 1_001;
    segment.alignment_json =
        Some(r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":1}"#.to_string());
    db.insert_segment(&segment).unwrap();
    let content_hash = ensure_test_audio_content_hash(&db, &segment.id);
    let revision = db.segment_review_revision(&segment.id).unwrap().unwrap();
    let media_grant_id = uuid::Uuid::new_v4().to_string();
    let client_attempt_id = uuid::Uuid::new_v4().to_string();
    db.set_playback_test_clock(1_000_000, 10_000);
    let session = db
        .begin_desktop_playback_session_v1(
            &segment.id,
            revision,
            &media_grant_id,
            &client_attempt_id,
            &source,
            &content_hash,
            None,
        )
        .unwrap();
    assert_eq!(session.clip_duration_ms, 1_001, "the server duration is the only authority denominator");
    db.set_playback_test_clock(1_001_000, 11_000);

    let below = db
        .finalize_desktop_playback_session_v1(
            &session.playback_receipt_id,
            &media_grant_id,
            &source,
            &content_hash,
            &[DesktopPlaybackInterval { start_ms: 0, end_ms: 850 }],
        )
        .expect_err("850/1001 is below 85% and must not create immutable evidence");
    assert!(below.to_string().contains("E_PLAYBACK_COVERAGE_INSUFFICIENT"), "{below}");
    let partial_rows: (i64, i64) = db
        .connection()
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM desktop_playback_intervals_v4 WHERE playback_receipt_id=?1),
                 (SELECT COUNT(*) FROM playback_receipts WHERE authority_session_id=?1)",
            [&session.playback_receipt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(partial_rows, (0, 0), "a subthreshold attempt must remain extendable, not become immutable");

    let exact = db
        .finalize_desktop_playback_session_v1(
            &session.playback_receipt_id,
            &media_grant_id,
            &source,
            &content_hash,
            &[DesktopPlaybackInterval { start_ms: 0, end_ms: 851 }],
        )
        .expect("ceil(1001*85/100)=851 ms must finalize on the same still-live attempt");
    assert_eq!(exact.unique_played_ms, 851);
}

#[test]
fn desktop_policy4_uses_active_time_not_wall_clock_steps_or_suspend_budget() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("policy4-clock.wav");
    std::fs::write(&source, b"RIFF-policy4-clock-test-source").unwrap();
    let db = make_db();
    let mut segment = make_segment("pb-policy4-clock", source.to_str().unwrap());
    segment.duration_ms = 400;
    db.insert_segment(&segment).unwrap();
    let content_hash = ensure_test_audio_content_hash(&db, &segment.id);
    let revision = db.segment_review_revision(&segment.id).unwrap().unwrap();
    let media_grant_id = uuid::Uuid::new_v4().to_string();
    let client_attempt_id = uuid::Uuid::new_v4().to_string();

    db.set_playback_test_clock(1_000_000, 10_000);
    let session = db
        .begin_desktop_playback_session_v1(
            &segment.id,
            revision,
            &media_grant_id,
            &client_attempt_id,
            &source,
            &content_hash,
            None,
        )
        .unwrap();
    let intervals = [DesktopPlaybackInterval { start_ms: 0, end_ms: 340 }];

    // A one-day wall-clock jump (or equivalent suspend duration) contributes zero active time and
    // cannot authorize an instant renderer counter.
    db.set_playback_test_clock(1_000_000 + 86_400_000, 10_000);
    let forward = db
        .finalize_desktop_playback_session_v1(
            &session.playback_receipt_id,
            &media_grant_id,
            &source,
            &content_hash,
            &intervals,
        )
        .expect_err("forward wall time without active workstation time must not mint a receipt");
    assert!(forward.to_string().contains("E_PLAYBACK_TIME_IMPLAUSIBLE"), "{forward}");

    // Moving wall time backwards must not strand honest work either. 220 ms of active time supports
    // 340 ms of canonical media at the declared 2x maximum, independent of the audit timestamp.
    db.set_playback_test_clock(500_000, 10_220);
    let receipt = db
        .finalize_desktop_playback_session_v1(
            &session.playback_receipt_id,
            &media_grant_id,
            &source,
            &content_hash,
            &intervals,
        )
        .expect("sufficient active time remains valid across a backwards wall-clock correction");
    assert_eq!(receipt.unique_played_ms, 340);
    db.clear_playback_test_clock();
}

#[test]
fn desktop_policy4_collects_expired_unfinalized_attempts_but_keeps_live_attempts() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("policy4-gc.wav");
    std::fs::write(&source, b"RIFF-policy4-gc-test-source").unwrap();
    let db = make_db();
    let mut segment = make_segment("pb-policy4-gc", source.to_str().unwrap());
    segment.duration_ms = 1_000;
    db.insert_segment(&segment).unwrap();
    let content_hash = ensure_test_audio_content_hash(&db, &segment.id);
    let revision = db.segment_review_revision(&segment.id).unwrap().unwrap();

    db.set_playback_test_clock(1_000_000, 10_000);
    let abandoned = db
        .begin_desktop_playback_session_v1(
            &segment.id,
            revision,
            &uuid::Uuid::new_v4().to_string(),
            &uuid::Uuid::new_v4().to_string(),
            &source,
            &content_hash,
            None,
        )
        .unwrap();
    db.set_playback_test_clock(
        1_000_000 + DESKTOP_PLAYBACK_SESSION_TTL_MS + 1,
        10_000 + u64::try_from(DESKTOP_PLAYBACK_SESSION_TTL_MS).unwrap() + 1,
    );
    let current = db
        .begin_desktop_playback_session_v1(
            &segment.id,
            revision,
            &uuid::Uuid::new_v4().to_string(),
            &uuid::Uuid::new_v4().to_string(),
            &source,
            &content_hash,
            None,
        )
        .unwrap();
    let counts: (i64, i64) = db
        .connection()
        .query_row(
            "SELECT
                 EXISTS(SELECT 1 FROM desktop_playback_sessions_v4 WHERE playback_receipt_id=?1),
                 EXISTS(SELECT 1 FROM desktop_playback_sessions_v4 WHERE playback_receipt_id=?2)",
            rusqlite::params![abandoned.playback_receipt_id, current.playback_receipt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (0, 1), "only the expired never-finalized attempt is collectible");
    db.clear_playback_test_clock();
}

#[test]
fn desktop_policy4_unfinalized_renderer_counter_cannot_survive_process_restart() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("policy4-restart.wav");
    std::fs::write(&source, b"RIFF-policy4-restart-test-source").unwrap();
    let database_path = directory.path().join("restart.sqlite3");
    let db = Database::open(database_path.to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    let mut segment = make_segment("pb-policy4-restart", source.to_str().unwrap());
    segment.duration_ms = 400;
    db.insert_segment(&segment).unwrap();
    let content_hash = ensure_test_audio_content_hash(&db, &segment.id);
    let revision = db.segment_review_revision(&segment.id).unwrap().unwrap();
    let media_grant_id = uuid::Uuid::new_v4().to_string();
    let client_attempt_id = uuid::Uuid::new_v4().to_string();
    db.set_playback_test_clock(1_000_000, 10_000);
    let session = db
        .begin_desktop_playback_session_v1(
            &segment.id,
            revision,
            &media_grant_id,
            &client_attempt_id,
            &source,
            &content_hash,
            None,
        )
        .unwrap();
    drop(db);

    let reopened = Database::open(database_path.to_str().unwrap()).unwrap();
    reopened.set_playback_test_clock(1_001_000, 11_000);
    let error = reopened
        .finalize_desktop_playback_session_v1(
            &session.playback_receipt_id,
            &media_grant_id,
            &source,
            &content_hash,
            &[DesktopPlaybackInterval { start_ms: 0, end_ms: 340 }],
        )
        .expect_err("a durable session row without its process-local active-time lease cannot mint evidence");
    assert!(error.to_string().contains("no live active-time authority"), "{error}");
}

/// Voice focus narrows the queue to the named clips and NOTHING else — and no focus is the full queue.
#[test]
fn voice_focus_narrows_the_pending_queue_to_exactly_the_named_clips() {
    let dir = tempfile::tempdir().unwrap();
    let db = make_db();
    // Real files on disk: pending_segment_ids refuses clips whose audio is missing.
    let mut ids = Vec::new();
    for n in 0..4 {
        let path = dir.path().join(format!("c{n}.wav"));
        std::fs::write(&path, b"RIFF").unwrap();
        let id = format!("focus-{n}");
        db.insert_segment(&make_segment(&id, path.to_str().unwrap())).unwrap();
        ids.push(id);
    }
    let all = db.pending_segment_ids_focused(None, None).unwrap();
    assert_eq!(all.len(), 4, "no focus is the full queue");

    let focus: std::collections::HashSet<String> = ["focus-1".to_string(), "focus-3".to_string()].into();
    let narrowed = db.pending_segment_ids_focused(None, Some(&focus)).unwrap();
    assert_eq!(narrowed.len(), 2);
    assert!(narrowed.contains(&"focus-1".to_string()) && narrowed.contains(&"focus-3".to_string()));
    assert!(!narrowed.contains(&"focus-0".to_string()), "a clip outside the focus must not be served");

    // An id that is focused but does not exist (or is not pending) simply yields nothing for it.
    let ghost: std::collections::HashSet<String> = ["nope".to_string()].into();
    assert!(db.pending_segment_ids_focused(None, Some(&ghost)).unwrap().is_empty());
}

/// A desktop decision must land COMPLETE in one call — decision and `verified` in the same commit.
///
/// Found 2026-08-20 by an external audit and confirmed on the live library: NINE rows carried
/// `human_decision` with `verified = 0`, all from the owner's own desktop session. `finalize` was
/// derived from `expected_revision.is_some()`, a CAS token only the phone supplies, so desktop
/// decisions never finalized. ReviewMode papered over it with a SECOND write
/// (`updateSegmentFields{verified:true}`); ReviewInbox has no such call at all, so every inbox
/// decision ever made stayed invisible to the corpus — the export counts `verified = 1`.
///
/// The two fields are one adjudication and must share one transaction, exactly as the phone path's
/// own doc comment already said.
#[test]
fn a_desktop_decision_is_finalized_in_the_same_commit() {
    let db = make_db();
    db.insert_segment(&make_segment("fin-1", "/a/clip.wav")).unwrap();

    db.finalize_human_review("fin-1", "accept", None, None, None).unwrap();
    let row = db.get_segment_by_id("fin-1").unwrap().unwrap();
    assert_eq!(row.human_decision.as_deref(), Some("accept"));
    assert!(row.verified, "a decided clip must be verified by the SAME call, not a second write");

    // An edit carries its text in the same commit too.
    db.insert_segment(&make_segment("fin-2", "/a/clip.wav")).unwrap();
    ensure_test_audio_content_hash(&db, "fin-2");
    db.finalize_human_review("fin-2", "edit", Some("دەقی ڕاست"), None, None).unwrap();
    let row = db.get_segment_by_id("fin-2").unwrap().unwrap();
    assert!(row.verified);
    assert_eq!(row.reviewed_by, None, "desktop effects remain anonymous by contract");

    // And the un-finalizing recorder still exists for batch tools that must NOT verify.
    db.insert_segment(&make_segment("fin-3", "/a/clip.wav")).unwrap();
    db.record_human_decision("fin-3", "accept", None, None).unwrap();
    assert!(
        !db.get_segment_by_id("fin-3").unwrap().unwrap().verified,
        "the plain recorder must stay non-finalizing so batch tools keep their semantics"
    );
}

#[test]
fn desktop_decision_retry_returns_the_original_commit_and_uuid_reuse_or_late_retry_fails_closed() {
    let db = make_db();
    db.insert_segment(&make_segment("desktop-replay", "/desktop-replay.wav")).unwrap();
    let audio_content_hash = ensure_test_audio_content_hash(&db, "desktop-replay");
    let revision = db.segment_review_revision("desktop-replay").unwrap().unwrap();
    let (source_start_ms, source_end_ms) = db.segment_source_span("desktop-replay").unwrap().unwrap();
    assert!(db
        .record_playback_receipt_if_at_revision(
            &PlaybackReceipt {
                segment_id: "desktop-replay".into(),
                segment_revision: revision,
                audio_content_hash: audio_content_hash.clone(),
                reviewer: None,
                session_id: Some("desktop".into()),
                started_at_ms: 1_700_000_000_000,
                played_ms: 1_000,
                clip_duration_ms: 1_000,
                source_start_ms: None,
                source_end_ms: None,
            },
            revision,
        )
        .unwrap());
    let proof = PlaybackDecisionProof {
        segment_revision: revision,
        audio_content_hash,
        source_start_ms,
        source_end_ms,
        authority_session_id: None,
        source_lease: None,
    };
    let operation_id = "11111111-2222-4333-8444-555555555555";
    let first = db
        .finalize_human_review_with_playback(
            "desktop-replay",
            "accept",
            Some("test"),
            Some(1_700_000_000_001),
            &proof,
            operation_id,
        )
        .unwrap();
    let replay = db
        .finalize_human_review_with_playback(
            "desktop-replay",
            "accept",
            Some("test"),
            Some(1_700_000_000_001),
            &proof,
            operation_id,
        )
        .expect("a lost-response retry with the exact frozen request returns its original commit");
    assert_eq!(replay.effect_event_id, first.effect_event_id);
    assert_eq!(replay.decided_revision, first.decided_revision);
    let effect_count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM human_decision_effect_events WHERE operation_id = ?1", [operation_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(effect_count, 1, "retry must not duplicate the immutable effect");

    db.set_speaker_change_score("desktop-replay", 0.42).unwrap();
    let metadata_revision = db.segment_review_revision("desktop-replay").unwrap().unwrap();
    assert!(metadata_revision > first.decided_revision);
    let after_metadata = db
        .finalize_human_review_with_playback(
            "desktop-replay",
            "accept",
            Some("test"),
            Some(1_700_000_000_001),
            &proof,
            operation_id,
        )
        .expect("unrelated metadata may advance revision without invalidating an exact lost-response replay");
    assert_eq!(after_metadata.effect_event_id, first.effect_event_id);
    assert_eq!(after_metadata.decided_revision, first.decided_revision);

    let reused = db
        .finalize_human_review_with_playback(
            "desktop-replay",
            "accept",
            Some("different text"),
            Some(1_700_000_000_001),
            &proof,
            operation_id,
        )
        .unwrap_err();
    assert!(reused.to_string().contains("different canonical payload"), "{reused}");

    assert!(matches!(
        db.undo_human_decision(first.effect_event_id, None, "00000000-0000-4000-8000-000000000309",).unwrap(),
        HumanDecisionUndoOutcome::Applied { .. }
    ));
    let stale_replay = db
        .finalize_human_review_with_playback(
            "desktop-replay",
            "accept",
            Some("test"),
            Some(1_700_000_000_001),
            &proof,
            operation_id,
        )
        .unwrap_err();
    assert!(stale_replay.to_string().contains("exact post-state is no longer current"), "{stale_replay}");
}

#[test]
fn a_full_listen_is_sufficient_evidence() {
    let db = make_db();
    insert_playback_segment(&db, "pb-1", 9_000);
    db.record_playback_receipt_raw(&receipt("pb-1", 1, TEST_AUDIO_CONTENT_HASH, 9_000, 9_000)).unwrap();
    assert!(db.has_sufficient_playback_evidence("pb-1", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap());
}

#[test]
fn a_receipt_for_a_different_source_span_never_authorizes_the_same_hash_revision_and_duration() {
    let db = make_db();
    insert_playback_segment(&db, "pb-span", 1_000);
    let revision = db.segment_review_revision("pb-span").unwrap().unwrap();
    let content_hash = db.segment_audio_content_hash("pb-span").unwrap().unwrap();
    db.record_playback_receipt_raw(&receipt("pb-span", revision, &content_hash, 1_000, 1_000)).unwrap();
    assert!(db.has_sufficient_playback_evidence("pb-span", revision, &content_hash, Some("Sara")).unwrap());

    // Model a trigger-disabled staged database: keep the same segment id, decoded-PCM hash,
    // duration, and revision while swapping only the window into the shared source recording.
    db.connection().execute("DROP TRIGGER speech_segments_review_revision", []).unwrap();
    db.connection().execute("DROP TRIGGER speech_segments_v60_paid_identity_immutable_update", []).unwrap();
    let (old_start, old_end) = test_source_span("pb-span", 1_000);
    db.connection()
        .execute(
            "UPDATE speech_segments
                SET alignment_json = json_object(
                    'source_start_ms', ?2, 'source_end_ms', ?3,
                    'chunk_index', 0, 'chunk_count', 1
                )
              WHERE id = ?1",
            params!["pb-span", old_start + 2_000, old_end + 2_000],
        )
        .unwrap();
    assert!(
        !db.has_sufficient_playback_evidence("pb-span", revision, &content_hash, Some("Sara")).unwrap(),
        "policy-3 evidence must bind the exact source window, not only the whole-source hash"
    );
}

#[test]
fn one_millisecond_alignment_rounding_is_valid_but_larger_duration_drift_is_not() {
    let db = make_db();
    db.insert_segment(&make_segment("pb-rounding", "/pb-rounding.wav")).unwrap();
    db.connection()
        .execute(
            "UPDATE speech_segments
                SET audio_content_hash = ?2,
                    alignment_json = json_object('source_start_ms', 50, 'source_end_ms', 1051)
              WHERE id = ?1",
            params!["pb-rounding", TEST_AUDIO_CONTENT_HASH],
        )
        .unwrap();
    let revision = db.segment_review_revision("pb-rounding").unwrap().unwrap();
    db.record_playback_receipt(&PlaybackReceipt {
        segment_id: "pb-rounding".into(),
        segment_revision: revision,
        audio_content_hash: TEST_AUDIO_CONTENT_HASH.into(),
        reviewer: Some("Sara".into()),
        session_id: None,
        started_at_ms: 1,
        played_ms: 900,
        clip_duration_ms: 1_000,
        source_start_ms: None,
        source_end_ms: None,
    })
    .unwrap();
    assert!(db
        .has_sufficient_playback_evidence("pb-rounding", revision, TEST_AUDIO_CONTENT_HASH, Some("Sara"))
        .unwrap());

    db.connection().execute("DROP TRIGGER speech_segments_review_revision", []).unwrap();
    db.connection().execute("DROP TRIGGER speech_segments_v60_paid_identity_immutable_update", []).unwrap();
    db.connection()
        .execute(
            "UPDATE speech_segments
                SET alignment_json = json_object('source_start_ms', 50, 'source_end_ms', 1052)
              WHERE id = 'pb-rounding'",
            [],
        )
        .unwrap();
    assert!(
        !db.has_sufficient_playback_evidence("pb-rounding", revision, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap(),
        "two milliseconds is outside the measured endpoint-rounding tolerance"
    );
}

/// The atomic mint (2026-08-20 hunt): check-and-insert in ONE statement, so a write landing between
/// a caller's version fence and the mint can never rebind the receipt to a revision (and
/// content hash) the reviewer never heard. `false` writes NOTHING.
#[test]
fn a_receipt_is_minted_only_at_the_expected_revision() {
    let db = make_db();
    db.insert_segment(&make_segment("pb-rev", "/a/clip.wav")).unwrap();
    ensure_test_audio_content_hash(&db, "pb-rev");
    let current = db.segment_review_revision("pb-rev").unwrap().unwrap_or(0);

    // At the verified revision: minted, with identity resolved from the row.
    assert!(db
        .record_playback_receipt_if_at_revision(&receipt("pb-rev", 0, OTHER_AUDIO_CONTENT_HASH, 9_000, 9_000), current)
        .unwrap());
    let n: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM playback_receipts WHERE segment_id='pb-rev'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);

    // At a revision the row is no longer on: declined, and NOTHING is written.
    assert!(!db
        .record_playback_receipt_if_at_revision(
            &receipt("pb-rev", 0, OTHER_AUDIO_CONTENT_HASH, 9_000, 9_000),
            current + 7,
        )
        .unwrap());
    let n: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM playback_receipts WHERE segment_id='pb-rev'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "a declined mint leaves no row behind");

    // And for a segment that does not exist at all: declined, not an error.
    assert!(!db.record_playback_receipt_if_at_revision(&receipt("ghost", 0, "fp", 1_000, 1_000), 0).unwrap());
}

#[test]
fn no_receipt_at_all_is_not_evidence() {
    let db = make_db();
    db.insert_segment(&make_segment("pb-2", "/a/clip.wav")).unwrap();
    assert!(
        !db.has_sufficient_playback_evidence("pb-2", 0, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap(),
        "a clip nobody played must never satisfy the listening requirement"
    );
}

#[test]
fn opening_a_clip_without_hearing_it_is_not_evidence() {
    // The exact failure the old `audioError` gate allowed: the audio loaded fine and was never heard.
    let db = make_db();
    insert_playback_segment(&db, "pb-3", 9_000);
    db.record_playback_receipt_raw(&receipt("pb-3", 1, TEST_AUDIO_CONTENT_HASH, 0, 9_000)).unwrap();
    assert!(!db.has_sufficient_playback_evidence("pb-3", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap());
}

/// Runtime authorization recomputes the server-bound raw counters.  ``coverage_ratio`` is a useful
/// derived audit field, but changing that REAL must never turn zero listening into permission.
#[test]
fn a_forged_coverage_ratio_wrong_policy_or_denominator_cannot_unlock_a_verdict() {
    let db = make_db();
    insert_playback_segment(&db, "pb-raw-authority", 1_000);
    db.record_playback_receipt_raw(&receipt("pb-raw-authority", 1, TEST_AUDIO_CONTENT_HASH, 0, 1_000)).unwrap();
    db.connection().execute("DROP TRIGGER playback_receipts_v60_policy3_immutable_update", []).unwrap();
    db.connection().execute("DROP TRIGGER playback_receipts_v67_policy4_immutable_update", []).unwrap();

    db.connection()
        .execute("UPDATE playback_receipts SET coverage_ratio = 1.0 WHERE segment_id = 'pb-raw-authority'", [])
        .unwrap();
    assert!(
        !db.has_sufficient_playback_evidence("pb-raw-authority", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap(),
        "a forged derived ratio cannot override played_ms=0"
    );

    db.connection()
        .execute(
            "UPDATE playback_receipts
                SET played_ms = 1000, clip_duration_ms = 1000, policy_version = ?1
              WHERE segment_id = 'pb-raw-authority'",
            [PLAYBACK_POLICY_VERSION + 1],
        )
        .unwrap();
    assert!(
        !db.has_sufficient_playback_evidence("pb-raw-authority", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap(),
        "raw counters minted under another policy cannot authorize this policy"
    );

    db.connection()
        .execute(
            "UPDATE playback_receipts
                SET played_ms = 100, clip_duration_ms = 100, policy_version = ?1
              WHERE segment_id = 'pb-raw-authority'",
            [PLAYBACK_POLICY_VERSION],
        )
        .unwrap();
    assert!(
        !db.has_sufficient_playback_evidence("pb-raw-authority", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap(),
        "a client-sized denominator cannot replace the server's 1000ms duration"
    );
}

/// The bar is a TUNED number, so pin it by behaviour and not only by its own name.
///
/// Every other receipt fixture sits at 0%, 50% or 100%, so the comparison could drift — `>` for
/// `>=`, an off-by-one, a literal hardcoded beside the constant — and the bar could be moved
/// anywhere between 0.51 and 1.00 without reddening a single test.
///
/// This asserts the boundary BEHAVES as the constant declares. It deliberately does not pin the
/// literal value (the owner tunes it; it went 0.90 -> 0.85 on 2026-08-19), so read it as "the
/// function and the constant agree", not as "the bar is 0.85".
#[test]
fn the_listening_bar_sits_exactly_where_the_constant_says() {
    let db = make_db();
    let total = 10_000_i64;
    let just_under = ((MIN_PLAYBACK_COVERAGE * total as f64) - 1.0).floor() as i64;
    let just_over = (MIN_PLAYBACK_COVERAGE * total as f64).ceil() as i64;

    insert_playback_segment(&db, "pb-bar-lo", total);
    db.record_playback_receipt_raw(&receipt("pb-bar-lo", 1, TEST_AUDIO_CONTENT_HASH, just_under, total)).unwrap();
    assert!(
        !db.has_sufficient_playback_evidence("pb-bar-lo", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap(),
        "{just_under}ms of {total}ms is below the bar and must not satisfy it"
    );

    insert_playback_segment(&db, "pb-bar-hi", total);
    db.record_playback_receipt_raw(&receipt("pb-bar-hi", 1, TEST_AUDIO_CONTENT_HASH, just_over, total)).unwrap();
    assert!(
        db.has_sufficient_playback_evidence("pb-bar-hi", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap(),
        "{just_over}ms of {total}ms is at or above the bar and must satisfy it"
    );
}

#[test]
fn half_a_sentence_is_not_enough_to_judge_it() {
    let db = make_db();
    insert_playback_segment(&db, "pb-4", 9_000);
    db.record_playback_receipt_raw(&receipt("pb-4", 1, TEST_AUDIO_CONTENT_HASH, 4_500, 9_000)).unwrap();
    assert!(!db.has_sufficient_playback_evidence("pb-4", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap());
}

#[test]
fn a_previous_clips_listen_can_never_unlock_this_one() {
    // The generation/source-change race, at the level that actually enforces it.
    let db = make_db();
    insert_playback_segment(&db, "pb-5", 9_000);
    insert_playback_segment(&db, "pb-6", 9_000);
    db.record_playback_receipt_raw(&receipt("pb-5", 1, TEST_AUDIO_CONTENT_HASH, 9_000, 9_000)).unwrap();
    assert!(
        !db.has_sufficient_playback_evidence("pb-6", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap(),
        "evidence for one clip must not unlock a different clip"
    );
}

#[test]
fn a_listen_of_different_audio_bytes_does_not_count() {
    // A policy-3 receipt with bytes other than the retained segment's decoded-PCM BLAKE3 identity
    // is rejected at insertion, before it can become playback authority.
    let db = make_db();
    insert_playback_segment(&db, "pb-7", 9_000);
    let err = db
        .record_playback_receipt_raw(&receipt("pb-7", 1, OTHER_AUDIO_CONTENT_HASH, 9_000, 9_000))
        .expect_err("mismatched decoded-PCM identity must fail closed");
    assert!(err.to_string().contains("policy-3 playback evidence"), "unexpected refusal: {err}");
    assert!(!db.has_sufficient_playback_evidence("pb-7", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap());
    let receipt_count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM playback_receipts WHERE segment_id = 'pb-7'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(receipt_count, 0, "a rejected identity must leave no receipt behind");
}

#[test]
fn a_correction_requires_its_own_listen() {
    // Re-review after an edit changes the text under judgement, so the earlier listen does not carry.
    let db = make_db();
    insert_playback_segment(&db, "pb-8", 9_000);
    db.record_playback_receipt_raw(&receipt("pb-8", 1, TEST_AUDIO_CONTENT_HASH, 9_000, 9_000)).unwrap();
    assert!(db.has_sufficient_playback_evidence("pb-8", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap());
    assert!(
        !db.has_sufficient_playback_evidence("pb-8", 2, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap(),
        "revision 1 is a different judgement and needs its own evidence"
    );
}

#[test]
fn seeking_and_replaying_accumulate_honestly() {
    // Cumulative media time: two partial listens that together cover the clip DO count, because the
    // reviewer did hear all of it — while wall-clock or a play() count would have accepted neither.
    let db = make_db();
    insert_playback_segment(&db, "pb-9", 9_000);
    db.record_playback_receipt_raw(&receipt("pb-9", 1, TEST_AUDIO_CONTENT_HASH, 5_000, 9_000)).unwrap();
    assert!(!db.has_sufficient_playback_evidence("pb-9", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap());
    db.record_playback_receipt_raw(&receipt("pb-9", 1, TEST_AUDIO_CONTENT_HASH, 8_700, 9_000)).unwrap();
    assert!(db.has_sufficient_playback_evidence("pb-9", 1, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap());
}

#[test]
fn a_zero_duration_clip_can_never_be_certified_heard() {
    // Corrupt/empty audio must fail closed at mint, not persist a receipt that later happens to be
    // ignored or substitute a client-provided denominator for missing server truth.
    let db = make_db();
    insert_playback_segment(&db, "pb-10", 0);
    let revision = db.segment_review_revision("pb-10").unwrap().unwrap();
    let claim = receipt("pb-10", revision, TEST_AUDIO_CONTENT_HASH, 1_000, 1_000);
    assert!(db.record_playback_receipt(&claim).unwrap_err().to_string().contains("positive server clip duration"));
    assert!(!db.record_playback_receipt_if_at_revision(&claim, revision).unwrap());

    let mut raw = claim;
    raw.clip_duration_ms = 0;
    assert!(db.record_playback_receipt_raw(&raw).unwrap_err().to_string().contains("positive clip duration"));
    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM playback_receipts WHERE segment_id='pb-10'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn playback_writers_reject_negative_fields_instead_of_clamping_them() {
    let db = make_db();
    insert_playback_segment(&db, "pb-negative", 1_000);
    let revision = db.segment_review_revision("pb-negative").unwrap().unwrap();
    let base = receipt("pb-negative", revision, TEST_AUDIO_CONTENT_HASH, 900, 1_000);

    let mut bad_receipts = Vec::new();
    let mut negative_revision = base.clone();
    negative_revision.segment_revision = -1;
    bad_receipts.push(negative_revision);
    let mut negative_start = base.clone();
    negative_start.started_at_ms = -1;
    bad_receipts.push(negative_start);
    let mut negative_played = base;
    negative_played.played_ms = -1;
    bad_receipts.push(negative_played);

    for bad in bad_receipts {
        assert!(db.record_playback_receipt_raw(&bad).is_err());
        assert!(db.record_playback_receipt(&bad).is_err());
        assert!(db.record_playback_receipt_if_at_revision(&bad, revision).is_err());
    }
    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM playback_receipts WHERE segment_id='pb-negative'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn a_receipt_records_which_policy_it_satisfied() {
    let db = make_db();
    insert_playback_segment(&db, "pb-11", 9_000);
    let revision = db.segment_review_revision("pb-11").unwrap().unwrap();
    db.record_playback_receipt_raw(&receipt("pb-11", revision, TEST_AUDIO_CONTENT_HASH, 9_000, 9_000)).unwrap();
    let version: i64 = db
        .connection()
        .query_row("SELECT policy_version FROM playback_receipts WHERE segment_id='pb-11'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, PLAYBACK_POLICY_VERSION, "a receipt must say which rule it met");
}

#[test]
fn a_verdict_without_a_listen_is_refused_by_the_backend() {
    // The enforcement point is the backend: a decision surface can be reloaded, scripted or replayed
    // offline, so a disabled button is usability, not a guarantee.
    let db = make_db();
    db.insert_segment(&make_segment("pb-guard-1", "/a/clip.wav")).unwrap();
    let error =
        db.require_playback_evidence("pb-guard-1", 0, TEST_AUDIO_CONTENT_HASH, Some("Sara")).unwrap_err().to_string();
    assert!(error.contains("E_NO_PLAYBACK_EVIDENCE"), "refusal must be machine-readable, got: {error}");
}

#[test]
fn a_verdict_with_a_real_listen_is_allowed() {
    let db = make_db();
    db.insert_segment(&make_segment("pb-guard-2", "/a/clip.wav")).unwrap();
    ensure_test_audio_content_hash(&db, "pb-guard-2");
    // Through the REAL front door: the mint resolves identity from the row, so the check derives it
    // the same way every production caller does, instead of hardcoding a content hash.
    db.record_playback_receipt(&receipt("pb-guard-2", 0, OTHER_AUDIO_CONTENT_HASH, 9_000, 9_000)).unwrap();
    let revision = db.segment_review_revision("pb-guard-2").unwrap().unwrap_or(0);
    let content_hash =
        db.segment_audio_content_hash("pb-guard-2").unwrap().expect("fixture has exact audio content hash");
    db.require_playback_evidence("pb-guard-2", revision, &content_hash, Some("Sara"))
        .expect("a heard clip must be decidable");
}

#[test]
fn evidence_for_the_wrong_clip_does_not_satisfy_the_guard() {
    let db = make_db();
    db.insert_segment(&make_segment("pb-guard-3", "/a/clip.wav")).unwrap();
    db.insert_segment(&make_segment("pb-guard-4", "/a/other.wav")).unwrap();
    ensure_test_audio_content_hash(&db, "pb-guard-3");
    ensure_test_audio_content_hash(&db, "pb-guard-4");
    db.record_playback_receipt(&receipt("pb-guard-3", 0, OTHER_AUDIO_CONTENT_HASH, 9_000, 9_000)).unwrap();
    let other_revision = db.segment_review_revision("pb-guard-4").unwrap().unwrap();
    assert!(db.require_playback_evidence("pb-guard-4", other_revision, TEST_AUDIO_CONTENT_HASH, Some("Sara")).is_err());
}

/// 2026-08-20 external review, blocker #1: "rows exist" must never mean "file completed". A
/// crash between persist_segments and the champion pass leaves `[Pending WSL 7B ASR]` rows (the
/// 2026-08-14 incident left 36 of them), and resume used to ADOPT them as a finished file. This
/// helper is the discriminator resume now uses to discard the stage instead.
#[test]
fn placeholder_rows_mark_a_file_as_an_interrupted_stage() {
    let db = make_db();
    let path = "/audio/interrupted-episode.wav";
    let mut staged = make_segment("stage-1", path);
    staged.raw_transcript = "[Pending WSL 7B ASR]".to_string();
    db.insert_segment(&staged).unwrap();
    let mut done = make_segment("stage-2", path);
    done.raw_transcript = "دەقی تەواو".to_string();
    db.insert_segment(&done).unwrap();

    assert!(
        db.audio_path_has_placeholder_rows(path).unwrap(),
        "one placeholder row is enough: the champion never finished this file"
    );

    // The champion fills the placeholder: the file becomes adoptable.
    db.connection()
        .execute("UPDATE speech_segments SET raw_transcript = 'دەقی چامپیۆن' WHERE id = 'stage-1'", [])
        .unwrap();
    assert!(!db.audio_path_has_placeholder_rows(path).unwrap(), "all real text = a completed file");

    // An EMPTY draft is an unfinished stage too, not a completed file.
    db.connection().execute("UPDATE speech_segments SET raw_transcript = '  ' WHERE id = 'stage-2'", []).unwrap();
    assert!(db.audio_path_has_placeholder_rows(path).unwrap(), "blank text cannot be adopted as done");

    assert!(!db.audio_path_has_placeholder_rows("/audio/other.wav").unwrap(), "no rows = nothing staged");
}
