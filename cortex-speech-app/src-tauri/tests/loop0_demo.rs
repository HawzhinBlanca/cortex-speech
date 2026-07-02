//! A runnable demonstration of "the app learns from your corrections", using the REAL backend code
//! the app runs (record_human_decision to capture, load_correction_memories + apply_memories to
//! apply). No mocks. Run it with:
//!
//!   cargo test --test loop0_demo -- --nocapture
//!
//! It walks through a realistic Central Kurdish mishearing: the ASR hears "Ù¾Ø§Ø´" (after) where the
//! speaker said "Ø¨Ø§Ø´" (good) â€” a natural p/b confusion â€” and shows the app fixing it on its own
//! after you have corrected it.

use cortex_speech_app_lib::corrections::{apply_memories, FiringConfig};
use cortex_speech_app_lib::db::{Database, SpeechSegment};

fn transcribe_one(db: &Database, id: &str, heard: &str) {
    db.insert_segment(&SpeechSegment {
        id: id.to_string(),
        audio_path: format!("/clips/{id}.wav"),
        raw_transcript: heard.to_string(),
        ..Default::default()
    })
    .expect("save segment");
}

#[test]
fn demo_app_learns_from_your_corrections() {
    let db = Database::open(":memory:").expect("open db");
    db.initialize().expect("set up db");

    let heard = "Ø¦Û•Ùˆ Ú©Ø§Ø±Û• Ø²Û†Ø± Ù¾Ø§Ø´ Ø¨ÙˆÙˆ"; // what the ASR mishears ("Ù¾Ø§Ø´" = after)
    let correct = "Ø¦Û•Ùˆ Ú©Ø§Ø±Û• Ø²Û†Ø± Ø¨Ø§Ø´ Ø¨ÙˆÙˆ"; // what was actually said ("Ø¨Ø§Ø´" = good)

    println!("\n===========================================================");
    println!("  DEMO â€” the app learns from your corrections");
    println!("===========================================================\n");

    println!("FIRST CLIP");
    println!("  App transcribed : {heard}");
    println!("  You corrected to: {correct}");
    transcribe_one(&db, "clip-1", heard);
    db.record_human_decision("clip-1", "edit", Some(correct), None).expect("apply your correction");
    println!("  (the app quietly remembered: \"Ù¾Ø§Ø´\" here should be \"Ø¨Ø§Ø´\")\n");

    println!("SECOND CLIP â€” the same mistake happens again");
    println!("  App transcribed : {heard}");
    println!("  You corrected to: {correct}");
    transcribe_one(&db, "clip-2", heard);
    db.record_human_decision("clip-2", "edit", Some(correct), None).expect("apply your correction");
    println!("  (you have now confirmed this fix twice -> the app trusts it enough to act on it)\n");

    // This is exactly what the app does on a new transcript when auto-correct is turned ON.
    let memories = db.load_correction_memories().expect("load what was learned");
    println!("THIRD CLIP â€” later, a brand-new clip with the same mistake");
    println!("  App transcribed       : {heard}");
    let auto_fixed = apply_memories(heard, &memories, &FiringConfig::default());
    println!("  With auto-correct ON  : {auto_fixed}");
    println!("                          ^^^ fixed automatically, with no retraining\n");

    assert_eq!(auto_fixed, correct, "the learned correction should be applied on its own");

    // And the safety check: a DIFFERENT, already-correct sentence is left untouched (no over-correcting).
    let unrelated = "Ø³Ø¨Û•ÛŒÙ†ÛŽ Ø¯Û•Ú†Ù…Û• Ø¨Ø§Ø²Ø§Ú•";
    let untouched = apply_memories(unrelated, &memories, &FiringConfig::default());
    println!("SAFETY CHECK â€” an unrelated, already-correct sentence");
    println!("  In  : {unrelated}");
    println!("  Out : {untouched}   (left alone â€” it only fixes the exact thing you corrected)\n");
    assert_eq!(untouched, unrelated, "unrelated text must not be altered");

    println!("===========================================================");
    println!("  RESULT: the word you fixed twice is now corrected by the");
    println!("  app on its own, and nothing else was touched.");
    println!("===========================================================\n");
}
