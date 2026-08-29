use crate::error::{AppError, AppResult, AudioError};
use lru::LruCache;
use ort::session::Session;
use ort::value::Tensor;
use std::io::Read;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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

thread_local! {
    /// How many times this thread has walked a source file through `decode_pcm_windows`.
    ///
    /// Exists because the worst bug in this path is INVISIBLE to output equality: decoding one
    /// source once PER CLIP instead of once per recording produces byte-identical clips and takes
    /// hours instead of seconds. Wall-clock is the other way to catch it, and a timing assertion on
    /// this box is a flake generator. Thread-local, so a parallel `cargo test` run cannot see another
    /// test's decodes.
    static DECODE_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Number of `decode_pcm_windows` walks on the CURRENT thread. Test observability; see `DECODE_PASSES`.
pub fn decode_passes_on_this_thread() -> usize {
    DECODE_PASSES.with(|passes| passes.get())
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

/// Duration in ms with a DECODE FALLBACK. A container that reports no usable frame count (VBR MP3 without
/// a Xing/Info header, streamed OGG/WebM, some live recordings) leaves `num_frames` None/0; reporting 0
/// then makes a perfectly decodable file look empty. Fall back to an actual decode (cached by
/// decode_to_pcm, so the pipeline reuses it) to get the true duration; a genuinely empty file still
/// yields 0. Both [`check_audio_file`] and [`get_duration_ms`] route through here so they can never
/// disagree (they previously diverged: check_audio_file used num_frames.unwrap_or(0) with NO fallback and
/// reported 0 ms for a valid VBR MP3, contradicting get_duration_ms's true value).
fn duration_ms_with_decode_fallback(path: &Path, num_frames: Option<u64>, sample_rate: f64) -> AppResult<i64> {
    match num_frames {
        Some(n) if n > 0 => Ok(frames_to_duration_ms(n, sample_rate)),
        _ => {
            let (decoded_rate, samples) = decode_to_pcm(path)?;
            if samples.is_empty() || decoded_rate == 0 {
                Ok(0)
            } else {
                Ok(((samples.len() as f64 / decoded_rate as f64) * 1000.0) as i64)
            }
        }
    }
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

    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let file =
        std::fs::File::open(path).map_err(|e| AppError::Audio(AudioError::Decode(format!("Cannot open: {e}"))))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| AppError::Audio(AudioError::Decode(format!("Cannot probe format: {e}"))))?;

    let track = format
        .tracks()
        .iter()
        .find(|t| matches!(t.codec_params, Some(CodecParameters::Audio(_))))
        .ok_or_else(|| AppError::Audio(AudioError::NoTracks(path.to_path_buf())))?;

    // n_frames moved from codec params to the Track in 0.6; audio rate/channels live on the Audio variant.
    let audio = match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p,
        _ => return Err(AppError::Audio(AudioError::NoTracks(path.to_path_buf()))),
    };
    let num_frames = track.num_frames;
    let sample_rate = audio.sample_rate.unwrap_or(TARGET_SAMPLE_RATE) as f64;
    let channels = audio.channels.as_ref().map(|c| c.count() as u16).unwrap_or(1);

    if sample_rate <= 0.0 {
        return Err(AppError::Audio(AudioError::Decode("Invalid sample rate in file".into())));
    }

    let format = path.extension().and_then(|e| e.to_str()).unwrap_or("unknown").to_string();

    Ok(AudioInfo {
        // Same decode fallback as get_duration_ms: a VBR MP3 without a Xing header has no frame count, and
        // reporting 0 ms here made check_audio disagree with the real import path (round-25 hunt).
        duration_ms: duration_ms_with_decode_fallback(path, num_frames, sample_rate)?,
        sample_rate: sample_rate as u32,
        channels,
        format,
    })
}

/// Decode any audio file to 16kHz mono 16-bit PCM using symphonia.
/// Results are cached in a small LRU to avoid re-decoding the same file.
pub fn decode_to_pcm<P: AsRef<Path>>(path: P) -> AppResult<(u32, Vec<i16>)> {
    // A caller with no way to stop waiting also has no way to abort: a flag that is never set.
    decode_to_pcm_abortable(path, &AtomicBool::new(false))
}

/// [`decode_to_pcm`] that gives up when `abort` is set.
///
/// A Rust thread cannot be killed from outside, so [`decode_to_pcm_with_timeout`] used to leave its
/// worker decoding a file nobody was waiting for any more — holding a core and up to ~1 GiB of
/// interleaved f32 while the caller had already failed and perhaps started another import. The
/// per-packet check below is what turns that timeout into a real stop.
fn decode_to_pcm_abortable<P: AsRef<Path>>(path: P, abort: &AtomicBool) -> AppResult<(u32, Vec<i16>)> {
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

    // symphonia 0.6 migration: probe() returns the FormatReader directly (opts by value); codec_params
    // is an Option<CodecParameters> enum (audio track = the Audio variant); decoders come from
    // make_audio_decoder; decoded buffers are GenericAudioBufferRef and copy out via
    // copy_to_vec_interleaved. Behaviour is identical to the 0.5 SampleBuffer path.
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let file = std::fs::File::open(path.as_ref()).map_err(AppError::Io)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.as_ref().extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;

    let track = format
        .tracks()
        .iter()
        .find(|t| matches!(t.codec_params, Some(CodecParameters::Audio(_))))
        .ok_or_else(|| AppError::Audio(AudioError::NoTracks(path.as_ref().to_path_buf())))?;

    let audio_params = match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p.clone(),
        _ => return Err(AppError::Audio(AudioError::NoTracks(path.as_ref().to_path_buf()))),
    };
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&audio_params, &AudioDecoderOptions::default())
        .map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;

    let track_id = track.id;
    // Bound how much we decode into RAM in one pass so a pathologically large (or hostile) file can't
    // OOM the host. ~256M interleaved f32 ≈ 1 GiB — far beyond any realistic clip/source; genuinely
    // long audio is decoded incrementally via decode_pcm_windows instead.
    const MAX_DECODE_SAMPLES: usize = 256 * 1024 * 1024;
    let mut all_samples: Vec<f32> = Vec::new();
    let mut actual_channels = 0u32;
    let mut actual_sample_rate = 0u32;

    let mut sample_buf: Vec<f32> = Vec::new();
    loop {
        if abort.load(Ordering::Relaxed) {
            return Err(AppError::Audio(AudioError::Decode(
                "audio decode cancelled (the caller stopped waiting)".into(),
            )));
        }
        let packet = match format.next_packet() {
            Ok(Some(pkt)) => pkt,
            Ok(None) => break, // clean end of stream (0.6 signals EOF with None, not an UnexpectedEof err)
            Err(symphonia::core::errors::Error::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(symphonia::core::errors::Error::ResetRequired) => {
                // A mid-stream reset (chained/multi-stream file, e.g. chained OGG) means there is MORE audio
                // after this point we can't decode in one pass. Keeping only the decoded prefix is the same
                // silent-truncation data-loss trap the corrupt-source arm below guards — fail LOUDLY so a
                // curation import never quietly drops the tail of a legitimate file.
                return Err(AppError::Audio(AudioError::Decode(
                    "decode stopped at a mid-stream reset (chained or multi-stream file). Re-encode to a \
                     single continuous stream (e.g. `ffmpeg -i INPUT -ar 16000 -ac 1 out.wav`) and re-import."
                        .to_string(),
                )));
            }
            Err(e) => {
                // Non-EOF error mid-file = corrupt/truncated source. Fail loudly rather than silently
                // importing only the decodable prefix (the old `Err(_) => break` data-loss trap).
                return Err(AppError::Audio(AudioError::Decode(format!(
                    "decode stopped on a non-EOF error (corrupt or truncated source?): {e}"
                ))));
            }
        };

        if packet.track_id != track_id {
            continue;
        }

        let decoded = decoder.decode(&packet).map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;
        let spec = decoded.spec();
        if spec.channels().count() == 0 {
            continue;
        }

        if actual_channels == 0 {
            actual_channels = spec.channels().count() as u32;
            actual_sample_rate = spec.rate();
        }

        sample_buf.clear();
        decoded.copy_to_vec_interleaved(&mut sample_buf);
        all_samples.extend_from_slice(&sample_buf);
        if all_samples.len() > MAX_DECODE_SAMPLES {
            return Err(AppError::Audio(AudioError::Decode(format!(
                "audio too large to decode in one pass (> {MAX_DECODE_SAMPLES} samples)"
            ))));
        }
    }

    if all_samples.is_empty() {
        return Err(AppError::Audio(AudioError::EmptyBuffer));
    }

    let channels = if actual_channels > 0 {
        actual_channels
    } else {
        audio_params.channels.as_ref().map(|c| c.count()).unwrap_or(1) as u32
    };
    let sample_rate = if actual_sample_rate > 0 {
        actual_sample_rate
    } else {
        audio_params.sample_rate.unwrap_or(TARGET_SAMPLE_RATE)
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
    DECODE_PASSES.with(|passes| passes.set(passes.get() + 1));
    let _span = crate::telemetry::TRACER.start_span(
        "audio.decode_pcm_windows",
        crate::telemetry::Tracer::metadata(vec![("path", path.to_string_lossy().to_string())]),
    );

    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let file = std::fs::File::open(path).map_err(AppError::Io)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;

    let track = format
        .tracks()
        .iter()
        .find(|t| matches!(t.codec_params, Some(CodecParameters::Audio(_))))
        .ok_or_else(|| AppError::Audio(AudioError::NoTracks(path.to_path_buf())))?;

    let audio_params = match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p.clone(),
        _ => return Err(AppError::Audio(AudioError::NoTracks(path.to_path_buf()))),
    };
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&audio_params, &AudioDecoderOptions::default())
        .map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;

    let track_id = track.id;
    let mut channels = audio_params.channels.as_ref().map(|c| c.count()).unwrap_or(1) as u32;
    let mut sample_rate = audio_params.sample_rate.unwrap_or(TARGET_SAMPLE_RATE);
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
        // Advance the clock BEFORE moving `pcm` into the window — cloning it just to read its length
        // afterwards doubled the peak memory of every window for nothing.
        let window_offset_ms = output_offset_ms;
        output_offset_ms += (pcm.len() as i64 * 1000) / TARGET_SAMPLE_RATE as i64;
        on_window(PcmWindow { offset_ms: window_offset_ms, sample_rate: TARGET_SAMPLE_RATE, pcm })?;
        Ok(())
    };

    let mut frame_buf: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(pkt)) => pkt,
            Ok(None) => break, // clean end of stream (0.6 signals EOF with None)
            Err(symphonia::core::errors::Error::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(symphonia::core::errors::Error::ResetRequired) => {
                // A mid-stream reset (chained/multi-stream file, e.g. chained OGG) means there is MORE audio
                // after this point we can't decode in one pass. Keeping only the decoded prefix is the same
                // silent-truncation data-loss trap the corrupt-source arm below guards — fail LOUDLY so a
                // curation import never quietly drops the tail of a legitimate file.
                return Err(AppError::Audio(AudioError::Decode(
                    "decode stopped at a mid-stream reset (chained or multi-stream file). Re-encode to a \
                     single continuous stream (e.g. `ffmpeg -i INPUT -ar 16000 -ac 1 out.wav`) and re-import."
                        .to_string(),
                )));
            }
            Err(e) => {
                // A non-EOF decode error mid-file means the source is corrupt/truncated. Treating it as a
                // clean end (the old `Err(_) => break`) silently imported only the decodable PREFIX and
                // dropped the rest — a data-loss trap for a curation app. Fail LOUDLY instead.
                return Err(AppError::Audio(AudioError::Decode(format!(
                    "decode stopped on a non-EOF error (corrupt or truncated source?): {e}"
                ))));
            }
        };

        if packet.track_id != track_id {
            continue;
        }

        let decoded = decoder.decode(&packet).map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;
        let spec = decoded.spec();
        if spec.channels().count() == 0 {
            continue;
        }

        if !spec_updated {
            channels = spec.channels().count() as u32;
            sample_rate = spec.rate();
            window_frames = ((window_ms as u64) * sample_rate as u64 / 1000).max(1) as usize;
            window_cap = window_frames * channels as usize;
            spec_updated = true;
        }

        frame_buf.clear();
        decoded.copy_to_vec_interleaved(&mut frame_buf);
        buf.extend_from_slice(&frame_buf);

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

/// Decode `path` in windows on a worker thread and hand each one to `on_window` **on the caller's
/// thread**, with at most a couple of windows alive at once.
///
/// This replaces the old `decode_pcm_windows_with_timeout`, whose callback had to be
/// `Send + 'static`. That bound forced its one caller (the long-audio import) to do the only thing
/// such a callback can do — push every window into a Vec and process the file afterwards — so the
/// "streaming" path still held the ENTIRE recording in RAM before it planned a single chunk
/// (~170 MB of PCM for a 1.5 h source, and it grows without limit with the file). Handing windows
/// back on the caller's thread lets the import consume each one with borrowed state instead.
///
/// Two bounds, both real:
/// - the `sync_channel(1)` back-pressures the decoder, so it can run at most one window ahead;
/// - `window_timeout` bounds the wait for EACH window. It is per-window on purpose: decode and ASR
///   now interleave, so a whole-file budget would be timing the ASR too.
///
/// On any early return the receiver drops, the worker's next `send` fails, and its decode loop exits
/// — the timeout genuinely stops the decoder instead of leaving it running unobserved.
///
/// `on_window`'s second argument is `is_last`: the consumer holds one window back so it can say
/// which window ends the file, which the caller needs and a plain stream cannot know in advance. A
/// held-back window is only ever released as `is_last` after the decoder reports success, so a
/// mid-file decode failure can never be mistaken for a clean end of file.
pub fn decode_pcm_windows_streaming<F>(
    path: std::path::PathBuf,
    window_ms: u32,
    window_timeout: Duration,
    mut on_window: F,
) -> AppResult<()>
where
    F: FnMut(PcmWindow, bool) -> AppResult<()>,
{
    use std::sync::mpsc::RecvTimeoutError;

    let (window_tx, window_rx) = std::sync::mpsc::sync_channel::<PcmWindow>(1);
    let (done_tx, done_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = decode_pcm_windows(&path, window_ms, |window| {
            window_tx.send(window).map_err(|_| {
                AppError::Audio(AudioError::Decode("audio decode cancelled (the consumer stopped)".into()))
            })
        });
        send_decode_worker_result(done_tx, result, "decode_pcm_windows");
    });

    let mut pending: Option<PcmWindow> = None;
    loop {
        match window_rx.recv_timeout(window_timeout) {
            Ok(window) => {
                if let Some(previous) = pending.replace(window) {
                    on_window(previous, false)?;
                }
            }
            // The sender is gone: the decode finished (or died). Its own result decides which.
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                return Err(AppError::Audio(AudioError::Decode(format!(
                    "Audio decode timed out after {window_timeout:?} waiting for the next window"
                ))));
            }
        }
    }

    done_rx.recv().unwrap_or_else(|_| {
        Err(AppError::Audio(AudioError::Decode("Audio decode worker thread disconnected".into())))
    })?;

    if let Some(last) = pending {
        on_window(last, true)?;
    }
    Ok(())
}

/// Decode audio to PCM with a configurable timeout for the symphonia decoder.
/// Returns an error if decoding does not complete within the given duration.
pub fn decode_to_pcm_with_timeout<P: AsRef<Path>>(path: P, timeout: Duration) -> AppResult<(u32, Vec<i16>)> {
    let path = path.as_ref().to_string_lossy().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    // The worker outlives the timeout — a thread cannot be killed from outside — so give it something
    // to notice with. Without this, a timed-out decode kept a core and up to ~1 GiB of interleaved f32
    // busy for the full length of a decode whose caller had already given up and moved on.
    let abort = Arc::new(AtomicBool::new(false));
    let worker_abort = Arc::clone(&abort);

    std::thread::spawn(move || {
        let result = decode_to_pcm_abortable(&path, &worker_abort);
        send_decode_worker_result(tx, result, "decode_to_pcm");
    });

    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            abort.store(true, Ordering::Relaxed);
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
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let file = std::fs::File::open(path.as_ref())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.as_ref().extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| AppError::Audio(AudioError::Decode(e.to_string())))?;
    let track = format
        .tracks()
        .iter()
        .find(|t| matches!(t.codec_params, Some(CodecParameters::Audio(_))))
        .ok_or_else(|| AppError::Audio(AudioError::NoTracks(path.as_ref().to_path_buf())))?;

    let audio = match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p,
        _ => return Err(AppError::Audio(AudioError::NoTracks(path.as_ref().to_path_buf()))),
    };
    let track_num_frames = track.num_frames;
    let sample_rate = audio.sample_rate.unwrap_or(TARGET_SAMPLE_RATE) as f64;

    if sample_rate <= 0.0 {
        return Err(AppError::Audio(AudioError::Decode("Invalid sample rate".into())));
    }

    // Shared with check_audio_file so the two can never disagree (the decode fallback handles a VBR MP3
    // without a Xing/Info header, streamed OGG/WebM, etc. — a missing frame count that would otherwise
    // report 0 ms for a perfectly decodable file).
    duration_ms_with_decode_fallback(path.as_ref(), track_num_frames, sample_rate)
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
        // Only genuinely transient I/O kinds are worth retrying; permanent ones (NotFound,
        // PermissionDenied, ...) just double the latency and fail again.
        AppError::Io(e) => matches!(
            e.kind(),
            std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        _ => false,
    }
}

/// Windowed-sinc (Hamming) low-pass FIR, applied as a centered, same-length, edge-clamped convolution.
/// Used as the anti-aliasing pre-filter before decimation. `cutoff_hz` is the -6 dB-ish passband edge.
fn lowpass_fir(samples: &[f32], sample_rate: f64, cutoff_hz: f64) -> Vec<f32> {
    const TAPS: usize = 63; // odd → symmetric, linear-phase kernel
    let half = (TAPS / 2) as isize;
    // Normalized cutoff in cycles/sample, kept strictly below Nyquist (0.5).
    let fc = (cutoff_hz / sample_rate).clamp(1e-4, 0.499);

    let mut kernel = [0.0f64; TAPS];
    let mut sum = 0.0f64;
    for (k, slot) in kernel.iter_mut().enumerate() {
        let n = k as isize - half;
        let sinc = if n == 0 {
            2.0 * fc
        } else {
            let x = std::f64::consts::PI * n as f64;
            (2.0 * fc * x).sin() / x
        };
        // Hamming window.
        let w = 0.54 - 0.46 * (2.0 * std::f64::consts::PI * k as f64 / (TAPS as f64 - 1.0)).cos();
        let v = sinc * w;
        *slot = v;
        sum += v;
    }
    // Normalize to unity DC gain so the filter neither amplifies nor attenuates the passband level.
    for v in kernel.iter_mut() {
        *v /= sum;
    }

    let len = samples.len();
    let mut out = vec![0.0f32; len];
    for (i, o) in out.iter_mut().enumerate() {
        let mut acc = 0.0f64;
        for (k, &kv) in kernel.iter().enumerate() {
            let idx = i as isize + (k as isize - half);
            let s = if idx < 0 {
                samples[0]
            } else if idx as usize >= len {
                samples[len - 1]
            } else {
                samples[idx as usize]
            };
            acc += kv * s as f64;
        }
        *o = acc as f32;
    }
    out
}

pub(crate) fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || samples.is_empty() {
        return samples.to_vec();
    }
    // Anti-aliasing: when DOWNSAMPLING, low-pass below the NEW Nyquist before decimating. Real input is
    // almost always 44.1k/48k, so every file is downsampled to 16k; linear interpolation alone is a poor
    // anti-alias filter (~-6 dB/octave, no null at Nyquist), so source energy above 8 kHz would fold back
    // into the speech band and corrupt the PCM fed to VAD/ASR, inflating WER/CER. Upsampling needs no
    // pre-filter (no new aliases are created), so it keeps the plain interpolation path.
    let prefiltered: Vec<f32>;
    let src: &[f32] = if to_rate < from_rate {
        let cutoff = 0.45 * to_rate as f64; // just under the new Nyquist (to_rate / 2)
        prefiltered = lowpass_fir(samples, from_rate as f64, cutoff);
        &prefiltered
    } else {
        samples
    };

    let ratio = to_rate as f64 / from_rate as f64;
    // Length is derived from `src` (the prefiltered signal), not the raw `samples`, so the
    // downsample pre-filter above is not silently dropped. `src == samples` on the upsample path.
    //
    // Bound the output length so a corrupt/crafted `from_rate` can't request an astronomical allocation.
    // A real resample to 16 kHz upsamples at most ~2x (an 8 kHz source); 16x is a very generous ceiling no
    // legitimate file reaches, but it caps the alloc a malformed header can demand: a WAV declaring
    // sample_rate=1 (it survives the `> 0` decode guard) would otherwise want src.len() * 16000 samples — a
    // hundreds-of-GB Vec::with_capacity that ABORTS the process (handle_alloc_error, not a catchable panic).
    // Downsampling (ratio < 1) always stays well under the cap.
    let new_len = ((src.len() as f64 * ratio).ceil() as usize).min(src.len().saturating_mul(16));

    // Anti-aliased windowed-sinc resampling. Each output sample is a Hamming-windowed-sinc-weighted sum
    // of the nearby SOURCE samples, with the sinc cutoff at the lower of the two Nyquist limits. When
    // DOWNsampling (the common 44.1/48 kHz -> 16 kHz import case) that cutoff is the TARGET Nyquist, so
    // source energy above it is removed before decimation instead of folding back as aliasing into the
    // speech band that OmniASR/VAD consume. Plain linear interpolation (the previous implementation) is
    // NOT an anti-alias filter and aliased every non-16 kHz import. Cost is O(new_len * 2*RADIUS).
    use std::f64::consts::PI;
    // Cutoff as a fraction of the SOURCE sample rate (cycles per source sample), in (0, 0.5].
    let cutoff = if to_rate < from_rate { (to_rate as f64 / 2.0) / from_rate as f64 } else { 0.5 };
    const RADIUS: i64 = 16; // sinc window half-width, in source samples
                            // Read from `src` (prefiltered on downsample, == samples on upsample), per theirs/base. Every
                            // index below is edge-clamped to [0, n-1], so this is panic-free for every rate — covering the
                            // out-of-bounds case theirs fixed (new_len = ceil(len*ratio) can overshoot the last index).
    let n = src.len() as i64;
    let mut out = Vec::with_capacity(new_len);
    for i in 0..new_len {
        let center = i as f64 / ratio; // output position expressed in source samples
        let c0 = center.floor() as i64;
        let mut acc = 0.0f64;
        let mut wsum = 0.0f64;
        for j in (c0 - RADIUS)..=(c0 + RADIUS + 1) {
            let dist = center - j as f64;
            if dist.abs() > RADIUS as f64 {
                continue;
            }
            let x = 2.0 * cutoff * dist;
            let sinc = if x.abs() < 1e-9 { 1.0 } else { (PI * x).sin() / (PI * x) };
            let window = 0.54 + 0.46 * (PI * dist / RADIUS as f64).cos(); // Hamming, centered
            let weight = sinc * window;
            let idx = j.clamp(0, n - 1) as usize; // edge clamp
            acc += weight * src[idx] as f64;
            wsum += weight;
        }
        // Normalize by the weight sum so DC gain is exactly 1 even with edge clamping.
        out.push(if wsum.abs() > 1e-12 { (acc / wsum) as f32 } else { 0.0 });
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
        // The FIRST ort use in the import pipeline, and the one measured hanging: with no ONNX
        // Runtime present this builder blocks forever rather than failing (see
        // models::ensure_ort_runtime_loadable). ort's load is process-wide and one-time, so probing
        // here also covers every session built afterwards; on a healthy install it is a cached
        // atomic read.
        crate::models::ensure_ort_runtime_loadable().map_err(AppError::Onnx)?;
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
            // Round-24 #6: ~96ms floor (3 frames). The old 480ms (15-frame) floor silently DROPPED any
            // real short word (e.g. "بەڵێ"/yes) flanked by silence — its audio was never transcribed
            // and vanished from the dataset. 3 frames still filters 1-2 frame spurious activations while
            // preserving genuine short utterances; downstream chunking merges/absorbs as needed.
            min_speech_frames: 3,
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
            // Round-24 #6: ~96ms floor (3 frames). The old 480ms (15-frame) floor silently DROPPED any
            // real short word (e.g. "بەڵێ"/yes) flanked by silence — its audio was never transcribed
            // and vanished from the dataset. 3 frames still filters 1-2 frame spurious activations while
            // preserving genuine short utterances; downstream chunking merges/absorbs as needed.
            min_speech_frames: 3,
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
            // Round-24 #6: ~96ms floor (3 frames). The old 480ms (15-frame) floor silently DROPPED any
            // real short word (e.g. "بەڵێ"/yes) flanked by silence — its audio was never transcribed
            // and vanished from the dataset. 3 frames still filters 1-2 frame spurious activations while
            // preserving genuine short utterances; downstream chunking merges/absorbs as needed.
            min_speech_frames: 3,
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

/// Which VAD backend ACTUALLY produced the speech regions — the honest per-segment provenance the export
/// reports, NOT a path-exists probe. Only the branch that actually returned regions may name itself: a
/// Silero that loads but then fails to build/run falls back to Energy at runtime (below), so a
/// present-but-broken model is never mislabeled as Silero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadBackend {
    /// Silero VAD v4 (ONNX) ran successfully.
    Silero,
    /// Silero was absent on disk, or loaded but failed to CREATE/RUN (SileroVad build or detect error) →
    /// adaptive energy-threshold fallback. (A model that fails its integrity hash or ONNX session load
    /// returns an `Err` instead — no regions at all — so it can never be recorded as a silent Silero.)
    Energy,
    /// No VAD ran — the whole buffer was taken as a single region (a file short enough to skip chunking),
    /// or there was no audio. `plan_speech_chunks` also returns this for its whole-buffer path.
    None,
}

impl VadBackend {
    /// Stable lowercase token persisted per segment (`vad_backend` column) and reported in the export.
    pub fn as_str(self) -> &'static str {
        match self {
            VadBackend::Silero => "silero",
            VadBackend::Energy => "energy",
            VadBackend::None => "none",
        }
    }
}

/// Voice Activity Detection using Silero VAD v4 via ONNX Runtime.
/// Falls back to energy-based VAD if the model file is not found. Returns the regions AND which backend
/// actually produced them, so the pipeline can record honest per-segment VAD provenance.
pub fn voice_activity_detection(
    pcm: &[i16],
    sample_rate: u32,
    threshold: f32,
) -> AppResult<(Vec<(usize, usize)>, VadBackend)> {
    if pcm.is_empty() {
        return Ok((Vec::new(), VadBackend::None));
    }

    crate::models::init_ort_dylib_path();
    // Per-file resolve (user dir, else bundled) — NOT active_models_dir().join: the latter is
    // all-or-nothing, so once the user downloads any OmniASR model into the user dir, the bundled-only
    // Silero VAD is orphaned and VAD silently drops to the energy fallback (round-26 hunt).
    let model_path = crate::models::resolve_model_file("silero_vad_v4.onnx");

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
                // Re-verify the pinned VAD model against its manifest hash before loading it — parity
                // with the ASR load-time integrity gate (asr.rs). Catches a silero_vad file swapped or
                // corrupted on disk AFTER its download-time verification. Runs only on a cache miss (the
                // session is cached below), so it's a one-time 2 MB hash, not per-call.
                crate::models::verify_model_path_runtime(&model_path, "silero_vad_v4.onnx")
                    .map_err(|e| AppError::Onnx(format!("VAD model integrity: {e}")))?;
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
                    return result.map(|regions| (regions, VadBackend::Silero));
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
                    return result.map(|regions| (regions, VadBackend::Silero));
                }
                tracing::warn!("Silero VAD fresh load failed, falling back to energy-based VAD");
            }
        }
    }

    vad_energy_fallback(pcm, sample_rate, threshold).map(|regions| (regions, VadBackend::Energy))
}

fn vad_energy_fallback(pcm: &[i16], _sample_rate: u32, threshold: f32) -> AppResult<Vec<(usize, usize)>> {
    let frame_size = 512usize;
    let hop_size = 160usize;
    let num_frames = (pcm.len().saturating_sub(frame_size)) / hop_size + 1;

    if num_frames == 0 {
        return Ok(vec![(0, pcm.len())]);
    }

    use rayon::prelude::*;
    // Per-frame mean absolute amplitude (~0.02-0.2 for ordinary speech). Round-24 #7: the old code
    // compared this against `threshold` — the Silero speech PROBABILITY (0..1, default 0.5), a totally
    // different scale — so real speech (amplitude well below 0.5) was classified as silence on EVERY
    // frame and the fallback returned the whole buffer as a single region (no VAD at all). Derive the
    // speech/silence cutoff ADAPTIVELY from the signal's own energy distribution instead.
    let energies: Vec<f32> = (0..num_frames)
        .into_par_iter()
        .map(|i| {
            let start = i * hop_size;
            let end = (start + frame_size).min(pcm.len());
            let frame = &pcm[start..end];
            frame.iter().map(|&s| (s as f32 / i16::MAX as f32).abs()).sum::<f32>() / frame.len().max(1) as f32
        })
        .collect();

    // Noise floor = 10th percentile, peak = 95th percentile (robust to outliers). A frame is speech
    // when its amplitude rises a sensitivity-scaled fraction of the way from the noise floor toward the
    // peak. The user's `vad_threshold` (0..1) is honored as that SENSITIVITY (higher = stricter), NOT
    // as a raw amplitude. A small absolute floor avoids a zero cutoff on near-silent input.
    let mut sorted = energies.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f32| sorted[((sorted.len() as f32 * p) as usize).min(sorted.len().saturating_sub(1))];
    let noise_floor = pct(0.10);
    let peak = pct(0.95);
    let frac = threshold.clamp(0.05, 0.95) * 0.5; // 0.5 -> 0.25 of the noise->peak span
    let amp_threshold = (noise_floor + frac * (peak - noise_floor)).max(0.005);
    let speech_frames: Vec<bool> = energies.iter().map(|&e| e > amp_threshold).collect();

    let mut segments = Vec::new();
    let mut in_speech = false;
    let mut seg_start = 0;
    // ~90ms floor (9 hops × 10ms at 16 kHz) — matched to the Silero path's ~96ms floor (min_speech_frames=3;
    // Round-24 #6). The old 30-hop (~300ms) floor was never lowered when the primary VAD's was, so whenever
    // this fallback ran (silero_vad_v4.onnx absent), it silently DROPPED any 96-300ms utterance Silero would
    // keep — the exact real-short-word loss (e.g. بەڵێ / "yes") the primary-VAD floor was lowered to prevent.
    // Kept at or below Silero's floor so the fallback never drops a run the primary VAD would retain.
    let min_speech_frames = 9;

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

    /// Write a mono/stereo 16-bit WAV of `secs` seconds at `rate` Hz containing a 440 Hz tone.
    /// A real container decoded by real symphonia - nothing about the parser under test is mocked.
    fn write_tone_wav(path: &std::path::Path, rate: u32, secs: f32, channels: u16) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).expect("create wav");
        let n = (rate as f32 * secs) as usize;
        for i in 0..n {
            let v = ((i as f32 * 440.0 * 2.0 * std::f32::consts::PI / rate as f32).sin() * 12000.0) as i16;
            for _ in 0..channels {
                w.write_sample(v).expect("write sample");
            }
        }
        w.finalize().expect("finalize wav");
    }

    /// The audio PARSER path on a real container: probe -> metadata -> decode -> resample.
    ///
    /// The charter asks for >80% line coverage on the audio parsers; this file sat at 77% because
    /// check_audio_file, decode_to_pcm, get_duration_ms and decode_pcm_windows - 103 of the 138
    /// never-executed functions - were reachable only through the running app or the model-gated
    /// integration suite. They are ordinary parsing code and deserve ordinary unit tests.
    #[test]
    fn check_audio_file_reports_real_container_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");

        let p = dir.path().join("mono16k.wav");
        write_tone_wav(&p, 16_000, 1.0, 1);
        let info = check_audio_file(&p).expect("must probe a real wav");
        assert_eq!(info.sample_rate, 16_000);
        assert_eq!(info.channels, 1);
        assert_eq!(info.format, "wav");
        assert!((info.duration_ms - 1000).abs() <= 2, "about 1000 ms, got {}", info.duration_ms);

        // Metadata must report the SOURCE rate and channel count, not the 16 kHz mono the decoder
        // later converts to.
        let p2 = dir.path().join("stereo44k.wav");
        write_tone_wav(&p2, 44_100, 0.5, 2);
        let info2 = check_audio_file(&p2).expect("must probe stereo");
        assert_eq!(info2.sample_rate, 44_100);
        assert_eq!(info2.channels, 2);
        assert!((info2.duration_ms - 500).abs() <= 2, "about 500 ms, got {}", info2.duration_ms);

        // Error paths, each one a failure a user can actually hit.
        assert!(check_audio_file(dir.path().join("nope.wav")).is_err(), "missing file must error");
        let empty = dir.path().join("empty.wav");
        std::fs::write(&empty, b"").expect("write empty");
        assert!(check_audio_file(&empty).is_err(), "empty file must error");
        let garbage = dir.path().join("garbage.wav");
        std::fs::write(&garbage, vec![0x00u8; 4096]).expect("write garbage");
        assert!(check_audio_file(&garbage).is_err(), "non-audio bytes must error, not panic");
    }

    #[test]
    fn decode_to_pcm_converts_to_target_rate_mono() {
        let dir = tempfile::tempdir().expect("tempdir");

        let p = dir.path().join("a.wav");
        write_tone_wav(&p, 16_000, 1.0, 1);
        let (rate, pcm) = decode_to_pcm(&p).expect("decode");
        assert_eq!(rate, TARGET_SAMPLE_RATE);
        assert!((pcm.len() as i64 - 16_000).abs() <= 2, "about 16000 samples, got {}", pcm.len());
        assert!(pcm.iter().any(|&s| s != 0), "a tone must not decode to silence");

        // Stereo 44.1 kHz must come back MONO at 16 kHz: about 0.5 s => about 8000 samples.
        let p2 = dir.path().join("b.wav");
        write_tone_wav(&p2, 44_100, 0.5, 2);
        let (rate2, pcm2) = decode_to_pcm(&p2).expect("decode stereo");
        assert_eq!(rate2, TARGET_SAMPLE_RATE, "output is always the target rate");
        let expected = 8_000i64;
        assert!(
            (pcm2.len() as i64 - expected).abs() < expected / 10,
            "about 8000 mono samples after downmix+resample, got {}",
            pcm2.len()
        );

        // A second call hits the content-hash cache and must return the identical buffer.
        let (rate3, pcm3) = decode_to_pcm(&p).expect("cached decode");
        assert_eq!((rate3, pcm3.len()), (rate, pcm.len()));

        assert!(decode_to_pcm(dir.path().join("missing.wav")).is_err());
    }

    #[test]
    fn get_duration_ms_matches_the_written_length() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (rate, secs, expected) in [(16_000u32, 1.0f32, 1000i64), (44_100, 0.25, 250), (8_000, 2.0, 2000)] {
            let p = dir.path().join(format!("d{rate}_{expected}.wav"));
            write_tone_wav(&p, rate, secs, 1);
            let ms = get_duration_ms(&p).expect("duration");
            assert!((ms - expected).abs() <= 3, "{rate} Hz / {secs}s => about {expected} ms, got {ms}");
        }
        assert!(get_duration_ms(dir.path().join("missing.wav")).is_err());
    }

    #[test]
    fn decode_pcm_windows_streams_the_whole_file_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("stream.wav");
        write_tone_wav(&p, 16_000, 2.0, 1); // 2 s => 32,000 samples at the target rate

        let mut offsets = Vec::new();
        let mut total = 0usize;
        let mut rates = Vec::new();
        decode_pcm_windows(&p, 500, |w| {
            offsets.push(w.offset_ms);
            rates.push(w.sample_rate);
            total += w.pcm.len();
            Ok(())
        })
        .expect("windowed decode");

        assert!(!offsets.is_empty(), "a 2 s file must yield at least one window");
        assert!(rates.iter().all(|&r| r == TARGET_SAMPLE_RATE), "every window is at the target rate");
        // Offsets must strictly increase - a repeated or reordered offset misplaces every
        // downstream segment timestamp.
        assert!(offsets.windows(2).all(|w| w[1] > w[0]), "offsets must increase: {offsets:?}");
        assert_eq!(offsets[0], 0, "the first window starts at the beginning of the file");
        // No samples may be dropped or duplicated across the windowing.
        assert!(
            (total as i64 - 32_000).abs() <= 32,
            "windows must cover the whole file (about 32000 samples), got {total}"
        );

        // A callback error must propagate rather than being swallowed mid-stream.
        let err = decode_pcm_windows(&p, 500, |_| Err(crate::error::AppError::Validation("stop".into())));
        assert!(err.is_err(), "a failing callback must abort the decode");

        assert!(decode_pcm_windows(dir.path().join("missing.wav"), 500, |_| Ok(())).is_err());
    }

    #[test]
    fn non_16khz_window_resampling_is_not_a_whole_buffer_identity_protocol() {
        // Characterization for import identity: the FIR and sinc resamplers need neighboring source
        // samples. Splitting a 44.1 kHz source resets that context at every boundary, so concatenated
        // canonical windows are intentionally NOT byte-identical to one whole-buffer resample. Import,
        // retry and later source verification must therefore all choose one fixed window protocol;
        // switching based on review chunk settings turns unchanged audio into a false replacement.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("route-change-44k.wav");
        write_tone_wav(&path, 44_100, 2.0, 1);

        let (whole_rate, whole_pcm) = decode_to_pcm(&path).expect("whole-buffer decode");
        let whole_hash = crate::fingerprint::AudioFingerprint::content_hash(&whole_pcm, whole_rate);

        let mut windowed = crate::fingerprint::StreamingIdentity::new();
        let mut windows = 0usize;
        decode_pcm_windows(&path, 500, |window| {
            windows += 1;
            windowed.push(&window.pcm, window.sample_rate);
            Ok(())
        })
        .expect("windowed decode");
        let windowed_hash = windowed.finish().content;

        assert!(windows > 1, "fixture must cross a resampling boundary");
        assert_ne!(
            whole_hash, windowed_hash,
            "a route-sensitive identity test must expose the 44.1 kHz boundary-state difference"
        );
    }

    #[test]
    fn evaluation_identity_decoders_use_the_fixed_import_window_protocol() {
        let evaluation_source = include_str!("eval.rs");
        let evaluation_production =
            evaluation_source.split("\n#[cfg(test)]\nmod tests").next().unwrap_or(evaluation_source);
        let identity_decodes = evaluation_production.matches("decode_pcm_windows(").count();
        let fixed_windows = evaluation_production.matches("crate::audio::DECODE_WINDOW_MS").count();
        assert_eq!(identity_decodes, 3, "evaluation has three source-identity/materialization decoders");
        assert_eq!(
            fixed_windows, identity_decodes,
            "every evaluation identity/materialization decoder must use the fixed 90-second import protocol"
        );
    }

    #[test]
    fn normalize_pcm_rms_moves_level_toward_the_target() {
        // A quiet tone normalised to a louder target must get louder, and the waveform shape must
        // survive: normalisation may not invert, clip, or NaN the signal.
        let mut pcm: Vec<f32> =
            (0..1600).map(|i| (i as f32 * 440.0 * 2.0 * std::f32::consts::PI / 16_000.0).sin() * 0.01).collect();
        let signs: Vec<bool> = pcm.iter().map(|v| *v >= 0.0).collect();
        let rms_before = (pcm.iter().map(|v| v * v).sum::<f32>() / pcm.len() as f32).sqrt();

        normalize_pcm_rms(&mut pcm, -20.0);

        let rms_after = (pcm.iter().map(|v| v * v).sum::<f32>() / pcm.len() as f32).sqrt();
        assert!(rms_after > rms_before, "a quiet clip must get louder: {rms_before} -> {rms_after}");
        assert!(pcm.iter().all(|v| v.is_finite()), "normalisation must not produce NaN/inf");
        assert!(pcm.iter().all(|v| v.abs() <= 1.0), "normalisation must not clip past full scale");
        assert!(pcm.iter().zip(&signs).all(|(v, s)| (*v >= 0.0) == *s), "normalisation must not invert the waveform");

        // Digital silence has no RMS to scale: leave it alone rather than divide by zero.
        let mut silent = vec![0.0f32; 128];
        normalize_pcm_rms(&mut silent, -20.0);
        assert!(silent.iter().all(|v| *v == 0.0), "silence must stay silent, not become NaN");
    }

    /// The worker-thread decode wrappers, on both outcomes.
    ///
    /// These exist so a pathological file cannot wedge the import pipeline forever, and their
    /// timeout branch is exactly the one that must work when it is finally needed - yet nothing
    /// exercised either wrapper or the shared `send_decode_worker_result` helper.
    #[test]
    fn timeout_wrappers_return_the_decode_or_a_transient_timeout_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("t.wav");
        write_tone_wav(&p, 16_000, 1.0, 1);

        // Generous budget: must behave exactly like the direct call.
        let (rate, pcm) = decode_to_pcm_with_timeout(&p, Duration::from_secs(30)).expect("decode within budget");
        assert_eq!(rate, TARGET_SAMPLE_RATE);
        assert!((pcm.len() as i64 - 16_000).abs() <= 2);

        // The windowed decoder hands windows back on THIS thread, so a plain local counter is enough
        // (and is the point of the API: no Send + 'static bound on the callback).
        let mut seen_count = 0usize;
        decode_pcm_windows_streaming(p.clone(), 500, Duration::from_secs(30), |_w, _last| {
            seen_count += 1;
            Ok(())
        })
        .expect("windowed decode within budget");
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(seen_count));
        assert!(
            seen.load(std::sync::atomic::Ordering::SeqCst) > 0,
            "the timeout wrapper must actually deliver windows, not just return Ok"
        );

        // A missing file must surface the decode error, not a timeout.
        let missing = decode_to_pcm_with_timeout(dir.path().join("missing.wav"), Duration::from_secs(30));
        assert!(missing.is_err());

        // An impossibly small budget must produce a TIMEOUT error, and that error must be
        // classified transient so the caller's retry logic engages (a permanent classification
        // would turn a slow disk into a hard import failure).
        let timed_out = decode_to_pcm_with_timeout(&p, Duration::from_nanos(1));
        if let Err(e) = timed_out {
            assert!(is_transient_decode_error(&e), "a decode timeout must be retryable, got: {e}");
        }
        // Not asserted as always-Err: a 1 ns budget races a fast local decode, and demanding the
        // timeout would be a flaky test. The classification above is the part that matters, and it
        // is checked whenever the race lands on the timeout side.
    }

    /// The streaming decoder's four contracts, all of which a long import depends on.
    ///
    /// Before 2026-08-17 its predecessor's `Send + 'static` callback left the import no choice but to
    /// buffer every window and process the file afterwards, so a 1.5 h recording held ~170 MB of PCM
    /// before planning a single chunk — the "streaming" path was streaming in name only.
    #[test]
    fn streaming_decode_is_bounded_flags_the_last_window_and_never_fakes_an_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("s.wav");
        write_tone_wav(&p, 16_000, 2.0, 1); // 2 s at 500 ms per window => 4 windows

        // 1. Every window is delivered, in order, exactly once, and only the FINAL one is is_last.
        let mut offsets = Vec::new();
        let mut last_flags = Vec::new();
        decode_pcm_windows_streaming(p.clone(), 500, Duration::from_secs(30), |w, is_last| {
            offsets.push(w.offset_ms);
            last_flags.push(is_last);
            Ok(())
        })
        .expect("streaming decode");
        assert!(offsets.len() >= 2, "a 2 s file at 500 ms windows must yield several windows: {offsets:?}");
        assert!(offsets.windows(2).all(|w| w[1] > w[0]), "offsets must increase: {offsets:?}");
        assert_eq!(offsets[0], 0, "the first window starts at the beginning of the file");
        assert_eq!(
            last_flags.iter().filter(|f| **f).count(),
            1,
            "exactly one window ends the file, got {last_flags:?}"
        );
        assert!(*last_flags.last().expect("a window"), "the LAST delivered window is the one flagged");

        // 2. A SLOW consumer (the real one does ASR per window) must still receive the whole file.
        //    The bound itself is structural — `sync_channel(1)` back-pressures the decoder — and the
        //    risk that buys is a stall, so that is what is worth a test: the decoder waits for a
        //    consumer slower than it, and neither side deadlocks.
        let mut slow_offsets = Vec::new();
        decode_pcm_windows_streaming(p.clone(), 500, Duration::from_secs(30), |w, _| {
            std::thread::sleep(Duration::from_millis(20));
            slow_offsets.push(w.offset_ms);
            Ok(())
        })
        .expect("streaming decode with a slow consumer");
        assert_eq!(slow_offsets, offsets, "back-pressure must not drop, duplicate, or reorder a window");

        // 3. A consumer error stops the decode instead of being swallowed mid-stream — and because
        //    the receiver drops with it, the worker's next send fails and its decode loop exits too.
        let mut delivered = 0usize;
        let stopped = decode_pcm_windows_streaming(p.clone(), 500, Duration::from_secs(30), |_w, _| {
            delivered += 1;
            Err(crate::error::AppError::Validation("stop".into()))
        });
        assert!(stopped.is_err(), "a failing consumer must abort the decode");
        assert_eq!(delivered, 1, "the decode must stop at the first refusal, not run the file out");

        // 4. A decode that never starts is an error, not a silent zero-window success.
        assert!(decode_pcm_windows_streaming(
            dir.path().join("missing.wav"),
            500,
            Duration::from_secs(30),
            |_w, _| Ok(())
        )
        .is_err());
    }

    /// `ensure_pcm_16khz` is the rate-normalisation seam every decode path funnels through.
    #[test]
    fn ensure_pcm_16khz_passes_through_or_resamples() {
        // Already at the target rate: returned untouched, same length, same samples.
        let pcm: Vec<i16> = (0..1000).map(|i| (i % 200) as i16 * 10).collect();
        let (rate, out) = ensure_pcm_16khz(TARGET_SAMPLE_RATE, pcm.clone()).expect("passthrough");
        assert_eq!(rate, TARGET_SAMPLE_RATE);
        assert_eq!(out, pcm, "a clip already at the target rate must not be resampled at all");

        // Downsampling halves the sample count (48 kHz -> 16 kHz is 3:1).
        let (rate2, out2) = ensure_pcm_16khz(48_000, pcm.clone()).expect("downsample");
        assert_eq!(rate2, TARGET_SAMPLE_RATE);
        let expected = 1000 / 3;
        assert!(
            (out2.len() as i64 - expected as i64).abs() < 20,
            "48k->16k is 3:1, expected about {expected}, got {}",
            out2.len()
        );

        // Upsampling grows it (8 kHz -> 16 kHz is 1:2).
        let (rate3, out3) = ensure_pcm_16khz(8_000, pcm.clone()).expect("upsample");
        assert_eq!(rate3, TARGET_SAMPLE_RATE);
        assert!((out3.len() as i64 - 2000).abs() < 40, "8k->16k is 1:2, expected about 2000, got {}", out3.len());
        assert!(out3.iter().all(|s| s.abs() as i32 <= i16::MAX as i32));

        // Empty input must stay empty rather than panic on an empty resample window.
        let (_, empty) = ensure_pcm_16khz(44_100, Vec::new()).expect("empty");
        assert!(empty.is_empty());
    }

    /// Voice activity detection over the REAL entry point, on whichever backend this machine has.
    ///
    /// Deliberately not skipped when the ONNX model is missing. `voice_activity_detection` reports
    /// which backend actually ran, so the test asserts the invariants that must hold either way and
    /// then states the backend - it always exercises one complete real path (Silero when the model
    /// is present, as on the dev rig and in CI after `npm run fetch-models`; the energy fallback
    /// otherwise) and can never pass vacuously by exercising neither.
    #[test]
    fn voice_activity_detection_finds_bursts_on_whichever_backend_is_available() {
        // 3 s at 16 kHz: silence, a 1 s loud tone, silence. One obvious speech region.
        let rate = TARGET_SAMPLE_RATE;
        let mut pcm = vec![0i16; rate as usize]; // 1 s silence
        pcm.extend(
            (0..rate as usize)
                .map(|i| ((i as f32 * 220.0 * 2.0 * std::f32::consts::PI / rate as f32).sin() * 14000.0) as i16),
        );
        pcm.extend(vec![0i16; rate as usize]); // 1 s silence

        let (regions, backend) = voice_activity_detection(&pcm, rate, 0.5).expect("vad must not error");
        println!("VAD backend in this environment: {backend:?}");

        assert!(!regions.is_empty(), "a 1 s tone between silences must yield at least one region");
        for (start, end) in &regions {
            assert!(start < end, "every region must be non-empty: {start}..{end}");
            assert!(*end <= pcm.len(), "a region may not run past the buffer: {end} > {}", pcm.len());
        }
        // Regions must be ordered and non-overlapping, or downstream chunk timestamps interleave.
        for pair in regions.windows(2) {
            assert!(pair[0].1 <= pair[1].0, "regions must not overlap: {:?} then {:?}", pair[0], pair[1]);
        }
        // The detected speech must actually intersect the loud middle second.
        let (lo, hi) = (rate as usize, 2 * rate as usize);
        assert!(
            regions.iter().any(|(s, e)| *s < hi && *e > lo),
            "no region overlapped the tone at {lo}..{hi}: {regions:?}"
        );
        assert!(
            matches!(backend, VadBackend::Silero | VadBackend::Energy),
            "a non-empty buffer must be attributed to a real backend, got {backend:?}"
        );

        // Empty input is the documented None case - no regions, no backend claimed.
        let (empty, none_backend) = voice_activity_detection(&[], rate, 0.5).expect("empty vad");
        assert!(empty.is_empty());
        assert!(matches!(none_backend, VadBackend::None), "empty audio must claim no backend");

        // Pure silence returns the WHOLE buffer as one region, still labelled with the backend that
        // ran. That surprised me, so it is pinned here with the reasoning rather than left implicit:
        // `probs_to_segments` deliberately falls back to (0, total) when it finds no speech, so a
        // file is never silently dropped, and `VadBackend::Silero` documents "Silero ran
        // successfully" - not "Silero positively detected speech". `VadBackend::None` means no VAD
        // ran at all, which is not this case. The labelling is therefore correct, but the
        // distinction matters to anyone reading `vad_backend` in an export: a silero-labelled
        // segment does NOT imply positive detection.
        let (silent_regions, silent_backend) =
            voice_activity_detection(&vec![0i16; rate as usize], rate, 0.5).expect("silence vad");
        assert_eq!(silent_regions.len(), 1, "no-speech audio collapses to a single whole-buffer region");
        assert_eq!(silent_regions[0], (0, rate as usize), "and that region spans the entire buffer");
        assert!(
            !matches!(silent_backend, VadBackend::None),
            "a backend DID run, so it must be named, not reported as None: {silent_backend:?}"
        );
    }

    /// VAD on audio that is NOT already at 16 kHz.
    ///
    /// Silero runs at a fixed rate, so `detect` resamples internally and then maps the segment
    /// boundaries back onto the caller's original sample indices. That mapping is a separate branch
    /// from the 16 kHz passthrough and had no coverage - yet importing an 8 kHz phone recording or a
    /// 44.1 kHz download is entirely ordinary, and a wrong mapping there silently misplaces every
    /// chunk boundary in the file.
    #[test]
    fn vad_maps_segments_back_to_the_source_rate() {
        for rate in [8_000u32, 44_100] {
            // 3 s at `rate`: silence, a 1 s tone, silence.
            let sec = rate as usize;
            let mut pcm = vec![0i16; sec];
            pcm.extend(
                (0..sec)
                    .map(|i| ((i as f32 * 220.0 * 2.0 * std::f32::consts::PI / rate as f32).sin() * 14000.0) as i16),
            );
            pcm.extend(vec![0i16; sec]);

            let (regions, backend) = voice_activity_detection(&pcm, rate, 0.5)
                .unwrap_or_else(|e| panic!("vad at {rate} Hz must not error: {e}"));
            assert!(!regions.is_empty(), "{rate} Hz: expected at least one region");
            for (s, e) in &regions {
                assert!(s < e, "{rate} Hz: empty region {s}..{e}");
                // The mapped indices must land inside the CALLER's buffer, not the internal
                // 16 kHz one - this is exactly what the back-mapping exists to guarantee.
                assert!(
                    *e <= pcm.len(),
                    "{rate} Hz: region end {e} exceeds the source buffer ({}) - segments were not \
                     mapped back to the source rate",
                    pcm.len()
                );
            }
            assert!(
                matches!(backend, VadBackend::Silero | VadBackend::Energy),
                "{rate} Hz: a real backend must be named, got {backend:?}"
            );
        }
    }

    /// The direct `SileroVad::new` constructor - the public API a caller uses without going through
    /// `voice_activity_detection`'s session cache. Skipped only when the bundled model is genuinely
    /// absent, and it says so rather than passing quietly.
    #[test]
    fn silero_vad_constructs_from_the_bundled_model_and_detects() {
        let model_path = crate::models::resolve_model_file("silero_vad_v4.onnx");
        if !model_path.exists() {
            println!("SKIP: silero_vad_v4.onnx not present at {model_path:?} - run npm run fetch-models");
            return;
        }
        crate::models::init_ort_dylib_path();

        let mut vad = SileroVad::new(&model_path, TARGET_SAMPLE_RATE, 0.5).expect("construct from bundled model");

        let sec = TARGET_SAMPLE_RATE as usize;
        let mut pcm = vec![0i16; sec / 2];
        pcm.extend((0..sec).map(|i| {
            ((i as f32 * 220.0 * 2.0 * std::f32::consts::PI / TARGET_SAMPLE_RATE as f32).sin() * 14000.0) as i16
        }));
        pcm.extend(vec![0i16; sec / 2]);

        let regions = vad.detect(&pcm).expect("detect on a real tone");
        assert!(!regions.is_empty(), "a 1 s tone must produce at least one region");
        assert!(regions.iter().all(|(s, e)| s < e && *e <= pcm.len()), "regions must be well formed");

        // The same instance must be reusable - `detect` carries recurrent state between calls, and a
        // second call must not error or return garbage indices.
        let again = vad.detect(&pcm).expect("second detect on the same instance");
        assert!(again.iter().all(|(s, e)| s < e && *e <= pcm.len()));
    }

    #[test]
    fn test_compute_waveform_empty() {
        assert!(compute_waveform(&[], 100).is_empty());
    }

    #[test]
    fn energy_vad_fallback_keeps_a_short_word_like_silero_does() {
        // Round-24 #6 lowered the SILERO floor to ~96ms (min_speech_frames=3) so real short words
        // (بەڵێ / "yes") survive, but the energy FALLBACK — used when silero_vad_v4.onnx is absent — kept a
        // 30-hop (~300ms) floor and silently DROPPED any 96-300ms utterance Silero would retain. Build a
        // ~150ms loud burst flanked by silence: the fallback must keep it as its own sub-segment, not drop
        // it (which collapses to the whole-buffer default and loses the word's boundary).
        let sr = 16_000u32;
        let ms = |m: usize| m * sr as usize / 1000;
        let mut pcm = vec![0i16; ms(400)]; // leading silence
        pcm.extend(std::iter::repeat(8000i16).take(ms(150))); // ~150ms burst, well above the noise floor
        pcm.extend(std::iter::repeat(0i16).take(ms(400))); // trailing silence

        let segs = vad_energy_fallback(&pcm, sr, 0.5).unwrap();
        assert_eq!(segs.len(), 1, "the single short burst must be exactly one segment, got {segs:?}");
        let (start, end) = segs[0];
        // Fail-before discriminator: the old 300ms floor drops the 150ms burst and the fallback collapses
        // to the whole buffer (0, len); the corrected floor keeps it as a proper sub-segment.
        assert!(end - start < pcm.len(), "fallback dropped the ~150ms burst (collapsed to whole buffer): {segs:?}");
        assert!(start >= ms(300), "the kept segment must bound the burst region, not start at 0: {segs:?}");
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
        let (result, backend) = voice_activity_detection(&[], 16000, 0.5).unwrap();
        assert!(result.is_empty());
        assert_eq!(backend, VadBackend::None, "empty input runs no VAD");
    }

    #[test]
    fn test_vad_silence_returns_segment() {
        let pcm = vec![0i16; 16000];
        let (segments, _backend) = voice_activity_detection(&pcm, 16000, 0.5).unwrap();
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
    fn resample_anti_aliases_above_target_nyquist_but_passes_speech_band() {
        use std::f64::consts::PI;
        let rms = |v: &[f32]| (v.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / v.len() as f64).sqrt();
        let tone =
            |hz: f64| -> Vec<f32> { (0..22050).map(|i| (2.0 * PI * hz * i as f64 / 44100.0).sin() as f32).collect() };
        // A 14 kHz tone sits ABOVE the 16 kHz target's 8 kHz Nyquist. Without an anti-alias filter it
        // folds back into the speech band (~2 kHz) at near-full amplitude; with the windowed-sinc
        // low-pass it is removed. Input RMS is ~0.707, so a strong attenuation proves the filter works.
        let high = resample(&tone(14_000.0), 44100, 16000);
        assert!(rms(&high) < 0.30, "14 kHz tone must be attenuated below target Nyquist, got rms {}", rms(&high));
        // A 1 kHz tone is well inside the speech band and must pass through essentially intact.
        let low = resample(&tone(1_000.0), 44100, 16000);
        assert!(rms(&low) > 0.55, "1 kHz speech-band tone must be preserved, got rms {}", rms(&low));
    }

    #[test]
    fn resample_upsampling_does_not_panic_on_overshoot_ratios() {
        // Round-16: new_len = ceil(len * to/from) can overshoot for certain upsample ratios so the final
        // iteration's floor(src_idx) reaches src.len(); when only `hi` was clamped, src[lo] panicked with
        // index-out-of-bounds. 7350 Hz (44100/6) and 14700 Hz (44100/3) are real HE-AAC/legacy rates, and
        // length 147 is a confirmed trigger. Assert these return cleanly (and at the expected length).
        for &(from, len) in &[(14700u32, 147usize), (7350, 147), (14700, 294), (7350, 2499)] {
            let input = vec![0.25f32; len];
            let out = resample(&input, from, 16000);
            let expected = (len as f64 * (16000.0 / from as f64)).ceil() as usize;
            assert_eq!(out.len(), expected, "resample({from}->16000, len={len}) length");
        }
        // Spot-check the previously-safe rates still work.
        for &from in &[8000u32, 11025, 12000, 22050, 44100, 48000] {
            let _ = resample(&vec![0.1f32; 147], from, 16000);
        }
    }

    #[test]
    fn resample_bounds_output_length_against_a_corrupt_low_sample_rate() {
        // A corrupt/crafted WAV can declare sample_rate=1 (it survives decode's `> 0` guard). Upsampling
        // it to 16 kHz is ratio 16000, so an unbounded new_len = src.len() * 16000 would be a
        // hundreds-of-GB Vec::with_capacity that ABORTS the whole process. The output length must be
        // capped to a sane upsample factor. Tiny input keeps the (bounded) allocation trivial.
        let out = resample(&vec![0.1f32; 1000], 1, 16000);
        assert!(out.len() <= 1000 * 16, "a corrupt low from_rate must not upsample past the cap: got {}", out.len());
        // The cap never clips a LEGITIMATE resample: 8 kHz -> 16 kHz still doubles (ratio 2, far under 16x)...
        assert_eq!(resample(&vec![0.2f32; 1000], 8000, 16000).len(), 2000, "8k->16k doubles length");
        // ...and a downsample shrinks, nowhere near the cap.
        assert_eq!(resample(&vec![0.3f32; 3000], 48000, 16000).len(), 1000, "48k->16k thirds the length");
    }

    #[test]
    fn resample_attenuates_above_nyquist_tone() {
        // Round-11 audit (aliasing): downsampling must low-pass BEFORE decimation. A tone above the new
        // Nyquist (8 kHz for a 16 kHz target) must be attenuated, not folded into the passband. Compare
        // a 9 kHz tone (above Nyquist) vs a 1 kHz tone (in band), both 48 kHz, after resampling to 16 kHz.
        let sr = 48000.0f64;
        let n = 48000usize; // 1 second
        let tone = |hz: f64| -> Vec<f32> {
            (0..n).map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / sr).sin() as f32).collect()
        };
        let energy =
            |s: &[f32]| -> f64 { s.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / s.len().max(1) as f64 };

        let e_in = energy(&resample(&tone(1000.0), 48000, 16000));
        let e_above = energy(&resample(&tone(9000.0), 48000, 16000));

        // The in-band tone passes through; the above-Nyquist tone is strongly attenuated. Before the
        // anti-aliasing fix the 9 kHz tone aliased to 7 kHz and retained comparable (un-attenuated) energy.
        assert!(e_in > 0.1, "in-band 1 kHz tone should pass: energy {e_in}");
        assert!(
            e_above < e_in * 0.1,
            "above-Nyquist 9 kHz tone must be attenuated >10x vs in-band: {e_above} vs {e_in}"
        );
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
    fn get_duration_ms_reports_real_duration_for_wav() {
        use hound::{WavSpec, WavWriter};
        use tempfile::TempDir;

        // Common metadata path: a WAV always carries n_frames, so this must stay byte-identical
        // to the old behavior (a regression guard for the unknown-frame fallback added to
        // get_duration_ms). Exactly 1 second of 16 kHz mono audio => 1000 ms.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("one_sec.wav");
        let spec =
            WavSpec { channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = WavWriter::create(&path, spec).unwrap();
        for _ in 0..16_000 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();

        assert_eq!(get_duration_ms(&path).unwrap(), 1000);
    }

    #[test]
    fn check_audio_and_get_duration_agree_via_shared_helper() {
        use hound::{WavSpec, WavWriter};
        use tempfile::TempDir;

        // Round-25 hunt: check_audio_file reported duration_ms=0 for a file get_duration_ms measured
        // correctly, because they duplicated the duration logic and check_audio used num_frames.unwrap_or(0)
        // with NO decode fallback. Both now route through duration_ms_with_decode_fallback, so they must
        // AGREE and report the real duration. Deterministic (WAV frame-count path, no decode): a source
        // policy (test_rust_runtime_panic_policy.py) pins that both functions use the shared helper; the
        // no-frame-count fallback branch itself is get_duration_ms's pre-existing, decode-based path.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("one_sec.wav");
        let spec =
            WavSpec { channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = WavWriter::create(&path, spec).unwrap();
        for _ in 0..16_000 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();

        let info = check_audio_file(&path).unwrap();
        assert_eq!(info.duration_ms, 1000, "check_audio reports the real duration, not 0");
        assert_eq!(
            info.duration_ms,
            get_duration_ms(&path).unwrap(),
            "check_audio and get_duration_ms must agree (they route through the same helper)"
        );
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

    #[test]
    fn decode_to_pcm_preserves_i16_sample_values_within_1_lsb() {
        use hound::{WavSpec, WavWriter};
        use tempfile::TempDir;

        // ABSOLUTE-value scaling guard (added after the symphonia 0.6 migration). A 16 kHz MONO WAV
        // decodes with NO resample and NO downmix, so the ONLY difference between the written samples
        // and the decoded PCM is the i16 -> f32 (÷32768) -> i16 (×32767) round-trip, which is ≤ ~1 LSB.
        // The other decode tests only check that DIFFERENT audio yields DIFFERENT PCM (relative), which
        // would NOT catch a decoder bump that stopped normalizing i16 to [-1, 1] (every sample would be
        // 32768× off). This test would. Pattern spans zero, both signs, small/large magnitudes, and the
        // i16 extremes. Uses a unique temp file (its own content hash), so it needs no cache clear and
        // leaves the shared global PCM cache untouched for the other tests running in parallel.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("scaling_roundtrip.wav");
        let spec =
            WavSpec { channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };

        let input: Vec<i16> = (0..2048)
            .map(|i| ((i as f64 * std::f64::consts::TAU / 128.0).sin() * 30000.0) as i16)
            .chain([0i16, 1, -1, 100, -100, 16384, -16384, i16::MAX, i16::MIN])
            .collect();
        {
            let mut writer = WavWriter::create(&path, spec).unwrap();
            for &s in &input {
                writer.write_sample(s).unwrap();
            }
            writer.finalize().unwrap();
        }

        let (rate, decoded) = decode_to_pcm(&path).unwrap();
        assert_eq!(rate, TARGET_SAMPLE_RATE);
        assert_eq!(decoded.len(), input.len(), "mono 16 kHz decode must preserve the exact sample count");

        let max_err =
            input.iter().zip(&decoded).map(|(&a, &b)| (a as i32 - b as i32).unsigned_abs()).max().unwrap_or(0);
        assert!(max_err <= 2, "i16 sample round-trip drifted by {max_err} LSB — i16<->f32 scaling regression?");
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

        // I/O kind matters: transient kinds retry, permanent ones must not.
        use std::io::{Error as IoError, ErrorKind};
        let interrupted = AppError::Io(IoError::from(ErrorKind::Interrupted));
        assert!(is_transient_decode_error(&interrupted), "Interrupted I/O should retry");
        let timed_out = AppError::Io(IoError::from(ErrorKind::TimedOut));
        assert!(is_transient_decode_error(&timed_out), "TimedOut I/O should retry");
        let not_found = AppError::Io(IoError::from(ErrorKind::NotFound));
        assert!(!is_transient_decode_error(&not_found), "NotFound I/O must NOT retry");
        let denied = AppError::Io(IoError::from(ErrorKind::PermissionDenied));
        assert!(!is_transient_decode_error(&denied), "PermissionDenied I/O must NOT retry");
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
        // cargo runs tests in parallel and they SHARE the global PCM cache, so this test must not assume
        // the cache is otherwise empty. Assert on this test's OWN unique key ("poisoned.wav", used by no
        // other test) instead of an absolute len(): a concurrent decode_to_pcm legitimately leaves
        // unrelated entries, which made the old `assert_eq!(len, 1)` flaky under CI's test ordering.
        {
            let mut cache = lock_pcm_cache();
            cache.put("poisoned.wav".into(), (TARGET_SAMPLE_RATE, vec![1, 2, 3]));
            assert!(cache.contains("poisoned.wav"), "entry must be present immediately after put");
        }

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = PCM_CACHE.lock().expect("lock PCM cache");
            panic!("poison PCM cache");
        }));

        clear_pcm_cache();
        assert!(!lock_pcm_cache().contains("poisoned.wav"), "clear must recover the poisoned lock and drop the entry");
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

        // ONE guard for the write AND the check. Taking the lock twice opens a window in which any
        // other test in the parallel suite that constructs a VAD repopulates this global, and the
        // assertion then fails for a reason that has nothing to do with poison recovery. Observed
        // 2026-08-09: green when run alone, red inside the full 1157-test run.
        //
        // What is being asserted is unchanged and is what the name promises: after a panic while the
        // lock was held, the lock is USABLE again and a write through it takes effect. The value of a
        // process-wide cache at some later instant was never this test's property to own.
        let mut cache = lock_vad_cache();
        *cache = None;
        assert!(cache.is_none(), "a poisoned lock must still be usable, and a write through it must land");
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
            let (result, _backend) = voice_activity_detection(&[], sample_rate, 0.5).unwrap();
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
