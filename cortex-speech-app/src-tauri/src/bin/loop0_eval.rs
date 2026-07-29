//! Does the LOOP-0 correction memory actually HELP? Measured, not assumed.
//!
//! `pipeline.rs` runs `apply_memories` over every transcript the app produces, so this layer silently
//! rewrites real data on the way to the corpus — and nothing measured whether those rewrites move CER
//! down, up, or not at all. An unmeasured correction layer is a liability, not an asset.
//!
//! Read-only. Opens the library, replays the firing rule over every human-verified clip, and scores
//! the raw ASR draft against the human gold text before and after firing.
//!
//! LEAVE-ONE-OUT IS NOT OPTIONAL. Every memory was extracted FROM a human correction on some segment,
//! so evaluating a memory against its own source segment is guaranteed to "fix" it — the same
//! self-confirmation the DB layer already warns about elsewhere. Memories whose `source_segment` is
//! the clip under test are excluded, so every number here is out-of-sample.
//!
//! Two passes are reported, and the difference between them matters:
//!   ARMED     — the production gates (confidence > tau_conf, hit_count >= min_hits). What fires today.
//!   PROJECTED — the same pool with the confidence/hit gates bypassed, phonetic gate kept. What the
//!               pool WOULD do if its memories were promoted. This is a projection, labelled as one:
//!               it is the answer to "is the pool any good yet", not a claim about current behaviour.
//!
//! Usage:  cargo run --bin loop0_eval [-- <path-to-cortex-speech.db>]
//! Falls back to CORTEX_APP_DATA_DIR, then %APPDATA%\cortex-speech\cortex-speech.db.

use cortex_speech_app_lib::corrections::{apply_memories, ContextMode, FiringConfig, MemoryEntry};
use cortex_speech_app_lib::db::SpeechSegment;
use cortex_speech_app_lib::wer::compute_cer;

/// One memory plus the segment it came from, which `load_correction_memories` does not carry.
struct SourcedMemory {
    entry: MemoryEntry,
    source_segment: Option<String>,
}

#[derive(Default)]
struct Tally {
    fired: usize,
    helped: usize,
    hurt: usize,
    neutral: usize,
    cer_before: f64,
    cer_after: f64,
}

impl Tally {
    fn report(&self, label: &str, scored: usize, cfg_note: &str) {
        println!("\n── {label} ─────────────────────────────────────────────");
        println!("   {cfg_note}");
        if scored == 0 {
            println!("   no scorable clips — nothing to report");
            return;
        }
        let before = self.cer_before / scored as f64;
        let after = self.cer_after / scored as f64;
        println!("   clips scored          : {scored}");
        println!("   clips a memory rewrote: {}", self.fired);
        println!("     of those, improved  : {}", self.helped);
        println!("     of those, worsened  : {}", self.hurt);
        println!("     of those, no change : {}", self.neutral);
        println!("   mean CER before firing: {:.4}", before);
        println!("   mean CER after  firing: {:.4}", after);
        let delta = after - before;
        let verdict = if self.fired == 0 {
            "INERT — nothing fired, so this layer is neither helping nor hurting"
        } else if delta < -1e-9 {
            "HELPS — firing lowers CER out-of-sample"
        } else if delta > 1e-9 {
            "HARMFUL — firing RAISES CER out-of-sample"
        } else {
            "NEUTRAL — firing changed text but not the error rate"
        };
        println!("   net delta             : {:+.4}  ({verdict})", delta);
    }
}

fn score(segments: &[(String, String, String)], memories: &[SourcedMemory], cfg: &FiringConfig) -> (Tally, usize) {
    let mut t = Tally::default();
    let mut scored = 0usize;
    for (id, raw, gold) in segments {
        // OUT-OF-SAMPLE: drop every memory this very clip produced.
        let usable: Vec<MemoryEntry> = memories
            .iter()
            .filter(|m| m.source_segment.as_deref() != Some(id.as_str()))
            .map(|m| m.entry.clone())
            .collect();
        let after = apply_memories(raw, &usable, cfg);
        // apply_memories re-joins on single spaces, so compare like with like: a pure whitespace
        // difference is not a correction and must not be scored as one.
        let before_norm = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        let cer_before = compute_cer(gold, &before_norm);
        let cer_after = compute_cer(gold, &after);
        t.cer_before += cer_before;
        t.cer_after += cer_after;
        scored += 1;
        if after != before_norm {
            t.fired += 1;
            if cer_after < cer_before {
                t.helped += 1;
            } else if cer_after > cer_before {
                t.hurt += 1;
            } else {
                t.neutral += 1;
            }
        }
    }
    (t, scored)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = std::env::args().nth(1).unwrap_or_else(|| {
        let dir = std::env::var("CORTEX_APP_DATA_DIR").unwrap_or_else(|_| {
            let appdata = std::env::var("APPDATA").unwrap_or_default();
            format!("{appdata}\\cortex-speech")
        });
        format!("{dir}\\cortex-speech.db")
    });
    println!("LOOP-0 firing evaluation (read-only)");
    println!("library: {db_path}");

    // Opened read-only on purpose: an evaluation must never be able to alter what it evaluates.
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;

    let memories: Vec<SourcedMemory> = conn
        .prepare(
            "SELECT wrong_token, human_token, slot_key, phonetic_key, confidence, hit_count, source_segment
             FROM correction_memory",
        )?
        .query_map([], |r| {
            Ok(SourcedMemory {
                entry: MemoryEntry {
                    wrong_token: r.get(0)?,
                    human_token: r.get(1)?,
                    slot_key: r.get(2)?,
                    phonetic_key: r.get(3)?,
                    confidence: r.get(4)?,
                    hit_count: r.get(5)?,
                },
                source_segment: r.get(6)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    // Scorable = a raw ASR draft to correct AND a human gold text to score against. `verified` alone
    // is not enough: a rejected clip has no usable gold.
    let segments: Vec<(String, String, String)> = conn
        .prepare(
            // Every column `quality::human_verified_text` consults must be selected, not just the
            // text ones: it decides whether a transcript counts as HUMAN gold from `verified`,
            // `is_gold`, `human_decision` and `verdict`. Leaving those at their defaults made the
            // helper reject all 26 usable clips and the whole evaluation silently scored nothing.
            "SELECT id, raw_transcript, annotated_transcript, verdict_transcript,
                    verified, is_gold, human_decision, verdict
             FROM speech_segments
             WHERE verified = 1
               AND COALESCE(raw_transcript,'') <> ''
               AND COALESCE(human_decision,'') <> 'reject'",
        )?
        .query_map([], |r| {
            let seg = SpeechSegment {
                id: r.get(0)?,
                raw_transcript: r.get(1)?,
                annotated_transcript: r.get(2)?,
                verdict_transcript: r.get(3)?,
                verified: r.get::<_, i64>(4)? != 0,
                is_gold: r.get::<_, i64>(5)? != 0,
                human_decision: r.get(6)?,
                verdict: r.get(7)?,
                ..SpeechSegment::default()
            };
            Ok(seg)
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|s| {
            // The SAME gold-text preference the app itself uses, rather than a second opinion here.
            let gold = cortex_speech_app_lib::quality::human_verified_text(&s)?.trim().to_string();
            if gold.is_empty() {
                return None;
            }
            Some((s.id.clone(), s.raw_transcript.clone(), gold))
        })
        .collect();

    let armed = FiringConfig::default();
    println!("\nmemories in pool : {}", memories.len());
    println!("gates            : confidence > {}, hit_count >= {}", armed.tau_conf, armed.min_hits);
    let eligible =
        memories.iter().filter(|m| m.entry.confidence > armed.tau_conf && m.entry.hit_count >= armed.min_hits).count();
    println!("memories past both gates: {eligible}");

    let (t, scored) = score(&segments, &memories, &armed);
    t.report("ARMED (production gates)", scored, "what actually fires today, out-of-sample");

    // Gates bypassed, phonetic gate KEPT — the structural match still has to hold, so this measures
    // the pool's quality rather than what unrestricted find-and-replace would do.
    let projected = FiringConfig {
        phon_tau: armed.phon_tau,
        tau_conf: f64::NEG_INFINITY,
        min_hits: 0,
        context: ContextMode::Exact,
    };
    let (tp, scored_p) = score(&segments, &memories, &projected);
    tp.report(
        "PROJECTED (confidence/hit gates bypassed — a projection, not current behaviour)",
        scored_p,
        "what this pool WOULD do if its memories were promoted past the gates",
    );

    // WHICH gate is actually blocking? The shipped rule requires BOTH neighbours to match exactly, so
    // a memory can only fire when the same bigram context recurs. These runs isolate that from the
    // confidence gates by loosening only the context requirement, driven through the REAL firing
    // function rather than a reimplementation of it. Precision matters more than recall here — a
    // rewrite that raises CER is worse than no rewrite — so read `worsened` before `improved`.
    for (mode, label) in [
        (ContextMode::Either, "CONTEXT: either neighbour (gates bypassed)"),
        (ContextMode::Ignore, "CONTEXT: neighbours ignored, phonetics only (gates bypassed)"),
    ] {
        let cfg = FiringConfig { phon_tau: armed.phon_tau, tau_conf: f64::NEG_INFINITY, min_hits: 0, context: mode };
        let (t, n) = score(&segments, &memories, &cfg);
        t.report(label, n, "diagnostic only — the shipped rule is unchanged");
    }

    println!("\nEvery number above is out-of-sample: memories are excluded from the clip they came from.");
    println!("Only the ARMED block describes what the app does today; the rest are diagnostics.");
    Ok(())
}
