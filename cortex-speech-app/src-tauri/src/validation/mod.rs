pub mod input;

use crate::db::Database;
use crate::error::AppResult;
use crate::quality;
use crate::settings::AppSettings;
use crate::wer;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReport {
    pub total_segments: usize,
    pub total_audio_files: usize,
    pub passed: usize,
    pub warnings: Vec<ValidationIssue>,
    pub errors: Vec<ValidationIssue>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ValidationIssue {
    pub severity: IssueSeverity,
    pub category: IssueCategory,
    pub segment_id: Option<String>,
    pub field: String,
    pub message: String,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type, PartialEq)]
pub enum IssueSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type, PartialEq)]
pub enum IssueCategory {
    MissingAudio,
    EmptyTranscript,
    DuplicateFingerprint,
    DurationMismatch,
    InvalidSpeaker,
    CorruptAudio,
    AnnotationIncomplete,
    HighWer,
    HighCer,
    QualityGateFailed,
    ClippingDetected,
    LowRmsVolume,
    AlignmentHeuristic,
    Other,
}

pub fn validate_dataset(db: &Database) -> AppResult<ValidationReport> {
    validate_dataset_with_settings(db, &AppSettings::default())
}

pub fn validate_dataset_with_settings(db: &Database, settings: &AppSettings) -> AppResult<ValidationReport> {
    let segments = db.get_segments(None)?;
    validate_segments_with_settings(&segments, settings)
}

/// Validate an immutable library snapshot. Production bundle export uses this entry point so the
/// validation report and every shipped row describe one database moment even if another process
/// inserts or edits rows while the files are being generated.
pub(crate) fn validate_segments_with_settings(
    segments: &[crate::db::SpeechSegment],
    settings: &AppSettings,
) -> AppResult<ValidationReport> {
    let mut issues = Vec::new();

    // 1. Check audio file existence
    let audio_paths: HashSet<&str> = segments.iter().map(|s| s.audio_path.as_str()).collect();
    for path in &audio_paths {
        if !Path::new(path).exists() {
            let seg_ids: Vec<&str> = segments.iter().filter(|s| s.audio_path == *path).map(|s| s.id.as_str()).collect();
            issues.push(ValidationIssue {
                severity: IssueSeverity::Error,
                category: IssueCategory::MissingAudio,
                segment_id: seg_ids.first().map(|s| s.to_string()),
                field: "audio_path".to_string(),
                message: format!("Audio file not found: {path}"),
                details: Some(format!("Referenced by {} segment(s)", seg_ids.len())),
            });
        }
    }

    // 2. Check empty transcripts. Flag only when the authoritative Verbatim-Law transcript is empty
    // (mirroring quality.rs::effective_transcript) — a clip whose raw ASR produced nothing but which a
    // curator then hand-annotated or explicitly decided is valid. A machine jury proposal alone is
    // evidence, not content authority, and must not hide the empty transcript.
    // Flagging it spuriously raised an EmptyTranscript warning that blocks a production bundle export
    // under the default warning_threshold=0.
    for seg in segments {
        let has_content = !crate::quality::effective_transcript(seg).trim().is_empty();
        if !has_content {
            issues.push(ValidationIssue {
                severity: IssueSeverity::Warning,
                category: IssueCategory::EmptyTranscript,
                segment_id: Some(seg.id.clone()),
                field: "raw_transcript".to_string(),
                message:
                    "Segment has no authoritative transcript (human verdict, annotation, and champion raw are empty)"
                        .to_string(),
                details: Some(format!("Path: {}", seg.audio_path)),
            });
        }
    }

    // 3. Check duration consistency (audio file vs stored)
    for seg in segments {
        if seg.duration_ms <= 0 {
            issues.push(ValidationIssue {
                severity: IssueSeverity::Warning,
                category: IssueCategory::DurationMismatch,
                segment_id: Some(seg.id.clone()),
                field: "duration_ms".to_string(),
                message: format!("Duration is 0 or negative: {}ms", seg.duration_ms),
                details: None,
            });
        }
    }

    // 4. Check speaker IDs
    let speaker_ids: HashSet<&str> = segments.iter().filter_map(|s| s.speaker_id.as_deref()).collect();
    for sid in &speaker_ids {
        if sid.trim().is_empty() {
            let affected: Vec<&str> =
                segments.iter().filter(|s| s.speaker_id.as_deref() == Some(*sid)).map(|s| s.id.as_str()).collect();
            issues.push(ValidationIssue {
                severity: IssueSeverity::Warning,
                category: IssueCategory::InvalidSpeaker,
                segment_id: affected.first().map(|s| s.to_string()),
                field: "speaker_id".to_string(),
                message: "Speaker ID is empty string".to_string(),
                details: Some(format!("Affects {} segment(s)", affected.len())),
            });
        }
    }

    // 5. Check annotation completeness
    for seg in segments {
        if let Some(ref ann) = seg.annotated_transcript {
            if ann.trim().is_empty() {
                issues.push(ValidationIssue {
                    severity: IssueSeverity::Warning,
                    category: IssueCategory::AnnotationIncomplete,
                    segment_id: Some(seg.id.clone()),
                    field: "annotated_transcript".to_string(),
                    message: "Annotation exists but is empty".to_string(),
                    details: None,
                });
            }
        }
    }

    // 6. WER/CER quality gates for annotated segments
    for seg in segments {
        if let Some(ref reference) = seg.annotated_transcript {
            if reference.trim().is_empty() {
                continue;
            }
            // Score the SAME hypothesis as quality.rs (the RAW ASR output) — compute_wer/cer
            // canonicalize both sides symmetrically, but the stored normalized_transcript may be
            // one-way number-verbalized, which irreversibly inflates WER/CER against a digit-form
            // reference. Scoring normalized here while quality.rs scores raw produced false
            // HighWer/HighCer Errors that blocked exports on perfect transcripts.
            let hypothesis = crate::quality::hypothesis_transcript(seg);
            let wer_score = wer::compute_wer(reference, hypothesis);
            let cer_score = wer::compute_cer(reference, hypothesis);

            if wer_score > settings.max_wer_threshold {
                issues.push(ValidationIssue {
                    severity: if settings.enforce_quality_gates {
                        IssueSeverity::Error
                    } else {
                        IssueSeverity::Warning
                    },
                    category: IssueCategory::HighWer,
                    segment_id: Some(seg.id.clone()),
                    field: "annotated_transcript".to_string(),
                    message: format!(
                        "WER {:.1}% exceeds threshold {:.1}%",
                        wer_score * 100.0,
                        settings.max_wer_threshold * 100.0
                    ),
                    details: Some(format!("CER {:.1}%", cer_score * 100.0)),
                });
            } else if cer_score > settings.max_cer_threshold {
                issues.push(ValidationIssue {
                    severity: if settings.enforce_quality_gates {
                        IssueSeverity::Error
                    } else {
                        IssueSeverity::Warning
                    },
                    category: IssueCategory::HighCer,
                    segment_id: Some(seg.id.clone()),
                    field: "annotated_transcript".to_string(),
                    message: format!(
                        "CER {:.1}% exceeds threshold {:.1}%",
                        cer_score * 100.0,
                        settings.max_cer_threshold * 100.0
                    ),
                    details: Some(format!("WER {:.1}%", wer_score * 100.0)),
                });
            }
        }
    }

    // 7. Check audio quality metrics (clipping and low RMS)
    for seg in segments {
        if let Some(clipping) = seg.clipping_ratio {
            if clipping > 0.01 {
                issues.push(ValidationIssue {
                    severity: IssueSeverity::Warning,
                    category: IssueCategory::ClippingDetected,
                    segment_id: Some(seg.id.clone()),
                    field: "clipping_ratio".to_string(),
                    message: format!("Segment has high clipping ratio: {:.2}%", clipping * 100.0),
                    details: Some(format!("Path: {}", seg.audio_path)),
                });
            }
        }
        if let Some(rms) = seg.rms_db {
            if rms < -40.0 {
                issues.push(ValidationIssue {
                    severity: IssueSeverity::Warning,
                    category: IssueCategory::LowRmsVolume,
                    segment_id: Some(seg.id.clone()),
                    field: "rms_db".to_string(),
                    message: format!("Segment has very low RMS volume: {:.1} dB", rms),
                    details: Some(format!("Path: {}", seg.audio_path)),
                });
            }
        }
    }

    // 8. Flag segments with imprecise (energy-heuristic) alignment timestamps
    for seg in segments {
        if seg.alignment_json.is_some() && seg.alignment_quality.as_deref() == Some("energy_heuristic") {
            issues.push(ValidationIssue {
                severity: IssueSeverity::Warning,
                category: IssueCategory::AlignmentHeuristic,
                segment_id: Some(seg.id.clone()),
                field: "alignment_quality".to_string(),
                message: "Segment uses energy-heuristic alignment (imprecise timestamps)".to_string(),
                details: Some("Run forced CTC alignment to get word-level precision.".to_string()),
            });
        }
    }

    let quality = quality::compute_quality_from_segments_with_settings(segments, settings);
    if settings.enforce_quality_gates && !quality.quality_gate_passed {
        issues.push(ValidationIssue {
            severity: IssueSeverity::Error,
            category: IssueCategory::QualityGateFailed,
            segment_id: None,
            field: "dataset".to_string(),
            message: "Dataset failed quality gates".to_string(),
            details: Some(format!(
                "WER outliers: {}, CER outliers: {}, empty: {}, low confidence: {}",
                quality.segments_above_wer_threshold,
                quality.segments_above_cer_threshold,
                quality.empty_transcript_count,
                quality.low_confidence_count
            )),
        });
    }

    let errors: Vec<_> = issues.iter().filter(|i| i.severity == IssueSeverity::Error).cloned().collect();
    let warnings: Vec<_> = issues.iter().filter(|i| i.severity == IssueSeverity::Warning).cloned().collect();
    // `passed` counts SEGMENTS with no failure attributable to them — NOT errors.len(). The old
    // count was wrong three ways: (1) a dataset-level gate Error (segment_id None) is not a segment but
    // was subtracted as one (phantom failing row with no "go to segment" target); (2) a segment that
    // both raised a per-segment Error and tripped the aggregate gate was double-counted; (3) when
    // several segments share one missing audio file the MissingAudio Error is deduped to a single
    // representative id, under-counting the failures. Attribute by segment id, and treat every segment
    // whose audio file is missing as failed.
    let failed_ids: HashSet<&str> = errors.iter().filter_map(|e| e.segment_id.as_deref()).collect();
    let missing_paths: HashSet<&str> = audio_paths.iter().copied().filter(|p| !Path::new(p).exists()).collect();
    let passed = segments
        .iter()
        .filter(|s| !failed_ids.contains(s.id.as_str()) && !missing_paths.contains(s.audio_path.as_str()))
        .count();

    // Audit 2026-08-05 #6: this subtitle read "All 144 segments passed validation" on a corpus with
    // ZERO exportable rows, which a reader takes as a publication verdict. It is not one. Validation
    // answers "is this data internally sound"; export eligibility is a different question answered by
    // training_grade_for_segment and the rights gate. Naming what was checked keeps the sentence true.
    let summary = if errors.is_empty() && warnings.is_empty() {
        format!("All {} segments passed validation checks", segments.len())
    } else {
        let mut parts = Vec::new();
        if !errors.is_empty() {
            parts.push(format!("{} error(s)", errors.len()));
        }
        if !warnings.is_empty() {
            parts.push(format!("{} warning(s)", warnings.len()));
        }
        format!("{} — {} passed validation checks", parts.join(", "), passed)
    };

    Ok(ValidationReport {
        total_segments: segments.len(),
        total_audio_files: audio_paths.len(),
        passed,
        errors,
        warnings,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, SpeechSegment};

    fn make_seg(id: &str, path: &str, text: &str) -> SpeechSegment {
        SpeechSegment {
            id: id.to_string(),
            created_at: None,
            audio_path: path.to_string(),
            raw_transcript: text.to_string(),
            normalized_transcript: None,
            annotated_transcript: None,
            alignment_json: None,
            duration_ms: 1000,
            speaker_id: None,
            verified: false,
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

    #[test]
    fn test_validate_empty_dataset() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let report = validate_dataset(&db).unwrap();
        assert_eq!(report.total_segments, 0);
        assert!(report.errors.is_empty());
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn passed_counts_failing_segments_not_error_issue_count() {
        // Round-18: `passed = total - errors.len()` under-counted failures when N segments share ONE
        // missing audio file — the MissingAudio Error is deduped to a single representative id, so the
        // old count reported passed = 2 - 1 = 1. Both rows referencing the missing file must count as
        // failed, so passed must be 0.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&make_seg("a", "/does/not/exist.wav", "x")).unwrap();
        db.insert_segment(&make_seg("b", "/does/not/exist.wav", "y")).unwrap();

        let report = validate_dataset(&db).unwrap();

        assert_eq!(report.total_segments, 2);
        assert_eq!(report.passed, 0, "both segments sharing a missing audio file must be counted as failed");
    }

    #[test]
    fn wer_gate_scores_raw_hypothesis_like_quality_rs() {
        // True-10 audit: validation scored normalized_transcript while quality.rs deliberately
        // scores RAW (one-way number verbalization inflates WER/CER irreversibly). A perfect
        // transcript with verbalized numbers must NOT raise HighWer/HighCer here — previously it
        // did, and with enforce_quality_gates on that false positive blocked the production bundle.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let mut s = make_seg("num-1", "/fake/num.wav", "تەمەنی ١٤ ساڵ");
        s.annotated_transcript = Some("تەمەنی ١٤ ساڵ".to_string()); // digit-form human reference
        s.normalized_transcript = Some("تەمەنی یەک چوار ساڵ".to_string()); // one-way verbalized
        db.insert_legacy_segment_fixture(&s).unwrap();

        let report = validate_dataset(&db).unwrap();
        let high_rate = |issues: &[ValidationIssue]| {
            issues.iter().any(|i| matches!(i.category, IssueCategory::HighWer | IssueCategory::HighCer))
        };
        assert!(
            !high_rate(&report.errors) && !high_rate(&report.warnings),
            "a perfect raw transcript must not trip the WER/CER gate because of a verbalized \
             normalized_transcript; issues: {:?} {:?}",
            report.errors,
            report.warnings
        );
    }

    #[test]
    fn test_validate_empty_transcript() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&make_seg("test1", "/fake/path.wav", "")).unwrap();
        let report = validate_dataset(&db).unwrap();
        assert!(report.warnings.iter().any(|i| i.category == IssueCategory::EmptyTranscript));
    }

    #[test]
    fn empty_raw_with_human_annotation_is_not_flagged_empty() {
        // Round-25 #2: a clip whose raw ASR was empty but which a curator hand-annotated is valid and
        // must NOT raise an EmptyTranscript warning (which would block a production bundle export).
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let mut seg = make_seg("annotated", "/fake/path.wav", "");
        seg.annotated_transcript = Some("دەقی دەستکارد".to_string());
        db.insert_legacy_segment_fixture(&seg).unwrap();
        let report = validate_dataset(&db).unwrap();
        assert!(
            !report.warnings.iter().any(|i| i.category == IssueCategory::EmptyTranscript),
            "a hand-annotated segment with empty raw ASR must not be flagged empty"
        );
    }

    #[test]
    fn machine_verdict_alone_cannot_hide_an_empty_authoritative_transcript() {
        let mut machine_only = make_seg("machine-only", "/fake/path.wav", "");
        machine_only.verdict = Some("jury_accept".to_string());
        machine_only.verdict_transcript = Some("machine jury proposal".to_string());

        let report = validate_segments_with_settings(&[machine_only.clone()], &AppSettings::default()).unwrap();
        assert!(
            report.warnings.iter().any(|issue| issue.category == IssueCategory::EmptyTranscript),
            "machine evidence must not mint transcript authority"
        );

        machine_only.human_decision = Some("accept".to_string());
        let report = validate_segments_with_settings(&[machine_only], &AppSettings::default()).unwrap();
        assert!(
            !report.warnings.iter().any(|issue| issue.category == IssueCategory::EmptyTranscript),
            "an explicit human decision may promote its frozen verdict transcript"
        );
    }

    #[test]
    fn test_validate_zero_duration() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&SpeechSegment { duration_ms: 0, ..make_seg("test1", "/fake/path.wav", "hello") }).unwrap();
        let report = validate_dataset(&db).unwrap();
        assert!(report.warnings.iter().any(|i| i.category == IssueCategory::DurationMismatch));
    }

    #[test]
    fn test_validate_missing_audio() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&make_seg("test1", "C:\\nonexistent\\file.wav", "hello")).unwrap();
        let report = validate_dataset(&db).unwrap();
        // Missing audio is a check against filesystem, should be error if file doesn't exist
        assert!(report.errors.iter().any(|i| i.category == IssueCategory::MissingAudio));
    }

    #[test]
    fn test_validate_annotations() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_legacy_segment_fixture(&SpeechSegment {
            annotated_transcript: Some("".to_string()),
            ..make_seg("test1", "/path.wav", "hello")
        })
        .unwrap();
        let report = validate_dataset(&db).unwrap();
        assert!(report.warnings.iter().any(|i| i.category == IssueCategory::AnnotationIncomplete));
    }

    #[test]
    fn test_validate_audio_quality_clipping() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&SpeechSegment { clipping_ratio: Some(0.05), ..make_seg("test1", "/fake.wav", "hello") })
            .unwrap();
        let report = validate_dataset(&db).unwrap();
        assert!(report.warnings.iter().any(|i| i.category == IssueCategory::ClippingDetected));
    }

    #[test]
    fn test_validate_audio_quality_low_rms() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&SpeechSegment { rms_db: Some(-45.0), ..make_seg("test1", "/fake.wav", "hello") }).unwrap();
        let report = validate_dataset(&db).unwrap();
        assert!(report.warnings.iter().any(|i| i.category == IssueCategory::LowRmsVolume));
    }
}
