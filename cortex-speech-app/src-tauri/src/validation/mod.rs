pub mod input;

use crate::db::Database;
use crate::error::AppResult;
use crate::quality;
use crate::settings::AppSettings;
use crate::wer;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReport {
    pub total_segments: usize,
    pub total_audio_files: usize,
    pub passed: usize,
    pub warnings: Vec<ValidationIssue>,
    pub errors: Vec<ValidationIssue>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationIssue {
    pub severity: IssueSeverity,
    pub category: IssueCategory,
    pub segment_id: Option<String>,
    pub field: String,
    pub message: String,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum IssueSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

    // 2. Check empty transcripts
    for seg in &segments {
        if seg.raw_transcript.trim().is_empty() {
            issues.push(ValidationIssue {
                severity: IssueSeverity::Warning,
                category: IssueCategory::EmptyTranscript,
                segment_id: Some(seg.id.clone()),
                field: "raw_transcript".to_string(),
                message: "Raw transcript is empty".to_string(),
                details: Some(format!("Path: {}", seg.audio_path)),
            });
        }
    }

    // 3. Check duration consistency (audio file vs stored)
    for seg in &segments {
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
    for seg in &segments {
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
    for seg in &segments {
        if let Some(ref reference) = seg.annotated_transcript {
            if reference.trim().is_empty() {
                continue;
            }
            let hypothesis = seg.normalized_transcript.as_deref().unwrap_or(&seg.raw_transcript);
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
    for seg in &segments {
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
    for seg in &segments {
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

    let quality = quality::compute_quality_from_segments_with_settings(&segments, settings);
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

    let summary = if errors.is_empty() && warnings.is_empty() {
        format!("All {} segments passed validation", segments.len())
    } else {
        let mut parts = Vec::new();
        if !errors.is_empty() {
            parts.push(format!("{} error(s)", errors.len()));
        }
        if !warnings.is_empty() {
            parts.push(format!("{} warning(s)", warnings.len()));
        }
        format!("{} — {} passed", parts.join(", "), passed)
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
            ood_score: None,
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
    fn test_validate_empty_transcript() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&make_seg("test1", "/fake/path.wav", "")).unwrap();
        let report = validate_dataset(&db).unwrap();
        assert!(report.warnings.iter().any(|i| i.category == IssueCategory::EmptyTranscript));
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
        db.insert_segment(&SpeechSegment {
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
