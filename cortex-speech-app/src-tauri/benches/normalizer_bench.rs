use cortex_speech_app_lib::normalizer::SoraniNormalizer;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn gen_text(word_count: usize) -> String {
    let words: Vec<String> = (0..word_count).map(|i| format!("کوردی ساڵانی {} و ژیانی ئەم کەسە", i)).collect();
    words.join(" ")
}

fn bench_normalizer(c: &mut Criterion) {
    let normalizer = SoraniNormalizer::new();

    let mut group = c.benchmark_group("normalizer");

    for &n in &[100, 1000, 10000] {
        let text = gen_text(n);
        group.bench_with_input(format!("{n}_words"), &text, |b, t| {
            b.iter(|| normalizer.normalize(black_box(t)));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_normalizer);
criterion_main!(benches);
