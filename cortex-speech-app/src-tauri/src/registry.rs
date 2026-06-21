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

/// Sources whose checkpoints originate OUTSIDE the trusted stock seed and therefore MUST carry
/// a non-empty content hash before they may enter the registry. Stock seeds (`meta-stock`) may
/// legitimately have an empty pin during first-seed bootstrap.
fn requires_nonempty_pin(source: &str) -> bool {
    matches!(source, "user-finetuned" | "cortex-finetuned")
}

const SELECT_COLS: &str =
    "id, family, model_card_name, checkpoint_sha256, checkpoint_path, source, license, status";

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
pub fn promote_to_champion(db: &Database, id: &str) -> AppResult<()> {
    let conn = db.connection();
    let tx = conn.unchecked_transaction()?;

    let family: String = tx
        .query_row("SELECT family FROM model_versions WHERE id = ?1", params![id], |r| r.get(0))
        .map_err(|_| AppError::Validation(format!("cannot promote unknown model version '{id}'")))?;

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

/// The current champion for a family, if one is crowned.
pub fn get_champion(db: &Database, family: &str) -> AppResult<Option<ModelVersion>> {
    let conn = db.connection();
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLS} FROM model_versions WHERE family = ?1 AND status = 'champion'"
    ))?;
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
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLS} FROM model_versions ORDER BY family ASC, created_at DESC, id ASC"
    ))?;
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
    let mut stmt =
        conn.prepare(&format!("SELECT {SELECT_COLS} FROM model_versions WHERE id = ?1"))?;
    let mut rows = stmt.query(params![id])?;
    match rows.next()? {
        Some(row) => Ok(Some(map_version(row)?)),
        None => Ok(None),
    }
}

/// The champion's gold CER, if a champion exists for the family and has been evaluated. This is the
/// CER baseline the promotion gate compares a challenger against (`decide_promotion`'s `champion_cer`).
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
    let path = std::path::Path::new(checkpoint_path);
    if !path.is_file() {
        return Err(AppError::Validation(format!(
            "cannot import checkpoint: no file at '{checkpoint_path}'"
        )));
    }
    let sha = crate::models::compute_file_sha256(path)
        .map_err(|e| AppError::Other(format!("hashing checkpoint '{checkpoint_path}': {e}")))?;
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
/// challenger's gold scorecard whose `vs_baseline` compares it against the champion; `champion_cer`
/// is the champion's own gold micro-CER (from `model_versions.gold_cer`). Pure and deterministic —
/// the same inputs always yield the same verdict, so the gate is reproducible in CI.
///
/// Note: per-protected-slice regression (per-speaker / per-condition) is part of the doc's full
/// gate but is not evaluated here yet — there is no slice breakdown in the scorecard at this layer.
/// A caller that has slice data must AND it with this decision.
pub fn decide_promotion(
    challenger: &Scorecard,
    champion_cer: f64,
    policy: &PromotionPolicy,
) -> PromotionDecision {
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
                reasons.push(
                    "WER gate FAILED: no paired baseline comparison in the challenger scorecard".to_string(),
                );
            } else {
                reasons.push("WER gate skipped: not required by policy".to_string());
            }
        }
    }

    // --- CER gate: never ship a CER regression (the product north star). ---
    let challenger_cer = challenger.system.micro_cer;
    let allowed_cer = champion_cer * (1.0 + policy.max_cer_regression_frac);
    if challenger_cer > allowed_cer + eps {
        promote = false;
        reasons.push(format!(
            "CER gate FAILED: challenger CER {:.4} exceeds the allowed {:.4} (champion {:.4} + {:.0}% tolerance)",
            challenger_cer, allowed_cer, champion_cer, policy.max_cer_regression_frac * 100.0
        ));
    } else {
        reasons.push(format!(
            "CER non-regression ok: challenger {challenger_cer:.4} <= allowed {allowed_cer:.4}"
        ));
    }

    // --- Optional CER-reduction target (the charter's ">=N% reduction"). ---
    if let Some(min_reduction) = policy.min_cer_reduction_frac {
        let reduction =
            if champion_cer > 0.0 { (champion_cer - challenger_cer) / champion_cer } else { 0.0 };
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

    // --- Slice gate: never ship a model that improves the aggregate while REGRESSING a slice. ---
    if let Some(cmp) = &challenger.vs_baseline {
        if cmp.slice_regressions.is_empty() {
            reasons.push("Slice gate ok: no per-condition slice regressed beyond tolerance".to_string());
        } else {
            promote = false;
            reasons.push(format!(
                "Slice gate FAILED: challenger regresses {} slice(s) despite a better aggregate — {}",
                cmp.slice_regressions.len(),
                cmp.slice_regressions.join("; ")
            ));
        }
    }

    PromotionDecision { promote, reasons }
}

/// Evaluate the promotion gate for a challenger and, if it passes, atomically promote it over the
/// current champion. `challenger_scorecard` must compare the challenger against the CURRENT champion
/// (its `vs_baseline`), as produced by the gold-eval harness. Returns the explainable decision in
/// both the promote and block cases.
///
/// First-champion bootstrap: if the family has no champion yet there is no baseline to beat, so the
/// challenger is promoted unconditionally as the family's first champion.
pub fn gate_and_promote(
    db: &Database,
    challenger_id: &str,
    challenger_scorecard: &Scorecard,
    policy: &PromotionPolicy,
) -> AppResult<PromotionDecision> {
    let challenger = get_model_version(db, challenger_id)?
        .ok_or_else(|| AppError::Validation(format!("cannot gate unknown model version '{challenger_id}'")))?;

    let decision = match champion_gold_cer(db, &challenger.family)? {
        None => PromotionDecision {
            promote: true,
            reasons: vec![format!(
                "no incumbent champion for family '{}' — promoted as the first champion",
                challenger.family
            )],
        },
        Some(champion_cer) => decide_promotion(challenger_scorecard, champion_cer, policy),
    };

    if decision.promote {
        promote_to_champion(db, challenger_id)?;
    }
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
        assert_eq!(get_model_version(&db, "v1").unwrap().unwrap().status, "rolled_back",
            "the prior champion is rolled back, not left as a second champion");
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
            "omniasr-7b",
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
        let yaml = build_fairseq2_asset_card(
            "omniASR_ckb_v3@user",
            "omniASR_LLM_7B",
            "/mnt/c/models/abc/consolidated.pt",
        )
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
            .query_row(
                "SELECT gold_wer, gold_cer, scorecard_json FROM model_versions WHERE id = 'v1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
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

    /// A challenger scorecard with a paired WER comparison against the champion.
    fn challenger_card(system_wer: f64, system_cer: f64, champion_wer: f64, beats: bool, p: f64) -> Scorecard {
        Scorecard {
            system: SystemScore {
                model_id: "challenger".into(),
                num_segments: 50,
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
                mapsswe_p_value: p,
                significant_at_05: p < 0.05,
                beats_baseline: beats,
                slice_regressions: Vec::new(),
            }),
            bootstrap_resamples: 1000,
            confidence: 0.95,
            seed: 7,
        }
    }

    #[test]
    fn promotes_when_it_beats_wer_and_does_not_regress_cer() {
        // Significantly lower WER and lower CER than the champion -> promote.
        let card = challenger_card(0.10, 0.05, 0.20, true, 0.01);
        let decision = decide_promotion(&card, 0.08, &PromotionPolicy::default());
        assert!(decision.promote, "a strict WER win with no CER regression must promote: {:?}", decision.reasons);
    }

    #[test]
    fn blocks_cer_regression_even_when_wer_wins() {
        // WER significantly better, but CER regressed (0.12 > champion 0.08) -> blocked.
        let card = challenger_card(0.10, 0.12, 0.20, true, 0.01);
        let decision = decide_promotion(&card, 0.08, &PromotionPolicy::default());
        assert!(!decision.promote, "a CER regression must block promotion despite a WER win");
        assert!(decision.reasons.iter().any(|r| r.contains("CER gate FAILED")), "{:?}", decision.reasons);
    }

    #[test]
    fn blocks_slice_regression_even_when_aggregate_wins() {
        // Beats the champion on aggregate WER and CER, but regresses a per-condition slice -> blocked.
        let mut card = challenger_card(0.10, 0.05, 0.20, true, 0.01);
        if let Some(cmp) = card.vs_baseline.as_mut() {
            cmp.slice_regressions
                .push("short (≤4 words): challenger WER 0.4000 vs baseline 0.2000 over 12 segments".into());
        }
        let decision = decide_promotion(&card, 0.08, &PromotionPolicy::default());
        assert!(!decision.promote, "a slice regression must block promotion despite an aggregate win");
        assert!(decision.reasons.iter().any(|r| r.contains("Slice gate FAILED")), "{:?}", decision.reasons);
    }

    #[test]
    fn blocks_when_wer_not_significantly_better() {
        // Lower CER, but the WER improvement is not significant (beats_baseline=false) -> blocked.
        let card = challenger_card(0.19, 0.05, 0.20, false, 0.40);
        let decision = decide_promotion(&card, 0.08, &PromotionPolicy::default());
        assert!(!decision.promote, "an insignificant WER change must block promotion");
        assert!(decision.reasons.iter().any(|r| r.contains("WER gate FAILED")), "{:?}", decision.reasons);
    }

    #[test]
    fn cer_reduction_target_blocks_small_gains_and_passes_large_ones() {
        let policy = PromotionPolicy { min_cer_reduction_frac: Some(0.30), ..PromotionPolicy::default() };
        // Champion CER 0.10; challenger 0.09 = only 10% reduction -> below the 30% target -> blocked.
        let small = challenger_card(0.10, 0.09, 0.20, true, 0.01);
        let d_small = decide_promotion(&small, 0.10, &policy);
        assert!(!d_small.promote, "10% CER reduction must miss the 30% target");
        assert!(d_small.reasons.iter().any(|r| r.contains("CER reduction gate FAILED")), "{:?}", d_small.reasons);
        // Challenger 0.06 = 40% reduction -> clears the target -> promote.
        let big = challenger_card(0.10, 0.06, 0.20, true, 0.01);
        assert!(decide_promotion(&big, 0.10, &policy).promote, "40% CER reduction must clear the 30% target");
    }

    #[test]
    fn missing_baseline_comparison_blocks_by_default() {
        let mut card = challenger_card(0.10, 0.05, 0.20, true, 0.01);
        card.vs_baseline = None;
        let decision = decide_promotion(&card, 0.08, &PromotionPolicy::default());
        assert!(!decision.promote, "no paired baseline comparison must block promotion by default");
    }

    // --- gate_and_promote integration ---

    #[test]
    fn gate_and_promote_crowns_first_champion_unconditionally() {
        let db = open();
        register_candidate(&db, &candidate("v1", "omniasr-7b", "user-finetuned", "sha")).unwrap();
        // Even a weak scorecard with no baseline promotes when there is no incumbent to beat.
        let mut card = challenger_card(0.5, 0.5, 0.5, false, 0.9);
        card.vs_baseline = None;
        let decision = gate_and_promote(&db, "v1", &card, &PromotionPolicy::default()).unwrap();
        assert!(decision.promote, "the first model must become champion: {:?}", decision.reasons);
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "v1");
    }

    #[test]
    fn gate_and_promote_swaps_in_a_qualified_challenger() {
        let db = open();
        // Incumbent champion with a recorded gold CER.
        register_candidate(&db, &candidate("champ", "omniasr-7b", "meta-stock", "shaA")).unwrap();
        record_eval_result(&db, "champ", 0.20, 0.10, 0.08, 0.12, None, "{}", None).unwrap();
        promote_to_champion(&db, "champ").unwrap();
        // Challenger that significantly beats WER and lowers CER (0.06 < 0.10).
        register_candidate(&db, &candidate("chall", "omniasr-7b", "user-finetuned", "shaB")).unwrap();
        let card = challenger_card(0.10, 0.06, 0.20, true, 0.01);
        let decision = gate_and_promote(&db, "chall", &card, &PromotionPolicy::default()).unwrap();
        assert!(decision.promote, "a qualified challenger must promote: {:?}", decision.reasons);
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "chall");
        assert_eq!(get_model_version(&db, "champ").unwrap().unwrap().status, "rolled_back");
    }

    #[test]
    fn gate_and_promote_keeps_champion_when_challenger_regresses_cer() {
        let db = open();
        register_candidate(&db, &candidate("champ", "omniasr-7b", "meta-stock", "shaA")).unwrap();
        record_eval_result(&db, "champ", 0.20, 0.10, 0.08, 0.12, None, "{}", None).unwrap();
        promote_to_champion(&db, "champ").unwrap();
        register_candidate(&db, &candidate("chall", "omniasr-7b", "user-finetuned", "shaB")).unwrap();
        // Beats WER but regresses CER (0.15 > 0.10) -> blocked; the champion is untouched.
        let card = challenger_card(0.10, 0.15, 0.20, true, 0.01);
        let decision = gate_and_promote(&db, "chall", &card, &PromotionPolicy::default()).unwrap();
        assert!(!decision.promote, "a CER-regressing challenger must be blocked");
        assert_eq!(get_champion(&db, "omniasr-7b").unwrap().unwrap().id, "champ", "champion is unchanged");
        assert_eq!(get_model_version(&db, "chall").unwrap().unwrap().status, "candidate");
    }

    #[test]
    fn gate_and_promote_errors_for_unknown_challenger() {
        let db = open();
        let card = challenger_card(0.1, 0.05, 0.2, true, 0.01);
        assert!(gate_and_promote(&db, "ghost", &card, &PromotionPolicy::default()).is_err());
    }
}
