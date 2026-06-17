#![no_main]

use libfuzzer_sys::fuzz_target;
use cortex_speech_app_lib::features::FbankExtractor;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let sample_rate = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if sample_rate == 0 || sample_rate > 192000 {
        return;
    }
    let fbank = FbankExtractor::new(sample_rate);

    let audio: Vec<f32> = data[4..]
        .chunks(4)
        .filter_map(|c| {
            if c.len() == 4 {
                let bytes: [u8; 4] = [c[0], c[1], c[2], c[3]];
                Some(f32::from_le_bytes(bytes))
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
