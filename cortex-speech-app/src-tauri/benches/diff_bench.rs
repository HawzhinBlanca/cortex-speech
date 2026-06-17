use cortex_speech_app_lib::diff;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn gen_text(word_count: usize) -> String {
    let words: Vec<String> = (0..word_count).map(|i| format!("word{i}")).collect();
    words.join(" ")
}

fn bench_diff(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff");

    for &n in &[10, 100, 1000] {
        let raw = gen_text(n);

        // Identical text
        group.bench_with_input(format!("identical_{n}_words"), &raw, |b, t| {
            b.iter(|| diff::compute_diff(black_box(t), black_box(t)));
        });

        // Divergent text (every 5th word changed)
        let mut ann_words: Vec<String> = (0..n).map(|i| format!("word{i}")).collect();
        for i in (4..n).step_by(5) {
            ann_words[i] = format!("CHANGED{i}");
        }
        let ann = ann_words.join(" ");
        group.bench_with_input(format!("divergent_{n}_words"), &(&raw, &ann), |b, (r, a)| {
            b.iter(|| diff::compute_diff(black_box(r), black_box(a)));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_diff);
criterion_main!(benches);
