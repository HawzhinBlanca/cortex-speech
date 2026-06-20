//! A runnable demonstration of "the app learns from your corrections", using the REAL backend code
//! the app runs (record_human_decision to capture, load_correction_memories + apply_memories to
//! apply). No mocks. Run it with:
//!
//!   cargo test --test loop0_demo -- --nocapture
//!
//! It walks through a realistic Central Kurdish mishearing: the ASR hears "پاش" (after) where the
//! speaker said "باش" (good) — a natural p/b confusion — and shows the app fixing it on its own
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

    let heard = "ئەو کارە زۆر پاش بوو"; // what the ASR mishears ("پاش" = after)
    let correct = "ئەو کارە زۆر باش بوو"; // what was actually said ("باش" = good)

    println!("\n===========================================================");
    println!("  DEMO — the app learns from your corrections");
    println!("===========================================================\n");

    println!("FIRST CLIP");
    println!("  App transcribed : {heard}");
    println!("  You corrected to: {correct}");
    transcribe_one(&db, "clip-1", heard);
    db.record_human_decision("clip-1", "edit", Some(correct)).expect("apply your correction");
    println!("  (the app quietly remembered: \"پاش\" here should be \"باش\")\n");

    println!("SECOND CLIP — the same mistake happens again");
    println!("  App transcribed : {heard}");
    println!("  You corrected to: {correct}");
    transcribe_one(&db, "clip-2", heard);
    db.record_human_decision("clip-2", "edit", Some(correct)).expect("apply your correction");
    println!("  (you have now confirmed this fix twice -> the app trusts it enough to act on it)\n");

    // This is exactly what the app does on a new transcript when auto-correct is turned ON.
    let memories = db.load_correction_memories().expect("load what was learned");
    println!("THIRD CLIP — later, a brand-new clip with the same mistake");
    println!("  App transcribed       : {heard}");
    let auto_fixed = apply_memories(heard, &memories, &FiringConfig::default());
    println!("  With auto-correct ON  : {auto_fixed}");
    println!("                          ^^^ fixed automatically, with no retraining\n");

    assert_eq!(auto_fixed, correct, "the learned correction should be applied on its own");

    // And the safety check: a DIFFERENT, already-correct sentence is left untouched (no over-correcting).
    let unrelated = "سبەینێ دەچمە بازاڕ";
    let untouched = apply_memories(unrelated, &memories, &FiringConfig::default());
    println!("SAFETY CHECK — an unrelated, already-correct sentence");
    println!("  In  : {unrelated}");
    println!("  Out : {untouched}   (left alone — it only fixes the exact thing you corrected)\n");
    assert_eq!(untouched, unrelated, "unrelated text must not be altered");

    println!("===========================================================");
    println!("  RESULT: the word you fixed twice is now corrected by the");
    println!("  app on its own, and nothing else was touched.");
    println!("===========================================================\n");
}
