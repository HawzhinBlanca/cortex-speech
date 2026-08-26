//! Model registry — the safe control surface for ingesting and promoting ASR model versions.
//!
//! The schema (migration v23) enforces the hard invariants at the DB level: at most one
//! `champion` per family (a partial unique index) and a closed `source`/`status` vocabulary
//! (CHECK constraints). This module is the gated control surface on top of that:
//!
//!   * **Import gate.** Registering a *user/cortex* fine-tune REQUIRES a non-empty checkpoint
//!     content hash. The architecture doc calls this out as a bug-class to close: an empty pin
//!     sails through `models::verify_extracted_against_pin` (which treats an empty pin as OK,
//!     correct for first-seed bootstrap) and would otherwise reach the trusted promotion path
//!     unverifiable. Stock seeds may still carry an empty pin during bootstrap, so the gate is
//!     scoped to externally-produced checkpoints.
//!   * **No import-straight-to-champion.** A freshly imported version is always a `candidate`;
//!     becoming `champion` is a separate, gated promotion — never a side effect of import.
//!   * **Atomic promotion.** Promoting a version demotes the prior champion of its family in the
//!     same transaction, so the one-champion invariant is never even momentarily tripped.

use crate::db::Database;
use crate::deployment::{DeploymentProvenance, VerifiedDeployment, VerifiedDeploymentRecord, OMNIASR_7B_FAMILY};
use crate::error::{AppError, AppResult};
use crate::scorecard::Scorecard;
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};

/// A row of the `model_versions` registry (the columns callers actually read back).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelVersion {
    pub id: String,
    pub family: String,
    pub model_card_name: Option<String>,
    pub checkpoint_sha256: String,
    pub checkpoint_path: String,
    pub source: String,
    pub license: String,
    pub status: String,
}

/// What a caller supplies to register a new candidate. `status` is deliberately absent — a
/// freshly imported version is always a `candidate`; promotion is a separate gated step.
#[derive(Debug, Clone)]
pub struct NewModelVersion {
    pub id: String,
    pub family: String,
    pub model_card_name: Option<String>,
    pub checkpoint_sha256: String,
    pub checkpoint_path: String,
    pub source: String,
    pub license: String,
}

/// The minimum identity required to prove that the process answering on the champion port is the
/// exact registry row it claims to serve. Paths and display names are deliberately excluded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentIdentity {
    pub model_version_id: String,
    pub deployment_sha256: String,
}

/// Sources whose checkpoints originate OUTSIDE the trusted stock seed and therefore MUST carry
/// a non-empty content hash before they may enter the registry. Stock seeds (`meta-stock`) may
/// legitimately have an empty pin during first-seed bootstrap.
fn requires_nonempty_pin(source: &str) -> bool {
    matches!(source, "user-finetuned" | "cortex-finetuned")
}

const SELECT_COLS: &str = "id, family, model_card_name, checkpoint_sha256, checkpoint_path, source, license, status";

fn map_version(row: &Row) -> rusqlite::Result<ModelVersion> {
    Ok(ModelVersion {
        id: row.get(0)?,
        family: row.get(1)?,
        model_card_name: row.get(2)?,
        checkpoint_sha256: row.get(3)?,
        checkpoint_path: row.get(4)?,
        source: row.get(5)?,
        license: row.get(6)?,
        status: row.get(7)?,
    })
}

/// Register a new model version as a `candidate`. Enforces the non-empty-pin import gate for
/// externally-produced checkpoints. The DB's CHECK constraint independently rejects an unknown
/// `source`, so an invalid source is refused even if this gate is bypassed.
pub fn register_candidate(db: &Database, new: &NewModelVersion) -> AppResult<()> {
    if requires_nonempty_pin(&new.source) && new.checkpoint_sha256.trim().is_empty() {
        return Err(AppError::Validation(format!(
            "refusing to register {} model '{}' with an empty checkpoint_sha256: a fine-tuned \
             checkpoint must be content-hash pinned before it can be trusted for promotion",
            new.source, new.id
        )));
    }
    db.connection().execute(
        "INSERT INTO model_versions
            (id, family, model_card_name, checkpoint_sha256, checkpoint_path, source, license, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'candidate')",
        params![
            new.id,
            new.family,
            new.model_card_name,
            new.checkpoint_sha256,
            new.checkpoint_path,
            new.source,
            new.license,
        ],
    )?;
    Ok(())
}

/// Promote a registered version to `champion`, atomically demoting the prior champion of the
/// same family to `rolled_back`. The whole swap is one transaction so the one-champion-per-family
/// invariant is never tripped, even momentarily.
pub(crate) fn promote_to_champion(db: &Database, id: &str) -> AppResult<()> {
    let conn = db.connection();
    let tx = conn.unchecked_transaction()?;

    // Only a genuinely-absent row is a "validation" problem (unknown id); a real DB error (locked /
    // IO / corrupt) must surface AS a DB error, not be mislabeled as an unknown model version
    // (round-24 hunt #13) — otherwise a transient fault reads as "you promoted a nonexistent model".
    let family: String =
        match tx.query_row("SELECT family FROM model_versions WHERE id = ?1", params![id], |r| r.get(0)) {
            Ok(family) => family,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(AppError::Validation(format!("cannot promote unknown model version '{id}'")));
            }
            Err(e) => return Err(e.into()),
        };

    // Demote the incumbent champion of this family (if any other row holds it).
    tx.execute(
        "UPDATE model_versions SET status = 'rolled_back'
         WHERE family = ?1 AND status = 'champion' AND id <> ?2",
        params![family, id],
    )?;
    // Crown the new champion.
    tx.execute(
        "UPDATE model_versions SET status = 'champion', promoted_at = datetime('now') WHERE id = ?1",
        params![id],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn set_champion_for_test(db: &Database, id: &str) -> AppResult<()> {
    promote_to_champion(db, id)
}

/// P5.2 (true-10 audit): mirror the registry's champions to `<data_dir>/champion.json` so external
/// consumers — the WSL 7B server, whose adapter path was previously HARDCODED, making promotion a
/// no-op at its final step — can resolve the current champion without reading the app database.
/// Written atomically (tmp + rename); snapshot.rs already lists champion.json in EXTRA_STATE, so
/// the pointer rides the rotating auto-snapshots. Returns true when the file was (re)written.
/// Called at startup (so a promotion made by any path is reflected on next launch) and intended to
/// be called by the future Promote action (P5.3).
pub fn sync_champion_pointer(db: &Database, data_dir: &std::path::Path) -> AppResult<bool> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare(&format!("SELECT {SELECT_COLS} FROM model_versions WHERE status = 'champion' ORDER BY family ASC"))?;
    let mut rows = stmt.query([])?;
    let mut families = serde_json::Map::new();
    while let Some(row) = rows.next()? {
        let champion = map_version(row)?;
        families.insert(
            champion.family.clone(),
            serde_json::json!({
                "modelVersionId": champion.id,
                "deploymentManifestPath": champion.checkpoint_path,
                "deploymentSha256": champion.checkpoint_sha256,
                "source": champion.source,
                "license": champion.license,
            }),
        );
    }
    let payload = serde_json::json!({ "schema": 2, "champions": families });
    let text = serde_json::to_string_pretty(&payload)
        .map_err(|e| AppError::Other(format!("champion pointer serialize: {e}")))?;
    let path = data_dir.join("champion.json");
    crate::atomic_file::recover_interrupted_replace(&path).map_err(AppError::Io)?;
    // Skip the write when unchanged so the file's mtime stays meaningful for external watchers.
    if std::fs::read_to_string(&path).map(|old| old == text).unwrap_or(false) {
        return Ok(false);
    }
    let tmp = data_dir.join(format!("champion.json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, &text).map_err(AppError::Io)?;
    crate::atomic_file::replace_file(&tmp, &path).map_err(AppError::Io)?;
    Ok(true)
}

/// Register a complete, locally verified deployment unit. New challengers must carry flywheel
/// provenance; the transparent-but-unverifiable legacy form is reserved for the one-time incumbent
/// bootstrap and can never enter through this path.
pub fn register_verified_deployment(
    db: &Database,
    verified: &VerifiedDeployment,
    source: &str,
    license: &str,
) -> AppResult<ModelVersion> {
    register_verified_deployment_record(db, &verified.record(), source, license)
}

pub fn register_verified_deployment_record(
    db: &Database,
    verified: &VerifiedDeploymentRecord,
    source: &str,
    license: &str,
) -> AppResult<ModelVersion> {
    if !matches!(verified.manifest.provenance, DeploymentProvenance::Flywheel { .. }) {
        return Err(AppError::Validation("legacy_bootstrap provenance cannot be registered as a challenger".into()));
    }
    let new = NewModelVersion {
        id: verified.manifest.model_id.clone(),
        family: verified.manifest.family.clone(),
        model_card_name: Some(verified.manifest.model_card.clone()),
        checkpoint_sha256: verified.deployment_sha256.clone(),
        checkpoint_path: verified.manifest_path.clone(),
        source: source.to_string(),
        license: license.to_string(),
    };
    register_candidate(db, &new)?;
    get_model_version(db, &new.id)?
        .ok_or_else(|| AppError::Other(format!("registered deployment '{}' could not be read back", new.id)))
}

const LEGACY_BASE_SHA256: &str = "1b29a4045ddfbe9125e6c9d465d5bc29063eea256ace37c129742edc07aed17a";
const LEGACY_ADAPTER_SHA256: &str = "c348ade8a8160319e7e6f070addb3c7b066b70716390e8f4ae548c7db7af3750";
const LEGACY_ADAPTER_CONFIG_SHA256: &str = "4b870f13ec88f4ca19cc3bdded779ec03090077a82e0381412fb80cc420ce331";
const LEGACY_TOKENIZER_SHA256: &str = "8aa11a1092142ef472537476ef6e76541123e2f0d789b79f3ebd119008240b1e";
const LEGACY_MEASUREMENT_EVIDENCE_SHA256: &str = "14793b6f9c4f67c9a08989e54212be85b56ddda06036e6ffd69e8d0f76416f88";
// Existing owner deployments were sealed before the historical N=922 evidence document gained its
// explicit duplication-weighted/non-primary warning. The model bytes and measured run did not
// change, so retain that one exact predecessor identity for upgrade compatibility; no arbitrary
// evidence hash is accepted.
const LEGACY_MEASUREMENT_EVIDENCE_SHA256_PRE_LABEL: &str =
    "6133abb5684f1f9856c99b4765cfea66e084ba4acd72ece649e316c467fee68e";
const LEGACY_MODEL_ID: &str = "omniasr-7b-legacy-c348ade8a816";

/// One-time, atomic admission of the measured incumbent that predates flywheel lineage. Every
/// behavior-determining byte is pinned to the measured live deployment, provenance explicitly says
/// training is unverifiable, and the family must contain zero rows. This is not a general shortcut:
/// future challengers can enter only through `register_verified_deployment_record`.
pub fn bootstrap_verified_legacy_deployment(
    db: &Database,
    verified: &VerifiedDeploymentRecord,
    license: &str,
) -> AppResult<ModelVersion> {
    let DeploymentProvenance::LegacyBootstrap { measurement_evidence_sha256, training_provenance } =
        &verified.manifest.provenance
    else {
        return Err(AppError::Validation("legacy bootstrap requires explicit legacy_bootstrap provenance".into()));
    };
    let manifest = &verified.manifest;
    if manifest.model_id != LEGACY_MODEL_ID
        || manifest.base_checkpoint.id != "omniASR-LLM-7B-v2"
        || manifest.base_checkpoint.sha256 != LEGACY_BASE_SHA256
        || manifest.adapter.sha256 != LEGACY_ADAPTER_SHA256
        || manifest.adapter_config.sha256 != LEGACY_ADAPTER_CONFIG_SHA256
        || manifest.tokenizer.sha256 != LEGACY_TOKENIZER_SHA256
        || !matches!(
            measurement_evidence_sha256.as_str(),
            LEGACY_MEASUREMENT_EVIDENCE_SHA256 | LEGACY_MEASUREMENT_EVIDENCE_SHA256_PRE_LABEL
        )
        || training_provenance != "unverifiable"
    {
        return Err(AppError::Validation(
            "legacy deployment does not match the measured incumbent's complete pinned identity".into(),
        ));
    }
    crate::validation::input::validate_identifier(license).map_err(AppError::Validation)?;
    let conn = db.connection();
    let tx = conn.unchecked_transaction()?;
    let family_rows: i64 =
        tx.query_row("SELECT COUNT(*) FROM model_versions WHERE family = ?1", params![OMNIASR_7B_FAMILY], |row| {
            row.get(0)
        })?;
    if family_rows != 0 {
        return Err(AppError::Validation(format!(
            "legacy bootstrap is single-use and requires an empty family; found {family_rows} existing row(s)"
        )));
    }
    tx.execute(
        "INSERT INTO model_versions
            (id, family, model_card_name, checkpoint_sha256, checkpoint_path, source, license, status, promoted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'meta-stock', ?6, 'champion', datetime('now'))",
        params![
            manifest.model_id,
            manifest.family,
            manifest.model_card,
            verified.deployment_sha256,
            verified.manifest_path,
            license,
        ],
    )?;
    tx.commit()?;
    get_model_version(db, LEGACY_MODEL_ID)?.ok_or_else(|| {
        AppError::Other("legacy champion was committed but could not be read back from the registry".into())
    })
}

fn verify_registry_deployment(version: &ModelVersion) -> AppResult<VerifiedDeployment> {
    if version.family != OMNIASR_7B_FAMILY {
        return Err(AppError::Validation(format!(
            "deployment verification is defined only for family '{OMNIASR_7B_FAMILY}', got '{}'",
            version.family
        )));
    }
    let verified = crate::deployment::verify_deployment_manifest(
        std::path::Path::new(&version.checkpoint_path),
        Some(&version.checkpoint_sha256),
    )?;
    if verified.manifest.model_id != version.id || verified.manifest.family != version.family {
        return Err(AppError::Validation(format!(
            "registry row '{}' does not match deployment manifest model/family identity",
            version.id
        )));
    }
    if version.model_card_name.as_deref() != Some(verified.manifest.model_card.as_str()) {
        return Err(AppError::Validation(format!(
            "registry row '{}' does not match deployment manifest model card",
            version.id
        )));
    }
    Ok(verified)
}

/// Is `model_id` a model of `family` in the registry?
///
/// Champion supremacy asks a question the per-row provenance filter cannot answer on its own: a row
/// drafted BEFORE the owner selected WSL7B carries the weaker engine's id, and matching it per-row
/// would surface that engine's hypotheses during champion review. This is the membership test that
/// keeps such a row contributing NO auxiliary vote, exactly as the fixed-string filter used to.
pub(crate) fn is_family_model(db: &Database, model_id: &str, family: &str) -> AppResult<bool> {
    let found: i64 = db.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM model_versions WHERE id = ?1 AND family = ?2)",
        rusqlite::params![model_id, family],
        |row| row.get(0),
    )?;
    Ok(found != 0)
}

pub fn champion_identity(db: &Database, family: &str) -> AppResult<Option<DeploymentIdentity>> {
    Ok(get_champion(db, family)?.map(|version| DeploymentIdentity {
        model_version_id: version.id,
        deployment_sha256: version.checkpoint_sha256,
    }))
}

/// The current champion for a family, if one is crowned.
pub fn get_champion(db: &Database, family: &str) -> AppResult<Option<ModelVersion>> {
    let conn = db.connection();
    let mut stmt =
        conn.prepare(&format!("SELECT {SELECT_COLS} FROM model_versions WHERE family = ?1 AND status = 'champion'"))?;
    let mut rows = stmt.query(params![family])?;
    match rows.next()? {
        Some(row) => Ok(Some(map_version(row)?)),
        None => Ok(None),
    }
}

/// All registered model versions, newest first within each family. The read surface a registry UI
/// lists.
pub fn list_model_versions(db: &Database) -> AppResult<Vec<ModelVersion>> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare(&format!("SELECT {SELECT_COLS} FROM model_versions ORDER BY family ASC, created_at DESC, id ASC"))?;
    let rows = stmt.query_map([], map_version)?;
    let mut versions = Vec::new();
    for row in rows {
        versions.push(row?);
    }
    Ok(versions)
}

/// Fetch a single model version by id.
pub fn get_model_version(db: &Database, id: &str) -> AppResult<Option<ModelVersion>> {
    let conn = db.connection();
    let mut stmt = conn.prepare(&format!("SELECT {SELECT_COLS} FROM model_versions WHERE id = ?1"))?;
    let mut rows = stmt.query(params![id])?;
    match rows.next()? {
        Some(row) => Ok(Some(map_version(row)?)),
        None => Ok(None),
    }
}

/// The champion's stored gold CER, if a champion exists for the family and has been evaluated.
/// Exposed for reporting/diagnostics. Note: the promotion gate (`decide_promotion`) no longer compares
/// against this frozen value — it gates on the PAIRED CER from the challenger's `vs_baseline` (same
/// gold-id intersection as the WER gate), so a model can't pass by being scored on an easier/larger set.
pub fn champion_gold_cer(db: &Database, family: &str) -> AppResult<Option<f64>> {
    let conn = db.connection();
    let value: Option<f64> = conn
        .query_row(
            "SELECT gold_cer FROM model_versions WHERE family = ?1 AND status = 'champion'",
            params![family],
            |row| row.get(0),
        )
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    Ok(value)
}

/// Record a version's gold-set evaluation onto its registry row (the scorecard + headline metrics
/// the promotion gate reads). Errors if the version id is unknown, so an eval can never silently
/// attach to nothing.
#[allow(clippy::too_many_arguments)]
pub fn record_eval_result(
    db: &Database,
    version_id: &str,
    gold_wer: f64,
    gold_cer: f64,
    gold_ci_low: f64,
    gold_ci_high: f64,
    mapsswe_p_vs_active: Option<f64>,
    scorecard_json: &str,
    eval_run_id: Option<&str>,
) -> AppResult<()> {
    let updated = db.connection().execute(
        "UPDATE model_versions
         SET gold_wer = ?2, gold_cer = ?3, gold_ci_low = ?4, gold_ci_high = ?5,
             mapsswe_p_vs_active = ?6, scorecard_json = ?7, eval_run_id = ?8
         WHERE id = ?1",
        params![
            version_id,
            gold_wer,
            gold_cer,
            gold_ci_low,
            gold_ci_high,
            mapsswe_p_vs_active,
            scorecard_json,
            eval_run_id,
        ],
    )?;
    if updated == 0 {
        return Err(AppError::Validation(format!(
            "cannot record an eval result for unknown model version '{version_id}'"
        )));
    }
    Ok(())
}

/// Import an external model checkpoint into the registry as a gated candidate: verify the file
/// exists, compute its SHA-256 server-side (content-addressing it — the caller never supplies the
/// hash), and register it. `register_candidate`'s import gate then guarantees a finetuned source can
/// never enter unpinned. Returns the computed checkpoint hash.
///
/// This is the safe half of ingesting a fine-tuned model. Writing the fairseq2 asset-card and
/// resolving the real base-card name (which must fail loudly rather than hardcode `…_v2`) are a
/// follow-up that needs the WSL fairseq2 registry.
/// Hash the checkpoint file (the slow, multi-hundred-MB-to-GB streaming SHA-256). Split out from
/// `import_checkpoint` so a caller can run it BEFORE taking the DB lock — holding the global db mutex
/// across a full-file read starves every UI DB poll (same lock-across-heavy-op class as the rest).
pub fn hash_checkpoint(checkpoint_path: &str) -> AppResult<String> {
    let path = std::path::Path::new(checkpoint_path);
    if !path.is_file() {
        return Err(AppError::Validation(format!("cannot import checkpoint: no file at '{checkpoint_path}'")));
    }
    crate::models::compute_file_sha256(path)
        .map_err(|e| AppError::Other(format!("hashing checkpoint '{checkpoint_path}': {e}")))
}

/// Register a (pre-hashed) checkpoint as an unpinned candidate. This is the brief DB-only half; pass
/// the `sha` from `hash_checkpoint` so the DB lock is held only for the insert, not the file read.
#[allow(clippy::too_many_arguments)]
pub fn register_checkpoint(
    db: &Database,
    id: &str,
    family: &str,
    checkpoint_path: &str,
    source: &str,
    license: &str,
    model_card_name: Option<String>,
    sha: String,
) -> AppResult<String> {
    if family == OMNIASR_7B_FAMILY {
        return Err(AppError::Validation(
            "OmniASR-7B is a composite deployment (base + adapter + adapter config + tokenizer); import a verified deployment_manifest.json instead of a bare checkpoint"
                .into(),
        ));
    }
    // The caller pre-hashes via `hash_checkpoint` and passes `sha` in, so the DB lock here covers only
    // the insert, not the (slow) file read — do NOT re-hash inline.
    register_candidate(
        db,
        &NewModelVersion {
            id: id.to_string(),
            family: family.to_string(),
            model_card_name,
            checkpoint_sha256: sha.clone(),
            checkpoint_path: checkpoint_path.to_string(),
            source: source.to_string(),
            license: license.to_string(),
        },
    )?;
    Ok(sha)
}

#[allow(clippy::too_many_arguments)]
pub fn import_checkpoint(
    db: &Database,
    id: &str,
    family: &str,
    checkpoint_path: &str,
    source: &str,
    license: &str,
    model_card_name: Option<String>,
) -> AppResult<String> {
    let sha = hash_checkpoint(checkpoint_path)?;
    register_checkpoint(db, id, family, checkpoint_path, source, license, model_card_name, sha)
}

/// Build the fairseq2 asset-card YAML that drops an imported checkpoint into the WSL inference path
/// with zero change to the Python code. Enforces the two hard preconditions the architecture doc
/// calls out:
///   - `base` (the RESOLVED published base-card name) must be non-empty — never write an asset card
///     pointing at an unresolved base, which would silently fail to load. Resolving the base from
///     the installed fairseq2 registry is the caller's job; this refuses an empty one loudly rather
///     than hardcoding `…_v2`.
///   - `checkpoint_wsl_path` must be a WSL-visible `/mnt/...` path: the 7B runs in WSL, so a Windows
///     `C:\...` path silently fails to load and is rejected here.
pub fn build_fairseq2_asset_card(name: &str, base: &str, checkpoint_wsl_path: &str) -> AppResult<String> {
    if name.trim().is_empty() {
        return Err(AppError::Validation("asset-card name must not be empty".into()));
    }
    if base.trim().is_empty() {
        return Err(AppError::Validation(
            "asset-card base must be the RESOLVED fairseq2 base-card name — refusing to write an \
             unresolved base (it would silently fail to load)"
                .into(),
        ));
    }
    if !checkpoint_wsl_path.starts_with("/mnt/") {
        return Err(AppError::Validation(format!(
            "checkpoint must be a WSL-visible /mnt/... path (the 7B runs in WSL; a Windows path \
             silently fails): got '{checkpoint_wsl_path}'"
        )));
    }
    Ok(format!("name: {name}\nbase: {base}\ncheckpoint: \"{checkpoint_wsl_path}\"\n"))
}

/// The policy a challenger must satisfy to be promoted over the current champion. Defaults encode
/// the doc's reconciled gate: the challenger must *significantly* beat the champion on WER (the
/// existing scorecard `beats_baseline` rule) AND must not regress CER. The optional reduction
/// target lets a caller additionally demand the charter's ">=N% CER reduction".
#[derive(Debug, Clone)]
pub struct PromotionPolicy {
    /// Require the challenger to significantly beat the champion on WER (lower micro-WER AND
    /// MAPSSWE p < 0.05 — the `beats_baseline` flag). The promotion-blocking WER guard.
    pub require_wer_beats_baseline: bool,
    /// Maximum fractional CER regression tolerated vs the champion. 0.0 = strict non-regression
    /// (challenger CER must be <= champion CER).
    pub max_cer_regression_frac: f64,
    /// If set, additionally require the challenger to REDUCE CER by at least this fraction vs the
    /// champion (e.g. 0.30 for the charter's >=30% target).
    pub min_cer_reduction_frac: Option<f64>,
}

impl Default for PromotionPolicy {
    fn default() -> Self {
        Self { require_wer_beats_baseline: true, max_cer_regression_frac: 0.0, min_cer_reduction_frac: None }
    }
}

/// The verdict of the promotion gate, with a human-readable reason for every criterion evaluated
/// (so a blocked promotion is always explainable, never a silent "no").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionDecision {
    pub promote: bool,
    pub reasons: Vec<String>,
}

/// Decide whether a challenger may be promoted over the current champion. `challenger` is the
/// challenger's gold scorecard whose `vs_baseline` compares it against the champion — both the WER and
/// CER gates use that PAIRED comparison (over the shared gold-id intersection), so no separate frozen
/// champion CER is needed. Pure and deterministic — the same inputs always yield the same verdict, so
/// the gate is reproducible in CI. A scorecard with no `vs_baseline` is blocked (fail-closed).
///
/// Note: per-protected-slice regression (per-speaker / per-condition) is part of the doc's full
/// gate but is not evaluated here yet — there is no slice breakdown in the scorecard at this layer.
/// A caller that has slice data must AND it with this decision.
pub fn decide_promotion(challenger: &Scorecard, policy: &PromotionPolicy) -> PromotionDecision {
    let mut reasons = Vec::new();
    let mut promote = true;
    let eps = 1e-9;

    // --- WER gate: the challenger must significantly beat the champion (no regression by luck). ---
    match &challenger.vs_baseline {
        Some(cmp) => {
            if policy.require_wer_beats_baseline && !cmp.beats_baseline {
                promote = false;
                reasons.push(format!(
                    "WER gate FAILED: challenger micro-WER {:.4} vs champion {:.4} (MAPSSWE p={:.3}) does not significantly beat baseline",
                    cmp.system_micro_wer, cmp.baseline_micro_wer, cmp.mapsswe_p_value
                ));
            } else {
                reasons.push(format!(
                    "WER gate ok: challenger {:.4} vs champion {:.4} (p={:.3}, beats_baseline={})",
                    cmp.system_micro_wer, cmp.baseline_micro_wer, cmp.mapsswe_p_value, cmp.beats_baseline
                ));
            }
        }
        None => {
            if policy.require_wer_beats_baseline {
                promote = false;
                reasons.push("WER gate FAILED: no paired baseline comparison in the challenger scorecard".to_string());
            } else {
                reasons.push("WER gate skipped: not required by policy".to_string());
            }
        }
    }

    // --- CER gate: never ship a CER regression (the product north star). Gate on the PAIRED CER
    // (challenger vs champion over the SAME gold-id intersection), exactly like the WER gate — NOT the
    // challenger's unpaired full-set micro_cer vs the champion's frozen stored gold_cer, which can be
    // computed over a different/grown gold set and would let a model regressing CER on the shared
    // segments pass by being evaluated on an easier/larger superset. ---
    match &challenger.vs_baseline {
        Some(cmp) => {
            let challenger_cer = cmp.system_micro_cer;
            let baseline_cer = cmp.baseline_micro_cer;
            let allowed_cer = baseline_cer * (1.0 + policy.max_cer_regression_frac);
            if challenger_cer > allowed_cer + eps {
                promote = false;
                reasons.push(format!(
                    "CER gate FAILED: challenger paired CER {:.4} exceeds the allowed {:.4} (champion {:.4} + {:.0}% tolerance)",
                    challenger_cer, allowed_cer, baseline_cer, policy.max_cer_regression_frac * 100.0
                ));
            } else {
                reasons.push(format!(
                    "CER non-regression ok: challenger paired {challenger_cer:.4} <= allowed {allowed_cer:.4}"
                ));
            }

            // --- Optional CER-reduction target (the charter's ">=N% reduction"), also paired. ---
            if let Some(min_reduction) = policy.min_cer_reduction_frac {
                let reduction = if baseline_cer > 0.0 { (baseline_cer - challenger_cer) / baseline_cer } else { 0.0 };
                if reduction + eps < min_reduction {
                    promote = false;
                    reasons.push(format!(
                        "CER reduction gate FAILED: challenger reduces CER by {:.1}%, below the required {:.1}%",
                        reduction * 100.0,
                        min_reduction * 100.0
                    ));
                } else {
                    reasons.push(format!(
                        "CER reduction ok: {:.1}% >= required {:.1}%",
                        reduction * 100.0,
                        min_reduction * 100.0
                    ));
                }
            }
        }
        None => {
            promote = false;
            reasons.push("CER gate FAILED: no paired baseline comparison in the challenger scorecard".to_string());
        }
    }

    // --- Slice gate: never ship a model that improves the aggregate while REGRESSING a slice. ---
    if let Some(cmp) = &challenger.vs_baseline {
        if !cmp.slice_regressions.is_empty() {
            promote = false;
            reasons.push(format!(
                "Slice gate FAILED: challenger regresses {} slice(s) despite a better aggregate — {}",
                cmp.slice_regressions.len(),
                cmp.slice_regressions.join("; ")
            ));
        } else if cmp.evaluated_slices == 0 {
            // Round-21 (promotion_slice_gate): NO length slice had enough paired data to evaluate, so
            // "no slice regressed" would be unearned assurance — exactly the conflation the honesty law
            // forbids. Fail closed: the slice gate is UNVERIFIED (not satisfied), which on the small
            // gold sets this project actually ships on is the common case. Grow the gold set (so a
            // length bucket reaches MIN_SLICE_SEGS) before a challenger can be crowned on slice grounds.
            promote = false;
            reasons.push(
                "Slice gate UNVERIFIED: no length slice had enough paired data to test for regression — blocking under fail-closed policy (grow the gold set so a slice reaches the minimum)".to_string(),
            );
        } else {
            reasons.push(format!(
                "Slice gate ok: no per-condition slice regressed beyond tolerance ({} slice(s) evaluated)",
                cmp.evaluated_slices
            ));
        }
    }

    PromotionDecision { promote, reasons }
}

/// Evaluate the legacy in-process scorecard gate without mutating registry state.
/// `challenger_scorecard` must compare the challenger against the CURRENT champion. Production
/// promotion is a multi-system operation and may run only through the durable saga; keeping this
/// helper decision-only prevents bypassing pointer, process identity, canary and rollback checks.
///
/// There is deliberately NO implicit first-champion shortcut. The measured incumbent is admitted
/// once through [`bootstrap_verified_legacy_deployment`]; every flywheel challenger is then evaluated against
/// that exact incumbent. Treating "registry empty" as a free pass lets a missing/corrupt registry
/// crown an unevaluated model.
pub fn gate_and_promote(
    db: &Database,
    challenger_id: &str,
    challenger_scorecard: &Scorecard,
    policy: &PromotionPolicy,
) -> AppResult<PromotionDecision> {
    let challenger = get_model_version(db, challenger_id)?
        .ok_or_else(|| AppError::Validation(format!("cannot gate unknown model version '{challenger_id}'")))?;

    // Safety-gate precondition #1 (round-24 hunt #12): the scorecard must BELONG to the challenger
    // being gated. A scorecard whose system.model_id names a different model would gate the wrong
    // model's metrics onto this promotion — silently crowning `challenger_id` on another model's
    // numbers. The harness stamps system.model_id (scorecard.rs), so this is a caller-contract check.
    if challenger_scorecard.system.model_id != challenger_id {
        return Err(AppError::Validation(format!(
            "promotion gate: scorecard is for model '{}', not the challenger '{}' being gated",
            challenger_scorecard.system.model_id, challenger_id
        )));
    }

    let challenger_deployment = verify_registry_deployment(&challenger)?;
    if !matches!(challenger_deployment.manifest.provenance, DeploymentProvenance::Flywheel { .. }) {
        return Err(AppError::Validation("promotion gate accepts only a fully traced flywheel challenger".into()));
    }

    // Distinguish "no champion exists" from "a champion exists" by the CHAMPION ROW itself — NOT by
    // whether its gold_cer is non-NULL. Keying on gold_cer would read a champion with a NULL gold_cer
    // as "no champion", letting the next challenger bypass the ENTIRE gate (a free promotion). The gate
    // uses the PAIRED champion CER carried in the challenger scorecard's vs_baseline, so the champion's
    // stored gold_cer value is not needed here at all.
    let decision = match get_champion(db, &challenger.family)? {
        None => PromotionDecision {
            promote: false,
            reasons: vec![format!(
                "no incumbent champion for family '{}' — refusing an unpaired first promotion; bootstrap the measured incumbent explicitly",
                challenger.family
            )],
        },
        Some(champion) => {
            // Validate the incumbent deployment too. A scorecard pairing against a row whose bytes
            // can no longer be reproduced is not trustworthy promotion evidence.
            let _incumbent_deployment = verify_registry_deployment(&champion)?;
            // Safety-gate precondition #2 (round-24 hunt #12, the stale-baseline hole): the scorecard's
            // paired baseline MUST be the CURRENT champion. In a batch fan-out, two challengers B and C
            // are both scored against champion A; gating B promotes it and rolls A back. C's scorecard
            // still pairs against A — every gate would pass against the now-STALE baseline and crown C
            // even though it was never compared to B, the model it displaces. Fail-closed on mismatch.
            if let Some(cmp) = &challenger_scorecard.vs_baseline {
                if cmp.baseline_model_id != champion.id {
                    return Err(AppError::Validation(format!(
                        "promotion gate: scorecard for '{challenger_id}' was compared against baseline '{}', \
                         but the current champion of family '{}' is '{}' — re-run the gold eval against the \
                         current champion before gating",
                        cmp.baseline_model_id, challenger.family, champion.id
                    )));
                }
            }
            decide_promotion(challenger_scorecard, policy)
        }
    };

    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db
    }

    fn candidate(id: &str, family: &str, source: &str, sha: &str) -> NewModelVersion {
        NewModelVersion {
            id: id.to_string(),
            family: family.to_string(),
            model_card_name: None,
            checkpoint_sha256: sha.to_string(),
            checkpoint_path: "/models/x.pt".to_string(),
            source: source.to_string(),
            license: "Apache-2.0".to_string(),
        }
    }

    fn register_test_flywheel(db: &Database, id: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let manifest = crate::deployment::tests::write_test_flywheel_manifest(dir.path(), id);
        let verified = crate::deployment::verify_deployment_manifest(&manifest, None).unwrap();
        register_verified_deployment(db, &verified, "cortex-finetuned", "Apache-2.0").unwrap();
        dir
    }

    fn test_legacy_record() -> (tempfile::TempDir, VerifiedDeploymentRecord) {
        let dir = tempfile::tempdir().unwrap();
        let manifest = crate::deployment::tests::write_test_flywheel_manifest(dir.path(), "fixture");
        let mut record = crate::deployment::verify_deployment_manifest(&manifest, None).unwrap().record();
        record.manifest.model_id = LEGACY_MODEL_ID.into();
        record.manifest.base_checkpoint.id = "omniASR-LLM-7B-v2".into();
        record.manifest.base_checkpoint.sha256 = LEGACY_BASE_SHA256.into();
        record.manifest.adapter.sha256 = LEGACY_ADAPTER_SHA256.into();
        record.manifest.adapter_config.sha256 = LEGACY_ADAPTER_CONFIG_SHA256.into();
        record.manifest.tokenizer.sha256 = LEGACY_TOKENIZER_SHA256.into();
        record.manifest.provenance = DeploymentProvenance::LegacyBootstrap {
            measurement_evidence_sha256: LEGACY_MEASUREMENT_EVIDENCE_SHA256.into(),
            training_provenance: "unverifiable".into(),
        };
        (dir, record)
    }

    #[test]
    fn legacy_bootstrap_is_exact_atomic_and_single_use() {
        let db = open();
        let (_dir, record) = test_legacy_record();
        let champion = bootstrap_verified_legacy_deployment(&db, &record, "Apache-2.0").unwrap();
        assert_eq!(champion.id, LEGACY_MODEL_ID);
        assert_eq!(champion.status, "champion");
        assert_eq!(list_model_versions(&db).unwrap().len(), 1);
        assert!(bootstrap_verified_legacy_deployment(&db, &record, "Apache-2.0").is_err());
        assert_eq!(list_model_versions(&db).unwrap().len(), 1, "a retry cannot duplicate or partially rewrite it");
    }

    #[test]
    fn legacy_bootstrap_rejects_one_byte_identity_drift_without_writing() {
        let db = open();
        let (_dir, mut record) = test_legacy_record();
        record.manifest.adapter_config.sha256 = "0".repeat(64);
        assert!(bootstrap_verified_legacy_deployment(&db, &record, "Apache-2.0").is_err());
        assert!(list_model_versions(&db).unwrap().is_empty());
    }

    #[test]
    fn legacy_bootstrap_accepts_only_the_exact_pre_label_evidence_identity_for_upgrade_compatibility() {
        let db = open();
        let (_dir, mut record) = test_legacy_record();
        let DeploymentProvenance::LegacyBootstrap { measurement_evidence_sha256, .. } = &mut record.manifest.provenance
        else {
            panic!("legacy fixture lost its provenance kind");
        };
        *measurement_evidence_sha256 = LEGACY_MEASUREMENT_EVIDENCE_SHA256_PRE_LABEL.into();

        let champion = bootstrap_verified_legacy_deployment(&db, &record, "Apache-2.0").unwrap();
        assert_eq!(champion.id, LEGACY_MODEL_ID);
        assert_eq!(list_model_versions(&db).unwrap().len(), 1);
    }

    #[test]
    fn champion_pointer_mirrors_promotions_and_skips_unchanged_writes() {
        // P5.2: promotion must be resolvable OUTSIDE the app db — the WSL 7B server reads
        // champion.json instead of a hardcoded adapter path.
        let db = open();
        let tmp = tempfile::TempDir::new().unwrap();

        // No champions yet -> an empty pointer is still written (schema present, zero families).
        assert!(sync_champion_pointer(&db, tmp.path()).unwrap(), "first sync writes the file");
        let text = std::fs::read_to_string(tmp.path().join("champion.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["schema"], 2);
        assert!(v["champions"].as_object().unwrap().is_empty());

        // Promote one -> the pointer carries id + sha + path for its family.
        register_candidate(&db, &candidate("m1", "omniasr-7b", "user-finetuned", "abc123")).unwrap();
        promote_to_champion(&db, "m1").unwrap();
        assert!(sync_champion_pointer(&db, tmp.path()).unwrap(), "changed registry rewrites the file");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join("champion.json")).unwrap()).unwrap();
        assert_eq!(v["champions"]["omniasr-7b"]["modelVersionId"], "m1");
        assert_eq!(v["champions"]["omniasr-7b"]["deploymentSha256"], "abc123");
        assert_eq!(v["champions"]["omniasr-7b"]["deploymentManifestPath"], "/models/x.pt");

        // Unchanged registry -> no rewrite (mtime stays meaningful for external watchers).
        assert!(!sync_champion_pointer(&db, tmp.path()).unwrap(), "unchanged content is not rewritten");
    }

    #[test]
    fn import_gate_rejects_empty_pin_for_finetuned_sources() {
        let db = open();
        // A user/cortex fine-tune with no content hash is refused at the gate...
        assert!(register_candidate(&db, &candidate("u1", "omniasr-7b", "user-finetuned", "")).is_err());
        assert!(register_candidate(&db, &candidate("c1", "omniasr-7b", "cortex-finetuned", "   ")).is_err());
        // ...and nothing was written.
        assert!(get_model_version(&db, "u1").unwrap().is_none());
        assert!(get_model_version(&db, "c1").unwrap().is_none());
    }

    #[test]
    fn import_gate_allows_empty_pin_for_stock_seed() {
        let db = open();
        // A stock seed may bootstrap with an empty pin (its archive hash is computed later).
        register_candidate(&db, &candidate("s1", "omniasr-ctc-1b", "meta-stock", "")).unwrap();
        let v = get_model_version(&db, "s1").unwrap().unwrap();
        assert_eq!(v.status, "candidate", "a freshly registered version is always a candidate");
    }

    #[test]
    fn register_never_imports_straight_to_champion() {
        let db = open();
        register_candidate(&db, &candidate("u1", "omniasr-7b", "user-finetuned", "sha-abc")).unwrap();
        let v = get_model_version(&db, "u1").unwrap().unwrap();
        assert_eq!(v.status, "candidate");
        assert!(get_champion(&db, "omniasr-7b").unwrap().is_none(), "no champion exists until promotion");
    }

    #[test]
    fn promotion_atomically_swaps_the_family_champion() {
        let db = open();
        register_candidate(&db, &candidate("v1", "omniasr-7b", "meta-stock", "sha1")).unwrap();
        register_candidate(&db, &candidate("v2", "omniasr-7b", "user-finetuned", "sha2")).unwrap();

        promote_to_champion(&db, "v1").unwrap();
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "v1");

        // Promoting v2 must demote v1 in the same step — never two champions in one family.
        promote_to_champion(&db, "v2").unwrap();
        let champ = get_champion(&db, "omniasr-7b").unwrap().unwrap();
        assert_eq!(champ.id, "v2", "the newly promoted version is champion");
        assert_eq!(
            get_model_version(&db, "v1").unwrap().unwrap().status,
            "rolled_back",
            "the prior champion is rolled back, not left as a second champion"
        );
    }

    #[test]
    fn champions_are_independent_across_families() {
        let db = open();
        register_candidate(&db, &candidate("o1", "omniasr-7b", "meta-stock", "sha1")).unwrap();
        register_candidate(&db, &candidate("w1", "whisper-ckb", "meta-stock", "sha2")).unwrap();
        promote_to_champion(&db, "o1").unwrap();
        promote_to_champion(&db, "w1").unwrap();
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "o1");
        assert_eq!(get_champion(&db, "whisper-ckb").unwrap().unwrap().id, "w1");
    }

    #[test]
    fn promoting_an_unknown_version_errors_and_changes_nothing() {
        let db = open();
        register_candidate(&db, &candidate("v1", "omniasr-7b", "meta-stock", "sha1")).unwrap();
        promote_to_champion(&db, "v1").unwrap();
        assert!(promote_to_champion(&db, "ghost").is_err(), "promoting a nonexistent id must error");
        // The real champion is untouched.
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "v1");
    }

    #[test]
    fn import_checkpoint_hashes_file_and_registers_gated_candidate() {
        let db = open();
        let tmp = tempfile::tempdir().unwrap();
        let ckpt = tmp.path().join("model.pt");
        std::fs::write(&ckpt, b"fake fine-tuned checkpoint bytes").unwrap();

        let sha = import_checkpoint(
            &db,
            "u7b",
            "omniasr-ctc-1b",
            ckpt.to_str().unwrap(),
            "user-finetuned",
            "Apache-2.0",
            Some("omniASR_ckb_v3".into()),
        )
        .unwrap();

        assert_eq!(sha.len(), 64, "a SHA-256 hex digest is 64 chars");
        assert_eq!(sha, crate::models::compute_file_sha256(&ckpt).unwrap(), "hash is deterministic over the bytes");
        let v = get_model_version(&db, "u7b").unwrap().unwrap();
        assert_eq!(v.checkpoint_sha256, sha, "the computed hash is stored, not a caller-supplied one");
        assert_eq!(v.status, "candidate", "an import is a candidate, never an instant champion");
        assert_eq!(v.source, "user-finetuned");
    }

    #[test]
    fn list_model_versions_returns_all_registered() {
        let db = open();
        assert!(list_model_versions(&db).unwrap().is_empty(), "an empty registry lists nothing");
        register_candidate(&db, &candidate("a", "omniasr-7b", "meta-stock", "s1")).unwrap();
        register_candidate(&db, &candidate("b", "whisper-ckb", "meta-stock", "s2")).unwrap();
        register_candidate(&db, &candidate("c", "omniasr-7b", "user-finetuned", "s3")).unwrap();
        let all = list_model_versions(&db).unwrap();
        assert_eq!(all.len(), 3, "every registered version is listed");
        let ids: Vec<&str> = all.iter().map(|v| v.id.as_str()).collect();
        assert!(ids.contains(&"a") && ids.contains(&"b") && ids.contains(&"c"));
    }

    #[test]
    fn asset_card_yaml_has_name_base_and_wsl_checkpoint() {
        let yaml =
            build_fairseq2_asset_card("omniASR_ckb_v3@user", "omniASR_LLM_7B", "/mnt/c/models/abc/consolidated.pt")
                .unwrap();
        assert!(yaml.contains("name: omniASR_ckb_v3@user"), "{yaml}");
        assert!(yaml.contains("base: omniASR_LLM_7B"), "{yaml}");
        assert!(yaml.contains("checkpoint: \"/mnt/c/models/abc/consolidated.pt\""), "{yaml}");
    }

    #[test]
    fn asset_card_refuses_unresolved_base() {
        // An empty/blank base must fail loudly — never hardcode or silently accept an unresolved card.
        assert!(build_fairseq2_asset_card("n", "", "/mnt/c/x.pt").is_err());
        assert!(build_fairseq2_asset_card("n", "   ", "/mnt/c/x.pt").is_err());
    }

    #[test]
    fn asset_card_refuses_windows_checkpoint_path() {
        assert!(
            build_fairseq2_asset_card("n", "base", "C:\\models\\x.pt").is_err(),
            "a Windows checkpoint path must be rejected — it silently fails to load in WSL"
        );
    }

    #[test]
    fn import_checkpoint_errors_on_missing_file() {
        let db = open();
        assert!(
            import_checkpoint(&db, "x", "omniasr-7b", "/no/such/file.pt", "user-finetuned", "Apache-2.0", None)
                .is_err(),
            "importing a nonexistent checkpoint must error, not register a phantom"
        );
        assert!(get_model_version(&db, "x").unwrap().is_none());
    }

    #[test]
    fn record_eval_result_persists_gold_metrics_and_feeds_champion_cer() {
        let db = open();
        register_candidate(&db, &candidate("v1", "omniasr-7b", "user-finetuned", "sha")).unwrap();
        record_eval_result(&db, "v1", 0.18, 0.07, 0.05, 0.09, Some(0.01), "{\"k\":1}", None).unwrap();

        let (wer, cer, scj): (f64, f64, String) = db
            .connection()
            .query_row("SELECT gold_wer, gold_cer, scorecard_json FROM model_versions WHERE id = 'v1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert!((wer - 0.18).abs() < 1e-9);
        assert!((cer - 0.07).abs() < 1e-9);
        assert_eq!(scj, "{\"k\":1}");

        // Once promoted, champion_gold_cer surfaces that CER as the gate's baseline.
        assert!(champion_gold_cer(&db, "omniasr-7b").unwrap().is_none(), "no champion yet");
        promote_to_champion(&db, "v1").unwrap();
        assert!((champion_gold_cer(&db, "omniasr-7b").unwrap().unwrap() - 0.07).abs() < 1e-9);
    }

    #[test]
    fn record_eval_result_errors_for_unknown_version() {
        let db = open();
        assert!(record_eval_result(&db, "ghost", 0.1, 0.1, 0.0, 0.2, None, "{}", None).is_err());
    }

    // --- promotion gate ---

    use crate::scorecard::{BaselineComparison, Scorecard, SystemScore};
    use crate::significance::ConfidenceInterval;

    fn ci(p: f64) -> ConfidenceInterval {
        ConfidenceInterval { point: p, lower: p, upper: p, confidence: 0.95 }
    }

    /// A challenger scorecard with a paired WER + CER comparison against the champion.
    /// Stamp a card's identity fields so it satisfies gate_and_promote's precondition checks (the
    /// scorecard belongs to `challenger_id` and is paired against `baseline_id`). The decide_promotion
    /// unit tests don't need this — they never read identity — so `challenger_card` keeps its literals.
    fn ided(mut card: Scorecard, challenger_id: &str, baseline_id: &str) -> Scorecard {
        card.system.model_id = challenger_id.into();
        if let Some(cmp) = card.vs_baseline.as_mut() {
            cmp.baseline_model_id = baseline_id.into();
        }
        card
    }

    fn challenger_card(
        system_wer: f64,
        system_cer: f64,
        champion_wer: f64,
        champion_cer: f64,
        beats: bool,
        p: f64,
    ) -> Scorecard {
        Scorecard {
            system: SystemScore {
                model_id: "challenger".into(),
                num_segments: 50,
                scored_segments: 50,
                micro_wer: system_wer,
                micro_cer: system_cer,
                macro_wer: system_wer,
                substitutions: 0,
                deletions: 0,
                insertions: 0,
                wer_ci: ci(system_wer),
                cer_ci: ci(system_cer),
            },
            vs_baseline: Some(BaselineComparison {
                baseline_model_id: "champion".into(),
                paired_segments: 50,
                baseline_micro_wer: champion_wer,
                system_micro_wer: system_wer,
                baseline_micro_cer: champion_cer,
                system_micro_cer: system_cer,
                mapsswe_p_value: p,
                significant_at_05: p < 0.05,
                beats_baseline: beats,
                slice_regressions: Vec::new(),
                evaluated_slices: 5, // slices were evaluated and clean (not "no data to evaluate")
            }),
            bootstrap_resamples: 1000,
            confidence: 0.95,
            seed: 7,
        }
    }

    #[test]
    fn promotes_when_it_beats_wer_and_does_not_regress_cer() {
        // Significantly lower WER and lower CER than the champion -> promote.
        let card = challenger_card(0.10, 0.05, 0.20, 0.08, true, 0.01);
        let decision = decide_promotion(&card, &PromotionPolicy::default());
        assert!(decision.promote, "a strict WER win with no CER regression must promote: {:?}", decision.reasons);
    }

    #[test]
    fn blocks_cer_regression_even_when_wer_wins() {
        // WER significantly better, but CER regressed (0.12 > champion 0.08) -> blocked.
        let card = challenger_card(0.10, 0.12, 0.20, 0.08, true, 0.01);
        let decision = decide_promotion(&card, &PromotionPolicy::default());
        assert!(!decision.promote, "a CER regression must block promotion despite a WER win");
        assert!(decision.reasons.iter().any(|r| r.contains("CER gate FAILED")), "{:?}", decision.reasons);
    }

    #[test]
    fn cer_gate_uses_paired_not_unpaired_full_set_cer() {
        // Round-10 audit HIGH: the CER gate compared the challenger's UNPAIRED full-set micro_cer
        // against the champion's frozen stored gold_cer. A challenger evaluated on a larger/easier gold
        // superset can show a LOWER full-set CER while REGRESSING CER on the shared (paired) segments.
        // The gate must use the paired CER and BLOCK such a model.
        let mut card = challenger_card(0.10, 0.05, 0.20, 0.10, true, 0.01);
        // Full-set micro_cer looks great (0.05) — under the old gate this would clear champion 0.10...
        card.system.micro_cer = 0.05;
        // ...but on the SHARED segments the challenger regresses CER vs the champion (0.13 > 0.10).
        if let Some(cmp) = card.vs_baseline.as_mut() {
            cmp.system_micro_cer = 0.13;
            cmp.baseline_micro_cer = 0.10;
        }
        let decision = decide_promotion(&card, &PromotionPolicy::default());
        assert!(
            !decision.promote,
            "a paired CER regression must block even when full-set CER looks lower: {:?}",
            decision.reasons
        );
        assert!(decision.reasons.iter().any(|r| r.contains("CER gate FAILED")), "{:?}", decision.reasons);
    }

    #[test]
    fn blocks_slice_regression_even_when_aggregate_wins() {
        // Beats the champion on aggregate WER and CER, but regresses a per-condition slice -> blocked.
        let mut card = challenger_card(0.10, 0.05, 0.20, 0.08, true, 0.01);
        if let Some(cmp) = card.vs_baseline.as_mut() {
            cmp.slice_regressions
                .push("short (≤4 words): challenger WER 0.4000 vs baseline 0.2000 over 12 segments".into());
        }
        let decision = decide_promotion(&card, &PromotionPolicy::default());
        assert!(!decision.promote, "a slice regression must block promotion despite an aggregate win");
        assert!(decision.reasons.iter().any(|r| r.contains("Slice gate FAILED")), "{:?}", decision.reasons);
    }

    #[test]
    fn blocks_when_no_slice_could_be_evaluated() {
        // Round-21 (promotion_slice_gate): a strict aggregate WER+CER win, but NO length slice had
        // enough paired data to test for regression (evaluated_slices == 0, the small-gold-set regime).
        // "No slice regressed" must NOT read as a green pass — fail closed as UNVERIFIED.
        let mut card = challenger_card(0.10, 0.05, 0.20, 0.08, true, 0.01);
        if let Some(cmp) = card.vs_baseline.as_mut() {
            cmp.slice_regressions.clear();
            cmp.evaluated_slices = 0;
        }
        let decision = decide_promotion(&card, &PromotionPolicy::default());
        assert!(!decision.promote, "an unverifiable slice gate must block promotion: {:?}", decision.reasons);
        assert!(
            decision.reasons.iter().any(|r| r.contains("Slice gate UNVERIFIED")),
            "must report the slice gate as UNVERIFIED, not an affirmative pass: {:?}",
            decision.reasons
        );
    }

    #[test]
    fn blocks_when_wer_not_significantly_better() {
        // Lower CER, but the WER improvement is not significant (beats_baseline=false) -> blocked.
        let card = challenger_card(0.19, 0.05, 0.20, 0.08, false, 0.40);
        let decision = decide_promotion(&card, &PromotionPolicy::default());
        assert!(!decision.promote, "an insignificant WER change must block promotion");
        assert!(decision.reasons.iter().any(|r| r.contains("WER gate FAILED")), "{:?}", decision.reasons);
    }

    #[test]
    fn cer_reduction_target_blocks_small_gains_and_passes_large_ones() {
        let policy = PromotionPolicy { min_cer_reduction_frac: Some(0.30), ..PromotionPolicy::default() };
        // Champion CER 0.10; challenger 0.09 = only 10% reduction -> below the 30% target -> blocked.
        let small = challenger_card(0.10, 0.09, 0.20, 0.10, true, 0.01);
        let d_small = decide_promotion(&small, &policy);
        assert!(!d_small.promote, "10% CER reduction must miss the 30% target");
        assert!(d_small.reasons.iter().any(|r| r.contains("CER reduction gate FAILED")), "{:?}", d_small.reasons);
        // Challenger 0.06 = 40% reduction -> clears the target -> promote.
        let big = challenger_card(0.10, 0.06, 0.20, 0.10, true, 0.01);
        assert!(decide_promotion(&big, &policy).promote, "40% CER reduction must clear the 30% target");
    }

    #[test]
    fn missing_baseline_comparison_blocks_by_default() {
        let mut card = challenger_card(0.10, 0.05, 0.20, 0.08, true, 0.01);
        card.vs_baseline = None;
        let decision = decide_promotion(&card, &PromotionPolicy::default());
        assert!(!decision.promote, "no paired baseline comparison must block promotion by default");
    }

    // --- gate_and_promote integration ---

    #[test]
    fn gate_and_promote_refuses_an_unpaired_first_challenger() {
        let db = open();
        let _v1 = register_test_flywheel(&db, "v1");
        let mut card = ided(challenger_card(0.5, 0.5, 0.5, 0.5, false, 0.9), "v1", "");
        card.vs_baseline = None;
        let decision = gate_and_promote(&db, "v1", &card, &PromotionPolicy::default()).unwrap();
        assert!(!decision.promote, "a missing incumbent must never become a free promotion: {:?}", decision.reasons);
        assert!(get_champion(&db, "omniasr-7b").unwrap().is_none());
    }

    #[test]
    fn gate_and_promote_is_decision_only_for_a_qualified_challenger() {
        let db = open();
        // Incumbent champion with a recorded gold CER.
        let _champ = register_test_flywheel(&db, "champ");
        record_eval_result(&db, "champ", 0.20, 0.10, 0.08, 0.12, None, "{}", None).unwrap();
        promote_to_champion(&db, "champ").unwrap();
        // Challenger that significantly beats WER and lowers CER (0.06 < 0.10).
        let _chall = register_test_flywheel(&db, "chall");
        let card = ided(challenger_card(0.10, 0.06, 0.20, 0.10, true, 0.01), "chall", "champ");
        let decision = gate_and_promote(&db, "chall", &card, &PromotionPolicy::default()).unwrap();
        assert!(decision.promote, "a qualified challenger must pass the gate: {:?}", decision.reasons);
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "champ");
        assert_eq!(get_model_version(&db, "chall").unwrap().unwrap().status, "candidate");
    }

    #[test]
    fn gate_and_promote_keeps_champion_when_challenger_regresses_cer() {
        let db = open();
        let _champ = register_test_flywheel(&db, "champ");
        record_eval_result(&db, "champ", 0.20, 0.10, 0.08, 0.12, None, "{}", None).unwrap();
        promote_to_champion(&db, "champ").unwrap();
        let _chall = register_test_flywheel(&db, "chall");
        // Beats WER but regresses CER (0.15 > 0.10) -> blocked; the champion is untouched.
        let card = ided(challenger_card(0.10, 0.15, 0.20, 0.10, true, 0.01), "chall", "champ");
        let decision = gate_and_promote(&db, "chall", &card, &PromotionPolicy::default()).unwrap();
        assert!(!decision.promote, "a CER-regressing challenger must be blocked");
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "champ", "champion is unchanged");
        assert_eq!(get_model_version(&db, "chall").unwrap().unwrap().status, "candidate");
    }

    #[test]
    fn gate_and_promote_does_not_bypass_gate_when_champion_gold_cer_is_null() {
        // Round-10 audit (latent, hardened pre-wiring): champion_gold_cer conflated "no champion" with
        // "champion exists but gold_cer NULL", so a challenger arriving while the incumbent had a NULL
        // gold_cer used to bypass the ENTIRE gate (free promotion). gate_and_promote now keys on the
        // champion ROW, so even a NULL-gold_cer champion still forces the challenger through the gate.
        let db = open();
        let _champ = register_test_flywheel(&db, "champ");
        promote_to_champion(&db, "champ").unwrap(); // crowned WITHOUT record_eval_result -> gold_cer NULL
        assert!(
            champion_gold_cer(&db, "omniasr-7b").unwrap().is_none(),
            "precondition: the champion's gold_cer is NULL"
        );

        let _chall = register_test_flywheel(&db, "chall");
        // A CER-regressing challenger (paired 0.15 > 0.10) must be BLOCKED, not free-promoted.
        let card = ided(challenger_card(0.10, 0.15, 0.20, 0.10, true, 0.01), "chall", "champ");
        let decision = gate_and_promote(&db, "chall", &card, &PromotionPolicy::default()).unwrap();
        assert!(
            !decision.promote,
            "a champion with NULL gold_cer must still gate the challenger: {:?}",
            decision.reasons
        );
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "champ", "champion unchanged");
    }

    #[test]
    fn gate_and_promote_errors_for_unknown_challenger() {
        let db = open();
        let card = challenger_card(0.1, 0.05, 0.2, 0.08, true, 0.01);
        assert!(gate_and_promote(&db, "ghost", &card, &PromotionPolicy::default()).is_err());
    }

    #[test]
    fn gate_and_promote_rejects_a_scorecard_paired_against_a_stale_baseline() {
        // Round-24 hunt #12 (the stale-baseline hole): a batch fan-out scores challengers B and C
        // both against champion A. Gating B promotes it and rolls A back. C's scorecard STILL pairs
        // against A — every WER/CER/slice gate would pass against the now-stale baseline and crown C,
        // though it was never compared to B, the champion it would displace. The gate must refuse a
        // scorecard whose baseline is not the CURRENT champion.
        let db = open();
        let _a = register_test_flywheel(&db, "A");
        record_eval_result(&db, "A", 0.20, 0.10, 0.08, 0.12, None, "{}", None).unwrap();
        promote_to_champion(&db, "A").unwrap();

        // B beats A. The decision is then handed to the saga; this test manually applies only the DB
        // phase so it can exercise the stale-baseline precondition in isolation.
        let _b = register_test_flywheel(&db, "B");
        let card_b = ided(challenger_card(0.10, 0.06, 0.20, 0.10, true, 0.01), "B", "A");
        assert!(gate_and_promote(&db, "B", &card_b, &PromotionPolicy::default()).unwrap().promote);
        promote_to_champion(&db, "B").unwrap();
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "B");

        // C's scorecard is still paired against the now-STALE baseline A — must be REFUSED, and B stays.
        let _c = register_test_flywheel(&db, "C");
        let card_c = ided(challenger_card(0.09, 0.05, 0.20, 0.10, true, 0.01), "C", "A");
        let err = gate_and_promote(&db, "C", &card_c, &PromotionPolicy::default());
        assert!(err.is_err(), "a scorecard paired against a rolled-back baseline must be refused, not gated");
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "B", "the real champion is unchanged");

        // Re-scored against the CURRENT champion B, C passes the decision gate but remains a candidate
        // until the durable saga performs all external side effects.
        let card_c_fresh = ided(challenger_card(0.09, 0.05, 0.10, 0.06, true, 0.01), "C", "B");
        assert!(gate_and_promote(&db, "C", &card_c_fresh, &PromotionPolicy::default()).unwrap().promote);
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "B");
        assert_eq!(get_model_version(&db, "C").unwrap().unwrap().status, "candidate");
    }

    #[test]
    fn gate_and_promote_rejects_a_scorecard_for_a_different_model() {
        // The scorecard must belong to the challenger being gated — a card whose system.model_id names
        // another model would crown `challenger_id` on someone else's numbers.
        let db = open();
        register_candidate(&db, &candidate("champ", "omniasr-7b", "meta-stock", "shaA")).unwrap();
        record_eval_result(&db, "champ", 0.20, 0.10, 0.08, 0.12, None, "{}", None).unwrap();
        promote_to_champion(&db, "champ").unwrap();
        register_candidate(&db, &candidate("chall", "omniasr-7b", "user-finetuned", "shaB")).unwrap();

        // Card's system.model_id is "someone-else", not "chall".
        let card = ided(challenger_card(0.10, 0.06, 0.20, 0.10, true, 0.01), "someone-else", "champ");
        let err = gate_and_promote(&db, "chall", &card, &PromotionPolicy::default());
        assert!(err.is_err(), "a scorecard for a different model must be refused");
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "champ", "champion unchanged");
    }
}
