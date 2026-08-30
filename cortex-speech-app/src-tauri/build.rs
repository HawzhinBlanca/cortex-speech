// M0.6 / P0.2: Bake the git SHA into the binary for the exe-is-HEAD freshness assertion.
use std::process::Command;

/// Tauri's Windows dialog/error path imports `TaskDialogIndirect` from Common-Controls v6. The
/// desktop executable receives Tauri's application manifest, but Cargo-generated Rust test
/// harnesses do not. Keep this input scoped to test targets: normal Tauri binaries already own a
/// manifest and Windows rejects a second manifest resource with CVT1100.
#[cfg(target_os = "windows")]
const WINDOWS_TEST_COMMON_CONTROLS_V6: &str = "windows-test-common-controls.manifest";
#[cfg(target_os = "windows")]
const WINDOWS_TEST_COMMON_CONTROLS_RESOURCE: &str = "windows-test-common-controls.rc";
#[cfg(target_os = "windows")]
const WINDOWS_TEST_COMMON_CONTROLS_ARCHIVE: &str = "windows-test-common-controls-object-archive.lib";

fn canonical_git_sha(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then(|| value.to_string())
}

fn main() {
    tauri_build::build();
    println!("cargo:rerun-if-env-changed=CORTEX_BUILD_GIT_SHA");
    #[cfg(target_os = "windows")]
    {
        let manifest_dir = std::path::PathBuf::from(
            std::env::var("CARGO_MANIFEST_DIR").expect("Cargo must provide CARGO_MANIFEST_DIR"),
        );
        let manifest = manifest_dir.join(WINDOWS_TEST_COMMON_CONTROLS_V6);
        let resource = manifest_dir.join(WINDOWS_TEST_COMMON_CONTROLS_RESOURCE);
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rerun-if-changed={}", resource.display());
        embed_resource::compile_for_tests(&resource, embed_resource::NONE)
            .manifest_required()
            .expect("Windows test Common-Controls v6 resource must compile");
        let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("Cargo must provide OUT_DIR"));
        let compiled_resource = out_dir.join("windows-test-common-controls.lib");
        let compiled_object = out_dir.join("windows-test-common-controls.obj");
        let archive = out_dir.join(WINDOWS_TEST_COMMON_CONTROLS_ARCHIVE);
        let target = std::env::var("TARGET").expect("Cargo must provide TARGET");
        let machine = if target.starts_with("x86_64-") {
            "X64"
        } else if target.starts_with("i686-") {
            "X86"
        } else if target.starts_with("aarch64-") {
            "ARM64"
        } else {
            panic!("unsupported Windows test-resource target: {target}");
        };
        let resource_converter = cc::windows_registry::find_tool(&target, "cvtres.exe")
            .expect("the MSVC resource converter is required for Windows test resources");
        let status = resource_converter
            .to_command()
            .arg("/NOLOGO")
            .arg(format!("/MACHINE:{machine}"))
            .arg(format!("/OUT:{}", compiled_object.display()))
            .arg(&compiled_resource)
            .status()
            .expect("the MSVC resource converter must start");
        assert!(status.success(), "the Windows test resource must convert to COFF");
        let library_tool = cc::windows_registry::find_tool(&target, "lib.exe")
            .expect("the MSVC library manager is required for Windows test resources");
        let status = library_tool
            .to_command()
            .arg("/NOLOGO")
            .arg(format!("/MACHINE:{machine}"))
            .arg(format!("/OUT:{}", archive.display()))
            .arg(&compiled_object)
            .status()
            .expect("the MSVC library manager must start");
        assert!(status.success(), "the Windows test resource archive must be created");
        println!("cargo:rustc-link-search=native={}", out_dir.display());
    }
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
    let git_sha = std::env::var("CORTEX_BUILD_GIT_SHA")
        .ok()
        .and_then(|value| canonical_git_sha(&value))
        .or_else(|| {
            Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|output| if output.status.success() { String::from_utf8(output.stdout).ok() } else { None })
                .and_then(|value| canonical_git_sha(&value))
        })
        .unwrap_or_else(|| {
            panic!(
                "Cortex builds require a canonical 40-character lowercase Git SHA; build inside the repository or set CORTEX_BUILD_GIT_SHA for a reproducible source archive"
            )
        });

    println!("cargo:rustc-env=GIT_SHA={git_sha}");
}
