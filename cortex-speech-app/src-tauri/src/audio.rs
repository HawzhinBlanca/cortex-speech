use crate::error::{AppError, AppResult, AudioError};
use lru::LruCache;
use ort::session::Session;
use ort::value::Tensor;
use std::io::Read;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::Duration;

pub const TARGET_SAMPLE_RATE: u32 = 16000;

/// Returns true if the PCM array is completely silent or only contains micro-noise.
pub fn is_silent(pcm: &[i16]) -> bool {
    // 50 is a very low threshold for 16-bit PCM (max 32768). unsigned_abs (not abs) so a -32768
    // sample can't overflow-panic (debug) / wrap (release) the silence check.
    pcm.iter().all(|&sample| sample.unsigned_abs() < 50)
}

/// Normalize the gain of an f32 PCM buffer to `target_rms_db` (e.g. -20.0).
///
/// This is applied **before** denoising and ASR inference so that audio recorded
/// at very low levels (phone calls, distant mics) doesn't produce empty transcripts
/// due to near-zero activations in the acoustic model.
///
/// Silent audio (RMS < 1e-8) is left untouched to avoid amplifying pure noise.
/// A hard peak limiter at ±0.99 FS prevents clipping after gain application.
pub fn normalize_pcm_rms(pcm: &mut [f32], target_rms_db: f32) {
    if pcm.is_empty() {
        return;
    }
    let rms = (pcm.iter().map(|&s| s * s).sum::<f32>() / pcm.len() as f32).sqrt();
    if rms < 1e-8 {
        return; // silence — skip to avoid amplifying noise floor
    }
    let rms_db = 20.0 * rms.log10();
    let gain_db = target_rms_db - rms_db;
    let gain_linear = 10.0_f32.powf(gain_db / 20.0);
    for s in pcm.iter_mut() {
        *s = (*s * gain_linear).clamp(-0.99, 0.99);
    }
}

/// Decode window for long-form audio (~90s of source audio per chunk).
pub const DECODE_WINDOW_MS: u32 = 90_000;

/// One mono 16-bit PCM window at `TARGET_SAMPLE_RATE`, with source-file time offset.
#[derive(Debug, Clone)]
pub struct PcmWindow {
    pub offset_ms: i64,
    pub sample_rate: u32,
    pub pcm: Vec<i16>,
}

/// Small LRU cache for decoded PCM data keyed by file content hash.
#[allow(clippy::type_complexity)]
static PCM_CACHE: LazyLock<Mutex<LruCache<String, (u32, Vec<i16>)>>> =
    LazyLock::new(|| Mutex::new(LruCache::new(pcm_cache_capacity())));

fn pcm_cache_key(path: &Path) -> AppResult<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn pcm_cache_capacity() -> NonZeroUsize {
    NonZeroUsize::new(10).unwrap_or(NonZeroUsize::MIN)
}

fn lock_pcm_cache() -> MutexGuard<'static, LruCache<String, (u32, Vec<i16>)>> {
    PCM_CACHE.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("Recovering poisoned PCM decode cache");
        poisoned.into_inner()
    })
}

/// Quick audio file metadata (no full decode).
#[derive(Debug, Clone)]
pub struct AudioInfo {
    pub duration_ms: i64,
    pub sample_rate: u32,
    pub channels: u16,
    pub format: String,
}

/// Duration in milliseconds from a per-channel frame count and sample rate.
///
/// Symphonia's `n_frames` is the per-channel frame count (one frame = one PCM sample across ALL
/// channels), so duration is `n_frames / sample_rate` and NEVER depends on channel count. Dividing
/// by channels (as an old `get_duration_ms` special case did for m4a/mp4/mov/3gp) halves a stereo
/// clip's duration. Both [`check_audio_file`] and [`get_duration_ms`] route through here so they can
/// never disagree on the same file.
fn frames_to_duration_ms(n_frames: u64, sample_rate: f64) -> i64 {
    if sample_rate <= 0.0 {
        return 0;
    }
    (n_frames as f64 / sample_rate * 1000.0) as i64
}

/// Validate that a file is a readable audio file and return its basic info.
pub fn check_audio_file<P: AsRef<Path>>(path: P) -> AppResult<AudioInfo> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(AppError::Audio(AudioError::Decode(format!("File not found: {}", path.display()))));
    }
    let metadata = std::fs::metadata(path)
        .map_err(|e| AppError::Audio(AudioError::Decode(format!("Cannot read metadata: {e}"))))?;
    if metadata.len() == 0 {
        return Err(AppError::Audio(AudioError::Decode(format!("File is empty: {}", path.display()))));
    }

    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file =
        std::fs::File::open(path).map_err(|e| AppError::Audio(AudioError::Decode(format!("Cannot open: {e}"))))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| AppError::Audio(AudioError::Decode(format!("Cannot probe format: {e}"))))?;

    let track = probed
        .format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| AppError::Audio(AudioError::NoTracks(path.to_path_buf())))?;

    let params = &track.codec_params;
    let n_frames = params.n_frames.unwrap_or(0);
    let sample_rate = params.sample_rate.unwrap_or(TARGET_SAMPLE_RATE) as f64;
    let channels = params.channels.map(|c| c.count() as u16).unwrap_or(1);

    if sample_rate <= 0.0 {
        return Err(AppError::Audio(AudioError::Decode("Invalid sample rate in file".into())));
    }

    let format = path.extension().and_then(|e| e.to_str()).unwrap_or("unknown").to_string();

    Ok(AudioInfo {
        duration_ms: frames_to_duration_ms(n_frames, sample_rate),
        sample_rate: sample_rate as u32,
        channels,
        format,
    })
}

/// Decode any audio file to 16kHz mono 16-bit PCM using symphonia.
/// Results are cached in a small LRU to avoid re-decoding the same file.
pub fn decode_to_pcm<P: AsRef<Path>>(path: P) -> AppResult<(u32, Vec<i16>)> {
    let path_str = path.as_ref().to_string_lossy().to_string();
    let cache_key = pcm_cache_key(path.as_ref())?;

    // Check cache first
    {
        let mut cache = lock_pcm_cache();
        if let Some(cached) = cache.get(&cache_key) {
            return Ok(cached.clone());
        }
    }

    let _span = crate::telemetry::TRACER
        .start_span("audio.decode_to_pcm", crate::telemetry::Tracer::metadata(vec![("path", path_str.clone())]));

    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path.as_ref()).map_err(AppError::Io)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.as_ref().extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let fmt_opts = FormatOptions::default();
    let meta_opts = MetadataOptions::default();
    let dec_opts = DecoderOptions::default();

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &fmt_opts, &meta_opts)
        .map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| AppError::Audio(AudioError::NoTracks(path.as_ref().to_path_buf())))?;

    let codec_params = track.codec_params.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &dec_opts)
        .map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;

    let track_id = track.id;
    let mut all_samples: Vec<f32> = Vec::new();
    let mut actual_channels = 0u32;
    let mut actual_sample_rate = 0u32;

    loop {
        let packet = match format.next_packet() {
            Ok(pkt) => pkt,
            Err(symphonia::core::errors::Error::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = decoder.decode(&packet).map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;
        if decoded.spec().channels.count() == 0 {
            continue;
        }

        if actual_channels == 0 {
            actual_channels = decoded.spec().channels.count() as u32;
            actual_sample_rate = decoded.spec().rate;
        }

        let mut sample_buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        sample_buf.copy_interleaved_ref(decoded);
        let samples = sample_buf.samples();
        all_samples.extend_from_slice(samples);
    }

    if all_samples.is_empty() {
        return Err(AppError::Audio(AudioError::EmptyBuffer));
    }

    let channels = if actual_channels > 0 {
        actual_channels
    } else {
        codec_params.channels.map(|c| c.count()).unwrap_or(1) as u32
    };
    let sample_rate = if actual_sample_rate > 0 {
        actual_sample_rate
    } else {
        codec_params.sample_rate.unwrap_or(TARGET_SAMPLE_RATE)
    };
    let pcm = interleaved_f32_to_pcm_i16(&all_samples, channels, sample_rate);

    let result = (TARGET_SAMPLE_RATE, pcm);

    // Store in cache
    let mut cache = lock_pcm_cache();
    cache.put(cache_key, result.clone());

    Ok(result)
}

/// Decode long audio in time windows; calls `on_window` for each chunk (16 kHz mono PCM).
pub fn decode_pcm_windows<P, F>(path: P, window_ms: u32, mut on_window: F) -> AppResult<()>
where
    P: AsRef<Path>,
    F: FnMut(PcmWindow) -> AppResult<()>,
{
    let path = path.as_ref();
    let _span = crate::telemetry::TRACER.start_span(
        "audio.decode_pcm_windows",
        crate::telemetry::Tracer::metadata(vec![("path", path.to_string_lossy().to_string())]),
    );

    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(AppError::Io)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| AppError::Audio(AudioError::NoTracks(path.to_path_buf())))?;

    let codec_params = track.codec_params.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;

    let track_id = track.id;
    let mut channels = codec_params.channels.map(|c| c.count()).unwrap_or(1) as u32;
    let mut sample_rate = codec_params.sample_rate.unwrap_or(TARGET_SAMPLE_RATE);
    let mut window_frames = ((window_ms as u64) * sample_rate as u64 / 1000).max(1) as usize;
    let mut window_cap = window_frames * channels as usize;
    let mut spec_updated = false;

    let mut buf: Vec<f32> = Vec::new();
    let mut output_offset_ms: i64 = 0;

    let mut emit_window = |samples: &[f32], ch: u32, sr: u32| -> AppResult<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let pcm = interleaved_f32_to_pcm_i16(samples, ch, sr);
        if pcm.is_empty() {
            return Ok(());
        }
        let window = PcmWindow { offset_ms: output_offset_ms, sample_rate: TARGET_SAMPLE_RATE, pcm: pcm.clone() };
        output_offset_ms += (pcm.len() as i64 * 1000) / TARGET_SAMPLE_RATE as i64;
        on_window(window)?;
        Ok(())
    };

    loop {
        let packet = match format.next_packet() {
            Ok(pkt) => pkt,
            Err(symphonia::core::errors::Error::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = decoder.decode(&packet).map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;
        if decoded.spec().channels.count() == 0 {
            continue;
        }

        if !spec_updated {
            channels = decoded.spec().channels.count() as u32;
            sample_rate = decoded.spec().rate;
            window_frames = ((window_ms as u64) * sample_rate as u64 / 1000).max(1) as usize;
            window_cap = window_frames * channels as usize;
            spec_updated = true;
        }

        let mut sample_buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        sample_buf.copy_interleaved_ref(decoded);
        buf.extend_from_slice(sample_buf.samples());

        while buf.len() >= window_cap {
            let chunk: Vec<f32> = buf.drain(..window_cap).collect();
            emit_window(&chunk, channels, sample_rate)?;
        }
    }

    if !buf.is_empty() {
        emit_window(&buf, channels, sample_rate)?;
    }

    Ok(())
}

/// Like [`decode_pcm_windows`] but fails if decoding exceeds `timeout`.
pub fn decode_pcm_windows_with_timeout<P, F>(path: P, window_ms: u32, timeout: Duration, on_window: F) -> AppResult<()>
where
    P: AsRef<Path> + Send + 'static,
    F: FnMut(PcmWindow) -> AppResult<()> + Send + 'static,
{
    let path_buf = path.as_ref().to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = decode_pcm_windows(&path_buf, window_ms, on_window);
        send_decode_worker_result(tx, result, "decode_pcm_windows");
    });

    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(AppError::Audio(AudioError::Decode(format!("Audio decode timed out after {:?}", timeout))))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(AppError::Audio(AudioError::Decode("Audio decode worker thread disconnected".into())))
        }
    }
}

/// Decode audio to PCM with a configurable timeout for the symphonia decoder.
/// Returns an error if decoding does not complete within the given duration.
pub fn decode_to_pcm_with_timeout<P: AsRef<Path>>(path: P, timeout: Duration) -> AppResult<(u32, Vec<i16>)> {
    let path = path.as_ref().to_string_lossy().to_string();
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = decode_to_pcm(&path);
        send_decode_worker_result(tx, result, "decode_to_pcm");
    });

    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(AppError::Audio(AudioError::Decode(format!("Audio decode timed out after {:?}", timeout))))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(AppError::Audio(AudioError::Decode("Audio decode worker thread disconnected".into())))
        }
    }
}

fn send_decode_worker_result<T>(
    tx: std::sync::mpsc::Sender<AppResult<T>>,
    result: AppResult<T>,
    operation: &'static str,
) {
    if tx.send(result).is_err() {
        tracing::warn!("Audio decode worker could not send {operation} result; receiver was dropped or timed out");
    }
}

/// Get audio duration in milliseconds.
pub fn get_duration_ms<P: AsRef<Path>>(path: P) -> AppResult<i64> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path.as_ref())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.as_ref().extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;
    let track = probed
        .format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| AppError::Audio(AudioError::NoTracks(path.as_ref().to_path_buf())))?;

    let params = &track.codec_params;
    let n_frames = params.n_frames.unwrap_or(0);
    let sample_rate = params.sample_rate.unwrap_or(TARGET_SAMPLE_RATE) as f64;

    if sample_rate <= 0.0 {
        return Err(AppError::Audio(AudioError::Decode("Invalid sample rate".into())));
    }

    Ok(frames_to_duration_ms(n_frames, sample_rate))
}

/// Clear the PCM decode cache.
pub fn clear_pcm_cache() {
    lock_pcm_cache().clear();
}

/// Compute a decimated waveform for visualization using parallel peak extraction.
pub fn compute_waveform(pcm: &[i16], num_points: usize) -> Vec<f32> {
    if pcm.is_empty() || num_points == 0 {
        return Vec::new();
    }

    let chunk_size = (pcm.len() / num_points).max(1);
    use rayon::prelude::*;
    pcm.par_chunks(chunk_size)
        .take(num_points)
        .map(|chunk| {
            (chunk.iter().map(|&s| (s as f32 / i16::MAX as f32).powi(2)).sum::<f32>() / chunk.len() as f32).sqrt()
        })
        .collect()
}

fn interleaved_f32_to_pcm_i16(samples: &[f32], channels: u32, sample_rate: u32) -> Vec<i16> {
    let mono = if channels > 1 { downmix_to_mono(samples, channels) } else { samples.to_vec() };
    let resampled =
        if sample_rate != TARGET_SAMPLE_RATE { resample(&mono, sample_rate, TARGET_SAMPLE_RATE) } else { mono };
    resampled.iter().map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).collect()
}

fn downmix_to_mono(samples: &[f32], channels: u32) -> Vec<f32> {
    let frame_count = samples.len() / channels as usize;
    let mut mono = Vec::with_capacity(frame_count);
    for i in 0..frame_count {
        let sum: f32 = samples[i * channels as usize..(i + 1) * channels as usize].iter().sum();
        mono.push(sum / channels as f32);
    }
    mono
}

/// Resample decoded PCM to 16 kHz when the decode path did not already normalize rate.
pub fn ensure_pcm_16khz(sample_rate: u32, pcm: Vec<i16>) -> AppResult<(u32, Vec<i16>)> {
    if sample_rate == TARGET_SAMPLE_RATE {
        return Ok((sample_rate, pcm));
    }
    let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
    let resampled = resample(&f32_pcm, sample_rate, TARGET_SAMPLE_RATE);
    let out: Vec<i16> = resampled.iter().map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).collect();
    Ok((TARGET_SAMPLE_RATE, out))
}

/// Whether a decode failure is likely transient and worth one retry.
pub fn is_transient_decode_error(err: &AppError) -> bool {
    match err {
        AppError::Audio(AudioError::Decode(msg)) => {
            let lower = msg.to_lowercase();
            lower.contains("timed out")
                || lower.contains("timeout")
                || lower.contains("disconnected")
                || lower.contains("worker thread")
                || lower.contains("temporarily")
        }
        AppError::Io(_) => true,
        _ => false,
    }
}

pub(crate) fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = to_rate as f64 / from_rate as f64;
    let new_len = (samples.len() as f64 * ratio).ceil() as usize;
    let mut out = Vec::with_capacity(new_len);

    for i in 0..new_len {
        let src_idx = i as f64 / ratio;
        let lo = src_idx.floor() as usize;
        let hi = (lo + 1).min(samples.len().saturating_sub(1));
        let frac = src_idx - lo as f64;
        let interpolated = samples[lo] as f64 * (1.0 - frac) + samples[hi] as f64 * frac;
        out.push(interpolated as f32);
    }
    out
}

pub struct SileroVad {
    session: std::sync::Arc<std::sync::Mutex<Session>>,
    state: Vec<f32>,
    state_dims: (usize, usize, usize),
    sample_rate: u32,
    frame_size: usize,
    threshold: f32,
    min_speech_frames: usize,
    min_silence_frames: usize,
}

impl SileroVad {
    pub fn new(model_path: &Path, sample_rate: u32, threshold: f32) -> AppResult<Self> {
        let session = Session::builder()
            .map_err(|e| AppError::Onnx(format!("VAD session builder: {e}")))?
            .commit_from_file(model_path)
            .map_err(|e| AppError::Onnx(format!("VAD model load: {e}")))?;

        let mut state_dims = (2usize, 1usize, 128usize);
        for input in session.inputs() {
            if input.name() == "state" {
                if let ort::value::ValueType::Tensor { shape, .. } = input.dtype() {
                    let dims: Vec<usize> = shape.iter().map(|&d| if d <= 0 { 1 } else { d as usize }).collect();
                    if dims.len() == 3 {
                        state_dims = (dims[0].max(1), dims[1].max(1), dims[2].max(1));
                    }
                }
            }
        }

        let state_size = state_dims.0 * state_dims.1 * state_dims.2;

        Ok(Self {
            session: std::sync::Arc::new(std::sync::Mutex::new(session)),
            state: vec![0.0; state_size],
            state_dims,
            sample_rate,
            frame_size: 512,
            threshold,
            min_speech_frames: 15,
            min_silence_frames: 8,
        })
    }

    /// Build a `SileroVad` from an already-loaded ONNX `Session`.
    /// The state_dims are discovered by inspecting the session inputs.
    pub fn new_with_session(
        cached_session: &std::sync::Arc<std::sync::Mutex<Session>>,
        sample_rate: u32,
        threshold: f32,
    ) -> AppResult<Self> {
        let session = cached_session.clone();

        let mut state_dims = (2usize, 1usize, 128usize);
        for input in session.lock().unwrap_or_else(|e| e.into_inner()).inputs() {
            if input.name() == "state" {
                if let ort::value::ValueType::Tensor { shape, .. } = input.dtype() {
                    let dims: Vec<usize> = shape.iter().map(|&d| if d <= 0 { 1 } else { d as usize }).collect();
                    if dims.len() == 3 {
                        state_dims = (dims[0].max(1), dims[1].max(1), dims[2].max(1));
                    }
                }
            }
        }

        let state_size = state_dims.0 * state_dims.1 * state_dims.2;

        Ok(Self {
            session,
            state: vec![0.0; state_size],
            state_dims,
            sample_rate,
            frame_size: 512,
            threshold,
            min_speech_frames: 15,
            min_silence_frames: 8,
        })
    }

    /// Build a `SileroVad` from an already-loaded ONNX `Session` with **known** (already discovered) state_dims.
    /// This avoids the redundant input inspection that `new_with_session` does on every call.
    pub fn new_with_session_cached(
        cached_session: &std::sync::Arc<std::sync::Mutex<Session>>,
        sample_rate: u32,
        threshold: f32,
        state_dims: (usize, usize, usize),
    ) -> AppResult<Self> {
        let session = cached_session.clone();
        let state_size = state_dims.0 * state_dims.1 * state_dims.2;

        Ok(Self {
            session,
            // Reuse cached state if the sample rate matches and state_dims are identical.
            state: vec![0.0; state_size],
            state_dims,
            sample_rate,
            frame_size: 512,
            threshold,
            min_speech_frames: 15,
            min_silence_frames: 8,
        })
    }

    pub fn detect(&mut self, pcm: &[i16]) -> AppResult<Vec<(usize, usize)>> {
        let f32_pcm: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let sr_val = 16000i64;

        let (vad_pcm, sample_ratio) = if self.sample_rate != 16000 {
            let resampled = resample(&f32_pcm, self.sample_rate, 16000);
            (resampled, self.sample_rate as f64 / 16000.0)
        } else {
            (f32_pcm, 1.0)
        };

        let mut speech_probs = Vec::new();
        let mut state = self.state.clone();

        for chunk in vad_pcm.chunks(self.frame_size) {
            if chunk.len() < self.frame_size {
                break;
            }

            let input_nd = ndarray::Array2::from_shape_vec((1, self.frame_size), chunk.to_vec())
                .map_err(|e| AppError::Onnx(format!("VAD input reshape: {e}")))?;
            let input_tensor =
                Tensor::from_array(input_nd).map_err(|e| AppError::Onnx(format!("VAD input tensor: {e}")))?;

            let sr_arr = ndarray::arr0(sr_val);
            let sr_tensor = Tensor::from_array(sr_arr).map_err(|e| AppError::Onnx(format!("VAD sr tensor: {e}")))?;

            let (d0, d1, d2) = self.state_dims;
            let state_nd = ndarray::Array3::from_shape_vec((d0, d1, d2), state.clone())
                .map_err(|e| AppError::Onnx(format!("VAD state reshape: {e}")))?;
            let state_tensor =
                Tensor::from_array(state_nd).map_err(|e| AppError::Onnx(format!("VAD state tensor: {e}")))?;

            let mut session_guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
            let outputs = session_guard
                .run(ort::inputs![
                    "input" => input_tensor,
                    "sr" => sr_tensor,
                    "state" => state_tensor,
                ])
                .map_err(|e| AppError::Onnx(format!("VAD inference: {e}")))?;

            let prob: f32 = outputs["output"]
                .try_extract_tensor::<f32>()
                .map(|(_, data)| data.first().copied().unwrap_or(0.0))
                .unwrap_or(0.0);

            if let Some(sn_val) = outputs.get("stateN") {
                if let Ok((_, data)) = sn_val.try_extract_tensor::<f32>() {
                    state = data.to_vec();
                }
            }

            drop(outputs);
            speech_probs.push(prob);
        }

        self.state = state;

        let segments = self.probs_to_segments(&speech_probs, vad_pcm.len())?;

        if (sample_ratio - 1.0).abs() > 0.01 {
            let mapped: Vec<(usize, usize)> = segments
                .into_iter()
                .map(|(start, end)| {
                    let mapped_start = (start as f64 * sample_ratio) as usize;
                    let mapped_end = (end as f64 * sample_ratio) as usize;
                    (mapped_start, mapped_end)
                })
                .map(|(s, e)| {
                    let capped_end = e.min(pcm.len());
                    let capped_start = s.min(capped_end);
                    (capped_start, capped_end)
                })
                .collect();
            Ok(mapped)
        } else {
            let capped: Vec<(usize, usize)> = segments.into_iter().map(|(s, e)| (s, e.min(pcm.len()))).collect();
            Ok(capped)
        }
    }

    fn probs_to_segments(&self, probs: &[f32], total_samples: usize) -> AppResult<Vec<(usize, usize)>> {
        let frame_hop = self.frame_size;
        let mut segments = Vec::new();
        let mut in_speech = false;
        let mut speech_start = 0usize;
        let mut speech_frame_count = 0usize;
        let mut silence_frame_count = 0usize;

        for (i, &prob) in probs.iter().enumerate() {
            if prob >= self.threshold {
                if !in_speech {
                    in_speech = true;
                    speech_start = i * frame_hop;
                }
                speech_frame_count += 1;
                silence_frame_count = 0;
            } else {
                silence_frame_count += 1;
                if in_speech && silence_frame_count >= self.min_silence_frames {
                    let end = i * frame_hop;
                    if speech_frame_count >= self.min_speech_frames {
                        segments.push((speech_start, end.min(total_samples)));
                    }
                    in_speech = false;
                    speech_frame_count = 0;
                }
            }
        }

        if in_speech && speech_frame_count >= self.min_speech_frames {
            segments.push((speech_start, total_samples));
        }

        if segments.is_empty() {
            segments.push((0, total_samples));
        }

        Ok(segments)
    }
}

type VadCacheEntry = Option<(std::sync::Arc<std::sync::Mutex<Session>>, (usize, usize, usize))>;

/// Caches a lazily-loaded VAD handle and its discovered state_dims in a module-level
/// `LazyLock` so that callers that invoke `detect()` consecutively (as in `proptest`,
/// ~64 cases) only pay the ONNX model load cost once and avoid redundant state_dims
/// discovery on subsequent calls.
static VAD_CACHE: std::sync::LazyLock<Mutex<VadCacheEntry>> = std::sync::LazyLock::new(|| Mutex::new(None));

fn lock_vad_cache() -> MutexGuard<'static, VadCacheEntry> {
    VAD_CACHE.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("Recovering poisoned VAD session cache");
        poisoned.into_inner()
    })
}

/// Voice Activity Detection using Silero VAD v4 via ONNX Runtime.
/// Falls back to energy-based VAD if the model file is not found.
pub fn voice_activity_detection(pcm: &[i16], sample_rate: u32, threshold: f32) -> AppResult<Vec<(usize, usize)>> {
    if pcm.is_empty() {
        return Ok(Vec::new());
    }

    crate::models::init_ort_dylib_path();
    let model_path = crate::models::active_models_dir().join("silero_vad_v4.onnx");

    if model_path.exists() {
        // Use a cached ONNX session AND state_dims across `voice_activity_detection`
        // calls so that callers that invoke `detect()` consecutively (as in `proptest`,
        // ~64 cases) only pay the ONNX model load cost once and avoid redundant
        // state_dims discovery on subsequent calls.
        if let Ok((cached_session, cached_state_dims)) = {
            let mut guard = lock_vad_cache();
            if let Some((session, _sd)) = guard.as_ref().cloned() {
                drop(guard);
                Ok::<_, AppError>((session, _sd))
            } else {
                let mut builder =
                    Session::builder().map_err(|e| AppError::Onnx(format!("VAD session builder: {e}")))?;
                let session = builder
                    .commit_from_file(&model_path)
                    .map_err(|e| AppError::Onnx(format!("VAD model load: {e}")))?;
                let session = std::sync::Arc::new(std::sync::Mutex::new(session));

                let mut state_dims = (2usize, 1usize, 128usize);
                for input in session.lock().unwrap_or_else(|e| e.into_inner()).inputs() {
                    if input.name() == "state" {
                        if let ort::value::ValueType::Tensor { shape, .. } = input.dtype() {
                            let dims: Vec<usize> = shape.iter().map(|&d| if d <= 0 { 1 } else { d as usize }).collect();
                            if dims.len() == 3 {
                                state_dims = (dims[0].max(1), dims[1].max(1), dims[2].max(1));
                            }
                        }
                    }
                }

                *guard = Some((session.clone(), state_dims));
                drop(guard);
                Ok::<_, AppError>((session, state_dims))
            }
        } {
            // Reuse cached state_dims instead of discovering them every time.
            let cached_sd = cached_state_dims;
            if let Ok(mut vad) = SileroVad::new_with_session_cached(&cached_session, sample_rate, threshold, cached_sd)
            {
                let timer = crate::inference::InferenceTimer::start("vad");
                let result = vad.detect(pcm);
                timer.finish(result.is_ok());
                if result.is_ok() {
                    return result;
                }
                tracing::warn!("Silero VAD with cached session failed; invalidating VAD cache to force fresh load");
            } else {
                tracing::warn!("Failed to create SileroVad with cached session; invalidating VAD cache");
            }

            // Invalidate cache
            *lock_vad_cache() = None;
        }

        // Fresh load path: discover state_dims on first call.
        {
            let session = Session::builder()
                .map_err(|e| AppError::Onnx(format!("VAD session builder: {e}")))?
                .commit_from_file(&model_path)
                .map_err(|e| AppError::Onnx(format!("VAD model load: {e}")))?;
            let session = std::sync::Arc::new(std::sync::Mutex::new(session));

            let mut state_dims = (2usize, 1usize, 128usize);
            for input in session.lock().unwrap_or_else(|e| e.into_inner()).inputs() {
                if input.name() == "state" {
                    if let ort::value::ValueType::Tensor { shape, .. } = input.dtype() {
                        let dims: Vec<usize> = shape.iter().map(|&d| if d <= 0 { 1 } else { d as usize }).collect();
                        if dims.len() == 3 {
                            state_dims = (dims[0].max(1), dims[1].max(1), dims[2].max(1));
                        }
                    }
                }
            }
            if let Ok(mut vad) = SileroVad::new_with_session_cached(&session, sample_rate, threshold, state_dims) {
                let timer = crate::inference::InferenceTimer::start("vad");
                let result = vad.detect(pcm);
                timer.finish(result.is_ok());
                if result.is_ok() {
                    // Update cache since fresh load succeeded
                    *lock_vad_cache() = Some((session.clone(), state_dims));
                    return result;
                }
                tracing::warn!("Silero VAD fresh load failed, falling back to energy-based VAD");
            }
        }
    }

    vad_energy_fallback(pcm, sample_rate, threshold)
}

fn vad_energy_fallback(pcm: &[i16], _sample_rate: u32, threshold: f32) -> AppResult<Vec<(usize, usize)>> {
    let frame_size = 512usize;
    let hop_size = 160usize;
    let num_frames = (pcm.len().saturating_sub(frame_size)) / hop_size + 1;

    if num_frames == 0 {
        return Ok(vec![(0, pcm.len())]);
    }

    use rayon::prelude::*;
    let speech_frames: Vec<bool> = (0..num_frames)
        .into_par_iter()
        .map(|i| {
            let start = i * hop_size;
            let end = (start + frame_size).min(pcm.len());
            let frame = &pcm[start..end];

            let energy: f32 =
                frame.iter().map(|&s| (s as f32 / i16::MAX as f32).abs()).sum::<f32>() / frame.len() as f32;

            // Use threshold directly instead of threshold * 0.1 for consistency with Silero VAD.
            energy > threshold
        })
        .collect();

    let mut segments = Vec::new();
    let mut in_speech = false;
    let mut seg_start = 0;
    let min_speech_frames = 30;

    for (i, &is_speech) in speech_frames.iter().enumerate() {
        if is_speech && !in_speech {
            in_speech = true;
            seg_start = i;
        } else if !is_speech && in_speech {
            in_speech = false;
            if i - seg_start >= min_speech_frames {
                let sample_start = seg_start * hop_size;
                let sample_end = (i * hop_size + frame_size).min(pcm.len());
                segments.push((sample_start, sample_end));
            }
        }
    }

    if in_speech {
        let sample_start = seg_start * hop_size;
        if num_frames - seg_start >= min_speech_frames {
            segments.push((sample_start, pcm.len()));
        }
    }

    if segments.is_empty() {
        segments.push((0, pcm.len()));
    }

    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_waveform_empty() {
        assert!(compute_waveform(&[], 100).is_empty());
    }

    #[test]
    fn is_silent_handles_min_i16_without_overflow() {
        // Hardening-audit HIGH (twin of the clipping bug): i16::abs() overflows at -32768. A loud
        // negative-rail sample must register as NOT silent without panicking.
        assert!(!is_silent(&[i16::MIN]), "a -32768 sample is loud, not silent");
        assert!(is_silent(&[0, 10, -10, 49, -49]), "micro-noise is silent");
    }

    #[test]
    fn test_compute_waveform_sine() {
        let pcm: Vec<i16> = (0..48000)
            .map(|i| (i16::MAX as f64 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 48000.0).sin()) as i16)
            .collect();
        let waveform = compute_waveform(&pcm, 100);
        assert_eq!(waveform.len(), 100);
        assert!(waveform.iter().any(|&v| v > 0.1));
    }

    #[test]
    fn test_vad_empty() {
        let result = voice_activity_detection(&[], 16000, 0.5).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_vad_silence_returns_segment() {
        let pcm = vec![0i16; 16000];
        let segments = voice_activity_detection(&pcm, 16000, 0.5).unwrap();
        assert!(!segments.is_empty());
    }

    #[test]
    fn test_downmix_mono() {
        let stereo = vec![0.5f32, -0.5f32, 0.25f32, -0.25f32];
        let mono = downmix_to_mono(&stereo, 2);
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 0.0f32).abs() < 1e-6);
    }

    #[test]
    fn test_resample_identity() {
        let input = vec![0.0f32, 0.5, 1.0, 0.5, 0.0];
        let output = resample(&input, 16000, 16000);
        assert_eq!(input, output);
    }

    #[test]
    fn test_resample_44100_to_16000() {
        let input: Vec<f32> = (0..44100).map(|i| (i as f32 / 44100.0).sin()).collect();
        let output = resample(&input, 44100, 16000);
        assert_eq!(output.len(), 16000);
    }

    #[test]
    fn test_ensure_pcm_16khz_noop_at_target() {
        let pcm = vec![1000i16; 16000];
        let (sr, out) = ensure_pcm_16khz(16000, pcm.clone()).unwrap();
        assert_eq!(sr, 16000);
        assert_eq!(out, pcm);
    }

    #[test]
    fn test_ensure_pcm_16khz_resamples() {
        let pcm = vec![8000i16; 44100];
        let (sr, out) = ensure_pcm_16khz(44100, pcm).unwrap();
        assert_eq!(sr, 16000);
        assert_eq!(out.len(), 16000);
    }

    #[test]
    fn test_interleaved_f32_to_pcm_i16() {
        let samples: Vec<f32> = (0..1600).map(|i| i as f32 / 1600.0).collect();
        let pcm = interleaved_f32_to_pcm_i16(&samples, 1, 16000);
        assert_eq!(pcm.len(), 1600);
    }

    #[test]
    fn duration_is_independent_of_channel_count() {
        // n_frames is per-channel, so duration must NOT be divided by channels. A 60s stereo 44.1k
        // clip has n_frames = 44100*60 (per channel) and must report 60000ms, not the old halved
        // 30000ms. check_audio_file and get_duration_ms both route through frames_to_duration_ms,
        // so they can never disagree on the same file.
        assert_eq!(frames_to_duration_ms(16_000, 16_000.0), 1000);
        assert_eq!(frames_to_duration_ms(44_100 * 60, 44_100.0), 60_000, "stereo 60s must not halve");
        assert_eq!(frames_to_duration_ms(0, 16_000.0), 0);
        assert_eq!(frames_to_duration_ms(16_000, 0.0), 0, "non-positive sample rate guarded");
    }

    #[test]
    fn test_decode_pcm_windows_short_wav() {
        use hound::{WavSpec, WavWriter};
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("short.wav");
        let spec =
            WavSpec { channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = WavWriter::create(&path, spec).unwrap();
        for i in 0..8000 {
            writer.write_sample((i as i16).wrapping_mul(10)).unwrap();
        }
        writer.finalize().unwrap();

        let mut count = 0usize;
        decode_pcm_windows(&path, 90_000, |_| {
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
    }

    fn write_constant_wav(path: &Path, sample: i16) {
        use hound::{WavSpec, WavWriter};
        let spec =
            WavSpec { channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = WavWriter::create(path, spec).unwrap();
        for _ in 0..1600 {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn decode_to_pcm_cache_is_bound_to_audio_content_not_path() {
        use tempfile::TempDir;

        clear_pcm_cache();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("replaceable.wav");

        write_constant_wav(&path, 1000);
        let (_first_rate, first_pcm) = decode_to_pcm(&path).unwrap();

        write_constant_wav(&path, 2000);
        let (_second_rate, second_pcm) = decode_to_pcm(&path).unwrap();

        assert_ne!(first_pcm, second_pcm, "same-path changed audio must not reuse stale decoded PCM");
        clear_pcm_cache();
    }

    #[test]
    fn test_is_transient_decode_error() {
        use crate::error::AppError;
        let timeout = AppError::Audio(AudioError::Decode("Audio decode timed out after 30s".into()));
        assert!(is_transient_decode_error(&timeout));
        let validation = AppError::Validation("bad".into());
        assert!(!is_transient_decode_error(&validation));
    }

    #[test]
    fn decode_paths_return_err_not_panic_on_corrupt_input() {
        use tempfile::TempDir;

        clear_pcm_cache();
        let dir = TempDir::new().unwrap();

        // A spread of hostile inputs a user could feed in (renamed file, partial
        // download, wrong format, zero-byte). Every decode entry point must surface a
        // graceful Err — never panic the worker thread that owns the app's audio work.
        let fixtures: &[(&str, &[u8])] = &[
            ("empty.wav", b""),
            ("garbage.wav", b"\x00\x01\x02\xff\xfe\xfd not audio at all \x7f\x80"),
            ("text.wav", b"this is plainly a text file, not a RIFF container"),
            ("truncated_riff.wav", b"RIFF\x10\x00\x00\x00WAVE"), // header start, then nothing
            ("bogus_fmt.wav", b"RIFFxxxxWAVEfmt \xff\xff\xff\xff\x01\x00\x99\x99garbage"),
            ("empty.mp3", b""),
            ("garbage.flac", b"fLaC\x00\x00\x00\x22 corrupt stream info \xde\xad\xbe\xef"),
        ];

        for (name, bytes) in fixtures {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();

            assert!(decode_to_pcm(&path).is_err(), "decode_to_pcm must Err on {name}");
            assert!(check_audio_file(&path).is_err(), "check_audio_file must Err on {name}");
            assert!(get_duration_ms(&path).is_err(), "get_duration_ms must Err on {name}");
            let windows = decode_pcm_windows(&path, 1000, |_| Ok(()));
            assert!(windows.is_err(), "decode_pcm_windows must Err on {name}");
        }

        // A path that doesn't exist at all must also Err, not panic.
        let missing = dir.path().join("does_not_exist.wav");
        assert!(decode_to_pcm(&missing).is_err());
        assert!(check_audio_file(&missing).is_err());
        assert!(get_duration_ms(&missing).is_err());

        clear_pcm_cache();
    }

    #[test]
    fn pcm_cache_clear_recovers_poisoned_lock() {
        {
            let mut cache = lock_pcm_cache();
            cache.put("poisoned.wav".into(), (TARGET_SAMPLE_RATE, vec![1, 2, 3]));
            assert_eq!(cache.len(), 1);
        }

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = PCM_CACHE.lock().expect("lock PCM cache");
            panic!("poison PCM cache");
        }));

        clear_pcm_cache();
        assert_eq!(lock_pcm_cache().len(), 0);
        assert_eq!(pcm_cache_capacity().get(), 10);
    }

    #[test]
    fn vad_cache_recovers_poisoned_lock() {
        {
            let mut cache = lock_vad_cache();
            *cache = None;
        }

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = VAD_CACHE.lock().expect("lock VAD cache");
            panic!("poison VAD cache");
        }));

        *lock_vad_cache() = None;
        assert!(lock_vad_cache().is_none());
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn compute_waveform_returns_correct_length(pcm_len in (0usize..16000), num_points in (0usize..200)) {
            let pcm: Vec<i16> = vec![0i16; pcm_len];
            let waveform = compute_waveform(&pcm, num_points);

            if pcm.is_empty() || num_points == 0 {
                prop_assert!(waveform.is_empty());
            } else {
                prop_assert_eq!(waveform.len(), num_points.min(pcm.len()));
            }
        }

        #[test]
        fn compute_waveform_values_are_in_range(pcm_len in (1usize..16000), num_points in (1usize..100)) {
            let pcm: Vec<i16> = (0..pcm_len).map(|i| (i as i16).wrapping_mul(73) % i16::MAX ).collect();
            let waveform = compute_waveform(&pcm, num_points);

            prop_assert!(!waveform.is_empty());
            prop_assert_eq!(waveform.len(), num_points.min(pcm.len()));
            for &v in &waveform {
                prop_assert!((0.0..=1.0).contains(&v),
                    "RMS value {} must be in [0.0, 1.0]", v);
            }
        }

        #[test]
        fn compute_waveform_detects_signal(pcm_len in (16000usize..48000)) {
            let pcm: Vec<i16> = (0..pcm_len).map(|i| {
                (i16::MAX as f64 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 16000.0).sin()) as i16
            }).collect();

            let waveform = compute_waveform(&pcm, 50);
            prop_assert_eq!(waveform.len(), 50);
            // At least one point should have significant energy
            prop_assert!(waveform.iter().any(|&v| v > 0.1),
                "Waveform should detect sine wave energy");
        }

        #[test]
        fn vad_empty_input_returns_empty(sample_rate in (8000u32..96000)) {
            let result = voice_activity_detection(&[], sample_rate, 0.5).unwrap();
            prop_assert!(result.is_empty());
        }


        #[test]
        fn resample_preserves_length_ratio(input_len in (1usize..16000)) {
            let input: Vec<f32> = (0..input_len).map(|i| i as f32 / input_len as f32).collect();
            let from_rate = 44100u32;
            let to_rate = 16000u32;

            let output = resample(&input, from_rate, to_rate);
            let expected_len = (input.len() as f64 * to_rate as f64 / from_rate as f64).ceil() as usize;

            prop_assert_eq!(output.len(), expected_len,
                "Resample {} -> {} of {} samples should produce {} samples",
                from_rate, to_rate, input_len, expected_len);
        }

        #[test]
        fn resample_identity_preserves_input(input in proptest::collection::vec(-1.0f32..1.0f32, 0..200)) {
            let output = resample(&input, 16000, 16000);
            prop_assert_eq!(input, output);
        }

        #[test]
        fn downmix_to_mono_reduces_channels(samples_len in (2usize..200).prop_filter("must be even", |v| v % 2 == 0)) {
            let channels = 2u32;
            let samples: Vec<f32> = (0..samples_len).map(|i| ((i as f32) / samples_len as f32) * 2.0 - 1.0).collect();
            let mono = downmix_to_mono(&samples, channels);
            prop_assert_eq!(mono.len(), samples.len() / channels as usize);
        }
    }
}
