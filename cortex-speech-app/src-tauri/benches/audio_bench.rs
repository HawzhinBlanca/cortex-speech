use cortex_speech_app_lib::audio;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn gen_synthetic_wav(num_samples: usize) -> Vec<i16> {
    (0..num_samples)
        .map(|i| (i16::MAX as f64 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 16000.0).sin()) as i16)
        .collect()
}

fn bench_decode_to_pcm(c: &mut Criterion) {
    let mut group = c.benchmark_group("audio");

    for &n in &[16000, 160000, 1600000] {
        let pcm = gen_synthetic_wav(n);
        let label = format!("decode_{}_samples", n);

        // Benchmark compute_waveform (not actual decode since that requires file I/O)
        group.bench_with_input(format!("waveform_{}", label), &pcm, |b, data| {
            b.iter(|| audio::compute_waveform(black_box(data), black_box(100)));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_decode_to_pcm);
criterion_main!(benches);
