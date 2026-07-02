// M0.6: Bake git SHA into the binary for exe-is-HEAD assertion.
use std::process::Command;

fn main() {
    tauri_build::build();

    // Capture the current git commit SHA at build time
    let git_sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|output| if output.status.success() { String::from_utf8(output.stdout).ok() } else { None })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_SHA={}", git_sha);
}
