//! Killable canonical-review-media materialization boundary.
//!
//! The desktop never runs Symphonia against a review source in-process. It creates one empty,
//! unpredictable cache target, freezes the source, and launches the current Cortex executable in
//! this pre-Tauri worker mode. [`crate::engine_runtime::run_contained_command`] gives the child an
//! explicit deadline, bounded pipes, a process-memory ceiling, a one-process Windows Job Object,
//! and kill-on-close teardown. The child can only open the already-existing target named in its
//! bounded request; the parent treats every response as untrusted and re-verifies the canonical WAV.

#[cfg(not(test))]
use crate::engine_runtime::{run_contained_command, ContainedCommandSpec};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::process::Command;
use std::time::{Duration, Instant};

const WORKER_ARG: &str = "--cortex-media-materialization-worker";
const PROTOCOL_SCHEMA: u8 = 1;
const WORKER_INPUT_LIMIT_BYTES: usize = 128 * 1024;
const WORKER_OUTPUT_LIMIT_BYTES: usize = 4 * 1024;
#[cfg(not(test))]
const WORKER_STDERR_LIMIT_BYTES: usize = 8 * 1024;
const WORKER_INTERNAL_TIMEOUT: Duration = Duration::from_secs(285);
#[cfg(not(test))]
const WORKER_PARENT_TIMEOUT: Duration = Duration::from_secs(300);
#[cfg(not(test))]
const WORKER_PROCESS_MEMORY_LIMIT_BYTES: usize = 512 * 1024 * 1024;

/// A canonical mono PCM16 WAV may represent at most 24 hours. This is a disk-output bound, not a
/// claim about ordinary source-file size. It stays below classic WAV's 32-bit data-chunk ceiling.
pub(crate) const MAX_CANONICAL_REVIEW_WAV_BYTES: u64 = 2_764_800_044;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterializeRequestV1 {
    schema: u8,
    source_path: PathBuf,
    target_path: PathBuf,
    expected_audio_content_hash: String,
    source_raw_blake3_before: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaterializeResponseV1 {
    schema: u8,
    pub(crate) source_raw_blake3_after: String,
    pub(crate) canonical_pcm_blake3: String,
    pub(crate) output_bytes: u64,
}

fn is_canonical_blake3(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl MaterializeResponseV1 {
    fn is_canonical(&self) -> bool {
        self.schema == PROTOCOL_SCHEMA
            && is_canonical_blake3(&self.source_raw_blake3_after)
            && is_canonical_blake3(&self.canonical_pcm_blake3)
            && (44..=MAX_CANONICAL_REVIEW_WAV_BYTES).contains(&self.output_bytes)
    }
}

pub(crate) fn raw_file_blake3_before(path: &Path, deadline: Instant) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|error| format!("Open media source for raw identity: {error}"))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 256 * 1024];
    loop {
        if Instant::now() >= deadline {
            return Err("Raw media identity exceeded its internal deadline".to_string());
        }
        let read = file.read(&mut buffer).map_err(|error| format!("Read media source raw identity: {error}"))?;
        if read == 0 {
            return Ok(hasher.finalize().to_hex().to_string());
        }
        hasher.update(&buffer[..read]);
    }
}

fn validate_request(request: &MaterializeRequestV1) -> Result<(PathBuf, PathBuf), String> {
    if request.schema != PROTOCOL_SCHEMA
        || !is_canonical_blake3(&request.expected_audio_content_hash)
        || !is_canonical_blake3(&request.source_raw_blake3_before)
    {
        return Err("Media materialization request failed its schema or identity contract".to_string());
    }
    let source =
        PathBuf::from(crate::validation::input::validate_file_path(request.source_path.to_string_lossy().as_ref())?);
    let target =
        PathBuf::from(crate::validation::input::validate_file_path(request.target_path.to_string_lossy().as_ref())?);
    if source != request.source_path || target != request.target_path || source == target {
        return Err("Media materialization request paths are not exact canonical identities".to_string());
    }
    let target_metadata =
        std::fs::metadata(&target).map_err(|error| format!("Inspect parent-created media target: {error}"))?;
    if !target_metadata.is_file() || target_metadata.len() != 0 {
        return Err("Media materialization target must be an existing empty private file".to_string());
    }
    if target.extension().and_then(|extension| extension.to_str()) != Some("wav")
        || target.file_stem().and_then(|stem| stem.to_str()).and_then(|stem| uuid::Uuid::parse_str(stem).ok()).is_none()
    {
        return Err("Media materialization target name is not a canonical UUID WAV".to_string());
    }
    Ok((source, target))
}

fn read_request(mut input: impl Read) -> Result<MaterializeRequestV1, String> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take((WORKER_INPUT_LIMIT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Media materialization request could not be read".to_string())?;
    if bytes.is_empty() || bytes.len() > WORKER_INPUT_LIMIT_BYTES {
        return Err("Media materialization request size is invalid".to_string());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Media materialization request is not canonical JSON".to_string())
}

fn parse_response(bytes: &[u8]) -> Result<MaterializeResponseV1, String> {
    if bytes.is_empty() || bytes.len() > WORKER_OUTPUT_LIMIT_BYTES {
        return Err("Media materialization response size is invalid".to_string());
    }
    let response: MaterializeResponseV1 = serde_json::from_slice(bytes)
        .map_err(|_| "Media materialization response is not canonical JSON".to_string())?;
    if !response.is_canonical() {
        return Err("Media materialization response failed its schema or identity contract".to_string());
    }
    Ok(response)
}

fn run_worker(mut input: impl Read, mut output: impl Write) -> Result<(), String> {
    let request = read_request(&mut input)?;
    let (source, target) = validate_request(&request)?;
    let deadline = Instant::now()
        .checked_add(WORKER_INTERNAL_TIMEOUT)
        .ok_or_else(|| "Media materialization internal deadline overflowed".to_string())?;

    let raw_before = raw_file_blake3_before(&source, deadline)?;
    if raw_before != request.source_raw_blake3_before {
        return Err("Media source bytes changed before contained decode".to_string());
    }
    let output_bytes = crate::media::materialize_canonical_review_wav_into_parent_target(
        &source,
        &target,
        target.parent().ok_or_else(|| "Media target has no parent directory".to_string())?,
        deadline,
    )?;
    if output_bytes > MAX_CANONICAL_REVIEW_WAV_BYTES {
        return Err("Canonical review WAV exceeded its fixed output limit".to_string());
    }
    let raw_after = raw_file_blake3_before(&source, deadline)?;
    if raw_after != raw_before {
        return Err("Media source bytes changed during contained decode".to_string());
    }
    let canonical_pcm_blake3 = crate::media::canonical_review_wav_pcm_blake3_before(&target, deadline)?;
    if canonical_pcm_blake3 != request.expected_audio_content_hash {
        return Err("Contained canonical media did not match the imported audio identity".to_string());
    }

    let response = MaterializeResponseV1 {
        schema: PROTOCOL_SCHEMA,
        source_raw_blake3_after: raw_after,
        canonical_pcm_blake3,
        output_bytes,
    };
    if !response.is_canonical() {
        return Err("Media materialization worker produced a non-canonical response".to_string());
    }
    let bytes =
        serde_json::to_vec(&response).map_err(|_| "Media materialization response could not be encoded".to_string())?;
    if bytes.len() > WORKER_OUTPUT_LIMIT_BYTES {
        return Err("Media materialization response exceeded its fixed limit".to_string());
    }
    output
        .write_all(&bytes)
        .and_then(|()| output.flush())
        .map_err(|_| "Media materialization response could not be written".to_string())
}

#[cfg(not(test))]
pub(crate) fn materialize_contained(
    source: &Path,
    target: &Path,
    expected_audio_content_hash: &str,
    source_raw_blake3_before: &str,
) -> Result<MaterializeResponseV1, String> {
    let source_path =
        std::fs::canonicalize(source).map_err(|error| format!("Canonicalize contained media source: {error}"))?;
    let target_path =
        std::fs::canonicalize(target).map_err(|error| format!("Canonicalize parent-created media target: {error}"))?;
    let request = MaterializeRequestV1 {
        schema: PROTOCOL_SCHEMA,
        source_path,
        target_path,
        expected_audio_content_hash: expected_audio_content_hash.to_string(),
        source_raw_blake3_before: source_raw_blake3_before.to_string(),
    };
    validate_request(&request)?;
    let request =
        serde_json::to_vec(&request).map_err(|_| "Media materialization request could not be encoded".to_string())?;
    if request.is_empty() || request.len() > WORKER_INPUT_LIMIT_BYTES {
        return Err("Media materialization request exceeds its fixed input limit".to_string());
    }
    let executable = std::env::current_exe()
        .map_err(|error| format!("Media materialization worker executable could not be resolved: {error}"))?;
    let mut command = Command::new(executable);
    command.arg(WORKER_ARG);
    let output = run_contained_command(
        command,
        ContainedCommandSpec {
            timeout: WORKER_PARENT_TIMEOUT,
            stdin_body: request,
            max_stdin_bytes: WORKER_INPUT_LIMIT_BYTES,
            max_stdout_bytes: WORKER_OUTPUT_LIMIT_BYTES,
            max_stderr_bytes: WORKER_STDERR_LIMIT_BYTES,
            process_memory_limit_bytes: Some(WORKER_PROCESS_MEMORY_LIMIT_BYTES),
            active_process_limit: Some(1),
        },
    )
    .map_err(|error| format!("Contained media materialization was not authoritative: {error}"))?;
    parse_response(&output.stdout)
}

/// Called before Tauri initialization. Returning `Some` means this process was intentionally
/// launched in the non-UI materialization role and the caller must exit with this code.
pub fn run_special_process_mode() -> Option<i32> {
    (std::env::args().nth(1).as_deref() == Some(WORKER_ARG)).then(|| {
        match run_worker(std::io::stdin().lock(), std::io::stdout().lock()) {
            Ok(()) => 0,
            Err(_) => {
                // Do not expose private paths, decoder internals, or the request. The parent retains
                // only a bounded stderr body and treats every non-zero exit as non-authoritative.
                eprintln!("media materialization worker rejected its request or could not finish it");
                2
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_test_wav(path: &Path) {
        let mut writer = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for sample in 0..1_600_i16 {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn request_for(source: &Path, target: &Path) -> MaterializeRequestV1 {
        MaterializeRequestV1 {
            schema: PROTOCOL_SCHEMA,
            source_path: std::fs::canonicalize(source).unwrap(),
            target_path: std::fs::canonicalize(target).unwrap(),
            expected_audio_content_hash: crate::export_bundle::current_canonical_pcm_blake3(source).unwrap(),
            source_raw_blake3_before: raw_file_blake3_before(source, Instant::now() + Duration::from_secs(5)).unwrap(),
        }
    }

    #[test]
    fn local_worker_materializes_only_the_existing_target_and_returns_exact_identities() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.wav");
        write_test_wav(&source);
        let target = directory.path().join(format!("{}.wav", uuid::Uuid::new_v4()));
        std::fs::OpenOptions::new().write(true).create_new(true).open(&target).unwrap().sync_all().unwrap();
        let request = request_for(&source, &target);
        let encoded = serde_json::to_vec(&request).unwrap();
        let mut output = Vec::new();

        run_worker(encoded.as_slice(), &mut output).unwrap();
        let response = parse_response(&output).unwrap();
        assert_eq!(response.source_raw_blake3_after, request.source_raw_blake3_before);
        assert_eq!(response.canonical_pcm_blake3, request.expected_audio_content_hash);
        assert_eq!(response.output_bytes, std::fs::metadata(&target).unwrap().len());
        assert_eq!(
            crate::media::canonical_review_wav_pcm_blake3(&target).unwrap(),
            request.expected_audio_content_hash
        );
    }

    #[test]
    fn worker_rejects_missing_nonempty_and_non_uuid_targets_without_creating_output() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.wav");
        write_test_wav(&source);

        let missing = directory.path().join(format!("{}.wav", uuid::Uuid::new_v4()));
        let raw = raw_file_blake3_before(&source, Instant::now() + Duration::from_secs(5)).unwrap();
        let expected = crate::export_bundle::current_canonical_pcm_blake3(&source).unwrap();
        let missing_request = MaterializeRequestV1 {
            schema: PROTOCOL_SCHEMA,
            source_path: std::fs::canonicalize(&source).unwrap(),
            target_path: missing.clone(),
            expected_audio_content_hash: expected.clone(),
            source_raw_blake3_before: raw.clone(),
        };
        let mut output = Vec::new();
        assert!(run_worker(serde_json::to_vec(&missing_request).unwrap().as_slice(), &mut output).is_err());
        assert!(!missing.exists(), "the worker must never create a caller-supplied target");
        assert!(output.is_empty());

        let nonempty = directory.path().join(format!("{}.wav", uuid::Uuid::new_v4()));
        std::fs::write(&nonempty, b"not empty").unwrap();
        let nonempty_request =
            MaterializeRequestV1 { target_path: std::fs::canonicalize(&nonempty).unwrap(), ..missing_request };
        assert!(run_worker(serde_json::to_vec(&nonempty_request).unwrap().as_slice(), &mut output).is_err());
        assert_eq!(std::fs::read(&nonempty).unwrap(), b"not empty");

        let named = directory.path().join("not-a-uuid.wav");
        std::fs::File::create(&named).unwrap();
        let named_request =
            MaterializeRequestV1 { target_path: std::fs::canonicalize(&named).unwrap(), ..nonempty_request };
        assert!(run_worker(serde_json::to_vec(&named_request).unwrap().as_slice(), &mut output).is_err());
        assert_eq!(std::fs::metadata(named).unwrap().len(), 0);
    }

    #[test]
    fn protocol_rejects_oversized_unknown_and_impossible_responses() {
        let mut output = Vec::new();
        for payload in [
            b"not-json".to_vec(),
            br#"{"schema":2,"source_path":"x","target_path":"y","expected_audio_content_hash":"x","source_raw_blake3_before":"y"}"#.to_vec(),
            vec![b'x'; WORKER_INPUT_LIMIT_BYTES + 1],
        ] {
            assert!(run_worker(payload.as_slice(), &mut output).is_err());
            assert!(output.is_empty());
        }
        assert!(parse_response(&vec![b'x'; WORKER_OUTPUT_LIMIT_BYTES + 1]).is_err());
        assert!(parse_response(
            br#"{"schema":1,"source_raw_blake3_after":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","canonical_pcm_blake3":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","output_bytes":0}"#
        )
        .is_err());
    }
}
