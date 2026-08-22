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
    // verdict_transcript intentionally NOT loaded — for a DPO-eligible (edit) row it equals the human fix
    // (== the pair's `chosen`), so surfacing it in the learning prompt leaked the gold label. See
    // build_learning_prompt.
    rationale: Option<String>,
    evidence_json: Option<String>,
    audio_path: String,
    duration_ms: i64,
    speaker_id: Option<String>,
    confidence: Option<f64>,
    agreement_score: Option<f64>,
    effect_event_id: Option<i64>,
    retained_human_text: Option<String>,
}

// ─── Export ──────────────────────────────────────────────────────────────────

/// Build a DPO preference dataset from `agent_examples`.
///
/// Excludes examples linked to `is_gold = 1` segments so the permanent
/// holdout is never used for training.
pub fn build_dpo_dataset(db: &Database) -> AppResult<DpoExportResult> {
    build_dpo_dataset_filtered(db, None)
}

/// Build preference pairs only for the immutable segment selection owned by a bundle snapshot.
/// This prevents a revoked/private/unvalidated row elsewhere in the live library from hitchhiking in
/// a production bundle whose tabular rows and rights gate never selected it.
pub(crate) fn build_dpo_dataset_for_segment_ids(
    db: &Database,
    allowed_segment_ids: &BTreeSet<String>,
) -> AppResult<DpoExportResult> {
    build_dpo_dataset_filtered(db, Some(allowed_segment_ids))
}

fn build_dpo_dataset_filtered(
    db: &Database,
    allowed_segment_ids: Option<&BTreeSet<String>>,
) -> AppResult<DpoExportResult> {
    // Holdout exclusion — never train on the permanent gold holdout (shared helpers). Hash catches
    // the same content at any path (when the file is present); path catches it fail-closed even when
    // the training file is gone.
    let holdout_hashes = holdout_content_hashes(db)?;
    let holdout_paths = holdout_audio_paths(db)?;

    let mut stmt = db.connection().prepare(
        "SELECT ae.segment_id,
                ae.wrong_transcript,
                ae.human_fix,
                ss.raw_transcript,
                ss.normalized_transcript,
                ss.verdict,
                ss.rationale,
                ss.evidence_json,
                ss.audio_path,
                ss.duration_ms,
                ss.speaker_id,
                ss.confidence,
                ss.agreement_score,
                ae.effect_event_id,
                COALESCE(NULLIF(TRIM(ss.verdict_transcript), ''),
                         NULLIF(TRIM(ss.annotated_transcript), '')) AS retained_human_text
         FROM agent_examples ae
         JOIN speech_segments ss ON ae.segment_id = ss.id
         WHERE ss.is_gold = 0 AND ae.verified_by_human = 1
           AND (
                (ae.effect_event_id IS NOT NULL AND EXISTS (
                     SELECT 1
                       FROM effective_human_decision_effects_v60 effect
                      WHERE effect.id = ae.effect_event_id
                        AND effect.segment_id = ae.segment_id
                        AND effect.action = 'edit'
                ))
                OR
                (ae.effect_event_id IS NULL
                 AND ss.verified = 1
                 AND ss.human_decision = 'edit')
           )
         ORDER BY ae.created_at DESC, ae.id ASC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(LearningRow {
            segment_id: row.get(0)?,
            wrong_transcript: row.get(1)?,
            human_fix: row.get(2)?,
            raw_transcript: row.get(3)?,
            normalized_transcript: row.get(4)?,
            verdict: row.get(5)?,
            rationale: row.get(6)?,
            evidence_json: row.get(7)?,
            audio_path: row.get(8)?,
            duration_ms: row.get(9)?,
            speaker_id: row.get(10)?,
            confidence: row.get(11)?,
            agreement_score: row.get(12)?,
            effect_event_id: row.get(13)?,
            retained_human_text: row.get(14)?,
        })
    })?;

    let mut pairs: Vec<DpoPair> = Vec::new();
    for r in rows {
        let row = r?;
        if allowed_segment_ids.is_some_and(|allowed| !allowed.contains(&row.segment_id)) {
            continue;
        }
        let wrong = row.wrong_transcript.trim();
        let fix = row.human_fix.trim();
        if row.effect_event_id.is_none() && !super::legacy_human_fix_is_current(row.retained_human_text.as_deref(), fix)
        {
            continue;
        }
        if wrong.is_empty() || fix.is_empty() || learning_text_key(wrong) == learning_text_key(fix) {
            continue;
        }

        // Gold-Holdout exclusion (fail-closed). Path match works even when the file is gone.
        if holdout_paths.contains(&row.audio_path) {
            tracing::warn!("Excluding segment {} from DPO export: matches holdout gold audio path", row.segment_id);
            continue;
        }
        let audio_path = Path::new(&row.audio_path);
        if audio_path.exists() {
            match crate::pipeline::source_audio_identity(audio_path) {
                Ok(identity) if holdout_hashes.contains(&identity.content_hash) => {
                    tracing::warn!(
                        "Excluding segment {} from DPO export: matches holdout gold audio hash",
                        row.segment_id
                    );
                    continue;
                }
                Ok(_) => {}
                Err(e) => {
                    // Fail CLOSED: a present-but-unhashable clip (transient lock/permission/flaky disk)
                    // could be the SAME CONTENT as a holdout gold clip at a different path — the path
                    // guard above only catches identical path strings. Including it risks leaking
                    // held-out audio into the training corpus and silently inflating eval, so exclude
                    // an unverifiable clip rather than contaminate.
                    tracing::warn!(
                        "Excluding segment {} from DPO export: could not hash audio to clear holdout ({e})",
                        row.segment_id
                    );
                    continue;
                }
            }
        }

        let prompt = build_learning_prompt(db, &row)?;
        pairs.push(DpoPair { prompt, chosen: fix.to_string(), rejected: wrong.to_string() });
    }

    let jsonl = pairs.iter().map(serde_json::to_string).collect::<Result<Vec<_>, _>>()?.join("\n");

    Ok(DpoExportResult { pair_count: pairs.len(), jsonl })
}

/// The audio content hashes of the permanent holdout gold clips — the single source of truth for
/// excluding held-out audio from any training/LM export. Shared by the DPO, LM-corpus, and
/// HuggingFace exports.
pub(crate) fn holdout_content_hashes(db: &Database) -> AppResult<std::collections::HashSet<String>> {
    // Prefer the hash persisted at import (migration v24): it is durable, so a moved/deleted gold
    // file no longer silently drops its hash (fail-closed). Fall back to hashing from disk only for
    // legacy rows written before the column existed.
    let mut stmt =
        db.connection().prepare("SELECT audio_path, audio_content_hash FROM gold_segments WHERE is_holdout = 1")?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)))?;
    let mut hashes = std::collections::HashSet::new();
    for row in rows {
        let (path_str, persisted) = row?;
        if let Some(hash) = persisted {
            hashes.insert(hash);
        } else {
            let path = Path::new(&path_str);
            if path.exists() {
                if let Ok(identity) = crate::pipeline::source_audio_identity(path) {
                    hashes.insert(identity.content_hash);
                }
            }
        }
    }
    Ok(hashes)
}

/// The audio paths of the permanent holdout gold clips. Used together with
/// [`holdout_content_hashes`] so exclusion is fail-CLOSED: a training segment that shares a holdout
/// clip's audio path is dropped even when its audio file is missing (so its content can no longer be
/// re-hashed). Without this, a deleted/moved training-segment file made `path.exists()` false and the
/// whole holdout check was skipped, silently leaking held-out content into the export.
pub(crate) fn holdout_audio_paths(db: &Database) -> AppResult<std::collections::HashSet<String>> {
    let mut stmt = db.connection().prepare("SELECT audio_path FROM gold_segments WHERE is_holdout = 1")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut paths = std::collections::HashSet::new();
    for row in rows {
        paths.insert(row?);
    }
    Ok(paths)
}

/// Export the LOOP-2 language-model training corpus: the human-confirmed-correct Sorani text from
/// reviewed (`human_decision` = accept|edit), non-gold segments — the curated correct text the
/// flywheel has accumulated. Each line is canonicalized with the char-only normalizer (Kaf/Yeh/Heh
/// folding, numbers preserved) so the LM's tokens match the ASR surface forms it will rescore, and
/// any segment whose audio matches a holdout gold clip is excluded by content hash. Run
/// `kenlm lmplz` on these lines externally (the same prepare-here / train-externally pattern as the
/// fine-tuned 7B).
pub fn export_lm_corpus(db: &Database) -> AppResult<Vec<String>> {
    let holdout = holdout_content_hashes(db)?;
    let holdout_paths = holdout_audio_paths(db)?;
    let normalizer = crate::normalizer::SoraniNormalizer::with_config(crate::normalizer::NormalizationConfig {
        normalize_numbers: false,
        verbalize_numbers: false,
        normalize_hamza: true,
        remove_diacritics: false,
    });

    // Priority mirrors the codebase's canonical "text the human confirmed": the committed
    // verdict_transcript first, then the human's typed annotation, then the VERBATIM raw draft.
    // annotated_transcript was OMITTED (round-24 hunt #8) — a segment whose fix lives only in
    // annotated_transcript (inbox-accept and legacy accepts leave verdict_transcript NULL) exported
    // the SUPERSEDED ASR draft as human-confirmed LM text, exactly the text the human replaced.
    // The normalized rung was removed (VERBATIM LAW 2026-08-12): it holds the LLM-refined
    // paraphrase, and a decided row whose verdict+annotated are empty would export machine
    // paraphrase labeled human-confirmed. Matches record_human_decision's loop0_draft_text
    // (annotated ▸ raw) and quality.rs's training_transcript_with_source.
    let mut stmt = db.connection().prepare(
        "SELECT COALESCE(NULLIF(verdict_transcript, ''), NULLIF(annotated_transcript, ''), raw_transcript),
                audio_path
         FROM speech_segments
         WHERE is_gold = 0 AND human_decision IN ('accept', 'edit')
         ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)))?;

    let mut corpus = Vec::new();
    for row_res in rows {
        let (text, audio_path) = row_res?;
        let Some(text) = text.filter(|t| !t.trim().is_empty()) else { continue };

        // Never emit an ASR PLACEHOLDER ("[Pending WSL 7B ASR]", "[ASR unavailable: …]", "n/a") as
        // human-confirmed LM training text. The COALESCE can fall through to a raw placeholder on an
        // accept/edit row (an export taken mid-import or after a stuck-placeholder incident), poisoning the
        // corpus with a non-speech marker — the exact case is_effective_placeholder catches on the dataset
        // paths, which this LM path bypasses (it never routes through the training-grade gate).
        if crate::quality::is_placeholder_transcript(&text) {
            continue;
        }

        // Fail-closed holdout exclusion: path match drops a held-out clip even if its file is gone.
        if holdout_paths.contains(&audio_path) {
            continue;
        }
        let path = Path::new(&audio_path);
        if path.exists() {
            match crate::pipeline::source_audio_identity(path) {
                Ok(identity) if holdout.contains(&identity.content_hash) => continue,
                Ok(_) => {}
                // Fail CLOSED: an unhashable-but-present clip could be the same content as a holdout
                // clip at a different path; exclude it rather than risk leaking held-out text into the LM
                // corpus and inflating eval.
                Err(_) => continue,
            }
        }

        let normalized = normalizer.normalize(&text);
        if !normalized.trim().is_empty() {
            corpus.push(normalized);
        }
    }
    Ok(corpus)
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
    // NOTE: deliberately NOT emitting verdict_transcript. record_human_decision("edit", fix) overwrites
    // speech_segments.verdict_transcript to the human fix AND that same fix becomes the DPO pair's `chosen`
    // (agent_examples.human_fix). Every DPO-eligible row is created by an edit, so verdict_transcript == chosen
    // for 100% of pairs — echoing it here would embed the gold label in the shared prompt and train a
    // copy-the-jury_transcript shortcut instead of ASR correction (a label leak that defeats the flywheel).
    // The model already gets the text to correct from raw_asr / normalized_asr / the Hypotheses block below.
    push_field(&mut prompt, "jury_rationale", row.rationale.as_deref());
    push_field(&mut prompt, "asr_confidence", row.confidence.map(|v| format!("{v:.3}")).as_deref());
    push_field(&mut prompt, "agreement_score", row.agreement_score.map(|v| format!("{v:.3}")).as_deref());

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

use crate::normalizer::learning_text_key;

/// POST the DPO preference dataset to a local fine-tuning endpoint.
pub fn run_dpo_update(db: &Database, endpoint: &str) -> AppResult<String> {
    // Same outbound allow-list the settings LLM endpoint enforces — this is a parallel channel that
    // POSTs private preference pairs, so it must not be repointable at an arbitrary/non-https host.
    crate::settings::validate_outbound_endpoint(endpoint)?;
    let export = build_dpo_dataset(db)?;

    if export.pair_count == 0 {
        return Ok("No preference pairs to export.".into());
    }

    let resp = crate::http::API_AGENT
        .post(endpoint)
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

    #[test]
    fn run_dpo_update_rejects_unsafe_endpoints_before_posting() {
        // Hardening-audit MEDIUM (exfil channel): the DPO export must enforce the same outbound
        // allow-list as the settings LLM endpoint, so it can't be repointed at an arbitrary host.
        let db = open_mem_db();
        assert!(
            run_dpo_update(&db, "http://attacker.example.com/collect").is_err(),
            "plain-http remote endpoint must be rejected up front"
        );
        assert!(run_dpo_update(&db, "").is_err(), "empty endpoint must be rejected");
        // An allowed endpoint passes validation; with no preference pairs it's a clean no-op (no POST
        // is attempted, so no network is touched in the test).
        let msg = run_dpo_update(&db, "https://localhost:65535/ingest").expect("valid endpoint passes validation");
        assert!(msg.contains("No preference pairs"), "expected no-op export, got: {msg}");
    }

    #[test]
    fn holdout_hashes_survive_a_deleted_gold_file() {
        // Round-3 audit (gold side): the holdout hash is persisted at import (migration v24), so a
        // moved/deleted gold file no longer silently drops it from the exclusion set (fail-closed).
        let db = open_mem_db();
        let temp = tempfile::tempdir().unwrap();
        let audio = temp.path().join("gold.wav");
        std::fs::write(&audio, b"gold audio bytes").unwrap();

        crate::eval::import_gold_segments(
            &db,
            vec![crate::eval::GoldSegmentInput {
                audio_path: audio.to_string_lossy().to_string(),
                reference: "ref".into(),
                is_holdout: true,
            }],
        )
        .unwrap();

        let with_file = holdout_content_hashes(&db).unwrap();
        assert_eq!(with_file.len(), 1, "hash present while the gold file exists");

        std::fs::remove_file(&audio).unwrap(); // gold file moved/deleted after registration
        assert_eq!(
            holdout_content_hashes(&db).unwrap(),
            with_file,
            "the persisted holdout hash must survive a deleted gold file"
        );
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
    fn build_dpo_trains_on_human_corrections_only_not_model_pseudo_labels() {
        // Blueprint #3: model corrections are captured (provenance-tagged pseudo) but NEVER enter the
        // training set, or training on model-generated labels causes model collapse. Only human
        // verbatim edits (verified_by_human=1) become DPO pairs.
        let db = open_mem_db();
        db.insert_segment(&SpeechSegment {
            id: "seg-1".into(),
            audio_path: "/a.wav".into(),
            raw_transcript: "wrong asr text".into(),
            duration_ms: 1000,
            ..Default::default()
        })
        .unwrap();
        // A HUMAN correction — verified_by_human defaults to 1 (trainable).
        super::super::insert_active_human_example_fixture(
            &db,
            "h1",
            "seg-1",
            "wrong asr text",
            "human verified fix",
            None,
        );
        // A MODEL correction — captured as pseudo (verified_by_human=0, NOT trainable).
        db.record_model_correction("seg-1", "wrong asr text", "model proposed fix", "jury").unwrap();

        let result = build_dpo_dataset(&db).expect("build dpo");
        assert_eq!(result.pair_count, 1, "only the human-verified correction trains");
        assert!(result.jsonl.contains("human verified fix"));
        assert!(
            !result.jsonl.contains("model proposed fix"),
            "a model pseudo-label must never enter the DPO training set"
        );
    }

    #[test]
    fn dpo_legacy_example_requires_current_matching_verified_edit() {
        let db = open_mem_db();
        assert_eq!(crate::migrations::rollback(&db, 1).unwrap(), vec![60]);
        db.insert_segment(&SpeechSegment {
            id: "legacy-dpo".into(),
            audio_path: "/legacy-dpo.wav".into(),
            raw_transcript: "wrong draft".into(),
            ..Default::default()
        })
        .unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET verified = 1, human_decision = 'edit', annotated_transcript = 'Human   Fix'
                  WHERE id = 'legacy-dpo'",
                [],
            )
            .unwrap();
        db.connection()
            .execute(
                "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix)
                 VALUES ('legacy-dpo-row', 'legacy-dpo', 'wrong draft', 'human fix')",
                [],
            )
            .unwrap();
        assert_eq!(crate::migrations::run_migrations(&db).unwrap(), vec![60]);

        assert_eq!(build_dpo_dataset(&db).unwrap().pair_count, 1);
        db.connection()
            .execute(
                "UPDATE speech_segments SET annotated_transcript = 'different correction'
                  WHERE id = 'legacy-dpo'",
                [],
            )
            .unwrap();
        assert_eq!(build_dpo_dataset(&db).unwrap().pair_count, 0);
        db.connection()
            .execute(
                "UPDATE speech_segments SET annotated_transcript = ' HUMAN fix ', verified = 0
                  WHERE id = 'legacy-dpo'",
                [],
            )
            .unwrap();
        assert_eq!(build_dpo_dataset(&db).unwrap().pair_count, 0, "unverified legacy evidence must remain inactive");
    }

    #[test]
    fn accept_memory_evidence_uses_the_authoritative_accepted_hypothesis() {
        // Accepting an alternative stored ASR hypothesis is still an `accept`, but the accepted text can
        // differ from annotated/raw. Memory evidence must be judged against what the human actually
        // accepted; comparing the prior draft to itself would invert this Confirm into an Override.
        let db = open_mem_db();
        assert_eq!(crate::migrations::rollback(&db, 1).unwrap(), vec![60]);
        let original = "ئەو ساڵە باش بوو";
        let accepted = "ئەو ساڵە خراپ بوو";
        let memory = crate::corrections::extract_substitution_memories(original, accepted)
            .into_iter()
            .next()
            .expect("one substitution memory");
        db.connection()
            .execute(
                "INSERT INTO correction_memory
                    (id, wrong_token, human_token, slot_key, phonetic_key,
                     confidence, hit_count, confirm_count, override_count)
                 VALUES ('accepted-hypothesis-memory', ?1, ?2, ?3, ?4, 0.5, 0, 0, 0)",
                params![memory.wrong_token, memory.human_token, memory.slot_key, memory.phonetic_key],
            )
            .unwrap();
        assert_eq!(crate::migrations::run_migrations(&db).unwrap(), vec![60]);

        db.insert_segment(&SpeechSegment {
            id: "accepted-hypothesis-segment".into(),
            audio_path: "/accepted-hypothesis.wav".into(),
            raw_transcript: original.into(),
            ..Default::default()
        })
        .unwrap();
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: "accepted-hypothesis-segment".into(),
            model_id: "alternative-asr-hypothesis".into(),
            transcript: accepted.into(),
            confidence: Some(0.9),
        })
        .unwrap();

        db.record_human_decision("accepted-hypothesis-segment", "accept", Some(accepted), None)
            .expect("accept alternative stored hypothesis");
        let (effective_action, confirm, override_delta): (String, i64, i64) = db
            .connection()
            .query_row(
                "SELECT effect.action, contribution.confirm_delta, contribution.override_delta
                   FROM human_decision_effect_events effect
                   JOIN correction_memory_contributions contribution
                     ON contribution.effect_event_id = effect.id
                  WHERE effect.segment_id = 'accepted-hypothesis-segment'
                    AND contribution.memory_id = 'accepted-hypothesis-memory'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(effective_action, "accept", "a stored ASR hypothesis must retain machine provenance");
        assert_eq!(
            (confirm, override_delta),
            (1, 0),
            "the memory moves the prior draft exactly toward the authoritative accepted text"
        );
    }

    #[test]
    fn export_lm_corpus_includes_reviewed_nongold_excludes_gold_and_unreviewed() {
        let db = open_mem_db();
        // Reviewed, non-gold -> included (correct text from verdict_transcript).
        db.insert_segment(&SpeechSegment {
            id: "c1".into(),
            audio_path: "/x/c1.wav".into(),
            raw_transcript: "raw1".into(),
            ..Default::default()
        })
        .unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET human_decision='edit', verdict_transcript='کوردی نوێ', is_gold=0 WHERE id='c1'",
                [],
            )
            .unwrap();
        // Gold -> excluded even though reviewed.
        db.insert_segment(&SpeechSegment {
            id: "g1".into(),
            audio_path: "/x/g1.wav".into(),
            raw_transcript: "raw2".into(),
            ..Default::default()
        })
        .unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET human_decision='accept', verdict_transcript='گۆڵدی نهێنی', is_gold=1 WHERE id='g1'",
                [],
            )
            .unwrap();
        // Unreviewed (no human_decision) -> excluded.
        db.insert_segment(&SpeechSegment {
            id: "u1".into(),
            audio_path: "/x/u1.wav".into(),
            raw_transcript: "ناوەندی پشت".into(),
            ..Default::default()
        })
        .unwrap();

        let corpus = export_lm_corpus(&db).unwrap();
        assert!(corpus.iter().any(|l| l.contains("کوردی")), "reviewed non-gold text included: {corpus:?}");
        assert!(!corpus.iter().any(|l| l.contains("گۆڵد")), "gold segment excluded");
        assert!(!corpus.iter().any(|l| l.contains("ناوەند")), "unreviewed segment excluded");
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
            agreement_score: Some(0.77),
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
        super::super::insert_active_human_example_fixture(
            &db,
            "ex-learning",
            "seg-learning",
            "agent wrong text",
            "human corrected text",
            None,
        );

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
        super::super::insert_active_human_example_fixture(
            &db,
            "ex-private-path-learning",
            "seg-private-path-learning",
            "agent wrong text",
            "human corrected text",
            None,
        );

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
        super::super::insert_active_human_example_fixture(
            &db,
            "ex-stale-reference-learning",
            "seg-stale-reference-learning",
            "agent wrong text",
            "human corrected text",
            None,
        );

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
            agreement_score: Some(0.88),
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
        super::super::insert_active_human_example_fixture(
            &db,
            "ex-reference-learning",
            "seg-reference-learning",
            "agent wrong text",
            "human corrected text",
            None,
        );

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
        super::super::insert_active_human_example_fixture(
            &db,
            "ex-duplicate",
            "seg-duplicate",
            "same   text",
            "same text",
            None,
        );

        let result = build_dpo_dataset(&db).expect("build dpo");
        assert_eq!(result.pair_count, 0);
        assert!(result.jsonl.is_empty());
    }

    #[test]
    fn test_build_dpo_excludes_holdout_hash() {
        let db = open_mem_db();
        let temp = tempfile::tempdir().expect("temp dir");
        let audio_path = temp.path().join("source.wav");
        std::fs::write(&audio_path, b"holdout check audio bytes").expect("write audio");
        let audio_path_str = audio_path.to_string_lossy().to_string();

        // 1. Insert segment into speech_segments and agent_examples
        let segment = SpeechSegment {
            id: "seg-holdout".to_string(),
            audio_path: audio_path_str.clone(),
            raw_transcript: "wrong text".to_string(),
            duration_ms: 1000,
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).expect("insert segment");
        super::super::insert_active_human_example_fixture(
            &db,
            "ex-holdout",
            "seg-holdout",
            "wrong text",
            "corrected text",
            None,
        );

        // 2. Insert into gold_segments with is_holdout = 1
        db.connection()
            .execute(
                "INSERT INTO gold_segments (id, audio_path, reference, is_holdout)
                 VALUES (?1, ?2, ?3, ?4)",
                params!["gold-holdout", audio_path_str.clone(), "corrected text", 1i32],
            )
            .expect("insert gold");

        // 3. Verify that build_dpo_dataset excludes it
        let result = build_dpo_dataset(&db).expect("build DPO");
        assert_eq!(result.pair_count, 0, "should exclude segment matching holdout hash");

        // 4. Update to is_holdout = 0
        db.connection()
            .execute("UPDATE gold_segments SET is_holdout = 0 WHERE id = 'gold-holdout'", [])
            .expect("update gold");

        // 5. Verify it is now included
        let result2 = build_dpo_dataset(&db).expect("build DPO");
        assert_eq!(result2.pair_count, 1, "should include segment when is_holdout is 0");
    }

    #[test]
    fn build_dpo_excludes_holdout_even_when_audio_file_is_missing() {
        // Round-6 fail-closed regression: a held-out clip whose on-disk file has been moved/deleted
        // (routine in curation) must STILL be excluded. Previously, path.exists()==false skipped the
        // whole holdout check and the held-out pair leaked into the DPO training set.
        let db = open_mem_db();
        let temp = tempfile::tempdir().expect("temp dir");
        let audio_path = temp.path().join("source.wav");
        std::fs::write(&audio_path, b"holdout check audio bytes").expect("write audio");
        let audio_path_str = audio_path.to_string_lossy().to_string();

        let segment = SpeechSegment {
            id: "seg-holdout".to_string(),
            audio_path: audio_path_str.clone(),
            raw_transcript: "wrong text".to_string(),
            duration_ms: 1000,
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).expect("insert segment");
        super::super::insert_active_human_example_fixture(
            &db,
            "ex-holdout",
            "seg-holdout",
            "wrong text",
            "corrected text",
            None,
        );
        db.connection()
            .execute(
                "INSERT INTO gold_segments (id, audio_path, reference, is_holdout)
                 VALUES (?1, ?2, ?3, ?4)",
                params!["gold-holdout", audio_path_str.clone(), "corrected text", 1i32],
            )
            .expect("insert gold");

        // The recording is deleted on disk before export.
        std::fs::remove_file(&audio_path).expect("delete audio");

        let result = build_dpo_dataset(&db).expect("build DPO");
        assert_eq!(result.pair_count, 0, "missing file must not leak held-out content into DPO");
    }

    #[test]
    fn build_dpo_excludes_present_but_unhashable_segment_fail_closed() {
        // Round-17: the holdout content-hash guard re-reads each training clip's audio. A clip that
        // EXISTS but cannot be hashed (transient lock / permission / flaky disk) used to fall through
        // and be INCLUDED (fail-open) — and since the path guard only matches identical path strings,
        // a same-content-at-a-different-path holdout clip would then leak into training. Verify it now
        // fails CLOSED: a present-but-unhashable clip is excluded. (A directory path is present but
        // unhashable on every platform.)
        let db = open_mem_db();
        let temp = tempfile::tempdir().expect("temp dir");

        // Control: a readable audio file yields a valid DPO pair.
        let readable = temp.path().join("readable.wav");
        std::fs::write(&readable, b"some audio bytes").expect("write audio");
        let seg = SpeechSegment {
            id: "seg-x".to_string(),
            audio_path: readable.to_string_lossy().to_string(),
            raw_transcript: "wrong text".to_string(),
            duration_ms: 1000,
            ..SpeechSegment::default()
        };
        db.insert_segment(&seg).expect("insert segment");
        super::super::insert_active_human_example_fixture(&db, "ex-x", "seg-x", "wrong text", "corrected text", None);
        assert_eq!(build_dpo_dataset(&db).expect("build").pair_count, 1, "readable clip is a valid pair");

        // Repoint the segment at a present-but-unhashable path (a directory): exists() is true but
        // source_audio_identity errors, so the holdout hash cannot be cleared -> fail closed -> excluded.
        let unhashable = temp.path().join("a_dir");
        std::fs::create_dir(&unhashable).expect("mkdir");
        db.connection()
            .execute(
                "UPDATE speech_segments SET audio_path = ?1 WHERE id = 'seg-x'",
                params![unhashable.to_string_lossy().to_string()],
            )
            .expect("update path");
        assert_eq!(
            build_dpo_dataset(&db).expect("build").pair_count,
            0,
            "a present-but-unhashable clip must be excluded (fail-closed), not leaked into training"
        );
    }

    #[test]
    fn export_lm_corpus_excludes_holdout_even_when_audio_file_is_missing() {
        let db = open_mem_db();
        let temp = tempfile::tempdir().expect("temp dir");
        let audio_path = temp.path().join("lm.wav");
        std::fs::write(&audio_path, b"lm holdout audio").expect("write audio");
        let audio_path_str = audio_path.to_string_lossy().to_string();

        let segment = SpeechSegment {
            id: "seg-lm".to_string(),
            audio_path: audio_path_str.clone(),
            raw_transcript: "کوردی".to_string(),
            duration_ms: 1000,
            ..SpeechSegment::default()
        };
        db.insert_segment(&segment).expect("insert segment");
        // Mark as human-accepted so it qualifies for the LM corpus.
        db.connection()
            .execute("UPDATE speech_segments SET human_decision = 'accept' WHERE id = 'seg-lm'", [])
            .expect("set decision");
        db.connection()
            .execute(
                "INSERT INTO gold_segments (id, audio_path, reference, is_holdout)
                 VALUES (?1, ?2, ?3, ?4)",
                params!["gold-lm", audio_path_str.clone(), "کوردی", 1i32],
            )
            .expect("insert gold");

        std::fs::remove_file(&audio_path).expect("delete audio");

        let corpus = export_lm_corpus(&db).expect("export lm");
        assert!(corpus.is_empty(), "missing file must not leak held-out text into the LM corpus");
    }

    #[test]
    fn export_lm_corpus_uses_the_annotated_fix_not_the_superseded_draft() {
        // Round-24 hunt #8: the inbox-accept / legacy-accept path leaves the human's fix in
        // annotated_transcript with verdict_transcript NULL. The corpus COALESCE omitted annotated,
        // so it exported the stale ASR draft (normalized_transcript) as human-confirmed LM text.
        let db = open_mem_db();
        let seg = SpeechSegment {
            id: "c-anno".to_string(),
            audio_path: "/anno.wav".to_string(), // absent -> no holdout hashing, included
            raw_transcript: "کوردی غەڵەت".to_string(),
            normalized_transcript: Some("کوردی غەڵەت".to_string()),
            annotated_transcript: Some("کوردی ڕاست".to_string()), // the human's typed fix
            ..SpeechSegment::default()
        };
        db.insert_segment(&seg).expect("insert");
        db.connection()
            .execute("UPDATE speech_segments SET human_decision='accept' WHERE id='c-anno'", [])
            .expect("accept");

        let corpus = export_lm_corpus(&db).expect("export");
        assert_eq!(corpus.len(), 1, "the accepted segment exports one line: {corpus:?}");
        assert!(corpus[0].contains("ڕاست"), "must export the human's annotated fix: {corpus:?}");
        assert!(!corpus[0].contains("غەڵەت"), "must NOT export the superseded ASR draft: {corpus:?}");
    }

    #[test]
    fn export_lm_corpus_skips_an_asr_placeholder_draft() {
        // A placeholder ("[Pending WSL 7B ASR]" / "[ASR unavailable …]") must NEVER be emitted as
        // human-confirmed LM training text. A segment can carry human_decision='accept' while its COALESCE'd
        // draft is still a bracket-placeholder (an export taken mid-import or after a stuck-placeholder
        // incident); this LM path bypasses the training-grade gate that catches it elsewhere, so it must skip
        // placeholders itself. A real accepted Kurdish draft is the control that MUST appear.
        let db = open_mem_db();
        let good = SpeechSegment {
            id: "lm-good".to_string(),
            audio_path: "/good.wav".to_string(), // absent -> no holdout hashing, included
            raw_transcript: "کوردی".to_string(),
            ..SpeechSegment::default()
        };
        db.insert_segment(&good).expect("insert good");
        let placeholder = SpeechSegment {
            id: "lm-ph".to_string(),
            audio_path: "/ph.wav".to_string(),
            raw_transcript: "[Pending WSL 7B ASR]".to_string(),
            ..SpeechSegment::default()
        };
        db.insert_segment(&placeholder).expect("insert placeholder");
        db.connection()
            .execute("UPDATE speech_segments SET human_decision='accept' WHERE id IN ('lm-good','lm-ph')", [])
            .expect("accept both");

        let corpus = export_lm_corpus(&db).expect("export");
        assert!(corpus.iter().any(|t| t.contains("کوردی")), "a real accepted draft must be in the corpus: {corpus:?}");
        assert!(
            !corpus.iter().any(|t| t.contains("Pending") || t.contains('[')),
            "an ASR placeholder must not be emitted as human-confirmed LM text: {corpus:?}"
        );
    }

    #[test]
    fn reversed_or_shadowed_edit_effect_retracts_the_dpo_pair_without_deleting_history() {
        // Schema v60 makes human learning artifacts append-only. Their visibility follows the owning
        // effect: reversing the edit or shadowing it with a later reject retracts learning without a
        // broad DELETE from agent_examples.
        let db = open_mem_db();
        let seg = SpeechSegment {
            id: "s-retract".to_string(),
            audio_path: "/r.wav".to_string(), // absent -> not holdout, DPO-eligible
            raw_transcript: "ئەو غەڵەتە".to_string(),
            ..SpeechSegment::default()
        };
        db.insert_segment(&seg).expect("insert");

        let first_effect = super::super::insert_active_human_example_fixture(
            &db,
            "s-retract-example-1",
            "s-retract",
            "ئەو غەڵەتە",
            "ئەو ڕاستە",
            None,
        );
        assert_eq!(build_dpo_dataset(&db).unwrap().pair_count, 1, "an edit produces a DPO pair");

        db.connection()
            .execute(
                "INSERT INTO human_decision_effect_reversals(effect_event_id, operation_id)
                 VALUES (?1, 'dpo-first-edit-undo')",
                [first_effect],
            )
            .unwrap();
        assert_eq!(build_dpo_dataset(&db).unwrap().pair_count, 0, "undo must retract the learning pair");

        super::super::insert_active_human_example_fixture(
            &db,
            "s-retract-example-2",
            "s-retract",
            "ئەو غەڵەتە",
            "ئەو ڕاستە",
            None,
        );
        assert_eq!(build_dpo_dataset(&db).unwrap().pair_count, 1, "re-edit re-creates the pair");
        super::super::insert_human_effect_fixture(&db, "s-retract", "reject", None);
        assert_eq!(build_dpo_dataset(&db).unwrap().pair_count, 0, "reject must retract the learning pair");
        let physical_rows: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM agent_examples WHERE segment_id = 's-retract'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(physical_rows, 2, "effect projection must retract learning without deleting evidence history");
    }

    #[test]
    fn dpo_prompt_does_not_leak_the_human_fix_into_the_shared_prompt() {
        // record_human_decision("edit", fix) sets speech_segments.verdict_transcript = fix AND makes that same
        // fix the DPO pair's `chosen` (agent_examples.human_fix). EVERY DPO-eligible pair is edit-born, so
        // verdict_transcript == chosen for 100% of pairs. If the learning prompt echoed verdict_transcript,
        // every pair would embed its own gold label in the shared prompt — fine-tuning would learn a
        // copy-the-jury_transcript shortcut instead of ASR correction, defeating the flywheel. The prompt must
        // NOT contain the chosen fix. Uses the PRODUCTION correction path (not a hand-built fixture that
        // decouples verdict_transcript from the fix, which is why the older prompt tests missed this). (hunt-9)
        let db = open_mem_db();
        let fix = "ئەو دەقە ڕاستکراوەیە";
        let seg = SpeechSegment {
            id: "seg-no-leak".to_string(),
            audio_path: "/tmp/no-leak.wav".to_string(),
            raw_transcript: "raw asr text".to_string(),
            duration_ms: 4200,
            ..SpeechSegment::default()
        };
        db.insert_segment(&seg).expect("insert");
        super::super::insert_active_human_example_fixture(
            &db,
            "seg-no-leak-example",
            "seg-no-leak",
            "raw asr text",
            fix,
            None,
        );

        let result = build_dpo_dataset(&db).expect("build dpo");
        assert_eq!(result.pair_count, 1, "an edit produces one DPO pair");
        let pair: DpoPair = serde_json::from_str(result.jsonl.lines().next().expect("jsonl row")).expect("dpo pair");
        assert_eq!(pair.chosen, fix, "the human fix is the chosen answer");
        assert!(
            !pair.prompt.contains(fix),
            "the DPO prompt must NOT contain the chosen fix (jury_transcript label leak):\n{}",
            pair.prompt
        );
    }
}
