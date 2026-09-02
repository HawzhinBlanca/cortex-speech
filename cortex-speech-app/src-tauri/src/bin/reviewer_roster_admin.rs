//! Offline, fail-closed maintenance for the durable Couch Review roster.
//!
//! The desktop and every writer must be stopped. Existing reviewers keep their durable pairing
//! credentials; missing reviewers receive fresh credentials through the same production session
//! lifecycle used by Settings. No secret URL or token is printed.

use cortex_speech_app_lib::{couch, flock::InstanceLock};
use std::path::{Path, PathBuf};

fn usage() -> &'static str {
    "Usage: reviewer_roster_admin --data-dir <app-data-dir> --db <cortex-speech.db> --reviewer <name> [--reviewer ...]"
}

fn value_after(args: &[String], flag: &str) -> Result<String, String> {
    let index = args.iter().position(|arg| arg == flag).ok_or_else(|| format!("missing {flag}"))?;
    args.get(index + 1).cloned().ok_or_else(|| format!("missing value after {flag}"))
}

fn reviewers(args: &[String]) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--reviewer" {
            let value = args.get(index + 1).ok_or_else(|| "missing value after --reviewer".to_string())?;
            names.push(value.clone());
            index += 2;
        } else {
            index += 1;
        }
    }
    if names.is_empty() {
        return Err("at least one --reviewer is required".to_string());
    }
    Ok(names)
}

fn validate_db_location(db_path: &Path, data_dir: &Path) -> Result<(), String> {
    if !db_path.is_file() {
        return Err(format!("database does not exist: {}", db_path.display()));
    }
    if db_path.parent() != Some(data_dir) {
        return Err("--db must be the database directly inside --data-dir".to_string());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data_dir = PathBuf::from(value_after(&args, "--data-dir").map_err(|error| format!("{error}\n{}", usage()))?);
    let db_path = PathBuf::from(value_after(&args, "--db").map_err(|error| format!("{error}\n{}", usage()))?);
    let requested = reviewers(&args).map_err(|error| format!("{error}\n{}", usage()))?;

    validate_db_location(&db_path, &data_dir)?;

    let _instance_lock = InstanceLock::try_lock(&data_dir)
        .map_err(|error| format!("roster maintenance requires Cortex and every writer to be stopped: {error}"))?;
    let status = couch::start(db_path.to_string_lossy().into_owned(), requested, Some(data_dir))?;
    let mut active: Vec<String> = status.reviewers.into_iter().map(|reviewer| reviewer.name).collect();
    active.sort();

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "durableSessionPrepared": status.running,
            "reviewers": active,
            "reviewerCount": active.len(),
            "certificateFingerprintPresent": status.certificate_fingerprint.is_some(),
            "secretsPrinted": false,
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn value_after_extracts_the_flag_value_or_refuses() {
        let supplied = args(&["--data-dir", "C:/data", "--db", "C:/data/cortex-speech.db"]);
        assert_eq!(value_after(&supplied, "--data-dir").unwrap(), "C:/data");
        assert_eq!(value_after(&supplied, "--db").unwrap(), "C:/data/cortex-speech.db");
        assert_eq!(value_after(&supplied, "--reviewer").unwrap_err(), "missing --reviewer");
        assert_eq!(value_after(&args(&["--db"]), "--db").unwrap_err(), "missing value after --db");
    }

    #[test]
    fn reviewers_collects_every_repeated_flag_and_requires_at_least_one() {
        let supplied =
            args(&["--data-dir", "C:/data", "--reviewer", "Sara", "--reviewer", "Hemn", "--reviewer", "Nechir"]);
        assert_eq!(reviewers(&supplied).unwrap(), ["Sara", "Hemn", "Nechir"]);
        assert_eq!(reviewers(&args(&["--data-dir", "C:/data"])).unwrap_err(), "at least one --reviewer is required");
        assert_eq!(
            reviewers(&args(&["--reviewer", "Sara", "--reviewer"])).unwrap_err(),
            "missing value after --reviewer"
        );
    }

    #[test]
    fn validate_db_location_requires_an_existing_db_directly_inside_the_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cortex-speech.db");
        let error = validate_db_location(&db_path, dir.path()).unwrap_err();
        assert!(error.contains("database does not exist"), "{error}");

        std::fs::write(&db_path, b"sqlite bytes").unwrap();
        assert!(validate_db_location(&db_path, dir.path()).is_ok());

        // A database nested any deeper than the data dir itself is refused: the instance lock
        // guards exactly one directory, and the db must live under that guard.
        let nested_dir = dir.path().join("nested");
        std::fs::create_dir(&nested_dir).unwrap();
        let nested_db = nested_dir.join("cortex-speech.db");
        std::fs::write(&nested_db, b"sqlite bytes").unwrap();
        assert_eq!(
            validate_db_location(&nested_db, dir.path()).unwrap_err(),
            "--db must be the database directly inside --data-dir"
        );
    }
}
