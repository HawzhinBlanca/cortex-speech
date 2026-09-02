//! Owner-rights stamping and voice-authority certificates for the flexible review pool.
//!
//! Moved verbatim out of `review_pool.rs` to keep that module under the production-size ceiling.
//! Nothing here changed; every item is re-exported from the parent so each `crate::review_pool::`
//! path keeps resolving exactly as before.

use super::*;

pub const OWNER_RIGHTS_LICENSE: &str = "owner-full-rights";
pub const OWNER_RIGHTS_CONSENT: &str = "speaker-agreement-paid-unrestricted-public";
pub const OWNER_RIGHTS_PERMITTED_USE: &str = "unrestricted: train, evaluate, publish, redistribute, commercial";
pub const OWNER_RIGHTS_ATTRIBUTION: &str = "Hawzhin (owner) — speakers paid and agreed to full public use";
pub const OWNER_RIGHTS_SOURCE: &str = "owner-supplied recording";

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RightsStampReport {
    pub recordings: usize,
    pub segments: usize,
    pub stamped_recordings: usize,
    pub already_exact_recordings: usize,
    pub rights_sha256: String,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RightsCoverageReport {
    pub recordings: usize,
    pub segment_rows: usize,
    pub exact_rows: usize,
    pub unstamped_rows: usize,
    pub conflicting_rows: usize,
    pub revoked_rows: usize,
    pub all_exact: bool,
    pub rights_sha256: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VoiceAuthorityDigests {
    pub voice_name: String,
    pub segment_count: usize,
    pub resolution_sha256: String,
    pub reviewer_sha256: String,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VoiceCertificateRecord {
    pub id: i64,
    pub pool_id: String,
    pub voice_name: String,
    pub resolution_sha256: String,
    pub rights_sha256: String,
    pub audio_sha256: String,
    pub reviewer_sha256: String,
    pub export_manifest_sha256: String,
    pub export_sha256sums_sha256: String,
    pub certificate_json: String,
    pub certificate_sha256: String,
    pub retained_segments: usize,
    pub rejected_segments: usize,
    pub total_duration_ms: i64,
    pub app_git_sha: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct VoiceCertificateInput<'a> {
    pub voice_name: &'a str,
    pub resolution_sha256: &'a str,
    pub rights_sha256: &'a str,
    pub audio_sha256: &'a str,
    pub reviewer_sha256: &'a str,
    pub export_manifest_sha256: &'a str,
    pub export_sha256sums_sha256: &'a str,
    pub certificate_json: &'a str,
    pub certificate_sha256: &'a str,
    pub retained_segments: usize,
    pub rejected_segments: usize,
    pub total_duration_ms: i64,
    pub created_at_ms: i64,
}

pub fn voice_authority_digests(db: &Database, voice_name: &str) -> Result<VoiceAuthorityDigests, String> {
    let voice_name = voice_name.trim();
    if voice_name.is_empty() {
        return Err("voice name cannot be blank".to_string());
    }
    let resolutions = segment_resolutions(db, Some(voice_name))?;
    if resolutions.is_empty() {
        return Err(format!("active review pool has no voice named {voice_name}"));
    }
    let reviewers = reviewer_sets(db)?;
    let mut resolution_digest = Sha256::new();
    let mut reviewer_digest = Sha256::new();
    hash_field(&mut resolution_digest, voice_name.as_bytes());
    hash_field(&mut reviewer_digest, voice_name.as_bytes());
    for resolution in &resolutions {
        hash_field(&mut resolution_digest, resolution.segment_id.as_bytes());
        hash_field(&mut resolution_digest, resolution.status.as_bytes());
        hash_field(&mut resolution_digest, resolution.evidence_sha256.as_bytes());
        hash_field(&mut resolution_digest, resolution.final_action.as_deref().unwrap_or("").as_bytes());
        hash_field(&mut resolution_digest, resolution.final_transcript.as_deref().unwrap_or("").as_bytes());

        hash_field(&mut reviewer_digest, resolution.segment_id.as_bytes());
        let mut evidence: Vec<_> =
            reviewers.get(&resolution.segment_id).map(|value| value.judged.values().collect()).unwrap_or_else(Vec::new);
        evidence.sort_unstable_by(|left, right| {
            reviewer_key(Some(&left.reviewer))
                .cmp(&reviewer_key(Some(&right.reviewer)))
                .then(left.evidence_id.cmp(&right.evidence_id))
        });
        for judgement in evidence {
            hash_field(&mut reviewer_digest, reviewer_key(Some(&judgement.reviewer)).as_bytes());
            hash_field(&mut reviewer_digest, judgement.evidence_id.as_bytes());
            hash_field(&mut reviewer_digest, judgement.outcome.digest_value().as_bytes());
        }
    }
    Ok(VoiceAuthorityDigests {
        voice_name: voice_name.to_string(),
        segment_count: resolutions.len(),
        resolution_sha256: resolution_digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect(),
        reviewer_sha256: reviewer_digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect(),
    })
}

fn validate_voice_certificate_evidence(
    db: &Database,
    pool: &ReviewPool,
    input: &VoiceCertificateInput<'_>,
    app_git_sha: &str,
) -> Result<(), String> {
    let voice_name = input.voice_name.trim();
    if voice_name.is_empty()
        || input.total_duration_ms < 0
        || input.created_at_ms <= 0
        || app_git_sha.len() != 40
        || !app_git_sha.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("voice certificate has invalid identity, duration, timestamp, or build provenance".to_string());
    }
    for (label, value) in [
        ("resolution", input.resolution_sha256),
        ("rights", input.rights_sha256),
        ("audio", input.audio_sha256),
        ("reviewer", input.reviewer_sha256),
        ("export manifest", input.export_manifest_sha256),
        ("export checksums", input.export_sha256sums_sha256),
        ("certificate", input.certificate_sha256),
    ] {
        if !valid_lower_sha256(value) {
            return Err(format!("voice certificate {label} digest is invalid"));
        }
    }
    let certificate_value = serde_json::from_str::<serde_json::Value>(input.certificate_json)
        .map_err(|error| format!("voice certificate JSON is invalid: {error}"))?;
    let dedup = dedup_status(db)?;
    let expected_u64 = |value: usize| u64::try_from(value).ok();
    let certificate_matches_authority = certificate_value.get("schemaVersion").and_then(serde_json::Value::as_u64)
        == Some(2)
        && certificate_value.get("poolId").and_then(serde_json::Value::as_str) == Some(pool.pool_id.as_str())
        && certificate_value.get("poolFocusSha256").and_then(serde_json::Value::as_str)
            == Some(pool.focus_sha256.as_str())
        && certificate_value.get("sourcePoolSegmentCount").and_then(serde_json::Value::as_u64)
            == expected_u64(dedup.source_segment_count)
        && certificate_value.get("canonicalReviewSegmentCount").and_then(serde_json::Value::as_u64)
            == expected_u64(dedup.canonical_segment_count)
        && certificate_value.get("excludedDuplicateSegmentCount").and_then(serde_json::Value::as_u64)
            == expected_u64(dedup.excluded_segment_count)
        && certificate_value.get("duplicateFamilyCount").and_then(serde_json::Value::as_u64)
            == expected_u64(dedup.duplicate_family_count)
        && certificate_value.get("dedupManifestSha256").and_then(serde_json::Value::as_str)
            == dedup.manifest_sha256.as_deref()
        && certificate_value.get("dedupAlgorithmId").and_then(serde_json::Value::as_str)
            == dedup.algorithm_id.as_deref()
        && certificate_value.get("dedupUnconfirmedRiskCount").and_then(serde_json::Value::as_u64)
            == expected_u64(dedup.unconfirmed_risk_count)
        && certificate_value.get("voiceName").and_then(serde_json::Value::as_str) == Some(voice_name)
        && certificate_value.get("championModelVersionId").and_then(serde_json::Value::as_str)
            == Some(pool.champion_model_version_id.as_str())
        && certificate_value.get("championDeploymentSha256").and_then(serde_json::Value::as_str)
            == Some(pool.champion_deployment_sha256.as_str())
        && certificate_value.get("resolutionSha256").and_then(serde_json::Value::as_str)
            == Some(input.resolution_sha256)
        && certificate_value.get("reviewerSha256").and_then(serde_json::Value::as_str) == Some(input.reviewer_sha256)
        && certificate_value.get("decisionAndReviewerEvidenceSha256").and_then(serde_json::Value::as_str)
            == Some(input.reviewer_sha256)
        && certificate_value.get("rightsSha256").and_then(serde_json::Value::as_str) == Some(input.rights_sha256)
        && certificate_value.get("audioSha256").and_then(serde_json::Value::as_str) == Some(input.audio_sha256)
        && certificate_value.get("exportManifestSha256").and_then(serde_json::Value::as_str)
            == Some(input.export_manifest_sha256)
        && certificate_value.get("exportSha256sumsSha256").and_then(serde_json::Value::as_str)
            == Some(input.export_sha256sums_sha256)
        && certificate_value.get("retainedSegments").and_then(serde_json::Value::as_u64)
            == expected_u64(input.retained_segments)
        && certificate_value.get("rejectedSegments").and_then(serde_json::Value::as_u64)
            == expected_u64(input.rejected_segments)
        && certificate_value.get("totalDurationMs").and_then(serde_json::Value::as_i64)
            == Some(input.total_duration_ms)
        && certificate_value.get("appGitSha").and_then(serde_json::Value::as_str) == Some(app_git_sha)
        && certificate_value.get("createdAtMs").and_then(serde_json::Value::as_i64) == Some(input.created_at_ms);
    if !dedup.applied
        || dedup.unconfirmed_risk_count != 0
        || dedup.source_segment_count != pool.focus_segment_count
        || dedup.canonical_segment_count != pool.review_segment_count
        || pool.dedup_manifest_sha256.as_deref() != dedup.manifest_sha256.as_deref()
        || !certificate_matches_authority
    {
        return Err("voice certificate JSON does not match its complete v64 pool authority".to_string());
    }
    let actual_certificate_sha: String =
        Sha256::digest(input.certificate_json.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    if actual_certificate_sha != input.certificate_sha256 {
        return Err("voice certificate JSON does not match its digest".to_string());
    }
    let authority = voice_authority_digests(db, voice_name)?;
    if authority.resolution_sha256 != input.resolution_sha256 || authority.reviewer_sha256 != input.reviewer_sha256 {
        return Err("voice certificate does not match current review authority".to_string());
    }
    let resolutions = segment_resolutions(db, Some(voice_name))?;
    if resolutions.iter().any(|row| !matches!(row.status.as_str(), "resolved" | "ownerResolved")) {
        return Err(format!("voice {voice_name} is not fully resolved"));
    }
    let retained = resolutions.iter().filter(|row| row.final_action.as_deref() == Some("retain")).count();
    let rejected = resolutions.iter().filter(|row| row.final_action.as_deref() == Some("reject")).count();
    if retained != input.retained_segments
        || rejected != input.rejected_segments
        || retained + rejected != resolutions.len()
    {
        return Err("voice certificate counts do not match resolved review outcomes".to_string());
    }
    Ok(())
}

pub fn voice_certificate(db: &Database, voice_name: &str) -> Result<Option<VoiceCertificateRecord>, String> {
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < REVIEW_POOL_SCHEMA_VERSION {
        return Ok(None);
    }
    let certificate = db
        .connection()
        .query_row(
            "SELECT id, pool_id, voice_name, resolution_sha256, rights_sha256, audio_sha256,
                    reviewer_sha256, export_manifest_sha256, export_sha256sums_sha256,
                    certificate_json, certificate_sha256, retained_segments, rejected_segments,
                    total_duration_ms, app_git_sha, created_at_ms
               FROM review_pool_voice_certificates WHERE voice_name=?1 COLLATE BINARY",
            [voice_name.trim()],
            |row| {
                let retained: i64 = row.get(11)?;
                let rejected: i64 = row.get(12)?;
                Ok(VoiceCertificateRecord {
                    id: row.get(0)?,
                    pool_id: row.get(1)?,
                    voice_name: row.get(2)?,
                    resolution_sha256: row.get(3)?,
                    rights_sha256: row.get(4)?,
                    audio_sha256: row.get(5)?,
                    reviewer_sha256: row.get(6)?,
                    export_manifest_sha256: row.get(7)?,
                    export_sha256sums_sha256: row.get(8)?,
                    certificate_json: row.get(9)?,
                    certificate_sha256: row.get(10)?,
                    retained_segments: usize::try_from(retained).unwrap_or(usize::MAX),
                    rejected_segments: usize::try_from(rejected).unwrap_or(usize::MAX),
                    total_duration_ms: row.get(13)?,
                    app_git_sha: row.get(14)?,
                    created_at_ms: row.get(15)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("review-pool voice certificate cannot be read: {error}"))?;
    let Some(certificate) = certificate else {
        return Ok(None);
    };
    let pool = load(db)?.ok_or_else(|| "voice certificate exists without an active review pool".to_string())?;
    if certificate.pool_id != pool.pool_id {
        return Err("voice certificate belongs to another active review pool".to_string());
    }
    validate_voice_certificate_evidence(
        db,
        &pool,
        &VoiceCertificateInput {
            voice_name: &certificate.voice_name,
            resolution_sha256: &certificate.resolution_sha256,
            rights_sha256: &certificate.rights_sha256,
            audio_sha256: &certificate.audio_sha256,
            reviewer_sha256: &certificate.reviewer_sha256,
            export_manifest_sha256: &certificate.export_manifest_sha256,
            export_sha256sums_sha256: &certificate.export_sha256sums_sha256,
            certificate_json: &certificate.certificate_json,
            certificate_sha256: &certificate.certificate_sha256,
            retained_segments: certificate.retained_segments,
            rejected_segments: certificate.rejected_segments,
            total_duration_ms: certificate.total_duration_ms,
            created_at_ms: certificate.created_at_ms,
        },
        &certificate.app_git_sha,
    )?;
    Ok(Some(certificate))
}

pub fn record_voice_certificate(
    db: &Database,
    input: &VoiceCertificateInput<'_>,
) -> Result<VoiceCertificateRecord, String> {
    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    let voice_name = input.voice_name.trim();
    validate_voice_certificate_evidence(db, &pool, input, crate::GIT_SHA)?;
    if let Some(existing) = voice_certificate(db, voice_name)? {
        if existing.certificate_sha256 == input.certificate_sha256 {
            return Ok(existing);
        }
        return Err(format!("voice {voice_name} already has a different immutable certificate"));
    }
    with_pool_full_sync(db, || {
        db.connection()
            .execute(
                "INSERT INTO review_pool_voice_certificates
                    (pool_id, voice_name, resolution_sha256, rights_sha256, audio_sha256,
                     reviewer_sha256, export_manifest_sha256, export_sha256sums_sha256,
                     certificate_json, certificate_sha256, retained_segments, rejected_segments,
                     total_duration_ms, app_git_sha, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                rusqlite::params![
                    pool.pool_id,
                    voice_name,
                    input.resolution_sha256,
                    input.rights_sha256,
                    input.audio_sha256,
                    input.reviewer_sha256,
                    input.export_manifest_sha256,
                    input.export_sha256sums_sha256,
                    input.certificate_json,
                    input.certificate_sha256,
                    i64::try_from(input.retained_segments).map_err(|_| "retained count is too large".to_string())?,
                    i64::try_from(input.rejected_segments).map_err(|_| "rejected count is too large".to_string())?,
                    input.total_duration_ms,
                    crate::GIT_SHA,
                    input.created_at_ms,
                ],
            )
            .map_err(|error| format!("review-pool voice certificate cannot be committed: {error}"))?;
        voice_certificate(db, voice_name)?.ok_or_else(|| "committed voice certificate cannot be reread".to_string())
    })
}

type RightsTuple = (Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>);

fn pool_source_rights_on(conn: &rusqlite::Connection) -> Result<BTreeMap<String, Vec<RightsTuple>>, String> {
    let mut statement = conn
        .prepare(
            "SELECT segment.audio_path, segment.rights_license, segment.rights_consent_basis,
                    segment.rights_permitted_use, segment.rights_attribution,
                    segment.rights_source, segment.rights_revoked_at
               FROM speech_segments segment
              WHERE EXISTS (
                    SELECT 1 FROM review_pool_members member
                    JOIN speech_segments pool_segment ON pool_segment.id=member.segment_id
                   WHERE pool_segment.audio_path=segment.audio_path
              )
              ORDER BY segment.audio_path, segment.id",
        )
        .map_err(|error| format!("review-pool recording rights cannot be prepared: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ),
            ))
        })
        .map_err(|error| format!("review-pool recording rights cannot be read: {error}"))?;
    let mut result: BTreeMap<String, Vec<RightsTuple>> = BTreeMap::new();
    for row in rows {
        let (path, rights) = row.map_err(|error| format!("review-pool recording rights are unreadable: {error}"))?;
        result.entry(path).or_default().push(rights);
    }
    Ok(result)
}

fn blank(value: &Option<String>) -> bool {
    value.as_deref().map_or(true, |text| text.trim().is_empty())
}

fn exact_owner_rights(rights: &RightsTuple) -> bool {
    rights.0.as_deref() == Some(OWNER_RIGHTS_LICENSE)
        && rights.1.as_deref() == Some(OWNER_RIGHTS_CONSENT)
        && rights.2.as_deref() == Some(OWNER_RIGHTS_PERMITTED_USE)
        && rights.3.as_deref() == Some(OWNER_RIGHTS_ATTRIBUTION)
        && rights.4.as_deref() == Some(OWNER_RIGHTS_SOURCE)
        && blank(&rights.5)
}

fn unstamped_rights(rights: &RightsTuple) -> bool {
    blank(&rights.0) && blank(&rights.1) && blank(&rights.2) && blank(&rights.3) && blank(&rights.4) && blank(&rights.5)
}

fn validate_pool_source_rights(rows: &BTreeMap<String, Vec<RightsTuple>>) -> Result<(usize, usize), String> {
    if rows.is_empty() {
        return Err("active review pool has no source recordings".to_string());
    }
    let mut exact = 0usize;
    let mut unstamped = 0usize;
    for (path, entries) in rows {
        for rights in entries {
            if !blank(&rights.5) {
                return Err(format!("review-pool recording has revoked rights and will not be changed: {path}"));
            }
            if exact_owner_rights(rights) {
                exact += 1;
            } else if unstamped_rights(rights) {
                unstamped += 1;
            } else {
                return Err(format!(
                    "review-pool recording has conflicting rights and will not be overwritten: {path}"
                ));
            }
        }
    }
    Ok((exact, unstamped))
}

fn pool_rights_digest(pool_id: &str, rows: &BTreeMap<String, Vec<RightsTuple>>) -> String {
    let mut digest = Sha256::new();
    hash_field(&mut digest, pool_id.as_bytes());
    for path in rows.keys() {
        hash_field(&mut digest, path.as_bytes());
        hash_field(&mut digest, OWNER_RIGHTS_LICENSE.as_bytes());
        hash_field(&mut digest, OWNER_RIGHTS_CONSENT.as_bytes());
        hash_field(&mut digest, OWNER_RIGHTS_PERMITTED_USE.as_bytes());
        hash_field(&mut digest, OWNER_RIGHTS_ATTRIBUTION.as_bytes());
        hash_field(&mut digest, OWNER_RIGHTS_SOURCE.as_bytes());
    }
    digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn rights_coverage(db: &Database) -> Result<RightsCoverageReport, String> {
    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    let rows = pool_source_rights_on(db.connection())?;
    let mut report = RightsCoverageReport {
        recordings: rows.len(),
        segment_rows: 0,
        exact_rows: 0,
        unstamped_rows: 0,
        conflicting_rows: 0,
        revoked_rows: 0,
        all_exact: false,
        rights_sha256: None,
    };
    for entries in rows.values() {
        for rights in entries {
            report.segment_rows += 1;
            if !blank(&rights.5) {
                report.revoked_rows += 1;
            } else if exact_owner_rights(rights) {
                report.exact_rows += 1;
            } else if unstamped_rights(rights) {
                report.unstamped_rows += 1;
            } else {
                report.conflicting_rows += 1;
            }
        }
    }
    report.all_exact = report.recordings > 0 && report.exact_rows == report.segment_rows;
    if report.all_exact {
        report.rights_sha256 = Some(pool_rights_digest(&pool.pool_id, &rows));
    }
    Ok(report)
}

pub fn stamp_owner_supplied_pool_rights(db: &Database) -> Result<RightsStampReport, String> {
    let pool = load(db)?.ok_or_else(|| "review pool is not active".to_string())?;
    if crate::migrations::get_current_version(db).map_err(|error| error.to_string())? < REVIEW_POOL_SCHEMA_VERSION {
        return Err("owner rights stamping requires review-pool schema 63".to_string());
    }
    with_pool_full_sync(db, || {
        let tx = rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| format!("review-pool rights stamping cannot lock the database: {error}"))?;
        let before = pool_source_rights_on(&tx)?;
        let (_exact_rows, unstamped_rows) = validate_pool_source_rights(&before)?;
        let stamped_rows = tx
            .execute(
                "UPDATE speech_segments
                    SET rights_license=?1, rights_consent_basis=?2, rights_permitted_use=?3,
                        rights_attribution=?4, rights_source=?5, updated_at=datetime('now')
                  WHERE EXISTS (
                        SELECT 1 FROM review_pool_members member
                        JOIN speech_segments pool_segment ON pool_segment.id=member.segment_id
                       WHERE pool_segment.audio_path=speech_segments.audio_path
                  )
                    AND TRIM(COALESCE(rights_license,''))=''
                    AND TRIM(COALESCE(rights_consent_basis,''))=''
                    AND TRIM(COALESCE(rights_permitted_use,''))=''
                    AND TRIM(COALESCE(rights_attribution,''))=''
                    AND TRIM(COALESCE(rights_source,''))=''
                    AND TRIM(COALESCE(rights_revoked_at,''))=''",
                rusqlite::params![
                    OWNER_RIGHTS_LICENSE,
                    OWNER_RIGHTS_CONSENT,
                    OWNER_RIGHTS_PERMITTED_USE,
                    OWNER_RIGHTS_ATTRIBUTION,
                    OWNER_RIGHTS_SOURCE,
                ],
            )
            .map_err(|error| format!("review-pool rights cannot be stamped: {error}"))?;
        if stamped_rows != unstamped_rows {
            return Err(format!(
                "review-pool rights changed during stamping ({stamped_rows}/{unstamped_rows} rows); transaction refused"
            ));
        }
        let after = pool_source_rights_on(&tx)?;
        validate_pool_source_rights(&after)?;
        if after.values().flatten().any(|rights| !exact_owner_rights(rights)) {
            return Err("review-pool rights are not exact after stamping".to_string());
        }
        let mut segments = 0usize;
        let mut stamped_recordings = 0usize;
        let mut already_exact_recordings = 0usize;
        for (path, entries) in &after {
            segments += entries.len();
            let before_entries =
                before.get(path).ok_or_else(|| format!("recording disappeared during stamping: {path}"))?;
            if before_entries.iter().any(unstamped_rights) {
                stamped_recordings += 1;
            } else {
                already_exact_recordings += 1;
            }
        }
        tx.commit().map_err(|error| format!("review-pool rights stamping cannot commit: {error}"))?;
        Ok(RightsStampReport {
            recordings: after.len(),
            segments,
            stamped_recordings,
            already_exact_recordings,
            rights_sha256: pool_rights_digest(&pool.pool_id, &after),
        })
    })
}
