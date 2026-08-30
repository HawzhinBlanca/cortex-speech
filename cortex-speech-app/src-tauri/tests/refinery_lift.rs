//! refinery-lift — the WS4 charter gate: fixed-seed injected-error synthetic benchmark.
//!
//! Proves the Disagreement Refinery's T0 algorithm (IRT consensus + per-bucket conformal
//! certification, the real compiled `jury::run_t0_gate` path — nothing reimplemented)
//! delivers >=30% relative CER reduction over the raw local ASR at <=15% human
//! escalation, on a fully offline, deterministic corpus.
//!
//! Scope boundary: schema 60+ deliberately disables machine verdict publication. This model-evidence
//! benchmark therefore rolls its disposable database back to the last compatible schema (59) before
//! seeding calibration truth and exercising the algorithm. It proves algorithmic lift only; it is not
//! evidence that current product review truth may be machine-authored, and it is not an owner-product
//! certification substitute.
//!
//! Design (honest by construction):
//! - Every reference is built ONLY from words of the committed, human-verified FLEURS
//!   ckb fixture transcript (tests/fixtures/fleurs_ckb_sample.txt) — no invented Sorani.
//! - Three voters mirror the production local ensemble: wsl-7b (anchor, strongest),
//!   ctc-1b, and omniasr-ctc-300m. The gated baseline is the raw CTC voter — exactly
//!   the charter's definition (">=30% CER reduction vs raw CTC"). HONESTY NOTE: in the
//!   owner's shipped default config `raw_transcript` comes from the WSL-7B champion,
//!   not the CTC — so the benchmark also PRINTS the jury-vs-strongest-voter CER for
//!   transparency; the charter gate itself is vs raw CTC, matching the 300M/offline
//!   configuration where the refinery's arbitration earns its keep.
//!   Each voter's hypothesis is the reference with seeded per-word char corruptions at
//!   that voter's error rate.
//! - Calibration (verified) and test segments are drawn from the SAME generative
//!   distribution, so the conformal exchangeability assumption genuinely holds. The
//!   calibration volume (5,000) is what `conformal::min_calibration_n` says the shipped
//!   5%-target/Bonferroni math needs — the benchmark synthesizes a properly calibrated
//!   bucket rather than loosening any constant.
//! - Escalated segments keep the RAW transcript in the post-jury column (design is
//!   conservative: escalation is counted as budget spent, never as free accuracy).
//! - Fixed seed end to end; the EM fit is deterministic (sorted M-step).

use cortex_speech_app_lib::db::{Database, SpeechSegment};
use cortex_speech_app_lib::eval::compute_label_quality_lift;
use cortex_speech_app_lib::jury::{self, T0Decision};
use cortex_speech_app_lib::settings::AutonLevel;
use tempfile::NamedTempFile;

const SEED: u64 = 0xC0DE_5EED;
const N_CAL: usize = 5000;
const N_TEST: usize = 400;
const WORDS_MIN: usize = 5;
const WORDS_MAX: usize = 9;
// Per-word corruption probability per voter (easy segments), mirroring the measured
// quality ordering of the local ensemble (7B best, 300M worst). 10% of segments are
// "hard" (all voters at 0.50 — high disagreement, recoverable) and 5% "vhard" (all
// voters at 0.85 — hopeless audio) so the benchmark exercises BOTH sides of the
// conformal frontier: hard segments certify at the 5%-average target, vhard segments
// must fall past the threshold and escalate.
const HARD_FRAC: f64 = 0.10;
const VHARD_FRAC: f64 = 0.05;
const EASY_RATES: [(&str, f64); 3] = [("wsl-7b", 0.03), ("ctc-1b", 0.12), ("omniasr-ctc-300m", 0.25)];
const HARD_RATE: f64 = 0.50;
const VHARD_RATE: f64 = 0.85;
const RAW_MODEL: &str = "omniasr-ctc-300m";

const FIXTURE_TEXT: &str = include_str!("fixtures/fleurs_ckb_sample.txt");

/// Same xorshift64 as eval.rs's paired bootstrap — deterministic, no external crate.
struct XorShift64(u64);
impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

fn word_bank() -> Vec<&'static str> {
    FIXTURE_TEXT.split_whitespace().collect()
}

fn char_pool(bank: &[&str]) -> Vec<char> {
    let mut pool: Vec<char> = bank.iter().flat_map(|w| w.chars()).filter(|c| c.is_alphabetic()).collect();
    pool.sort_unstable();
    pool.dedup();
    pool
}

/// Replace one seeded char of `word` with a different char from the fixture's own
/// alphabet — a 1-char substitution error, the shape of a real recognizer slip.
fn corrupt_word(word: &str, pool: &[char], rng: &mut XorShift64) -> String {
    let chars: Vec<char> = word.chars().collect();
    if chars.is_empty() || pool.len() < 2 {
        return word.to_string();
    }
    let idx = rng.below(chars.len());
    let original = chars[idx];
    let mut replacement = pool[rng.below(pool.len())];
    for _ in 0..8 {
        if replacement != original {
            break;
        }
        replacement = pool[rng.below(pool.len())];
    }
    let mut out: String = chars[..idx].iter().collect();
    out.push(replacement);
    out.extend(&chars[idx + 1..]);
    out
}

/// Corrupt each word of `reference` independently with probability `rate`.
/// Word count is preserved (substitution-only), keeping slot alignment honest.
fn corrupt_text(reference: &str, rate: f64, pool: &[char], rng: &mut XorShift64) -> String {
    reference
        .split_whitespace()
        .map(|w| if rng.next_f64() < rate { corrupt_word(w, pool, rng) } else { w.to_string() })
        .collect::<Vec<_>>()
        .join(" ")
}

struct BenchSegment {
    id: String,
    reference: String,
    /// (model_id, corrupted transcript) per voter.
    hyps: Vec<(String, String)>,
    is_calibration: bool,
    class: &'static str,
}

fn generate_corpus(rng: &mut XorShift64) -> Vec<BenchSegment> {
    let bank = word_bank();
    let pool = char_pool(&bank);
    let mut out = Vec::with_capacity(N_CAL + N_TEST);
    for i in 0..(N_CAL + N_TEST) {
        let is_calibration = i < N_CAL;
        let id = if is_calibration { format!("cal_{i:05}") } else { format!("tst_{:05}", i - N_CAL) };
        let n_words = WORDS_MIN + rng.below(WORDS_MAX - WORDS_MIN + 1);
        let reference = (0..n_words).map(|_| bank[rng.below(bank.len())].to_string()).collect::<Vec<_>>().join(" ");
        let class = {
            let r = rng.next_f64();
            if r < VHARD_FRAC {
                "vhard"
            } else if r < VHARD_FRAC + HARD_FRAC {
                "hard"
            } else {
                "easy"
            }
        };
        let hyps = EASY_RATES
            .iter()
            .map(|(model, easy_rate)| {
                let rate = match class {
                    "vhard" => VHARD_RATE,
                    "hard" => HARD_RATE,
                    _ => *easy_rate,
                };
                (model.to_string(), corrupt_text(&reference, rate, &pool, rng))
            })
            .collect();
        out.push(BenchSegment { id, reference, hyps, is_calibration, class });
    }
    out
}

fn insert_corpus(db: &Database, corpus: &[BenchSegment]) {
    let segments: Vec<SpeechSegment> = corpus
        .iter()
        .map(|b| {
            let raw = b.hyps.iter().find(|(m, _)| m == RAW_MODEL).map(|(_, t)| t.clone()).expect("raw voter present");
            SpeechSegment {
                id: b.id.clone(),
                audio_path: String::new(),
                raw_transcript: raw,
                annotated_transcript: b.is_calibration.then(|| b.reference.clone()),
                duration_ms: 2000,
                verified: b.is_calibration,
                confidence: Some(0.8),
                ctc_score: Some(-1.0),
                clipping_ratio: Some(0.0),
                snr_db: Some(20.0),
                ..SpeechSegment::default()
            }
        })
        .collect();
    db.insert_segments_batch(&segments).expect("insert segments");

    let conn = db.connection();
    conn.execute("SAVEPOINT bench_hyps", []).expect("savepoint");
    {
        let mut stmt = conn
            .prepare(
                "INSERT INTO segment_hypotheses (segment_id, model_id, transcript, confidence)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .expect("prepare");
        for b in corpus {
            for (model, transcript) in &b.hyps {
                stmt.execute(rusqlite::params![b.id, model, transcript, 0.8f64]).expect("insert hyp");
            }
        }
    }
    conn.execute("RELEASE SAVEPOINT bench_hyps", []).expect("release");
}

/// The model-evidence charter gate. Ignored in the default suite; run by scripts/verify_10.py as the
/// dedicated algorithm-only refinery-lift leg (no models, no network — pure offline machinery).
#[test]
#[ignore]
fn refinery_lift_injected_error_benchmark() {
    let tmp_db = NamedTempFile::new().expect("tmp db");
    let db = Database::open(tmp_db.path().to_str().expect("utf8 path")).expect("open db");
    db.initialize().expect("init db");
    let rolled_back = cortex_speech_app_lib::migrations::rollback(&db, 10).expect("rollback disposable benchmark DB");
    assert_eq!(
        rolled_back,
        vec![69, 68, 67, 66, 65, 64, 63, 62, 61, 60],
        "the algorithm benchmark must run at the exact last machine-verdict-compatible schema"
    );

    let mut rng = XorShift64::new(SEED);
    let corpus = generate_corpus(&mut rng);
    insert_corpus(&db, &corpus);

    let test_ids: Vec<String> = corpus.iter().filter(|b| !b.is_calibration).map(|b| b.id.clone()).collect();
    let report = jury::run_t0_gate(&db, &test_ids, &AutonLevel::ActConfirm, false).expect("run_t0_gate");

    // Map decisions back by id; escalated segments keep the RAW transcript (conservative:
    // the human budget is spent, the machine claims no accuracy for it).
    let mut jury_by_id = std::collections::HashMap::new();
    for d in &report.decisions {
        match d {
            T0Decision::AutoAccept { segment_id, consensus, .. } => {
                jury_by_id.insert(segment_id.clone(), Some(consensus.clone()));
            }
            T0Decision::EscalateToT1 { segment_id, .. } => {
                jury_by_id.insert(segment_id.clone(), None);
            }
        }
    }

    let triples: Vec<(String, String, String)> = corpus
        .iter()
        .filter(|b| !b.is_calibration)
        .map(|b| {
            let raw = b.hyps.iter().find(|(m, _)| m == RAW_MODEL).map(|(_, t)| t.clone()).expect("raw");
            let jury_text = jury_by_id.get(&b.id).cloned().flatten().unwrap_or_else(|| raw.clone());
            (b.reference.clone(), raw, jury_text)
        })
        .collect();
    assert_eq!(triples.len(), N_TEST, "every test segment must receive a decision");

    let lift = compute_label_quality_lift(&triples, 2000, SEED);
    assert!(lift.raw_micro_cer > 0.0, "injector produced no errors — benchmark is vacuous");
    let relative_reduction = lift.cer_lift / lift.raw_micro_cer;
    let escalation_fraction = report.escalated as f64 / report.total as f64;

    // Transparency (not gated): the same jury output scored against the STRONGEST voter
    // (the shipped-default raw_transcript producer). This surfaces how much of the raw-CTC
    // lift is arbitration vs simply trusting the best model — the charter gate is vs raw CTC.
    let anchor_triples: Vec<(String, String, String)> = corpus
        .iter()
        .filter(|b| !b.is_calibration)
        .map(|b| {
            let anchor = b.hyps.iter().find(|(m, _)| m == "wsl-7b").map(|(_, t)| t.clone()).expect("anchor");
            let raw = b.hyps.iter().find(|(m, _)| m == RAW_MODEL).map(|(_, t)| t.clone()).expect("raw");
            let jury_text = jury_by_id.get(&b.id).cloned().flatten().unwrap_or(raw);
            (b.reference.clone(), anchor, jury_text)
        })
        .collect();
    let anchor_lift = compute_label_quality_lift(&anchor_triples, 2000, SEED);

    println!("REFINERY-LIFT BENCH (seed {SEED:#x}, {N_CAL} calibration / {N_TEST} test)");
    println!("  raw_micro_cer      = {:.5}", lift.raw_micro_cer);
    println!("  jury_micro_cer     = {:.5}", lift.jury_micro_cer);
    println!("  cer_lift (abs)     = {:.5} [95% CI {:.5}, {:.5}]", lift.cer_lift, lift.lift_ci_low, lift.lift_ci_high);
    println!("  relative_reduction = {:.1}% (gate: >=30%)", relative_reduction * 100.0);
    println!(
        "  escalation         = {}/{} = {:.1}% (gate: <=15%)",
        report.escalated,
        report.total,
        escalation_fraction * 100.0
    );
    println!(
        "  [info, not gated] strongest-voter (wsl-7b) micro CER = {:.5}; jury vs strongest lift = {:+.5}",
        anchor_lift.raw_micro_cer, anchor_lift.cer_lift
    );

    assert!(
        relative_reduction >= 0.30,
        "refinery CER reduction {:.1}% below the 30% charter gate (raw {:.5} -> jury {:.5})",
        relative_reduction * 100.0,
        lift.raw_micro_cer,
        lift.jury_micro_cer
    );
    assert!(
        escalation_fraction <= 0.15,
        "escalation {:.1}% exceeds the 15% charter budget ({}/{})",
        escalation_fraction * 100.0,
        report.escalated,
        report.total
    );
}

/// Diagnostic companion (ignored): prints the real IRT-confidence / consensus-CER
/// distributions per difficulty class plus the conformal calibration outcome — run it
/// when the benchmark gate fails to see WHERE the refinery lost separation.
#[test]
#[ignore]
fn refinery_lift_diag_score_cer_distribution() {
    use cortex_speech_app_lib::db::SegmentHypothesis;
    use cortex_speech_app_lib::quality::{conformal, irt};
    use cortex_speech_app_lib::wer::compute_cer;

    let mut rng = XorShift64::new(SEED);
    let corpus = generate_corpus(&mut rng);
    let hyps: Vec<SegmentHypothesis> = corpus
        .iter()
        .flat_map(|b| {
            b.hyps.iter().map(|(m, t)| SegmentHypothesis {
                segment_id: b.id.clone(),
                model_id: m.clone(),
                transcript: t.clone(),
                confidence: Some(0.8),
            })
        })
        .collect();
    let res = irt::fit_irt_consensus(&hyps);
    println!("model abilities: {:?}", res.model_abilities);

    let mut cal_scored: Vec<(f64, f64)> = Vec::new();
    for class in ["easy", "hard", "vhard"] {
        let mut confs = Vec::new();
        let mut cers = Vec::new();
        for b in corpus.iter().filter(|b| b.is_calibration && b.class == class) {
            let conf = res.segment_confidences.get(&b.id).copied().unwrap_or(0.0);
            let cons = res.consensus_transcripts.get(&b.id).cloned().unwrap_or_default();
            let cer = compute_cer(&b.reference, &cons).min(1.0);
            confs.push(conf);
            cers.push(cer);
            cal_scored.push((conformal::nonconformity(conf, Some(-1.0)), cer));
        }
        if confs.is_empty() {
            println!("{class}: n=0 (class did not occur in the calibration draw)");
            continue;
        }
        confs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        cers.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let q = |v: &Vec<f64>, p: f64| v[((v.len() - 1) as f64 * p) as usize];
        println!(
            "{class}: n={} conf q10/50/90 = {:.3}/{:.3}/{:.3}  cer q10/50/90 = {:.4}/{:.4}/{:.4} mean_cer={:.4}",
            confs.len(),
            q(&confs, 0.1),
            q(&confs, 0.5),
            q(&confs, 0.9),
            q(&cers, 0.1),
            q(&cers, 0.5),
            q(&cers, 0.9),
            cers.iter().sum::<f64>() / cers.len() as f64
        );
    }
    let bucket_confidence = 1.0 - (1.0 - 0.90) / conformal::N_SNR_BUCKETS as f64;
    let (t, bound, is_cal) = conformal::calibrate_threshold(&cal_scored, 0.05, bucket_confidence);
    println!("calibrate_threshold: threshold={t:.4} bound={bound:.4} is_calibrated={is_cal}");
}

/// Fast injector self-check (default suite): deterministic, corruption-shaped, alignment-safe.
#[test]
fn injected_error_generator_is_deterministic_and_alignment_safe() {
    let bank = word_bank();
    assert!(bank.len() >= 10, "committed fixture transcript must supply the word bank");
    let pool = char_pool(&bank);
    assert!(pool.len() >= 2);

    let reference = bank[..8.min(bank.len())].join(" ");
    let a = corrupt_text(&reference, 1.0, &pool, &mut XorShift64::new(7));
    let b = corrupt_text(&reference, 1.0, &pool, &mut XorShift64::new(7));
    assert_eq!(a, b, "same seed must reproduce the same corruption");
    assert_ne!(a, reference, "rate 1.0 must actually corrupt");
    assert_eq!(
        a.split_whitespace().count(),
        reference.split_whitespace().count(),
        "substitution-only corruption must preserve word count"
    );
    let clean = corrupt_text(&reference, 0.0, &pool, &mut XorShift64::new(7));
    assert_eq!(clean, reference, "rate 0.0 must be the identity");

    let c1 = generate_corpus(&mut XorShift64::new(SEED));
    let c2 = generate_corpus(&mut XorShift64::new(SEED));
    assert_eq!(c1.len(), N_CAL + N_TEST);
    assert!(c1.iter().zip(&c2).all(|(x, y)| x.id == y.id && x.reference == y.reference && x.hyps == y.hyps));
}
