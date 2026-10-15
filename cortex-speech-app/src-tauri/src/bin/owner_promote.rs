//! Owner-only champion promotion entry point (v12 plan, Part B step 5). See `owner_promotion.rs`.
//!
//! ```text
//! owner_promote --repo <git checkout> --authorization <committed record .json>
//!               --verdict <promotion_gate deployment verdict.json> --eval-proof <heldout_eval.proof.json>
//!               --canary-suite-id <id> --data-dir <Cortex data dir>
//! ```
//!
//! It ADMITS the durable promotion job and exits; it never swaps the champion itself. The next Cortex
//! start runs the saga through startup recovery (exact-identity health, canary, rollback on failure).
//! Requires Cortex and every writer stopped (instance lock), and an interactive terminal.

use cortex_speech_app_lib::champion_promotion::{self, CanaryIdentity, PromotionContract};
use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::deployment::OMNIASR_7B_FAMILY;
use cortex_speech_app_lib::owner_promotion::{self as owner, OwnerPromotionAuthorization};
use cortex_speech_app_lib::registry;
use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

type Result<T> = std::result::Result<T, String>;

const FLAGS: [&str; 6] = ["--repo", "--authorization", "--verdict", "--eval-proof", "--canary-suite-id", "--data-dir"];

fn parse_args(args: &[String]) -> Result<HashMap<&'static str, String>> {
    let mut values = HashMap::new();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let key = FLAGS.iter().find(|known| *known == flag).ok_or_else(|| format!("unknown argument {flag}"))?;
        let value = iter.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if values.insert(*key, value.clone()).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    match FLAGS.iter().find(|flag| !values.contains_key(*flag)) {
        Some(missing) => Err(format!("missing {missing}")),
        None => Ok(values),
    }
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

/// The bytes of `relative` as committed on the protected `origin/main` (no fetch: run `git fetch` first).
fn show_origin_main(repo: &Path, relative: &str) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("show")
        .arg(format!("origin/main:{relative}"))
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if !output.status.success() {
        return Err(format!("{relative} is not committed on origin/main"));
    }
    Ok(output.stdout)
}

/// Owner authority, part 1+2: the record is byte-identical on origin/main and the canon line it quotes
/// is in `docs/OWNER_CANON.md` on origin/main.
fn verify_committed_authorization(repo: &Path, record: &Path) -> Result<(OwnerPromotionAuthorization, Vec<u8>)> {
    let bytes = read(record)?;
    let auth = owner::parse_authorization(&bytes).map_err(|error| error.to_string())?;
    let repo = repo.canonicalize().map_err(|error| format!("repo: {error}"))?;
    let record_path = record.canonicalize().map_err(|error| format!("authorization: {error}"))?;
    let relative = record_path
        .strip_prefix(&repo)
        .map_err(|_| "the authorization record must live inside --repo".to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    if show_origin_main(&repo, &relative)? != bytes {
        return Err("the authorization record differs from its committed origin/main bytes".into());
    }
    let canon = String::from_utf8(show_origin_main(&repo, "docs/OWNER_CANON.md")?)
        .map_err(|_| "docs/OWNER_CANON.md on origin/main is not UTF-8".to_string())?;
    if !owner::canon_authorizes(&canon, &auth) {
        return Err("docs/OWNER_CANON.md on origin/main does not contain the record's canonLine".into());
    }
    Ok((auth, bytes))
}

/// Owner authority, part 3: a typed phrase naming the exact deployment, on an interactive terminal.
fn require_interactive_confirmation(auth: &OwnerPromotionAuthorization) -> Result<()> {
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    confirm(interactive, &mut std::io::stdin().lock(), auth)
}

fn confirm(interactive: bool, input: &mut impl BufRead, auth: &OwnerPromotionAuthorization) -> Result<()> {
    if !interactive {
        return Err("owner confirmation requires an interactive terminal; piped input is refused".into());
    }
    let phrase = owner::confirmation_phrase(auth);
    print!(
        "Promote {} (deployment {}) over {}.\nType exactly: {phrase}\n> ",
        auth.candidate.model_version_id, auth.candidate.deployment_sha256, auth.incumbent.model_version_id
    );
    std::io::stdout().flush().map_err(|error| error.to_string())?;
    let mut typed = String::new();
    input.read_line(&mut typed).map_err(|error| error.to_string())?;
    if typed.trim_end_matches(['\r', '\n']) != phrase {
        return Err("confirmation phrase did not match; nothing was admitted".into());
    }
    Ok(())
}

fn run(args: &[String]) -> Result<String> {
    let values = parse_args(args)?;
    let path = |key: &str| PathBuf::from(&values[key]);
    let (auth, record_bytes) = verify_committed_authorization(&path("--repo"), &path("--authorization"))?;
    let verdict_bytes = read(&path("--verdict"))?;
    let scorecard_sha256 = owner::verify_gate_evidence(&verdict_bytes, &read(&path("--eval-proof"))?, &auth)
        .map_err(|error| error.to_string())?;

    let data_dir = path("--data-dir");
    let _instance_lock = cortex_speech_app_lib::flock::InstanceLock::try_lock(&data_dir)
        .map_err(|error| format!("owner_promote requires Cortex and every writer to be stopped: {error}"))?;
    let db_path = data_dir.join("cortex-speech.db");
    let mut db = Database::open_with_retry(&db_path.to_string_lossy()).map_err(|error| error.to_string())?;
    let champion = registry::champion_identity(&db, OMNIASR_7B_FAMILY).map_err(|error| error.to_string())?;
    if champion.as_ref().map(|c| (&c.model_version_id, &c.deployment_sha256))
        != Some((&auth.incumbent.model_version_id, &auth.incumbent.deployment_sha256))
    {
        return Err("the registry champion is not the record's incumbent".into());
    }
    let candidate = registry::get_model_version(&db, &auth.candidate.model_version_id)
        .map_err(|error| error.to_string())?
        .ok_or("the candidate is not registered")?;
    if candidate.status != "candidate" || candidate.checkpoint_sha256 != auth.candidate.deployment_sha256 {
        return Err("the registered candidate is not the record's exact deployment".into());
    }
    let suite_id = values["--canary-suite-id"].clone();
    let canary_path = data_dir.join("promotion_canaries").join(format!("{suite_id}.json"));
    let suite_sha256 = owner::sha256_hex(&read(&canary_path)?);

    require_interactive_confirmation(&auth)?;

    let authorization_sha256 =
        owner::store_authorization(&data_dir, &record_bytes).map_err(|error| error.to_string())?;
    let contract = PromotionContract {
        incumbent: auth.incumbent.clone(),
        candidate: auth.candidate.clone(),
        scorecard_sha256,
        verdict_sha256: owner::sha256_hex(&verdict_bytes),
        canary: CanaryIdentity { suite_id, suite_sha256, expected_model: auth.candidate.clone() },
        owner_authorization_sha256: Some(authorization_sha256.clone()),
    };
    let short = authorization_sha256.get(..16).unwrap_or("");
    let job_id = format!("owner-promotion-{short}");
    let key = format!("owner-promotion:{}:{authorization_sha256}", auth.candidate.deployment_sha256);
    let job = champion_promotion::admit_owner_promotion(&mut db, &job_id, &key, contract)
        .map_err(|error| error.to_string())?;
    Ok(job.id)
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(job) => {
            println!(
                "OWNER PROMOTION ADMITTED: job {job}. Start Cortex: startup recovery runs the saga \
                 (exact identity + canary, automatic rollback on any failure)."
            );
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("OWNER PROMOTION REFUSED: {error}");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "user.name=test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn record(canon_line: &str) -> String {
        serde_json::json!({
            "schema": 1, "decision": "promote_champion", "family": "omniasr-7b",
            "incumbent": {"modelVersionId": "omniasr-7b-legacy-c348ade8a816", "deploymentSha256": "a".repeat(64)},
            "candidate": {"modelVersionId": "omniasr-7b-external-0123456789abcdef01234567", "deploymentSha256": "b".repeat(64)},
            "canonLine": canon_line, "ownerWords": "promote v12", "date": "2026-10-20",
        })
        .to_string()
    }

    fn line() -> String {
        format!("change canon: promote omniasr-7b-external-0123456789abcdef01234567 deployment {}", "b".repeat(64))
    }

    /// A repo whose origin/main is HEAD after one commit of `canon` and `record`.
    fn repo_with(canon: &str, record: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        git(dir.path(), &["init", "-q"]);
        std::fs::create_dir_all(dir.path().join("docs/owner_decisions")).unwrap();
        std::fs::write(dir.path().join("docs/OWNER_CANON.md"), canon).unwrap();
        std::fs::write(dir.path().join("docs/owner_decisions/v12.json"), record).unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "owner decision"]);
        git(dir.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        dir
    }

    #[test]
    fn authority_needs_the_record_and_its_canon_line_committed_on_origin_main() {
        let canon = format!("# OWNER CANON\n- v12 (owner \"{}\")\n", line());
        let dir = repo_with(&canon, &record(&line()));
        let record_path = dir.path().join("docs/owner_decisions/v12.json");
        assert!(verify_committed_authorization(dir.path(), &record_path).is_ok());

        // An uncommitted edit of the record is not the owner's committed decision.
        std::fs::write(&record_path, record(&format!("{} ", line()))).unwrap();
        assert!(verify_committed_authorization(dir.path(), &record_path).unwrap_err().contains("differs"));

        // A committed record whose canon line the owner never wrote into the canon is refused.
        let dir = repo_with("# OWNER CANON\n", &record(&line()));
        let record_path = dir.path().join("docs/owner_decisions/v12.json");
        assert!(verify_committed_authorization(dir.path(), &record_path).unwrap_err().contains("canonLine"));
    }

    #[test]
    fn arguments_are_exact_and_complete_and_piped_input_never_confirms() {
        let full: Vec<String> = FLAGS.iter().flat_map(|flag| [flag.to_string(), "x".to_string()]).collect();
        assert!(parse_args(&full).is_ok());
        assert!(parse_args(&full[2..]).unwrap_err().contains("missing --repo"));
        assert!(parse_args(&[full.clone(), vec!["--repo".into(), "y".into()]].concat()).is_err());
        assert!(parse_args(&["--force".to_string()]).is_err());
        let auth = owner::parse_authorization(record(&line()).as_bytes()).unwrap();
        let typed = |text: &str| std::io::Cursor::new(text.as_bytes().to_vec());
        let phrase = format!("{}\r\n", owner::confirmation_phrase(&auth));
        assert!(confirm(false, &mut typed(&phrase), &auth).unwrap_err().contains("interactive terminal"));
        assert!(confirm(true, &mut typed("yes\n"), &auth).unwrap_err().contains("did not match"));
        assert!(confirm(true, &mut typed(&phrase), &auth).is_ok());
    }
}
