//! Immutable duplicate-manifest authority for the flexible review pool.
//!
//! This module owns canonical manifest parsing, frozen-pool binding, deterministic canonical
//! selection, review-authority preservation and the single durable exclusion transaction.

use super::{
    load, owner_adjudications_on, reviewer_sets, valid_lower_sha256, with_pool_full_sync, Database,
    REVIEW_POOL_DEDUP_SCHEMA_VERSION,
};
use rusqlite::OptionalExtension;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PoolDedupStatus {
    pub applied: bool,
    pub algorithm_id: Option<String>,
    pub manifest_sha256: Option<String>,
    pub source_segment_count: usize,
    pub canonical_segment_count: usize,
    pub excluded_segment_count: usize,
    pub duplicate_family_count: usize,
    pub unconfirmed_risk_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DedupManifest {
    manifest_schema: u32,
    algorithm: DedupAlgorithm,
    pool: DedupPoolIdentity,
    summary: DedupSummary,
    families: Vec<DedupFamily>,
    generated_at_ms: i64,
    manifest_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DedupAlgorithm {
    id: String,
    minimum_text_characters: u32,
    offset_tolerance_ms: i64,
    minimum_text_similarity_ppm: i64,
    audio_duration_tolerance_ms: i64,
    minimum_waveform_correlation_ppm: i64,
    comparison_sample_rate_hz: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DedupPoolIdentity {
    pool_id: String,
    source_focus_segment_count: usize,
    source_focus_sha256: String,
    champion_model_version_id: String,
    champion_deployment_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DedupSummary {
    candidate_text_groups: usize,
    cleared_repeated_text_groups: usize,
    duplicate_families: usize,
    excluded_members: usize,
    canonical_members: usize,
    unconfirmed_risk_groups: usize,
    reviewed_canonical_members: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DedupFamily {
    family_id: String,
    voice_name: String,
    canonical_segment_id: String,
    canonical_selection_reason: String,
    members: Vec<DedupMember>,
    proof_edges: Vec<DedupProofEdge>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DedupMember {
    segment_id: String,
    voice_name: String,
    source_file_name: String,
    raw_transcript_sha256: String,
    audio_content_hash: String,
    source_start_ms: i64,
    source_end_ms: i64,
    duration_ms: i64,
    review_evidence_count: usize,
    snr_milli_db: Option<i64>,
    clipping_ppm: Option<i64>,
    signal_anomaly_ppm: Option<i64>,
    confidence_ppm: Option<i64>,
    canonical: bool,
}

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DedupProofEdge {
    left_segment_id: String,
    right_segment_id: String,
    correlation_ppm: i64,
}

#[derive(Debug, Clone)]
struct DedupSelectionEvidence {
    source_file_name: String,
    snr_milli_db: Option<i64>,
    clipping_ppm: Option<i64>,
    signal_anomaly_ppm: Option<i64>,
    confidence_ppm: Option<i64>,
}

type DedupSelectionKey = (bool, Reverse<i64>, bool, i64, bool, i64, bool, Reverse<i64>, String, String);
pub(super) type RegistryDedupRow = (String, i64, String, String, String, i64, Option<String>, i64);
fn write_canonical_json(value: &serde_json::Value, output: &mut Vec<u8>) -> Result<(), String> {
    match value {
        serde_json::Value::Null => output.extend_from_slice(b"null"),
        serde_json::Value::Bool(value) => output.extend_from_slice(if *value { b"true" } else { b"false" }),
        serde_json::Value::Number(value) => output.extend_from_slice(value.to_string().as_bytes()),
        serde_json::Value::String(value) => output.extend_from_slice(
            serde_json::to_string(value)
                .map_err(|error| format!("dedup manifest string cannot be serialized: {error}"))?
                .as_bytes(),
        ),
        serde_json::Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_json(value, output)?;
            }
            output.push(b']');
        }
        serde_json::Value::Object(values) => {
            output.push(b'{');
            let mut keys: Vec<_> = values.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                output.extend_from_slice(
                    serde_json::to_string(key)
                        .map_err(|error| format!("dedup manifest key cannot be serialized: {error}"))?
                        .as_bytes(),
                );
                output.push(b':');
                write_canonical_json(&values[key], output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

pub(crate) fn canonical_json_bytes(value: &serde_json::Value) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    write_canonical_json(value, &mut output)?;
    Ok(output)
}

pub(crate) fn normalized_text_sha256(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    Sha256::digest(normalized.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn scaled(value: Option<f64>, multiplier: f64) -> Option<i64> {
    value.filter(|value| value.is_finite()).map(|value| (value * multiplier).round() as i64)
}

fn selection_key(segment_id: &str, evidence: &DedupSelectionEvidence) -> DedupSelectionKey {
    (
        evidence.snr_milli_db.is_none(),
        Reverse(evidence.snr_milli_db.unwrap_or_default()),
        evidence.clipping_ppm.is_none(),
        evidence.clipping_ppm.unwrap_or_default(),
        evidence.signal_anomaly_ppm.is_none(),
        evidence.signal_anomaly_ppm.unwrap_or_default(),
        evidence.confidence_ppm.is_none(),
        Reverse(evidence.confidence_ppm.unwrap_or_default()),
        evidence.source_file_name.to_lowercase(),
        segment_id.to_string(),
    )
}

pub(super) fn load_dedup_binding(
    db: &Database,
    pool_id: &str,
    source_count: usize,
    source_sha256: &str,
) -> Result<(PoolDedupStatus, HashSet<String>), String> {
    let schema_version = crate::migrations::get_current_version(db).map_err(|error| error.to_string())?;
    if schema_version < REVIEW_POOL_DEDUP_SCHEMA_VERSION {
        return Ok((
            PoolDedupStatus {
                applied: false,
                algorithm_id: None,
                manifest_sha256: None,
                source_segment_count: source_count,
                canonical_segment_count: source_count,
                excluded_segment_count: 0,
                duplicate_family_count: 0,
                unconfirmed_risk_count: 0,
            },
            HashSet::new(),
        ));
    }
    let manifest: Option<(String, String, i64, i64, i64, i64, i64)> = db
        .connection()
        .query_row(
            "SELECT algorithm_id, manifest_sha256, source_focus_segment_count,
                    family_count, excluded_count, canonical_count, unconfirmed_risk_count
               FROM review_pool_dedup_manifests WHERE pool_id=?1",
            [pool_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )
        .optional()
        .map_err(|error| format!("review-pool dedup manifest cannot be read: {error}"))?;
    let Some((
        algorithm_id,
        manifest_sha256,
        manifest_source_count,
        family_count,
        excluded_count,
        canonical_count,
        unconfirmed,
    )) = manifest
    else {
        let orphan_exclusions: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM review_pool_duplicate_exclusions", [], |row| row.get(0))
            .map_err(|error| format!("review-pool duplicate exclusions cannot be counted: {error}"))?;
        if orphan_exclusions != 0 {
            return Err("review-pool duplicate exclusions exist without their manifest".to_string());
        }
        return Ok((
            PoolDedupStatus {
                applied: false,
                algorithm_id: None,
                manifest_sha256: None,
                source_segment_count: source_count,
                canonical_segment_count: source_count,
                excluded_segment_count: 0,
                duplicate_family_count: 0,
                unconfirmed_risk_count: 0,
            },
            HashSet::new(),
        ));
    };
    if algorithm_id != "cortex-cross-file-waveform-correlation-v1"
        || !valid_lower_sha256(&manifest_sha256)
        || usize::try_from(manifest_source_count).ok() != Some(source_count)
        || !valid_lower_sha256(source_sha256)
        || excluded_count < 0
        || canonical_count < 1
        || family_count < 0
        || unconfirmed != 0
        || manifest_source_count != excluded_count + canonical_count
    {
        return Err("review-pool dedup manifest has invalid summary authority".to_string());
    }
    let stored_source_sha256: String = db
        .connection()
        .query_row("SELECT source_focus_sha256 FROM review_pool_dedup_manifests WHERE pool_id=?1", [pool_id], |row| {
            row.get(0)
        })
        .map_err(|error| format!("review-pool dedup source digest cannot be read: {error}"))?;
    if stored_source_sha256 != source_sha256 {
        return Err("review-pool dedup manifest belongs to another source-pool digest".to_string());
    }
    let mut statement = db
        .connection()
        .prepare(
            "SELECT exclusion.segment_id
               FROM review_pool_duplicate_exclusions exclusion
               JOIN review_pool_members member
                 ON member.pool_id=exclusion.pool_id AND member.segment_id=exclusion.segment_id
               JOIN review_pool_members canonical
                 ON canonical.pool_id=exclusion.pool_id
                AND canonical.segment_id=exclusion.canonical_segment_id
              WHERE exclusion.pool_id=?1
                AND member.voice_name=canonical.voice_name COLLATE BINARY
                AND NOT EXISTS (
                    SELECT 1 FROM review_pool_duplicate_exclusions nested
                     WHERE nested.pool_id=exclusion.pool_id
                       AND nested.segment_id=exclusion.canonical_segment_id
                )
              ORDER BY exclusion.segment_id",
        )
        .map_err(|error| format!("review-pool duplicate exclusions cannot be prepared: {error}"))?;
    let excluded: HashSet<String> = statement
        .query_map([pool_id], |row| row.get(0))
        .map_err(|error| format!("review-pool duplicate exclusions cannot be read: {error}"))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("review-pool duplicate exclusion is unreadable: {error}"))?;
    if usize::try_from(excluded_count).ok() != Some(excluded.len())
        || usize::try_from(canonical_count).ok() != source_count.checked_sub(excluded.len())
    {
        return Err("review-pool duplicate exclusions do not match their manifest summary".to_string());
    }
    let excluded_with_authority: bool = db
        .connection()
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                 JOIN speech_segments segment ON segment.id=exclusion.segment_id
                WHERE segment.verified=1
                  AND segment.human_decision IN
                      ('accept','edit','reject','human_accept','human_edit','human_reject')
             ) OR EXISTS(
                 SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                 JOIN effective_review_pool_decisions_v62 decision
                   ON decision.pool_id=exclusion.pool_id AND decision.segment_id=exclusion.segment_id
             ) OR EXISTS(
                 SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                 JOIN effective_independent_review_decisions_v61 decision
                   ON decision.segment_id=exclusion.segment_id
             ) OR EXISTS(
                 SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                 JOIN review_pool_owner_adjudications adjudication
                   ON adjudication.pool_id=exclusion.pool_id AND adjudication.segment_id=exclusion.segment_id
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("review-pool excluded authority cannot be checked: {error}"))?;
    if excluded_with_authority {
        return Err("review-pool duplicate exclusion would discard existing review authority".to_string());
    }
    Ok((
        PoolDedupStatus {
            applied: true,
            algorithm_id: Some(algorithm_id),
            manifest_sha256: Some(manifest_sha256),
            source_segment_count: source_count,
            canonical_segment_count: usize::try_from(canonical_count).unwrap_or(usize::MAX),
            excluded_segment_count: excluded.len(),
            duplicate_family_count: usize::try_from(family_count).unwrap_or(usize::MAX),
            unconfirmed_risk_count: 0,
        },
        excluded,
    ))
}

pub fn dedup_status(db: &Database) -> Result<PoolDedupStatus, String> {
    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    let (status, _) = load_dedup_binding(db, &pool.pool_id, pool.focus_segment_count, &pool.focus_sha256)?;
    Ok(status)
}

pub fn apply_dedup_manifest(db: &Database, manifest_json: &str) -> Result<PoolDedupStatus, String> {
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < REVIEW_POOL_DEDUP_SCHEMA_VERSION
    {
        return Err("review-pool duplicate exclusions require schema 64".to_string());
    }
    let mut manifest_value: serde_json::Value = serde_json::from_str(manifest_json)
        .map_err(|error| format!("review-pool dedup manifest JSON is invalid: {error}"))?;
    let claimed_sha256 = manifest_value
        .get("manifestSha256")
        .and_then(serde_json::Value::as_str)
        .filter(|value| valid_lower_sha256(value))
        .ok_or_else(|| "review-pool dedup manifest has no valid payload digest".to_string())?
        .to_string();
    manifest_value
        .as_object_mut()
        .ok_or_else(|| "review-pool dedup manifest root must be an object".to_string())?
        .remove("manifestSha256");
    let actual_sha256: String =
        Sha256::digest(canonical_json_bytes(&manifest_value)?).iter().map(|byte| format!("{byte:02x}")).collect();
    if actual_sha256 != claimed_sha256 {
        return Err("review-pool dedup manifest payload does not match its digest".to_string());
    }
    manifest_value
        .as_object_mut()
        .ok_or_else(|| "review-pool dedup manifest root changed while hashing".to_string())?
        .insert("manifestSha256".to_string(), serde_json::Value::String(claimed_sha256.clone()));
    let canonical_manifest = String::from_utf8(canonical_json_bytes(&manifest_value)?)
        .map_err(|_| "review-pool dedup manifest is not canonical UTF-8".to_string())?;
    let manifest: DedupManifest = serde_json::from_value(manifest_value)
        .map_err(|error| format!("review-pool dedup manifest contract is invalid: {error}"))?;
    if manifest.manifest_sha256 != claimed_sha256 {
        return Err("review-pool dedup manifest digest field changed while parsing".to_string());
    }

    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    let existing: Option<String> = db
        .connection()
        .query_row("SELECT manifest_sha256 FROM review_pool_dedup_manifests WHERE pool_id=?1", [&pool.pool_id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|error| format!("existing review-pool dedup manifest cannot be read: {error}"))?;
    if let Some(existing) = existing {
        return if existing == claimed_sha256 {
            dedup_status(db)
        } else {
            Err("active review pool already has a different immutable dedup manifest".to_string())
        };
    }
    if manifest.manifest_schema != 1
        || manifest.generated_at_ms <= 0
        || manifest.algorithm.id != "cortex-cross-file-waveform-correlation-v1"
        || manifest.algorithm.minimum_text_characters != 25
        || manifest.algorithm.offset_tolerance_ms != 500
        || manifest.algorithm.minimum_text_similarity_ppm != 900_000
        || manifest.algorithm.audio_duration_tolerance_ms != 120
        || manifest.algorithm.minimum_waveform_correlation_ppm != 980_000
        || manifest.algorithm.comparison_sample_rate_hz != 16_000
        || manifest.pool.pool_id != pool.pool_id
        || manifest.pool.source_focus_segment_count != pool.focus_segment_count
        || manifest.pool.source_focus_sha256 != pool.focus_sha256
        || manifest.pool.champion_model_version_id != pool.champion_model_version_id
        || manifest.pool.champion_deployment_sha256 != pool.champion_deployment_sha256
        || manifest.summary.unconfirmed_risk_groups != 0
        || manifest.summary.duplicate_families != manifest.families.len()
        // One transcript-candidate group can split into several disconnected waveform families.
        // Therefore family count is not bounded by candidate-group count; only the number of groups
        // cleared as harmless repeated text must fit inside the original candidate population.
        || manifest.summary.candidate_text_groups < manifest.summary.cleared_repeated_text_groups
        || manifest.summary.canonical_members + manifest.summary.excluded_members != pool.focus_segment_count
    {
        return Err("review-pool dedup manifest does not match the frozen pool or algorithm canon".to_string());
    }
    let certificate_count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM review_pool_voice_certificates", [], |row| row.get(0))
        .map_err(|error| format!("review-pool certificates cannot be counted: {error}"))?;
    if certificate_count != 0 {
        return Err("duplicate exclusions cannot be applied after a voice certificate exists".to_string());
    }

    let reviewers = reviewer_sets(db)?;
    let adjudications = owner_adjudications_on(db.connection())?;
    let mut selection_statement = db
        .connection()
        .prepare(
            "SELECT id, audio_path, snr_db, clipping_ratio, signal_anomaly_score, confidence
               FROM speech_segments
              WHERE EXISTS (SELECT 1 FROM review_pool_members member WHERE member.segment_id=id)",
        )
        .map_err(|error| format!("dedup selection evidence cannot be prepared: {error}"))?;
    let selection_rows = selection_statement
        .query_map([], |row| {
            let path: String = row.get(1)?;
            Ok((
                row.get::<_, String>(0)?,
                DedupSelectionEvidence {
                    source_file_name: Path::new(&path)
                        .file_name()
                        .map(|value| value.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    snr_milli_db: scaled(row.get(2)?, 1_000.0),
                    clipping_ppm: scaled(row.get(3)?, 1_000_000.0),
                    signal_anomaly_ppm: scaled(row.get(4)?, 1_000_000.0),
                    confidence_ppm: scaled(row.get(5)?, 1_000_000.0),
                },
            ))
        })
        .map_err(|error| format!("dedup selection evidence cannot be read: {error}"))?;
    let selection: HashMap<String, DedupSelectionEvidence> = selection_rows
        .collect::<Result<_, _>>()
        .map_err(|error| format!("dedup selection evidence is unreadable: {error}"))?;

    let mut all_family_members = HashSet::new();
    let mut exclusions: Vec<(String, String, String)> = Vec::new();
    let mut reviewed_canonical_members = 0usize;
    for family in &manifest.families {
        if !valid_lower_sha256(&family.family_id) || family.members.len() < 2 || family.voice_name.trim().is_empty() {
            return Err("review-pool dedup family has invalid identity or cardinality".to_string());
        }
        let mut segment_ids: Vec<String> = family.members.iter().map(|member| member.segment_id.clone()).collect();
        segment_ids.sort_unstable();
        if segment_ids.windows(2).any(|window| window[0] == window[1])
            || !segment_ids.iter().all(|segment_id| all_family_members.insert(segment_id.clone()))
        {
            return Err("review-pool dedup families contain duplicate segment membership".to_string());
        }
        let member_ids: HashSet<_> = segment_ids.iter().cloned().collect();
        let canonical_flags: Vec<_> = family.members.iter().filter(|member| member.canonical).collect();
        if canonical_flags.len() != 1 || canonical_flags[0].segment_id != family.canonical_segment_id {
            return Err(format!("dedup family {} has ambiguous canonical membership", family.family_id));
        }
        let mut actual_reviewed = Vec::new();
        for member in &family.members {
            let frozen = pool
                .members
                .get(&member.segment_id)
                .ok_or_else(|| format!("dedup member {} is outside the active source pool", member.segment_id))?;
            let selection_evidence = selection
                .get(&member.segment_id)
                .ok_or_else(|| format!("dedup member {} has no selection evidence", member.segment_id))?;
            let review_count = reviewers.get(&member.segment_id).map_or(0, |value| value.judged.len())
                + adjudications.get(&member.segment_id).map_or(0, Vec::len);
            if member.voice_name != family.voice_name
                || frozen.voice_name != family.voice_name
                || member.raw_transcript_sha256 != normalized_text_sha256(&frozen.raw_transcript)
                || member.audio_content_hash != frozen.audio_content_hash
                || member.source_start_ms != frozen.source_start_ms
                || member.source_end_ms != frozen.source_end_ms
                || member.duration_ms != frozen.duration_ms
                || member.review_evidence_count != review_count
                || member.source_file_name != selection_evidence.source_file_name
                || member.snr_milli_db != selection_evidence.snr_milli_db
                || member.clipping_ppm != selection_evidence.clipping_ppm
                || member.signal_anomaly_ppm != selection_evidence.signal_anomaly_ppm
                || member.confidence_ppm != selection_evidence.confidence_ppm
            {
                return Err(format!("dedup member {} does not match frozen pool evidence", member.segment_id));
            }
            if review_count != 0 {
                actual_reviewed.push(member.segment_id.as_str());
            }
        }
        if actual_reviewed.len() > 1 {
            return Err(format!("dedup family {} would retire more than one reviewed clip", family.family_id));
        }
        if let Some(reviewed) = actual_reviewed.first() {
            if *reviewed != family.canonical_segment_id
                || family.canonical_selection_reason != "preserve-human-review-evidence"
            {
                return Err(format!("dedup family {} does not preserve its reviewed member", family.family_id));
            }
            reviewed_canonical_members += 1;
        } else {
            let mut ranked_members = Vec::with_capacity(segment_ids.len());
            for segment_id in &segment_ids {
                let evidence = selection
                    .get(segment_id.as_str())
                    .ok_or_else(|| format!("dedup member {segment_id} lost its validated selection evidence"))?;
                ranked_members.push((selection_key(segment_id, evidence), segment_id));
            }
            let expected = ranked_members
                .iter()
                .min_by_key(|(key, _)| key)
                .map(|(_, segment_id)| *segment_id)
                .ok_or_else(|| format!("dedup family {} has no selectable member", family.family_id))?;
            if *expected != family.canonical_segment_id
                || family.canonical_selection_reason != "best-measured-audio-quality-then-stable-identity"
            {
                return Err(format!("dedup family {} canonical selection is not deterministic", family.family_id));
            }
        }

        let edge_order: Vec<_> = family
            .proof_edges
            .iter()
            .map(|edge| (edge.left_segment_id.as_str(), edge.right_segment_id.as_str()))
            .collect();
        let mut sorted_edge_order = edge_order.clone();
        sorted_edge_order.sort_unstable();
        if edge_order != sorted_edge_order {
            return Err(format!("dedup family {} proof edges are not canonical-order", family.family_id));
        }
        let index: HashMap<_, _> = segment_ids.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
        let mut parent: Vec<usize> = (0..segment_ids.len()).collect();
        fn find(parent: &mut [usize], mut index: usize) -> usize {
            while parent[index] != index {
                parent[index] = parent[parent[index]];
                index = parent[index];
            }
            index
        }
        for edge in &family.proof_edges {
            if edge.left_segment_id == edge.right_segment_id
                || !member_ids.contains(&edge.left_segment_id)
                || !member_ids.contains(&edge.right_segment_id)
                || !(980_000..=1_000_001).contains(&edge.correlation_ppm)
            {
                return Err(format!("dedup family {} has invalid waveform proof", family.family_id));
            }
            let left = index[edge.left_segment_id.as_str()];
            let right = index[edge.right_segment_id.as_str()];
            let left_root = find(&mut parent, left);
            let right_root = find(&mut parent, right);
            if left_root != right_root {
                parent[right_root] = left_root;
            }
        }
        let root = find(&mut parent, 0);
        if (1..segment_ids.len()).any(|index| find(&mut parent, index) != root) {
            return Err(format!("dedup family {} waveform proof is disconnected", family.family_id));
        }
        let family_material = serde_json::json!({
            "poolId": &pool.pool_id,
            "proofEdges": &family.proof_edges,
            "segmentIds": &segment_ids,
        });
        let actual_family_id: String =
            Sha256::digest(canonical_json_bytes(&family_material)?).iter().map(|byte| format!("{byte:02x}")).collect();
        if actual_family_id != family.family_id {
            return Err(format!("dedup family {} does not match its proof digest", family.family_id));
        }
        for member in &family.members {
            if !member.canonical {
                if member.review_evidence_count != 0 {
                    return Err(format!("dedup exclusion {} has review evidence", member.segment_id));
                }
                exclusions.push((
                    member.segment_id.clone(),
                    family.canonical_segment_id.clone(),
                    family.family_id.clone(),
                ));
            }
        }
    }
    exclusions.sort_unstable();
    if exclusions.len() != manifest.summary.excluded_members
        || pool.focus_segment_count - exclusions.len() != manifest.summary.canonical_members
        || reviewed_canonical_members != manifest.summary.reviewed_canonical_members
    {
        return Err("review-pool dedup manifest summary does not match validated families".to_string());
    }

    with_pool_full_sync(db, || {
        let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| format!("review-pool dedup application cannot lock the database: {error}"))?;
        tx.execute(
            "INSERT INTO review_pool_dedup_manifests
                (pool_id, source_focus_segment_count, source_focus_sha256, algorithm_id,
                 family_count, excluded_count, canonical_count, unconfirmed_risk_count,
                 manifest_json, manifest_sha256, app_git_sha, created_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                &pool.pool_id,
                i64::try_from(pool.focus_segment_count).map_err(|_| "source pool is too large".to_string())?,
                &pool.focus_sha256,
                &manifest.algorithm.id,
                i64::try_from(manifest.summary.duplicate_families)
                    .map_err(|_| "duplicate family count is too large".to_string())?,
                i64::try_from(exclusions.len()).map_err(|_| "duplicate exclusion count is too large".to_string())?,
                i64::try_from(manifest.summary.canonical_members)
                    .map_err(|_| "canonical member count is too large".to_string())?,
                &canonical_manifest,
                &claimed_sha256,
                crate::GIT_SHA,
                manifest.generated_at_ms,
            ],
        )
        .map_err(|error| format!("review-pool dedup manifest cannot be committed: {error}"))?;
        {
            let mut statement = tx
                .prepare(
                    "INSERT INTO review_pool_duplicate_exclusions
                        (pool_id, segment_id, canonical_segment_id, family_id, created_at_ms)
                     VALUES(?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(|error| format!("review-pool duplicate exclusion writer cannot be prepared: {error}"))?;
            for (segment_id, canonical_segment_id, family_id) in &exclusions {
                statement
                    .execute(rusqlite::params![
                        &pool.pool_id,
                        segment_id,
                        canonical_segment_id,
                        family_id,
                        manifest.generated_at_ms,
                    ])
                    .map_err(|error| format!("duplicate exclusion {segment_id} cannot be committed: {error}"))?;
            }
        }
        let committed: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM review_pool_duplicate_exclusions WHERE pool_id=?1",
                [&pool.pool_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("committed duplicate exclusions cannot be counted: {error}"))?;
        if usize::try_from(committed).ok() != Some(exclusions.len()) {
            return Err("review-pool duplicate exclusion transaction is incomplete".to_string());
        }
        tx.commit().map_err(|error| format!("review-pool dedup application cannot commit: {error}"))?;
        Ok(())
    })?;
    dedup_status(db)
}
