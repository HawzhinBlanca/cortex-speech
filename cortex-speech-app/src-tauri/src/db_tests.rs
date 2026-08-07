//! Unit tests for `db.rs`, split out via `#[path]` (Week-4 decomposition) to keep db.rs itself
//! under the 3-4k-line target. Included from db.rs as `#[cfg(test)] #[path = "db_tests.rs"] mod tests;`
//! so `super::*` still resolves to the `db` module. Tests are UNCHANGED — only relocated.

use super::*;

fn make_db() -> Database {
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    db
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

    // Contrast — the BUG the fix removes: the old whole-row upsert of the STALE snapshot wipes the
    // concurrent edit back to None.
    let mut whole_row = stale.clone();
    whole_row.normalized_transcript = Some("NORMALIZED".to_string());
    db.insert_segment(&whole_row).unwrap();
    let clobbered = db.get_segment_by_id("n1").unwrap().unwrap();
    assert_eq!(
        clobbered.annotated_transcript, None,
        "the whole-row upsert of the stale snapshot CLOBBERS the concurrent edit — what the targeted update avoids"
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
    db.write_segment_verdict("seg-1", "auto_accept", Some("t"), None, None, Some(0.9), false).unwrap();
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
fn redecision_undo_and_retranscribe_sequence_preserves_or_resets_exactly_as_designed() {
    // Reproduction of the 2026-07-14 live-test data-loss sequence, mechanically, at the DB layer —
    // written BEFORE any fix, per process. The UI flow on an ALREADY-VERIFIED clip was:
    //   (1) save a new decision (recordHumanDecision + whole-row upsert),
    //   (2) Undo review (clearHumanDecision + upsert of the pre-save snapshot),
    //   (3) re-transcribe (upsert with a fresh machine draft + verified=false).
    // This pins what each stage does to the owner's gold so the responsibilities are provable:
    // the undo RESTORES everything (incl. the prior decision, via human_decision=excluded);
    // the RE-TRANSCRIBE is the destructive step (fresh draft wipes annotated + verified by design).
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();

    // Owner's reviewed row: verified gold with an 'edit' decision.
    let mut owner = make_segment("s1", "/a.wav");
    owner.annotated_transcript = Some("owner gold کە کە".into());
    owner.verified = true;
    db.insert_segment(&owner).unwrap();
    db.record_human_decision("s1", "edit", Some("owner gold کە کە"), None).unwrap();
    // The frontend STORE row mirrors the full DB row (all columns selected) — snapshot it like
    // ReviewMode's `{...seg}` undo entry does.
    let prev_snapshot = db.get_segment_by_id("s1").unwrap().unwrap();
    assert_eq!(prev_snapshot.human_decision.as_deref(), Some("edit"), "precondition: decision in snapshot");

    // (1) A NEW decision is saved over it (the live test's 'Use this text' + Save).
    db.record_human_decision("s1", "edit", Some("gemini text خۆ"), None).unwrap();
    let mut resaved = db.get_segment_by_id("s1").unwrap().unwrap();
    resaved.annotated_transcript = Some("gemini text خۆ".into());
    resaved.verified = true;
    db.insert_segment(&resaved).unwrap();

    // (2a) THE BUG, documented: the pre-fix undo pair (clearHumanDecision + plain updateSegment
    // upsert) loses the PRIOR decision, because insert_segment deliberately omits the decision
    // columns (anti-clobber for ordinary edits). This is exactly the 2026-07-14 live data loss.
    db.clear_human_decision("s1").unwrap();
    db.insert_segment(&prev_snapshot).unwrap();
    let after_old_pair = db.get_segment_by_id("s1").unwrap().unwrap();
    assert_eq!(
        after_old_pair.annotated_transcript.as_deref(),
        Some("owner gold کە کە"),
        "the old pair does restore the transcript (which is why the loss was so easy to miss)"
    );
    assert!(after_old_pair.verified, "the old pair does restore verified");
    assert_eq!(
        after_old_pair.human_decision, None,
        "DOCUMENTED BUG: the old clear+upsert pair silently loses the prior decision — the reason \
             undoLast now uses restore_segment_snapshot instead"
    );

    // (2b) THE FIX: the lossless snapshot restore (what undoLast calls now) brings back the FULL
    // pre-save state — prior decision included.
    db.insert_segment_full(&prev_snapshot).unwrap();
    let after_undo = db.get_segment_by_id("s1").unwrap().unwrap();
    assert_eq!(after_undo.annotated_transcript.as_deref(), Some("owner gold کە کە"));
    assert!(after_undo.verified);
    assert_eq!(
        after_undo.human_decision.as_deref(),
        Some("edit"),
        "insert_segment_full must restore the PRIOR decision losslessly"
    );

    // (3) Re-transcribe on the (restored, verified) clip: fresh draft + verified=false, exactly
    // what ReviewMode.retranscribe upserts. THIS is the destructive-by-design step.
    let mut retranscribed = db.get_segment_by_id("s1").unwrap().unwrap();
    retranscribed.raw_transcript = "fresh machine draft".into();
    retranscribed.annotated_transcript = Some("fresh machine draft".into());
    retranscribed.verified = false;
    db.insert_segment(&retranscribed).unwrap();
    let after_rt = db.get_segment_by_id("s1").unwrap().unwrap();
    assert!(!after_rt.verified, "re-transcribe reopens the clip");
    assert_eq!(after_rt.annotated_transcript.as_deref(), Some("fresh machine draft"));
    // The decision column itself survives a re-transcribe upsert (it rides the row) — so the final
    // NULL observed live can only have come from the UNDO's clear if stage (2) failed, or from the
    // row having no decision at snapshot time. This assertion documents the mechanical truth.
    assert_eq!(
        after_rt.human_decision.as_deref(),
        Some("edit"),
        "a re-transcribe upsert does not itself clear human_decision"
    );
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
    // Write-path audit (Week 2): the verdict UPDATE and the decision_verdicts INSERT are one
    // invariant. Fault-inject the second statement by dropping its table: the whole write must
    // FAIL and the verdict UPDATE must ROLL BACK — never a verdict without its C4 denominator row.
    let db = make_db();
    db.insert_segment(&make_segment("atom", "/audio/s.wav")).unwrap();
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
    db.write_segment_verdict("confident", "escalated", None, None, None, Some(0.9), true).unwrap();
    db.write_segment_verdict("shaky", "escalated", None, None, None, Some(0.2), true).unwrap();
    db.write_segment_verdict("legacy", "escalated", None, None, None, None, true).unwrap();

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

    // A verdict written through the dedicated connection persists to the same file and is visible
    // back to the primary — so the jury writing through it loses nothing.
    dedicated.write_segment_verdict("s1", "jury_accept", Some("hi"), None, None, Some(0.9), false).unwrap();
    let seen = primary.get_segment_by_id("s1").unwrap().unwrap();
    assert_eq!(seen.verdict.as_deref(), Some("jury_accept"));
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
    let plant = |id: &str, raw: &str, answer: Option<&str>| {
        let mut s = make_segment(id, &format!("/{id}.wav"));
        s.raw_transcript = raw.to_string();
        s.verified = true;
        s.is_gold = true;
        if let Some(a) = answer {
            s.human_decision = Some("edit".into());
            s.verdict = Some("human_edit".into());
            s.verdict_transcript = Some(a.to_string());
        }
        db.insert_segment_full(&s).unwrap();
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
        let mut peer = make_segment("sc-peer-edit", "/peer.wav");
        peer.raw_transcript = "دەقی هەڵە".into();
        peer.verified = true;
        peer.is_gold = false;
        peer.human_decision = Some("edit".into());
        peer.verdict = Some("human_edit".into());
        peer.verdict_transcript = Some("وەڵامی هاوکار".into());
        peer.reviewed_by = Some("Hemn".into()); // decided on a phone, by someone who is not the owner
        db.insert_segment_full(&peer).unwrap();
    }
    // The OWNER's own desktop verification: not flagged gold either, but `reviewed_by` is NULL
    // because the desktop path passes no annotator. This is the case that makes the mechanism
    // reachable at all — without it the candidate set is empty in every real installation.
    {
        let mut owner = make_segment("sc-owner-edit", "/owner.wav");
        owner.raw_transcript = "دەقی هەڵەی سێ".into();
        owner.verified = true;
        owner.is_gold = false;
        owner.human_decision = Some("edit".into());
        owner.verdict = Some("human_edit".into());
        owner.verdict_transcript = Some("ڕاستی سێ".into());
        owner.reviewed_by = None;
        db.insert_segment_full(&owner).unwrap();
    }

    let ids = |limit: usize| -> Vec<String> {
        db.list_spot_check_candidates(limit, "Sara", &std::collections::HashSet::new())
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

    // The expected text is the HUMAN answer, never the raw draft — grading against the draft would
    // score a blind accept as perfect. Asserted against the row that came back rather than a
    // hardcoded string: the answer key must be right for EVERY candidate, not just whichever one
    // happens to sort first.
    for (seg, expected) in db.list_spot_check_candidates(10, "Sara", &std::collections::HashSet::new()).unwrap() {
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
        .list_spot_check_candidates(10, "Hemn", &std::collections::HashSet::new())
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
fn a_human_decision_records_which_reviewer_made_it() {
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

    db.record_human_decision_by("att-phone", "accept", None, None, Some("Sara")).unwrap();
    assert_eq!(reviewed_by("att-phone").as_deref(), Some("Sara"), "an attributed decision names its reviewer");

    db.record_human_decision("att-desktop", "accept", None, None).unwrap();
    assert_eq!(reviewed_by("att-desktop"), None, "an unattributed decision stores NULL, never a made-up name");

    db.record_human_decision_by("att-redecide", "accept", None, None, Some("Sara")).unwrap();
    db.record_human_decision_by("att-redecide", "edit", Some("ڕاستکراوە"), None, Some("Hemn")).unwrap();
    assert_eq!(reviewed_by("att-redecide").as_deref(), Some("Hemn"), "the CURRENT decision's author wins");
    db.record_human_decision("att-redecide", "accept", None, None).unwrap();
    assert_eq!(reviewed_by("att-redecide"), None, "a desktop re-review clears the previous reviewer's name");

    db.clear_human_decision("att-phone").unwrap();
    assert_eq!(reviewed_by("att-phone"), None, "undo retracts the attribution along with the decision");
}

#[test]
fn reviewer_attribution_survives_a_whole_row_upsert() {
    // WHOLE-ROW CLOBBER — the recurring defect family in this file. `insert_segment_full` rewrites EVERY
    // column from a snapshot, and the couch's own undo path uses it. A `reviewed_by` missing from that
    // statement's column list would silently revert to NULL on any restore, stripping the attribution off
    // rows that still carry the decision. `insert_segment`'s 17-column subset deliberately OMITS it, the
    // same way it omits human_decision, so an ASR-only re-write must LEAVE it intact, not clear it.
    let db = make_db();
    db.insert_segment(&make_segment("rt-1", "/rt-1.wav")).unwrap();
    db.record_human_decision_by("rt-1", "accept", None, Some(4200), Some("Sara")).unwrap();

    // Round-trip the FULL row, exactly as the couch undo does.
    let snapshot = db.get_segment_by_id("rt-1").unwrap().unwrap();
    assert_eq!(snapshot.reviewed_by.as_deref(), Some("Sara"));
    db.insert_segment_full(&snapshot).unwrap();
    assert_eq!(
        db.get_segment_by_id("rt-1").unwrap().unwrap().reviewed_by.as_deref(),
        Some("Sara"),
        "insert_segment_full must persist reviewed_by — dropping it is the whole-row-clobber bug"
    );

    // The ASR-column subset must not touch it (it never carries a human decision).
    let mut asr_only = make_segment("rt-1", "/rt-1.wav");
    asr_only.raw_transcript = "re-decoded".to_string();
    db.insert_segment(&asr_only).unwrap();
    assert_eq!(
        db.get_segment_by_id("rt-1").unwrap().unwrap().reviewed_by.as_deref(),
        Some("Sara"),
        "an ASR-only upsert must leave the human attribution intact"
    );
}

#[test]
fn write_segment_verdict_records_all_machine_verdict_classes() {
    // P1.2: decision_verdicts must classify every machine verdict for the C4 denominator. Before the
    // fix only jury_accept recorded a row; auto_accept and jury_edit (also auto-resolutions) dropped.
    let db = Database::open(":memory:").unwrap();
    db.initialize().unwrap();
    for id in ["aa", "je", "es", "hv"] {
        db.insert_segment(&make_segment(id, &format!("/{id}.wav"))).unwrap();
    }
    db.record_human_decision("hv", "accept", None, None).unwrap();

    db.write_segment_verdict("aa", "auto_accept", Some("m"), None, None, Some(0.9), false).unwrap();
    db.write_segment_verdict("je", "jury_edit", Some("m"), None, None, Some(0.8), false).unwrap();
    db.write_segment_verdict("es", "escalated", None, None, None, None, true).unwrap();
    db.write_segment_verdict("hv", "auto_accept", Some("m"), None, None, Some(0.9), false).unwrap();

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

    // Re-apply the repair migration (rewind past v35 so run_migrations re-runs it). v36's
    // proof-metadata ALTERs are not idempotent, so undo them first (its down_sql) or the
    // re-run fails on "duplicate column name". Same for v39's RENAME COLUMN — re-running it on an
    // already-renamed schema fails with "no such column: ood_score" — so undo it too. And the same for
    // v52's rename: the replay passes back through v40, whose INSERT…SELECT names `agent_confidence`
    // explicitly, so the column has to be under its pre-v52 name for that leg exactly as it would be
    // during a real upgrade. v52 re-runs at the end of the replay and restores the HEAD name.
    db.connection()
        .execute_batch(
            "DROP INDEX IF EXISTS idx_segments_confidence_source;
                 DROP INDEX IF EXISTS idx_segments_cloud_call;
                 ALTER TABLE speech_segments DROP COLUMN normalizer_version;
                 ALTER TABLE speech_segments DROP COLUMN decoder_config_hash;
                 ALTER TABLE speech_segments DROP COLUMN cloud_call;
                 ALTER TABLE speech_segments DROP COLUMN confidence_source;
                 ALTER TABLE speech_segments RENAME COLUMN signal_anomaly_score TO ood_score;
                 ALTER TABLE speech_segments RENAME COLUMN agreement_score TO agent_confidence;",
        )
        .unwrap();
    db.connection().execute("DELETE FROM schema_migrations WHERE version >= 35", []).unwrap();
    crate::migrations::run_migrations(&db).unwrap();

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
    db.insert_segment(&seg).unwrap();

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
            Some("omniasr-ctc-300m"),
            false,
        )
        .unwrap());
    let s1 = db.get_segment_by_audio_path("/u1.wav").unwrap().unwrap();
    assert_eq!(s1.raw_transcript, composed, "ASR-update raw_transcript must be stored NFC");
    assert_eq!(s1.confidence_source.as_deref(), Some("heuristic"));
    assert_eq!(s1.model_version_id.as_deref(), Some("omniasr-ctc-300m"));
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
    assert!(db.update_verified("ver-1", true).unwrap());
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
fn machine_verdict_never_overwrites_a_human_decision() {
    // The jury (machine) write runs on a separate connection and may land AFTER a curator decided the
    // same segment mid-run. The human is authoritative: a late write_segment_verdict must be a no-op,
    // never reverting the human verdict/transcript or re-escalating an accepted segment.
    let db = make_db();
    db.insert_segment(&make_segment("hv1", "/hv1.wav")).unwrap();
    db.record_human_decision("hv1", "accept", None, None).unwrap();

    db.write_segment_verdict("hv1", "jury_accept", Some("machine consensus"), Some("r"), None, Some(0.9), true)
        .unwrap();

    let seg = db.get_segment_by_id("hv1").unwrap().unwrap();
    assert_eq!(seg.verdict.as_deref(), Some("human_accept"), "machine verdict clobbered the human decision");
    assert_eq!(seg.human_decision.as_deref(), Some("accept"), "human_decision must be preserved");
    assert!(!seg.escalated, "a human-accepted segment must not be re-escalated by a late machine write");

    // Sanity: the SAME machine write DOES apply to a fresh (non-human) segment — the guard is targeted.
    db.insert_segment(&make_segment("hv2", "/hv2.wav")).unwrap();
    db.write_segment_verdict("hv2", "jury_accept", Some("machine"), None, None, Some(0.8), false).unwrap();
    let seg2 = db.get_segment_by_id("hv2").unwrap().unwrap();
    assert_eq!(seg2.verdict.as_deref(), Some("jury_accept"), "a machine verdict must apply to a non-human segment");
}

#[test]
fn clear_human_decision_reopens_the_segment_for_re_adjudication() {
    // Undo of a human decision must FULLY re-open the segment: clear the human decision AND the
    // verdict it set (the pre-decision machine verdict is gone), returning it to the review queue.
    // Otherwise the stale verdict='human_*' both shows as decided on reload and blocks re-jury.
    let db = make_db();
    db.insert_segment(&make_segment("cl1", "/cl1.wav")).unwrap();
    db.record_human_decision("cl1", "edit", Some("human gold"), None).unwrap();
    assert_eq!(db.get_segment_by_id("cl1").unwrap().unwrap().verdict.as_deref(), Some("human_edit"));

    db.clear_human_decision("cl1").unwrap();
    let cleared = db.get_segment_by_id("cl1").unwrap().unwrap();
    assert_eq!(cleared.human_decision, None, "human_decision must be cleared");
    assert_eq!(cleared.verdict, None, "the stale human verdict must be cleared, not left as 'human_edit'");
    assert_eq!(cleared.verdict_transcript, None, "the human gold transcript is part of the undone decision");
    assert!(cleared.escalated, "a re-opened segment returns to the review queue");

    // A fresh machine verdict now applies (the human-decision guard no longer blocks it).
    db.write_segment_verdict("cl1", "jury_accept", Some("machine"), None, None, Some(0.8), false).unwrap();
    assert_eq!(db.get_segment_by_id("cl1").unwrap().unwrap().verdict.as_deref(), Some("jury_accept"));
}

#[test]
fn clear_escalation_is_the_exact_inverse_of_a_flag() {
    // A UI flag() sets verdict='escalated' + escalated=1. Undo must clear BOTH (unlike
    // clear_human_decision, which sets escalated=1). And it must NOT touch a segment that a human
    // decided after the flag.
    let db = make_db();
    db.insert_segment(&make_segment("fl1", "/fl1.wav")).unwrap();
    db.write_segment_verdict("fl1", "escalated", None, Some("Flagged for second-pass adjudication"), None, None, true)
        .unwrap();
    let flagged = db.get_segment_by_id("fl1").unwrap().unwrap();
    assert!(flagged.escalated && flagged.verdict.as_deref() == Some("escalated"));

    db.clear_escalation("fl1").unwrap();
    let un = db.get_segment_by_id("fl1").unwrap().unwrap();
    assert!(!un.escalated, "escalated flag must be cleared (inverse of flag)");
    assert_eq!(un.verdict, None, "the 'escalated' verdict must be cleared");
    assert_eq!(un.rationale, None, "the flag rationale must be cleared");

    // Guard: once a human has decided, clear_escalation must be a no-op (never stomp a decision).
    db.insert_segment(&make_segment("fl2", "/fl2.wav")).unwrap();
    db.write_segment_verdict("fl2", "escalated", None, Some("flag"), None, None, true).unwrap();
    db.record_human_decision("fl2", "accept", None, None).unwrap();
    db.clear_escalation("fl2").unwrap();
    let kept = db.get_segment_by_id("fl2").unwrap().unwrap();
    assert_eq!(kept.human_decision.as_deref(), Some("accept"), "a human-decided row must be untouched");
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
    first.raw_transcript = "hawzhin reliable transcript".to_string();
    let mut second = make_segment("fts-2", "/data/audio/fts-2.wav");
    second.raw_transcript = "hawzhin retained transcript".to_string();

    db.insert_segment(&first).expect("insert first");
    db.insert_segment(&second).expect("insert second");

    let before_delete = db.search_segments("hawzhin").expect("search before delete");
    assert_eq!(before_delete.len(), 2, "FTS should index inserted transcripts");

    db.delete_segments_batch(&["fts-1".to_string()]).expect("batch delete");

    let after_delete = db.search_segments("hawzhin").expect("search after delete");
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
    first.raw_transcript = "hawzhin vacuumable transcript".to_string();
    let mut second = make_segment("vac-2", "/data/audio/vac-2.wav");
    second.raw_transcript = "hawzhin surviving transcript".to_string();
    db.insert_segment(&first).expect("insert first");
    db.insert_segment(&second).expect("insert second");
    db.delete_segments_batch(&["vac-1".to_string()]).expect("batch delete");

    db.vacuum().expect("vacuum must succeed");

    let hits = db.search_segments("hawzhin").expect("search after vacuum");
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
fn batch_transcription_update_preserves_human_review_and_seeds_annotation() {
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
            Some("omniasr-ctc-300m"),
            false,
            "fresh asr",
        )
        .expect("update verified");
    assert!(!updated, "a verified row must be skipped, not updated");
    let after = db.get_segment_by_id("verified-1").unwrap().unwrap();
    assert!(after.verified, "verified flag must NOT be reverted by the batch");
    assert_eq!(after.annotated_transcript.as_deref(), Some("human gold"), "human annotation preserved");
    assert_eq!(after.raw_transcript, "old asr", "human-owned row's raw must not be clobbered");

    // (c): a fresh unreviewed row with no annotation IS updated and seeds the annotation.
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
            Some("omniasr-ctc-300m"),
            false,
            "new asr",
        )
        .expect("update fresh");
    assert!(updated, "an unreviewed row is updated");
    let after = db.get_segment_by_id("fresh-1").unwrap().unwrap();
    assert_eq!(after.raw_transcript, "new asr");
    assert_eq!(after.annotated_transcript.as_deref(), Some("new asr"), "annotation seeded when empty");
    assert_eq!(after.confidence_source.as_deref(), Some("heuristic"));
    assert_eq!(after.model_version_id.as_deref(), Some("omniasr-ctc-300m"));
    assert!(!after.cloud_call);

    // (d): an unverified row the user annotated (without verifying) keeps that annotation; only
    // the ASR fields refresh — the seed is ignored because COALESCE reads the CURRENT row.
    let mut annotated = make_segment("annot-1", "/c.wav");
    annotated.raw_transcript = "old".to_string();
    annotated.annotated_transcript = Some("user typed".to_string());
    db.insert_segment(&annotated).expect("insert annotated");
    let updated = db
        .update_batch_transcription_if_unreviewed(
            "annot-1",
            "new asr",
            Some("new asr"),
            Some(0.7),
            Some("real_posterior"),
            Some("omniasr-ctc-1b"),
            false,
            "seed ignored",
        )
        .expect("update annotated");
    assert!(updated, "an unverified annotated row still refreshes ASR");
    let after = db.get_segment_by_id("annot-1").unwrap().unwrap();
    assert_eq!(after.annotated_transcript.as_deref(), Some("user typed"), "existing annotation preserved (COALESCE)");
    assert_eq!(after.raw_transcript, "new asr", "raw ASR refreshed on an unverified row");
    assert_eq!(after.confidence_source.as_deref(), Some("real_posterior"));
    assert_eq!(after.model_version_id.as_deref(), Some("omniasr-ctc-1b"));
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

    // A no-op edit: the corrected text equals the raw ASR (up to the learning key).
    db.record_human_decision("noop-1", "edit", Some("hello world"), None).expect("record no-op edit");

    let ledger_rows: i64 =
        db.conn.query_row("SELECT COUNT(*) FROM corrections WHERE segment_id='noop-1'", [], |r| r.get(0)).unwrap();
    assert_eq!(ledger_rows, 0, "a no-op edit must NOT append a corrections-ledger row");

    // A genuine correction on the same kind of row DOES record a ledger entry.
    let mut seg2 = make_segment("real-1", &audio_path);
    seg2.raw_transcript = "helo wrld".to_string();
    db.insert_segment(&seg2).expect("insert real");
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
    assert!(db.update_verified("merge-ver", true).unwrap());
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

    // A NEW verified row (id not present locally) is still importable — the guard only refuses OVERWRITES.
    let fresh = vec![SpeechSegment {
        id: "merge-new".to_string(),
        audio_path: "/n.wav".to_string(),
        raw_transcript: "brand new".to_string(),
        verified: true,
        duration_ms: 1000,
        ..SpeechSegment::default()
    }];
    let (created2, _u2) = db.merge_dataset_json(&serde_json::to_string(&fresh).unwrap()).expect("merge new");
    assert_eq!(created2, 1, "a new verified row (not a local overwrite) still imports");
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
    seg.verdict = Some("jury_accept".to_string());
    seg.verdict_transcript = Some("agent proposed transcript".to_string());
    seg.escalated = true;
    db.insert_segment(&seg).expect("insert segment");
    db.write_segment_verdict(
        "learn-agent",
        "jury_accept",
        Some("agent proposed transcript"),
        Some("agent rationale"),
        None,
        Some(0.81),
        true,
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
    seg.verdict_transcript = Some("same   text".to_string());
    db.insert_segment(&seg).expect("insert segment");
    db.write_segment_verdict("learn-same", "jury_accept", Some("same   text"), None, None, Some(0.9), true)
        .expect("write agent verdict");

    db.record_human_decision("learn-same", "edit", Some("same text"), None).expect("record human edit");

    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM agent_examples WHERE segment_id = ?1", params!["learn-same"], |row| row.get(0))
        .expect("count examples");
    assert_eq!(count, 0);
}

#[test]
fn record_human_decision_appends_to_corrections_ledger() {
    let db = make_db();
    // A real on-disk audio file so the durable content hash (the ledger's identity) can be
    // computed, even though the database itself is in memory.
    let tmp = tempfile::tempdir().expect("tempdir");
    let audio = tmp.path().join("clip.wav");
    std::fs::write(&audio, b"RIFF....fake-audio-bytes").expect("write audio");
    let expected_hash = crate::pipeline::source_audio_identity(&audio).expect("identity").content_hash;

    let mut seg = make_segment("led-1", audio.to_str().expect("audio path"));
    seg.raw_transcript = "wrong text".to_string();
    db.insert_segment(&seg).expect("insert segment");
    // The agent verdict the human is about to override (captured into jury_verdict).
    db.write_segment_verdict("led-1", "jury_accept", Some("agent guess"), None, None, Some(0.7), true)
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
    assert_eq!(hash, expected_hash, "the ledger must key on the durable audio content hash");
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
fn edit_with_missing_audio_still_records_verdict_without_ledger_row() {
    // Best-effort ledger: a missing audio file must never block the human's correction.
    let db = make_db();
    let mut seg = make_segment("led-missing", "/nonexistent/gone.wav");
    seg.raw_transcript = "wrong".to_string();
    db.insert_segment(&seg).expect("insert segment");

    db.record_human_decision("led-missing", "edit", Some("right"), None).expect("edit must still succeed");

    let fresh = db.get_segment_by_id("led-missing").expect("load").expect("exists");
    assert_eq!(fresh.human_decision.as_deref(), Some("edit"), "the verdict is recorded despite missing audio");
    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM corrections WHERE segment_id = ?1", params!["led-missing"], |r| r.get(0))
        .expect("count");
    assert_eq!(count, 0, "no ledger row when the audio identity cannot be computed");
}

#[test]
fn edit_populates_correction_memory_with_substitution() {
    let db = make_db();
    let mut seg = make_segment("mem-1", "/data/audio/mem-1.wav");
    seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
    db.insert_segment(&seg).expect("insert");
    db.record_human_decision("mem-1", "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");

    let (wrong, human, hits): (String, String, i64) = db
        .connection()
        .query_row(
            "SELECT wrong_token, human_token, hit_count FROM correction_memory WHERE source_segment = ?1",
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
        db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
    }
    let (rows, max_hits): (i64, i64) = db
        .connection()
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(hit_count), 0) FROM correction_memory
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
    db.record_human_decision("mem-rep", "edit", Some("ئەو خراپ بوو ئەو خراپ بوو"), None).expect("edit");

    let (rows, max_hits): (i64, i64) = db
        .connection()
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(hit_count), 0) FROM correction_memory
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
    db.record_human_decision("cf-0", "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
    let fresh = db.load_correction_memories().expect("load")[0].confidence;
    assert!(fresh < tau, "a freshly captured memory sits at the 0.5 prior, below tau_conf: {fresh}");

    // Each further human edit of the same confusion is an independent confirmation -> confidence climbs.
    for id in ["cf-1", "cf-2"] {
        let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
        seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&seg).expect("insert");
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
        db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
    }
    let (fired_set, confirms): (i64, i64) = db
        .connection()
        .query_row(
            "SELECT last_fired_at IS NOT NULL, confirm_count FROM correction_memory WHERE wrong_token = 'باش'",
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
        .query_row("SELECT override_count FROM correction_memory WHERE wrong_token = 'باش'", [], |r| r.get(0))
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
fn deleting_a_segment_preserves_its_loop0_over_trigger_evidence() {
    // C5 survivor-bias guard: the owner's normal cleanup (review a bad clip, then delete it) must not
    // erase the over-trigger evidence that gate reads — else the gate looks safer than reality.
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

    db.delete_segment("ov-1").expect("delete");

    let ot_after =
        db.intelligence_report().expect("report")["loop0Shadow"]["firedButHumanAcceptedOriginal"].as_i64().unwrap();
    assert_eq!(ot_after, 1, "the over-trigger evidence SURVIVES deletion via the durable archive");
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
    db.write_segment_verdict("esc", "escalated", None, None, None, Some(0.83), true).unwrap();
    db.write_segment_verdict("esc", "escalated", None, Some("cloud off"), None, None, true).unwrap();
    let seg = db.get_segment_by_id("esc").unwrap().unwrap();
    assert_eq!(seg.agreement_score, Some(0.83), "a None re-write must not destroy the IRT confidence");
    // A later write that CARRIES a signal still replaces it.
    db.write_segment_verdict("esc", "escalated", None, None, None, Some(0.41), true).unwrap();
    let seg = db.get_segment_by_id("esc").unwrap().unwrap();
    assert_eq!(seg.agreement_score, Some(0.41));
}

#[test]
fn c4_precision_survives_deleting_a_contradicted_auto_accept() {
    // True-10 audit 2026-07-09 (v34, same class as the v33 C5 fix): deleting a reviewed bad
    // clip CASCADE-deleted its decision_verdicts row, shrinking t0HumanContradicted — the C4
    // precision that authorizes raising the autonomy dial could only drift optimistic. The
    // archive must preserve the contradiction across the delete.
    let db = make_db();
    db.insert_segment(&make_segment("good", "/audio/g.wav")).unwrap();
    db.insert_segment(&make_segment("bad", "/audio/b.wav")).unwrap();
    // Two T0 auto-accepts; the human confirms one and contradicts the other.
    db.write_segment_verdict("good", "auto_accept", Some("ok"), None, None, Some(0.9), false).unwrap();
    db.write_segment_verdict("bad", "auto_accept", Some("wrong"), None, None, Some(0.9), false).unwrap();
    db.record_human_decision("good", "accept", None, None).unwrap();
    db.record_human_decision("bad", "reject", None, None).unwrap();

    let before = db.intelligence_report().unwrap();
    assert_eq!(before["autoAcceptPrecision"]["t0HumanContradicted"], 1);
    assert_eq!(before["autoAcceptPrecision"]["t0HumanConfirmed"], 1);

    // The owner's documented cleanup: review the bad clip, then delete it.
    db.delete_segment("bad").unwrap();

    let after = db.intelligence_report().unwrap();
    assert_eq!(
        after["autoAcceptPrecision"]["t0HumanContradicted"], 1,
        "deleting the contradicted clip must not erase the contradiction (survivor bias)"
    );
    assert_eq!(after["autoAcceptPrecision"]["t0Accepts"], 2, "the T0 denominator survives too");
    // And batch delete folds the archive the same way.
    db.delete_segments_batch(&["good".to_string()]).unwrap();
    let final_report = db.intelligence_report().unwrap();
    assert_eq!(final_report["autoAcceptPrecision"]["t0HumanConfirmed"], 1);
    assert_eq!(final_report["autoAcceptPrecision"]["t0Accepts"], 2);
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
    // The per-segment semantics survive deletion through the archive.
    db.delete_segment("re").unwrap();
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
    db.insert_segment(&good).unwrap();
    let mut bad = make_segment("cal-bad", "/audio/cb.wav");
    bad.verified = true;
    bad.annotated_transcript = Some("دەقی خراپ".to_string());
    bad.snr_db = Some(20.0);
    db.insert_segment(&bad).unwrap();
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
fn merge_dataset_json_preserves_review_provenance_on_newly_created_rows() {
    // DATA-LOSS regression: SpeechSegment deserializes every jury / human-review / gold column, but the
    // merge's INSERT path used its own 21-column statement that silently DROPPED verdict,
    // verdict_transcript, rationale, evidence_json, agreement_score, escalated, human_decision,
    // corrected_at, is_gold, alignment_quality and created_at for NEW ids. Merging a reviewed dataset
    // into another library therefore stripped the human work product — the merged rows then graded as
    // unreviewed machine drafts. The INSERT path must be the lossless full-column insert
    // (insert_segment_full), exactly like the delete-undo restore.
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
    let (created, updated) = db.merge_dataset_json(&json).expect("merge");
    assert_eq!((created, updated), (1, 0), "a new id must take the INSERT path");

    let row = db.get_segment_by_id("merge-new-gold").unwrap().expect("row created");
    assert_eq!(row.human_decision.as_deref(), Some("edit"), "human_decision must survive the merge");
    assert!(row.is_gold, "is_gold must survive the merge");
    assert_eq!(row.verdict.as_deref(), Some("human_edit"), "verdict must survive the merge");
    assert_eq!(row.corrected_at.as_deref(), Some("2026-01-02 03:05:00"), "corrected_at must survive");
    assert_eq!(row.alignment_quality.as_deref(), Some("word_aligner"), "alignment_quality must survive");
    assert_eq!(
        row.created_at.as_deref(),
        Some("2026-01-02 03:04:05"),
        "created_at must survive, or the merged row reorders every ORDER BY created_at view/export"
    );
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
        db.insert_segment(&SpeechSegment {
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
    db.write_segment_verdict("s1", "escalated", None, None, None, Some(0.73), true).unwrap();

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
        db.insert_segment(&SpeechSegment {
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
        db.write_segment_verdict(id, "escalated", None, None, None, Some(conf), true).unwrap();
    }

    let order: Vec<String> = db.get_segments_suspect_first(None).unwrap().into_iter().map(|s| s.id).collect();

    let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
    assert!(pos("noisy") < pos("disputed"), "poor-SNR audio must outrank a disagreement: {order:?}");
    assert!(pos("clipped") < pos("disputed"), "clipped audio must outrank a disagreement: {order:?}");
    assert!(pos("disputed") < pos("clean"), "within clean audio, low agreement still comes first: {order:?}");
    assert_eq!(order.last().unwrap(), "clean", "the least suspect clip is last: {order:?}");
}
