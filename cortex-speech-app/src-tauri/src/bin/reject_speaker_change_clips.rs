//! Reject the clips MEASURED to hold a speaker change — the owner's decision, applied in bulk.
//!
//! WHY THIS EXISTS. `speaker_change_probe` measures, per clip, whether its two halves are different
//! people, calibrated against the owner's own blind listening pass (0/15 misclassified at 0.59).
//! Migration v47 stores that score and the phone shows a badge, but a flag is not a decision: 13 of
//! the 17 flagged clips are still pending, and the owner asked for them to be rejected rather than
//! listened to one at a time. This applies exactly that, once.
//!
//! WHY REJECT AND NOT DELETE. A rejected clip stays in the library — `is_human_rejected` keeps it out
//! of every export and every "verified" count, and the decision is reversible by re-reviewing. Nothing
//! here removes audio or transcripts.
//!
//! IT USES THE REAL DECISION PATH. `Database::record_human_decision` is the same call the desktop and
//! the phone make: it validates the decision string, writes verdict + human_decision + corrected_at in
//! one transaction, and leaves the transcript alone. Hand-written SQL could set a combination the app
//! itself can never produce.
//!
//! ONLY PENDING CLIPS. A flagged clip the owner already decided is left exactly as it is — overwriting
//! a human judgement with a bulk rule is precisely what this must not do.
//!
//! DRY RUN BY DEFAULT. Without `--apply` it lists what WOULD change and writes nothing.
//!
//! Usage: reject_speaker_change_clips [--apply] [--data-dir <dir>]

use cortex_speech_app_lib::db::Database;
use cortex_speech_app_lib::diarization::SPEAKER_CHANGE_THRESHOLD;
use std::path::PathBuf;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let apply = args.iter().any(|a| a == "--apply");
    let data_dir: PathBuf = args
        .iter()
        .position(|a| a == "--data-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("cortex-speech"));

    let db_path = data_dir.join("cortex-speech.db").to_string_lossy().to_string();
    let db = Database::open(&db_path).map_err(|e| format!("open {db_path}: {e}"))?;
    println!("db    : {db_path}");
    println!("mode  : {}\n", if apply { "APPLY (writes)" } else { "DRY RUN (writes nothing)" });

    let segments = db.get_segments(None).map_err(|e| e.to_string())?;
    let flagged: Vec<_> = segments
        .iter()
        .filter(|s| s.speaker_change_score.is_some_and(|v| (v as f32) < SPEAKER_CHANGE_THRESHOLD))
        .collect();
    // PENDING only. `verified` is the app's own "this clip has left the review queue" flag, so an
    // already-decided clip is one a human has ruled on — bulk rules do not get to overrule that.
    let (pending, decided): (Vec<&&_>, Vec<&&_>) = flagged.iter().partition(|s| !s.verified);

    println!("flagged (score < {SPEAKER_CHANGE_THRESHOLD}): {}", flagged.len());
    println!("  already decided, LEFT ALONE          : {}", decided.len());
    println!("  pending, to reject                   : {}\n", pending.len());

    let mut done = 0usize;
    for s in &pending {
        let score = s.speaker_change_score.unwrap_or_default();
        println!("  {}  score {score:.4}  speaker {}", &s.id[..8], s.speaker_id.as_deref().unwrap_or("-"));
        if apply {
            // ONE commit (2026-08-20 hunt). The previous two-write version — decision first, then a
            // whole-row upsert to set `verified` — left a kill window in which a clip came back
            // correctly rejected AND still pending, exactly the half-written state the finalize
            // transaction exists to make unrepresentable. `finalize_human_review` records the
            // decision identity, verdict and `verified` atomically; a reject stays in the library,
            // out of exports, and out of every queue.
            db.finalize_human_review(&s.id, "reject", None, None, None).map_err(|e| format!("{}: {e}", s.id))?;
            done += 1;
        }
    }

    if apply {
        println!("\nrejected {done} clip(s).");
        println!("UNDO: re-review any of them in the app or on the phone — the decision is reversible,");
        println!("      the audio and transcripts were never touched.");
    } else {
        println!("\nDRY RUN — nothing was written. Re-run with --apply.");
    }
    Ok(())
}
