//! Killable technical-audio verification boundary.
//!
//! Symphonia is pure Rust, but a hostile or malformed container can still make a parser allocate or
//! stop making progress inside one call. Deadlines checked between decoder calls cannot contain that
//! case. Production therefore executes the probe in a fresh copy of the signed application before
//! Tauri starts. On Windows [`crate::engine_runtime::run_contained_command`] starts it suspended,
//! assigns it to a kill-on-close Job Object with a process-memory ceiling, and only then resumes it.
//! A timeout, crash, memory-limit termination, invalid reply, or output flood is inconclusive and can
//! never authorize a database write.

#[cfg(not(test))]
use crate::engine_runtime::{run_contained_command, ContainedCommandSpec};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const WORKER_ARG: &str = "--cortex-technical-audio-probe-worker";
const SELF_TEST_ARG: &str = "--cortex-technical-audio-probe-self-test";
const PROTOCOL_SCHEMA: u8 = 1;
const WORKER_INPUT_LIMIT_BYTES: usize = 256 * 1024;
const WORKER_OUTPUT_LIMIT_BYTES: usize = 4 * 1024;
#[cfg(not(test))]
const WORKER_STDERR_LIMIT_BYTES: usize = 4 * 1024;
const WORKER_INTERNAL_BUDGET: Duration = Duration::from_secs(10);
#[cfg(not(test))]
const WORKER_PARENT_TIMEOUT: Duration = Duration::from_secs(12);
#[cfg(not(test))]
const WORKER_PROCESS_MEMORY_LIMIT_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TechnicalAudioProbeObservation {
    DecodeFailed,
    MissingFile,
    PermissionDenied,
    CorruptContainer,
    Healthy,
    Inconclusive,
}

impl TechnicalAudioProbeObservation {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::DecodeFailed => "decodeFailed",
            Self::MissingFile => "missingFile",
            Self::PermissionDenied => "permissionDenied",
            Self::CorruptContainer => "corruptContainer",
            Self::Healthy => "healthy",
            Self::Inconclusive => "inconclusive",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TechnicalAudioFailureEvidence {
    pub(crate) observation: TechnicalAudioProbeObservation,
    /// BLAKE3 of the exact source bytes whose container/decode failure was reproduced. Missing and
    /// permission-denied files have no readable bytes; those conditions are rechecked directly.
    pub(crate) source_blake3: Option<String>,
}

impl TechnicalAudioFailureEvidence {
    fn inconclusive() -> Self {
        Self { observation: TechnicalAudioProbeObservation::Inconclusive, source_blake3: None }
    }

    fn is_canonical(&self) -> bool {
        let hash_is_canonical = self.source_blake3.as_deref().is_some_and(|hash| {
            hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        match self.observation {
            TechnicalAudioProbeObservation::DecodeFailed | TechnicalAudioProbeObservation::CorruptContainer => {
                hash_is_canonical
            }
            TechnicalAudioProbeObservation::MissingFile
            | TechnicalAudioProbeObservation::PermissionDenied
            | TechnicalAudioProbeObservation::Healthy
            | TechnicalAudioProbeObservation::Inconclusive => self.source_blake3.is_none(),
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeRequestV1 {
    schema: u8,
    path: PathBuf,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeResponseV1 {
    schema: u8,
    evidence: TechnicalAudioFailureEvidence,
}

/// Decode packets one at a time without materializing the file's PCM. A positive technical failure
/// may be returned as soon as the decoder proves it; reaching the deadline is inconclusive. Healthy
/// is returned only after a clean EOF with frames.
fn probe_decode_streaming_capped(
    file: std::fs::File,
    path: &Path,
    deadline: Instant,
) -> TechnicalAudioProbeObservation {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
    let mut format =
        match symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default()) {
            Ok(format) if Instant::now() < deadline => format,
            Ok(_) => return TechnicalAudioProbeObservation::Inconclusive,
            Err(symphonia::core::errors::Error::IoError(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return TechnicalAudioProbeObservation::MissingFile;
            }
            Err(symphonia::core::errors::Error::IoError(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                return TechnicalAudioProbeObservation::PermissionDenied;
            }
            Err(symphonia::core::errors::Error::IoError(_)) => {
                return TechnicalAudioProbeObservation::Inconclusive;
            }
            Err(_) => return TechnicalAudioProbeObservation::CorruptContainer,
        };
    let Some(track) =
        format.tracks().iter().find(|track| matches!(track.codec_params, Some(CodecParameters::Audio(_))))
    else {
        return TechnicalAudioProbeObservation::CorruptContainer;
    };
    let audio_params = match &track.codec_params {
        Some(CodecParameters::Audio(parameters)) => parameters.clone(),
        _ => return TechnicalAudioProbeObservation::CorruptContainer,
    };
    // PCM WAV has no codec checksum, but its RIFF data extent gives an exact per-channel frame
    // count. Do not apply this equality rule to compressed/gapless formats whose declared duration
    // may legitimately include encoder padding.
    let exact_declared_frames = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wav") || extension.eq_ignore_ascii_case("wave"))
        .then_some(track.num_frames)
        .flatten();
    let mut decoder = match symphonia::default::get_codecs()
        .make_audio_decoder(&audio_params, &AudioDecoderOptions::default().verify(true))
    {
        Ok(decoder) if Instant::now() < deadline => decoder,
        Ok(_) => return TechnicalAudioProbeObservation::Inconclusive,
        Err(_) => return TechnicalAudioProbeObservation::DecodeFailed,
    };
    let track_id = track.id;
    let mut decoded_frames = 0_u64;

    loop {
        if Instant::now() >= deadline {
            return TechnicalAudioProbeObservation::Inconclusive;
        }
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return TechnicalAudioProbeObservation::Inconclusive;
                }
                if exact_declared_frames.is_some_and(|declared| decoded_frames != declared) {
                    return TechnicalAudioProbeObservation::DecodeFailed;
                }
                if decoder.finalize().verify_ok == Some(false) {
                    return TechnicalAudioProbeObservation::DecodeFailed;
                }
                if Instant::now() >= deadline {
                    return TechnicalAudioProbeObservation::Inconclusive;
                }
                return if decoded_frames == 0 {
                    TechnicalAudioProbeObservation::DecodeFailed
                } else {
                    TechnicalAudioProbeObservation::Healthy
                };
            }
            Err(symphonia::core::errors::Error::IoError(ref error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                return if Instant::now() < deadline {
                    TechnicalAudioProbeObservation::DecodeFailed
                } else {
                    TechnicalAudioProbeObservation::Inconclusive
                };
            }
            Err(symphonia::core::errors::Error::IoError(ref error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return TechnicalAudioProbeObservation::MissingFile;
            }
            Err(symphonia::core::errors::Error::IoError(ref error))
                if error.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                return TechnicalAudioProbeObservation::PermissionDenied;
            }
            Err(symphonia::core::errors::Error::IoError(_)) => {
                return TechnicalAudioProbeObservation::Inconclusive;
            }
            Err(_) => {
                return if Instant::now() < deadline {
                    TechnicalAudioProbeObservation::DecodeFailed
                } else {
                    TechnicalAudioProbeObservation::Inconclusive
                };
            }
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) if Instant::now() < deadline => decoded,
            Ok(_) => return TechnicalAudioProbeObservation::Inconclusive,
            Err(_) if Instant::now() < deadline => return TechnicalAudioProbeObservation::DecodeFailed,
            Err(_) => return TechnicalAudioProbeObservation::Inconclusive,
        };
        decoded_frames = decoded_frames.saturating_add(decoded.frames() as u64);
    }
}

fn blake3_reader_before(file: &mut std::fs::File, deadline: Instant) -> std::io::Result<Option<String>> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 256 * 1024];
    loop {
        if Instant::now() >= deadline {
            return Ok(None);
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(hasher.finalize().to_hex().to_string()))
}

fn blake3_file_before(path: &Path, deadline: Instant) -> std::io::Result<Option<String>> {
    let mut file = std::fs::File::open(path)?;
    blake3_reader_before(&mut file, deadline)
}

fn probe_technical_audio_failure_local(path: &Path, deadline: Instant) -> TechnicalAudioFailureEvidence {
    let mut file = match std::fs::File::open(path) {
        Ok(file) if Instant::now() < deadline => file,
        Ok(_) => return TechnicalAudioFailureEvidence::inconclusive(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return TechnicalAudioFailureEvidence {
                observation: TechnicalAudioProbeObservation::MissingFile,
                source_blake3: None,
            };
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return TechnicalAudioFailureEvidence {
                observation: TechnicalAudioProbeObservation::PermissionDenied,
                source_blake3: None,
            };
        }
        Err(_) => return TechnicalAudioFailureEvidence::inconclusive(),
    };
    let source_before = match blake3_reader_before(&mut file, deadline) {
        Ok(Some(hash)) => hash,
        _ => return TechnicalAudioFailureEvidence::inconclusive(),
    };
    if file.rewind().is_err() || Instant::now() >= deadline {
        return TechnicalAudioFailureEvidence::inconclusive();
    }
    let observation = probe_decode_streaming_capped(file, path, deadline);
    let source_blake3 = if matches!(
        observation,
        TechnicalAudioProbeObservation::DecodeFailed | TechnicalAudioProbeObservation::CorruptContainer
    ) {
        blake3_file_before(path, deadline).ok().flatten().filter(|source_after| source_after == &source_before)
    } else {
        None
    };
    let evidence = TechnicalAudioFailureEvidence {
        observation: if matches!(
            observation,
            TechnicalAudioProbeObservation::DecodeFailed | TechnicalAudioProbeObservation::CorruptContainer
        ) && source_blake3.is_none()
        {
            TechnicalAudioProbeObservation::Inconclusive
        } else {
            observation
        },
        source_blake3,
    };
    if evidence.is_canonical() {
        evidence
    } else {
        TechnicalAudioFailureEvidence::inconclusive()
    }
}

fn parse_worker_response(bytes: &[u8]) -> Result<TechnicalAudioFailureEvidence, String> {
    if bytes.is_empty() || bytes.len() > WORKER_OUTPUT_LIMIT_BYTES {
        return Err("technical-audio probe response size is invalid".to_string());
    }
    let response: ProbeResponseV1 = serde_json::from_slice(bytes)
        .map_err(|_| "technical-audio probe response is not canonical JSON".to_string())?;
    if response.schema != PROTOCOL_SCHEMA || !response.evidence.is_canonical() {
        return Err("technical-audio probe response failed its schema or evidence contract".to_string());
    }
    Ok(response.evidence)
}

#[cfg(not(test))]
fn contained_probe(path: &Path) -> Result<TechnicalAudioFailureEvidence, String> {
    let request = serde_json::to_vec(&ProbeRequestV1 { schema: PROTOCOL_SCHEMA, path: path.to_path_buf() })
        .map_err(|_| "technical-audio probe request could not be encoded".to_string())?;
    if request.is_empty() || request.len() > WORKER_INPUT_LIMIT_BYTES {
        return Err("technical-audio probe request exceeds its fixed input limit".to_string());
    }
    let executable = std::env::current_exe()
        .map_err(|error| format!("technical-audio probe executable could not be resolved: {error}"))?;
    let mut command = Command::new(executable);
    command.arg(WORKER_ARG).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
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
    .map_err(|error| format!("technical-audio probe worker was not authoritative: {error}"))?;
    parse_worker_response(&output.stdout)
}

fn fail_closed_probe_result(result: Result<TechnicalAudioFailureEvidence, String>) -> TechnicalAudioFailureEvidence {
    result.unwrap_or_else(|error| {
        tracing::warn!(%error, "Technical-audio probe failed closed without authorizing a review effect");
        TechnicalAudioFailureEvidence::inconclusive()
    })
}

/// Production always crosses the killable subprocess boundary. Unit tests use the same local probe
/// deterministically because their `current_exe` is Rust's test harness, not the application binary;
/// containment itself is covered by dedicated real-process fault drills in `engine_runtime`.
pub(crate) fn probe_technical_audio_failure(path: &Path, deadline: Instant) -> TechnicalAudioFailureEvidence {
    #[cfg(test)]
    {
        probe_technical_audio_failure_local(path, deadline)
    }
    #[cfg(not(test))]
    {
        let _ = deadline;
        fail_closed_probe_result(contained_probe(path))
    }
}

#[cfg(test)]
pub(crate) fn technical_audio_failure_evidence_is_current(
    path: &Path,
    evidence: &TechnicalAudioFailureEvidence,
) -> bool {
    match evidence.observation {
        TechnicalAudioProbeObservation::MissingFile => {
            std::fs::File::open(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        }
        TechnicalAudioProbeObservation::PermissionDenied => {
            std::fs::File::open(path).is_err_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
        }
        TechnicalAudioProbeObservation::DecodeFailed | TechnicalAudioProbeObservation::CorruptContainer => {
            evidence.source_blake3.as_deref().is_some_and(|expected| {
                blake3_file_before(path, Instant::now() + WORKER_INTERNAL_BUDGET)
                    .is_ok_and(|actual| actual.as_deref() == Some(expected))
            })
        }
        TechnicalAudioProbeObservation::Healthy | TechnicalAudioProbeObservation::Inconclusive => false,
    }
}

/// A readable failed source held under the Windows sharing contract that was revalidated after the
/// contained probe. Keeping this value alive denies new write/delete access to the opened file until
/// the durable review transaction finishes.
///
/// The public product is Windows-only. Non-Windows builds deliberately cannot construct this lease:
/// a plain POSIX read handle does not freeze the pathname or prevent writers and must not be presented
/// as equivalent proof.
pub(crate) struct TechnicalAudioSourceLease {
    _sealed_source: std::fs::File,
}

/// Revalidate positive technical-failure evidence immediately before database admission and return an
/// OS lease that must be retained through the durable write. Only readable existing-file failures can
/// cross this boundary. A missing path has no object on which Windows can hold a deny-delete lease, so
/// it is rejected rather than authorizing truth from an inherently racy negative directory entry.
pub(crate) fn acquire_technical_audio_source_lease(
    path: &Path,
    evidence: &TechnicalAudioFailureEvidence,
    deadline: Instant,
) -> Result<TechnicalAudioSourceLease, String> {
    if !evidence.is_canonical() {
        return Err("technical-audio evidence is not canonical".to_string());
    }
    match evidence.observation {
        TechnicalAudioProbeObservation::MissingFile => {
            Err("missing-file technical-audio evidence cannot acquire an immutable source lease".to_string())
        }
        TechnicalAudioProbeObservation::DecodeFailed | TechnicalAudioProbeObservation::CorruptContainer => {
            let expected = evidence
                .source_blake3
                .as_deref()
                .ok_or_else(|| "technical-audio existing-file evidence has no source hash".to_string())?;

            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt as _;

                // Permit concurrent readers (including playback), but deny both write and delete
                // sharing. Windows checks this against already-open handles as well as future opens:
                // an admitted writer therefore makes acquisition fail rather than creating a false
                // lease, and a later writer/rename/delete is rejected until this handle is dropped.
                const FILE_SHARE_READ: u32 = 0x0000_0001;
                let mut options = std::fs::OpenOptions::new();
                options.read(true).share_mode(FILE_SHARE_READ);
                let mut source = options
                    .open(path)
                    .map_err(|error| format!("technical-audio source could not be sealed read-only: {error}"))?;
                if !source
                    .metadata()
                    .map_err(|error| format!("technical-audio sealed source metadata could not be read: {error}"))?
                    .is_file()
                {
                    return Err("technical-audio source is not a regular file".to_string());
                }
                let actual = blake3_reader_before(&mut source, deadline)
                    .map_err(|error| format!("technical-audio sealed source could not be hashed: {error}"))?
                    .ok_or_else(|| "technical-audio sealed-source hash exceeded its deadline".to_string())?;
                if actual != expected {
                    return Err("technical-audio source changed before its immutable lease was acquired".to_string());
                }
                Ok(TechnicalAudioSourceLease { _sealed_source: source })
            }

            #[cfg(not(windows))]
            {
                let _ = (path, expected, deadline);
                Err("existing-file technical-audio commits require the supported Windows lease boundary".to_string())
            }
        }
        TechnicalAudioProbeObservation::PermissionDenied => {
            Err("permission-denied technical-audio evidence cannot acquire the required readable source lease"
                .to_string())
        }
        TechnicalAudioProbeObservation::Healthy | TechnicalAudioProbeObservation::Inconclusive => {
            Err("non-failure technical-audio evidence cannot authorize a source lease".to_string())
        }
    }
}

fn read_worker_request(mut input: impl Read) -> Result<ProbeRequestV1, String> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take((WORKER_INPUT_LIMIT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "technical-audio probe request could not be read".to_string())?;
    if bytes.is_empty() || bytes.len() > WORKER_INPUT_LIMIT_BYTES {
        return Err("technical-audio probe request size is invalid".to_string());
    }
    let request: ProbeRequestV1 = serde_json::from_slice(&bytes)
        .map_err(|_| "technical-audio probe request is not canonical JSON".to_string())?;
    if request.schema != PROTOCOL_SCHEMA || request.path.as_os_str().is_empty() {
        return Err("technical-audio probe request failed its schema contract".to_string());
    }
    Ok(request)
}

fn run_worker(mut input: impl Read, mut output: impl std::io::Write) -> Result<(), String> {
    let request = read_worker_request(&mut input)?;
    let evidence = probe_technical_audio_failure_local(&request.path, Instant::now() + WORKER_INTERNAL_BUDGET);
    if !evidence.is_canonical() {
        return Err("technical-audio probe produced non-canonical evidence".to_string());
    }
    let response = serde_json::to_vec(&ProbeResponseV1 { schema: PROTOCOL_SCHEMA, evidence })
        .map_err(|_| "technical-audio probe response could not be encoded".to_string())?;
    if response.len() > WORKER_OUTPUT_LIMIT_BYTES {
        return Err("technical-audio probe response exceeded its fixed limit".to_string());
    }
    output
        .write_all(&response)
        .and_then(|()| output.flush())
        .map_err(|_| "technical-audio probe response could not be written".to_string())
}

/// Called before Tauri initialization. Returning `Some` means this process was intentionally
/// launched in the non-UI probe role and the caller must exit with the returned code.
pub fn run_special_process_mode() -> Option<i32> {
    match std::env::args().nth(1).as_deref() {
        Some(WORKER_ARG) => match run_worker(std::io::stdin().lock(), std::io::stdout().lock()) {
            Ok(()) => Some(0),
            Err(_) => {
                // Deliberately omit the path, decoder detail, and raw payload. The parent treats
                // every non-zero exit as inconclusive and retains at most a fixed stderr body.
                eprintln!("technical-audio probe worker rejected its request or could not finish it");
                Some(2)
            }
        },
        Some(SELF_TEST_ARG) => {
            #[cfg(not(test))]
            {
                let definitely_missing =
                    std::env::temp_dir().join(format!("cortex-technical-probe-self-test-{}.wav", uuid::Uuid::new_v4()));
                let passed = contained_probe(&definitely_missing)
                    .is_ok_and(|evidence| evidence.observation == TechnicalAudioProbeObservation::MissingFile);
                Some(if passed { 0 } else { 3 })
            }
            #[cfg(test)]
            {
                // The unit-test executable is libtest rather than the application binary. Real
                // process composition is exercised by invoking this mode on the built app.
                Some(3)
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_rejects_invalid_oversized_and_unknown_payloads_without_output() {
        for payload in [
            b"not-json".to_vec(),
            br#"{"schema":2,"path":"C:/audio.wav"}"#.to_vec(),
            br#"{"schema":1,"path":"","extra":true}"#.to_vec(),
            vec![b'x'; WORKER_INPUT_LIMIT_BYTES + 1],
        ] {
            let mut output = Vec::new();
            assert!(run_worker(payload.as_slice(), &mut output).is_err());
            assert!(output.is_empty(), "an invalid request must not produce a partial response");
        }
    }

    #[test]
    fn response_parser_rejects_invalid_payload_and_impossible_evidence_pairs() {
        let invalid = [
            br#"{"schema":1,"evidence":{"observation":"decodeFailed","source_blake3":null}}"#.as_slice(),
            br#"{"schema":1,"evidence":{"observation":"healthy","source_blake3":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#.as_slice(),
            br#"{"schema":1,"evidence":{"observation":"future","source_blake3":null}}"#.as_slice(),
            br#"{"schema":1,"evidence":{"observation":"healthy","source_blake3":null},"extra":1}"#.as_slice(),
        ];
        for payload in invalid {
            assert!(parse_worker_response(payload).is_err(), "invalid response was accepted");
        }
        assert!(parse_worker_response(&vec![b'x'; WORKER_OUTPUT_LIMIT_BYTES + 1]).is_err());
    }

    #[test]
    fn local_worker_round_trip_is_canonical_and_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.wav");
        let request = serde_json::to_vec(&ProbeRequestV1 { schema: PROTOCOL_SCHEMA, path: missing }).unwrap();
        let mut output = Vec::new();
        run_worker(request.as_slice(), &mut output).unwrap();
        assert!(output.len() <= WORKER_OUTPUT_LIMIT_BYTES);
        let evidence = parse_worker_response(&output).unwrap();
        assert_eq!(evidence.observation, TechnicalAudioProbeObservation::MissingFile);
        assert!(evidence.source_blake3.is_none());
    }

    #[test]
    fn every_containment_or_protocol_error_becomes_non_authoritative_evidence() {
        let evidence = fail_closed_probe_result(Err("simulated timeout, crash, or invalid response".to_string()));
        assert_eq!(evidence, TechnicalAudioFailureEvidence::inconclusive());
        assert!(!technical_audio_failure_evidence_is_current(Path::new("unused"), &evidence));
    }
}
