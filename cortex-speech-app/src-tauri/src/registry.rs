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

    PromotionDecision { promote, reasons }
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
}
