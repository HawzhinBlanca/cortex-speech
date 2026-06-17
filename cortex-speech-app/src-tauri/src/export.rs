use crate::atomic_file::{remove_file_on_error, replace_file};
use crate::audio;
use crate::chunking;
use crate::db::{Database, SourceTranscriptRecord, SpeechSegment};
use crate::error::{AppError, AppResult};
use crate::quality::{self, TrainingGradeReport, TrainingGradeSummary};
use crate::settings::ExportFormat;
use arrow_array::{BooleanArray, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use std::collections::BTreeSet;
use std::io::Write;
use std::sync::Arc;

#[derive(serde::Serialize)]
pub struct DatasetMetadata {
    pub name: String,
    pub version: String,
    pub language: String,
    pub script: String,
    pub total_segments: usize,
    pub total_duration_ms: i64,
    pub verified_segments: usize,
    pub training_grade_summary: TrainingGradeSummary,
    pub exported_at: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportSegmentRecord<'a> {
    #[serde(flatten)]
    segment: &'a SpeechSegment,
    training_transcript: String,
    transcript_source: String,
    training_grade: String,
    training_ready: bool,
    training_reasons: Vec<String>,
}

impl<'a> ExportSegmentRecord<'a> {
    fn new(segment: &'a SpeechSegment) -> Self {
        let report = quality::training_grade_for_segment(segment);
        Self {
            segment,
            training_transcript: report.transcript,
            transcript_source: report.transcript_source,
            training_grade: report.grade,
            training_ready: report.training_ready,
            training_reasons: report.reasons,
        }
    }
}

fn is_training_ready_for_huggingface_export(
    db: &Database,
    segment: &SpeechSegment,
    grade: &TrainingGradeReport,
    ready_agentic_segment_ids: &BTreeSet<String>,
    required_source_reference_models: &[String],
) -> AppResult<bool> {
    if !grade.training_ready {
        return Ok(false);
    }
    if grade.grade == quality::TRAINING_GRADE_SILVER {
        if !ready_agentic_segment_ids.contains(&segment.id) {
            return Ok(false);
        }
        let hypotheses = db.get_hypotheses_for_segment(&segment.id)?;
        if !quality::hypothesis_coverage_for_model_outputs(&hypotheses).passes_minimum {
            return Ok(false);
        }
        if segment_has_source_reference_commit_evidence(segment)
            && !source_reference_identity_verified_for_huggingface_export(
                db,
                segment,
                required_source_reference_models,
            )?
        {
            return Ok(false);
        }
        return Ok(true);
    }
    Ok(true)
}

fn segment_has_source_reference_commit_evidence(segment: &SpeechSegment) -> bool {
    let Some(evidence) = segment.evidence_json.as_deref().map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(evidence) else {
        return false;
    };
    quality::has_source_reference_commit_evidence(&value)
}

fn source_reference_identity_verified_for_huggingface_export(
    db: &Database,
    segment: &SpeechSegment,
    required_source_reference_models: &[String],
) -> AppResult<bool> {
    let references = db.get_source_transcripts_for_audio(&segment.audio_path)?;
    if references.is_empty() {
        return Ok(false);
    }

    for required_model in required_source_reference_models {
        let Some(reference) = references.iter().find(|reference| reference.model_id == *required_model) else {
            return Ok(false);
        };
        if !crate::agentic::is_usable_source_reference_transcript(&reference.transcript_text)
            || !source_reference_record_matches_current_audio(reference)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn source_reference_record_matches_current_audio(reference: &SourceTranscriptRecord) -> bool {
    let Some(stored_hash) = reference.audio_content_hash.as_deref().map(str::trim).filter(|value| !value.is_empty())
    else {
        return false;
    };
    let Some(stored_size) = reference.audio_size_bytes else {
        return false;
    };
    let Ok(current_identity) = crate::pipeline::source_audio_identity(std::path::Path::new(&reference.audio_path))
    else {
        return false;
    };
    stored_hash == current_identity.content_hash && stored_size == current_identity.size_bytes
}

fn ready_agentic_huggingface_segment_ids(db: &Database) -> AppResult<BTreeSet<String>> {
    let Some(report) = crate::runs::list_agent_import_reports(db, Some(1))?.into_iter().next() else {
        return Ok(BTreeSet::new());
    };
    let promotion_ready = report
        .summary
        .orchestration_stages
        .iter()
        .any(|stage| stage.stage == "dataset_promotion" && stage.status == "ready");
    let readiness_ready = report
        .summary
        .agentic_readiness
        .as_ref()
        .and_then(|readiness| readiness.get("ready"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && report
            .summary
            .agentic_readiness
            .as_ref()
            .and_then(|readiness| readiness.get("status"))
            .and_then(serde_json::Value::as_str)
            == Some("ready");
    if !promotion_ready || !readiness_ready {
        return Ok(BTreeSet::new());
    }
    Ok(report.segment_ids.into_iter().collect())
}

pub fn export_dataset(db: &Database, path: &std::path::Path, format: &ExportFormat) -> AppResult<()> {
    let segments = db.get_segments(None)?;
    let total_duration: i64 = segments.iter().map(|s| s.duration_ms).sum();
    let verified = segments.iter().filter(|s| s.verified).count();

    let metadata = DatasetMetadata {
        name: "cortex-kurdish-speech-dataset".into(),
        version: "2.0".into(),
        language: "ckb".into(),
        script: "Arabic".into(),
        total_segments: segments.len(),
        total_duration_ms: total_duration,
        verified_segments: verified,
        training_grade_summary: quality::training_grade_summary(&segments),
        exported_at: chrono::Utc::now().to_rfc3339(),
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    match format {
        ExportFormat::Json => export_json(path, &metadata, &segments),
        ExportFormat::Jsonl => export_jsonl(path, &segments),
        ExportFormat::Csv => export_csv(path, &segments),
        ExportFormat::Parquet => export_parquet(path, &segments),
    }
}

/// Export a HuggingFace Datasets–compatible directory (split folders + metadata + dataset card).
pub fn export_huggingface_dataset(
    db: &Database,
    dir: &std::path::Path,
    settings: &crate::settings::AppSettings,
) -> AppResult<()> {
    std::fs::create_dir_all(dir)?;
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir)?;

    let train_dir = data_dir.join("train");
    let val_dir = data_dir.join("validation");
    let test_dir = data_dir.join("test");

    std::fs::create_dir_all(&train_dir)?;
    std::fs::create_dir_all(&val_dir)?;
    std::fs::create_dir_all(&test_dir)?;

    let segments = db.get_segments(None)?;
    if segments.is_empty() {
        return Ok(());
    }
    let ready_agentic_segment_ids = ready_agentic_huggingface_segment_ids(db)?;
    let required_source_reference_models = settings.source_reference_models();

    // Normalize ratios
    let mut train_ratio = settings.hf_train_ratio;
    let mut val_ratio = settings.hf_val_ratio;
    let mut test_ratio = settings.hf_test_ratio;
    let ratio_sum = train_ratio + val_ratio + test_ratio;
    if ratio_sum > 0.0 {
        train_ratio /= ratio_sum;
        val_ratio /= ratio_sum;
        test_ratio /= ratio_sum;
    } else {
        train_ratio = 0.8;
        val_ratio = 0.1;
        test_ratio = 0.1;
    }

    // Group segments
    let mut groups: std::collections::HashMap<String, Vec<SpeechSegment>> = std::collections::HashMap::new();
    if settings.hf_speaker_disjoint {
        for seg in segments.clone() {
            let spk = seg.speaker_id.as_deref().unwrap_or("").trim().to_string();
            let key = if spk.is_empty() { format!("__unknown_{}", seg.id) } else { spk };
            groups.entry(key).or_default().push(seg);
        }
    } else {
        for seg in segments.clone() {
            groups.insert(seg.id.clone(), vec![seg]);
        }
    }

    // Shuffling using custom LCG PRNG (Fisher-Yates)
    struct Lcg {
        state: u64,
    }
    impl Lcg {
        fn new(seed: u64) -> Self {
            Self { state: seed }
        }
        fn next_u32(&mut self) -> u32 {
            self.state = self.state.wrapping_mul(1664525).wrapping_add(1013904223);
            (self.state >> 32) as u32
        }
        fn shuffle<T>(&mut self, slice: &mut [T]) {
            for i in (1..slice.len()).rev() {
                let j = (self.next_u32() as usize) % (i + 1);
                slice.swap(i, j);
            }
        }
    }

    let mut group_keys: Vec<String> = groups.keys().cloned().collect();
    let mut lcg = Lcg::new(settings.hf_split_seed);
    lcg.shuffle(&mut group_keys);

    let total_duration: i64 = segments.iter().map(|s| s.duration_ms).sum();
    let target_train = (total_duration as f64 * train_ratio) as i64;
    let target_val = (total_duration as f64 * val_ratio) as i64;
    let target_test = (total_duration as f64 * test_ratio) as i64;

    let mut train_dur = 0i64;
    let mut val_dur = 0i64;
    let mut test_dur = 0i64;

    let mut train_segs = Vec::new();
    let mut val_segs = Vec::new();
    let mut test_segs = Vec::new();

    for key in group_keys {
        let segs = &groups[&key];
        let group_dur: i64 = segs.iter().map(|s| s.duration_ms).sum();

        let train_deficit = target_train - train_dur;
        let val_deficit = target_val - val_dur;
        let test_deficit = target_test - test_dur;

        let assigned_split = if train_deficit >= val_deficit && train_deficit >= test_deficit {
            train_dur += group_dur;
            train_segs.extend(segs.clone());
            "train"
        } else if val_deficit >= train_deficit && val_deficit >= test_deficit {
            val_dur += group_dur;
            val_segs.extend(segs.clone());
            "validation"
        } else {
            test_dur += group_dur;
            test_segs.extend(segs.clone());
            "test"
        };

        for seg in segs {
            db.update_segment_split(&seg.id, assigned_split).map_err(|error| {
                AppError::Other(format!("Failed to persist split {assigned_split} for {}: {error}", seg.id))
            })?;
        }
    }

    // Helper closure to process and write a split's files
    let process_split = |split_segs: &[SpeechSegment],
                         _split_name: &str,
                         dest_dir: &std::path::Path|
     -> AppResult<(usize, f64)> {
        if split_segs.is_empty() {
            return Ok((0, 0.0));
        }

        let csv_path = dest_dir.join("metadata.csv");
        let csv_tmp = csv_path.with_extension("csv.tmp");
        remove_file_on_error(
            &csv_tmp,
            (|| -> AppResult<(usize, f64)> {
                let mut csv_wtr = csv::Writer::from_path(&csv_tmp)?;
                csv_wtr.write_record([
                    "file_name",
                    "transcription",
                    "speaker_id",
                    "duration_ms",
                    "verified",
                    "training_grade",
                    "training_ready",
                    "transcript_source",
                    "training_reasons",
                ])?;

                let mut total_exported_dur = 0.0;
                let mut count = 0;

                // Group segments by source audio_path so each source file is decoded only once.
                // For a 2-hour podcast split into N segments, this avoids N full re-decodes.
                let mut segs_by_source: std::collections::HashMap<&str, Vec<&SpeechSegment>> =
                    std::collections::HashMap::new();
                for seg in split_segs {
                    segs_by_source.entry(seg.audio_path.as_str()).or_default().push(seg);
                }

                for (source_path_str, segs) in segs_by_source {
                    let source_path = std::path::Path::new(source_path_str);
                    if !source_path.exists() {
                        for seg in &segs {
                            tracing::warn!("Skipping segment {} in HF export: audio not found", seg.id);
                        }
                        continue;
                    }

                    // Decode the source file exactly once.
                    let (sample_rate, full_pcm) = match audio::decode_to_pcm(source_path_str) {
                        Ok(res) => res,
                        Err(e) => {
                            tracing::error!("Failed to decode {source_path_str} in HF export: {e}");
                            continue;
                        }
                    };

                    for seg in segs {
                        let grade = quality::training_grade_for_segment(seg);
                        if !grade.training_ready {
                            tracing::warn!(
                                "Skipping segment {} in HF export: training grade {} ({})",
                                seg.id,
                                grade.grade,
                                grade.reasons.join("; ")
                            );
                            continue;
                        }
                        if !is_training_ready_for_huggingface_export(
                            db,
                            seg,
                            &grade,
                            &ready_agentic_segment_ids,
                            &required_source_reference_models,
                        )? {
                            tracing::warn!(
                                    "Skipping segment {} in HF export: machine training-ready row is missing multi-model hypothesis coverage, ready agentic promotion coverage, or configured source-reference model coverage/current audio identity",
                                    seg.id
                                );
                            continue;
                        }

                        // Slice from the already-decoded PCM buffer.
                        let pcm_slice: std::borrow::Cow<[i16]> = if let Some(ref alignment) = seg.alignment_json {
                            if let Some(meta) = chunking::SegmentSourceMeta::from_alignment_json(alignment) {
                                let start = chunking::ms_to_samples(meta.source_start_ms.max(0) as u32, sample_rate);
                                let end = chunking::ms_to_samples(meta.source_end_ms.max(0) as u32, sample_rate)
                                    .min(full_pcm.len());
                                if end > start {
                                    std::borrow::Cow::Borrowed(&full_pcm[start..end])
                                } else {
                                    std::borrow::Cow::Borrowed(&full_pcm)
                                }
                            } else {
                                std::borrow::Cow::Borrowed(&full_pcm)
                            }
                        } else {
                            std::borrow::Cow::Borrowed(&full_pcm)
                        };

                        let stem = source_path.file_stem().unwrap_or_default().to_string_lossy();
                        let clean_stem = stem
                            .chars()
                            .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
                            .collect::<String>();
                        let filename = format!("{}_{}.wav", clean_stem, seg.id);
                        let out_audio_path = dest_dir.join(&filename);

                        write_wav_atomic(&out_audio_path, 16000, pcm_slice.as_ref())?;

                        let dur_str = seg.duration_ms.to_string();
                        let verified_str = if seg.verified { "1" } else { "0" };
                        let training_ready_str = if grade.training_ready { "1" } else { "0" };
                        let reasons = grade.reasons.join("; ");

                        csv_wtr.write_record([
                            filename.as_str(),
                            grade.transcript.as_str(),
                            seg.speaker_id.as_deref().unwrap_or(""),
                            dur_str.as_str(),
                            verified_str,
                            grade.grade.as_str(),
                            training_ready_str,
                            grade.transcript_source.as_str(),
                            reasons.as_str(),
                        ])?;

                        total_exported_dur += seg.duration_ms as f64 / 1000.0;
                        count += 1;
                    }
                }

                csv_wtr.flush()?;
                drop(csv_wtr);
                replace_file(&csv_tmp, &csv_path)?;
                Ok((count, total_exported_dur))
            })(),
        )
    };

    let (train_count, train_secs) = process_split(&train_segs, "train", &train_dir)?;
    let (val_count, val_secs) = process_split(&val_segs, "validation", &val_dir)?;
    let (test_count, test_secs) = process_split(&test_segs, "test", &test_dir)?;

    let total_count = train_count + val_count + test_count;
    let total_secs = train_secs + val_secs + test_secs;

    // Write dataset card (README.md)
    let model_str = format!("{:?}", settings.asr_model_size);
    let readme = format!(
        r#"---
language:
- ckb
task_categories:
- automatic-speech-recognition
tags:
- audio
- speech
- kurdish
license: {}
pretty_name: Cortex Kurdish Speech Dataset
size_categories:
- n<1K
---

# Cortex Kurdish (Sorani) Speech Dataset

This dataset was exported from Cortex Speech Processor.

## Dataset Summary
- **Language**: Central Kurdish (Sorani, ckb)
- **License**: {}
- **Provenance**: Exported via Cortex Speech App v{}, using ASR Model {} on {}

## Split Statistics
| Split | Examples | Duration (seconds) |
|---|---|---|
| Train | {} | {:.2} |
| Validation | {} | {:.2} |
| Test | {} | {:.2} |
| **Total** | {} | {:.2} |
"#,
        settings.hf_license,
        settings.hf_license,
        env!("CARGO_PKG_VERSION"),
        model_str,
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
        train_count,
        train_secs,
        val_count,
        val_secs,
        test_count,
        test_secs,
        total_count,
        total_secs
    );
    write_text_atomic(&dir.join("README.md"), &readme)?;

    // Write dataset_infos.json
    let info = serde_json::json!({
        "cortex-kurdish-split-speech": {
            "description": "Sorani Kurdish speech segments split into train/validation/test with relative paths",
            "features": {
                "file_name": {"dtype": "string", "_type": "Value"},
                "transcription": {"dtype": "string", "_type": "Value"},
                "speaker_id": {"dtype": "string", "_type": "Value"},
                "duration_ms": {"dtype": "int64", "_type": "Value"},
                "verified": {"dtype": "bool", "_type": "Value"},
                "training_grade": {"dtype": "string", "_type": "Value"},
                "training_ready": {"dtype": "bool", "_type": "Value"},
                "transcript_source": {"dtype": "string", "_type": "Value"},
                "training_reasons": {"dtype": "string", "_type": "Value"},
            },
            "splits": {
                "train": {"num_examples": train_count},
                "validation": {"num_examples": val_count},
                "test": {"num_examples": test_count}
            }
        }
    });
    write_text_atomic(&dir.join("dataset_infos.json"), &serde_json::to_string_pretty(&info)?)?;

    Ok(())
}

fn export_json(path: &std::path::Path, metadata: &DatasetMetadata, segments: &[SpeechSegment]) -> AppResult<()> {
    let records = export_records(segments);
    let json = serde_json::to_string_pretty(&serde_json::json!({
        "metadata": metadata,
        "segments": records,
    }))?;
    // Atomic write: write to .tmp then rename to avoid truncated output on crash.
    let tmp = path.with_extension("json.tmp");
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            std::fs::write(&tmp, &json)?;
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

fn export_jsonl(path: &std::path::Path, segments: &[SpeechSegment]) -> AppResult<()> {
    // Atomic write: accumulate into a temp file, then rename.
    let tmp = path.with_extension("jsonl.tmp");
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            let mut file = std::fs::File::create(&tmp)?;
            for seg in segments {
                let line = serde_json::to_string(&ExportSegmentRecord::new(seg))?;
                writeln!(file, "{line}")?;
            }
            file.flush()?;
            drop(file);
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

fn export_csv(path: &std::path::Path, segments: &[SpeechSegment]) -> AppResult<()> {
    // Atomic write: write CSV to .tmp then rename.
    let tmp = path.with_extension("csv.tmp");
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            let mut wtr = csv::Writer::from_path(&tmp)?;
            wtr.write_record([
                "id",
                "audio_path",
                "raw_transcript",
                "normalized_transcript",
                "annotated_transcript",
                "duration_ms",
                "speaker_id",
                "verified",
                "training_transcript",
                "transcript_source",
                "training_grade",
                "training_ready",
                "training_reasons",
            ])?;

            for seg in segments {
                let grade = quality::training_grade_for_segment(seg);
                let reasons = grade.reasons.join("; ");
                wtr.write_record([
                    seg.id.as_str(),
                    seg.audio_path.as_str(),
                    seg.raw_transcript.as_str(),
                    seg.normalized_transcript.as_deref().unwrap_or(""),
                    seg.annotated_transcript.as_deref().unwrap_or(""),
                    &seg.duration_ms.to_string(),
                    seg.speaker_id.as_deref().unwrap_or(""),
                    if seg.verified { "1" } else { "0" },
                    grade.transcript.as_str(),
                    grade.transcript_source.as_str(),
                    grade.grade.as_str(),
                    if grade.training_ready { "1" } else { "0" },
                    reasons.as_str(),
                ])?;
            }
            wtr.flush()?;
            drop(wtr);
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

fn export_records(segments: &[SpeechSegment]) -> Vec<ExportSegmentRecord<'_>> {
    segments.iter().map(ExportSegmentRecord::new).collect()
}

fn write_text_atomic(path: &std::path::Path, text: &str) -> AppResult<()> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|ext| ext.to_str()).unwrap_or("tmp")));
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            std::fs::write(&tmp, text)?;
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

fn write_wav_atomic(path: &std::path::Path, sample_rate: u32, samples: &[i16]) -> AppResult<()> {
    let tmp = unique_tmp_path(path);
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut wav_writer = hound::WavWriter::create(&tmp, spec)
                .map_err(|e| crate::error::AppError::Other(format!("Failed to create WAV: {e}")))?;
            for &sample in samples {
                wav_writer
                    .write_sample(sample)
                    .map_err(|e| crate::error::AppError::Other(format!("Failed to write sample: {e}")))?;
            }
            wav_writer.finalize().map_err(|e| crate::error::AppError::Other(format!("Failed to finalize WAV: {e}")))?;
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

fn unique_tmp_path(path: &std::path::Path) -> std::path::PathBuf {
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("export.wav");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    path.with_file_name(format!("{file_name}.tmp-{}-{nonce}", std::process::id()))
}

fn export_parquet(path: &std::path::Path, segments: &[SpeechSegment]) -> AppResult<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("audio_path", DataType::Utf8, false),
        Field::new("raw_transcript", DataType::Utf8, false),
        Field::new("normalized_transcript", DataType::Utf8, true),
        Field::new("annotated_transcript", DataType::Utf8, true),
        Field::new("alignment_json", DataType::Utf8, true),
        Field::new("duration_ms", DataType::Int64, false),
        Field::new("speaker_id", DataType::Utf8, true),
        Field::new("verified", DataType::Boolean, false),
        Field::new("training_transcript", DataType::Utf8, false),
        Field::new("transcript_source", DataType::Utf8, false),
        Field::new("training_grade", DataType::Utf8, false),
        Field::new("training_ready", DataType::Boolean, false),
        Field::new("training_reasons", DataType::Utf8, false),
    ]));

    let grade_reports: Vec<TrainingGradeReport> = segments.iter().map(quality::training_grade_for_segment).collect();
    let grade_reasons: Vec<String> = grade_reports.iter().map(|report| report.reasons.join("; ")).collect();
    let ids: StringArray = segments.iter().map(|s| Some(s.id.as_str())).collect();
    let audio_paths: StringArray = segments.iter().map(|s| Some(s.audio_path.as_str())).collect();
    let raw: StringArray = segments.iter().map(|s| Some(s.raw_transcript.as_str())).collect();
    let normalized: StringArray = segments.iter().map(|s| s.normalized_transcript.as_deref()).collect();
    let annotated: StringArray = segments.iter().map(|s| s.annotated_transcript.as_deref()).collect();
    let alignment: StringArray = segments.iter().map(|s| s.alignment_json.as_deref()).collect();
    let duration_ms: Int64Array = segments.iter().map(|s| Some(s.duration_ms)).collect();
    let speaker_id: StringArray = segments.iter().map(|s| s.speaker_id.as_deref()).collect();
    let verified: BooleanArray = segments.iter().map(|s| Some(s.verified)).collect();
    let training_transcript: StringArray =
        grade_reports.iter().map(|report| Some(report.transcript.as_str())).collect();
    let transcript_source: StringArray =
        grade_reports.iter().map(|report| Some(report.transcript_source.as_str())).collect();
    let training_grade: StringArray = grade_reports.iter().map(|report| Some(report.grade.as_str())).collect();
    let training_ready: BooleanArray = grade_reports.iter().map(|report| Some(report.training_ready)).collect();
    let training_reasons: StringArray = grade_reasons.iter().map(|reasons| Some(reasons.as_str())).collect();

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(ids),
            Arc::new(audio_paths),
            Arc::new(raw),
            Arc::new(normalized),
            Arc::new(annotated),
            Arc::new(alignment),
            Arc::new(duration_ms),
            Arc::new(speaker_id),
            Arc::new(verified),
            Arc::new(training_transcript),
            Arc::new(transcript_source),
            Arc::new(training_grade),
            Arc::new(training_ready),
            Arc::new(training_reasons),
        ],
    )
    .map_err(|e| crate::error::AppError::Other(format!("Parquet batch build failed: {e}")))?;

    let tmp = path.with_extension("parquet.tmp");
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            let file = std::fs::File::create(&tmp)?;
            let props = WriterProperties::builder().set_compression(Compression::SNAPPY).build();
            let mut writer = ArrowWriter::try_new(file, schema, Some(props))
                .map_err(|e| crate::error::AppError::Other(format!("Parquet writer failed: {e}")))?;
            writer.write(&batch).map_err(|e| crate::error::AppError::Other(format!("Parquet write failed: {e}")))?;
            writer.close().map_err(|e| crate::error::AppError::Other(format!("Parquet close failed: {e}")))?;
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, SegmentHypothesis, SourceTranscriptRecord};
    use tempfile::NamedTempFile;

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
        let validation_metadata =
            std::fs::read_to_string(out_dir.join("data/validation/metadata.csv")).unwrap_or_default();
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
            ood_score: None,
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
    fn export_parquet_writes_valid_file() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        db.insert_segment(&sample_segment("pq-1")).unwrap();

        let out_tmp = NamedTempFile::new().unwrap();
        let out_path = out_tmp.path().with_extension("parquet");
        export_dataset(&db, &out_path, &ExportFormat::Parquet).unwrap();

        assert!(out_path.exists());
        assert!(out_path.metadata().unwrap().len() > 0);

        let file = std::fs::File::open(&out_path).unwrap();
        let reader =
            parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file).unwrap().build().unwrap();
        let batches: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
        assert!(batches[0].schema().field_with_name("training_transcript").is_ok());
        assert!(batches[0].schema().field_with_name("training_grade").is_ok());
        assert!(batches[0].schema().field_with_name("training_ready").is_ok());
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
        record_ready_agent_report(
            &db,
            segment.audio_path.as_str(),
            segment.id.as_str(),
            "run-hf-legacy-source-reference",
        );

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
        let segment =
            insert_machine_silver_segment_with_hf_coverage(&db, &wav_path, "hf-missing-source-reference-model");
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
        record_ready_agent_report(
            &db,
            segment.audio_path.as_str(),
            segment.id.as_str(),
            "run-hf-stale-source-reference",
        );

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

    fn assert_no_tmp_exports(dir: &std::path::Path) {
        let tmp_left = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains(".tmp-"));
        assert!(!tmp_left, "temporary export files should be promoted or removed");
    }
}
