//! Live verification that the refiner works through OpenRouter (IGNORED by default — needs a real
//! OPENROUTER_API_KEY in secrets.env + network). Proves OpenRouter is a usable LLM provider for the
//! refiner/jury, bypassing the direct-Gemini 429 quota block.
//!
//! Run:  cargo test --test openrouter_live -- --ignored --nocapture
//!
//! The key is read from the local secrets file and NEVER printed; only the model output is shown.

fn read_key(name: &str) -> Option<String> {
    let path = std::path::Path::new(&std::env::var("APPDATA").ok()?).join("cortex-speech").join("secrets.env");
    for line in std::fs::read_to_string(path).ok()?.lines() {
        if let Some(rest) = line.trim().strip_prefix(&format!("{name}=")) {
            let v = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
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
        "You are a Central Kurdish (Sorani) proofreader. Output ONLY the corrected Sorani text."
            .to_string(),
    )
    .expect("refiner should construct with a key");
    let out = refiner.refine_text("ساڵی دووهەزار و بیستویەک ڕادەگەیەنرێت").expect("openrouter refine should succeed");
    println!("OPENROUTER_REFINE:: {out}");
    assert!(
        out.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)),
        "expected Sorani output, got: {out}"
    );
}
