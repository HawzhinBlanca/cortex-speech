// M0.6 / P0.2: Bake the git SHA into the binary for the exe-is-HEAD freshness assertion.
use std::process::Command;

fn main() {
    tauri_build::build();

    // The baked SHA is only trustworthy if cargo re-runs this script whenever HEAD moves. Without
    // these rerun-if-changed hints cargo caches the build-script output, so a `cargo build` after a
    // new commit re-links the crate with a STALE GIT_SHA (the exact bug the freshness gate exists to
    // catch — it would then flag a correct exe, or worse, pass a stale one). `.git/logs/HEAD` is the
    // reflog: it is appended on every HEAD change (commit, checkout, reset); `.git/HEAD` changes on
    // branch switch. Resolve the real git dir so this works in worktrees/submodules too.
    if let Some(git_dir) = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={git_dir}/logs/HEAD");
    }

    // Capture the current git commit SHA at build time.
    let git_sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|output| if output.status.success() { String::from_utf8(output.stdout).ok() } else { None })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_SHA={git_sha}");
}
