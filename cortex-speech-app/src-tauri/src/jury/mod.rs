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

    let ctc = seg.ctc_score.unwrap_or(-5.0);
    let nonconformity_score = ((1.0 - irt_confidence) + 0.1 * (-ctc)).max(0.0);

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

/// Batch-run the T0 gate over a list of segment IDs.
///
/// Steps:
///   1. Load all hypotheses for the requested segments.
///   2. Run IRT to get per-segment consensus + confidence.
///   3. Get the conformal threshold (from verified segments).
///   4. Route each segment to AutoAccept or EscalateToT1.
///   5. Write the verdict to the DB.
pub fn run_t0_gate(db: &Database, segment_ids: &[String]) -> AppResult<T0GateReport> {
    // 1. Load only the requested segments (not the full dataset).
    let all_segs = db.get_segments_by_ids(segment_ids)?;
    let target_segs: Vec<&SpeechSegment> = all_segs.iter().collect();

    let all_hyps = db.get_all_hypotheses()?;

    // We still need all verified segments to calibrate the conformal threshold.
    let all_verified = db.get_segments(Some(true))?;

    // 2. Run IRT over all hypotheses
    let irt_results = irt::fit_irt_consensus(&all_hyps);

    // 3. Determine conformal threshold (requires all verified segments for calibration)
    let cert = conformal::calibrate_and_certify(&all_verified, 0.05, 0.90);
    let threshold = cert.threshold;

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

        let decision = t0_gate_segment(seg, &seg_hyps, &consensus, irt_confidence, threshold);

        // 5. Write verdict to DB
        match &decision {
            T0Decision::AutoAccept { consensus, confidence, .. } => {
                write_verdict(db, &seg.id, Verdict::AutoAccept, Some(consensus), None, None, Some(*confidence))?;
                auto_accepted += 1;
            }
            T0Decision::EscalateToT1 { .. } => {
                write_verdict(db, &seg.id, Verdict::Escalated, None, None, None, None)?;
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

/// Retrieve k nearest few-shot examples for a given segment.
/// Currently uses recency (most recent first) as a proxy for similarity.
/// Phase 6 will upgrade to embedding-based retrieval.
pub fn get_few_shot_examples(db: &Database, _segment_id: &str, k: usize) -> AppResult<Vec<FewShotExample>> {
    let mut stmt = db.connection().prepare(
        "SELECT id, segment_id, wrong_transcript, human_fix, created_at
         FROM agent_examples
         ORDER BY created_at DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![k as i64], |row| {
        Ok(FewShotExample {
            id: row.get(0)?,
            segment_id: row.get(1)?,
            wrong_transcript: row.get(2)?,
            human_fix: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Return escalated segments ordered by descending disagreement (riskiest first).
pub fn get_escalation_queue(db: &Database, limit: usize) -> AppResult<Vec<EscalatedItem>> {
    let mut stmt = db.connection().prepare(
        "SELECT id, raw_transcript, normalized_transcript, audio_path,
                confidence, agent_confidence, rationale, evidence_json
         FROM speech_segments
         WHERE escalated = 1 AND (human_decision IS NULL OR human_decision = '')
         ORDER BY COALESCE(agent_confidence, 0.5) ASC
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
}
