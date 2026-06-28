use crate::atomic_file::{remove_file_on_error, replace_file};
use crate::audio;
use crate::db::{Database, SpeechSegment};
use crate::error::{AppError, AppResult};
use crate::validation::input as validate;
use flacenc::error::Verify;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioExportOptions {
    pub output_dir: String,
    pub format: AudioExportFormat,
    pub sample_rate: u32,
    pub include_metadata: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AudioExportFormat {
    Wav,
    Flac,
}

impl Default for AudioExportOptions {
    fn default() -> Self {
        Self { output_dir: String::new(), format: AudioExportFormat::Wav, sample_rate: 16000, include_metadata: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioExportResult {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub output_dir: String,
    pub files: Vec<String>,
    pub errors: Vec<String>,
}

struct ExportedAudioFile {
    filename: String,
    segment: SpeechSegment,
}

/// Export audio segments from a dataset.
/// For each segment, copies or extracts the relevant audio portion to the output directory.
pub fn export_audio_segments(
    db: &Database,
    segment_ids: &[String],
    options: &AudioExportOptions,
) -> AppResult<AudioExportResult> {
    if options.sample_rate < 8000 || options.sample_rate > 96000 {
        return Err(AppError::Validation(format!(
            "Invalid sample rate: {} (must be between 8000 and 96000)",
            options.sample_rate
        )));
    }

    let validated = validate::validate_output_path(&options.output_dir).map_err(AppError::Validation)?;
    let options = AudioExportOptions { output_dir: validated, ..options.clone() };
    let output_dir = Path::new(&options.output_dir);
    if !output_dir.exists() {
        std::fs::create_dir_all(output_dir)
            .map_err(|e| AppError::Other(format!("Failed to create output dir: {e}")))?;
    }

    // Fail-closed: never export held-out gold eval audio, exactly like export_dataset / HF / bundle.
    // Resolve the requested ids to segments, run the shared holdout filter, and skip any requested id
    // that loaded but is held out. Ids that don't load are NOT excluded here, so export_single_segment
    // still runs and reports their not-found error (preserving the original failure accounting).
    let requested: Vec<SpeechSegment> =
        segment_ids.iter().filter_map(|id| db.get_segment_by_id(id).ok().flatten()).collect();
    let loaded_ids: std::collections::HashSet<String> = requested.iter().map(|s| s.id.clone()).collect();
    let allowed_ids: std::collections::HashSet<String> =
        crate::export::exclude_holdout_segments(db, requested)?.into_iter().map(|s| s.id).collect();
    let holdout_excluded: std::collections::HashSet<String> = loaded_ids.difference(&allowed_ids).cloned().collect();

    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut skipped_holdout = 0usize;
    let mut files = Vec::new();
    let mut errors = Vec::new();
    let mut exported = Vec::new();

    for id in segment_ids {
        if holdout_excluded.contains(id) {
            // Intentional fail-closed exclusion; exclude_holdout_segments already logged the reason.
            skipped_holdout += 1;
            continue;
        }
        match export_single_segment(db, id, &options) {
            Ok(exported_file) => {
                succeeded += 1;
                files.push(exported_file.filename.clone());
                exported.push(exported_file);
            }
            Err(e) => {
                failed += 1;
                errors.push(format!("{id}: {e}"));
            }
        }
    }

    if skipped_holdout > 0 {
        tracing::warn!("Audio export: skipped {skipped_holdout} held-out gold segment(s) — not exported (fail-closed)");
    }

    if options.include_metadata && !exported.is_empty() {
        write_metadata_csv(output_dir, &exported, &options)?;
        files.push("metadata.csv".to_string());
    }

    Ok(AudioExportResult {
        total: segment_ids.len(),
        succeeded,
        failed,
        output_dir: options.output_dir.clone(),
        files,
        errors,
    })
}

fn export_single_segment(
    db: &Database,
    segment_id: &str,
    options: &AudioExportOptions,
) -> AppResult<ExportedAudioFile> {
    let seg =
        db.get_segment_by_id(segment_id)?.ok_or_else(|| AppError::Other(format!("Segment not found: {segment_id}")))?;

    let source_path = Path::new(&seg.audio_path);
    if !source_path.exists() {
        return Err(AppError::Other(format!("Audio file not found: {}", seg.audio_path)));
    }

    // Decode audio to 16-bit PCM
    let (sample_rate, pcm_samples) =
        audio::decode_to_pcm(&seg.audio_path).map_err(|e| AppError::Other(format!("Failed to decode audio: {e}")))?;

    // Slice to the segment's alignment window. A present-but-out-of-range window must SKIP
    // (return Err, which the caller counts as a failed segment) — never fall back to the whole
    // recording, which would pair a multi-minute file with one segment's short transcript (silent
    // training-data corruption). Shares the exact guard the HF exporter uses (export::slice_for_export).
    let pcm_window = crate::export::slice_for_export(&pcm_samples, sample_rate, seg.alignment_json.as_deref())
        .ok_or_else(|| {
            AppError::Other(format!(
                "Segment {segment_id}: alignment window out of range for the decoded audio; skipping \
                 to avoid exporting the whole recording under one segment's transcript"
            ))
        })?;
    let pcm_samples = resample_pcm_i16(pcm_window.as_ref(), sample_rate, options.sample_rate);

    // Determine output filename
    let stem = source_path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = match options.format {
        AudioExportFormat::Wav => "wav",
        AudioExportFormat::Flac => "flac",
    };
    let safe_segment_id = validate::sanitize_filename(segment_id);
    let output_filename = format!("{stem}_{safe_segment_id}.{ext}");
    let output_path = Path::new(&options.output_dir).join(&output_filename);
    let tmp_path = temporary_output_path(&output_path);

    match options.format {
        AudioExportFormat::Wav => {
            // Write WAV file (16-bit mono PCM)
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: options.sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };

            remove_file_on_error(
                &tmp_path,
                (|| -> AppResult<()> {
                    let mut writer = hound::WavWriter::create(&tmp_path, spec)
                        .map_err(|e| AppError::Other(format!("Failed to create temporary WAV file: {e}")))?;

                    for &sample in &pcm_samples {
                        writer
                            .write_sample(sample)
                            .map_err(|e| AppError::Other(format!("Failed to write sample: {e}")))?;
                    }

                    writer.finalize().map_err(|e| AppError::Other(format!("Failed to finalize WAV: {e}")))?;
                    replace_file(&tmp_path, &output_path)
                        .map_err(|e| AppError::Other(format!("Failed to promote exported WAV file: {e}")))?;
                    Ok(())
                })(),
            )?;
        }
        AudioExportFormat::Flac => {
            // Write FLAC file using flacenc
            let config = flacenc::config::Encoder::default()
                .into_verified()
                .map_err(|e| AppError::Other(format!("FLAC encoder config error: {:?}", e)))?;

            // Convert i16 samples to i32 for flacenc
            let i32_samples: Vec<i32> = pcm_samples.iter().map(|&s| s as i32).collect();

            // MemSource::from_samples(samples, channels, bits_per_sample, sample_rate)
            let source = flacenc::source::MemSource::from_samples(&i32_samples, 1, 16, options.sample_rate as usize);

            let flac_stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
                .map_err(|e| AppError::Other(format!("FLAC encoding failed: {:?}", e)))?;

            use flacenc::component::BitRepr;
            let mut sink = flacenc::bitsink::ByteSink::new();
            flac_stream
                .write(&mut sink)
                .map_err(|e| AppError::Other(format!("Failed to write FLAC stream to sink: {:?}", e)))?;

            remove_file_on_error(
                &tmp_path,
                (|| -> AppResult<()> {
                    std::fs::write(&tmp_path, sink.as_slice())
                        .map_err(|e| AppError::Other(format!("Failed to write temporary FLAC file: {e}")))?;
                    replace_file(&tmp_path, &output_path)
                        .map_err(|e| AppError::Other(format!("Failed to promote exported FLAC file: {e}")))?;
                    Ok(())
                })(),
            )?;
        }
    }

    Ok(ExportedAudioFile { filename: output_filename, segment: seg })
}

fn write_metadata_csv(
    output_dir: &Path,
    exported: &[ExportedAudioFile],
    options: &AudioExportOptions,
) -> AppResult<()> {
    let metadata_path = output_dir.join("metadata.csv");
    let tmp_path = metadata_path.with_extension("csv.tmp");
    remove_file_on_error(
        &tmp_path,
        (|| -> AppResult<()> {
            let mut wtr = csv::Writer::from_path(&tmp_path)?;
            wtr.write_record([
                "file_name",
                "segment_id",
                "source_audio_path",
                "raw_transcript",
                "normalized_transcript",
                "annotated_transcript",
                "duration_ms",
                "speaker_id",
                "verified",
                "confidence",
                "ctc_score",
                "clipping_ratio",
                "rms_db",
                "snr_db",
                "split",
                "ood_score",
                "verdict",
                "alignment_quality",
                "export_sample_rate",
                "export_format",
            ])?;

            for item in exported {
                let seg = &item.segment;
                let duration_ms = seg.duration_ms.to_string();
                let confidence = optional_f64(seg.confidence);
                let ctc_score = optional_f64(seg.ctc_score);
                let clipping_ratio = optional_f64(seg.clipping_ratio);
                let rms_db = optional_f64(seg.rms_db);
                let snr_db = optional_f64(seg.snr_db);
                let ood_score = optional_f64(seg.ood_score);
                let export_sample_rate = options.sample_rate.to_string();
                let export_format = match options.format {
                    AudioExportFormat::Wav => "wav",
                    AudioExportFormat::Flac => "flac",
                };
                // CWE-1236: neutralize spreadsheet formula injection on the free-text columns
                // (transcripts / speaker / verdict). Numeric/enum structural columns are left untouched.
                // The source path is reduced to its BASENAME (via export_audio_ref) so this shared
                // metadata.csv never leaks the curator's absolute path — the OS username + directory
                // layout — exactly as the JSON/JSONL/CSV/Parquet/HF exporters do.
                let source_ref = crate::export::export_audio_ref(&seg.audio_path);
                let raw_t = crate::export::csv_safe_cell(seg.raw_transcript.as_str());
                let norm_t = crate::export::csv_safe_cell(seg.normalized_transcript.as_deref().unwrap_or(""));
                let annot_t = crate::export::csv_safe_cell(seg.annotated_transcript.as_deref().unwrap_or(""));
                let speaker_t = crate::export::csv_safe_cell(seg.speaker_id.as_deref().unwrap_or(""));
                let verdict_t = crate::export::csv_safe_cell(seg.verdict.as_deref().unwrap_or(""));
                wtr.write_record([
                    item.filename.as_str(),
                    seg.id.as_str(),
                    source_ref,
                    raw_t.as_ref(),
                    norm_t.as_ref(),
                    annot_t.as_ref(),
                    duration_ms.as_str(),
                    speaker_t.as_ref(),
                    if seg.verified { "1" } else { "0" },
                    confidence.as_str(),
                    ctc_score.as_str(),
                    clipping_ratio.as_str(),
                    rms_db.as_str(),
                    snr_db.as_str(),
                    seg.split.as_deref().unwrap_or(""),
                    ood_score.as_str(),
                    verdict_t.as_ref(),
                    seg.alignment_quality.as_deref().unwrap_or(""),
                    export_sample_rate.as_str(),
                    export_format,
                ])?;
            }

            wtr.flush()?;
            drop(wtr);
            replace_file(&tmp_path, &metadata_path)
                .map_err(|e| AppError::Other(format!("Failed to promote audio export metadata: {e}")))?;
            Ok(())
        })(),
    )
}

fn optional_f64(value: Option<f64>) -> String {
    value.map(|number| number.to_string()).unwrap_or_default()
}

fn resample_pcm_i16(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
    if from_rate == to_rate {
        return samples.to_vec();
    }
    let f32_samples: Vec<f32> = samples.iter().map(|&sample| sample as f32 / i16::MAX as f32).collect();
    audio::resample(&f32_samples, from_rate, to_rate)
        .into_iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect()
}

fn temporary_output_path(output_path: &Path) -> PathBuf {
    let file_name = output_path.file_name().and_then(|name| name.to_str()).unwrap_or("export.wav");
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or(0);
    output_path.with_file_name(format!("{file_name}.tmp-{}-{nonce}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, SpeechSegment};
    use std::fs;
    use tempfile::TempDir;

    fn make_wav_file(path: &Path) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..16000 {
            let sample = (i as f32 * 0.1).sin();
            writer.write_sample((sample * i16::MAX as f32) as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn insert_test_segment(db: &Database, id: &str, wav_path: &Path) {
        let seg = SpeechSegment {
            id: id.to_string(),
            created_at: None,
            audio_path: wav_path.to_string_lossy().to_string(),
            raw_transcript: "hello".to_string(),
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
        };
        db.insert_segment(&seg).unwrap();
    }

    fn output_wav_info(path: &Path) -> (u32, u32) {
        let reader = hound::WavReader::open(path).unwrap();
        let spec = reader.spec();
        (spec.sample_rate, reader.duration())
    }

    #[test]
    fn metadata_csv_reduces_source_path_to_basename_never_leaking_absolute_path() {
        // The shared metadata.csv must publish only the source BASENAME, never the curator's absolute
        // path (which leaks the OS username + directory layout), exactly like the JSON/JSONL/CSV/Parquet/HF
        // exporters. Test write_metadata_csv directly so no fixture audio is needed.
        let tmp = TempDir::new().unwrap();
        let seg = SpeechSegment {
            id: "s1".to_string(),
            // A fake absolute path with a username + dir layout. Deliberately NOT a Windows per-user
            // profile path: that form is itself a repo-hygiene-gate violation (the gate can't tell a fake
            // fixture username from a real one), so this uses the C:\Recordings\ convention that
            // export.rs::exports_never_leak_absolute_paths already uses.
            audio_path: "C:\\Recordings\\studio_user\\private_clips\\clip_001.wav".to_string(),
            raw_transcript: "hello".to_string(),
            ..SpeechSegment::default()
        };
        let exported = vec![ExportedAudioFile { filename: "ep_s1.wav".to_string(), segment: seg }];
        write_metadata_csv(tmp.path(), &exported, &AudioExportOptions::default()).unwrap();
        let csv = fs::read_to_string(tmp.path().join("metadata.csv")).unwrap();
        assert!(!csv.contains("studio_user"), "absolute path leaked the OS username:\n{csv}");
        assert!(!csv.contains("private_clips"), "absolute path leaked the directory layout:\n{csv}");
        assert!(csv.contains("clip_001.wav"), "the source basename must be published as provenance:\n{csv}");
    }

    #[test]
    fn test_export_audio_segment() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        insert_test_segment(&db, "exp1", &wav_path);

        let out_dir = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .unwrap();

        assert_eq!(result.succeeded, 1);
        assert!(out_dir.join(&result.files[0]).exists());
    }

    #[test]
    fn test_export_writes_metadata_when_requested() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp1", &wav_path);

        let out_dir = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 24_000,
                include_metadata: true,
            },
        )
        .unwrap();

        assert_eq!(result.succeeded, 1);
        assert!(result.files.contains(&"metadata.csv".to_string()));
        let metadata_path = out_dir.join("metadata.csv");
        assert!(metadata_path.exists());
        assert!(!metadata_path.with_extension("csv.tmp").exists());

        let mut reader = csv::Reader::from_path(metadata_path).unwrap();
        let headers = reader.headers().unwrap().clone();
        let row = reader.records().next().unwrap().unwrap();
        assert_eq!(metadata_value(&headers, &row, "file_name"), "test_exp1.wav");
        assert_eq!(metadata_value(&headers, &row, "segment_id"), "exp1");
        assert_eq!(metadata_value(&headers, &row, "raw_transcript"), "hello");
        assert_eq!(metadata_value(&headers, &row, "verified"), "0");
        assert_eq!(metadata_value(&headers, &row, "export_sample_rate"), "24000");
        assert_eq!(metadata_value(&headers, &row, "export_format"), "wav");
        assert!(reader.records().next().is_none());
    }

    #[test]
    fn test_export_skips_metadata_when_disabled() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp1", &wav_path);

        let out_dir = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16_000,
                include_metadata: false,
            },
        )
        .unwrap();

        assert_eq!(result.files, vec!["test_exp1.wav"]);
        assert!(!out_dir.join("metadata.csv").exists());
    }

    #[test]
    fn test_export_audio_segment_flac() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp1", &wav_path);

        let out_dir = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Flac,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .unwrap();

        assert_eq!(result.succeeded, 1);
        let flac_file = out_dir.join(&result.files[0]);
        assert!(flac_file.exists());

        // Verify it can be decoded back to PCM using the app's decoder
        let (sample_rate, samples) = crate::audio::decode_to_pcm(&flac_file).unwrap();
        assert_eq!(sample_rate, 16000);
        assert!(!samples.is_empty());
    }

    #[test]
    fn test_export_replaces_existing_file_and_cleans_temp() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp1", &wav_path);

        let out_dir = tmp.path().join("out");
        fs::create_dir_all(&out_dir).unwrap();
        let output_path = out_dir.join("test_exp1.wav");
        fs::write(&output_path, b"stale partial export").unwrap();

        let result = export_audio_segments(
            &db,
            &["exp1".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 16000,
                include_metadata: true,
            },
        )
        .unwrap();

        assert_eq!(result.succeeded, 1);
        assert_eq!(result.files, vec!["test_exp1.wav", "metadata.csv"]);
        assert_ne!(fs::read(&output_path).unwrap(), b"stale partial export");
        let (sample_rate, sample_count) = output_wav_info(&output_path);
        assert_eq!(sample_rate, 16000);
        assert_eq!(sample_count, 16000);
        let temp_left = fs::read_dir(&out_dir)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains(".tmp-"));
        assert!(!temp_left, "temporary export file should be promoted or removed");
    }

    #[test]
    fn test_export_resamples_to_requested_sample_rate() {
        let tmp = TempDir::new().unwrap();
        let wav_path = tmp.path().join("test.wav");
        make_wav_file(&wav_path);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        insert_test_segment(&db, "exp24", &wav_path);

        let out_dir = tmp.path().join("out");
        let result = export_audio_segments(
            &db,
            &["exp24".to_string()],
            &AudioExportOptions {
                output_dir: out_dir.to_string_lossy().to_string(),
                format: AudioExportFormat::Wav,
                sample_rate: 24000,
                include_metadata: true,
            },
        )
        .unwrap();

        assert_eq!(result.succeeded, 1);
        let (sample_rate, sample_count) = output_wav_info(&out_dir.join(&result.files[0]));
        assert_eq!(sample_rate, 24000);
        assert!(
            (23900..=24100).contains(&sample_count),
            "resampled one-second clip should have about 24000 samples, got {sample_count}"
        );
    }

    #[test]
    fn test_export_nonexistent_segment() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        let out_dir = TempDir::new().unwrap();
        let result = export_audio_segments(
            &db,
            &["nonexistent".to_string()],
            &AudioExportOptions { output_dir: out_dir.path().to_string_lossy().to_string(), ..Default::default() },
        )
        .unwrap();

        assert_eq!(result.failed, 1);
    }

    fn metadata_value<'a>(headers: &csv::StringRecord, row: &'a csv::StringRecord, name: &str) -> &'a str {
        let index = headers.iter().position(|header| header == name).unwrap();
        row.get(index).unwrap()
    }
}
