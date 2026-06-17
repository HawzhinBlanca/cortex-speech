use crate::db::SpeechSegment;
use crate::wer::compute_cer;
use serde::Serialize;

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConformalCertificate {
    pub target_error: f64,
    pub confidence_level: f64,
    pub threshold: f64,
    pub total_certified: usize,
    pub certified_segment_ids: Vec<String>,
    pub expected_error_bound: f64,
    pub is_calibrated: bool,
}

/// Computes the nonconformity score for a segment.
/// S = (1.0 - confidence) + 0.1 * (-ctc_score)
pub fn compute_nonconformity_score(seg: &SpeechSegment) -> f64 {
    let conf = seg.confidence.unwrap_or(0.5);
    let ctc = seg.ctc_score.unwrap_or(-5.0);
    let score = (1.0 - conf) + 0.1 * (-ctc);
    score.max(0.0)
}

/// Calibrates the conformal threshold using verified segments as the calibration set.
/// If there are fewer than 10 verified segments, it falls back to a default heuristic.
pub fn calibrate_and_certify(
    all_segments: &[SpeechSegment],
    target_error: f64,     // e.g., 0.05 for 5% CER
    confidence_level: f64, // e.g., 0.95 for 95% confidence
) -> ConformalCertificate {
    let delta = 1.0 - confidence_level;
    let delta = delta.max(1e-5); // safety guard

    // Extract calibration set: verified segments with non-empty reference
    let cal_set: Vec<(&SpeechSegment, f64, f64)> = all_segments
        .iter()
        .filter_map(|s| {
            if !s.verified {
                return None;
            }
            let ref_text = s.annotated_transcript.as_deref()?.trim();
            if ref_text.is_empty() {
                return None;
            }
            let score = compute_nonconformity_score(s);
            let hyp_text = &s.raw_transcript;
            let cer = compute_cer(ref_text, hyp_text).min(1.0); // Bound error to [0, 1] for Hoeffding
            Some((s, score, cer))
        })
        .collect();

    // Check if we have enough calibration data
    if cal_set.len() < 10 {
        // Fallback to heuristic threshold
        let heuristic_threshold = 0.35; // default conservative threshold
        let mut certified_ids = Vec::new();
        for seg in all_segments {
            if compute_nonconformity_score(seg) <= heuristic_threshold {
                certified_ids.push(seg.id.clone());
            }
        }
        return ConformalCertificate {
            target_error,
            confidence_level,
            threshold: heuristic_threshold,
            total_certified: certified_ids.len(),
            certified_segment_ids: certified_ids,
            expected_error_bound: target_error, // nominal
            is_calibrated: false,
        };
    }

    // Sort calibration set by nonconformity score ascending
    let mut sorted_cal = cal_set;
    sorted_cal.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    let n = sorted_cal.len();
    let mut best_k = 0;
    let mut best_threshold = 0.0;
    let mut best_bound = 1.0;

    // We search for the largest k (selected subset size) such that the Hoeffding upper bound is <= target_error
    for k in 1..=n {
        let sum_error: f64 = sorted_cal.iter().take(k).map(|item| item.2).sum();
        let empirical_risk = sum_error / k as f64;

        // Hoeffding inequality upper bound:
        // R+(k) = R_emp + sqrt( ln(1/delta) / (2 * k) )
        let bound = empirical_risk + ((1.0 / delta).ln() / (2.0 * k as f64)).sqrt();

        if bound <= target_error {
            best_k = k;
            best_threshold = sorted_cal[k - 1].1;
            best_bound = bound;
        }
    }

    // If no subset could be certified with mathematical certainty (best_k == 0),
    // we use the single best element as a threshold, but warn/flag it as uncalibrated.
    if best_k == 0 {
        let fallback_threshold = sorted_cal[0].1;
        let mut certified_ids = Vec::new();
        for seg in all_segments {
            if compute_nonconformity_score(seg) <= fallback_threshold {
                certified_ids.push(seg.id.clone());
            }
        }
        return ConformalCertificate {
            target_error,
            confidence_level,
            threshold: fallback_threshold,
            total_certified: certified_ids.len(),
            certified_segment_ids: certified_ids,
            expected_error_bound: 1.0,
            is_calibrated: false,
        };
    }

    // Certify all segments in the entire dataset that are below the calibrated threshold
    let mut certified_ids = Vec::new();
    for seg in all_segments {
        if compute_nonconformity_score(seg) <= best_threshold {
            certified_ids.push(seg.id.clone());
        }
    }

    ConformalCertificate {
        target_error,
        confidence_level,
        threshold: best_threshold,
        total_certified: certified_ids.len(),
        certified_segment_ids: certified_ids,
        expected_error_bound: best_bound,
        is_calibrated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_segment(id: &str, confidence: f64, ctc: f64, verified: bool, raw: &str, ann: &str) -> SpeechSegment {
        SpeechSegment {
            id: id.to_string(),
            created_at: None,
            audio_path: "".to_string(),
            raw_transcript: raw.to_string(),
            normalized_transcript: None,
            annotated_transcript: Some(ann.to_string()),
            alignment_json: None,
            duration_ms: 1000,
            speaker_id: None,
            verified,
            confidence: Some(confidence),
            ctc_score: Some(ctc),
            clipping_ratio: None,
            rms_db: None,
            snr_db: None,
            split: None,
            ood_score: None,
            ..SpeechSegment::default()
        }
    }

    #[test]
    fn test_nonconformity_score() {
        let seg1 = mock_segment("1", 0.9, -1.0, true, "test", "test");
        // (1.0 - 0.9) + 0.1 * 1.0 = 0.1 + 0.1 = 0.2
        assert!((compute_nonconformity_score(&seg1) - 0.2).abs() < 1e-5);
    }

    #[test]
    fn test_conformal_risk_control() {
        let mut segments = Vec::new();
        // 50 verified correct segments (low error, low nonconformity)
        for i in 1..=50 {
            segments.push(mock_segment(&format!("c{}", i), 0.95, -0.5, true, "کورد", "کورد"));
        }
        // 10 verified incorrect segments (high error, high nonconformity)
        for i in 51..=60 {
            segments.push(mock_segment(&format!("c{}", i), 0.4, -6.0, true, "خراب", "جوان"));
        }

        // Add 5 unverified segments
        for i in 1..=5 {
            segments.push(mock_segment(&format!("u{}", i), 0.95, -0.5, false, "کورد", ""));
        }

        let cert = calibrate_and_certify(&segments, 0.2, 0.90);
        assert!(cert.is_calibrated);
        assert!(cert.threshold > 0.0);
        // The unverified segments have low score so they should be certified
        assert!(cert.certified_segment_ids.contains(&"u1".to_string()));
    }
}
