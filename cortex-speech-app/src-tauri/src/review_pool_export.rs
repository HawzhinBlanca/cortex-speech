//! Deterministic, crash-safe ASR/TTS export for one completed flexible-pool voice.
//!
//! This authority is deliberately separate from the legacy sequential-campaign exporter. It accepts
//! only a fully resolved v64 voice, exact owner rights, the pool-bound OmniASR-7B identity, and audio
//! bytes that still reproduce the canonical PCM identity captured when the pool was activated.

use crate::db::{Database, RecordingRights};
use crate::error::{AppError, AppResult};
use crate::review_pool::{self, SegmentResolution};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const POOL_EXPORT_SCHEMA_VERSION: u32 = 2;
const TTS_SAMPLE_RATE: u32 = 24_000;
const ASR_SAMPLE_RATE: u32 = 16_000;

#[cfg(test)]
thread_local! {
    // Rust executes tests in parallel inside one process. A process-global fault flag lets an
    // unrelated export consume another test's injected crash, making the crash-safety proof flaky.
    // Export is synchronous, so thread-local one-shot injection models the exact call boundary while
    // keeping parallel fixtures isolated.
    static FAIL_AFTER_PUBLICATION_BEFORE_CERTIFICATION: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
fn arm_publication_crash() {
    FAIL_AFTER_PUBLICATION_BEFORE_CERTIFICATION.with(|flag| flag.set(true));
}

#[cfg(test)]
fn take_publication_crash() -> bool {
    FAIL_AFTER_PUBLICATION_BEFORE_CERTIFICATION.with(|flag| flag.replace(false))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PoolDatasetOptions {
    pub output_dir: String,
    pub voice_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PoolDatasetResult {
    pub output_dir: String,
    pub pool_id: String,
    pub voice_name: String,
    pub retained_segments: usize,
    pub rejected_segments: usize,
    pub total_duration_ms: i64,
    pub manifest_sha256: String,
    pub sha256sums_sha256: String,
    pub certificate_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PoolExportRow {
    segment_id: String,
    audio_path: String,
    raw_transcript: String,
    model_version_id: String,
    audio_content_hash: String,
    source_start_ms: i64,
    source_end_ms: i64,
    duration_ms: i64,
    rights: RecordingRights,
    resolution: SegmentResolution,
}

fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn sha256_file(path: &Path) -> AppResult<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

fn exact_owner_rights(segment_id: &str, rights: &RecordingRights) -> AppResult<()> {
    let exact = rights.license.as_deref() == Some(review_pool::OWNER_RIGHTS_LICENSE)
        && rights.consent_basis.as_deref() == Some(review_pool::OWNER_RIGHTS_CONSENT)
        && rights.permitted_use.as_deref() == Some(review_pool::OWNER_RIGHTS_PERMITTED_USE)
        && rights.attribution.as_deref() == Some(review_pool::OWNER_RIGHTS_ATTRIBUTION)
        && rights.source.as_deref() == Some(review_pool::OWNER_RIGHTS_SOURCE)
        && rights.revoked_at.as_deref().map(str::trim).unwrap_or_default().is_empty();
    if !exact {
        return Err(AppError::Validation(format!(
            "{segment_id}: exact owner-supplied recording rights are missing, conflicting, or revoked"
        )));
    }
    Ok(())
}

fn load_rows(db: &Database, pool_id: &str, voice_name: &str) -> AppResult<Vec<PoolExportRow>> {
    let resolutions: HashMap<String, SegmentResolution> = review_pool::segment_resolutions(db, Some(voice_name))
        .map_err(AppError::Validation)?
        .into_iter()
        .map(|row| (row.segment_id.clone(), row))
        .collect();
    if resolutions.is_empty() {
        return Err(AppError::Validation(format!("active review pool has no voice named {voice_name}")));
    }
    let mut statement = db.connection().prepare(
        "SELECT member.segment_id, segment.audio_path, member.raw_transcript,
                member.model_version_id, member.audio_content_hash, member.source_start_ms,
                member.source_end_ms, member.duration_ms, segment.rights_license,
                segment.rights_consent_basis, segment.rights_permitted_use,
                segment.rights_attribution, segment.rights_source, segment.rights_revoked_at
           FROM review_pool_members member
           JOIN speech_segments segment ON segment.id=member.segment_id
          WHERE member.pool_id=?1 AND member.voice_name=?2 COLLATE BINARY
            AND NOT EXISTS (
                SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                 WHERE exclusion.pool_id=member.pool_id AND exclusion.segment_id=member.segment_id
            )
          ORDER BY member.segment_id",
    )?;
    let rows = statement
        .query_map(rusqlite::params![pool_id, voice_name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                RecordingRights {
                    license: row.get(8)?,
                    consent_basis: row.get(9)?,
                    permitted_use: row.get(10)?,
                    attribution: row.get(11)?,
                    source: row.get(12)?,
                    revoked_at: row.get(13)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut result = Vec::with_capacity(rows.len());
    for (segment_id, audio_path, raw_transcript, model_version_id, audio_content_hash, start, end, duration, rights) in
        rows
    {
        let resolution = resolutions.get(&segment_id).cloned().ok_or_else(|| {
            AppError::Validation(format!("{segment_id}: resolution authority disappeared during export setup"))
        })?;
        result.push(PoolExportRow {
            segment_id,
            audio_path,
            raw_transcript,
            model_version_id,
            audio_content_hash,
            source_start_ms: start,
            source_end_ms: end,
            duration_ms: duration,
            rights,
            resolution,
        });
    }
    if result.len() != resolutions.len() {
        return Err(AppError::Validation("voice membership and resolution counts disagree".to_string()));
    }
    Ok(result)
}

fn strict_sample_index(ms: i64, sample_rate: u32, label: &str) -> AppResult<usize> {
    if ms < 0 {
        return Err(AppError::Validation(format!("{label} cannot be negative")));
    }
    let scaled = i128::from(ms) * i128::from(sample_rate);
    if scaled % 1000 != 0 {
        return Err(AppError::Validation(format!("{label} does not land on an exact sample boundary")));
    }
    usize::try_from(scaled / 1000).map_err(|_| AppError::Validation(format!("{label} exceeds addressable audio")))
}

fn read_master(path: &Path) -> AppResult<Vec<i16>> {
    if !path.is_file() {
        return Err(AppError::Validation(format!("source WAV is missing: {}", path.display())));
    }
    let mut reader = hound::WavReader::open(path)
        .map_err(|error| AppError::Validation(format!("source is not a readable WAV {}: {error}", path.display())))?;
    let spec = reader.spec();
    if spec.sample_rate != TTS_SAMPLE_RATE
        || spec.channels != 1
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err(AppError::Validation(format!(
            "TTS source must be mono 24 kHz PCM16 WAV; {} is {} Hz, {} channel(s), {:?}{}",
            path.display(),
            spec.sample_rate,
            spec.channels,
            spec.sample_format,
            spec.bits_per_sample
        )));
    }
    let samples = reader
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| AppError::Validation(format!("cannot decode PCM16 source {}: {error}", path.display())))?;
    if samples.is_empty() {
        return Err(AppError::Validation(format!("source WAV has no samples: {}", path.display())));
    }
    Ok(samples)
}

fn write_pcm16_wav(path: &Path, sample_rate: u32, samples: &[i16]) -> AppResult<()> {
    if samples.is_empty() {
        return Err(AppError::Validation(format!("refusing to write an empty WAV: {}", path.display())));
    }
    let spec =
        hound::WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|error| AppError::Other(format!("cannot create WAV {}: {error}", path.display())))?;
    for sample in samples {
        writer
            .write_sample(*sample)
            .map_err(|error| AppError::Other(format!("cannot write WAV {}: {error}", path.display())))?;
    }
    writer.finalize().map_err(|error| AppError::Other(format!("cannot finalize WAV {}: {error}", path.display())))?;
    Ok(())
}

fn write_jsonl<T: Serialize>(writer: &mut BufWriter<File>, value: &T) -> AppResult<()> {
    serde_json::to_writer(&mut *writer, value).map_err(|error| AppError::Other(error.to_string()))?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn sync_tree(path: &Path) -> AppResult<()> {
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        if child.is_dir() {
            sync_tree(&child)?;
        } else if child.is_file() {
            OpenOptions::new().write(true).open(child)?.sync_all()?;
        }
    }
    Ok(())
}

fn tree_inventory(root: &Path) -> AppResult<BTreeMap<String, bool>> {
    fn walk(root: &Path, directory: &Path, inventory: &mut BTreeMap<String, bool>) -> AppResult<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
                return Err(AppError::Validation(format!(
                    "export tree contains an unsupported filesystem entry: {}",
                    path.display()
                )));
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| AppError::Validation("export inventory escaped its root".to_string()))?
                .to_str()
                .ok_or_else(|| AppError::Validation("export inventory contains a non-Unicode path".to_string()))?
                .replace('\\', "/");
            inventory.insert(relative, kind.is_dir());
            if kind.is_dir() {
                walk(root, &path, inventory)?;
            }
        }
        Ok(())
    }

    let mut inventory = BTreeMap::new();
    walk(root, root, &mut inventory)?;
    Ok(inventory)
}

fn verify_exact_tree(expected: &Path, published: &Path) -> AppResult<()> {
    if !published.is_dir() {
        return Err(AppError::Validation(format!(
            "existing export destination is not a directory: {}",
            published.display()
        )));
    }
    let expected_inventory = tree_inventory(expected)?;
    let published_inventory = tree_inventory(published)?;
    if published_inventory != expected_inventory {
        return Err(AppError::Validation(format!(
            "existing export destination has a different file inventory: {}",
            published.display()
        )));
    }
    for (relative, is_directory) in expected_inventory {
        if !is_directory && sha256_file(&expected.join(&relative))? != sha256_file(&published.join(&relative))? {
            return Err(AppError::Validation(format!(
                "existing export destination differs at {relative}: {}",
                published.display()
            )));
        }
    }
    Ok(())
}

fn write_sha256sums(root: &Path, relative_files: &[String]) -> AppResult<String> {
    let path = root.join("SHA256SUMS");
    let mut writer = BufWriter::new(File::create(&path)?);
    for relative in relative_files {
        writeln!(writer, "{}  {}", sha256_file(&root.join(relative))?, relative.replace('\\', "/"))?;
    }
    writer.flush()?;
    drop(writer);
    sha256_file(&path)
}

fn remove_staging(path: &Path) {
    if path.is_dir() {
        let _ = fs::remove_dir_all(path);
    }
}

/// Export and certify one independently completed voice. The caller must hold the Cortex instance
/// lock; `pool_admin export` enforces that requirement so review evidence cannot change mid-export.
pub fn export_voice(db: &Database, options: &PoolDatasetOptions) -> AppResult<PoolDatasetResult> {
    let pool = review_pool::load(db)
        .map_err(AppError::Validation)?
        .ok_or_else(|| AppError::Validation("review pool is not active".to_string()))?;
    let dedup = review_pool::dedup_status(db).map_err(AppError::Validation)?;
    let dedup_manifest_sha256 = dedup
        .manifest_sha256
        .as_deref()
        .filter(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| {
            AppError::Validation("review-pool export requires an applied immutable dedup manifest".to_string())
        })?;
    let dedup_algorithm_id = dedup.algorithm_id.as_deref().ok_or_else(|| {
        AppError::Validation("review-pool export requires a bound duplicate-detection algorithm".to_string())
    })?;
    if !dedup.applied
        || dedup_algorithm_id != "cortex-cross-file-waveform-correlation-v1"
        || dedup.unconfirmed_risk_count != 0
        || dedup.source_segment_count != pool.focus_segment_count
        || dedup.canonical_segment_count != pool.review_segment_count
        || dedup.excluded_segment_count != pool.excluded_duplicate_count
        || dedup.duplicate_family_count != pool.duplicate_family_count
        || pool.dedup_manifest_sha256.as_deref() != Some(dedup_manifest_sha256)
    {
        return Err(AppError::Validation(
            "review-pool export duplicate authority does not match the immutable active pool".to_string(),
        ));
    }
    let voice_name = options.voice_name.trim();
    if voice_name.is_empty() || voice_name.chars().any(char::is_control) {
        return Err(AppError::Validation("voice name must be a non-blank printable label".to_string()));
    }
    let output_value =
        crate::validation::input::validate_output_path(&options.output_dir).map_err(AppError::Validation)?;
    let output = PathBuf::from(output_value);
    if output.exists() && !output.is_dir() {
        return Err(AppError::Validation(format!(
            "export destination already exists and is not a directory: {}",
            output.display()
        )));
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| AppError::Validation("export destination needs an explicit parent".to_string()))?;
    fs::create_dir_all(parent)?;
    let leaf = output
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| AppError::Validation("export destination name is not valid Unicode".to_string()))?;
    let staging = parent.join(format!(".{leaf}.staging-{}", uuid::Uuid::new_v4().hyphenated()));
    fs::create_dir(&staging)?;

    let result = (|| -> AppResult<PoolDatasetResult> {
        let rows = load_rows(db, &pool.pool_id, voice_name)?;
        if rows.iter().any(|row| !matches!(row.resolution.status.as_str(), "resolved" | "ownerResolved")) {
            return Err(AppError::Validation(format!("voice {voice_name} is not fully resolved")));
        }
        for row in &rows {
            if row.model_version_id != pool.champion_model_version_id {
                return Err(AppError::Validation(format!(
                    "{}: pool draft is not the bound OmniASR-7B champion",
                    row.segment_id
                )));
            }
            exact_owner_rights(&row.segment_id, &row.rights)?;
            match row.resolution.final_action.as_deref() {
                Some("reject") if row.resolution.final_transcript.is_none() => {}
                Some("retain") => {
                    let text = row.resolution.final_transcript.as_deref().map(str::trim).unwrap_or_default();
                    if text.is_empty() || crate::quality::is_placeholder_transcript(text) {
                        return Err(AppError::Validation(format!(
                            "{}: retained transcript is blank or a placeholder",
                            row.segment_id
                        )));
                    }
                }
                _ => return Err(AppError::Validation(format!("{}: resolved outcome is malformed", row.segment_id))),
            }
        }
        let authority = review_pool::voice_authority_digests(db, voice_name).map_err(AppError::Validation)?;

        let mut rights_digest = Sha256::new();
        hash_field(&mut rights_digest, voice_name.as_bytes());
        for row in &rows {
            hash_field(&mut rights_digest, row.segment_id.as_bytes());
            for value in [
                review_pool::OWNER_RIGHTS_LICENSE,
                review_pool::OWNER_RIGHTS_CONSENT,
                review_pool::OWNER_RIGHTS_PERMITTED_USE,
                review_pool::OWNER_RIGHTS_ATTRIBUTION,
                review_pool::OWNER_RIGHTS_SOURCE,
            ] {
                hash_field(&mut rights_digest, value.as_bytes());
            }
        }
        let rights_sha256: String = rights_digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect();

        fs::create_dir_all(staging.join("asr/audio_16k"))?;
        fs::create_dir_all(staging.join("tts/audio_24k"))?;
        let mut asr_meta = BufWriter::new(File::create(staging.join("asr/metadata.jsonl"))?);
        let mut tts_meta = BufWriter::new(File::create(staging.join("tts/metadata.jsonl"))?);
        let mut rights_meta = BufWriter::new(File::create(staging.join("rights.jsonl"))?);
        let mut exclusions = BufWriter::new(File::create(staging.join("exclusions.jsonl"))?);
        let mut files = vec![
            "asr/metadata.jsonl".to_string(),
            "tts/metadata.jsonl".to_string(),
            "rights.jsonl".to_string(),
            "exclusions.jsonl".to_string(),
        ];
        let retained_rows: Vec<&PoolExportRow> =
            rows.iter().filter(|row| row.resolution.final_action.as_deref() == Some("retain")).collect();
        let rejected_rows: Vec<&PoolExportRow> =
            rows.iter().filter(|row| row.resolution.final_action.as_deref() == Some("reject")).collect();
        let ordinals: HashMap<&str, usize> =
            retained_rows.iter().enumerate().map(|(index, row)| (row.segment_id.as_str(), index + 1)).collect();
        let mut by_source: BTreeMap<&str, Vec<&PoolExportRow>> = BTreeMap::new();
        for row in &rows {
            write_jsonl(
                &mut rights_meta,
                &serde_json::json!({
                    "id": row.segment_id,
                    "included": row.resolution.final_action.as_deref() == Some("retain"),
                    "license": review_pool::OWNER_RIGHTS_LICENSE,
                    "consentBasis": review_pool::OWNER_RIGHTS_CONSENT,
                    "permittedUse": review_pool::OWNER_RIGHTS_PERMITTED_USE,
                    "attribution": review_pool::OWNER_RIGHTS_ATTRIBUTION,
                    "source": review_pool::OWNER_RIGHTS_SOURCE,
                }),
            )?;
            by_source.entry(&row.audio_path).or_default().push(row);
        }
        let mut audio_digest = Sha256::new();
        hash_field(&mut audio_digest, voice_name.as_bytes());
        let mut source_sha_by_path = HashMap::new();
        let mut total_duration_ms = 0_i64;
        for (source_value, source_rows) in by_source {
            let source = Path::new(source_value);
            if !source.is_file() {
                return Err(AppError::Validation(format!("source WAV is missing: {}", source.display())));
            }
            let source_sha = sha256_file(source)?;
            let current_pcm = crate::export_bundle::current_canonical_pcm_blake3(source)?;
            if source_rows.iter().any(|row| row.audio_content_hash != current_pcm) {
                return Err(AppError::Validation(format!(
                    "{}: source audio no longer matches the pool-bound PCM identity",
                    source.display()
                )));
            }
            let master = read_master(source)?;
            source_sha_by_path.insert(source_value.to_string(), source_sha.clone());
            for row in source_rows {
                let start = strict_sample_index(row.source_start_ms, TTS_SAMPLE_RATE, "source_start_ms")?;
                let end = strict_sample_index(row.source_end_ms, TTS_SAMPLE_RATE, "source_end_ms")?;
                if end <= start
                    || end > master.len()
                    || (row.source_end_ms - row.source_start_ms - row.duration_ms).abs() > 1
                {
                    return Err(AppError::Validation(format!(
                        "{}: source span is outside its master or disagrees with duration",
                        row.segment_id
                    )));
                }
                hash_field(&mut audio_digest, row.segment_id.as_bytes());
                hash_field(&mut audio_digest, source_sha.as_bytes());
                hash_field(&mut audio_digest, row.audio_content_hash.as_bytes());
                hash_field(&mut audio_digest, row.source_start_ms.to_string().as_bytes());
                hash_field(&mut audio_digest, row.source_end_ms.to_string().as_bytes());
                if row.resolution.final_action.as_deref() == Some("reject") {
                    hash_field(&mut audio_digest, b"rejected-no-copy");
                    continue;
                }
                let clip = &master[start..end];
                let ordinal = ordinals.get(row.segment_id.as_str()).copied().ok_or_else(|| {
                    AppError::Validation(format!("{}: deterministic export ordinal is missing", row.segment_id))
                })?;
                let file_name = format!("{ordinal:06}.wav");
                let tts_relative = format!("tts/audio_24k/{file_name}");
                let tts_path = staging.join(&tts_relative);
                let source_bytes_preserved = start == 0 && end == master.len();
                if source_bytes_preserved {
                    fs::copy(source, &tts_path)?;
                } else {
                    write_pcm16_wav(&tts_path, TTS_SAMPLE_RATE, clip)?;
                }
                let tts_sha = sha256_file(&tts_path)?;
                if source_bytes_preserved && tts_sha != source_sha {
                    return Err(AppError::Validation(format!(
                        "{}: byte-preserved TTS copy differs from its source master",
                        row.segment_id
                    )));
                }
                let (_, asr_pcm) = crate::audio::ensure_pcm_16khz(TTS_SAMPLE_RATE, clip.to_vec())?;
                let asr_relative = format!("asr/audio_16k/{file_name}");
                let asr_path = staging.join(&asr_relative);
                write_pcm16_wav(&asr_path, ASR_SAMPLE_RATE, &asr_pcm)?;
                let asr_sha = sha256_file(&asr_path)?;
                let asr_duration_ms = (asr_pcm.len() as i64).saturating_mul(1000) / i64::from(ASR_SAMPLE_RATE);
                if (asr_duration_ms - row.duration_ms).abs() > 1 {
                    return Err(AppError::Validation(format!(
                        "{}: ASR duration changed during exact-span resampling",
                        row.segment_id
                    )));
                }
                let text = row.resolution.final_transcript.as_deref().unwrap_or_default().trim();
                write_jsonl(
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
                        "resolutionStatus": row.resolution.status,
                        "resolutionEvidenceSha256": row.resolution.evidence_sha256,
                        "audioSha256": asr_sha,
                        "sourceMasterSha256": source_sha,
                        "sourceStartMs": row.source_start_ms,
                        "sourceEndMs": row.source_end_ms,
                    }),
                )?;
                write_jsonl(
                    &mut tts_meta,
                    &serde_json::json!({
                        "id": row.segment_id,
                        "audio": format!("audio_24k/{file_name}"),
                        "verbatimText": text,
                        "normalizedText": crate::normalizer::canonical_training_text(text),
                        "speaker": voice_name,
                        "durationMs": row.duration_ms,
                        "sampleRate": TTS_SAMPLE_RATE,
                        "sourceBytesPreserved": source_bytes_preserved,
                        "audioSha256": tts_sha,
                        "sourceMasterSha256": source_sha,
                        "sourceStartMs": row.source_start_ms,
                        "sourceEndMs": row.source_end_ms,
                    }),
                )?;
                hash_field(&mut audio_digest, tts_sha.as_bytes());
                hash_field(&mut audio_digest, asr_sha.as_bytes());
                hash_field(&mut audio_digest, if source_bytes_preserved { b"preserved" } else { b"extracted" });
                total_duration_ms = total_duration_ms.saturating_add(row.duration_ms);
                files.push(tts_relative);
                files.push(asr_relative);
            }
        }
        for row in &rejected_rows {
            write_jsonl(
                &mut exclusions,
                &serde_json::json!({
                    "id": row.segment_id,
                    "reason": "human_reject_after_independent_review",
                    "resolutionStatus": row.resolution.status,
                    "resolutionEvidenceSha256": row.resolution.evidence_sha256,
                    "audioCopied": false,
                }),
            )?;
        }
        asr_meta.flush()?;
        tts_meta.flush()?;
        rights_meta.flush()?;
        exclusions.flush()?;
        drop(asr_meta);
        drop(tts_meta);
        drop(rights_meta);
        drop(exclusions);
        let audio_sha256: String = audio_digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect();

        let manifest = serde_json::json!({
            "schemaVersion": POOL_EXPORT_SCHEMA_VERSION,
            "poolId": pool.pool_id,
            "poolFocusSha256": pool.focus_sha256,
            "sourcePoolSegmentCount": dedup.source_segment_count,
            "canonicalReviewSegmentCount": dedup.canonical_segment_count,
            "excludedDuplicateSegmentCount": dedup.excluded_segment_count,
            "duplicateFamilyCount": dedup.duplicate_family_count,
            "dedupManifestSha256": dedup_manifest_sha256,
            "dedupAlgorithmId": dedup_algorithm_id,
            "dedupUnconfirmedRiskCount": dedup.unconfirmed_risk_count,
            "voiceName": voice_name,
            "championModelVersionId": pool.champion_model_version_id,
            "championDeploymentSha256": pool.champion_deployment_sha256,
            "resolutionSha256": authority.resolution_sha256,
            "reviewerSha256": authority.reviewer_sha256,
            "decisionAndReviewerEvidenceSha256": authority.reviewer_sha256,
            "rightsSha256": rights_sha256,
            "audioSha256": audio_sha256,
            "retainedSegments": retained_rows.len(),
            "rejectedSegments": rejected_rows.len(),
            "totalDurationMs": total_duration_ms,
            "transcriptAuthority": "two matching independent reviewers, matching pair among three, or owner adjudication",
            "asr": {"directory": "asr", "sampleRate": ASR_SAMPLE_RATE, "audio": "mono PCM16 WAV"},
            "tts": {"directory": "tts", "sampleRate": TTS_SAMPLE_RATE,
                    "audio": "byte-preserved masters or exact-sample bounded PCM16 extraction"},
            "exclusions": {"file": "exclusions.jsonl", "rejectedAudioCopied": false},
        });
        let manifest_bytes =
            serde_json::to_vec_pretty(&manifest).map_err(|error| AppError::Other(error.to_string()))?;
        fs::write(staging.join("manifest.json"), &manifest_bytes)?;
        files.push("manifest.json".to_string());
        files.sort_unstable();
        let manifest_sha256 = sha256_bytes(&manifest_bytes);
        let sha256sums_sha256 = write_sha256sums(&staging, &files)?;

        // Re-prove mutable rights, database bindings, review authority, and source bytes immediately
        // before publication. The instance lock prevents writers; this second read detects accidental
        // out-of-band changes and implementation drift.
        let current_rows = load_rows(db, &pool.pool_id, voice_name)?;
        if current_rows != rows {
            return Err(AppError::Validation(
                "voice authority, rights, or pool membership changed during export".into(),
            ));
        }
        let current_authority = review_pool::voice_authority_digests(db, voice_name).map_err(AppError::Validation)?;
        if current_authority != authority {
            return Err(AppError::Validation("review authority changed during export".to_string()));
        }
        for (source, before_sha) in &source_sha_by_path {
            if sha256_file(Path::new(source))? != *before_sha {
                return Err(AppError::Validation(format!("source audio changed during export: {source}")));
            }
        }

        let existing = review_pool::voice_certificate(db, voice_name).map_err(AppError::Validation)?;
        let certificate_value = |app_git_sha: &str, created_at_ms: i64| {
            serde_json::json!({
                "schemaVersion": POOL_EXPORT_SCHEMA_VERSION,
                "poolId": pool.pool_id,
                "poolFocusSha256": pool.focus_sha256,
                "sourcePoolSegmentCount": dedup.source_segment_count,
                "canonicalReviewSegmentCount": dedup.canonical_segment_count,
                "excludedDuplicateSegmentCount": dedup.excluded_segment_count,
                "duplicateFamilyCount": dedup.duplicate_family_count,
                "dedupManifestSha256": dedup_manifest_sha256,
                "dedupAlgorithmId": dedup_algorithm_id,
                "dedupUnconfirmedRiskCount": dedup.unconfirmed_risk_count,
                "voiceName": voice_name,
                "championModelVersionId": pool.champion_model_version_id,
                "championDeploymentSha256": pool.champion_deployment_sha256,
                "resolutionSha256": authority.resolution_sha256,
                "reviewerSha256": authority.reviewer_sha256,
                "decisionAndReviewerEvidenceSha256": authority.reviewer_sha256,
                "rightsSha256": rights_sha256,
                "audioSha256": audio_sha256,
                "exportManifestSha256": manifest_sha256,
                "exportSha256sumsSha256": sha256sums_sha256,
                "retainedSegments": retained_rows.len(),
                "rejectedSegments": rejected_rows.len(),
                "totalDurationMs": total_duration_ms,
                "appGitSha": app_git_sha,
                "createdAtMs": created_at_ms,
            })
        };
        let (certificate_json, certificate_sha256, created_at_ms) = if let Some(certificate) = &existing {
            let same = certificate.pool_id == pool.pool_id
                && certificate.resolution_sha256 == authority.resolution_sha256
                && certificate.rights_sha256 == rights_sha256
                && certificate.audio_sha256 == audio_sha256
                && certificate.reviewer_sha256 == authority.reviewer_sha256
                && certificate.export_manifest_sha256 == manifest_sha256
                && certificate.export_sha256sums_sha256 == sha256sums_sha256
                && certificate.retained_segments == retained_rows.len()
                && certificate.rejected_segments == rejected_rows.len()
                && certificate.total_duration_ms == total_duration_ms;
            if !same {
                return Err(AppError::Validation(format!(
                    "voice {voice_name} has an immutable certificate for different export evidence"
                )));
            }
            let parsed: serde_json::Value = serde_json::from_str(&certificate.certificate_json)
                .map_err(|error| AppError::Validation(format!("stored voice certificate is invalid JSON: {error}")))?;
            if parsed != certificate_value(&certificate.app_git_sha, certificate.created_at_ms)
                || sha256_bytes(certificate.certificate_json.as_bytes()) != certificate.certificate_sha256
            {
                return Err(AppError::Validation(format!(
                    "voice {voice_name} has an internally inconsistent immutable certificate"
                )));
            }
            (certificate.certificate_json.clone(), certificate.certificate_sha256.clone(), certificate.created_at_ms)
        } else if output.is_dir() {
            // A complete destination without a database row is the only legal residue of a process
            // dying after the atomic directory rename but before SQLite certification. Reuse its
            // original timestamp/certificate bytes only when every evidence field is exact; the
            // full tree comparison below then proves this is recovery, never adoption of foreign data.
            let json = fs::read_to_string(output.join("certificate.json")).map_err(|error| {
                AppError::Validation(format!(
                    "existing export destination has no readable recovery certificate: {error}"
                ))
            })?;
            let parsed: serde_json::Value = serde_json::from_str(&json).map_err(|error| {
                AppError::Validation(format!("existing export recovery certificate is invalid JSON: {error}"))
            })?;
            let created_at_ms =
                parsed.get("createdAtMs").and_then(serde_json::Value::as_i64).filter(|value| *value > 0).ok_or_else(
                    || AppError::Validation("existing export recovery certificate has no valid timestamp".into()),
                )?;
            // Recovery deliberately requires the same immutable binary that published the tree.
            // Trusting appGitSha from a mutable orphan directory would let a filesystem edit forge
            // provenance. The versioned publishing release remains available for exact recovery.
            if parsed != certificate_value(crate::GIT_SHA, created_at_ms) {
                return Err(AppError::Validation(
                    "existing export recovery certificate does not match current voice authority".into(),
                ));
            }
            let digest = sha256_bytes(json.as_bytes());
            (json, digest, created_at_ms)
        } else {
            let created_at_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| AppError::Validation(format!("system clock is invalid: {error}")))?
                .as_millis();
            let created_at_ms = i64::try_from(created_at_ms)
                .map_err(|_| AppError::Validation("system clock exceeds SQLite integer range".to_string()))?;
            let certificate_value = certificate_value(crate::GIT_SHA, created_at_ms);
            let json = serde_json::to_string(&certificate_value).map_err(|error| AppError::Other(error.to_string()))?;
            let digest = sha256_bytes(json.as_bytes());
            (json, digest, created_at_ms)
        };
        fs::write(staging.join("certificate.json"), certificate_json.as_bytes())?;
        fs::write(
            staging.join("_COMPLETE.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schemaVersion": POOL_EXPORT_SCHEMA_VERSION,
                "manifestSha256": manifest_sha256,
                "sha256sumsSha256": sha256sums_sha256,
                "certificateSha256": certificate_sha256,
            }))
            .map_err(|error| AppError::Other(error.to_string()))?,
        )?;
        sync_tree(&staging)?;
        if output.exists() {
            verify_exact_tree(&staging, &output)?;
        } else {
            // Publication is one same-filesystem rename of a fully written, fully verified tree.
            // If the process dies immediately afterward, the artifact is complete but certification
            // remains conservatively absent; rerunning this exact command verifies every byte and
            // finishes the database commit without replacing the artifact.
            fs::rename(&staging, &output)?;
            crate::atomic_file::fsync_parent_dir(&output);
            #[cfg(test)]
            if take_publication_crash() {
                return Err(AppError::Validation(format!(
                    "injected crash window after atomic publication; certification remains absent for {}",
                    output.display()
                )));
            }
        }

        if existing.is_none() {
            review_pool::record_voice_certificate(
                db,
                &review_pool::VoiceCertificateInput {
                    voice_name,
                    resolution_sha256: &authority.resolution_sha256,
                    rights_sha256: &rights_sha256,
                    audio_sha256: &audio_sha256,
                    reviewer_sha256: &authority.reviewer_sha256,
                    export_manifest_sha256: &manifest_sha256,
                    export_sha256sums_sha256: &sha256sums_sha256,
                    certificate_json: &certificate_json,
                    certificate_sha256: &certificate_sha256,
                    retained_segments: retained_rows.len(),
                    rejected_segments: rejected_rows.len(),
                    total_duration_ms,
                    created_at_ms,
                },
            )
            .map_err(|error| {
                AppError::Validation(format!(
                    "export is atomically published but database certification failed; rerun the same command to recover: {error}"
                ))
            })?;
        }
        Ok(PoolDatasetResult {
            output_dir: output.to_string_lossy().to_string(),
            pool_id: pool.pool_id,
            voice_name: voice_name.to_string(),
            retained_segments: retained_rows.len(),
            rejected_segments: rejected_rows.len(),
            total_duration_ms,
            manifest_sha256,
            sha256sums_sha256,
            certificate_sha256,
        })
    })();
    if result.is_err() {
        remove_staging(&staging);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CHAMPION: &str = "omniasr-7b-pool-export-test";
    const RAW: &str = "دەقی چامپیۆن";

    fn rollback_fixture_to(db: &Database, target_version: i64) {
        let expected = crate::migrations::MIGRATIONS
            .iter()
            .filter(|migration| migration.version > target_version)
            .rev()
            .map(|migration| migration.version)
            .collect::<Vec<_>>();
        assert_eq!(crate::migrations::rollback(db, expected.len()).unwrap(), expected);
        assert_eq!(crate::migrations::get_current_version(db).unwrap(), target_version);
    }

    fn upgrade_fixture_from(db: &Database, source_version: i64) {
        let expected = crate::migrations::MIGRATIONS
            .iter()
            .filter(|migration| migration.version > source_version)
            .map(|migration| migration.version)
            .collect::<Vec<_>>();
        assert_eq!(crate::migrations::run_migrations(db).unwrap(), expected);
        assert_eq!(crate::migrations::get_current_version(db).unwrap(), crate::migrations::max_supported_version());
    }

    fn write_master(path: &Path, milliseconds: usize, seed: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TTS_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for index in 0..(milliseconds * 24) {
            writer.write_sample((((index + seed * 137) % 2000) as i16).saturating_sub(1000)).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn reviewed_segment(
        id: &str,
        audio_path: &Path,
        start_ms: i64,
        end_ms: i64,
        reject: bool,
    ) -> crate::db::SpeechSegment {
        let final_text = format!("دەقی {id}");
        crate::db::SpeechSegment {
            id: id.to_string(),
            audio_path: audio_path.to_string_lossy().to_string(),
            raw_transcript: RAW.to_string(),
            annotated_transcript: (!reject).then_some(final_text.clone()),
            verdict: Some(if reject { "human_reject" } else { "human_edit" }.to_string()),
            verdict_transcript: (!reject).then_some(final_text),
            human_decision: Some(if reject { "reject" } else { "edit" }.to_string()),
            reviewed_by: Some("Rubar".to_string()),
            verified: true,
            duration_ms: end_ms - start_ms,
            model_version_id: Some(TEST_CHAMPION.to_string()),
            alignment_json: Some(format!("{{\"source_start_ms\":{start_ms},\"source_end_ms\":{end_ms}}}")),
            ..crate::db::SpeechSegment::default()
        }
    }

    fn fixture() -> (tempfile::TempDir, Database) {
        let directory = tempfile::tempdir().unwrap();
        let bounded_master = directory.path().join("bounded-master.wav");
        let full_master = directory.path().join("full-master.wav");
        let reject_master = directory.path().join("reject-master.wav");
        let duplicate_master = directory.path().join("duplicate-master.wav");
        write_master(&bounded_master, 2_000, 0);
        write_master(&full_master, 1_000, 1);
        write_master(&reject_master, 1_000, 2);
        write_master(&duplicate_master, 2_000, 0);
        let mut duplicate_reader = hound::WavReader::open(&duplicate_master).unwrap();
        let duplicate_spec = duplicate_reader.spec();
        let mut duplicate_samples = duplicate_reader.samples::<i16>().collect::<Result<Vec<_>, _>>().unwrap();
        drop(duplicate_reader);
        duplicate_samples[100] = duplicate_samples[100].saturating_add(1);
        let mut duplicate_writer = hound::WavWriter::create(&duplicate_master, duplicate_spec).unwrap();
        for sample in duplicate_samples {
            duplicate_writer.write_sample(sample).unwrap();
        }
        duplicate_writer.finalize().unwrap();
        let bounded_hash = crate::export_bundle::current_canonical_pcm_blake3(&bounded_master).unwrap();
        let full_hash = crate::export_bundle::current_canonical_pcm_blake3(&full_master).unwrap();
        let reject_hash = crate::export_bundle::current_canonical_pcm_blake3(&reject_master).unwrap();
        let duplicate_hash = crate::export_bundle::current_canonical_pcm_blake3(&duplicate_master).unwrap();
        assert_ne!(duplicate_hash, bounded_hash);

        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        crate::registry::register_candidate(
            &db,
            &crate::registry::NewModelVersion {
                id: TEST_CHAMPION.to_string(),
                family: crate::deployment::OMNIASR_7B_FAMILY.to_string(),
                model_card_name: Some("pool export test".to_string()),
                checkpoint_sha256: "c".repeat(64),
                checkpoint_path: "/test/export-champion.json".to_string(),
                source: "cortex-finetuned".to_string(),
                license: "owner-full-rights".to_string(),
            },
        )
        .unwrap();
        db.connection().execute("UPDATE model_versions SET status='champion' WHERE id=?1", [TEST_CHAMPION]).unwrap();
        rollback_fixture_to(&db, 59);
        let mut duplicate_segment = reviewed_segment("duplicate", &duplicate_master, 500, 1_500, false);
        duplicate_segment.annotated_transcript = None;
        duplicate_segment.verdict = None;
        duplicate_segment.verdict_transcript = None;
        duplicate_segment.human_decision = None;
        duplicate_segment.reviewed_by = None;
        duplicate_segment.verified = false;
        for (segment, audio_hash) in [
            (reviewed_segment("bounded", &bounded_master, 500, 1_500, false), bounded_hash.as_str()),
            (reviewed_segment("full", &full_master, 0, 1_000, false), full_hash.as_str()),
            (reviewed_segment("rejected", &reject_master, 0, 1_000, true), reject_hash.as_str()),
            (duplicate_segment, duplicate_hash.as_str()),
        ] {
            db.insert_segment_full(&segment).unwrap();
            db.connection()
                .execute("UPDATE speech_segments SET audio_content_hash=?1 WHERE id=?2", [audio_hash, &segment.id])
                .unwrap();
        }
        upgrade_fixture_from(&db, 59);
        let pool = review_pool::activate(
            &db,
            "123e4567-e89b-42d3-a456-426614174070",
            &[
                review_pool::PoolMemberInput { segment_id: "bounded".into(), voice_name: "Lamo".into() },
                review_pool::PoolMemberInput { segment_id: "full".into(), voice_name: "Lamo".into() },
                review_pool::PoolMemberInput { segment_id: "rejected".into(), voice_name: "Lamo".into() },
                review_pool::PoolMemberInput { segment_id: "duplicate".into(), voice_name: "Lamo".into() },
            ],
        )
        .unwrap();
        review_pool::stamp_owner_supplied_pool_rights(&db).unwrap();
        let segment_ids = vec!["bounded".to_string(), "duplicate".to_string()];
        let proof_edges = vec![serde_json::json!({
            "leftSegmentId": "bounded",
            "rightSegmentId": "duplicate",
            "correlationPpm": 1_000_000,
        })];
        let family_material = serde_json::json!({
            "poolId": &pool.pool_id,
            "proofEdges": &proof_edges,
            "segmentIds": &segment_ids,
        });
        let family_id: String = Sha256::digest(review_pool::canonical_json_bytes(&family_material).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let raw_sha256 = review_pool::normalized_text_sha256(RAW);
        let mut dedup_manifest = serde_json::json!({
            "manifestSchema": 1,
            "algorithm": {
                "id": "cortex-cross-file-waveform-correlation-v1",
                "minimumTextCharacters": 25,
                "offsetToleranceMs": 500,
                "minimumTextSimilarityPpm": 900_000,
                "audioDurationToleranceMs": 120,
                "minimumWaveformCorrelationPpm": 980_000,
                "comparisonSampleRateHz": 16_000,
            },
            "pool": {
                "poolId": &pool.pool_id,
                "sourceFocusSegmentCount": pool.focus_segment_count,
                "sourceFocusSha256": &pool.focus_sha256,
                "championModelVersionId": &pool.champion_model_version_id,
                "championDeploymentSha256": &pool.champion_deployment_sha256,
            },
            "summary": {
                "candidateTextGroups": 1,
                "clearedRepeatedTextGroups": 0,
                "duplicateFamilies": 1,
                "excludedMembers": 1,
                "canonicalMembers": 3,
                "unconfirmedRiskGroups": 0,
                "reviewedCanonicalMembers": 1,
            },
            "families": [{
                "familyId": family_id,
                "voiceName": "Lamo",
                "canonicalSegmentId": "bounded",
                "canonicalSelectionReason": "preserve-human-review-evidence",
                "members": [{
                    "segmentId": "bounded",
                    "voiceName": "Lamo",
                    "sourceFileName": "bounded-master.wav",
                    "rawTranscriptSha256": &raw_sha256,
                    "audioContentHash": &bounded_hash,
                    "sourceStartMs": 500,
                    "sourceEndMs": 1_500,
                    "durationMs": 1_000,
                    "reviewEvidenceCount": 1,
                    "snrMilliDb": null,
                    "clippingPpm": null,
                    "signalAnomalyPpm": null,
                    "confidencePpm": null,
                    "canonical": true,
                }, {
                    "segmentId": "duplicate",
                    "voiceName": "Lamo",
                    "sourceFileName": "duplicate-master.wav",
                    "rawTranscriptSha256": &raw_sha256,
                    "audioContentHash": &duplicate_hash,
                    "sourceStartMs": 500,
                    "sourceEndMs": 1_500,
                    "durationMs": 1_000,
                    "reviewEvidenceCount": 0,
                    "snrMilliDb": null,
                    "clippingPpm": null,
                    "signalAnomalyPpm": null,
                    "confidencePpm": null,
                    "canonical": false,
                }],
                "proofEdges": proof_edges,
            }],
            "generatedAtMs": 1,
        });
        let dedup_sha256: String = Sha256::digest(review_pool::canonical_json_bytes(&dedup_manifest).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        dedup_manifest
            .as_object_mut()
            .unwrap()
            .insert("manifestSha256".into(), serde_json::Value::String(dedup_sha256));
        let dedup_json = String::from_utf8(review_pool::canonical_json_bytes(&dedup_manifest).unwrap()).unwrap();
        let dedup = review_pool::apply_dedup_manifest(&db, &dedup_json).unwrap();
        assert_eq!(dedup.source_segment_count, 4);
        assert_eq!(dedup.canonical_segment_count, 3);
        assert_eq!(dedup.excluded_segment_count, 1);
        for (index, (id, hash, start, end, reject)) in [
            ("bounded", bounded_hash.as_str(), 500, 1_500, false),
            ("full", full_hash.as_str(), 0, 1_000, false),
            ("rejected", reject_hash.as_str(), 0, 1_000, true),
        ]
        .into_iter()
        .enumerate()
        {
            let (_, revision) = db.get_segment_by_id_with_revision(id).unwrap().unwrap();
            let transcript = format!("دەقی {id}");
            review_pool::record_decision(
                &db,
                &pool,
                &review_pool::PoolDecisionInput {
                    segment_id: id,
                    reviewer: "Alle",
                    action: if reject { "reject" } else { "edit" },
                    submitted_transcript: (!reject).then_some(transcript.as_str()),
                    served_transcript: RAW,
                    served_revision: revision,
                    audio_content_hash: Some(hash),
                    source_start_ms: Some(start),
                    source_end_ms: Some(end),
                    duration_ms: end - start,
                    requested_action: if reject { "bad" } else { "edit" },
                    requested_transcript: if reject { RAW } else { transcript.as_str() },
                    operation_id: &format!("123e4567-e89b-42d3-a456-42661417407{}", index + 1),
                    operation_payload_hash: &format!("{}", index + 1).repeat(64),
                    created_at_ms: (index + 1) as i64,
                    playback_authority_session_id: Some(
                        &review_pool::mint_synthetic_playback_authority(&db, "Alle", id, &"f".repeat(64)).unwrap(),
                    ),
                },
            )
            .unwrap()
            .unwrap();
        }
        (directory, db)
    }

    fn read_jsonl(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    #[test]
    fn pool_export_is_deterministic_excludes_rejects_and_preserves_exact_audio_contracts() {
        let (directory, db) = fixture();
        let first_output = directory.path().join("export-one");
        let first = export_voice(
            &db,
            &PoolDatasetOptions {
                output_dir: first_output.to_string_lossy().to_string(),
                voice_name: "Lamo".to_string(),
            },
        )
        .unwrap();
        assert_eq!(first.retained_segments, 2);
        assert_eq!(first.rejected_segments, 1);
        assert_eq!(first.total_duration_ms, 2_000);
        assert!(first_output.join("certificate.json").is_file());
        assert_eq!(
            review_pool::voice_certificate(&db, "Lamo").unwrap().unwrap().certificate_sha256,
            first.certificate_sha256
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(first_output.join("manifest.json")).unwrap()).unwrap();
        let certificate: serde_json::Value =
            serde_json::from_slice(&fs::read(first_output.join("certificate.json")).unwrap()).unwrap();
        for value in [&manifest, &certificate] {
            assert_eq!(value["schemaVersion"], 2);
            assert_eq!(value["sourcePoolSegmentCount"], 4);
            assert_eq!(value["canonicalReviewSegmentCount"], 3);
            assert_eq!(value["excludedDuplicateSegmentCount"], 1);
            assert_eq!(value["duplicateFamilyCount"], 1);
            assert_eq!(value["dedupUnconfirmedRiskCount"], 0);
            assert_eq!(value["dedupAlgorithmId"], "cortex-cross-file-waveform-correlation-v1");
            assert_eq!(value["decisionAndReviewerEvidenceSha256"], value["reviewerSha256"]);
        }

        let tts = read_jsonl(&first_output.join("tts/metadata.jsonl"));
        assert_eq!(tts.len(), 2);
        let bounded = tts.iter().find(|row| row["id"] == "bounded").unwrap();
        let full = tts.iter().find(|row| row["id"] == "full").unwrap();
        assert_eq!(bounded["sourceBytesPreserved"], false);
        assert_eq!(bounded["sourceStartMs"], 500);
        assert_eq!(bounded["sourceEndMs"], 1_500);
        assert_eq!(full["sourceBytesPreserved"], true);
        assert_eq!(
            sha256_file(&first_output.join("tts/audio_24k/000002.wav")).unwrap(),
            sha256_file(&directory.path().join("full-master.wav")).unwrap()
        );
        let asr_reader = hound::WavReader::open(first_output.join("asr/audio_16k/000001.wav")).unwrap();
        assert_eq!(asr_reader.spec().sample_rate, ASR_SAMPLE_RATE);
        assert_eq!(asr_reader.spec().channels, 1);
        assert_eq!(asr_reader.duration(), 16_000);
        assert_eq!(read_jsonl(&first_output.join("exclusions.jsonl"))[0]["id"], "rejected");
        assert!(
            !read_jsonl(&first_output.join("rights.jsonl")).iter().any(|row| row["id"] == "duplicate"),
            "proven non-canonical duplicates must be absent from every export surface"
        );
        assert_eq!(fs::read_dir(first_output.join("tts/audio_24k")).unwrap().count(), 2);

        let second_output = directory.path().join("export-two");
        let second = export_voice(
            &db,
            &PoolDatasetOptions {
                output_dir: second_output.to_string_lossy().to_string(),
                voice_name: "Lamo".to_string(),
            },
        )
        .unwrap();
        assert_eq!(second.manifest_sha256, first.manifest_sha256);
        assert_eq!(second.sha256sums_sha256, first.sha256sums_sha256);
        assert_eq!(second.certificate_sha256, first.certificate_sha256);

        let exact_retry = export_voice(
            &db,
            &PoolDatasetOptions {
                output_dir: first_output.to_string_lossy().to_string(),
                voice_name: "Lamo".to_string(),
            },
        )
        .unwrap();
        assert_eq!(exact_retry.certificate_sha256, first.certificate_sha256);
    }

    #[test]
    fn pool_export_refuses_rights_or_audio_drift() {
        let (directory, db) = fixture();
        db.connection().execute("UPDATE speech_segments SET rights_license='conflict' WHERE id='bounded'", []).unwrap();
        let rights_error = export_voice(
            &db,
            &PoolDatasetOptions {
                output_dir: directory.path().join("rights-refused").to_string_lossy().to_string(),
                voice_name: "Lamo".to_string(),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(rights_error.contains("rights"), "unexpected refusal: {rights_error}");

        let (directory, db) = fixture();
        write_master(&directory.path().join("bounded-master.wav"), 2_000, 0);
        // The replacement has the same container contract but different PCM content.
        let path = directory.path().join("bounded-master.wav");
        let mut reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        let mut samples = reader.samples::<i16>().collect::<Result<Vec<_>, _>>().unwrap();
        drop(reader);
        samples[100] = samples[100].saturating_add(1);
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for sample in samples {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        let audio_error = export_voice(
            &db,
            &PoolDatasetOptions {
                output_dir: directory.path().join("audio-refused").to_string_lossy().to_string(),
                voice_name: "Lamo".to_string(),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(audio_error.contains("PCM identity"), "unexpected refusal: {audio_error}");
    }

    #[test]
    fn pool_export_refuses_missing_audio_without_publishing_output() {
        let (directory, db) = fixture();
        std::fs::remove_file(directory.path().join("bounded-master.wav")).unwrap();
        let output = directory.path().join("missing-audio-refused");

        let error = export_voice(
            &db,
            &PoolDatasetOptions { output_dir: output.to_string_lossy().to_string(), voice_name: "Lamo".to_string() },
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("source WAV is missing"), "unexpected refusal: {error}");
        assert!(!output.exists(), "failed export must never publish an output directory");
    }

    #[test]
    fn atomic_publication_precedes_certificate_and_a_crash_window_is_retryable() {
        let (directory, db) = fixture();
        let output = directory.path().join("crash-window");
        arm_publication_crash();

        let error = export_voice(
            &db,
            &PoolDatasetOptions { output_dir: output.to_string_lossy().to_string(), voice_name: "Lamo".to_string() },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("after atomic publication"), "unexpected injected failure: {error}");
        assert!(output.join("_COMPLETE.json").is_file(), "publication must be all-or-nothing and self-verifying");
        assert!(output.join("certificate.json").is_file());
        assert!(
            review_pool::voice_certificate(&db, "Lamo").unwrap().is_none(),
            "a crash may not make final certification claim that publication completed"
        );

        let retried = export_voice(
            &db,
            &PoolDatasetOptions { output_dir: output.to_string_lossy().to_string(), voice_name: "Lamo".to_string() },
        )
        .unwrap();
        assert_eq!(
            fs::canonicalize(&retried.output_dir).unwrap(),
            fs::canonicalize(&output).unwrap(),
            "the retry must publish the requested directory even when Windows canonicalizes it with a device prefix"
        );
        assert!(output.join("_COMPLETE.json").is_file());
        assert!(output.join("certificate.json").is_file());
        assert_eq!(
            review_pool::voice_certificate(&db, "Lamo").unwrap().unwrap().certificate_sha256,
            retried.certificate_sha256,
            "the retry must certify the exact already-published bytes"
        );
    }

    #[test]
    fn crash_recovery_refuses_a_tampered_published_tree_without_certifying_it() {
        let (directory, db) = fixture();
        let output = directory.path().join("tampered-crash-window");
        arm_publication_crash();
        export_voice(
            &db,
            &PoolDatasetOptions { output_dir: output.to_string_lossy().to_string(), voice_name: "Lamo".to_string() },
        )
        .unwrap_err();
        fs::write(output.join("unexpected.txt"), b"not part of the certified export").unwrap();

        let error = export_voice(
            &db,
            &PoolDatasetOptions { output_dir: output.to_string_lossy().to_string(), voice_name: "Lamo".to_string() },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("different file inventory"), "unexpected refusal: {error}");
        assert!(
            review_pool::voice_certificate(&db, "Lamo").unwrap().is_none(),
            "tampered crash residue must never become certified"
        );
    }

    #[test]
    fn stored_certificate_is_revalidated_and_database_tampering_fails_closed() {
        let (directory, db) = fixture();
        export_voice(
            &db,
            &PoolDatasetOptions {
                output_dir: directory.path().join("certified").to_string_lossy().to_string(),
                voice_name: "Lamo".to_string(),
            },
        )
        .unwrap();
        assert!(review_pool::voice_certificate(&db, "Lamo").unwrap().is_some());

        // SQLite's immutable trigger prevents ordinary tampering. Dropping it here models low-level
        // database corruption or an operator bypass and proves that read-only certification still
        // refuses the row instead of trusting its presence.
        db.connection().execute("DROP TRIGGER review_pool_voice_certificates_immutable_update", []).unwrap();
        db.connection()
            .execute(
                "UPDATE review_pool_voice_certificates
                    SET certificate_json=json_set(certificate_json, '$.canonicalReviewSegmentCount', 999)
                  WHERE voice_name='Lamo'",
                [],
            )
            .unwrap();
        let error = review_pool::voice_certificate(&db, "Lamo").unwrap_err();
        assert!(
            error.contains("complete v64 pool authority") || error.contains("digest"),
            "unexpected certificate refusal: {error}"
        );
    }

    /// One-voice, one-clip pool with rights stamped but NO dedup manifest and NO second opinion,
    /// so each export precondition can be peeled off one refusal at a time.
    fn lite_fixture(master_ms: usize, end_ms: i64) -> (tempfile::TempDir, Database, crate::review_pool::ReviewPool) {
        let directory = tempfile::tempdir().unwrap();
        let master = directory.path().join("solo-master.wav");
        write_master(&master, master_ms, 3);
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        crate::registry::register_candidate(
            &db,
            &crate::registry::NewModelVersion {
                id: TEST_CHAMPION.to_string(),
                family: crate::deployment::OMNIASR_7B_FAMILY.to_string(),
                model_card_name: Some("pool export lite".to_string()),
                checkpoint_sha256: "c".repeat(64),
                checkpoint_path: "/test/export-champion.json".to_string(),
                source: "cortex-finetuned".to_string(),
                license: "owner-full-rights".to_string(),
            },
        )
        .unwrap();
        db.connection().execute("UPDATE model_versions SET status='champion' WHERE id=?1", [TEST_CHAMPION]).unwrap();
        rollback_fixture_to(&db, 59);
        db.insert_segment_full(&reviewed_segment("solo", &master, 0, end_ms, false)).unwrap();
        upgrade_fixture_from(&db, 59);
        let master_hash = crate::export_bundle::current_canonical_pcm_blake3(&master).unwrap();
        db.connection()
            .execute("UPDATE speech_segments SET audio_content_hash=?1 WHERE id='solo'", [master_hash])
            .unwrap();
        let pool = review_pool::activate(
            &db,
            "123e4567-e89b-42d3-a456-426614174200",
            &[review_pool::PoolMemberInput { segment_id: "solo".into(), voice_name: "Lamo".into() }],
        )
        .unwrap();
        review_pool::stamp_owner_supplied_pool_rights(&db).unwrap();
        (directory, db, pool)
    }

    fn empty_dedup_manifest(pool: &crate::review_pool::ReviewPool) -> String {
        let mut value = serde_json::json!({
            "manifestSchema": 1,
            "algorithm": {
                "id": "cortex-cross-file-waveform-correlation-v1",
                "minimumTextCharacters": 25,
                "offsetToleranceMs": 500,
                "minimumTextSimilarityPpm": 900_000,
                "audioDurationToleranceMs": 120,
                "minimumWaveformCorrelationPpm": 980_000,
                "comparisonSampleRateHz": 16_000,
            },
            "pool": {
                "poolId": &pool.pool_id,
                "sourceFocusSegmentCount": pool.focus_segment_count,
                "sourceFocusSha256": &pool.focus_sha256,
                "championModelVersionId": &pool.champion_model_version_id,
                "championDeploymentSha256": &pool.champion_deployment_sha256,
            },
            "summary": {
                "candidateTextGroups": 0,
                "clearedRepeatedTextGroups": 0,
                "duplicateFamilies": 0,
                "excludedMembers": 0,
                "canonicalMembers": pool.focus_segment_count,
                "unconfirmedRiskGroups": 0,
                "reviewedCanonicalMembers": 0,
            },
            "families": [],
            "generatedAtMs": 1,
        });
        let digest: String = Sha256::digest(review_pool::canonical_json_bytes(&value).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        value.as_object_mut().unwrap().insert("manifestSha256".into(), serde_json::Value::String(digest));
        String::from_utf8(review_pool::canonical_json_bytes(&value).unwrap()).unwrap()
    }

    fn resolve_solo(db: &Database, pool: &crate::review_pool::ReviewPool, end_ms: i64) {
        let (_, revision) = db.get_segment_by_id_with_revision("solo").unwrap().unwrap();
        let audio_hash: String = db
            .connection()
            .query_row("SELECT audio_content_hash FROM speech_segments WHERE id='solo'", [], |row| row.get(0))
            .unwrap();
        review_pool::record_decision(
            db,
            pool,
            &review_pool::PoolDecisionInput {
                segment_id: "solo",
                reviewer: "Alle",
                action: "edit",
                submitted_transcript: Some("دەقی solo"),
                served_transcript: RAW,
                served_revision: revision,
                audio_content_hash: Some(&audio_hash),
                source_start_ms: Some(0),
                source_end_ms: Some(end_ms),
                duration_ms: end_ms,
                requested_action: "edit",
                requested_transcript: "دەقی solo",
                operation_id: "123e4567-e89b-42d3-a456-426614174201",
                operation_payload_hash: &"9".repeat(64),
                created_at_ms: 5,
                playback_authority_session_id: Some(
                    &review_pool::mint_synthetic_playback_authority(db, "Alle", "solo", &"f".repeat(64)).unwrap(),
                ),
            },
        )
        .unwrap()
        .unwrap();
    }

    fn export_error(db: &Database, output: &Path, voice_name: &str) -> String {
        export_voice(
            db,
            &PoolDatasetOptions { output_dir: output.to_string_lossy().to_string(), voice_name: voice_name.into() },
        )
        .unwrap_err()
        .to_string()
    }

    #[test]
    fn export_refuses_missing_pool_dedup_unresolved_voices_and_bad_destinations() {
        let fresh = Database::open(":memory:").unwrap();
        fresh.initialize().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let error = export_error(&fresh, &scratch.path().join("no-pool"), "Lamo");
        assert!(error.contains("review pool is not active"), "unexpected refusal: {error}");

        let (directory, db, pool) = lite_fixture(1_000, 1_000);
        let error = export_error(&db, &directory.path().join("no-dedup"), "Lamo");
        assert!(
            error.contains("review-pool export requires an applied immutable dedup manifest"),
            "unexpected refusal: {error}"
        );

        review_pool::apply_dedup_manifest(&db, &empty_dedup_manifest(&pool)).unwrap();
        let error = export_error(&db, &directory.path().join("blank-voice"), "   ");
        assert!(error.contains("voice name must be a non-blank printable label"), "unexpected refusal: {error}");
        let error = export_error(&db, &directory.path().join("control-voice"), "La\u{7}mo");
        assert!(error.contains("voice name must be a non-blank printable label"), "unexpected refusal: {error}");
        let error = export_error(&db, &directory.path().join("ghost-voice"), "Ghost");
        assert!(error.contains("active review pool has no voice named Ghost"), "unexpected refusal: {error}");
        let error = export_error(&db, &directory.path().join("unresolved"), "Lamo");
        assert!(
            error.contains("voice Lamo is not fully resolved"),
            "one canonical opinion is not a decision and must not export: {error}"
        );

        resolve_solo(&db, &pool, 1_000);
        let occupied = directory.path().join("occupied");
        fs::write(&occupied, b"a file, not a directory").unwrap();
        let error = export_error(&db, &occupied, "Lamo");
        assert!(
            error.contains("export destination already exists and is not a directory"),
            "unexpected refusal: {error}"
        );

        let output = directory.path().join("lite-export");
        let result = export_voice(
            &db,
            &PoolDatasetOptions { output_dir: output.to_string_lossy().to_string(), voice_name: "Lamo".to_string() },
        )
        .unwrap();
        assert_eq!(result.retained_segments, 1);
        assert_eq!(result.rejected_segments, 0);
        assert_eq!(result.total_duration_ms, 1_000);
        assert_eq!(
            review_pool::voice_certificate(&db, "Lamo").unwrap().unwrap().certificate_sha256,
            result.certificate_sha256
        );
    }

    #[test]
    fn export_refuses_a_span_that_escapes_its_master() {
        let (directory, db, pool) = lite_fixture(500, 1_000);
        review_pool::apply_dedup_manifest(&db, &empty_dedup_manifest(&pool)).unwrap();
        resolve_solo(&db, &pool, 1_000);
        let output = directory.path().join("escaping-span");
        let error = export_error(&db, &output, "Lamo");
        assert!(
            error.contains("solo: source span is outside its master or disagrees with duration"),
            "unexpected refusal: {error}"
        );
        assert!(!output.exists(), "a refused export must publish nothing");
    }

    #[test]
    fn export_audio_helper_contracts_are_exact() {
        let directory = tempfile::tempdir().unwrap();

        let error = strict_sample_index(-1, TTS_SAMPLE_RATE, "source_start_ms").unwrap_err().to_string();
        assert!(error.contains("source_start_ms cannot be negative"), "unexpected refusal: {error}");
        let error = strict_sample_index(1, 22_050, "source_end_ms").unwrap_err().to_string();
        assert!(error.contains("source_end_ms does not land on an exact sample boundary"), "unexpected: {error}");
        assert_eq!(strict_sample_index(1_000, TTS_SAMPLE_RATE, "span").unwrap(), 24_000);

        let error = read_master(&directory.path().join("missing.wav")).unwrap_err().to_string();
        assert!(error.contains("source WAV is missing"), "unexpected refusal: {error}");
        let wrong_rate = directory.path().join("16k.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: ASR_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wrong_rate, spec).unwrap();
        writer.write_sample(1_i16).unwrap();
        writer.finalize().unwrap();
        let error = read_master(&wrong_rate).unwrap_err().to_string();
        assert!(error.contains("TTS source must be mono 24 kHz PCM16 WAV"), "unexpected refusal: {error}");
        let empty = directory.path().join("empty.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TTS_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        hound::WavWriter::create(&empty, spec).unwrap().finalize().unwrap();
        let error = read_master(&empty).unwrap_err().to_string();
        assert!(error.contains("source WAV has no samples"), "unexpected refusal: {error}");
        let error = write_pcm16_wav(&directory.path().join("out.wav"), TTS_SAMPLE_RATE, &[]).unwrap_err().to_string();
        assert!(error.contains("refusing to write an empty WAV"), "unexpected refusal: {error}");

        let exact = RecordingRights {
            license: Some(review_pool::OWNER_RIGHTS_LICENSE.to_string()),
            consent_basis: Some(review_pool::OWNER_RIGHTS_CONSENT.to_string()),
            permitted_use: Some(review_pool::OWNER_RIGHTS_PERMITTED_USE.to_string()),
            attribution: Some(review_pool::OWNER_RIGHTS_ATTRIBUTION.to_string()),
            source: Some(review_pool::OWNER_RIGHTS_SOURCE.to_string()),
            revoked_at: None,
        };
        exact_owner_rights("seg", &exact).unwrap();
        let mut conflicting = exact.clone();
        conflicting.license = Some("CC-BY-4.0".to_string());
        let error = exact_owner_rights("seg", &conflicting).unwrap_err().to_string();
        assert!(
            error.contains("seg: exact owner-supplied recording rights are missing, conflicting, or revoked"),
            "unexpected refusal: {error}"
        );
        let mut revoked = exact.clone();
        revoked.revoked_at = Some("2026-08-24T00:00:00Z".to_string());
        assert!(exact_owner_rights("seg", &revoked).is_err());

        let expected = directory.path().join("expected");
        fs::create_dir(&expected).unwrap();
        let not_a_dir = directory.path().join("published-file");
        fs::write(&not_a_dir, b"file").unwrap();
        let error = verify_exact_tree(&expected, &not_a_dir).unwrap_err().to_string();
        assert!(error.contains("existing export destination is not a directory"), "unexpected refusal: {error}");
    }

    #[test]
    fn crash_recovery_refuses_missing_invalid_or_foreign_certificates_and_content_drift() {
        let (directory, db) = fixture();
        let crashed = |name: &str| -> PathBuf {
            let output = directory.path().join(name);
            arm_publication_crash();
            let error = export_error(&db, &output, "Lamo");
            assert!(error.contains("after atomic publication"), "unexpected injected failure: {error}");
            output
        };

        let output = crashed("missing-cert");
        fs::remove_file(output.join("certificate.json")).unwrap();
        let error = export_error(&db, &output, "Lamo");
        assert!(
            error.contains("existing export destination has no readable recovery certificate"),
            "unexpected refusal: {error}"
        );

        let output = crashed("invalid-cert");
        fs::write(output.join("certificate.json"), b"not json").unwrap();
        let error = export_error(&db, &output, "Lamo");
        assert!(error.contains("existing export recovery certificate is invalid JSON"), "unexpected refusal: {error}");

        let output = crashed("no-timestamp");
        fs::write(output.join("certificate.json"), b"{}").unwrap();
        let error = export_error(&db, &output, "Lamo");
        assert!(
            error.contains("existing export recovery certificate has no valid timestamp"),
            "unexpected refusal: {error}"
        );

        let output = crashed("foreign-cert");
        let mut certificate: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(output.join("certificate.json")).unwrap()).unwrap();
        certificate["voiceName"] = serde_json::json!("Other");
        fs::write(output.join("certificate.json"), serde_json::to_string(&certificate).unwrap()).unwrap();
        let error = export_error(&db, &output, "Lamo");
        assert!(
            error.contains("existing export recovery certificate does not match current voice authority"),
            "unexpected refusal: {error}"
        );

        let output = crashed("drifted-bytes");
        fs::write(output.join("rights.jsonl"), b"{\"id\":\"forged\"}\n").unwrap();
        let error = export_error(&db, &output, "Lamo");
        assert!(error.contains("differs at rights.jsonl"), "unexpected refusal: {error}");

        assert!(
            review_pool::voice_certificate(&db, "Lamo").unwrap().is_none(),
            "no refused recovery may certify the voice"
        );
    }

    #[test]
    fn publication_crash_injection_is_thread_local() {
        let armed = std::thread::spawn(|| {
            arm_publication_crash();
            assert!(take_publication_crash(), "the arming thread must observe its one-shot fault");
            assert!(!take_publication_crash(), "the fault must be consumed exactly once");
        });
        let clean = std::thread::spawn(|| {
            assert!(!take_publication_crash(), "one test thread must never inherit another thread's export fault");
        });
        armed.join().unwrap();
        clean.join().unwrap();
    }
}
