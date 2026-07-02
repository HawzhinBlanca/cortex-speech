pub mod debate;
pub mod learning;
/// jury/mod.rs — Disagreement Refinery router (T0 gate)
///
/// Sits on top of the IRT Refinery.  For each segment it decides:
///   AutoAccept  — IRT consensus confidence ≥ calibrated threshold **and** text is fluent.
///   EscalateToT1 — disagreement detected; forward to the tool-using T1 judge.
///
/// The threshold is read from the most-recent conformal certificate.  If no
/// certificate exists (< 10 verified segments) the heuristic fallback (0.35)
/// is used — conservative by design.
pub mod t1_judge;
pub mod t2_listener;

use crate::db::{Database, SegmentHypothesis, SpeechSegment};
use crate::error::AppResult;
use crate::quality::conformal;
use crate::quality::irt;
use rusqlite::params;
use serde::{Deserialize, Serialize};

// ────────────────────────────────────────────────────────────────────────────
// Verdicts
// ────────────────────────────────────────────────────────────────────────────

/// Possible verdicts written to `speech_segments.verdict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    AutoAccept,
    JuryAccept,
    JuryEdit,
    Escalated,
    HumanAccept,
    HumanEdit,
    HumanReject,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Verdict::AutoAccept => "auto_accept",
            Verdict::JuryAccept => "jury_accept",
            Verdict::JuryEdit => "jury_edit",
            Verdict::Escalated => "escalated",
            Verdict::HumanAccept => "human_accept",
            Verdict::HumanEdit => "human_edit",
            Verdict::HumanReject => "human_reject",
        };
        write!(f, "{s}")
    }
}

// ────────────────────────────────────────────────────────────────────────────
// T0 gate decision
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum T0Decision {
    AutoAccept { segment_id: String, consensus: String, confidence: f64 },
    EscalateToT1 { segment_id: String, hypotheses: Vec<SegmentHypothesis>, disagreement_score: f64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct T0GateReport {
    pub total: usize,
    pub auto_accepted: usize,
    pub escalated: usize,
    pub decisions: Vec<T0Decision>,
}

/// Hard distrust vetoes that block an auto-accept no matter how high the IRT agreement is: poor audio
/// quality (low SNR / clipping) or a single distinct recognizer. With <2 distinct voters the IRT
/// "consensus" is a degenerate single-hypothesis prior, and a lone model's confidence is the most
/// dangerous routing signal (confidently wrong exactly on the rare/OOD Sorani tail). These are kept hard
/// even under ActAuto — committing such a segment at the agreement confidence would stamp a high
/// confidence on audio/consensus the gate explicitly distrusted. (NOTE: 300M and 1B are architecturally
/// KIN, so two-of-them agreement can still be a correlated confident error — adding an architecturally
/// INDEPENDENT recognizer's vote is the follow-up that fully closes that hole.)
fn has_hard_distrust_veto(seg: &SpeechSegment, hyps: &[SegmentHypothesis]) -> bool {
    let poor_quality =
        seg.snr_db.map(|snr| snr < 5.0).unwrap_or(false) || seg.clipping_ratio.map(|clip| clip > 0.1).unwrap_or(false);
    // Count only voters that actually CONTRIBUTED to the consensus. fit_irt_consensus drops
    // empty-transcript hypotheses before building the consensus + irt_confidence, so an empty "" from
    // one model (common when 300M and 1B disagree on whether a low-energy span contains speech) must
    // NOT count toward the two-recognizer guard — otherwise the surviving lone recognizer satisfies
    // distinct_voters >= 2 and the gate auto-accepts a single-model verdict, silently defeating the
    // "never auto-accept on a single recognizer" invariant this veto exists to enforce.
    let distinct_voters = {
        let mut ids: Vec<&str> =
            hyps.iter().filter(|h| !h.transcript.trim().is_empty()).map(|h| h.model_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    };
    poor_quality || distinct_voters < 2
}

/// Evaluate a single segment against the IRT consensus and conformal threshold.
pub fn t0_gate_segment(
    seg: &SpeechSegment,
    hyps: &[SegmentHypothesis],
    consensus: &str,
    irt_confidence: f64,
    threshold: f64,
) -> T0Decision {
    // Disagreement score: complement of IRT confidence.
    // Values closer to 1.0 = high disagreement.
    let disagreement_score = 1.0 - irt_confidence;

    // Same formula AND same confidence source the threshold was calibrated on (see run_t0_gate).
    let nonconformity_score = conformal::nonconformity(irt_confidence, seg.ctc_score);

    // The hard distrust vetoes (poor audio quality, single distinct recognizer) live in
    // has_hard_distrust_veto so the SAME guards are shared with apply_autonomy's ActAuto path. The
    // helper's distinct-voter count filters out empty-transcript hypotheses, matching fit_irt_consensus
    // (theirs' fix), so a lone recognizer can never satisfy the two-voter guard.
    if nonconformity_score <= threshold && !has_hard_distrust_veto(seg, hyps) {
        T0Decision::AutoAccept {
            segment_id: seg.id.clone(),
            consensus: consensus.to_string(),
            confidence: irt_confidence,
        }
    } else {
        T0Decision::EscalateToT1 { segment_id: seg.id.clone(), hypotheses: hyps.to_vec(), disagreement_score }
    }
}

/// Modulate a base T0 routing decision by the curator's Autonomy Dial. This is what makes the dial
/// REAL (it was previously read by no backend logic): the SAME segment routes differently per level.
///   - Observe / Propose: never auto-commit — a would-be AutoAccept is staged (EscalateToT1) for the
///     human. (Observe additionally writes NO verdict at all — handled in run_t0_gate.)
///   - ActConfirm (default): the base decision — auto-accept agreements, escalate the rest.
///   - ActAuto: fully unattended — a would-be EscalateToT1 is committed (AutoAccept).
pub fn apply_autonomy(
    decision: T0Decision,
    autonomy: &crate::settings::AutonLevel,
    seg: &SpeechSegment,
    consensus: &str,
    hypotheses: &[SegmentHypothesis],
    confidence: f64,
) -> T0Decision {
    use crate::settings::AutonLevel;
    match autonomy {
        AutonLevel::Observe | AutonLevel::Propose => match decision {
            T0Decision::AutoAccept { segment_id, .. } => T0Decision::EscalateToT1 {
                segment_id,
                hypotheses: hypotheses.to_vec(),
                disagreement_score: 1.0 - confidence,
            },
            escalate => escalate,
        },
        AutonLevel::ActConfirm => decision,
        AutonLevel::ActAuto => match decision {
            // ActAuto force-commits unattended — but the HARD distrust vetoes (poor audio quality, single
            // recognizer) keep escalating even here. Promoting such a segment would write Verdict::
            // AutoAccept with the IRT AGREEMENT confidence, which carries no information about the acoustic
            // veto that caused the escalation — stamping a high confidence on a segment the gate explicitly
            // distrusted. Only a borderline conformal-threshold escalation (no hard veto) is promoted.
            T0Decision::EscalateToT1 { segment_id, hypotheses: esc_hyps, disagreement_score } => {
                if has_hard_distrust_veto(seg, hypotheses) {
                    T0Decision::EscalateToT1 { segment_id, hypotheses: esc_hyps, disagreement_score }
                } else {
                    T0Decision::AutoAccept { segment_id, consensus: consensus.to_string(), confidence }
                }
            }
            accept => accept,
        },
    }
}

/// Batch-run the T0 gate over a list of segment IDs.
///
/// Steps:
///   1. Load all hypotheses for the requested segments.
///   2. Run IRT to get per-segment consensus + confidence.
///   3. Get the conformal threshold (from verified segments).
///   4. Route each segment to AutoAccept or EscalateToT1.
///   5. Write the verdict to the DB.
pub fn run_t0_gate(
    db: &Database,
    segment_ids: &[String],
    autonomy: &crate::settings::AutonLevel,
    learn_abilities: bool,
) -> AppResult<T0GateReport> {
    // 1. Load only the requested segments (not the full dataset).
    let all_segs = db.get_segments_by_ids(segment_ids)?;
    let target_segs: Vec<&SpeechSegment> = all_segs.iter().collect();

    let all_hyps = db.get_all_hypotheses()?;

    // We still need all verified segments to calibrate the conformal threshold.
    let all_verified = db.get_segments(Some(true))?;

    // 2. Run IRT over all hypotheses. When ability-learning is enabled (opt-in, F7), warm-start the
    //    consensus from the persisted per-model abilities and persist the freshly-fit ones so the jury
    //    learns each engine's real strength over time. Default OFF ⇒ empty priors ⇒ byte-identical to
    //    the hardcoded-heuristic path (no persistence).
    let irt_results = if learn_abilities {
        let priors = db.load_model_abilities().unwrap_or_else(|e| {
            tracing::warn!("failed to load persisted IRT model abilities; falling back to empty priors: {e}");
            std::collections::HashMap::new()
        });
        let r = irt::fit_irt_consensus_with_priors(&all_hyps, &priors);
        if r.abilities_were_fit {
            if let Err(e) = db.save_model_abilities(&r.model_abilities) {
                tracing::warn!("failed to persist IRT model abilities: {e}");
            }
        }
        r
    } else {
        irt::fit_irt_consensus(&all_hyps)
    };

    // 3. Calibrate the conformal threshold on the SAME IRT-based nonconformity score the gate uses
    //    below. Previously the threshold was calibrated on seg.confidence-based nonconformity while
    //    the gate compared it against irt_confidence-based nonconformity — a different score
    //    distribution under the same cutoff, which silently VOIDED the coverage guarantee.
    //    Calibrate PER SNR/condition bucket — a single global threshold is invalid across studio,
    //    field and noisy recordings.
    //
    //    Round-21 #2: a bucket with too little verified data to calibrate is NOT given the global
    //    (clean-dominated) threshold as a fallback. Borrowing a cutoff calibrated on a DIFFERENT
    //    condition advertises a per-condition coverage guarantee we cannot honor — a clean-calibrated
    //    threshold is meaningless for noisy audio (the score↔CER relationship differs by condition).
    //    Instead, an uncalibrated bucket is flagged, and every segment in it is fail-closed →
    //    escalated to human review (see the routing loop). And because calibrating up to
    //    N_SNR_BUCKETS separate thresholds inflates the family-wise miscoverage ~N×, each bucket's
    //    confidence level is Bonferroni-tightened so the JOINT per-condition guarantee across all
    //    conditions still holds at `T0_CONFIDENCE_LEVEL`.
    //
    // The conformal guarantee requires the threshold to be calibrated on the SAME nonconformity score
    // the gate compares against — including the SAME fallback when a segment has no IRT confidence (a
    // no-hypothesis row, or a blank-anchor segment skipped by fit_irt_consensus). Calibration (here) and
    // the gate (below) must use ONE shared default, or they place the same condition at two different
    // points of the score distribution and void coverage; previously calibration used 0.5 while the gate
    // used 0.0, scoring the same input 0.5 apart. 0.0 (⇒ maximal nonconformity ⇒ escalate) is the safe,
    // conservative default for a no-signal segment, used in both places.
    const MISSING_IRT_CONFIDENCE: f64 = 0.0;
    const T0_TARGET_ERROR: f64 = 0.05;
    const T0_CONFIDENCE_LEVEL: f64 = 0.90;
    // Bonferroni split of the miscoverage budget across the per-condition buckets.
    let bucket_confidence = 1.0 - (1.0 - T0_CONFIDENCE_LEVEL) / conformal::N_SNR_BUCKETS as f64;
    let mut bucket_scored: [Vec<(f64, f64)>; conformal::N_SNR_BUCKETS] = std::array::from_fn(|_| Vec::new());
    for s in &all_verified {
        let Some(ref_text) = s.annotated_transcript.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
            continue;
        };
        let irt_conf = irt_results.segment_confidences.get(&s.id).copied().unwrap_or(MISSING_IRT_CONFIDENCE);
        let score = conformal::nonconformity(irt_conf, s.ctc_score);
        // Calibrate against the text the gate actually COMMITS on auto-accept — the IRT consensus
        // (falling back to raw_transcript when there's no consensus) — exactly as t0_gate_segment /
        // run_t0_gate select it. Certifying raw_transcript's CER while committing the consensus would
        // make the conformal coverage guarantee cover a DIFFERENT quantity than the one published, void
        // on every segment where consensus != raw (the disagreement case the jury exists to resolve).
        let committed =
            irt_results.consensus_transcripts.get(&s.id).map(String::as_str).unwrap_or(s.raw_transcript.as_str());
        let cer = crate::wer::compute_cer(ref_text, committed).min(1.0);
        bucket_scored[conformal::snr_bucket(s.snr_db)].push((score, cer));
    }
    let mut bucket_calibrated = [false; conformal::N_SNR_BUCKETS];
    let bucket_thresholds: [f64; conformal::N_SNR_BUCKETS] = std::array::from_fn(|b| {
        let (t, _bound, is_cal) = conformal::calibrate_threshold(&bucket_scored[b], T0_TARGET_ERROR, bucket_confidence);
        bucket_calibrated[b] = is_cal;
        t
    });

    // 4. Route and collect decisions
    let mut decisions = Vec::new();
    let mut auto_accepted = 0usize;
    let mut escalated = 0usize;

    for seg in &target_segs {
        // Skip if already decided (has a non-empty verdict)
        if seg.verdict.as_ref().map(|v| !v.is_empty()).unwrap_or(false) {
            continue;
        }

        let seg_hyps: Vec<SegmentHypothesis> = all_hyps.iter().filter(|h| h.segment_id == seg.id).cloned().collect();

        let consensus =
            irt_results.consensus_transcripts.get(&seg.id).cloned().unwrap_or_else(|| seg.raw_transcript.clone());

        let irt_confidence = irt_results.segment_confidences.get(&seg.id).copied().unwrap_or(MISSING_IRT_CONFIDENCE);

        // Gate this segment against its OWN acoustic-condition bucket — but only if that bucket is
        // conformally calibrated. Round-21 #2: an uncalibrated bucket carries no coverage guarantee,
        // so fail closed (escalate) rather than borrow a threshold from a different, clean-dominated
        // condition. (Under ActAuto the gate is bypassed downstream anyway; this hardens the default
        // ActConfirm path, where the threshold actually has teeth.)
        let bucket = conformal::snr_bucket(seg.snr_db);
        let base_decision = if bucket_calibrated[bucket] {
            t0_gate_segment(seg, &seg_hyps, &consensus, irt_confidence, bucket_thresholds[bucket])
        } else {
            T0Decision::EscalateToT1 {
                segment_id: seg.id.clone(),
                hypotheses: seg_hyps.clone(),
                disagreement_score: 1.0 - irt_confidence,
            }
        };
        // The Autonomy Dial decides whether the gate may auto-commit.
        let decision = apply_autonomy(base_decision, autonomy, seg, &consensus, &seg_hyps, irt_confidence);

        // 5. Write verdict to DB
        match &decision {
            T0Decision::AutoAccept { consensus, confidence, .. } => {
                write_verdict(db, &seg.id, Verdict::AutoAccept, Some(consensus), None, None, Some(*confidence))?;
                auto_accepted += 1;
            }
            T0Decision::EscalateToT1 { .. } => {
                // Observe is pure observation: it stages nothing and commits no verdict.
                if !matches!(autonomy, crate::settings::AutonLevel::Observe) {
                    write_verdict(db, &seg.id, Verdict::Escalated, None, None, None, None)?;
                }
                escalated += 1;
            }
        }

        decisions.push(decision);
    }

    Ok(T0GateReport { total: target_segs.len(), auto_accepted, escalated, decisions })
}

// ────────────────────────────────────────────────────────────────────────────
// Verdict helpers (shared by T1, T2, learning)
// ────────────────────────────────────────────────────────────────────────────

pub fn write_verdict(
    db: &Database,
    segment_id: &str,
    verdict: Verdict,
    transcript: Option<&str>,
    rationale: Option<&str>,
    evidence_json: Option<&str>,
    agent_confidence: Option<f64>,
) -> AppResult<()> {
    // Never let this MACHINE verdict overwrite a HUMAN decision. The T0/T1/T2 jury runs on a SEPARATE
    // WAL connection from the human path (record_human_decision), snapshots its segments once, then can
    // lag a multi-second T2 cloud call — so a curator may accept/edit the same segment mid-run, and the
    // in-memory "already has a verdict" skip reads the STALE snapshot. The same guard as db::
    // write_segment_verdict and the consensus/ASR write paths makes a verdict for an already-human-decided
    // segment a 0-row no-op, keeping the human's verdict/gold transcript authoritative.
    let affected = db.connection().execute(
        "UPDATE speech_segments
         SET verdict           = ?2,
             verdict_transcript = ?3,
             rationale         = ?4,
             evidence_json     = ?5,
             agent_confidence  = ?6,
             escalated         = ?7,
             updated_at        = datetime('now')
         WHERE id = ?1
           AND (human_decision IS NULL OR human_decision = '')
           AND (verdict IS NULL OR verdict NOT IN ('human_accept', 'human_edit', 'human_reject'))",
        params![
            segment_id,
            verdict.to_string(),
            transcript,
            rationale,
            evidence_json,
            agent_confidence,
            (verdict == Verdict::Escalated) as i32,
        ],
    )?;

    // Flywheel capture: when the jury ACCEPTS a transcript that differs from the raw ASR, the model
    // corrected OmniASR — record it as a provenance-tagged PSEUDO example (never auto-trained; gated
    // behind human review). Best-effort: a capture failure must not fail the verdict write. Gated on
    // `affected > 0` so we never capture a "model correction" for a verdict the guard above did NOT
    // write (the segment was human-decided) — that would tag the human's row with a phantom pseudo-label.
    if affected > 0 && matches!(verdict, Verdict::AutoAccept | Verdict::JuryAccept) {
        if let Some(corrected) = transcript {
            let raw: Option<String> = db
                .connection()
                .query_row("SELECT raw_transcript FROM speech_segments WHERE id = ?1", params![segment_id], |r| {
                    r.get(0)
                })
                .ok();
            if let Some(raw) = raw {
                if raw.trim() != corrected.trim() {
                    let _ = db.record_model_correction(segment_id, &raw, corrected, "jury");
                }
            }
        }
    }
    Ok(())
}

/// Record the final human decision and write to `agent_examples` for few-shot
/// memory (skipped if the segment is gold-holdout). M2.1: Accepts optional timestamp_ms for decision timing.
pub fn record_human_decision(
    db: &Database,
    segment_id: &str,
    decision: &str, // "accept" | "edit" | "reject"
    corrected_transcript: Option<&str>,
    timestamp_ms: Option<i64>,
) -> AppResult<()> {
    db.record_human_decision(segment_id, decision, corrected_transcript, timestamp_ms)
}

/// The normalized word set of a transcript, for lexical-relevance scoring. Uses the char-only
/// normalizer so orthographic variants (Kaf/Yeh/Heh) count as the same word.
fn relevance_token_set(text: &str) -> std::collections::HashSet<String> {
    let normalizer = crate::normalizer::SoraniNormalizer::with_config(crate::normalizer::NormalizationConfig {
        normalize_numbers: false,
        verbalize_numbers: false,
        normalize_hamza: true,
        remove_diacritics: false,
    });
    normalizer.normalize(text).split_whitespace().map(|w| w.to_string()).collect()
}

/// Jaccard similarity between two word sets (|∩| / |∪|), 0.0 when both are empty.
fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / union as f64
}

/// Retrieve k few-shot examples for a segment, ranked by lexical relevance to that segment's text
/// (not mere recency), so the LLM is primed on ON-TOPIC corrections. Falls back to recency when the
/// segment has no text. A bounded recent pool is re-ranked, and ties keep recency order (the SQL
/// already returns newest-first and the sort is stable). The doc's §2.4 upgrade over the old
/// recency-only retrieval that ignored its segment_id.
pub fn get_few_shot_examples(db: &Database, segment_id: &str, k: usize) -> AppResult<Vec<FewShotExample>> {
    // The current segment's canonical text (prefer normalized), if present.
    let segment_text: Option<String> = db
        .connection()
        .query_row(
            "SELECT COALESCE(NULLIF(normalized_transcript, ''), raw_transcript)
             FROM speech_segments WHERE id = ?1",
            params![segment_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();

    // Re-rank a bounded recent pool (keeps cost flat as the example store grows).
    let pool = 200usize.max(k);
    let mut stmt = db.connection().prepare(
        // Only human-verified examples seed the LLM corrector's few-shot context; model pseudo-labels
        // (verified_by_human=0) are captured but never used as exemplars until a human signs off.
        //
        // HOLDOUT EXCLUSION: never inject a held-out gold clip's correction into the live judge — its
        // human_fix IS the held-out reference text, so showing it to the T2 cloud judge contaminates the
        // benchmark and inflates measured WER/CER. The DPO/LM exports already gate on this; the few-shot
        // path must too. A clip promoted to holdout keeps its audio_path, so excluding examples whose
        // segment audio_path is a holdout gold path (plus the defensive is_gold=0) closes the leak in
        // SQL without re-hashing every candidate in the jury hot loop.
        "SELECT ae.id, ae.segment_id, ae.wrong_transcript, ae.human_fix, ae.created_at
         FROM agent_examples ae
         JOIN speech_segments ss ON ae.segment_id = ss.id
         WHERE ae.verified_by_human = 1
           AND ss.is_gold = 0
           AND ss.audio_path NOT IN (SELECT audio_path FROM gold_segments WHERE is_holdout = 1)
         ORDER BY ae.created_at DESC, ae.id ASC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![pool as i64], |row| {
        Ok(FewShotExample {
            id: row.get(0)?,
            segment_id: row.get(1)?,
            wrong_transcript: row.get(2)?,
            human_fix: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    let mut examples: Vec<FewShotExample> = rows.collect::<Result<Vec<_>, _>>()?;

    if let Some(text) = segment_text.filter(|t| !t.trim().is_empty()) {
        let query = relevance_token_set(&text);
        // An example is relevant if either its ASR-side or its corrected text overlaps the segment.
        let score = |ex: &FewShotExample| {
            jaccard(&query, &relevance_token_set(&ex.wrong_transcript))
                .max(jaccard(&query, &relevance_token_set(&ex.human_fix)))
        };
        // Stable sort by descending relevance; equal scores retain the newest-first SQL order.
        examples.sort_by(|a, b| score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal));
    }

    examples.truncate(k);
    Ok(examples)
}

/// Return escalated segments ordered by descending disagreement (riskiest first).
pub fn get_escalation_queue(db: &Database, limit: usize) -> AppResult<Vec<EscalatedItem>> {
    let mut stmt = db.connection().prepare(
        "SELECT id, raw_transcript, normalized_transcript, audio_path,
                confidence, agent_confidence, rationale, evidence_json
         FROM speech_segments
         WHERE escalated = 1 AND (human_decision IS NULL OR human_decision = '')
         ORDER BY COALESCE(agent_confidence, 0.5) ASC, id ASC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(EscalatedItem {
            id: row.get(0)?,
            raw_transcript: row.get(1)?,
            normalized_transcript: row.get(2)?,
            audio_path: row.get(3)?,
            asr_confidence: row.get(4)?,
            agent_confidence: row.get(5)?,
            rationale: row.get(6)?,
            evidence_json: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Return a time-series of (date, escalation_rate) for the dashboard.
pub fn get_escalation_rate_trend(db: &Database) -> AppResult<Vec<EscalationTrendPoint>> {
    // Select the 30 MOST-RECENT activity days (inner DESC + LIMIT), then present them oldest→newest
    // (outer ASC) for the chart. A plain `ORDER BY day ASC LIMIT 30` keeps the EARLIEST 30 days, so the
    // trend would freeze on the first month of history once a project runs the jury on >30 days.
    let mut stmt = db.connection().prepare(
        "SELECT day, total, esc FROM (
             SELECT date(updated_at) as day,
                    COUNT(*) as total,
                    SUM(escalated) as esc
             FROM speech_segments
             WHERE updated_at IS NOT NULL AND verdict IS NOT NULL
             GROUP BY day
             ORDER BY day DESC
             LIMIT 30
         )
         ORDER BY day ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let total: i64 = row.get(1)?;
        let esc: i64 = row.get(2)?;
        Ok(EscalationTrendPoint {
            date: row.get(0)?,
            escalation_rate: if total > 0 { esc as f64 / total as f64 } else { 0.0 },
            total,
            escalated: esc,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ────────────────────────────────────────────────────────────────────────────
// Shared response types
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FewShotExample {
    pub id: String,
    pub segment_id: String,
    pub wrong_transcript: String,
    pub human_fix: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalatedItem {
    pub id: String,
    pub raw_transcript: String,
    pub normalized_transcript: Option<String>,
    pub audio_path: String,
    pub asr_confidence: Option<f64>,
    pub agent_confidence: Option<f64>,
    pub rationale: Option<String>,
    pub evidence_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationTrendPoint {
    pub date: String,
    pub escalation_rate: f64,
    pub total: i64,
    pub escalated: i64,
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SegmentHypothesis;

    fn make_hyp(seg_id: &str, model: &str, text: &str) -> SegmentHypothesis {
        SegmentHypothesis {
            segment_id: seg_id.into(),
            model_id: model.into(),
            transcript: text.into(),
            confidence: Some(0.9),
        }
    }

    fn make_seg(id: &str, transcript: &str) -> SpeechSegment {
        SpeechSegment {
            id: id.into(),
            created_at: None,
            audio_path: "/tmp/test.wav".into(),
            raw_transcript: transcript.into(),
            normalized_transcript: None,
            annotated_transcript: None,
            alignment_json: None,
            duration_ms: 3000,
            speaker_id: None,
            verified: false,
            confidence: Some(0.8),
            ctc_score: Some(-2.0),
            clipping_ratio: None,
            rms_db: None,
            snr_db: None,
            split: None,
            ood_score: None,
            verdict: None,
            verdict_transcript: None,
            rationale: None,
            evidence_json: None,
            agent_confidence: None,
            escalated: false,
            human_decision: None,
            corrected_at: None,
            is_gold: false,
            alignment_quality: None,
        }
    }

    #[test]
    fn test_t0_gate_auto_accept() {
        let seg = make_seg("s1", "کوردستان");
        let hyps = vec![make_hyp("s1", "gemini", "کوردستان"), make_hyp("s1", "asr-1b", "کوردستان")];
        // High confidence → auto-accept
        let decision = t0_gate_segment(&seg, &hyps, "کوردستان", 0.95, 0.60);
        assert!(matches!(decision, T0Decision::AutoAccept { .. }));
    }

    #[test]
    fn test_t0_gate_escalate() {
        let seg = make_seg("s1", "کوردستان");
        let hyps = vec![make_hyp("s1", "gemini", "کوردستان"), make_hyp("s1", "asr-1b", "ئێران")];
        // Low confidence → escalate
        let decision = t0_gate_segment(&seg, &hyps, "کوردستان", 0.45, 0.60);
        assert!(matches!(decision, T0Decision::EscalateToT1 { .. }));
    }

    #[test]
    fn t0_gate_escalates_a_single_recognizer_even_at_high_confidence() {
        // Only one model produced a hypothesis: the IRT "consensus" is a degenerate single-hypothesis
        // prior, and a lone model's confidence is the dangerous routing signal. Must escalate.
        let seg = make_seg("s1", "کوردستان");
        let one = vec![make_hyp("s1", "omniasr-ctc-300m", "کوردستان")];
        assert!(
            matches!(t0_gate_segment(&seg, &one, "کوردستان", 0.99, 0.60), T0Decision::EscalateToT1 { .. }),
            "a single recognizer must escalate even at perfect confidence"
        );
        // Two distinct recognizers at the same confidence/threshold DO auto-accept.
        let two = vec![make_hyp("s1", "omniasr-ctc-300m", "کوردستان"), make_hyp("s1", "omniasr-ctc-1b", "کوردستان")];
        assert!(matches!(t0_gate_segment(&seg, &two, "کوردستان", 0.99, 0.60), T0Decision::AutoAccept { .. }));
    }

    #[test]
    fn t0_gate_escalates_when_the_second_voter_returned_an_empty_transcript() {
        // Two model_ids are present but ONE returned "" (no speech detected — common when 300M and 1B
        // disagree on a low-energy span). IRT drops the empty hypothesis and derives its consensus +
        // confidence from the single surviving recognizer, so the empty one must NOT count toward the
        // two-distinct-voters guard. Even at perfect confidence this must escalate, not auto-accept a
        // lone-model verdict.
        let seg = make_seg("s1", "کوردستان");
        let hyps = vec![make_hyp("s1", "omniasr-ctc-1b", "کوردستان"), make_hyp("s1", "omniasr-ctc-300m", "")];
        assert!(
            matches!(t0_gate_segment(&seg, &hyps, "کوردستان", 0.99, 0.60), T0Decision::EscalateToT1 { .. }),
            "an empty-transcript hypothesis must not count as a second voter"
        );
    }

    #[test]
    fn few_shot_examples_rank_by_relevance_not_just_recency() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        // Example-owning segments (FK) + the query segment whose text matches the relevant example.
        for (id, text) in [("e-rel", "x"), ("e-old1", "x"), ("e-old2", "x"), ("seg-q", "ساڵی نوێ پیرۆز بێت")]
        {
            db.insert_segment(&make_seg(id, text)).unwrap();
        }
        // The RELEVANT example is the OLDEST; two irrelevant examples are newer. Recency alone would
        // surface the newer ones.
        let conn = db.connection();
        conn.execute(
            "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix, created_at)
             VALUES ('a-rel', 'e-rel', 'ساڵی نوێ پیرۆز', 'ساڵی نوێ پیرۆز بێت', '2020-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix, created_at)
             VALUES ('a-new1', 'e-old1', 'کتێبی مێژوو', 'کتێبی مێژووی کورد', '2025-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix, created_at)
             VALUES ('a-new2', 'e-old2', 'ئاو هەوا', 'ئاو و هەوا', '2025-06-01')",
            [],
        )
        .unwrap();

        let top = get_few_shot_examples(&db, "seg-q", 1).unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].id, "a-rel", "the lexically relevant example must outrank the more recent ones");

        // A segment with no text falls back to recency (newest first).
        db.insert_segment(&make_seg("seg-empty", "")).unwrap();
        let recency = get_few_shot_examples(&db, "seg-empty", 1).unwrap();
        assert_eq!(recency[0].id, "a-new2", "no segment text -> recency fallback returns the newest");
    }

    #[test]
    fn few_shot_excludes_holdout_gold_corrections() {
        // Round-19 holdout-leak: a correction whose segment audio was promoted to a HOLDOUT gold clip
        // must NOT be served as a few-shot exemplar — its human_fix IS the held-out reference text, so
        // injecting it into the live T2 judge contaminates the benchmark and inflates measured WER/CER.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        let mut held = make_seg("seg-held", "x");
        held.audio_path = "/data/holdout.wav".to_string();
        db.insert_segment(&held).unwrap();
        let mut ok = make_seg("seg-ok", "x");
        ok.audio_path = "/data/train.wav".to_string();
        db.insert_segment(&ok).unwrap();
        db.insert_segment(&make_seg("seg-q", "کوردی باشە")).unwrap();

        let conn = db.connection();
        conn.execute(
            "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix, created_at)
             VALUES ('a-held', 'seg-held', 'کوردی', 'کوردی باشە', '2024-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix, created_at)
             VALUES ('a-ok', 'seg-ok', 'کوردی', 'کوردی باشە', '2024-01-02')",
            [],
        )
        .unwrap();
        // Promote the first clip to a HOLDOUT gold reference (same audio_path).
        conn.execute(
            "INSERT INTO gold_segments (id, audio_path, reference, is_holdout)
             VALUES ('g1', '/data/holdout.wav', 'کوردی باشە', 1)",
            [],
        )
        .unwrap();

        let ids: Vec<String> = get_few_shot_examples(&db, "seg-q", 10).unwrap().into_iter().map(|e| e.id).collect();
        assert!(
            !ids.iter().any(|id| id == "a-held"),
            "a holdout gold clip's correction must NOT be served as a few-shot example: {ids:?}"
        );
        assert!(ids.iter().any(|id| id == "a-ok"), "a non-holdout correction is still available");
    }

    #[test]
    fn apply_autonomy_changes_routing_per_dial() {
        use crate::settings::AutonLevel;
        // Good-quality segment (snr/clipping None -> not poor) with TWO distinct recognizers -> no hard veto.
        let seg = make_seg("s1", "x");
        let hyps = vec![make_hyp("s1", "m1", "x"), make_hyp("s1", "m2", "x")];
        let accept = || T0Decision::AutoAccept { segment_id: "s1".into(), consensus: "x".into(), confidence: 0.9 };
        let escalate =
            || T0Decision::EscalateToT1 { segment_id: "s1".into(), hypotheses: hyps.clone(), disagreement_score: 0.4 };

        // ActConfirm (default): base decisions pass through unchanged.
        assert!(matches!(
            apply_autonomy(accept(), &AutonLevel::ActConfirm, &seg, "x", &hyps, 0.9),
            T0Decision::AutoAccept { .. }
        ));
        assert!(matches!(
            apply_autonomy(escalate(), &AutonLevel::ActConfirm, &seg, "x", &hyps, 0.4),
            T0Decision::EscalateToT1 { .. }
        ));

        // Propose / Observe: a confident accept is STAGED for the human instead of auto-committed.
        assert!(matches!(
            apply_autonomy(accept(), &AutonLevel::Propose, &seg, "x", &hyps, 0.9),
            T0Decision::EscalateToT1 { .. }
        ));
        assert!(matches!(
            apply_autonomy(accept(), &AutonLevel::Observe, &seg, "x", &hyps, 0.9),
            T0Decision::EscalateToT1 { .. }
        ));

        // ActAuto: a borderline conformal escalate WITHOUT a hard veto is committed unattended.
        assert!(matches!(
            apply_autonomy(escalate(), &AutonLevel::ActAuto, &seg, "x", &hyps, 0.4),
            T0Decision::AutoAccept { .. }
        ));

        // ActAuto must NOT override the hard distrust vetoes: a single recognizer stays escalated...
        let one_hyp = vec![make_hyp("s1", "only", "x")];
        assert!(
            matches!(
                apply_autonomy(escalate(), &AutonLevel::ActAuto, &seg, "x", &one_hyp, 0.4),
                T0Decision::EscalateToT1 { .. }
            ),
            "single-voter segment must stay escalated even under ActAuto"
        );
        // ...and so does poor audio quality.
        let mut noisy = make_seg("s1", "x");
        noisy.snr_db = Some(2.0);
        assert!(
            matches!(
                apply_autonomy(escalate(), &AutonLevel::ActAuto, &noisy, "x", &hyps, 0.4),
                T0Decision::EscalateToT1 { .. }
            ),
            "poor-audio segment must stay escalated even under ActAuto"
        );
    }

    #[test]
    fn run_t0_gate_observe_writes_no_verdict_unlike_actconfirm() {
        use crate::settings::AutonLevel;
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&make_seg("s-dial", "کوردستان")).unwrap();
        // Two disagreeing hypotheses -> the base gate escalates this segment.
        for (m, t) in [("gemini", "کوردستان"), ("asr-1b", "ئێران")] {
            db.insert_hypothesis(&SegmentHypothesis {
                segment_id: "s-dial".into(),
                model_id: m.into(),
                transcript: t.into(),
                confidence: Some(0.8),
            })
            .unwrap();
        }

        // Observe: pure observation — the report counts the segment but NO verdict is committed.
        let obs = run_t0_gate(&db, &["s-dial".to_string()], &AutonLevel::Observe, false).unwrap();
        assert_eq!(obs.total, 1);
        assert!(
            db.get_segment_by_id("s-dial").unwrap().unwrap().verdict.as_deref().unwrap_or("").is_empty(),
            "Observe must commit no verdict"
        );

        // ActConfirm: the SAME segment now gets its escalated verdict written — the dial changed backend behavior.
        let act = run_t0_gate(&db, &["s-dial".to_string()], &AutonLevel::ActConfirm, false).unwrap();
        assert_eq!(act.escalated, 1);
        assert_eq!(db.get_segment_by_id("s-dial").unwrap().unwrap().verdict.as_deref(), Some("escalated"));
    }

    #[test]
    fn write_verdict_never_overwrites_a_human_decision() {
        // The T0/T1/T2 jury (write_verdict, on a separate connection) can land AFTER a curator decided the
        // same segment mid-run. The human is authoritative: the machine write must be a no-op — and the
        // flywheel must NOT capture a model pseudo-correction for a verdict it did not actually write.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&make_seg("s-hv", "raw text")).unwrap();
        db.record_human_decision("s-hv", "accept", None, None).unwrap();

        write_verdict(&db, "s-hv", Verdict::AutoAccept, Some("machine consensus"), None, None, Some(0.9)).unwrap();

        let seg = db.get_segment_by_id("s-hv").unwrap().unwrap();
        assert_eq!(seg.verdict.as_deref(), Some("human_accept"), "T0 write_verdict clobbered the human decision");
        assert_eq!(seg.human_decision.as_deref(), Some("accept"), "human_decision must be preserved");
        assert!(!seg.escalated, "a human-accepted segment must not be re-escalated by a late machine write");

        let captured: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM agent_examples WHERE segment_id = 's-hv'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(captured, 0, "no model-correction example may be captured when the verdict write no-ops");
    }

    #[test]
    fn run_t0_gate_fails_closed_when_no_snr_bucket_is_calibrated() {
        // Round-21 #2: a clean, high-confidence, TWO-recognizer segment (snr_db None ⇒ not poor-quality)
        // that would auto-accept against a borrowed cold-start cutoff. With too little verified data to
        // calibrate ANY SNR bucket, the gate must fail closed (escalate) rather than auto-accept against
        // an uncalibrated/clean-dominated threshold borrowed from another condition.
        use crate::settings::AutonLevel;
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db.insert_segment(&make_seg("s-uncal", "کوردستان")).unwrap(); // snr_db None → unknown bucket (4)
        for (m, t) in [("omniasr-ctc-300m", "کوردستان"), ("omniasr-ctc-1b", "کوردستان")] {
            db.insert_hypothesis(&SegmentHypothesis {
                segment_id: "s-uncal".into(),
                model_id: m.into(),
                transcript: t.into(),
                confidence: Some(0.95),
            })
            .unwrap();
        }
        let report = run_t0_gate(&db, &["s-uncal".to_string()], &AutonLevel::ActConfirm, false).unwrap();
        assert_eq!(report.auto_accepted, 0, "no calibrated bucket → nothing may auto-accept");
        assert_eq!(report.escalated, 1, "the segment fails closed to human review");
        assert!(matches!(report.decisions[0], T0Decision::EscalateToT1 { .. }));
    }
}
