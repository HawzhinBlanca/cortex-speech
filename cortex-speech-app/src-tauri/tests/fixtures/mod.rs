#![allow(dead_code)]

use hound::{WavSpec, WavWriter};
use std::path::Path;

/// Generate a valid WAV file with a sine wave at the given frequency.
pub fn create_test_wav(path: &Path, duration_secs: f64, sample_rate: u32, frequency: f64) -> hound::Result<()> {
    let spec = WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut writer = WavWriter::create(path, spec)?;
    let num_samples = (sample_rate as f64 * duration_secs) as u64;
    for i in 0..num_samples {
        let t = i as f64 / sample_rate as f64;
        let sample = (i16::MAX as f64 * (2.0 * std::f64::consts::PI * frequency * t).sin()) as i16;
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Generate a valid WAV file with silence (all zeros).
pub fn create_silent_wav(path: &Path, duration_secs: f64, sample_rate: u32) -> hound::Result<()> {
    let spec = WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut writer = WavWriter::create(path, spec)?;
    let num_samples = (sample_rate as f64 * duration_secs) as u64;
    for _ in 0..num_samples {
        writer.write_sample(0i16)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Create a WAV file with only a header and no audio samples.
pub fn create_empty_wav(path: &Path, sample_rate: u32) -> hound::Result<()> {
    let spec = WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let writer = WavWriter::create(path, spec)?;
    writer.finalize()?;
    Ok(())
}

/// Names the test in flight so the next `0xC0000409` abort says which one it was.
///
/// THE CRASH IS NOT BINARY-SPECIFIC, which is why this lives here rather than in one test file.
/// `fatal runtime error: Rust cannot catch foreign exceptions` / STATUS_STACK_BUFFER_OVERRUN is a C++
/// exception escaping the native ONNX/sherpa stack across the FFI boundary, and it lands on whichever
/// test binary happens to be exercising that path: `e2e_pipeline` twice, then `pipeline_integration`
/// once the first was instrumented and passed cleanly. Arming a single file would just move the blind
/// spot, which is exactly what the first attempt did.
///
/// The process dies before libtest can attribute anything, and it has never reproduced standalone, so
/// this has to be armed during the sweep where the crash actually happens.
///
/// Two mechanisms, because either alone is insufficient:
///
///   1. A per-binary MUTEX serialises that binary's tests, so exactly one is ever in flight. Scoped
///      this way on purpose: `--test-threads=1` on the whole gate was MEASURED at 365 s vs 140 s for
///      the lib suite alone (2.6x), ~14 minutes added to every sweep. The mutex costs a few seconds.
///   2. An explicitly FLUSHED breadcrumb file. libtest writes `test <name> ... ` WITHOUT a trailing
///      newline and stdout to a pipe is line-buffered, so on an abort that line dies in the buffer with
///      everything else. A flushed file survives — an abort cannot unwind what is already on disk.
///
/// On a clean run every START is followed by its END. After an abort the last START stands alone, and
/// that is the answer. Verified under a real SIGKILL, not argued.
///
/// Best-effort throughout: every file operation ignores errors and a poisoned lock is recovered rather
/// than propagated. A diagnostic that could fail a healthy test would be worse than no diagnostic.
pub fn crash_breadcrumb(binary: &'static str, test_name: &'static str) -> impl Drop {
    use std::io::Write;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

    fn note(line: &str) {
        // Beside the other sweep artifacts, so a crash investigation finds it with the FAIL/CRASH logs.
        let path = std::env::temp_dir().join("cortex-verify10").join("native_crash.breadcrumb.log");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }

    struct Breadcrumb {
        label: String,
        _guard: MutexGuard<'static, ()>,
    }
    impl Drop for Breadcrumb {
        fn drop(&mut self) {
            note(&format!("END   {}", self.label));
        }
    }

    // A poisoned lock means an EARLIER test panicked, which is not a reason to stop naming this one.
    let guard = SERIAL.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|p| p.into_inner());
    // The binary is part of the label because cargo interleaves nothing but the log is shared: an
    // occurrence must say WHICH binary as well as which test, and cargo's own line can scroll far away.
    let label = format!("{binary}::{test_name}");
    note(&format!("START {label}"));
    Breadcrumb { label, _guard: guard }
}
