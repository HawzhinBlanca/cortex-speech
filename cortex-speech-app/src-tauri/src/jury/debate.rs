//! jury/debate.rs — Diverse-judge debate tier (Phase 7)
//!
//! Invoked ONLY when T2 self-consistency fails (no majority after N samples).
//! Two diverse judges deliberate: a Gemini audio-LLM and a re-ASR run from
//! a shadow model.  A swap-agreement guard prevents position-biased verdicts.

use serde::{Deserialize, Serialize};

// ─── Types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebateVerdict {
    pub winning_transcript: String,
    pub minority_view: Option<String>,
    /// True if the verdict survived a hypothesis-order swap.
    pub swap_stable: bool,
    pub must_escalate: bool,
    pub reason: String,
}

// ─── Core logic ──────────────────────────────────────────────────────────────

/// Run the debate tier.
///
/// Parameters:
///   - `judge_a_transcript`: verdict from the primary audio-LLM judge.
///   - `judge_b_transcript`: verdict from the shadow re-ASR judge.
///   - `judge_a_swapped`: verdict from judge A after hypothesis order is swapped.
///   - `judge_b_swapped`: verdict from judge B after hypothesis order is swapped.
///
/// Swap-agreement rule: a verdict is only accepted if it survives the swap.
/// If both judges agree on the same transcript **and** it is stable under swap
/// → accept.  Otherwise → escalate to human.
pub fn adjudicate(
    judge_a_transcript: &str,
    judge_b_transcript: &str,
    judge_a_swapped: &str,
    judge_b_swapped: &str,
) -> DebateVerdict {
    let judge_a = judge_a_transcript.trim();
    let judge_b = judge_b_transcript.trim();
    let judge_a_swapped = judge_a_swapped.trim();
    let judge_b_swapped = judge_b_swapped.trim();
    let a_stable = judge_a == judge_a_swapped;
    let b_stable = judge_b == judge_b_swapped;
    let ab_agree = judge_a == judge_b;

    if judge_a.is_empty() || judge_b.is_empty() {
        return DebateVerdict {
            winning_transcript: String::new(),
            minority_view: if judge_b.is_empty() { None } else { Some(judge_b.to_string()) },
            swap_stable: false,
            must_escalate: true,
            reason: "Diverse judge debate produced a blank transcript; escalating to human.".into(),
        };
    }

    if ab_agree && a_stable && b_stable {
        DebateVerdict {
            winning_transcript: judge_a.to_string(),
            minority_view: None,
            swap_stable: true,
            must_escalate: false,
            reason: "Both diverse judges agreed and verdict is swap-stable.".into(),
        }
    } else if ab_agree {
        // Judges agree but at least one is not swap-stable — soft win, still escalate
        DebateVerdict {
            winning_transcript: judge_a.to_string(),
            minority_view: None,
            swap_stable: false,
            must_escalate: true,
            reason: "Judges agreed but verdict is not swap-stable — possible position bias.".into(),
        }
    } else {
        DebateVerdict {
            winning_transcript: String::new(),
            minority_view: Some(judge_b.to_string()),
            swap_stable: false,
            must_escalate: true,
            reason: format!("Diverse judges disagree: A='{}' vs B='{}'. Escalating to human.", judge_a, judge_b),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debate_clean_agreement() {
        let v = adjudicate("کوردستان", "کوردستان", "کوردستان", "کوردستان");
        assert!(!v.must_escalate);
        assert!(v.swap_stable);
        assert_eq!(v.winning_transcript, "کوردستان");
    }

    #[test]
    fn test_debate_swap_unstable() {
        // Judges agree but flip under swap → must escalate
        let v = adjudicate("کوردستان", "کوردستان", "ئێران", "ئێران");
        assert!(v.must_escalate);
        assert!(!v.swap_stable);
    }

    #[test]
    fn test_debate_disagree() {
        let v = adjudicate("کوردستان", "ئێران", "کوردستان", "ئێران");
        assert!(v.must_escalate);
        assert!(v.minority_view.is_some());
    }

    #[test]
    fn test_debate_blank_agreement_escalates() {
        let v = adjudicate("", "   ", "", "   ");
        assert!(v.must_escalate);
        assert!(v.winning_transcript.is_empty());
        assert!(v.reason.contains("blank transcript"));
    }
}
