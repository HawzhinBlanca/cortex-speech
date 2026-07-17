//! Unit tests for `export.rs`, split out via `#[path]` (Week-4 decomposition) to keep export.rs under the
//! 3-4k-line target. Included from export.rs as `#[cfg(test)] #[path = "export_tests.rs"] mod tests;`; `super::*`
//! still resolves to the parent module. Tests are UNCHANGED — only relocated (dedented one level).

use super::*;
use crate::db::{Database, SegmentHypothesis, SourceTranscriptRecord};
use tempfile::NamedTempFile;

#[test]
fn sanitized_clip_filename_blocks_path_traversal() {
    // A crafted stem/id from an imported dataset must never produce a separator or `..` escape:
    // the result is always a single, join-safe component (CWE-22 regression).
    for (stem, id) in
        [("../../etc/pass", "../../../root/id"), ("a/b\\c", ".."), ("normal", "..\\..\\win"), ("clip\0", "seg/../../x")]
    {
        let f = sanitized_clip_filename(stem, id);
        assert!(!f.contains('/') && !f.contains('\\'), "no separators: {f:?}");
        assert!(!f.contains(".."), "no parent-dir tokens: {f:?}");
        assert_eq!(std::path::Path::new(&f).components().count(), 1, "must be a single path component: {f:?}");
        assert!(std::path::Path::new("/export/dir").join(&f).starts_with("/export/dir"), "stays under dir: {f:?}");
    }
    // Normal stems/ids pass through unchanged (already filename-safe -> no disambiguating hash).
    assert_eq!(sanitized_clip_filename("clip01", "seg_42"), "clip01_seg_42.wav");
    // A v4-UUID id is filename-safe, so it passes through verbatim (no hash suffix, no collision).
    assert_eq!(
        sanitized_clip_filename("rec", "550e8400-e29b-41d4-a716-446655440000"),
        "rec_550e8400-e29b-41d4-a716-446655440000.wav"
    );
    // Two ids that collapse to the same cleaned form must NOT collide: the raw-id hash disambiguates.
    let a = sanitized_clip_filename("rec", "a/b");
    let b = sanitized_clip_filename("rec", "a.b");
    assert_ne!(a, b, "distinct ids that clean to the same value must not share a filename");
    assert!(a.starts_with("rec_a_b_") && b.starts_with("rec_a_b_"), "{a:?} {b:?}");
}

fn insert_machine_silver_segment_with_hf_coverage(
    db: &Database,
    wav_path: &std::path::Path,
    id: &str,
) -> SpeechSegment {
    let mut segment = sample_segment(id);
    segment.audio_path = wav_path.to_string_lossy().to_string();
    segment.verified = false;
    segment.annotated_transcript = None;
    segment.confidence = Some(0.95);
    segment.clipping_ratio = Some(0.0);
    segment.rms_db = Some(-20.0);
    segment.snr_db = Some(20.0);
    db.insert_segment(&segment).unwrap();
    let evidence_json = serde_json::json!({
        "referenceModelId": "multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash",
        "selectedModelId": "omniasr-wsl-7b",
        "selectedTranscript": "reference candidate",
        "shouldCommit": true
    })
    .to_string();
    db.write_segment_verdict(
        &segment.id,
        "jury_accept",
        Some("reference candidate"),
        Some("multi-reference consensus committed agent text"),
        Some(evidence_json.as_str()),
        Some(0.92),
        false,
    )
    .unwrap();
    for model_id in ["omniasr-wsl-7b", "omniasr-ctc-300m"] {
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: segment.id.clone(),
            model_id: model_id.to_string(),
            transcript: "reference candidate".to_string(),
            confidence: Some(0.95),
        })
        .unwrap();
    }
    segment
}

fn record_ready_agent_report(db: &Database, audio_path: &str, segment_id: &str, run_id: &str) {
    crate::runs::record_agent_import_report_with_options(
        db,
        "file",
        &[audio_path.to_string()],
        &[segment_id.to_string()],
        Some(&serde_json::json!({
            "referenceCommitted": 1,
            "humanInbox": 0,
        })),
        None,
        crate::runs::AgentImportReportOptions {
            agent_run_id: Some(run_id.to_string()),
            agentic_readiness: Some(serde_json::json!({
                "status": "ready",
                "ready": true,
                "sourceReferenceModels": ["gemini-2.5-pro", "gemini-2.5-flash"],
                "availableHypothesisModels": ["omniasr-wsl-7b", "omniasr-ctc-300m"],
                "requiredHypothesisModels": 2,
                "checks": [{
                    "id": "hypothesis_coverage",
                    "label": "Multi-model hypothesis coverage",
                    "status": "ready",
                    "detail": "Two hypothesis models are ready"
                }]
            })),
            ..crate::runs::AgentImportReportOptions::default()
        },
    )
    .unwrap();
}

fn insert_source_reference_with_identity(db: &Database, audio_path: &str, model_id: &str) {
    let identity = crate::pipeline::source_audio_identity(std::path::Path::new(audio_path)).unwrap();
    db.upsert_source_transcript(&SourceTranscriptRecord {
        audio_path: audio_path.to_string(),
        model_id: model_id.to_string(),
        audio_content_hash: Some(identity.content_hash),
        audio_size_bytes: Some(identity.size_bytes),
        transcript_path: format!("source_transcripts\\{model_id}.whole_file_reference.txt"),
        transcript_text: "whole file reference transcript".to_string(),
        created_at: None,
    })
    .unwrap();
}

fn insert_source_reference_without_identity(db: &Database, audio_path: &str, model_id: &str) {
    db.upsert_source_transcript(&SourceTranscriptRecord {
        audio_path: audio_path.to_string(),
        model_id: model_id.to_string(),
        audio_content_hash: None,
        audio_size_bytes: None,
        transcript_path: format!("source_transcripts\\{model_id}.whole_file_reference.txt"),
        transcript_text: "whole file reference transcript".to_string(),
        created_at: None,
    })
    .unwrap();
}

fn insert_stale_source_reference_identity(db: &Database, audio_path: &str, model_id: &str) {
    db.upsert_source_transcript(&SourceTranscriptRecord {
        audio_path: audio_path.to_string(),
        model_id: model_id.to_string(),
        audio_content_hash: Some("stale-audio-hash".to_string()),
        audio_size_bytes: Some(1),
        transcript_path: format!("source_transcripts\\{model_id}.whole_file_reference.txt"),
        transcript_text: "whole file reference transcript".to_string(),
        created_at: None,
    })
    .unwrap();
}

fn all_huggingface_metadata(out_dir: &std::path::Path) -> String {
    let train_metadata = std::fs::read_to_string(out_dir.join("data/train/metadata.csv")).unwrap_or_default();
    let validation_metadata = std::fs::read_to_string(out_dir.join("data/validation/metadata.csv")).unwrap_or_default();
    let test_metadata = std::fs::read_to_string(out_dir.join("data/test/metadata.csv")).unwrap_or_default();
    format!("{train_metadata}\n{validation_metadata}\n{test_metadata}")
}

fn sample_segment(id: &str) -> SpeechSegment {
    SpeechSegment {
        id: id.to_string(),
        created_at: None,
        audio_path: format!("/audio/{id}.wav"),
        raw_transcript: "سڵاو".to_string(),
        normalized_transcript: Some("سلاو".to_string()),
        annotated_transcript: None,
        alignment_json: None,
        duration_ms: 1200,
        speaker_id: Some("speaker_a".to_string()),
        verified: true,
        confidence: None,
        ctc_score: None,
        clipping_ratio: None,
        rms_db: None,
        snr_db: None,
        split: None,
        signal_anomaly_score: None,
        ..SpeechSegment::default()
    }
}

fn sample_metadata() -> DatasetMetadata {
    DatasetMetadata {
        name: "test-dataset".to_string(),
        version: "1.0".to_string(),
        language: "ckb".to_string(),
        script: "Arabic".to_string(),
        total_segments: 1,
        total_duration_ms: 1200,
        verified_segments: 1,
        training_grade_summary: TrainingGradeSummary {
            total_segments: 1,
            training_ready_segments: 1,
            gold_segments: 1,
            silver_segments: 0,
            review_segments: 0,
            rejected_segments: 0,
        },
        composition: DatasetComposition {
            speakers: Vec::new(),
            dominant_speaker_share: 0.0,
            dominant_speaker_over_50pct: false,
        },
        exported_at: "2026-06-16T00:00:00Z".to_string(),
    }
}

fn write_silent_wav(path: &std::path::Path) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for _ in 0..16000 {
        writer.write_sample(0i16).unwrap();
    }
    writer.finalize().unwrap();
}

#[test]
fn assign_splits_reproducible_and_no_recording_leakage() {
    use std::collections::HashMap;
    let mk = |id: &str, src: &str, spk: Option<&str>, dur: i64| SpeechSegment {
        id: id.to_string(),
        audio_path: format!("/data/{src}"),
        speaker_id: spk.map(str::to_string),
        duration_ms: dur,
        ..SpeechSegment::default()
    };
    let mut segs = Vec::new();
    for (src, spk) in [
        ("recA.wav", Some("S1")),
        ("recB.wav", Some("S2")),
        ("recC.wav", None),
        ("recD.wav", Some("S1")), // same speaker as recA — disjoint must keep S1 together
    ] {
        for i in 0..6 {
            segs.push(mk(&format!("{src}-{i}"), src, spk, 5000));
        }
    }

    // 1. Reproducible: same segments + seed → identical assignment, every run.
    let a = assign_splits(&segs, 0.8, 0.1, 0.1, 7, false);
    let b = assign_splits(&segs, 0.8, 0.1, 0.1, 7, false);
    assert_eq!(a, b, "same seed must yield the same split");

    // 2. No source recording leaks across splits (non-disjoint groups by recording).
    let split_of: HashMap<&str, &str> = a.iter().map(|(id, s)| (id.as_str(), *s)).collect();
    let mut rec_split: HashMap<&str, &str> = HashMap::new();
    for s in &segs {
        let src = s.audio_path.rsplit(['/', '\\']).next().unwrap();
        let split = split_of[s.id.as_str()];
        if let Some(prev) = rec_split.insert(src, split) {
            assert_eq!(prev, split, "recording {src} leaked across splits");
        }
    }

    // 3. Speaker-disjoint: a known speaker never spans two splits (S1 is in recA + recD).
    let d = assign_splits(&segs, 0.8, 0.1, 0.1, 7, true);
    let dsplit: HashMap<&str, &str> = d.iter().map(|(id, s)| (id.as_str(), *s)).collect();
    let mut spk_split: HashMap<&str, &str> = HashMap::new();
    for s in &segs {
        if let Some(spk) = s.speaker_id.as_deref() {
            let split = dsplit[s.id.as_str()];
            if let Some(prev) = spk_split.insert(spk, split) {
                assert_eq!(prev, split, "speaker {spk} leaked across splits");
            }
        }
    }
}

#[test]
fn multi_speaker_recording_stays_in_one_split_under_speaker_disjoint() {
    use std::collections::{HashMap, HashSet};
    // Round-10 audit HIGH: in speaker-disjoint mode the OLD grouping keyed purely on speaker, so a
    // single recording diarized into two speakers could land its chunks in DIFFERENT splits — the
    // same room/mic acoustic content leaking train<->test. The connected-components grouping must
    // keep every recording intact AND stay speaker-disjoint.
    let mk = |id: &str, src: &str, spk: &str, dur: i64| SpeechSegment {
        id: id.to_string(),
        audio_path: format!("/data/{src}"),
        speaker_id: Some(spk.to_string()),
        duration_ms: dur,
        ..SpeechSegment::default()
    };
    let mut segs = Vec::new();
    // One recording diarized into TWO speakers (an interview), plus two single-speaker recordings.
    for i in 0..4 {
        segs.push(mk(&format!("interview-A{i}"), "interview.wav", "SPEAKER_00", 5000));
    }
    for i in 0..4 {
        segs.push(mk(&format!("interview-B{i}"), "interview.wav", "SPEAKER_01", 5000));
    }
    for i in 0..6 {
        segs.push(mk(&format!("solo1-{i}"), "solo1.wav", "SPEAKER_02", 5000));
    }
    for i in 0..6 {
        segs.push(mk(&format!("solo2-{i}"), "solo2.wav", "SPEAKER_03", 5000));
    }

    let a = assign_splits(&segs, 0.34, 0.33, 0.33, 11, true);
    let split_of: HashMap<&str, &str> = a.iter().map(|(id, s)| (id.as_str(), *s)).collect();

    // The multi-speaker recording must be entirely within ONE split (no recording leakage).
    let interview_splits: HashSet<&str> =
        segs.iter().filter(|s| s.audio_path.ends_with("interview.wav")).map(|s| split_of[s.id.as_str()]).collect();
    assert_eq!(interview_splits.len(), 1, "a multi-speaker recording must not straddle splits: {interview_splits:?}");

    // And speaker-disjointness still holds: no speaker spans two splits.
    let mut spk_split: HashMap<&str, &str> = HashMap::new();
    for s in &segs {
        let spk = s.speaker_id.as_deref().unwrap();
        let split = split_of[s.id.as_str()];
        if let Some(prev) = spk_split.insert(spk, split) {
            assert_eq!(prev, split, "speaker {spk} leaked across splits");
        }
    }
}

#[test]
fn sha256sums_manifest_covers_files_and_verifies() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"abc").unwrap();
    std::fs::create_dir_all(dir.path().join("data/train")).unwrap();
    std::fs::write(dir.path().join("data/train/clip.wav"), b"hello world").unwrap();
    std::fs::write(dir.path().join("metadata.csv.tmp"), b"staging").unwrap();
    // An audio-clip staging fragment left by a crashed/concurrent run: `<name>.tmp-<pid>-<nonce>`.
    // The old `.ends_with(".tmp")` rule missed this shape, so it would have been hashed in as a real
    // dataset artifact. It must be excluded.
    std::fs::write(dir.path().join("clip.wav.tmp-1234-567890"), b"crash leftover").unwrap();
    // A REAL file that merely contains ".tmp-" in its stem must NOT be excluded (the tail has letters).
    std::fs::write(dir.path().join("foo.tmp-bar.wav"), b"a genuine clip").unwrap();

    write_sha256sums(dir.path()).unwrap();
    let sums = std::fs::read_to_string(dir.path().join("SHA256SUMS")).unwrap();

    // Known vector for sha256("abc").
    assert!(sums.contains("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  a.txt"));
    // Nested file present with a forward-slash relative path.
    assert!(sums.lines().any(|l| l.ends_with("  data/train/clip.wav")));
    // Both staging shapes and the manifest itself are excluded...
    assert!(!sums.contains("metadata.csv.tmp"), ".csv.tmp staging excluded");
    assert!(!sums.contains(".tmp-1234-567890"), ".tmp-<pid>-<nonce> clip staging excluded");
    assert!(!sums.contains("SHA256SUMS"));
    // ...but a genuine file whose name merely contains `.tmp-` is kept.
    assert!(sums.lines().any(|l| l.ends_with("  foo.tmp-bar.wav")), "a real .tmp-named clip stays");
    // Every listed hash matches an independent recompute.
    for line in sums.lines() {
        let (hash, rel) = line.split_once("  ").unwrap();
        let bytes = std::fs::read(dir.path().join(rel)).unwrap();
        assert_eq!(hash, sha256_hex(&bytes), "hash mismatch for {rel}");
    }
}

#[test]
fn export_json_replaces_existing_file() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let path = tmp_dir.path().join("dataset.json");
    std::fs::write(&path, "{\"old\":true}").unwrap();

    export_json(&path, &sample_metadata(), &[sample_segment("json-1")]).unwrap();

    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("json-1"));
    assert!(saved.contains("training_grade_summary"));
    assert!(saved.contains("trainingTranscript"));
    assert!(saved.contains("trainingGrade"));
    assert!(!saved.contains("\"old\""));
    assert!(!path.with_extension("json.tmp").exists());
}

#[test]
fn export_jsonl_replaces_existing_file() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let path = tmp_dir.path().join("dataset.jsonl");
    std::fs::write(&path, "__stale_jsonl_payload__\n").unwrap();

    export_jsonl(&path, &[sample_segment("jsonl-1")]).unwrap();

    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("jsonl-1"));
    assert!(saved.contains("trainingTranscript"));
    assert!(saved.contains("trainingGrade"));
    assert!(!saved.contains("__stale_jsonl_payload__"));
    assert!(!path.with_extension("jsonl.tmp").exists());
}

#[test]
fn export_csv_replaces_existing_file() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let path = tmp_dir.path().join("dataset.csv");
    std::fs::write(&path, "__stale_csv_payload__\n").unwrap();

    export_csv(&path, &[sample_segment("csv-1")]).unwrap();

    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("csv-1"));
    assert!(saved.contains("training_transcript"));
    assert!(saved.contains("training_grade"));
    assert!(!saved.contains("__stale_csv_payload__"));
    assert!(!path.with_extension("csv.tmp").exists());
}

#[test]
fn export_csv_survives_adversarial_transcript_content() {
    // Dataset integrity: a Kurdish transcript containing a comma, double quotes, and a newline
    // must NOT corrupt the CSV — the csv crate quotes/escapes it. This locks that contract so a
    // future hand-rolled writer (which would silently split rows / inject) is caught immediately.
    let tmp_dir = tempfile::tempdir().unwrap();
    let path = tmp_dir.path().join("adv.csv");
    let mut seg = sample_segment("adv-1");
    let nasty = "کوردی، \"دەربڕین\"\nدێڕی نوێ";
    seg.raw_transcript = nasty.to_string();
    export_csv(&path, &[seg]).unwrap();

    let mut rdr = csv::ReaderBuilder::new().has_headers(true).from_path(&path).unwrap();
    let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
    assert_eq!(records.len(), 1, "the embedded comma/newline must not create extra rows");
    assert_eq!(&records[0][0], "adv-1");
    assert_eq!(&records[0][2], nasty, "the transcript round-trips intact through CSV escaping");
}

#[test]
fn csv_safe_cell_quotes_only_formula_leads() {
    // Contract: a leading formula trigger is single-quote-prefixed; everything else is
    // returned untouched (and borrowed, no allocation).
    assert_eq!(csv_safe_cell("=1+1").as_ref(), "'=1+1");
    assert_eq!(csv_safe_cell("+1").as_ref(), "'+1");
    assert_eq!(csv_safe_cell("-1").as_ref(), "'-1");
    assert_eq!(csv_safe_cell("@SUM(A1)").as_ref(), "'@SUM(A1)");
    assert_eq!(csv_safe_cell("\tx").as_ref(), "'\tx");
    assert_eq!(csv_safe_cell("\rx").as_ref(), "'\rx");
    // Normal Sorani text and an embedded (non-leading) '=' are left exactly as-is.
    assert_eq!(csv_safe_cell("کوردی").as_ref(), "کوردی");
    assert_eq!(csv_safe_cell("a=b").as_ref(), "a=b");
    assert_eq!(csv_safe_cell("").as_ref(), "");
}

#[test]
fn export_csv_neutralizes_spreadsheet_formula_injection() {
    // CWE-1236: a transcript/speaker that begins with a formula trigger must be neutralized
    // so it can never execute when the exported dataset CSV is opened in a spreadsheet app.
    let tmp_dir = tempfile::tempdir().unwrap();
    let path = tmp_dir.path().join("inject.csv");
    let mut seg = sample_segment("inj-1");
    seg.raw_transcript = "=HYPERLINK(\"http://evil/\",\"x\")".to_string();
    seg.speaker_id = Some("@SUM(1+1)".to_string());
    export_csv(&path, &[seg]).unwrap();

    let mut rdr = csv::ReaderBuilder::new().has_headers(true).from_path(&path).unwrap();
    let rec = rdr.records().next().unwrap().unwrap();
    // Header order: 0 id, 2 raw_transcript, 6 speaker_id.
    assert!(rec[2].starts_with("'="), "leading = must be quote-prefixed, got {:?}", &rec[2]);
    assert!(rec[6].starts_with("'@"), "leading @ must be quote-prefixed, got {:?}", &rec[6]);
}

#[test]
fn slice_for_export_skips_out_of_range_and_degenerate_windows() {
    // Round-2 audit: a present-but-out-of-range window must SKIP (None), not emit the whole file.
    let full = vec![0i16; 1000]; // ~62ms at 16kHz
                                 // start beyond the (shortened) buffer:
    let beyond = crate::chunking::SegmentSourceMeta {
        source_start_ms: 5000,
        source_end_ms: 6000,
        chunk_index: 0,
        chunk_count: 1,
    };
    assert!(slice_for_export(&full, 16000, Some(&beyond.to_alignment_json())).is_none(), "out-of-range -> skip");
    // degenerate end <= start:
    let degenerate =
        crate::chunking::SegmentSourceMeta { source_start_ms: 30, source_end_ms: 30, chunk_index: 0, chunk_count: 1 };
    assert!(slice_for_export(&full, 16000, Some(&degenerate.to_alignment_json())).is_none(), "degenerate -> skip");
}

#[test]
fn slice_for_export_valid_window_and_whole_file_fallback() {
    let full: Vec<i16> = (0..16000).collect::<Vec<i32>>().iter().map(|&i| i as i16).collect();
    // Valid 0..500ms = 0..8000 samples.
    let valid =
        crate::chunking::SegmentSourceMeta { source_start_ms: 0, source_end_ms: 500, chunk_index: 0, chunk_count: 1 };
    let s = slice_for_export(&full, 16000, Some(&valid.to_alignment_json())).expect("valid window");
    assert_eq!(s.len(), 8000, "valid window slices to exactly its sample span");
    // No alignment -> whole file (intended fallback).
    let whole = slice_for_export(&full, 16000, None).expect("whole file");
    assert_eq!(whole.len(), full.len());
}

#[test]
fn slice_for_export_skips_present_but_offsetless_alignment() {
    // The clobbered shape: alignment_json is a bare word-timestamp array (no source_start_ms), the
    // exact state a broken background aligner leaves. It must SKIP (None), never fall back to the
    // whole file — otherwise a 10s clip's transcript would be paired with the entire recording.
    let full = vec![0i16; 16000];
    let bare_array = r#"[{"word":"x","start":0.0,"end":1.0,"confidence":0.5}]"#;
    assert!(
        slice_for_export(&full, 16000, Some(bare_array)).is_none(),
        "a present-but-offset-less (clobbered) alignment must be skipped, never emitted whole-file"
    );
    // A MERGED object (offsets + words) still slices correctly — the good post-fix shape.
    let meta =
        crate::chunking::SegmentSourceMeta { source_start_ms: 100, source_end_ms: 500, chunk_index: 0, chunk_count: 2 };
    let words = vec![crate::aligner::WordTimestamp { word: "x".into(), start: 0.0, end: 0.4, confidence: 0.5 }];
    let merged = crate::chunking::merge_word_timestamps(Some(&meta.to_alignment_json()), &words);
    let s = slice_for_export(&full, 16000, Some(&merged)).expect("merged alignment still slices");
    assert_eq!(s.len(), chunking::ms_to_samples(400, 16000), "merged object slices to its 100..500ms window");
}

#[test]
fn slice_for_export_skips_offset_beyond_u32_instead_of_wrap_slicing() {
    // A malformed/corrupted alignment blob can carry an i64 offset > u32::MAX. A bare `as u32` would
    // WRAP it (mod 2^32) to a small in-range index and export an UNRELATED window mislabeled with this
    // segment's transcript — silent training-data corruption. It must SKIP, matching the identical
    // guard in chunking::slice_pcm_by_alignment. Without the guard: start_ms 2^32 wraps to 0 and
    // end_ms 2^32+500 wraps to 500, so this would have sliced [0..8000] and returned Some(8000).
    let full = vec![0i16; 16000];
    let wrapping = crate::chunking::SegmentSourceMeta {
        source_start_ms: u32::MAX as i64 + 1, // wraps to 0 under `as u32`
        source_end_ms: u32::MAX as i64 + 501, // wraps to 500 under `as u32`
        chunk_index: 0,
        chunk_count: 1,
    };
    assert!(
        slice_for_export(&full, 16000, Some(&wrapping.to_alignment_json())).is_none(),
        "an offset > u32::MAX must SKIP, never wrap-slice an unrelated window into the export"
    );
}

#[test]
fn compute_composition_flags_a_dominant_speaker() {
    let mk = |id: &str, spk: &str, dur: i64| SpeechSegment {
        id: id.into(),
        speaker_id: Some(spk.into()),
        duration_ms: dur,
        ..Default::default()
    };
    // Speaker A = 8000ms, B = 2000ms -> A dominates (80% of duration > 50%).
    let c = compute_composition(&[mk("1", "A", 5000), mk("2", "A", 3000), mk("3", "B", 2000)]);
    assert_eq!(c.speakers.len(), 2);
    assert_eq!(c.speakers[0].speaker_id, "A", "speakers sorted by duration desc");
    assert_eq!((c.speakers[0].segments, c.speakers[0].duration_ms), (2, 8000));
    assert!((c.dominant_speaker_share - 0.8).abs() < 1e-9);
    assert!(c.dominant_speaker_over_50pct, "one speaker over 50% is flagged");
    // A balanced corpus is not flagged.
    assert!(!compute_composition(&[mk("1", "A", 5000), mk("2", "B", 5000)]).dominant_speaker_over_50pct);
}

#[test]
fn hf_export_persists_splits_only_after_a_successful_write() {
    // Round-2 audit MEDIUM: splits were committed to the DB BEFORE files were written. They must
    // now be persisted last — present after a successful export, absent before.
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let wav_path = tmp_dir.path().join("hf-split.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
    for _ in 0..16000 {
        writer.write_sample(0i16).unwrap();
    }
    writer.finalize().unwrap();

    let mut seg = sample_segment("hf-split-1");
    seg.audio_path = wav_path.to_string_lossy().to_string();
    db.insert_segment(&seg).unwrap();
    assert!(db.get_segment_by_id("hf-split-1").unwrap().unwrap().split.is_none(), "split unset before export");

    let out_dir = tempfile::tempdir().unwrap();
    export_huggingface_dataset(&db, out_dir.path(), &crate::settings::AppSettings::default()).unwrap();

    let split = db.get_segment_by_id("hf-split-1").unwrap().unwrap().split;
    assert!(
        matches!(split.as_deref(), Some("train" | "validation" | "test")),
        "split persisted after a successful export, got {split:?}"
    );
}

#[test]
fn hf_export_excludes_holdout_gold_audio_from_training() {
    // Round-3 audit HIGH: a clip registered as a holdout (for WER/CER eval) that also exists as a
    // training-ready segment leaked into data/train, contaminating the eval set. It must be
    // excluded, while a non-holdout segment is still exported.
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    let tmp_dir = tempfile::tempdir().unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let write_wav = |name: &str, val: i16| {
        let p = tmp_dir.path().join(name);
        let mut w = hound::WavWriter::create(&p, spec).unwrap();
        for _ in 0..16000 {
            w.write_sample(val).unwrap();
        }
        w.finalize().unwrap();
        p.to_string_lossy().to_string()
    };
    // Distinct audio content -> distinct content hashes.
    let holdout_path = write_wav("holdout.wav", 0);
    let keep_path = write_wav("keep.wav", 1000);

    let mut hseg = sample_segment("hold-1");
    hseg.audio_path = holdout_path.clone();
    db.insert_segment(&hseg).unwrap();
    let mut kseg = sample_segment("keep-1");
    kseg.audio_path = keep_path;
    db.insert_segment(&kseg).unwrap();

    // Register the holdout clip's source as a holdout gold reference.
    crate::eval::import_gold_segments(
        &db,
        vec![crate::eval::GoldSegmentInput { audio_path: holdout_path, reference: "ref".into(), is_holdout: true }],
    )
    .unwrap();

    let out_dir = tempfile::tempdir().unwrap();
    export_huggingface_dataset(&db, out_dir.path(), &crate::settings::AppSettings::default()).unwrap();

    let all_csv: String = ["train", "validation", "test"]
        .iter()
        .map(|s| std::fs::read_to_string(out_dir.path().join("data").join(s).join("metadata.csv")).unwrap_or_default())
        .collect();
    assert!(all_csv.contains("keep-1"), "the non-holdout segment must still be exported");
    assert!(!all_csv.contains("hold-1"), "the holdout gold clip must NOT leak into any split");
}

#[test]
fn dataset_export_excludes_holdout_gold_audio_from_training_tables() {
    // The bundle/tabular export (JSON/JSONL/CSV/Parquet) must apply the SAME holdout exclusion as the
    // HF export — otherwise a clip registered as an eval holdout that also exists as a normal segment
    // leaks its reference transcript into the training tables.
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    let tmp_dir = tempfile::tempdir().unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let write_wav = |name: &str, val: i16| {
        let p = tmp_dir.path().join(name);
        let mut w = hound::WavWriter::create(&p, spec).unwrap();
        for _ in 0..16000 {
            w.write_sample(val).unwrap();
        }
        w.finalize().unwrap();
        p.to_string_lossy().to_string()
    };
    let holdout_path = write_wav("h.wav", 0);
    let keep_path = write_wav("k.wav", 1000);

    let mut hseg = sample_segment("hold-x");
    hseg.audio_path = holdout_path.clone();
    hseg.raw_transcript = "SECRETHOLDOUTREF".into();
    db.insert_segment(&hseg).unwrap();
    let mut kseg = sample_segment("keep-x");
    kseg.audio_path = keep_path;
    kseg.raw_transcript = "KEPTTRAININGTEXT".into();
    db.insert_segment(&kseg).unwrap();

    crate::eval::import_gold_segments(
        &db,
        vec![crate::eval::GoldSegmentInput { audio_path: holdout_path, reference: "ref".into(), is_holdout: true }],
    )
    .unwrap();

    let out = NamedTempFile::new().unwrap();
    let out_path = out.path().with_extension("json");
    export_dataset(&db, &out_path, &ExportFormat::Json).unwrap();
    let body = std::fs::read_to_string(&out_path).unwrap();
    assert!(body.contains("KEPTTRAININGTEXT"), "the non-holdout segment must still be exported");
    assert!(!body.contains("SECRETHOLDOUTREF"), "the holdout gold clip must NOT leak into the dataset export");
    assert!(!body.contains("hold-x"), "the holdout segment id must NOT appear in the export");
}

#[test]
fn plain_export_excludes_holdout_gold_audio() {
    // Round-22 #1: the plain JSON/JSONL/CSV/Parquet export (and the production bundle that wraps
    // it) must apply the SAME holdout exclusion as the HF export, or a clip registered as a
    // holdout WER/CER reference leaks into the published training set and contaminates the gate.
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    let tmp_dir = tempfile::tempdir().unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let write_wav = |name: &str, val: i16| {
        let p = tmp_dir.path().join(name);
        let mut w = hound::WavWriter::create(&p, spec).unwrap();
        for _ in 0..16000 {
            w.write_sample(val).unwrap();
        }
        w.finalize().unwrap();
        p.to_string_lossy().to_string()
    };
    let holdout_path = write_wav("holdout.wav", 0);
    let keep_path = write_wav("keep.wav", 1000);
    let mut hseg = sample_segment("hold-1");
    hseg.audio_path = holdout_path.clone();
    db.insert_segment(&hseg).unwrap();
    let mut kseg = sample_segment("keep-1");
    kseg.audio_path = keep_path;
    db.insert_segment(&kseg).unwrap();
    crate::eval::import_gold_segments(
        &db,
        vec![crate::eval::GoldSegmentInput { audio_path: holdout_path, reference: "ref".into(), is_holdout: true }],
    )
    .unwrap();

    let out = tmp_dir.path().join("dataset.csv");
    export_dataset(&db, &out, &ExportFormat::Csv).unwrap();
    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.contains("keep-1"), "the non-holdout segment must still be exported");
    assert!(!body.contains("hold-1"), "the holdout gold clip must NOT leak into the plain export");
}

#[test]
fn plain_export_excludes_human_rejected_segments() {
    // A human-REJECTED clip ("mark bad" in review) is KEPT in the library but must never appear in a
    // plain JSON/JSONL/CSV/Parquet export, nor be counted as verified — it carries verified=true only
    // to leave the review queue. Mirrors the human_reject exclusion the HF/training path already does.
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    db.insert_segment(&sample_segment("keep-1")).unwrap();
    db.insert_segment(&sample_segment("bad-1")).unwrap();
    // Reviewer marks it bad: a human 'reject' decision (verdict=human_reject) while verified stays true.
    db.record_human_decision("bad-1", "reject", None, None).unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let out = tmp_dir.path().join("dataset.json");
    export_dataset(&db, &out, &ExportFormat::Json).unwrap();
    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.contains("keep-1"), "the confirmed-good segment must still be exported");
    assert!(!body.contains("bad-1"), "a human-rejected clip must NOT appear in the plain export");

    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        parsed["metadata"]["verified_segments"].as_u64(),
        Some(1),
        "a human-rejected clip must not be counted as verified"
    );
    assert_eq!(
        parsed["metadata"]["total_segments"].as_u64(),
        Some(1),
        "a human-rejected clip must not be counted in the exported total"
    );
}

#[test]
fn export_parquet_writes_valid_file() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    let mut seg = sample_segment("pq-1");
    // Approximate (energy-heuristic) per-word timing — the marker Parquet must ship (audit P1 #8).
    seg.alignment_quality = Some("energy_heuristic".to_string());
    db.insert_segment(&seg).unwrap();

    let out_tmp = NamedTempFile::new().unwrap();
    let out_path = out_tmp.path().with_extension("parquet");
    export_dataset(&db, &out_path, &ExportFormat::Parquet).unwrap();

    assert!(out_path.exists());
    assert!(out_path.metadata().unwrap().len() > 0);

    let file = std::fs::File::open(&out_path).unwrap();
    let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file).unwrap().build().unwrap();
    let batches: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_rows(), 1);
    assert!(batches[0].schema().field_with_name("training_transcript").is_ok());
    assert!(batches[0].schema().field_with_name("training_grade").is_ok());
    assert!(batches[0].schema().field_with_name("training_ready").is_ok());
    // Audit P1 #8: the timing-precision marker ships alongside alignment_json (was silently dropped).
    assert!(
        batches[0].schema().field_with_name("alignment_quality").is_ok(),
        "Parquet must carry alignment_quality like the JSON/JSONL formats (CSV ships no alignment fields)"
    );
    use arrow_array::Array;
    let col = batches[0].column_by_name("alignment_quality").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
    assert_eq!(col.value(0), "energy_heuristic", "the approximate-timing marker must round-trip");
}

#[test]
fn export_parquet_replaces_existing_file() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();
    db.insert_segment(&sample_segment("pq-replace")).unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let out_path = tmp_dir.path().join("dataset.parquet");
    std::fs::write(&out_path, "__stale_parquet_payload__").unwrap();

    export_dataset(&db, &out_path, &ExportFormat::Parquet).unwrap();

    assert!(out_path.exists());
    assert!(out_path.metadata().unwrap().len() > "__stale_parquet_payload__".len() as u64);
    assert!(!out_path.with_extension("parquet.tmp").exists());
}

#[test]
fn exports_never_leak_absolute_paths() {
    // A curator's absolute path embeds their OS username + drive layout; publishing it into a
    // shared CSV/JSONL/JSON/Parquet is a real PII leak. Every exporter must emit only the audio
    // basename. Fixture uses a SYNTHETIC absolute path (no real home dir — keeps the repo's
    // private-path hygiene gate green) standing in for that username + drive layout.
    let tmp_dir = tempfile::tempdir().unwrap();
    let d = tmp_dir.path();
    let mut seg = sample_segment("p1");
    seg.audio_path = "C:\\SynthHome\\synth_user\\private_recordings\\clip_001.wav".to_string();
    let segs = [seg];

    export_json(&d.join("o.json"), &sample_metadata(), &segs).unwrap();
    export_jsonl(&d.join("o.jsonl"), &segs).unwrap();
    export_csv(&d.join("o.csv"), &segs).unwrap();
    export_parquet(&d.join("o.parquet"), &segs).unwrap();

    for name in ["o.json", "o.jsonl", "o.csv"] {
        let body = std::fs::read_to_string(d.join(name)).unwrap();
        assert!(body.contains("clip_001.wav"), "{name} should keep the basename");
        assert!(!body.contains("synth_user"), "{name} leaked the OS username: {body}");
        assert!(!body.contains("SynthHome"), "{name} leaked an absolute path");
        assert!(!body.contains("private_recordings"), "{name} leaked a directory");
    }
    // Parquet is binary — scan the raw bytes for the same leaked substrings.
    let pq = std::fs::read(d.join("o.parquet")).unwrap();
    let has = |needle: &str| pq.windows(needle.len()).any(|w| w == needle.as_bytes());
    assert!(has("clip_001.wav"), "parquet should keep the basename");
    assert!(!has("synth_user"), "parquet leaked the OS username");
    assert!(!has("private_recordings"), "parquet leaked a directory");
}

#[test]
fn export_writers_error_cleanly_on_unwritable_destination() {
    // Use an existing FILE as the would-be parent directory: writing the temp file
    // underneath it must fail with a clean Err — never panic, never half-write.
    let tmp_dir = tempfile::tempdir().unwrap();
    let blocker = tmp_dir.path().join("blocker");
    std::fs::write(&blocker, b"i am a file, not a directory").unwrap();
    let seg = [sample_segment("x")];

    assert!(export_json(&blocker.join("d.json"), &sample_metadata(), &seg).is_err());
    assert!(export_jsonl(&blocker.join("d.jsonl"), &seg).is_err());
    assert!(export_csv(&blocker.join("d.csv"), &seg).is_err());
    assert!(export_parquet(&blocker.join("d.parquet"), &seg).is_err());

    // The blocker is untouched and no stray artifact was created.
    assert_eq!(std::fs::read_to_string(&blocker).unwrap(), "i am a file, not a directory");
}

#[test]
fn export_writers_handle_empty_dataset() {
    // Exporting a dataset with zero segments must produce well-formed, non-panicking
    // output (a fresh install or a fully-filtered export hits this).
    let tmp_dir = tempfile::tempdir().unwrap();
    let d = tmp_dir.path();
    let empty: [SpeechSegment; 0] = [];

    export_json(&d.join("e.json"), &sample_metadata(), &empty).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.join("e.json")).unwrap()).unwrap();
    assert!(parsed.get("segments").is_some_and(|s| s.as_array().is_some_and(|a| a.is_empty())));

    export_jsonl(&d.join("e.jsonl"), &empty).unwrap();
    assert_eq!(std::fs::read_to_string(d.join("e.jsonl")).unwrap(), "");

    export_csv(&d.join("e.csv"), &empty).unwrap();
    assert!(d.join("e.csv").exists());

    export_parquet(&d.join("e.parquet"), &empty).unwrap();
    assert!(d.join("e.parquet").exists());
}

#[test]
fn export_huggingface_writes_dataset_files() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let wav_path = tmp_dir.path().join("hf-1.wav");

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
    for _ in 0..16000 {
        writer.write_sample(0i16).unwrap();
    }
    writer.finalize().unwrap();

    let mut seg = sample_segment("hf-1");
    seg.audio_path = wav_path.to_string_lossy().to_string();
    db.insert_segment(&seg).unwrap();

    let out_dir = tempfile::tempdir().unwrap();
    let settings = crate::settings::AppSettings::default();
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    assert!(out_dir.path().join("data/train/metadata.csv").exists());
    assert!(out_dir.path().join("README.md").exists());
    assert!(out_dir.path().join("dataset_infos.json").exists());
    let metadata = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
    assert!(metadata.contains("training_grade"));
    assert!(metadata.contains("gold"));

    // The real export emits a correct integrity manifest covering every artifact.
    let sums = std::fs::read_to_string(out_dir.path().join("SHA256SUMS")).unwrap();
    assert!(sums.lines().any(|l| l.ends_with("  README.md")));
    for line in sums.lines() {
        let (hash, rel) = line.split_once("  ").unwrap();
        assert_eq!(hash, sha256_hex(&std::fs::read(out_dir.path().join(rel)).unwrap()));
    }
}

#[test]
fn export_huggingface_counts_dropped_missing_audio() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    // A segment whose source audio simply does not exist on disk.
    let mut seg = sample_segment("missing-1");
    seg.audio_path = "/nonexistent/does_not_exist.wav".to_string();
    db.insert_segment(&seg).unwrap();

    let out_dir = tempfile::tempdir().unwrap();
    let settings = crate::settings::AppSettings::default();
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    // The drop is surfaced (no longer silent) in dataset_infos.json, and the segment
    // is absent from the exported metadata.
    let info = std::fs::read_to_string(out_dir.path().join("dataset_infos.json")).unwrap();
    assert!(info.contains("\"droppedUnavailableAudio\": 1"), "info: {info}");
    let train_meta = out_dir.path().join("data/train/metadata.csv");
    if train_meta.exists() {
        assert!(!std::fs::read_to_string(train_meta).unwrap().contains("missing-1"));
    }
}

#[test]
fn export_huggingface_skips_rows_not_ready_for_training() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let wav_path = tmp_dir.path().join("hf-filter.wav");

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
    for _ in 0..16000 {
        writer.write_sample(0i16).unwrap();
    }
    writer.finalize().unwrap();

    let mut ready = sample_segment("hf-ready");
    ready.audio_path = wav_path.to_string_lossy().to_string();
    db.insert_segment(&ready).unwrap();

    let mut reject = sample_segment("hf-reject");
    reject.audio_path = wav_path.to_string_lossy().to_string();
    reject.raw_transcript.clear();
    reject.normalized_transcript = None;
    reject.annotated_transcript = None;
    reject.verified = false;
    db.insert_segment(&reject).unwrap();

    let out_dir = tempfile::tempdir().unwrap();
    let settings =
        crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    let train_metadata = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
    let validation_metadata =
        std::fs::read_to_string(out_dir.path().join("data/validation/metadata.csv")).unwrap_or_default();
    let test_metadata = std::fs::read_to_string(out_dir.path().join("data/test/metadata.csv")).unwrap_or_default();
    let all_metadata = format!("{train_metadata}\n{validation_metadata}\n{test_metadata}");

    assert!(all_metadata.contains("hf-ready"));
    assert!(!all_metadata.contains("hf-reject"));
}

#[test]
fn hf_metadata_rows_are_ordered_deterministically_by_source_path() {
    // Round-15: metadata.csv rows were emitted in HashMap (per-process-random) iteration order, so
    // two identical exports produced different bytes and different SHA256SUMS, breaking the
    // byte-reproducibility the manifest promises. With a BTreeMap the per-source row blocks are
    // written in sorted source-path order — deterministic. Insert the sources OUT of sorted order
    // and assert the rows still come out sorted, proving the ordering is by source path (not
    // insertion/segment order) and is therefore stable across runs.
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let wav_a = tmp_dir.path().join("a_source.wav");
    let wav_z = tmp_dir.path().join("z_source.wav");
    write_silent_wav(&wav_a);
    write_silent_wav(&wav_z);

    // Insert the z-source segment FIRST (non-sorted insertion order).
    let mut sz = sample_segment("seg-from-z");
    sz.audio_path = wav_z.to_string_lossy().to_string();
    db.insert_segment(&sz).unwrap();
    let mut sa = sample_segment("seg-from-a");
    sa.audio_path = wav_a.to_string_lossy().to_string();
    db.insert_segment(&sa).unwrap();

    let out_dir = tempfile::tempdir().unwrap();
    let settings = crate::settings::AppSettings {
        hf_speaker_disjoint: false,
        hf_train_ratio: 1.0,
        hf_val_ratio: 0.0,
        hf_test_ratio: 0.0,
        ..crate::settings::AppSettings::default()
    };
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    let meta = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
    let a_idx = meta.find("a_source_seg-from-a.wav").expect("a_source row present");
    let z_idx = meta.find("z_source_seg-from-z.wav").expect("z_source row present");
    assert!(
        a_idx < z_idx,
        "metadata.csv rows must be sorted by source path (a_source before z_source) so the file \
         is byte-reproducible:\n{meta}"
    );
}

#[test]
fn export_huggingface_skips_machine_ready_rows_without_hypothesis_coverage() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let wav_path = tmp_dir.path().join("hf-weak-machine.wav");
    write_silent_wav(&wav_path);

    let mut weak = sample_segment("hf-weak-machine");
    weak.audio_path = wav_path.to_string_lossy().to_string();
    weak.verified = false;
    weak.annotated_transcript = None;
    weak.confidence = Some(0.95);
    weak.clipping_ratio = Some(0.0);
    weak.rms_db = Some(-20.0);
    weak.snr_db = Some(20.0);
    db.insert_segment(&weak).unwrap();
    let evidence_json = serde_json::json!({
        "referenceModelId": "multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash",
        "selectedModelId": "omniasr-wsl-7b",
        "shouldCommit": true
    })
    .to_string();
    db.write_segment_verdict(
        &weak.id,
        "jury_accept",
        Some("reference candidate"),
        Some("legacy source-reference commit"),
        Some(evidence_json.as_str()),
        Some(0.92),
        false,
    )
    .unwrap();
    db.insert_hypothesis(&SegmentHypothesis {
        segment_id: weak.id.clone(),
        model_id: "omniasr-wsl-7b".to_string(),
        transcript: "reference candidate".to_string(),
        confidence: Some(0.95),
    })
    .unwrap();

    let out_dir = tempfile::tempdir().unwrap();
    let settings =
        crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    let train_metadata = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
    let validation_metadata =
        std::fs::read_to_string(out_dir.path().join("data/validation/metadata.csv")).unwrap_or_default();
    let test_metadata = std::fs::read_to_string(out_dir.path().join("data/test/metadata.csv")).unwrap_or_default();
    let all_metadata = format!("{train_metadata}\n{validation_metadata}\n{test_metadata}");

    assert!(!all_metadata.contains("hf-weak-machine"));
    assert!(!out_dir.path().join("data/train/hf-weak-machine_hf-weak-machine.wav").exists());
}

#[test]
fn export_huggingface_skips_machine_ready_rows_without_ready_agentic_report() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let wav_path = tmp_dir.path().join("hf-no-agent-report.wav");
    write_silent_wav(&wav_path);
    insert_machine_silver_segment_with_hf_coverage(&db, &wav_path, "hf-no-agent-report");

    let out_dir = tempfile::tempdir().unwrap();
    let settings =
        crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    let train_metadata = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
    let validation_metadata =
        std::fs::read_to_string(out_dir.path().join("data/validation/metadata.csv")).unwrap_or_default();
    let test_metadata = std::fs::read_to_string(out_dir.path().join("data/test/metadata.csv")).unwrap_or_default();
    let all_metadata = format!("{train_metadata}\n{validation_metadata}\n{test_metadata}");

    assert!(!all_metadata.contains("hf-no-agent-report"));
    assert!(!out_dir.path().join("data/train/hf-no-agent-report_hf-no-agent-report.wav").exists());
}

#[test]
fn export_huggingface_skips_machine_ready_rows_not_covered_by_latest_ready_agentic_report() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let covered_wav = tmp_dir.path().join("hf-covered-agent-report.wav");
    let uncovered_wav = tmp_dir.path().join("hf-uncovered-agent-report.wav");
    write_silent_wav(&covered_wav);
    write_silent_wav(&uncovered_wav);
    let covered = insert_machine_silver_segment_with_hf_coverage(&db, &covered_wav, "hf-covered-agent-report");
    insert_machine_silver_segment_with_hf_coverage(&db, &uncovered_wav, "hf-uncovered-agent-report");
    insert_source_reference_with_identity(&db, covered.audio_path.as_str(), "gemini-2.5-pro");
    insert_source_reference_with_identity(&db, covered.audio_path.as_str(), "gemini-2.5-flash");
    record_ready_agent_report(&db, covered.audio_path.as_str(), covered.id.as_str(), "run-hf-covered-agent-report");

    let out_dir = tempfile::tempdir().unwrap();
    let settings =
        crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    let train_metadata = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
    let validation_metadata =
        std::fs::read_to_string(out_dir.path().join("data/validation/metadata.csv")).unwrap_or_default();
    let test_metadata = std::fs::read_to_string(out_dir.path().join("data/test/metadata.csv")).unwrap_or_default();
    let all_metadata = format!("{train_metadata}\n{validation_metadata}\n{test_metadata}");

    assert!(all_metadata.contains("hf-covered-agent-report"));
    assert!(!all_metadata.contains("hf-uncovered-agent-report"));
}

#[test]
fn export_huggingface_skips_machine_ready_rows_without_current_source_reference_identity() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let wav_path = tmp_dir.path().join("hf-legacy-source-reference.wav");
    write_silent_wav(&wav_path);
    let segment = insert_machine_silver_segment_with_hf_coverage(&db, &wav_path, "hf-legacy-source-reference");
    insert_source_reference_without_identity(&db, segment.audio_path.as_str(), "gemini-2.5-pro");
    insert_source_reference_without_identity(&db, segment.audio_path.as_str(), "gemini-2.5-flash");
    record_ready_agent_report(&db, segment.audio_path.as_str(), segment.id.as_str(), "run-hf-legacy-source-reference");

    let out_dir = tempfile::tempdir().unwrap();
    let settings =
        crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    let all_metadata = all_huggingface_metadata(out_dir.path());

    assert!(!all_metadata.contains("hf-legacy-source-reference"));
    assert!(!out_dir.path().join("data/train/hf-legacy-source-reference_hf-legacy-source-reference.wav").exists());
}

#[test]
fn export_huggingface_skips_machine_ready_rows_missing_configured_source_reference_model() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let wav_path = tmp_dir.path().join("hf-missing-source-reference-model.wav");
    write_silent_wav(&wav_path);
    let segment = insert_machine_silver_segment_with_hf_coverage(&db, &wav_path, "hf-missing-source-reference-model");
    insert_source_reference_with_identity(&db, segment.audio_path.as_str(), "gemini-2.5-pro");
    record_ready_agent_report(
        &db,
        segment.audio_path.as_str(),
        segment.id.as_str(),
        "run-hf-missing-source-reference-model",
    );

    let out_dir = tempfile::tempdir().unwrap();
    let settings =
        crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    let all_metadata = all_huggingface_metadata(out_dir.path());

    assert!(!all_metadata.contains("hf-missing-source-reference-model"));
    assert!(!out_dir
        .path()
        .join("data/train/hf-missing-source-reference-model_hf-missing-source-reference-model.wav")
        .exists());
}

#[test]
fn export_huggingface_skips_machine_ready_rows_with_stale_source_reference_identity() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let wav_path = tmp_dir.path().join("hf-stale-source-reference.wav");
    write_silent_wav(&wav_path);
    let segment = insert_machine_silver_segment_with_hf_coverage(&db, &wav_path, "hf-stale-source-reference");
    insert_stale_source_reference_identity(&db, segment.audio_path.as_str(), "gemini-2.5-pro");
    insert_stale_source_reference_identity(&db, segment.audio_path.as_str(), "gemini-2.5-flash");
    record_ready_agent_report(&db, segment.audio_path.as_str(), segment.id.as_str(), "run-hf-stale-source-reference");

    let out_dir = tempfile::tempdir().unwrap();
    let settings =
        crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    let all_metadata = all_huggingface_metadata(out_dir.path());

    assert!(!all_metadata.contains("hf-stale-source-reference"));
    assert!(!out_dir.path().join("data/train/hf-stale-source-reference_hf-stale-source-reference.wav").exists());
}

#[test]
fn export_huggingface_writes_machine_ready_rows_with_matching_ready_agentic_report() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let wav_path = tmp_dir.path().join("hf-ready-agent-report.wav");
    write_silent_wav(&wav_path);
    let segment = insert_machine_silver_segment_with_hf_coverage(&db, &wav_path, "hf-ready-agent-report");
    insert_source_reference_with_identity(&db, segment.audio_path.as_str(), "gemini-2.5-pro");
    insert_source_reference_with_identity(&db, segment.audio_path.as_str(), "gemini-2.5-flash");
    record_ready_agent_report(&db, segment.audio_path.as_str(), segment.id.as_str(), "run-hf-ready-agent-report");

    let out_dir = tempfile::tempdir().unwrap();
    let settings =
        crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    let train_metadata = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
    let validation_metadata =
        std::fs::read_to_string(out_dir.path().join("data/validation/metadata.csv")).unwrap_or_default();
    let test_metadata = std::fs::read_to_string(out_dir.path().join("data/test/metadata.csv")).unwrap_or_default();
    let all_metadata = format!("{train_metadata}\n{validation_metadata}\n{test_metadata}");

    assert!(all_metadata.contains("hf-ready-agent-report"));
    assert!(all_metadata.contains("silver"));
    assert!(all_metadata.contains("jury_verdict"));
}

#[test]
fn export_huggingface_replaces_metadata_files() {
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let wav_path = tmp_dir.path().join("hf-atomic.wav");

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
    for _ in 0..16000 {
        writer.write_sample(0i16).unwrap();
    }
    writer.finalize().unwrap();

    let mut seg = sample_segment("hf-atomic");
    seg.audio_path = wav_path.to_string_lossy().to_string();
    db.insert_segment(&seg).unwrap();

    let out_dir = tempfile::tempdir().unwrap();
    let train_dir = out_dir.path().join("data/train");
    std::fs::create_dir_all(&train_dir).unwrap();
    std::fs::write(train_dir.join("metadata.csv"), "__stale_split_metadata__").unwrap();
    let stale_wav_path = train_dir.join("hf-atomic_hf-atomic.wav");
    std::fs::write(&stale_wav_path, "__stale_wav_payload__").unwrap();
    // Orphan from a prior, larger export: a clip whose name this export does NOT produce.
    let orphan_wav_path = train_dir.join("old-recording_orphan-seg.wav");
    std::fs::write(&orphan_wav_path, "__orphan_wav_payload__").unwrap();
    std::fs::write(out_dir.path().join("README.md"), "__stale_readme__").unwrap();
    std::fs::write(out_dir.path().join("dataset_infos.json"), "__stale_info__").unwrap();
    let settings = crate::settings::AppSettings::default();

    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    let readme = std::fs::read_to_string(out_dir.path().join("README.md")).unwrap();
    let info = std::fs::read_to_string(out_dir.path().join("dataset_infos.json")).unwrap();
    let split_metadata = std::fs::read_to_string(train_dir.join("metadata.csv")).unwrap();
    assert!(!readme.contains("__stale_readme__"));
    assert!(!info.contains("__stale_info__"));
    assert!(!split_metadata.contains("__stale_split_metadata__"));
    assert!(split_metadata.contains("hf-atomic_hf-atomic.wav"));
    assert_ne!(std::fs::read(&stale_wav_path).unwrap(), b"__stale_wav_payload__");
    assert_eq!(hound::WavReader::open(&stale_wav_path).unwrap().spec().sample_rate, 16000);
    assert!(!out_dir.path().join("README.md.tmp").exists());
    assert!(!out_dir.path().join("dataset_infos.json.tmp").exists());
    assert!(!train_dir.join("metadata.csv.tmp").exists());
    assert_no_tmp_exports(&train_dir);
}

#[test]
fn hf_reexport_removes_orphan_wav_for_a_dropped_segment() {
    // Round-12 audit (#5/#6): a re-export must remove the WAV of a segment that no longer exports
    // (so it isn't left orphaned and hashed into SHA256SUMS with no metadata row), while keeping the
    // still-exporting segments and a metadata.csv for every split. An EMPTY re-export is a separate
    // no-op that preserves the prior export, so this scenario keeps one segment exporting.
    let db_tmp = NamedTempFile::new().unwrap();
    let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
    db.initialize().unwrap();

    let tmp_dir = tempfile::tempdir().unwrap();
    let wav_path = tmp_dir.path().join("clip.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
    for _ in 0..16000 {
        writer.write_sample(0i16).unwrap();
    }
    writer.finalize().unwrap();

    for id in ["orphan-seg", "keep-seg"] {
        let mut seg = sample_segment(id);
        seg.audio_path = wav_path.to_string_lossy().to_string();
        db.insert_segment(&seg).unwrap();
    }

    let out_dir = tempfile::tempdir().unwrap();
    let settings = crate::settings::AppSettings::default();
    let data_dir = out_dir.path().join("data");

    let find_wavs = |root: &std::path::Path| -> Vec<std::path::PathBuf> {
        ["train", "validation", "test"]
            .iter()
            .flat_map(|s| std::fs::read_dir(root.join("data").join(s)).ok().into_iter().flatten().flatten())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "wav").unwrap_or(false))
            .collect()
    };

    // Run 1: both segments export.
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();
    assert_eq!(find_wavs(out_dir.path()).len(), 2, "run 1 exports two wavs");

    // One segment no longer exports; re-export into the SAME directory.
    db.delete_segment("orphan-seg").unwrap();
    export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

    let after = find_wavs(out_dir.path());
    assert_eq!(after.len(), 1, "only the kept segment's wav remains, got {after:?}");
    assert!(after[0].to_string_lossy().contains("keep-seg"), "kept wav is keep-seg: {after:?}");

    let sums = std::fs::read_to_string(out_dir.path().join("SHA256SUMS")).unwrap();
    assert!(!sums.contains("orphan-seg"), "SHA256SUMS must not list the orphan WAV:\n{sums}");
    assert!(sums.contains("keep-seg"), "SHA256SUMS must list the kept WAV:\n{sums}");

    // Every declared split still has a metadata.csv (header-only for the empty ones).
    for s in ["train", "validation", "test"] {
        assert!(data_dir.join(s).join("metadata.csv").exists(), "split {s} must have a metadata.csv");
    }
}

fn assert_no_tmp_exports(dir: &std::path::Path) {
    let tmp_left =
        std::fs::read_dir(dir).unwrap().flatten().any(|entry| entry.file_name().to_string_lossy().contains(".tmp-"));
    assert!(!tmp_left, "temporary export files should be promoted or removed");
}
