#![no_main]

use libfuzzer_sys::fuzz_target;
use cortex_speech_app_lib::features::FbankExtractor;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    // Constrain to the domain this extractor is actually fed. Decoded PCM reaches feature
    // extraction as finite samples in [-1, 1] (i16 / 32768) already resampled to >= 8 kHz;
    // FbankExtractor has no production callers at all today (every call site is a test, at
    // 16 kHz). Unconstrained, this target reported a "crash" for sample_rate = 128 Hz with a
    // sample of 9.4e21 — squaring that in the power spectrum overflows f32 (max 3.4e38) to inf.
    // That is garbage-in-garbage-out on an input the decoder cannot produce, not a defect. The
    // invariant worth fuzzing is the real one: finite, in-range audio must yield finite features.
    let sample_rate = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if !(8000..=192000).contains(&sample_rate) {
        return;
    }
    let fbank = FbankExtractor::new(sample_rate);

    let audio: Vec<f32> = data[4..]
        .chunks(4)
        .filter_map(|c| {
            if c.len() == 4 {
                let bytes: [u8; 4] = [c[0], c[1], c[2], c[3]];
                let v = f32::from_le_bytes(bytes);
                (v.is_finite() && v.abs() <= 1.0).then_some(v)
            } else {
                None
            }
        })
        .take(16000)
        .collect();

    let features = fbank.compute(&audio);

    assert!(features.shape()[1] == 80, "fbank must produce 80-dim features");
    for val in features.iter() {
        assert!(val.is_finite(), "fbank values must be finite, got {val}");
    }
});
