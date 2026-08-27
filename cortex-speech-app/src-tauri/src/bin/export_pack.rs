//! Export a fine-tune pack from the live library and SEAL its snapshot — headless.
//!
//! ```text
//! export_pack <output-dir>
//! ```
//!
//! Phase 3 of docs/PLAN_TRUE_10.md needs the challenger chain to run with zero manual steps, and its
//! first link had no command: `export_finetune_pack` existed only behind a Tauri IPC, so sealing a
//! snapshot meant clicking through the desktop app. This is that link.
//!
//! Takes the same instance lock as the desktop for the complete read + seal operation. The seal is a
//! write, and a concurrent restore could otherwise export old-generation rows then attach the seal
//! to the restored generation. Close the app before running this maintenance binary.

use cortex_speech_app_lib::db::Database;
use std::path::PathBuf;

fn app_data_dir() -> PathBuf {
    std::env::var_os("CORTEX_APP_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("cortex-speech")))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("cortex-speech"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let out_dir = std::env::args().nth(1).map(PathBuf::from).ok_or("Usage: export_pack <output-dir>")?;
    let data_dir = app_data_dir();
    let _instance_lock = cortex_speech_app_lib::flock::InstanceLock::try_lock(&data_dir).map_err(|error| {
        format!("Cannot export a sealed pack while Cortex is running: {error}. Stop review and close the app first.")
    })?;
    let db_path = data_dir.join("cortex-speech.db");
    let db = Database::open_with_retry(&db_path.to_string_lossy())?;

    // The durable corpus ledger lives beside the library, so a pack's provenance survives the pack
    // being deleted.
    let ledger = data_dir.join("corpus_ledger.jsonl");

    println!("exporting fine-tune pack from {} -> {}", db_path.display(), out_dir.display());
    let result = cortex_speech_app_lib::eval::export_finetune_pack(&db, &out_dir, Some(&ledger))?;

    println!();
    println!("  verified candidates       : {}", result.total_verified);
    println!("  excluded (holdout leak)   : {}", result.excluded_unexportable);
    println!("  excluded (not training-ready): {}", result.excluded_not_training_ready);
    println!("  skipped (empty/dup/undecodable): {}", result.skipped);
    println!("  emitted                   : {}", result.emitted);
    println!("  of those, no human decision: {}", result.emitted_without_human_decision);
    println!("  snapshot id               : {}", result.snapshot_id);
    println!("  newly sealed              : {}", result.newly_sealed);

    if result.emitted == 0 {
        // An empty pack is not a successful export: there is nothing to train on, and returning 0
        // would let a pipeline treat it as a finished step.
        return Err("pack is EMPTY — no training-ready rows survived the guards".into());
    }
    println!();
    println!("pack written to {}", out_dir.display());
    Ok(())
}
