//! Live verification that the refiner works through OpenRouter (IGNORED by default — needs a real
//! OPENROUTER_API_KEY in secrets.env + network). Proves OpenRouter is a usable LLM provider for the
//! refiner/jury, bypassing the direct-Gemini 429 quota block.
//!
//! Run:  cargo test --test openrouter_live -- --ignored --nocapture
//!
//! The key is read from the local secrets file and NEVER printed; only the model output is shown.

/// Read the key through the PRODUCTION loader, not by hand-parsing `secrets.env`.
///
/// This used to strip `NAME=` off the raw line and use whatever followed. That works only while every
/// value happens to be plaintext. `secrets.env` values may also be `dpapi:<base64>` — DPAPI-encrypted
/// at rest, which is what `set_api_key` writes — and the hand-parser handed the CIPHERTEXT to
/// OpenRouter as a bearer token. Measured 2026-08-11: encrypting the stored OpenRouter key (same value,
/// verified byte-identical by hash) turned this test red with `status code 401`, while the app itself
/// kept refining fine because every production path goes through `ApiKeys`.
///
/// A test that re-implements credential loading is testing a file format, not the product — and it
/// fails on the day the product improves.
fn read_key(name: &str) -> Option<String> {
    let data_dir = std::path::Path::new(&std::env::var("APPDATA").ok()?).join("cortex-speech");
    let keys = cortex_speech_app_lib::api_keys::ApiKeys::load(&data_dir);
    match name {
        "OPENROUTER_API_KEY" => keys.openrouter,
        "GEMINI_API_KEY" => keys.gemini,
        "ELEVENLABS_API_KEY" => keys.elevenlabs,
        _ => None,
    }
}

#[test]
#[ignore = "live: needs OPENROUTER_API_KEY + network"]
fn openrouter_refines_sorani() {
    let key = match read_key("OPENROUTER_API_KEY") {
        Some(k) => k,
        None => {
            eprintln!("skip: no OPENROUTER_API_KEY");
            return;
        }
    };
    let refiner = cortex_speech_app_lib::llm_refiner::LlmRefiner::for_openrouter(
        key,
        "openai/gpt-4o-mini".to_string(),
        "You are a Central Kurdish (Sorani) proofreader. Output ONLY the corrected Sorani text.".to_string(),
    )
    .expect("refiner should construct with a key");
    let out = refiner.refine_text("ساڵی دووهەزار و بیستویەک ڕادەگەیەنرێت").expect("openrouter refine should succeed");
    println!("OPENROUTER_REFINE:: {out}");
    assert!(out.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)), "expected Sorani output, got: {out}");
}
