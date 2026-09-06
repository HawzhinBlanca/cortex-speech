//! Immutable duplicate-manifest authority for the flexible review pool.
//!
//! This module owns canonical manifest parsing, frozen-pool binding, deterministic canonical
//! selection, review-authority preservation and the durable exclusion transactions: the one v1 base
//! manifest (schema 64) and the append-only chain of superseding v2 manifests (schema 70).
//!
//! Why v2 exists (measured 2026-09-06 on the live pool): the v1 verdict compared two waveforms at
//! zero lag with a 0.98 bar. A twin cut a few milliseconds differently scored near zero and was
//! cleared as "the same sentence read twice". At the best lag inside 1.5 s, 88% of 8,909 text-matched
//! cross-file pairs were classified as candidates while same-voice different-sentence controls never
//! exceeded 0.2. A superseding manifest states the COMPLETE dedup state (every applied family
//! reproduced, new ones appended), may retire a reviewed twin (its evidence stays durable; the clip
//! leaves serving, resolution, certification and export). Applied exclusion rows remain immutable;
//! applied canonicals can retire through explicit chains. New exclusions additionally require
//! independent full-clip PCM verification; historical correlation claims alone are insufficient.

use super::{
    load, owner_adjudications_on, reviewer_sets, valid_lower_sha256, with_pool_full_sync, Database,
    REVIEW_POOL_DEDUP_SCHEMA_VERSION, REVIEW_POOL_DEDUP_SUPERSESSION_SCHEMA_VERSION,
};
use rusqlite::OptionalExtension;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub const DEDUP_ALGORITHM_V1: &str = "cortex-cross-file-waveform-correlation-v1";
pub const DEDUP_ALGORITHM_V2: &str = "cortex-cross-file-waveform-correlation-v2";

/// A manifest correlation is candidate evidence, not authority to discard unmatched words. New
/// exclusions must independently prove COMPLETE decoded clip identity. Existing immutable rows
/// and exact manifest retries remain readable; they are never silently rewritten or repriced.
fn require_complete_clip_identity(db: &Database, exclusions: &[(String, String, String)]) -> Result<(), String> {
    let mut identities: HashMap<String, (usize, Vec<u8>)> = HashMap::new();
    for (member, canonical, _) in exclusions {
        for id in [member, canonical] {
            if identities.contains_key(id) {
                continue;
            }
            let segment = db
                .get_segment_by_id(id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("dedup audio member {id} is missing"))?;
            let current_hash = crate::export_bundle::current_canonical_pcm_blake3(Path::new(&segment.audio_path))
                .map_err(|error| format!("dedup source identity cannot be verified for {id}: {error}"))?;
            if db.segment_audio_content_hash(id).map_err(|error| error.to_string())?.as_deref()
                != Some(current_hash.as_str())
            {
                return Err(format!("dedup source audio changed for {id}"));
            }
            let wav = crate::agentic::segment_audio_as_wav_bytes(&segment).map_err(|error| error.to_string())?;
            let mut reader = hound::WavReader::new(std::io::Cursor::new(wav)).map_err(|error| error.to_string())?;
            let samples: Vec<i16> =
                reader.samples::<i16>().collect::<Result<_, _>>().map_err(|error| error.to_string())?;
            let first = *samples.first().ok_or_else(|| format!("dedup clip {id} is empty"))? as i32;
            let mut hash = Sha256::new();
            for sample in &samples {
                hash.update((i32::from(*sample) - first).to_le_bytes());
            }
            identities.insert(id.clone(), (samples.len(), hash.finalize().to_vec()));
        }
        if identities[member] != identities[canonical] {
            return Err(format!("E_DEDUP_PARTIAL_CONTENT: {member} and {canonical} lack complete PCM equivalence; retain both for review"));
        }
    }
    Ok(())
}
/// v2 waveform verdict, pinned exactly as `scripts/check_dataset_duplicates.py` measures it: best lag
/// inside ±1.5 s, at least 60% of the shorter clip overlapping, candidate correlation ≥ 0.40.
/// These preserve the manifest wire contract; complete-clip verification above is additional authority.
const V2_MINIMUM_WAVEFORM_CORRELATION_PPM: i64 = 400_000;
const V2_PROBABLE_WAVEFORM_CORRELATION_PPM: i64 = 200_000;
const V2_MAXIMUM_LAG_MS: i64 = 1_500;
const V2_AUDIO_DURATION_TOLERANCE_MS: i64 = 1_500;
const V2_MINIMUM_OVERLAP_PPM: i64 = 600_000;
const V2_TEXT_CANDIDATE_SIMILARITY_PPM: i64 = 700_000;

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
    /// How many superseding (v2) manifests sit on top of the v1 base. 0 on a v1-only pool.
    pub supersession_count: usize,
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
    /// Schema 2 only: the exact dedup authority this manifest replaces (v1 base or latest v2).
    #[serde(default)]
    supersedes: Option<DedupSupersedes>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DedupSupersedes {
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
    // Schema 2 additions.
    #[serde(default)]
    maximum_lag_ms: Option<i64>,
    #[serde(default)]
    minimum_overlap_ppm: Option<i64>,
    #[serde(default)]
    text_candidate_similarity_ppm: Option<i64>,
    #[serde(default)]
    probable_waveform_correlation_ppm: Option<i64>,
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
    // Schema 2 additions: exclusions this manifest introduces on top of the authority it supersedes,
    // retired members that carry review evidence, and the surfaced-not-excluded observations.
    #[serde(default)]
    newly_excluded_members: Option<usize>,
    #[serde(default)]
    excluded_reviewed_members: Option<usize>,
    #[serde(default)]
    probable_duplicate_pairs: Option<usize>,
    #[serde(default)]
    cross_voice_risk_groups: Option<usize>,
    #[serde(default)]
    retired_applied_canonicals: Option<usize>,
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
    // Schema 2 additions. Skipped when absent so v1 family digests stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lag_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    overlap_ppm: Option<i64>,
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
                supersession_count: 0,
            },
            HashSet::new(),
        ));
    }
    let supersessions_available = schema_version >= REVIEW_POOL_DEDUP_SUPERSESSION_SCHEMA_VERSION;
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
                supersession_count: 0,
            },
            HashSet::new(),
        ));
    };
    let (mut algorithm_id, mut manifest_sha256, mut family_count, mut excluded_count, mut canonical_count) =
        (algorithm_id, manifest_sha256, family_count, excluded_count, canonical_count);
    if algorithm_id != DEDUP_ALGORITHM_V1 {
        return Err("review-pool dedup base manifest has an unknown algorithm".to_string());
    }
    // The latest superseding manifest, when one exists, IS the dedup authority: it restates every
    // applied family and adds its own, so its counts describe the whole pool.
    let mut supersession_count = 0usize;
    if supersessions_available {
        let latest: Option<(String, String, i64, i64, i64, i64)> = db
            .connection()
            .query_row(
                "SELECT algorithm_id, manifest_sha256, family_count, excluded_count, canonical_count, sequence
                   FROM review_pool_dedup_supersessions WHERE pool_id=?1
                  ORDER BY sequence DESC LIMIT 1",
                [pool_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .optional()
            .map_err(|error| format!("review-pool dedup supersessions cannot be read: {error}"))?;
        if let Some((latest_algorithm, latest_sha256, latest_families, latest_excluded, latest_canonical, sequence)) =
            latest
        {
            if latest_algorithm != DEDUP_ALGORITHM_V2 || sequence < 1 {
                return Err("review-pool dedup supersession has an unknown algorithm".to_string());
            }
            algorithm_id = latest_algorithm;
            manifest_sha256 = latest_sha256;
            family_count = latest_families;
            excluded_count = latest_excluded;
            canonical_count = latest_canonical;
            supersession_count = usize::try_from(sequence).unwrap_or(usize::MAX);
        }
    }
    if !valid_lower_sha256(&manifest_sha256)
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
            "SELECT exclusion.segment_id, exclusion.canonical_segment_id
               FROM review_pool_duplicate_exclusions exclusion
               JOIN review_pool_members member
                 ON member.pool_id=exclusion.pool_id AND member.segment_id=exclusion.segment_id
               JOIN review_pool_members canonical
                 ON canonical.pool_id=exclusion.pool_id
                AND canonical.segment_id=exclusion.canonical_segment_id
              WHERE exclusion.pool_id=?1
                AND member.voice_name=canonical.voice_name COLLATE BINARY
              ORDER BY exclusion.segment_id",
        )
        .map_err(|error| format!("review-pool duplicate exclusions cannot be prepared: {error}"))?;
    let chain: HashMap<String, String> = statement
        .query_map([pool_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|error| format!("review-pool duplicate exclusions cannot be read: {error}"))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("review-pool duplicate exclusion is unreadable: {error}"))?;
    // A superseding manifest may retire an applied canonical under a better twin, so an exclusion's
    // canonical may itself be excluded. Every chain must still end at a LIVE root without cycling;
    // otherwise the pool would hold a family with no canonical at all.
    for (segment_id, mut canonical) in chain.iter().map(|(segment, canonical)| (segment, canonical.clone())) {
        let mut hops = 0usize;
        while let Some(next) = chain.get(&canonical) {
            hops += 1;
            if hops > chain.len() {
                return Err(format!("review-pool duplicate exclusion {segment_id} has a cyclic canonical chain"));
            }
            canonical = next.clone();
        }
    }
    let excluded: HashSet<String> = chain.keys().cloned().collect();
    if usize::try_from(excluded_count).ok() != Some(excluded.len())
        || usize::try_from(canonical_count).ok() != source_count.checked_sub(excluded.len())
    {
        return Err("review-pool duplicate exclusions do not match their manifest summary".to_string());
    }
    // A v1 base exclusion may never carry review authority. A superseding (v2) exclusion may — that
    // is the point of supersession — but only when the manifest it names is a durable supersession
    // row; an exclusion claiming an unknown manifest is a forged retirement and fails reopen.
    let base_only = if supersessions_available { "AND exclusion.manifest_sha256=''" } else { "" };
    let excluded_with_authority: bool = db
        .connection()
        .query_row(
            &format!(
                "SELECT EXISTS(
                     SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                     JOIN speech_segments segment ON segment.id=exclusion.segment_id
                    WHERE segment.verified=1 {base_only}
                      AND segment.human_decision IN
                          ('accept','edit','reject','human_accept','human_edit','human_reject')
                 ) OR EXISTS(
                     SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                     JOIN effective_review_pool_decisions_v62 decision
                       ON decision.pool_id=exclusion.pool_id AND decision.segment_id=exclusion.segment_id
                    WHERE 1 {base_only}
                 ) OR EXISTS(
                     SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                     JOIN effective_independent_review_decisions_v61 decision
                       ON decision.segment_id=exclusion.segment_id
                    WHERE 1 {base_only}
                 ) OR EXISTS(
                     SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                     JOIN review_pool_owner_adjudications adjudication
                       ON adjudication.pool_id=exclusion.pool_id AND adjudication.segment_id=exclusion.segment_id
                    WHERE 1 {base_only}
                 )"
            ),
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("review-pool excluded authority cannot be checked: {error}"))?;
    if excluded_with_authority {
        return Err("review-pool duplicate exclusion would discard existing review authority".to_string());
    }
    if supersessions_available {
        let forged: bool = db
            .connection()
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM review_pool_duplicate_exclusions exclusion
                    WHERE exclusion.pool_id=?1 AND exclusion.manifest_sha256 <> ''
                      AND NOT EXISTS (
                          SELECT 1 FROM review_pool_dedup_supersessions supersession
                           WHERE supersession.pool_id=exclusion.pool_id
                             AND supersession.manifest_sha256=exclusion.manifest_sha256
                      )
                 )",
                [pool_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("review-pool superseding exclusions cannot be checked: {error}"))?;
        if forged {
            return Err(
                "review-pool duplicate exclusion names a superseding manifest that was never applied".to_string()
            );
        }
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
            supersession_count,
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
    if manifest.manifest_schema == 2 {
        return apply_superseding_manifest(db, &manifest, &canonical_manifest, &claimed_sha256);
    }
    if manifest.supersedes.is_some() {
        return Err("review-pool dedup manifest schema 1 cannot supersede another manifest".to_string());
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
        || manifest.algorithm.id != DEDUP_ALGORITHM_V1
        || manifest.algorithm.maximum_lag_ms.is_some()
        || manifest.algorithm.minimum_overlap_ppm.is_some()
        || manifest.algorithm.text_candidate_similarity_ppm.is_some()
        || manifest.algorithm.probable_waveform_correlation_ppm.is_some()
        || manifest.summary.newly_excluded_members.is_some()
        || manifest.summary.excluded_reviewed_members.is_some()
        || manifest.summary.probable_duplicate_pairs.is_some()
        || manifest.summary.cross_voice_risk_groups.is_some()
        || manifest.summary.retired_applied_canonicals.is_some()
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
    let selection = selection_evidence(db)?;

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
                || edge.lag_ms.is_some()
                || edge.overlap_ppm.is_some()
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

    require_complete_clip_identity(db, &exclusions)?;

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

/// Selection evidence for every pool member, keyed by clip id (shared by the v1 and v2 validators).
fn selection_evidence(db: &Database) -> Result<HashMap<String, DedupSelectionEvidence>, String> {
    let mut statement = db
        .connection()
        .prepare(
            "SELECT id, audio_path, snr_db, clipping_ratio, signal_anomaly_score, confidence
               FROM speech_segments
              WHERE EXISTS (SELECT 1 FROM review_pool_members member WHERE member.segment_id=id)",
        )
        .map_err(|error| format!("dedup selection evidence cannot be prepared: {error}"))?;
    let rows = statement
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
    rows.collect::<Result<_, _>>().map_err(|error| format!("dedup selection evidence is unreadable: {error}"))
}

/// Apply a schema-2 superseding manifest on top of the current dedup authority (schema 70+).
///
/// The manifest must restate every applied family exactly (same excluded clips under the same
/// canonical), may add members to an applied family and may add whole new families. It may retire a
/// clip that carries review evidence — the evidence stays durable and the clip only leaves serving
/// and export — but it can never retire an applied canonical or move an applied exclusion. Canonical
/// selection is deterministic and re-derived here from the frozen pool, so the manifest can only
/// state what the database itself proves.
fn apply_superseding_manifest(
    db: &Database,
    manifest: &DedupManifest,
    canonical_manifest: &str,
    claimed_sha256: &str,
) -> Result<PoolDedupStatus, String> {
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())?
        < REVIEW_POOL_DEDUP_SUPERSESSION_SCHEMA_VERSION
    {
        return Err("superseding review-pool dedup manifests require schema 70".to_string());
    }
    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    let base_sha256: Option<String> = db
        .connection()
        .query_row("SELECT manifest_sha256 FROM review_pool_dedup_manifests WHERE pool_id=?1", [&pool.pool_id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|error| format!("existing review-pool dedup manifest cannot be read: {error}"))?;
    let Some(base_sha256) = base_sha256 else {
        return Err("a superseding dedup manifest requires an applied base manifest".to_string());
    };
    let latest: Option<(String, i64)> = db
        .connection()
        .query_row(
            "SELECT manifest_sha256, sequence FROM review_pool_dedup_supersessions
              WHERE pool_id=?1 ORDER BY sequence DESC LIMIT 1",
            [&pool.pool_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| format!("review-pool dedup supersessions cannot be read: {error}"))?;
    let (current_sha256, next_sequence) = match latest {
        Some((sha256, sequence)) => (sha256, sequence + 1),
        None => (base_sha256, 1),
    };
    if current_sha256 == claimed_sha256 {
        return dedup_status(db);
    }
    let supersedes = manifest
        .supersedes
        .as_ref()
        .ok_or_else(|| "superseding dedup manifest names no authority to supersede".to_string())?;
    if supersedes.manifest_sha256 != current_sha256 {
        return Err("superseding dedup manifest does not supersede the current dedup authority".to_string());
    }
    let algorithm = &manifest.algorithm;
    if manifest.generated_at_ms <= 0
        || algorithm.id != DEDUP_ALGORITHM_V2
        || algorithm.minimum_text_characters != 25
        || algorithm.offset_tolerance_ms != 500
        || algorithm.minimum_text_similarity_ppm != 900_000
        || algorithm.audio_duration_tolerance_ms != V2_AUDIO_DURATION_TOLERANCE_MS
        || algorithm.minimum_waveform_correlation_ppm != V2_MINIMUM_WAVEFORM_CORRELATION_PPM
        || algorithm.comparison_sample_rate_hz != 16_000
        || algorithm.maximum_lag_ms != Some(V2_MAXIMUM_LAG_MS)
        || algorithm.minimum_overlap_ppm != Some(V2_MINIMUM_OVERLAP_PPM)
        || algorithm.text_candidate_similarity_ppm != Some(V2_TEXT_CANDIDATE_SIMILARITY_PPM)
        || algorithm.probable_waveform_correlation_ppm != Some(V2_PROBABLE_WAVEFORM_CORRELATION_PPM)
        || manifest.pool.pool_id != pool.pool_id
        || manifest.pool.source_focus_segment_count != pool.focus_segment_count
        || manifest.pool.source_focus_sha256 != pool.focus_sha256
        || manifest.pool.champion_model_version_id != pool.champion_model_version_id
        || manifest.pool.champion_deployment_sha256 != pool.champion_deployment_sha256
        || manifest.summary.unconfirmed_risk_groups != 0
        || manifest.summary.duplicate_families != manifest.families.len()
        || manifest.summary.candidate_text_groups < manifest.summary.cleared_repeated_text_groups
        || manifest.summary.canonical_members + manifest.summary.excluded_members != pool.focus_segment_count
        || manifest.summary.newly_excluded_members.is_none()
        || manifest.summary.excluded_reviewed_members.is_none()
        || manifest.summary.probable_duplicate_pairs.is_none()
        || manifest.summary.cross_voice_risk_groups.is_none()
        || manifest.summary.retired_applied_canonicals.is_none()
    {
        return Err("superseding dedup manifest does not match the frozen pool or the v2 algorithm canon".to_string());
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
    let selection = selection_evidence(db)?;
    let mut applied_statement = db
        .connection()
        .prepare("SELECT segment_id, canonical_segment_id FROM review_pool_duplicate_exclusions WHERE pool_id=?1")
        .map_err(|error| format!("applied duplicate exclusions cannot be prepared: {error}"))?;
    let applied: HashMap<String, String> = applied_statement
        .query_map([&pool.pool_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|error| format!("applied duplicate exclusions cannot be read: {error}"))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("applied duplicate exclusion is unreadable: {error}"))?;
    let applied_canonicals: HashSet<&str> = applied.values().map(String::as_str).collect();

    let mut all_family_members = HashSet::new();
    let mut restated_exclusions = HashSet::new();
    let mut new_exclusions: Vec<(String, String, String)> = Vec::new();
    let mut reviewed_canonical_members = 0usize;
    let mut excluded_reviewed_members = 0usize;
    let mut retired_applied_canonicals = 0usize;
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
        let mut review_counts: Vec<(usize, &str)> = Vec::with_capacity(family.members.len());
        for member in &family.members {
            let frozen = pool
                .members
                .get(&member.segment_id)
                .or_else(|| pool.retired_member(&member.segment_id))
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
            review_counts.push((review_count, member.segment_id.as_str()));
            if let Some(applied_canonical) = applied.get(&member.segment_id) {
                // An applied exclusion row is immutable: its canonical must sit in this very family,
                // either as the family canonical or as an applied canonical retired here in turn.
                if !member_ids.contains(applied_canonical) {
                    return Err(format!(
                        "superseding manifest separates applied exclusion {} from its canonical",
                        member.segment_id
                    ));
                }
                restated_exclusions.insert(member.segment_id.clone());
            }
        }
        let family_applied_canonicals: Vec<&str> =
            segment_ids.iter().map(String::as_str).filter(|id| applied_canonicals.contains(id)).collect();
        let expected_canonical: (&str, &str) = if !family_applied_canonicals.is_empty() {
            // Two applied v1 families that the lag-tolerant verdict now proves to be one recording
            // merge here: the applied canonical with the most human evidence stays, the other retires
            // (its own applied exclusions keep pointing at it — a chain the binding resolves).
            let most = family_applied_canonicals
                .iter()
                .map(|id| review_counts.iter().find(|(_, member)| member == id).map_or(0, |(count, _)| *count))
                .max()
                .unwrap_or(0);
            let mut tied: Vec<(DedupSelectionKey, &str)> = family_applied_canonicals
                .iter()
                .filter(|id| {
                    review_counts.iter().find(|(_, member)| member == *id).map_or(0, |(count, _)| *count) == most
                })
                .map(|id| {
                    selection
                        .get(*id)
                        .map(|evidence| (selection_key(id, evidence), *id))
                        .ok_or_else(|| format!("dedup member {id} lost its validated selection evidence"))
                })
                .collect::<Result<_, _>>()?;
            tied.sort();
            retired_applied_canonicals += family_applied_canonicals.len() - 1;
            (tied[0].1, "preserve-applied-canonical")
        } else if review_counts.iter().any(|(count, _)| *count > 0) {
            // Most human evidence wins; an exact tie falls back to the stable audio-quality key so two
            // builders can never disagree.
            let most = review_counts.iter().map(|(count, _)| *count).max().unwrap_or(0);
            let mut tied: Vec<(DedupSelectionKey, &str)> = review_counts
                .iter()
                .filter(|(count, _)| *count == most)
                .map(|(_, segment_id)| {
                    selection
                        .get(*segment_id)
                        .map(|evidence| (selection_key(segment_id, evidence), *segment_id))
                        .ok_or_else(|| format!("dedup member {segment_id} lost its validated selection evidence"))
                })
                .collect::<Result<_, _>>()?;
            tied.sort();
            (tied[0].1, "preserve-most-human-review-evidence")
        } else {
            let mut ranked: Vec<(DedupSelectionKey, &str)> = Vec::with_capacity(segment_ids.len());
            for segment_id in &segment_ids {
                let evidence = selection
                    .get(segment_id.as_str())
                    .ok_or_else(|| format!("dedup member {segment_id} lost its validated selection evidence"))?;
                ranked.push((selection_key(segment_id, evidence), segment_id.as_str()));
            }
            ranked.sort();
            (ranked[0].1, "best-measured-audio-quality-then-stable-identity")
        };
        if expected_canonical.0 != family.canonical_segment_id
            || family.canonical_selection_reason != expected_canonical.1
        {
            return Err(format!("dedup family {} canonical selection is not deterministic", family.family_id));
        }
        if review_counts.iter().any(|(count, id)| *count > 0 && *id == family.canonical_segment_id) {
            reviewed_canonical_members += 1;
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
            let lag_ok = edge.lag_ms.is_some_and(|lag| lag.abs() <= V2_MAXIMUM_LAG_MS);
            let overlap_ok =
                edge.overlap_ppm.is_some_and(|overlap| (V2_MINIMUM_OVERLAP_PPM..=1_000_000).contains(&overlap));
            if edge.left_segment_id == edge.right_segment_id
                || !member_ids.contains(&edge.left_segment_id)
                || !member_ids.contains(&edge.right_segment_id)
                || !(V2_MINIMUM_WAVEFORM_CORRELATION_PPM..=1_000_001).contains(&edge.correlation_ppm)
                || !lag_ok
                || !overlap_ok
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
            if member.canonical {
                continue;
            }
            if member.review_evidence_count != 0 {
                excluded_reviewed_members += 1;
            }
            if !applied.contains_key(&member.segment_id) {
                new_exclusions.push((
                    member.segment_id.clone(),
                    family.canonical_segment_id.clone(),
                    family.family_id.clone(),
                ));
            }
        }
    }
    if restated_exclusions.len() != applied.len() {
        return Err("superseding manifest drops an applied duplicate exclusion".to_string());
    }
    new_exclusions.sort_unstable();
    let total_excluded = applied.len() + new_exclusions.len();
    if total_excluded != manifest.summary.excluded_members
        || pool.focus_segment_count.checked_sub(total_excluded) != Some(manifest.summary.canonical_members)
        || manifest.summary.newly_excluded_members != Some(new_exclusions.len())
        || manifest.summary.excluded_reviewed_members != Some(excluded_reviewed_members)
        || manifest.summary.retired_applied_canonicals != Some(retired_applied_canonicals)
        || reviewed_canonical_members != manifest.summary.reviewed_canonical_members
    {
        return Err("superseding dedup manifest summary does not match validated families".to_string());
    }

    require_complete_clip_identity(db, &new_exclusions)?;

    with_pool_full_sync(db, || {
        let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| format!("review-pool dedup supersession cannot lock the database: {error}"))?;
        tx.execute(
            "INSERT INTO review_pool_dedup_supersessions
                (pool_id, sequence, supersedes_manifest_sha256, algorithm_id, family_count, excluded_count,
                 canonical_count, newly_excluded_count, excluded_reviewed_count, unconfirmed_risk_count,
                 manifest_json, manifest_sha256, app_git_sha, created_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                &pool.pool_id,
                next_sequence,
                &current_sha256,
                DEDUP_ALGORITHM_V2,
                i64::try_from(manifest.families.len()).map_err(|_| "duplicate family count is too large".to_string())?,
                i64::try_from(total_excluded).map_err(|_| "duplicate exclusion count is too large".to_string())?,
                i64::try_from(manifest.summary.canonical_members)
                    .map_err(|_| "canonical member count is too large".to_string())?,
                i64::try_from(new_exclusions.len()).map_err(|_| "new exclusion count is too large".to_string())?,
                i64::try_from(excluded_reviewed_members)
                    .map_err(|_| "excluded reviewed count is too large".to_string())?,
                canonical_manifest,
                claimed_sha256,
                crate::GIT_SHA,
                manifest.generated_at_ms,
            ],
        )
        .map_err(|error| format!("review-pool dedup supersession cannot be committed: {error}"))?;
        {
            let mut statement = tx
                .prepare(
                    "INSERT INTO review_pool_duplicate_exclusions
                        (pool_id, segment_id, canonical_segment_id, family_id, created_at_ms, manifest_sha256)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(|error| format!("review-pool duplicate exclusion writer cannot be prepared: {error}"))?;
            for (segment_id, canonical_segment_id, family_id) in &new_exclusions {
                statement
                    .execute(rusqlite::params![
                        &pool.pool_id,
                        segment_id,
                        canonical_segment_id,
                        family_id,
                        manifest.generated_at_ms,
                        claimed_sha256,
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
        if usize::try_from(committed).ok() != Some(total_excluded) {
            return Err("review-pool duplicate exclusion transaction is incomplete".to_string());
        }
        tx.commit().map_err(|error| format!("review-pool dedup supersession cannot commit: {error}"))?;
        Ok(())
    })?;
    dedup_status(db)
}
