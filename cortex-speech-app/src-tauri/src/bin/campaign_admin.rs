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

const MIN_CAMPAIGN_SCHEMA_VERSION: i64 = 61;

fn open_database(path: &Path, read_only: bool) -> Result<(Database, i64), String> {
    let path = path.to_str().ok_or_else(|| "database path is not valid Unicode".to_string())?;
    let db = if read_only { Database::open_detached_read_snapshot(path) } else { Database::open(path) }
        .map_err(|error| format!("cannot open campaign database: {error}"))?;
    let schema_version = cortex_speech_app_lib::migrations::validate_applied_history(db.connection())
        .map_err(|error| format!("campaign database migration history is invalid: {error}"))?;
    Ok((db, schema_version))
}

fn require_campaign_schema(schema_version: i64, command: &str) -> Result<(), String> {
    if schema_version < MIN_CAMPAIGN_SCHEMA_VERSION {
        return Err(format!(
            "{command} requires schema {MIN_CAMPAIGN_SCHEMA_VERSION} or newer, found schema {schema_version}; start the tested release once to perform the normal application migration, then retry"
        ));
    }
    Ok(())
}

fn run() -> Result<serde_json::Value, String> {
    run_with(std::env::args().skip(1).collect())
}

fn run_with(args: Vec<String>) -> Result<serde_json::Value, String> {
    let command = args.first().map(String::as_str).ok_or_else(|| {
        "usage: campaign_admin <activate-second-pass|adjudicate|certify|export> --db <path> ...".to_string()
    })?;
    let allowed = match command {
        "activate-second-pass" => ["--db", "--focus", "--expected-max-review-event-id"].as_slice(),
        "adjudicate" => ["--db", "--manual"].as_slice(),
        "certify" => ["--db", "--focus"].as_slice(),
        "export" => ["--db", "--output", "--voice"].as_slice(),
        other => return Err(format!("unknown campaign_admin command: {other}")),
    };
    validate_flags(&args, allowed)?;
    let db_path = PathBuf::from(value_after(&args, "--db")?);
    if !db_path.is_file() {
        return Err(format!("database does not exist: {}", db_path.display()));
    }
    let _instance_lock = if command == "certify" {
        None
    } else {
        let data_dir =
            db_path.parent().ok_or_else(|| format!("database has no parent data directory: {}", db_path.display()))?;
        Some(
            cortex_speech_app_lib::flock::InstanceLock::try_lock(data_dir)
                .map_err(|error| format!("{command} requires Cortex and every writer to be stopped: {error}"))?,
        )
    };
    let read_only = matches!(command, "certify" | "export");
    let (db, schema_version) = open_database(&db_path, read_only)?;
    match command {
        "activate-second-pass" => {
            require_campaign_schema(schema_version, command)?;
            let focus_path = PathBuf::from(value_after(&args, "--focus")?);
            let expected: i64 = value_after(&args, "--expected-max-review-event-id")?
                .parse()
                .map_err(|_| "--expected-max-review-event-id must be a non-negative integer".to_string())?;
            if expected < 0 {
                return Err("--expected-max-review-event-id must be non-negative".to_string());
            }
            let focus: FocusFile = read_json(&focus_path, "voice focus")?;
            let supplied_count = focus.segment_ids.len();
            let ids: HashSet<String> = focus.segment_ids.into_iter().collect();
            if ids.len() != supplied_count {
                return Err("voice focus contains duplicate segment ids".to_string());
            }
            let progress = review_campaign::activate_second_pass(&db, &ids, expected)?;
            serde_json::to_value(progress).map_err(|error| error.to_string())
        }
        "adjudicate" => {
            require_campaign_schema(schema_version, command)?;
            let manual = match optional_value_after(&args, "--manual")? {
                Some(path) => read_json::<Vec<ManualAdjudication>>(Path::new(&path), "manual adjudication file")?,
                None => Vec::new(),
            };
            let progress = review_campaign::adjudicate_and_advance(&db, &manual)?;
            serde_json::to_value(progress).map_err(|error| error.to_string())
        }
        "certify" => {
            db.connection()
                .execute_batch("BEGIN DEFERRED")
                .map_err(|error| format!("cannot begin an atomic certification read: {error}"))?;
            let policy = review_campaign::load(&db)?.ok_or_else(|| "no sequential campaign exists".to_string())?;
            let phase = policy.phase();
            let first_pass_status = match optional_value_after(&args, "--focus")? {
                Some(path) if policy.progress.is_none() => {
                    let focus: FocusFile = read_json(Path::new(&path), "voice focus")?;
                    let supplied_count = focus.segment_ids.len();
                    let ids: HashSet<String> = focus.segment_ids.into_iter().collect();
                    if ids.len() != supplied_count {
                        return Err("voice focus contains duplicate segment ids".to_string());
                    }
                    Some(review_campaign::first_pass_status_for_focus(&db, &policy, &ids)?)
                }
                Some(path) => {
                    let focus: FocusFile = read_json(Path::new(&path), "voice focus")?;
                    let supplied_count = focus.segment_ids.len();
                    let ids: HashSet<String> = focus.segment_ids.into_iter().collect();
                    if ids.len() != supplied_count {
                        return Err("voice focus contains duplicate segment ids".to_string());
                    }
                    let file_evidence = review_campaign::focus_evidence(&ids)?;
                    let registered = review_campaign::verify_registered_focus(&db, &policy)?;
                    if file_evidence != registered {
                        return Err("voice focus file disagrees with immutable registered focus".to_string());
                    }
                    None
                }
                None => None,
            };
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
            let quick_check: String = db
                .connection()
                .query_row("PRAGMA quick_check", [], |row| row.get(0))
                .map_err(|error| format!("database quick_check failed to run: {error}"))?;
            let integrity_check =
                db.integrity_check().map_err(|error| format!("database integrity_check failed: {error}"))?;
            let foreign_key_violations: i64 = db
                .connection()
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| row.get(0))
                .map_err(|error| format!("database foreign-key check failed: {error}"))?;
            if quick_check.trim() != "ok" || integrity_check.trim() != "ok" || foreign_key_violations != 0 {
                return Err(format!(
                    "database health proof failed: quick_check={quick_check:?}, integrity_check={integrity_check:?}, foreign_key_violations={foreign_key_violations}"
                ));
            }
            let result = serde_json::json!({
                "schemaVersion": schema_version,
                "databaseQuickCheck": quick_check,
                "databaseIntegrityCheck": integrity_check,
                "foreignKeyViolationCount": foreign_key_violations,
                "campaignId": policy.campaign_id,
                "phase": phase.as_str(),
                "authorizedReviewer": policy.authorized_reviewer(),
                "registeredFocus": registered_focus,
                "firstPassMaxReviewEventId": first_pass,
                "firstPassStatus": first_pass_status,
                "independentPassMaxDecisionId": independent_pass,
                "independentPending": independent_pending,
                "adjudicationCount": policy.progress.as_ref().map(|value| value.adjudication_count),
                "conflictsRemaining": policy.progress.as_ref().map(|value| value.conflicts_remaining),
                "completed": phase == review_campaign::CampaignPhase::Completed,
                "authorityValid": true,
            });
            db.connection()
                .execute_batch("COMMIT")
                .map_err(|error| format!("cannot finish the atomic certification read: {error}"))?;
            Ok(result)
        }
        "export" => {
            require_campaign_schema(schema_version, command)?;
            db.connection()
                .execute_batch("BEGIN DEFERRED")
                .map_err(|error| format!("cannot begin an atomic export read: {error}"))?;
            let output_dir = value_after(&args, "--output")?;
            let voice_name = value_after(&args, "--voice")?;
            let result = cortex_speech_app_lib::production_dataset::export_finalized_voice_dataset(
                &db,
                &cortex_speech_app_lib::production_dataset::ProductionDatasetOptions { output_dir, voice_name },
            )
            .map_err(|error| error.to_string())?;
            db.connection()
                .execute_batch("COMMIT")
                .map_err(|error| format!("cannot finish the atomic export read: {error}"))?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        }
        _ => unreachable!("command was validated before database access"),
    }
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(value) => {
            println!("{}", serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string()));
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("campaign_admin: {error}");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| item.to_string()).collect()
    }

    /// Real file-backed database carrying the full production migration history.
    fn fresh_db(dir: &Path) -> PathBuf {
        let path = dir.join("cortex-speech.db");
        let db = Database::open(path.to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        path
    }

    // ── Argument validation ──────────────────────────────────────────────────────────────────────

    #[test]
    fn validate_flags_accepts_only_known_flag_value_pairs() {
        let allowed = ["--db", "--focus"];
        assert!(validate_flags(&args(&["certify", "--db", "a.db", "--focus", "f.json"]), &allowed).is_ok());
        assert!(validate_flags(&args(&["certify"]), &allowed).is_ok(), "flags are optional at this layer");
        assert!(validate_flags(&args(&["certify", "--output", "x"]), &allowed)
            .unwrap_err()
            .contains("unknown argument for certify: --output"));
        assert!(validate_flags(&args(&["certify", "stray"]), &allowed)
            .unwrap_err()
            .contains("unknown argument for certify: stray"));
        assert_eq!(
            validate_flags(&args(&["certify", "--db", "a.db", "--db", "b.db"]), &allowed).unwrap_err(),
            "duplicate argument: --db"
        );
        assert_eq!(validate_flags(&args(&["certify", "--db"]), &allowed).unwrap_err(), "missing value after --db");
        assert_eq!(
            validate_flags(&args(&["certify", "--db", "--focus"]), &allowed).unwrap_err(),
            "missing value after --db",
            "a --flag can never be consumed as a value"
        );
    }

    #[test]
    fn value_after_and_optional_value_after_extract_or_refuse() {
        let supplied = args(&["certify", "--db", "a.db"]);
        assert_eq!(value_after(&supplied, "--db").unwrap(), "a.db");
        assert_eq!(value_after(&supplied, "--focus").unwrap_err(), "missing --focus");
        assert_eq!(value_after(&args(&["certify", "--db"]), "--db").unwrap_err(), "missing value after --db");
        assert_eq!(optional_value_after(&supplied, "--focus").unwrap(), None);
        assert_eq!(optional_value_after(&supplied, "--db").unwrap(), Some("a.db".to_string()));
        assert_eq!(
            optional_value_after(&args(&["certify", "--focus"]), "--focus").unwrap_err(),
            "missing value after --focus"
        );
    }

    #[test]
    fn read_json_labels_missing_and_malformed_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.json");
        let error = read_json::<FocusFile>(&missing, "voice focus").map(|focus| focus.segment_ids).unwrap_err();
        assert!(error.contains("cannot read voice focus"), "{error}");
        let malformed = dir.path().join("bad.json");
        std::fs::write(&malformed, b"{not json").unwrap();
        let error = read_json::<FocusFile>(&malformed, "voice focus").map(|focus| focus.segment_ids).unwrap_err();
        assert!(error.contains("invalid voice focus"), "{error}");
        std::fs::write(&malformed, br#"{"segment_ids": ["a", "b"]}"#).unwrap();
        assert_eq!(read_json::<FocusFile>(&malformed, "voice focus").unwrap().segment_ids, ["a", "b"]);
    }

    #[test]
    fn require_campaign_schema_refuses_premigration_databases() {
        let error = require_campaign_schema(MIN_CAMPAIGN_SCHEMA_VERSION - 1, "adjudicate").unwrap_err();
        assert!(error.contains("adjudicate requires schema 61 or newer, found schema 60"), "{error}");
        assert!(require_campaign_schema(MIN_CAMPAIGN_SCHEMA_VERSION, "adjudicate").is_ok());
    }

    #[test]
    fn open_database_validates_migration_history_in_both_modes() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_db(dir.path());
        let (_db, schema_version) = open_database(&path, false).unwrap();
        assert!(schema_version >= MIN_CAMPAIGN_SCHEMA_VERSION, "found schema {schema_version}");
        let (_snapshot, snapshot_version) = open_database(&path, true).unwrap();
        assert_eq!(snapshot_version, schema_version);
    }

    // ── The real command ladder, end to end, against a real database ─────────────────────────────

    #[test]
    fn run_with_refuses_unknown_commands_and_arguments_before_touching_any_database() {
        assert!(run_with(Vec::new()).unwrap_err().starts_with("usage: campaign_admin"));
        assert_eq!(run_with(args(&["demolish"])).unwrap_err(), "unknown campaign_admin command: demolish");
        assert!(run_with(args(&["certify", "--voice", "Kawa"]))
            .unwrap_err()
            .contains("unknown argument for certify: --voice"));
        let error = run_with(args(&["certify", "--db", "Z:/no/such/cortex-speech.db"])).unwrap_err();
        assert!(error.contains("database does not exist"), "{error}");
    }

    #[test]
    fn activate_second_pass_validates_its_inputs_then_hits_the_real_campaign_fence() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(dir.path());
        let db_arg = db_path.to_str().unwrap();
        let focus_path = dir.path().join("focus.json");

        let error = run_with(args(&[
            "activate-second-pass",
            "--db",
            db_arg,
            "--focus",
            focus_path.to_str().unwrap(),
            "--expected-max-review-event-id",
            "ten",
        ]))
        .unwrap_err();
        assert_eq!(error, "--expected-max-review-event-id must be a non-negative integer");

        let error = run_with(args(&[
            "activate-second-pass",
            "--db",
            db_arg,
            "--focus",
            focus_path.to_str().unwrap(),
            "--expected-max-review-event-id",
            "-3",
        ]))
        .unwrap_err();
        assert_eq!(error, "--expected-max-review-event-id must be non-negative");

        std::fs::write(&focus_path, br#"{"segment_ids": ["seg-1", "seg-1"]}"#).unwrap();
        let error = run_with(args(&[
            "activate-second-pass",
            "--db",
            db_arg,
            "--focus",
            focus_path.to_str().unwrap(),
            "--expected-max-review-event-id",
            "0",
        ]))
        .unwrap_err();
        assert_eq!(error, "voice focus contains duplicate segment ids");

        // A clean focus reaches the real activation API, which fails closed without a campaign.
        std::fs::write(&focus_path, br#"{"segment_ids": ["seg-1", "seg-2"]}"#).unwrap();
        let error = run_with(args(&[
            "activate-second-pass",
            "--db",
            db_arg,
            "--focus",
            focus_path.to_str().unwrap(),
            "--expected-max-review-event-id",
            "0",
        ]))
        .unwrap_err();
        assert_eq!(error, "no sequential review campaign is active");
    }

    #[test]
    fn adjudicate_parses_the_manual_file_then_hits_the_real_campaign_fence() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(dir.path());
        let db_arg = db_path.to_str().unwrap();

        let manual_path = dir.path().join("manual.json");
        std::fs::write(&manual_path, b"[{").unwrap();
        let error =
            run_with(args(&["adjudicate", "--db", db_arg, "--manual", manual_path.to_str().unwrap()])).unwrap_err();
        assert!(error.contains("invalid manual adjudication file"), "{error}");

        // Without --manual the command still fails closed: no campaign exists to adjudicate.
        let error = run_with(args(&["adjudicate", "--db", db_arg])).unwrap_err();
        assert_eq!(error, "no sequential review campaign is active");
    }

    #[test]
    fn certify_and_export_fail_closed_on_a_healthy_database_without_a_campaign() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(dir.path());
        let db_arg = db_path.to_str().unwrap();

        let error = run_with(args(&["certify", "--db", db_arg])).unwrap_err();
        assert_eq!(error, "no sequential campaign exists");

        let output_dir = dir.path().join("out");
        let error =
            run_with(args(&["export", "--db", db_arg, "--output", output_dir.to_str().unwrap(), "--voice", "Kawa"]))
                .unwrap_err();
        assert!(error.contains("no completed sequential campaign"), "{error}");
    }
}
