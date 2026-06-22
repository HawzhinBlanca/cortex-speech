//! Live verification of the Rust ElevenLabs Scribe client (IGNORED by default — needs a real
//! ELEVENLABS_API_KEY in secrets.env + network). It proves the Rust multipart request actually works
//! against the API, not just that the unit-tested parser is correct.
//!
//! Run:  cargo test --test scribe_live -- --ignored --nocapture
//!
//! The key is read from the local secrets file and NEVER printed; only the transcription is shown.

fn read_elevenlabs_key() -> Option<String> {
    let path = std::path::Path::new(&std::env::var("APPDATA").ok()?).join("cortex-speech").join("secrets.env");
    let contents = std::fs::read_to_string(path).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ELEVENLABS_API_KEY=") {
            let v = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

#[test]
#[ignore = "live: needs ELEVENLABS_API_KEY + network"]
fn scribe_transcribes_nawras_in_sorani() {
    let key = match read_elevenlabs_key() {
        Some(k) => k,
        None => {
            eprintln!("skip: no ELEVENLABS_API_KEY in secrets.env");
            return;
        }
    };
    let audio = format!("{}\\Desktop\\Nawras - KU.wav", std::env::var("USERPROFILE").unwrap());
    let text = cortex_speech_app_lib::scribe_api::transcribe(
        &audio,
        &key,
        cortex_speech_app_lib::scribe_api::DEFAULT_MODEL,
        "kur",
    )
    .expect("scribe transcription should succeed");
    println!("SCRIBE_RUST_RESULT:: {text}");
    assert!(text.chars().count() > 20, "expected a real transcription, got: {text}");
    assert!(text.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)), "expected Sorani (Arabic-script) characters");
}
