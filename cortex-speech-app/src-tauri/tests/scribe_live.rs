//! Live verification of the Rust ElevenLabs Scribe client (IGNORED by default — needs a real
//! ELEVENLABS_API_KEY in secrets.env + network). It proves the Rust multipart request actually works
//! against the API, not just that the unit-tested parser is correct.
//!
//! Run:  cargo test --test scribe_live -- --ignored --nocapture
//!
//! The key is read from the local secrets file and NEVER printed; only the transcription is shown.

/// Read the key through the PRODUCTION loader, not by hand-parsing `secrets.env`.
///
/// `secrets.env` values may be plaintext OR `dpapi:<base64>` — DPAPI-encrypted at rest, which is what
/// `set_api_key` writes. A hand-parser returns the CIPHERTEXT and sends it as a bearer token. Its twin
/// in openrouter_live.rs did exactly that the day its key was encrypted and failed with `401`; this
/// one is currently masked only because no ElevenLabs key is set. `ApiKeys::load` handles both.
fn read_elevenlabs_key() -> Option<String> {
    let data_dir = std::path::Path::new(&std::env::var("APPDATA").ok()?).join("cortex-speech");
    cortex_speech_app_lib::api_keys::ApiKeys::load(&data_dir).elevenlabs
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
