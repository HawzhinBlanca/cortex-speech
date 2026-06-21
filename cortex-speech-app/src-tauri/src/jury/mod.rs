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

    let poor_quality = if let Some(snr) = seg.snr_db { snr < 5.0 } else { false }
        || if let Some(clip) = seg.clipping_ratio { clip > 0.1 } else { false };

    if nonconformity_score <= threshold && !poor_quality {
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
            T0Decision::EscalateToT1 { segment_id, .. } => {
                T0Decision::AutoAccept { segment_id, consensus: consensus.to_string(), confidence }
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
) -> AppResult<T0GateReport> {
    // 1. Load only the requested segments (not the full dataset).
    let all_segs = db.get_segments_by_ids(segment_ids)?;
    let target_segs: Vec<&SpeechSegment> = all_segs.iter().collect();

    let all_hyps = db.get_all_hypotheses()?;

    // We still need all verified segments to calibrate the conformal threshold.
    let all_verified = db.get_segments(Some(true))?;

    // 2. Run IRT over all hypotheses
    let irt_results = irt::fit_irt_consensus(&all_hyps);

    // 3. Calibrate the conformal threshold on the SAME IRT-based nonconformity score the gate uses
    //    below. Previously the threshold was calibrated on seg.confidence-based nonconformity while
    //    the gate compared it against irt_confidence-based nonconformity — a different score
    //    distribution under the same cutoff, which silently VOIDED the coverage guarantee.
    let cal_scored: Vec<(f64, f64)> = all_verified
        .iter()
        .filter_map(|s| {
            let ref_text = s.annotated_transcript.as_deref()?.trim();
            if ref_text.is_empty() {
                return None;
            }
            let irt_conf = irt_results.segment_confidences.get(&s.id).copied().unwrap_or(0.5);
            let score = conformal::nonconformity(irt_conf, s.ctc_score);
            let cer = crate::wer::compute_cer(ref_text, &s.raw_transcript).min(1.0);
            Some((score, cer))
        })
        .collect();
    let (threshold, _bound, _is_calibrated) = conformal::calibrate_threshold(&cal_scored, 0.05, 0.90);

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

        let irt_confidence = irt_results.segment_confidences.get(&seg.id).copied().unwrap_or(0.0);

        let base_decision = t0_gate_segment(seg, &seg_hyps, &consensus, irt_confidence, threshold);
        // The Autonomy Dial decides whether the gate may auto-commit.
        let decision = apply_autonomy(base_decision, autonomy, &consensus, &seg_hyps, irt_confidence);

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
    db.connection().execute(
        "UPDATE speech_segments
         SET verdict           = ?2,
             verdict_transcript = ?3,
             rationale         = ?4,
             evidence_json     = ?5,
             agent_confidence  = ?6,
             escalated         = ?7,
             updated_at        = datetime('now')
         WHERE id = ?1",
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
    Ok(())
}

/// Record the final human decision and write to `agent_examples` for few-shot
/// memory (skipped if the segment is gold-holdout).
pub fn record_human_decision(
    db: &Database,
    segment_id: &str,
    decision: &str, // "accept" | "edit" | "reject"
    corrected_transcript: Option<&str>,
) -> AppResult<()> {
    db.record_human_decision(segment_id, decision, corrected_transcript)
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
        "SELECT id, segment_id, wrong_transcript, human_fix, created_at
         FROM agent_examples
         ORDER BY created_at DESC
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
    let mut stmt = db.connection().prepare(
        "SELECT date(updated_at) as day,
                COUNT(*) as total,
                SUM(escalated) as esc
         FROM speech_segments
         WHERE updated_at IS NOT NULL AND verdict IS NOT NULL
         GROUP BY day
         ORDER BY day ASC
         LIMIT 30",
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
    fn few_shot_examples_rank_by_relevance_not_just_recency() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        // Example-owning segments (FK) + the query segment whose text matches the relevant example.
        for (id, text) in [("e-rel", "x"), ("e-old1", "x"), ("e-old2", "x"), ("seg-q", "ساڵی نوێ پیرۆز بێت")] {
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
    fn apply_autonomy_changes_routing_per_dial() {
        use crate::settings::AutonLevel;
        let hyps = vec![make_hyp("s1", "m", "x")];
        let accept = || T0Decision::AutoAccept { segment_id: "s1".into(), consensus: "x".into(), confidence: 0.9 };
        let escalate =
            || T0Decision::EscalateToT1 { segment_id: "s1".into(), hypotheses: hyps.clone(), disagreement_score: 0.4 };

        // ActConfirm (default): base decisions pass through unchanged.
        assert!(matches!(apply_autonomy(accept(), &AutonLevel::ActConfirm, "x", &hyps, 0.9), T0Decision::AutoAccept { .. }));
        assert!(matches!(apply_autonomy(escalate(), &AutonLevel::ActConfirm, "x", &hyps, 0.4), T0Decision::EscalateToT1 { .. }));

        // Propose / Observe: a confident accept is STAGED for the human instead of auto-committed.
        assert!(matches!(apply_autonomy(accept(), &AutonLevel::Propose, "x", &hyps, 0.9), T0Decision::EscalateToT1 { .. }));
        assert!(matches!(apply_autonomy(accept(), &AutonLevel::Observe, "x", &hyps, 0.9), T0Decision::EscalateToT1 { .. }));

        // ActAuto: a low-confidence escalate is committed unattended.
        assert!(matches!(apply_autonomy(escalate(), &AutonLevel::ActAuto, "x", &hyps, 0.4), T0Decision::AutoAccept { .. }));
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
        let obs = run_t0_gate(&db, &["s-dial".to_string()], &AutonLevel::Observe).unwrap();
        assert_eq!(obs.total, 1);
        assert!(
            db.get_segment_by_id("s-dial").unwrap().unwrap().verdict.unwrap_or_default().is_empty(),
            "Observe must commit no verdict"
        );

        // ActConfirm: the SAME segment now gets its escalated verdict written — the dial changed backend behavior.
        let act = run_t0_gate(&db, &["s-dial".to_string()], &AutonLevel::ActConfirm).unwrap();
        assert_eq!(act.escalated, 1);
        assert_eq!(db.get_segment_by_id("s-dial").unwrap().unwrap().verdict.as_deref(), Some("escalated"));
    }
}
