//! Purpose-bound final ASR + TTS dataset export for a completed sequential campaign.
//!
//! Legacy exporters intentionally remain blocked: they read the mutable first-pass row. This module
//! reads only immutable adjudications, requires genuine recording rights, preserves the 24 kHz TTS
//! master bytes, emits a separate 16 kHz ASR view, and publishes by one same-parent atomic rename.

use crate::db::{Database, RecordingRights};
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const EXPORT_SCHEMA_VERSION: u32 = 1;
const TTS_MASTER_SAMPLE_RATE: u32 = 24_000;
const ASR_SAMPLE_RATE: u32 = 16_000;
const PROVEN_LEGACY_OMNIASR_7B_MODEL_ID: &str = "omniasr-7b-legacy-c348ade8a816";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProductionDatasetOptions {
    pub output_dir: String,
    pub voice_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProductionDatasetResult {
    pub output_dir: String,
    pub campaign_id: String,
    pub voice_name: String,
    pub retained_segments: usize,
    pub rejected_segments: usize,
    pub total_duration_ms: i64,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone)]
struct FinalRow {
    ordinal: i64,
    segment_id: String,
    audio_path: String,
    raw_transcript: String,
    duration_ms: i64,
    speaker_id: Option<String>,
    model_version_id: Option<String>,
    alignment_json: Option<String>,
    audio_content_hash: Option<String>,
    final_action: String,
    final_transcript: Option<String>,
    resolution_kind: String,
    first_review_event_id: i64,
    second_decision_id: i64,
    rights: RecordingRights,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RightsRecord<'a> {
    segment_id: &'a str,
    license: &'a str,
    consent_basis: &'a str,
    permitted_use: &'a str,
    attribution: Option<&'a str>,
    source: &'a str,
}

fn nonblank(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|value| !value.is_empty())
}

fn permission_tokens(value: &str) -> HashSet<String> {
    value
        .to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn validate_training_and_tts_rights(segment_id: &str, rights: &RecordingRights) -> Result<(), String> {
    if nonblank(&rights.revoked_at).is_some() {
        return Err(format!("{segment_id}: recording rights were revoked"));
    }
    let license = nonblank(&rights.license).ok_or_else(|| format!("{segment_id}: rights license is missing"))?;
    let basis =
        nonblank(&rights.consent_basis).ok_or_else(|| format!("{segment_id}: rights consent basis is missing"))?;
    let permitted = nonblank(&rights.permitted_use).ok_or_else(|| format!("{segment_id}: permitted use is missing"))?;
    let source = nonblank(&rights.source).ok_or_else(|| format!("{segment_id}: rights source is missing"))?;
    let tokens = permission_tokens(permitted);
    if !tokens.contains("train") && !tokens.contains("training") {
        return Err(format!("{segment_id}: permitted use does not explicitly authorize training"));
    }
    let tts = ["tts", "voice_clone", "voice-clone", "voice_cloning", "voice-cloning", "speech_synthesis"];
    if !tts.iter().any(|token| tokens.contains(*token)) {
        return Err(format!("{segment_id}: permitted use does not explicitly authorize TTS/voice synthesis"));
    }
    let _ = (license, basis, source);
    Ok(())
}

fn load_final_rows(db: &Database, campaign_id: &str) -> AppResult<Vec<FinalRow>> {
    let mut statement = db.connection().prepare(
        "SELECT focus.ordinal, segment.id, segment.audio_path, segment.raw_transcript,
                segment.duration_ms, segment.speaker_id, segment.model_version_id,
                segment.alignment_json, segment.audio_content_hash,
                adjudication.final_action, adjudication.final_transcript,
                adjudication.resolution_kind, adjudication.first_review_event_id,
                adjudication.second_decision_id,
                segment.rights_license, segment.rights_consent_basis,
                segment.rights_permitted_use, segment.rights_attribution,
                segment.rights_source, segment.rights_revoked_at
           FROM review_campaign_focus focus
           JOIN speech_segments segment ON segment.id = focus.segment_id
           JOIN review_campaign_adjudications adjudication
             ON adjudication.campaign_id = focus.campaign_id
            AND adjudication.segment_id = focus.segment_id
          WHERE focus.campaign_id = ?1
          ORDER BY focus.ordinal",
    )?;
    let rows = statement
        .query_map([campaign_id], |row| {
            Ok(FinalRow {
                ordinal: row.get(0)?,
                segment_id: row.get(1)?,
                audio_path: row.get(2)?,
                raw_transcript: row.get(3)?,
                duration_ms: row.get(4)?,
                speaker_id: row.get(5)?,
                model_version_id: row.get(6)?,
                alignment_json: row.get(7)?,
                audio_content_hash: row.get(8)?,
                final_action: row.get(9)?,
                final_transcript: row.get(10)?,
                resolution_kind: row.get(11)?,
                first_review_event_id: row.get(12)?,
                second_decision_id: row.get(13)?,
                rights: RecordingRights {
                    license: row.get(14)?,
                    consent_basis: row.get(15)?,
                    permitted_use: row.get(16)?,
                    attribution: row.get(17)?,
                    source: row.get(18)?,
                    revoked_at: row.get(19)?,
                },
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn sha256_file(path: &Path) -> AppResult<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

fn strict_source_span(row: &FinalRow) -> AppResult<(i64, i64)> {
    let raw = row
        .alignment_json
        .as_deref()
        .ok_or_else(|| AppError::Validation(format!("{}: TTS master export requires a source span", row.segment_id)))?;
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|error| AppError::Validation(format!("{}: invalid alignment JSON: {error}", row.segment_id)))?;
    let start = value
        .get("source_start_ms")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| AppError::Validation(format!("{}: source_start_ms is not an integer", row.segment_id)))?;
    let end = value
        .get("source_end_ms")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| AppError::Validation(format!("{}: source_end_ms is not an integer", row.segment_id)))?;
    if start < 0 || end <= start || (end - start - row.duration_ms).abs() > 1 {
        return Err(AppError::Validation(format!(
            "{}: TTS requires one complete chunked master whose source span matches its duration (span={start}..{end}, duration={})",
            row.segment_id, row.duration_ms
        )));
    }
    Ok((start, end))
}

fn validate_tts_master(row: &FinalRow) -> AppResult<hound::WavSpec> {
    let path = Path::new(&row.audio_path);
    if !path.is_file() {
        return Err(AppError::Validation(format!("{}: source WAV is missing: {}", row.segment_id, row.audio_path)));
    }
    let (source_start_ms, source_end_ms) = strict_source_span(row)?;
    let reader = hound::WavReader::open(path)
        .map_err(|error| AppError::Validation(format!("{}: source is not a readable WAV: {error}", row.segment_id)))?;
    let spec = reader.spec();
    let samples = reader.duration() as i64;
    let wav_duration_ms = samples.saturating_mul(1000) / i64::from(spec.sample_rate.max(1));
    if spec.sample_rate != TTS_MASTER_SAMPLE_RATE
        || spec.channels != 1
        || spec.sample_format != hound::SampleFormat::Int
        || spec.bits_per_sample != 16
        || (wav_duration_ms - (source_end_ms - source_start_ms)).abs() > 1
    {
        return Err(AppError::Validation(format!(
            "{}: TTS master must be complete mono 24 kHz PCM16 WAV; found {} Hz, {} channel(s), {:?}{}, {} ms",
            row.segment_id, spec.sample_rate, spec.channels, spec.sample_format, spec.bits_per_sample, wav_duration_ms
        )));
    }
    Ok(spec)
}

fn write_asr_wav(source: &Path, output: &Path) -> AppResult<i64> {
    let (sample_rate, samples) = crate::audio::decode_to_pcm(source)?;
    if sample_rate != ASR_SAMPLE_RATE || samples.is_empty() {
        return Err(AppError::Validation(format!(
            "ASR decode must yield non-empty mono {ASR_SAMPLE_RATE} Hz PCM; got {sample_rate} Hz / {} samples",
            samples.len()
        )));
    }
    let spec =
        hound::WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut writer = hound::WavWriter::create(output, spec)
        .map_err(|error| AppError::Other(format!("cannot create ASR WAV {}: {error}", output.display())))?;
    for sample in &samples {
        writer.write_sample(*sample).map_err(|error| AppError::Other(format!("cannot write ASR WAV: {error}")))?;
    }
    writer.finalize().map_err(|error| AppError::Other(format!("cannot finalize ASR WAV: {error}")))?;
    Ok((samples.len() as i64).saturating_mul(1000) / i64::from(sample_rate))
}

fn write_jsonl_line<T: Serialize>(writer: &mut BufWriter<File>, value: &T) -> AppResult<()> {
    serde_json::to_writer(&mut *writer, value).map_err(|error| AppError::Other(error.to_string()))?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn write_sha256sums(root: &Path, relative_files: &[String]) -> AppResult<()> {
    let mut writer = BufWriter::new(File::create(root.join("SHA256SUMS"))?);
    for relative in relative_files {
        let digest = sha256_file(&root.join(relative))?;
        writeln!(writer, "{digest}  {}", relative.replace('\\', "/"))?;
    }
    writer.flush()?;
    Ok(())
}

fn cleanup_staging(path: &Path) {
    if path.is_dir() {
        let _ = fs::remove_dir_all(path);
    }
}

fn sync_export_files(path: &Path) -> AppResult<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        if child.is_dir() {
            sync_export_files(&child)?;
        } else if child.is_file() {
            OpenOptions::new().write(true).open(&child)?.sync_all()?;
        }
    }
    Ok(())
}

/// Export one completed named voice as two explicit datasets. Nothing is published if any clip,
/// right, model identity, source byte, or adjudication fails validation.
pub fn export_finalized_voice_dataset(
    db: &Database,
    options: &ProductionDatasetOptions,
) -> AppResult<ProductionDatasetResult> {
    let policy = crate::review_campaign::require_finalized_production_export(db, "ASR/TTS production export")?;
    let voice_name = options.voice_name.trim();
    if voice_name.is_empty() || voice_name.chars().any(char::is_control) {
        return Err(AppError::Validation("voice name must be a non-blank printable label".to_string()));
    }
    let validated =
        crate::validation::input::validate_output_path(&options.output_dir).map_err(AppError::Validation)?;
    let output = PathBuf::from(validated);
    if output.exists() {
        return Err(AppError::Validation(format!(
            "production output already exists; choose a new empty destination: {}",
            output.display()
        )));
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| AppError::Validation("production output needs an explicit parent directory".to_string()))?;
    fs::create_dir_all(parent)?;
    let leaf = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::Validation("production output directory name is not valid Unicode".to_string()))?;
    let staging = parent.join(format!(".{leaf}.staging-{}", uuid::Uuid::new_v4().hyphenated()));
    if staging.exists() {
        return Err(AppError::Validation("production staging directory already exists".to_string()));
    }
    fs::create_dir(&staging)?;
    let result = (|| -> AppResult<ProductionDatasetResult> {
        let rows = load_final_rows(db, &policy.campaign_id)?;
        if rows.len() != policy.focus_segment_count {
            return Err(AppError::Validation(format!(
                "adjudicated focus changed before export: {}/{} rows",
                rows.len(),
                policy.focus_segment_count
            )));
        }
        let mut retained = Vec::new();
        let mut rejected = Vec::new();
        let mut source_paths = HashSet::new();
        for row in rows {
            if row.speaker_id.as_deref() != Some(voice_name) {
                return Err(AppError::Validation(format!(
                    "{}: speaker identity {:?} does not match requested voice {voice_name}",
                    row.segment_id, row.speaker_id
                )));
            }
            let model_id = row
                .model_version_id
                .as_deref()
                .ok_or_else(|| AppError::Validation(format!("{}: ASR model identity is missing", row.segment_id)))?;
            if model_id != PROVEN_LEGACY_OMNIASR_7B_MODEL_ID
                && !crate::registry::is_family_model(db, model_id, crate::deployment::OMNIASR_7B_FAMILY)?
            {
                return Err(AppError::Validation(format!(
                    "{}: draft model {model_id} is not proven OmniASR-7B",
                    row.segment_id
                )));
            }
            match row.final_action.as_str() {
                "reject" => rejected.push(row),
                "retain" => {
                    let text = row.final_transcript.as_deref().map(str::trim).filter(|text| !text.is_empty());
                    if text.is_none() || text.is_some_and(crate::quality::is_placeholder_transcript) {
                        return Err(AppError::Validation(format!(
                            "{}: final retained transcript is blank or placeholder",
                            row.segment_id
                        )));
                    }
                    validate_training_and_tts_rights(&row.segment_id, &row.rights).map_err(AppError::Validation)?;
                    validate_tts_master(&row)?;
                    let stored_pcm = row
                        .audio_content_hash
                        .as_deref()
                        .filter(|value| crate::db::is_canonical_audio_content_hash(value))
                        .ok_or_else(|| {
                            AppError::Validation(format!("{}: canonical PCM identity is missing", row.segment_id))
                        })?;
                    let current_pcm = crate::export_bundle::current_canonical_pcm_blake3(Path::new(&row.audio_path))?;
                    if current_pcm != stored_pcm {
                        return Err(AppError::Validation(format!(
                            "{}: source audio no longer matches its stored canonical PCM identity",
                            row.segment_id
                        )));
                    }
                    let canonical = fs::canonicalize(&row.audio_path)?;
                    if !source_paths.insert(canonical) {
                        return Err(AppError::Validation(format!(
                            "{}: more than one retained segment names the same TTS master",
                            row.segment_id
                        )));
                    }
                    retained.push(row);
                }
                other => {
                    return Err(AppError::Validation(format!(
                        "{}: unknown final adjudication action {other}",
                        row.segment_id
                    )))
                }
            }
        }
        fs::create_dir_all(staging.join("asr/audio_16k"))?;
        fs::create_dir_all(staging.join("tts/audio_24k_master"))?;
        let mut asr_meta = BufWriter::new(File::create(staging.join("asr/metadata.jsonl"))?);
        let mut tts_meta = BufWriter::new(File::create(staging.join("tts/metadata.jsonl"))?);
        let mut rights_meta = BufWriter::new(File::create(staging.join("rights.jsonl"))?);
        let mut exclusion_meta = BufWriter::new(File::create(staging.join("exclusions.jsonl"))?);
        let mut files = vec![
            "asr/metadata.jsonl".to_string(),
            "tts/metadata.jsonl".to_string(),
            "rights.jsonl".to_string(),
            "exclusions.jsonl".to_string(),
        ];
        let mut total_duration_ms = 0i64;
        let mut source_hashes = BTreeMap::new();
        for row in &retained {
            let file_name = format!("{:06}.wav", row.ordinal + 1);
            let source = Path::new(&row.audio_path);
            let source_sha = sha256_file(source)?;
            let tts_relative = format!("tts/audio_24k_master/{file_name}");
            let tts_path = staging.join(&tts_relative);
            fs::copy(source, &tts_path)?;
            let copied_sha = sha256_file(&tts_path)?;
            let source_after_sha = sha256_file(source)?;
            if copied_sha != source_sha || source_after_sha != source_sha {
                return Err(AppError::Validation(format!(
                    "{}: source WAV changed during export or copied bytes differ",
                    row.segment_id
                )));
            }
            let asr_relative = format!("asr/audio_16k/{file_name}");
            let asr_path = staging.join(&asr_relative);
            let asr_duration_ms = write_asr_wav(source, &asr_path)?;
            if (asr_duration_ms - row.duration_ms).abs() > 1 {
                return Err(AppError::Validation(format!(
                    "{}: ASR resample duration drifted ({} vs {} ms)",
                    row.segment_id, asr_duration_ms, row.duration_ms
                )));
            }
            let asr_sha = sha256_file(&asr_path)?;
            let text = row.final_transcript.as_deref().unwrap_or_default().trim();
            let normalized = crate::normalizer::canonical_training_text(text);
            let license = nonblank(&row.rights.license).unwrap_or_default();
            let consent = nonblank(&row.rights.consent_basis).unwrap_or_default();
            let permitted = nonblank(&row.rights.permitted_use).unwrap_or_default();
            let rights_source = nonblank(&row.rights.source).unwrap_or_default();
            write_jsonl_line(
                &mut asr_meta,
                &serde_json::json!({
                    "id": row.segment_id,
                    "audio": format!("audio_16k/{file_name}"),
                    "text": text,
                    "speaker": voice_name,
                    "durationMs": asr_duration_ms,
                    "sampleRate": ASR_SAMPLE_RATE,
                    "modelVersionId": row.model_version_id,
                    "championRawTranscript": row.raw_transcript,
                    "resolutionKind": row.resolution_kind,
                    "firstReviewEventId": row.first_review_event_id,
                    "secondDecisionId": row.second_decision_id,
                    "audioSha256": asr_sha,
                    "sourceMasterSha256": source_sha,
                    "sourcePcmIdentity": row.audio_content_hash,
                }),
            )?;
            write_jsonl_line(
                &mut tts_meta,
                &serde_json::json!({
                    "id": row.segment_id,
                    "audio": format!("audio_24k_master/{file_name}"),
                    "verbatimText": text,
                    "normalizedText": normalized,
                    "speaker": voice_name,
                    "durationMs": row.duration_ms,
                    "sampleRate": TTS_MASTER_SAMPLE_RATE,
                    "sourceBytesPreserved": true,
                    "audioSha256": source_sha,
                    "resolutionKind": row.resolution_kind,
                    "firstReviewEventId": row.first_review_event_id,
                    "secondDecisionId": row.second_decision_id,
                }),
            )?;
            write_jsonl_line(
                &mut rights_meta,
                &RightsRecord {
                    segment_id: &row.segment_id,
                    license,
                    consent_basis: consent,
                    permitted_use: permitted,
                    attribution: nonblank(&row.rights.attribution),
                    source: rights_source,
                },
            )?;
            files.push(asr_relative);
            files.push(tts_relative);
            source_hashes.insert(row.segment_id.clone(), source_sha);
            total_duration_ms = total_duration_ms.saturating_add(row.duration_ms);
        }
        for row in &rejected {
            write_jsonl_line(
                &mut exclusion_meta,
                &serde_json::json!({
                    "id": row.segment_id,
                    "reason": "human_reject_after_independent_review",
                    "resolutionKind": row.resolution_kind,
                    "firstReviewEventId": row.first_review_event_id,
                    "secondDecisionId": row.second_decision_id,
                }),
            )?;
        }
        asr_meta.flush()?;
        tts_meta.flush()?;
        rights_meta.flush()?;
        exclusion_meta.flush()?;
        drop(asr_meta);
        drop(tts_meta);
        drop(rights_meta);
        drop(exclusion_meta);

        // Re-prove every mutable external fact after the expensive copy/resample work and before the
        // atomic rename. Adjudications are immutable, but rights and source bytes may be revoked or
        // replaced while export is running.
        let current_policy =
            crate::review_campaign::require_finalized_production_export(db, "ASR/TTS production export commit")?;
        if current_policy != policy {
            return Err(AppError::Validation("campaign authority changed during production export".to_string()));
        }
        for row in &retained {
            let current_rights = db.rights_for_segment(&row.segment_id)?;
            if current_rights != row.rights {
                return Err(AppError::Validation(format!(
                    "{}: rights changed during production export",
                    row.segment_id
                )));
            }
            if db.segment_audio_content_hash(&row.segment_id)? != row.audio_content_hash {
                return Err(AppError::Validation(format!(
                    "{}: stored canonical PCM identity changed during production export",
                    row.segment_id
                )));
            }
            let source_sha = sha256_file(Path::new(&row.audio_path))?;
            if source_hashes.get(&row.segment_id) != Some(&source_sha) {
                return Err(AppError::Validation(format!(
                    "{}: source audio changed before production commit",
                    row.segment_id
                )));
            }
        }
        let manifest = serde_json::json!({
            "schemaVersion": EXPORT_SCHEMA_VERSION,
            "campaignId": policy.campaign_id,
            "focusSha256": policy.focus_sha256,
            "voiceName": voice_name,
            "createdAtMs": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0),
            "appGitSha": crate::GIT_SHA,
            "retainedSegments": retained.len(),
            "rejectedSegments": rejected.len(),
            "totalDurationMs": total_duration_ms,
            "transcriptAuthority": "immutable independent-review adjudication",
            "draftModelFamily": crate::deployment::OMNIASR_7B_FAMILY,
            "asr": {"directory": "asr", "sampleRate": ASR_SAMPLE_RATE, "audio": "mono PCM16 WAV"},
            "tts": {"directory": "tts", "sampleRate": TTS_MASTER_SAMPLE_RATE,
                    "audio": "byte-preserved mono PCM16 WAV masters"},
            "rights": {"file": "rights.jsonl", "requires": ["license", "consentBasis", "source", "train", "tts"]},
            "exclusions": {"file": "exclusions.jsonl", "rejectedAudioCopied": false},
        });
        let manifest_path = staging.join("manifest.json");
        fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest).map_err(|e| AppError::Other(e.to_string()))?)?;
        files.push("manifest.json".to_string());
        files.sort();
        write_sha256sums(&staging, &files)?;
        let manifest_sha256 = sha256_file(&manifest_path)?;
        let sums_sha256 = sha256_file(&staging.join("SHA256SUMS"))?;
        fs::write(
            staging.join("_COMPLETE.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schemaVersion": EXPORT_SCHEMA_VERSION,
                "manifestSha256": manifest_sha256,
                "sha256sumsSha256": sums_sha256,
            }))
            .map_err(|e| AppError::Other(e.to_string()))?,
        )?;
        // A rename is atomic for readers but does not by itself make buffered file contents durable.
        // Flush every published byte first so a power loss cannot leave a visible, half-persisted pack.
        sync_export_files(&staging)?;
        fs::rename(&staging, &output)?;
        Ok(ProductionDatasetResult {
            output_dir: output.to_string_lossy().to_string(),
            campaign_id: policy.campaign_id,
            voice_name: voice_name.to_string(),
            retained_segments: retained.len(),
            rejected_segments: rejected.len(),
            total_duration_ms,
            manifest_sha256,
        })
    })();
    if result.is_err() {
        cleanup_staging(&staging);
    }
    result
}
