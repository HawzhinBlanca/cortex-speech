//! jury/learning.rs — DPO-lite preference export
//!
//! Phase 6 of the Listening Jury build.
//! Exports the `agent_examples` table as a preference-pair dataset suitable
//! for a DPO fine-tuning endpoint (Ollama/vLLM compatible JSON-lines format).
//!
//! The few-shot retrieval (nearest-example lookup) lives in `jury::mod`
//! via `get_few_shot_examples()`; this module handles the batch export.

use crate::db::{Database, SourceTranscriptRecord};
use crate::error::AppResult;
use crate::pipeline::SourceAudioIdentity;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

// ─── Types ─────────────────────────────────────────────────────────────────

/// A single DPO preference pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DpoPair {
    /// The shared prompt context (audio features or segment metadata).
    pub prompt: String,
    /// The "chosen" (human-approved) response.
    pub chosen: String,
    /// The "rejected" (agent-proposed wrong) response.
    pub rejected: String,
}

/// Result of a DPO export run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DpoExportResult {
    pub pair_count: usize,
    /// JSONL string ready to POST to the fine-tuning endpoint.
    pub jsonl: String,
}

#[derive(Debug)]
struct LearningRow {
    segment_id: String,
    wrong_transcript: String,
    human_fix: String,
    raw_transcript: String,
    normalized_transcript: Option<String>,
    verdict: Option<String>,
    verdict_transcript: Option<String>,
    rationale: Option<String>,
    evidence_json: Option<String>,
    audio_path: String,
    duration_ms: i64,
    speaker_id: Option<String>,
    confidence: Option<f64>,
    agent_confidence: Option<f64>,
}

// ─── Export ──────────────────────────────────────────────────────────────────

/// Build a DPO preference dataset from `agent_examples`.
///
/// Excludes examples linked to `is_gold = 1` segments so the permanent
/// holdout is never used for training.
pub fn build_dpo_dataset(db: &Database) -> AppResult<DpoExportResult> {
    let mut stmt = db.connection().prepare(
        "SELECT ae.segment_id,
                ae.wrong_transcript,
                ae.human_fix,
                ss.raw_transcript,
                ss.normalized_transcript,
                ss.verdict,
                ss.verdict_transcript,
                ss.rationale,
                ss.evidence_json,
                ss.audio_path,
                ss.duration_ms,
                ss.speaker_id,
                ss.confidence,
                ss.agent_confidence
         FROM agent_examples ae
         JOIN speech_segments ss ON ae.segment_id = ss.id
         WHERE ss.is_gold = 0
         ORDER BY ae.created_at DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(LearningRow {
            segment_id: row.get(0)?,
            wrong_transcript: row.get(1)?,
            human_fix: row.get(2)?,
            raw_transcript: row.get(3)?,
            normalized_transcript: row.get(4)?,
            verdict: row.get(5)?,
            verdict_transcript: row.get(6)?,
            rationale: row.get(7)?,
            evidence_json: row.get(8)?,
            audio_path: row.get(9)?,
            duration_ms: row.get(10)?,
            speaker_id: row.get(11)?,
            confidence: row.get(12)?,
            agent_confidence: row.get(13)?,
        })
    })?;

    let mut pairs: Vec<DpoPair> = Vec::new();
    for r in rows {
        let row = r?;
        let wrong = row.wrong_transcript.trim();
        let fix = row.human_fix.trim();
        if wrong.is_empty() || fix.is_empty() || learning_text_key(wrong) == learning_text_key(fix) {
            continue;
        }
        let prompt = build_learning_prompt(db, &row)?;
        pairs.push(DpoPair { prompt, chosen: fix.to_string(), rejected: wrong.to_string() });
    }

    let jsonl = pairs.iter().map(serde_json::to_string).collect::<Result<Vec<_>, _>>()?.join("\n");

    Ok(DpoExportResult { pair_count: pairs.len(), jsonl })
}

fn build_learning_prompt(db: &Database, row: &LearningRow) -> AppResult<String> {
    let hypotheses = db.get_hypotheses_for_segment(&row.segment_id)?;
    let references = current_audio_source_references(db, &row.audio_path)?;

    let mut prompt = String::new();
    prompt.push_str("Task: learn to correct Sorani Kurdish ASR/jury transcript proposals from human edits.\n");
    prompt.push_str("Return only the corrected transcript text.\n\n");
    prompt.push_str("Segment context:\n");
    push_field(&mut prompt, "segment_id", Some(row.segment_id.as_str()));
    push_field(&mut prompt, "audio_path", Some(&learning_audio_path_label(&row.audio_path)));
    push_field(&mut prompt, "duration_ms", Some(&row.duration_ms.to_string()));
    push_field(&mut prompt, "speaker_id", row.speaker_id.as_deref());
    push_field(&mut prompt, "raw_asr", Some(row.raw_transcript.as_str()));
    push_field(&mut prompt, "normalized_asr", row.normalized_transcript.as_deref());
    push_field(&mut prompt, "jury_verdict", row.verdict.as_deref());
    push_field(&mut prompt, "jury_transcript", row.verdict_transcript.as_deref());
    push_field(&mut prompt, "jury_rationale", row.rationale.as_deref());
    push_field(&mut prompt, "asr_confidence", row.confidence.map(|v| format!("{v:.3}")).as_deref());
    push_field(&mut prompt, "agent_confidence", row.agent_confidence.map(|v| format!("{v:.3}")).as_deref());

    if let Some(evidence) = row
        .evidence_json
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .filter(|evidence| learning_evidence_has_current_source_reference_coverage(evidence, &references))
    {
        push_field(&mut prompt, "jury_evidence_json", Some(&preview(evidence, 1200)));
        append_reference_agreement_summary(&mut prompt, evidence);
    }

    if !hypotheses.is_empty() {
        prompt.push_str("Hypotheses:\n");
        for hyp in hypotheses {
            let confidence = hyp.confidence.map(|v| format!("{v:.3}")).unwrap_or_else(|| "n/a".to_string());
            prompt.push_str(&format!(
                "- model={} confidence={} text={}\n",
                hyp.model_id,
                confidence,
                preview(&hyp.transcript, 400)
            ));
        }
    }

    if !references.is_empty() {
        prompt.push_str("Source references:\n");
        for reference in references {
            prompt.push_str(&format!(
                "- model={} text={}\n",
                reference.model_id,
                preview(&reference.transcript_text, 700)
            ));
        }
    }

    Ok(prompt)
}

fn current_audio_source_references(db: &Database, audio_path: &str) -> AppResult<Vec<SourceTranscriptRecord>> {
    let references = db.get_source_transcripts_for_audio(audio_path)?;
    if references.is_empty() {
        return Ok(Vec::new());
    }

    let Ok(current_identity) = crate::pipeline::source_audio_identity(Path::new(audio_path)) else {
        return Ok(Vec::new());
    };

    Ok(references
        .into_iter()
        .filter(|reference| source_reference_matches_current_audio(reference, &current_identity))
        .collect())
}

fn source_reference_matches_current_audio(
    reference: &SourceTranscriptRecord,
    current_identity: &SourceAudioIdentity,
) -> bool {
    if !crate::agentic::is_usable_source_reference_transcript(&reference.transcript_text) {
        return false;
    }
    let Some(stored_hash) = reference.audio_content_hash.as_deref().filter(|value| !value.trim().is_empty()) else {
        return false;
    };
    let Some(stored_size) = reference.audio_size_bytes else {
        return false;
    };

    stored_hash == current_identity.content_hash && stored_size == current_identity.size_bytes
}

fn learning_evidence_has_current_source_reference_coverage(
    evidence_json: &str,
    references: &[SourceTranscriptRecord],
) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(evidence_json) else {
        return true;
    };
    let referenced_models = source_reference_model_ids_in_evidence(&value);
    if referenced_models.is_empty() {
        return true;
    }

    let current_models = references.iter().map(|reference| reference.model_id.as_str()).collect::<BTreeSet<_>>();
    referenced_models.iter().all(|model| current_models.contains(model.as_str()))
}

fn source_reference_model_ids_in_evidence(value: &serde_json::Value) -> BTreeSet<String> {
    let mut models = BTreeSet::new();
    collect_source_reference_model_ids(value, &mut models);
    models
}

fn collect_source_reference_model_ids(value: &serde_json::Value, models: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(model_id) = map.get("referenceModelId").and_then(serde_json::Value::as_str) {
                collect_reference_model_id_tokens(model_id, models);
            }
            for key in ["sourceReferenceModels", "requiredSourceReferenceModels"] {
                if let Some(items) = map.get(key).and_then(serde_json::Value::as_array) {
                    for item in items {
                        if let Some(model_id) = item.as_str() {
                            collect_reference_model_id_tokens(model_id, models);
                        }
                    }
                }
            }
            if let Some(selection) = map.get("referenceSelection") {
                collect_source_reference_model_ids(selection, models);
            }
            if let Some(items) = map.get("referenceAgreement").and_then(serde_json::Value::as_array) {
                for item in items {
                    collect_source_reference_model_ids(item, models);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_source_reference_model_ids(item, models);
            }
        }
        _ => {}
    }
}

fn collect_reference_model_id_tokens(model_id: &str, models: &mut BTreeSet<String>) {
    let payload = model_id.rsplit_once(':').map(|(_, tail)| tail).unwrap_or(model_id);
    for token in payload.split('+') {
        let token = token.trim();
        if !token.is_empty() && token != "unknown" {
            models.insert(token.to_string());
        }
    }
}

fn learning_audio_path_label(audio_path: &str) -> String {
    Path::new(audio_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "audio".to_string())
}

fn push_field(prompt: &mut String, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        let value = value.trim();
        if !value.is_empty() {
            prompt.push_str(&format!("- {key}: {}\n", preview(value, 700)));
        }
    }
}

fn preview(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim().replace(['\r', '\n'], " ");
    let mut chars = trimmed.chars();
    let mut out: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}

fn append_reference_agreement_summary(prompt: &mut String, evidence_json: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(evidence_json) else {
        return;
    };

    let mut agreements = Vec::new();
    collect_reference_agreements(&value, &mut agreements);
    if agreements.is_empty() {
        return;
    }

    prompt.push_str("Reference agreement:\n");
    for agreement in agreements {
        let reference_model = json_str(agreement, "referenceModelId").unwrap_or("unknown");
        let selected_model = json_str(agreement, "selectedModelId").unwrap_or("unknown");
        let selected_text = json_str(agreement, "selectedTranscript").unwrap_or("");
        let should_commit = agreement.get("shouldCommit").and_then(serde_json::Value::as_bool).unwrap_or(false);
        let score = json_f64(agreement, "selectedScore")
            .map(|value| format!("{value:.3}"))
            .unwrap_or_else(|| "n/a".to_string());
        let margin =
            json_f64(agreement, "margin").map(|value| format!("{value:.3}")).unwrap_or_else(|| "n/a".to_string());
        prompt.push_str(&format!(
            "- reference_model={} selected_model={} should_commit={} score={} margin={} text={}\n",
            reference_model,
            selected_model,
            should_commit,
            score,
            margin,
            preview(selected_text, 300)
        ));
    }
}

fn collect_reference_agreements<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(items) = map.get("referenceAgreement").and_then(serde_json::Value::as_array) {
                out.extend(items);
            }
            if let Some(selection) = map.get("referenceSelection") {
                collect_reference_agreements(selection, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_reference_agreements(item, out);
            }
        }
        _ => {}
    }
}

fn json_str<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(serde_json::Value::as_str).filter(|value| !value.trim().is_empty())
}

fn json_f64(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(serde_json::Value::as_f64)
}

fn learning_text_key(text: &str) -> String {
    text.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// POST the DPO preference dataset to a local fine-tuning endpoint.
pub fn run_dpo_update(db: &Database, endpoint: &str) -> AppResult<String> {
    let export = build_dpo_dataset(db)?;

    if export.pair_count == 0 {
        return Ok("No preference pairs to export.".into());
    }

    let resp = ureq::post(endpoint)
        .set("Content-Type", "application/x-ndjson")
        .send_string(&export.jsonl)
        .map_err(|e| crate::error::AppError::Other(format!("DPO update POST failed: {e}")))?;

    let status = resp.status();
    Ok(format!("DPO update submitted: {} pairs → {} (HTTP {})", export.pair_count, endpoint, status))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{SegmentHypothesis, SourceTranscriptRecord, SpeechSegment};
    use rusqlite::params;

    fn open_mem_db() -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db
    }

    fn insert_current_source_reference(db: &Database, audio_path: &str, model_id: &str, transcript_text: &str) {
        let identity =
            crate::pipeline::source_audio_identity(std::path::Path::new(audio_path)).expect("audio identity");
        db.upsert_source_transcript(&SourceTranscriptRecord {
            audio_path: audio_path.to_string(),
            model_id: model_id.to_string(),
            audio_content_hash: Some(identity.content_hash),
            audio_size_bytes: Some(identity.size_bytes),
            transcript_path: format!("{model_id}.source_reference.txt"),
            transcript_text: transcript_text.to_string(),
            created_at: None,
        })
        .expect("insert source reference");
    }

    #[test]
    fn test_build_dpo_empty() {
        let db = open_mem_db();
        let result = build_dpo_dataset(&db).unwrap();
        assert_eq!(result.pair_count, 0);
        assert!(result.jsonl.is_empty());
    }

    #[test]
    fn build_dpo_dataset_includes_multi_agent_context() {
        let db = open_mem_db();
        let temp = tempfile::tempdir().expect("temp dir");
        let audio_path = temp.path().join("source.wav");
        std::fs::write(&audio_path, b"current source audio bytes").expect("write audio");
        let audio_path_str = audio_path.to_string_lossy().to_string();

        let segment = SpeechSegment {
            id: "seg-learning".to_string(),
            audio_path: audio_path_str.clone(),
            raw_transcript: "raw asr text".to_string(),
            normalized_transcript: Some("normalized asr text".to_string()),
            duration_ms: 4200,
            speaker_id: Some("speaker-a".to_string()),
            confidence: Some(0.61),
            verdict: Some("jury_accept".to_string()),
            verdict_transcript: Some("agent wrong text".to_string()),
            rationale: Some("reference-aware agent selected a weak candidate".to_string()),
            evidence_json: Some("{\"referenceCommitted\":false}".to_string()),
            agent_confidence: Some(0.77),
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).expect("insert segment");
        db.write_segment_verdict(
            "seg-learning",
            "jury_accept",
            Some("agent wrong text"),
            Some("reference-aware agent selected a weak candidate"),
            Some("{\"referenceCommitted\":false}"),
            Some(0.77),
            false,
        )
        .expect("write verdict");
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: "seg-learning".to_string(),
            model_id: "omniasr-wsl-7b".to_string(),
            transcript: "agent wrong text".to_string(),
            confidence: Some(0.77),
        })
        .expect("insert hypothesis");
        insert_current_source_reference(&db, &audio_path_str, "gemini-2.5-pro", "source reference text");
        db.connection()
            .execute(
                "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix)
                 VALUES (?1, ?2, ?3, ?4)",
                params!["ex-learning", "seg-learning", "agent wrong text", "human corrected text"],
            )
            .expect("insert learning example");

        let result = build_dpo_dataset(&db).expect("build dpo");
        assert_eq!(result.pair_count, 1);
        let pair: DpoPair = serde_json::from_str(result.jsonl.lines().next().expect("jsonl row")).expect("dpo pair");

        assert_eq!(pair.chosen, "human corrected text");
        assert_eq!(pair.rejected, "agent wrong text");
        assert!(pair.prompt.contains("Hypotheses:"));
        assert!(pair.prompt.contains("omniasr-wsl-7b"));
        assert!(pair.prompt.contains("Source references:"));
        assert!(pair.prompt.contains("gemini-2.5-pro"));
        assert!(pair.prompt.contains("jury_rationale"));
        assert!(pair.prompt.contains("reference-aware agent selected a weak candidate"));
    }

    #[test]
    fn build_dpo_dataset_sanitizes_prompt_audio_path() {
        let db = open_mem_db();
        let segment = SpeechSegment {
            id: "seg-private-path-learning".to_string(),
            audio_path: r"Z:\source-audio-root\private\source-audio.wav".to_string(),
            raw_transcript: "raw asr text".to_string(),
            duration_ms: 4200,
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).expect("insert segment");
        db.connection()
            .execute(
                "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    "ex-private-path-learning",
                    "seg-private-path-learning",
                    "agent wrong text",
                    "human corrected text"
                ],
            )
            .expect("insert learning example");

        let result = build_dpo_dataset(&db).expect("build dpo");
        assert_eq!(result.pair_count, 1);
        let pair: DpoPair = serde_json::from_str(result.jsonl.lines().next().expect("jsonl row")).expect("dpo pair");

        assert!(pair.prompt.contains("audio_path: source-audio.wav"));
        assert!(!pair.prompt.contains(r"Z:\source-audio-root"));
        assert!(!pair.prompt.contains(r"\private\"));
    }

    #[test]
    fn build_dpo_dataset_ignores_stale_source_reference_context() {
        let db = open_mem_db();
        let temp = tempfile::tempdir().expect("temp dir");
        let audio_path = temp.path().join("source.wav");
        std::fs::write(&audio_path, b"current source audio bytes").expect("write audio");
        let audio_path_str = audio_path.to_string_lossy().to_string();
        let evidence = serde_json::json!({
            "referenceModelId": "gemini-2.5-pro",
            "selectedModelId": "omniasr-wsl-7b",
            "selectedTranscript": "agent wrong text",
            "shouldCommit": true,
            "referenceAgreement": [
                {
                    "referenceModelId": "gemini-2.5-pro",
                    "selectedModelId": "omniasr-wsl-7b",
                    "selectedTranscript": "agent wrong text",
                    "selectedScore": 0.91,
                    "margin": 0.22,
                    "shouldCommit": true
                }
            ]
        })
        .to_string();

        let segment = SpeechSegment {
            id: "seg-stale-reference-learning".to_string(),
            audio_path: audio_path_str.clone(),
            raw_transcript: "raw asr text".to_string(),
            duration_ms: 4200,
            verdict: Some("jury_accept".to_string()),
            verdict_transcript: Some("agent wrong text".to_string()),
            evidence_json: Some(evidence),
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).expect("insert segment");
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: "seg-stale-reference-learning".to_string(),
            model_id: "omniasr-wsl-7b".to_string(),
            transcript: "agent wrong text".to_string(),
            confidence: Some(0.77),
        })
        .expect("insert hypothesis");
        db.upsert_source_transcript(&SourceTranscriptRecord {
            audio_path: audio_path_str,
            model_id: "gemini-2.5-pro".to_string(),
            audio_content_hash: Some("stale-audio-hash".to_string()),
            audio_size_bytes: Some(1),
            transcript_path: temp.path().join("stale-source.txt").to_string_lossy().to_string(),
            transcript_text: "stale unrelated source reference text".to_string(),
            created_at: None,
        })
        .expect("insert stale source reference");
        db.connection()
            .execute(
                "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    "ex-stale-reference-learning",
                    "seg-stale-reference-learning",
                    "agent wrong text",
                    "human corrected text"
                ],
            )
            .expect("insert learning example");

        let result = build_dpo_dataset(&db).expect("build dpo");
        assert_eq!(result.pair_count, 1);
        let pair: DpoPair = serde_json::from_str(result.jsonl.lines().next().expect("jsonl row")).expect("dpo pair");

        assert!(pair.prompt.contains("Hypotheses:"));
        assert!(pair.prompt.contains("omniasr-wsl-7b"));
        assert!(!pair.prompt.contains("Source references:"));
        assert!(!pair.prompt.contains("jury_evidence_json"));
        assert!(!pair.prompt.contains("Reference agreement:"));
        assert!(!pair.prompt.contains("gemini-2.5-pro"));
        assert!(!pair.prompt.contains("stale unrelated source reference text"));
    }

    #[test]
    fn build_dpo_dataset_summarizes_reference_agreement_votes() {
        let db = open_mem_db();
        let temp = tempfile::tempdir().expect("temp dir");
        let audio_path = temp.path().join("reference-source.wav");
        std::fs::write(&audio_path, b"current reference source audio bytes").expect("write audio");
        let audio_path_str = audio_path.to_string_lossy().to_string();
        let evidence = serde_json::json!({
            "referenceModelId": "multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash",
            "selectedModelId": "omniasr-wsl-7b",
            "selectedTranscript": "agent wrong text",
            "shouldCommit": true,
            "referenceAgreement": [
                {
                    "referenceModelId": "gemini-2.5-pro",
                    "selectedModelId": "omniasr-wsl-7b",
                    "selectedTranscript": "agent wrong text",
                    "selectedScore": 0.91,
                    "margin": 0.22,
                    "shouldCommit": true
                },
                {
                    "referenceModelId": "gemini-2.5-flash",
                    "selectedModelId": "omniasr-wsl-7b",
                    "selectedTranscript": "agent wrong text",
                    "selectedScore": 0.88,
                    "margin": 0.19,
                    "shouldCommit": true
                }
            ]
        })
        .to_string();
        let segment = SpeechSegment {
            id: "seg-reference-learning".to_string(),
            audio_path: audio_path_str.clone(),
            raw_transcript: "raw asr text".to_string(),
            verdict: Some("jury_accept".to_string()),
            verdict_transcript: Some("agent wrong text".to_string()),
            rationale: Some("multi-reference consensus committed agent text".to_string()),
            evidence_json: Some(evidence.clone()),
            duration_ms: 2100,
            agent_confidence: Some(0.88),
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).expect("insert segment");
        db.write_segment_verdict(
            "seg-reference-learning",
            "jury_accept",
            Some("agent wrong text"),
            Some("multi-reference consensus committed agent text"),
            Some(&evidence),
            Some(0.88),
            false,
        )
        .expect("write verdict");
        insert_current_source_reference(&db, &audio_path_str, "gemini-2.5-pro", "source reference pro text");
        insert_current_source_reference(&db, &audio_path_str, "gemini-2.5-flash", "source reference flash text");
        db.connection()
            .execute(
                "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix)
                 VALUES (?1, ?2, ?3, ?4)",
                params!["ex-reference-learning", "seg-reference-learning", "agent wrong text", "human corrected text"],
            )
            .expect("insert learning example");

        let result = build_dpo_dataset(&db).expect("build dpo");
        let pair: DpoPair = serde_json::from_str(result.jsonl.lines().next().expect("jsonl row")).expect("dpo pair");

        assert!(pair.prompt.contains("Reference agreement:"));
        assert!(pair.prompt.contains("reference_model=gemini-2.5-pro"));
        assert!(pair.prompt.contains("reference_model=gemini-2.5-flash"));
        assert!(pair.prompt.contains("selected_model=omniasr-wsl-7b"));
        assert!(pair.prompt.contains("should_commit=true"));
        assert!(pair.prompt.contains("score=0.910"));
        assert!(pair.prompt.contains("margin=0.190"));
    }

    #[test]
    fn build_dpo_dataset_skips_duplicate_wrong_and_fix_text() {
        let db = open_mem_db();
        let segment = SpeechSegment {
            id: "seg-duplicate".to_string(),
            audio_path: "/audio/source.wav".to_string(),
            raw_transcript: "same text".to_string(),
            duration_ms: 1000,
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).expect("insert segment");
        db.connection()
            .execute(
                "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix)
                 VALUES (?1, ?2, ?3, ?4)",
                params!["ex-duplicate", "seg-duplicate", "same   text", "same text"],
            )
            .expect("insert learning example");

        let result = build_dpo_dataset(&db).expect("build dpo");
        assert_eq!(result.pair_count, 0);
        assert!(result.jsonl.is_empty());
    }
}
