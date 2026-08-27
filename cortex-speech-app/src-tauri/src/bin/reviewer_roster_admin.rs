//! Offline, fail-closed maintenance for the durable Couch Review roster.
//!
//! The desktop and every writer must be stopped. Existing reviewers keep their durable pairing
//! credentials; missing reviewers receive fresh credentials through the same production session
//! lifecycle used by Settings. No secret URL or token is printed.

use cortex_speech_app_lib::{couch, flock::InstanceLock};
use std::path::PathBuf;

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data_dir = PathBuf::from(value_after(&args, "--data-dir").map_err(|error| format!("{error}\n{}", usage()))?);
    let db_path = PathBuf::from(value_after(&args, "--db").map_err(|error| format!("{error}\n{}", usage()))?);
    let requested = reviewers(&args).map_err(|error| format!("{error}\n{}", usage()))?;

    if !db_path.is_file() {
        return Err(format!("database does not exist: {}", db_path.display()).into());
    }
    if db_path.parent() != Some(data_dir.as_path()) {
        return Err("--db must be the database directly inside --data-dir".into());
    }

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
