from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
SETTINGS_RS = REPO_ROOT / "src-tauri" / "src" / "settings.rs"
PIPELINE_RS = REPO_ROOT / "src-tauri" / "src" / "pipeline.rs"
COMMANDS_RS = REPO_ROOT / "src-tauri" / "src" / "commands.rs"
LLM_REFINER_RS = REPO_ROOT / "src-tauri" / "src" / "llm_refiner.rs"
T2_LISTENER_RS = REPO_ROOT / "src-tauri" / "src" / "jury" / "t2_listener.rs"
GEMINI_API_RS = REPO_ROOT / "src-tauri" / "src" / "gemini_api.rs"
RELEASE_DOCS = REPO_ROOT / "docs" / "RELEASE.md"


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def assert_contains(text: str, expected: str, context: str) -> None:
    if expected not in text:
        raise AssertionError(f"{context} is missing: {expected}")


def test_cloud_llm_defaults_are_opt_out() -> None:
    settings = read(SETTINGS_RS)

    assert_contains(settings, "cloud_llm_opt_in: false", SETTINGS_RS.name)
    assert_contains(settings, "jury_cloud_opt_in: false", SETTINGS_RS.name)
    assert_contains(settings, "llm_api_key: \"\".to_string()", SETTINGS_RS.name)
    assert_contains(settings, "llm_api_key_configured: false", SETTINGS_RS.name)
    assert_contains(settings, "llm_mode: LlmMode::default()", SETTINGS_RS.name)
    assert_contains(settings, "#[default]\n    Local", SETTINGS_RS.name)


def test_gemini_refinement_requires_effective_opt_in_mode() -> None:
    settings = read(SETTINGS_RS)
    pipeline = read(PIPELINE_RS)

    assert_contains(settings, "pub fn effective_llm_mode(&self) -> LlmMode", SETTINGS_RS.name)
    assert_contains(settings, "if self.llm_mode == LlmMode::Gemini && !self.cloud_llm_opt_in", SETTINGS_RS.name)
    if pipeline.count("effective_llm_mode()") < 2:
        raise AssertionError("pipeline.rs must route LLM refinement through effective_llm_mode()")
    assert_contains(pipeline, "crate::llm_refiner::LlmRefiner::new(", PIPELINE_RS.name)


def test_t2_audio_cloud_calls_require_jury_opt_in() -> None:
    commands = read(COMMANDS_RS)

    assert_contains(commands, "let cloud_opt_in = settings.jury_cloud_opt_in;", COMMANDS_RS.name)
    assert_contains(commands, "if cloud_opt_in && !api_key.trim().is_empty()", COMMANDS_RS.name)
    assert_contains(commands, '"T1 could not resolve; T2 disabled (cloud opt-in off)".to_string()', COMMANDS_RS.name)
    assert_contains(commands, 'db.write_segment_verdict(seg_id, "escalated", None, Some(&reason)', COMMANDS_RS.name)
    assert_contains(commands, "if !cloud_opt_in", COMMANDS_RS.name)
    assert_contains(commands, "Cloud opt-in is required for T2", COMMANDS_RS.name)
    assert_contains(commands, "Gemini API key is required for T2", COMMANDS_RS.name)


def test_api_keys_are_not_persisted_or_returned_to_client() -> None:
    settings = read(SETTINGS_RS)

    assert_contains(settings, "settings.llm_api_key.clear();", "legacy settings load scrub")
    assert_contains(settings, "persisted.llm_api_key.clear();", "settings save scrub")
    assert_contains(settings, "settings.llm_api_key.clear();", "client response scrub")
    assert_contains(settings, "pub fn merge_session_secret_from(&mut self, current: &Self)", SETTINGS_RS.name)
    assert_contains(settings, "save_clears_secret_material_but_marks_key_configured", SETTINGS_RS.name)
    assert_contains(settings, "load_scrubs_legacy_plaintext_secret_from_settings_file", SETTINGS_RS.name)
    assert_contains(settings, "for_client_response_clears_session_secret_but_preserves_configured_flag", SETTINGS_RS.name)


def test_cloud_error_paths_redact_secrets() -> None:
    llm_refiner = read(LLM_REFINER_RS)
    t2_listener = read(T2_LISTENER_RS)
    gemini_api = read(GEMINI_API_RS)

    assert_contains(llm_refiner, "redact_api_key(&e.to_string(), &self.api_key)", LLM_REFINER_RS.name)
    assert_contains(t2_listener, "redact_api_key(&e.to_string(), api_key)", T2_LISTENER_RS.name)
    assert_contains(gemini_api, 'pub(crate) const API_KEY_HEADER: &str = "x-goog-api-key";', GEMINI_API_RS.name)
    assert_contains(gemini_api, "request.set(API_KEY_HEADER, api_key)", GEMINI_API_RS.name)
    assert_contains(gemini_api, "assert!(!url.contains(\"?key=\"));", GEMINI_API_RS.name)


def test_release_docs_keep_cloud_privacy_gate() -> None:
    docs = read(RELEASE_DOCS)

    assert_contains(
        docs,
        "Gemini/cloud LLM use is explicitly opted in and visibly marked as sending text to a provider.",
        RELEASE_DOCS.name,
    )
    assert_contains(
        docs,
        "No API keys, settings files, logs, local media, temp DBs, reports, or private paths are included in release artifacts.",
        RELEASE_DOCS.name,
    )


def main() -> None:
    test_cloud_llm_defaults_are_opt_out()
    test_gemini_refinement_requires_effective_opt_in_mode()
    test_t2_audio_cloud_calls_require_jury_opt_in()
    test_api_keys_are_not_persisted_or_returned_to_client()
    test_cloud_error_paths_redact_secrets()
    test_release_docs_keep_cloud_privacy_gate()
    print("cloud privacy policy regression passed")


if __name__ == "__main__":
    main()
