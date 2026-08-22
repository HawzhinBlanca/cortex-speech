//! Offline, fail-closed administration for the sequential Rubar -> Alle review campaign.
//!
//! This binary deliberately has no "force" switch. Phase changes are compare-and-swap operations
//! whose evidence is revalidated by the serving/export code on every later load.

use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::review_campaign::{self, ManualAdjudication};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FocusFile {
    segment_ids: Vec<String>,
}

fn validate_flags(args: &[String], allowed: &[&str]) -> Result<(), String> {
    let mut seen = HashSet::new();
    let mut index = 1usize;
    while index < args.len() {
        let flag = args[index].as_str();
        if !flag.starts_with("--") || !allowed.contains(&flag) {
            return Err(format!("unknown argument for {}: {flag}", args[0]));
        }
        if !seen.insert(flag.to_string()) {
            return Err(format!("duplicate argument: {flag}"));
        }
        let Some(value) = args.get(index + 1) else {
            return Err(format!("missing value after {flag}"));
        };
        if value.starts_with("--") {
            return Err(format!("missing value after {flag}"));
        }
        index += 2;
    }
    Ok(())
}

fn value_after(args: &[String], flag: &str) -> Result<String, String> {
    let index = args.iter().position(|arg| arg == flag).ok_or_else(|| format!("missing {flag}"))?;
    args.get(index + 1).cloned().ok_or_else(|| format!("missing value after {flag}"))
}

fn optional_value_after(args: &[String], flag: &str) -> Result<Option<String>, String> {
    match args.iter().position(|arg| arg == flag) {
        Some(index) => args.get(index + 1).cloned().map(Some).ok_or_else(|| format!("missing value after {flag}")),
        None => Ok(None),
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, label: &str) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("cannot read {label} {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid {label} {}: {error}", path.display()))
}

fn open_database(path: &Path) -> Result<Database, String> {
    let path = path.to_str().ok_or_else(|| "database path is not valid Unicode".to_string())?;
    let db = Database::open(path).map_err(|error| format!("cannot open campaign database: {error}"))?;
    db.initialize().map_err(|error| format!("cannot initialize campaign database: {error}"))?;
    Ok(db)
}

fn run() -> Result<serde_json::Value, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).ok_or_else(|| {
        "usage: campaign_admin <activate-second-pass|adjudicate|certify|export> --db <path> ...".to_string()
    })?;
    let allowed = match command {
        "activate-second-pass" => ["--db", "--focus", "--expected-max-review-event-id"].as_slice(),
        "adjudicate" => ["--db", "--manual"].as_slice(),
        "certify" => ["--db"].as_slice(),
        "export" => ["--db", "--output", "--voice"].as_slice(),
        other => return Err(format!("unknown campaign_admin command: {other}")),
    };
    validate_flags(&args, allowed)?;
    let db_path = PathBuf::from(value_after(&args, "--db")?);
    if !db_path.is_file() {
        return Err(format!("database does not exist: {}", db_path.display()));
    }
    let db = open_database(&db_path)?;
    match command {
        "activate-second-pass" => {
            let focus_path = PathBuf::from(value_after(&args, "--focus")?);
            let expected: i64 = value_after(&args, "--expected-max-review-event-id")?
                .parse()
                .map_err(|_| "--expected-max-review-event-id must be a non-negative integer".to_string())?;
            if expected < 0 {
                return Err("--expected-max-review-event-id must be non-negative".to_string());
            }
            let focus: FocusFile = read_json(&focus_path, "voice focus")?;
            let ids: HashSet<String> = focus.segment_ids.into_iter().collect();
            let progress = review_campaign::activate_second_pass(&db, &ids, expected)?;
            serde_json::to_value(progress).map_err(|error| error.to_string())
        }
        "adjudicate" => {
            let manual = match optional_value_after(&args, "--manual")? {
                Some(path) => read_json::<Vec<ManualAdjudication>>(Path::new(&path), "manual adjudication file")?,
                None => Vec::new(),
            };
            let progress = review_campaign::adjudicate_and_advance(&db, &manual)?;
            serde_json::to_value(progress).map_err(|error| error.to_string())
        }
        "certify" => {
            let policy = review_campaign::load(&db)?.ok_or_else(|| "no sequential campaign exists".to_string())?;
            let phase = policy.phase();
            let (registered_focus, first_pass) = if policy.progress.is_some() {
                (
                    Some(review_campaign::verify_registered_focus(&db, &policy)?),
                    Some(review_campaign::verify_first_pass_complete(&db, &policy)?),
                )
            } else {
                (None, None)
            };
            let independent_pass = if matches!(
                phase,
                review_campaign::CampaignPhase::AdjudicationActive | review_campaign::CampaignPhase::Completed
            ) {
                Some(review_campaign::verify_independent_pass_complete(&db, &policy)?)
            } else {
                None
            };
            let independent_pending = if phase == review_campaign::CampaignPhase::SecondPassActive {
                Some(review_campaign::independent_pending_segment_ids(&db, &policy)?.len())
            } else {
                None
            };
            if phase == review_campaign::CampaignPhase::Completed {
                review_campaign::verify_campaign_completion(&db, &policy)?;
            }
            Ok(serde_json::json!({
                "campaignId": policy.campaign_id,
                "phase": phase.as_str(),
                "authorizedReviewer": policy.authorized_reviewer(),
                "registeredFocus": registered_focus,
                "firstPassMaxReviewEventId": first_pass,
                "independentPassMaxDecisionId": independent_pass,
                "independentPending": independent_pending,
                "adjudicationCount": policy.progress.as_ref().map(|value| value.adjudication_count),
                "conflictsRemaining": policy.progress.as_ref().map(|value| value.conflicts_remaining),
                "completed": phase == review_campaign::CampaignPhase::Completed,
                "authorityValid": true,
            }))
        }
        "export" => {
            let output_dir = value_after(&args, "--output")?;
            let voice_name = value_after(&args, "--voice")?;
            let result = cortex_speech_app_lib::production_dataset::export_finalized_voice_dataset(
                &db,
                &cortex_speech_app_lib::production_dataset::ProductionDatasetOptions { output_dir, voice_name },
            )
            .map_err(|error| error.to_string())?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        }
        _ => unreachable!("command was validated before database access"),
    }
}

fn main() {
    match run() {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())),
        Err(error) => {
            eprintln!("campaign_admin: {error}");
            std::process::exit(2);
        }
    }
}
