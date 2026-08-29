from pathlib import Path
import re

from _pipeline_policy_util import pipeline_surface


REPO_ROOT = Path(__file__).resolve().parents[1]
SETTINGS_RS = REPO_ROOT / "src-tauri" / "src" / "settings.rs"
PIPELINE_RS = REPO_ROOT / "src-tauri" / "src" / "pipeline.rs"
COMMANDS_RS = REPO_ROOT / "src-tauri" / "src" / "commands.rs"
SCRIBE_API_RS = REPO_ROOT / "src-tauri" / "src" / "scribe_api.rs"
LIB_RS = REPO_ROOT / "src-tauri" / "src" / "lib.rs"
API_KEYS_RS = REPO_ROOT / "src-tauri" / "src" / "api_keys.rs"
LLM_REFINER_RS = REPO_ROOT / "src-tauri" / "src" / "llm_refiner.rs"
T2_LISTENER_RS = REPO_ROOT / "src-tauri" / "src" / "jury" / "t2_listener.rs"
GEMINI_API_RS = REPO_ROOT / "src-tauri" / "src" / "gemini_api.rs"
RELEASE_DOCS = REPO_ROOT / "docs" / "RELEASE.md"


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


from _policy_util import strip_comments as _strip_comments  # noqa: E402


# Week-4 decomposes commands.rs into slices under src/commands/. The jury T2 cloud-consent gates moved
# with them, so these privacy checks scan the whole command surface rather than one implementation file.
COMMANDS_DIR = REPO_ROOT / "src-tauri" / "src" / "commands"


def command_surface() -> str:
    parts = [read(COMMANDS_RS)]
    if COMMANDS_DIR.is_dir():
        parts += [read(path) for path in sorted(COMMANDS_DIR.rglob("*.rs"))]
    text = "\n".join(parts)
    if "#[tauri::command]" not in text:
        raise AssertionError("no #[tauri::command] in the command surface — this cloud-privacy gate would pass vacuously")
    return text


def assert_contains(text: str, expected: str, context: str) -> None:
    if expected not in text:
        raise AssertionError(f"{context} is missing: {expected}")


def assert_literal_invoke(text: str, command: str, context: str) -> None:
    """Require a statically named generated or closed handwritten IPC call."""
    invocation = re.compile(
        rf"\b(?:invokeLegacy|invokeCritical|__TAURI_INVOKE)"
        rf"\s*(?:<[\s\S]{{0,2000}}?>\s*)?\(\s*(['\"]){re.escape(command)}\1"
    )
    if not invocation.search(text):
        raise AssertionError(
            f"{context} lost required champion production invoke {command!r}; "
            "the absence policy must not pass by deleting the real review flow"
        )


def test_cloud_llm_defaults_are_opt_out() -> None:
    settings = read(SETTINGS_RS)

    assert_contains(settings, "cloud_llm_opt_in: false", SETTINGS_RS.name)
    assert_contains(settings, "jury_cloud_opt_in: false", SETTINGS_RS.name)
    assert_contains(settings, "llm_api_key: \"\".to_string()", SETTINGS_RS.name)
    assert_contains(settings, "llm_api_key_configured: false", SETTINGS_RS.name)
    assert_contains(settings, "llm_mode: LlmMode::default()", SETTINGS_RS.name)
    # 2026-08-20: factory default moved Local -> None (strictly MORE private - a fresh install
    # talks to no LLM endpoint at all until the owner opts in; external review blocker #7).
    assert_contains(settings, "#[default]\n    None", SETTINGS_RS.name)


def test_gemini_refinement_requires_effective_opt_in_mode() -> None:
    settings = read(SETTINGS_RS)
    pipeline = pipeline_surface(REPO_ROOT / "src-tauri" / "src")

    assert_contains(settings, "pub fn effective_llm_mode(&self) -> LlmMode", SETTINGS_RS.name)
    assert_contains(settings, "if self.llm_mode == LlmMode::Gemini && !self.cloud_llm_opt_in", SETTINGS_RS.name)
    if pipeline.count("effective_llm_mode()") < 2:
        raise AssertionError("pipeline.rs must route LLM refinement through effective_llm_mode()")
    assert_contains(pipeline, "crate::llm_refiner::LlmRefiner::new(", PIPELINE_RS.name)


def test_t2_audio_cloud_calls_require_jury_opt_in() -> None:
    commands = command_surface()

    assert_contains(commands, "let cloud_opt_in = settings.jury_cloud_opt_in;", COMMANDS_RS.name)
    assert_contains(commands, "if cloud_opt_in && !api_key.trim().is_empty()", COMMANDS_RS.name)
    assert_contains(commands, '"T1 could not resolve; T2 disabled (cloud opt-in off)".to_string()', COMMANDS_RS.name)
    # The cloud-OFF path must still reach a verdict locally: escalate to the human inbox, never call out.
    #
    # This used to match the single-line call `db.write_segment_verdict(seg_id, "escalated", None,
    # Some(&reason)`. That broke when the call gained an evidence_json argument and rustfmt wrapped it —
    # a formatting change, with the privacy behaviour identical. Matching source LAYOUT made this gate
    # brittle in the one direction that matters least and silent in the direction that matters most: the
    # old string would have gone on passing if someone had inserted a cloud call beside it.
    #
    # Pinned on the semantics instead, which is strictly stronger. `reason::T1_UNRESOLVED` is written
    # ONLY on the cloud-disabled branch, so its presence proves that branch still escalates, and the
    # absence check below proves it does so without egress.
    assert_contains(commands, "crate::jury::reason::T1_UNRESOLVED", COMMANDS_RS.name)
    cloud_off_branch = commands.split("Cloud disabled — escalate to human inbox", 1)
    if len(cloud_off_branch) != 2:
        raise AssertionError(
            "the cloud-disabled escalation branch in commands.rs is no longer findable — re-point this "
            "check rather than deleting it, or the no-egress guarantee stops being verified"
        )
    tail = cloud_off_branch[1][:1200]
    if 'write_segment_verdict(' not in tail or '"escalated"' not in tail:
        raise AssertionError("the cloud-disabled branch must still escalate the segment to a human")
    for forbidden in ("t2_listener::", "reqwest", "api_key", "segment_audio_as_wav_base64"):
        if forbidden in tail:
            raise AssertionError(
                f"the cloud-disabled branch references {forbidden!r} — with jury cloud opt-in OFF this "
                "path must reach a local verdict without touching any cloud transport or segment audio"
            )
    assert_contains(commands, "if !cloud_opt_in", COMMANDS_RS.name)
    assert_contains(commands, "Cloud opt-in is required for T2", COMMANDS_RS.name)
    # A judge API key is required before any T2 cloud call. Substring (not the Gemini-specific wording)
    # so the guard still holds after the jury gained an OpenRouter transport (Gemini key OR OpenRouter
    # key) — the key check itself is unchanged; the message just names both providers now.
    assert_contains(commands, "key is required for T2", COMMANDS_RS.name)


def test_removed_cloud_stt_and_alternative_retranscribe_surfaces_stay_absent() -> None:
    """The production reviewer flow is champion-only.

    ElevenLabs Scribe was not owner-requested, so merely hiding its button or defaulting consent off is
    insufficient: the client, key provider, IPC commands, module, endpoint implementation, and settings
    field must not ship. The per-clip constrained/MMS commands are also excluded because they returned a
    draft for a stale frontend whole-row upsert with no atomic backend provenance commit. Their standalone
    offline diagnostics remain outside this production surface.

    Match exact executable identifiers rather than the bare word ``scribe``: historical database provenance
    labels must remain readable and ordinary words such as ``transcribe`` contain that substring.
    """
    if SCRIBE_API_RS.exists():
        raise AssertionError("src-tauri/src/scribe_api.rs still ships the unrequested ElevenLabs HTTP client")

    champion_required: dict[Path, tuple[str, ...]] = {
        LIB_RS: ("commands::transcribe_segment",),
        COMMANDS_DIR / "transcribe.rs": ("pub async fn transcribe_segment(",),
        REPO_ROOT / "src" / "Workstation.svelte": ("createWorkstationSegmentActions",),
        REPO_ROOT / "src" / "lib" / "workstationSegmentActions.ts": (
            "export function createWorkstationSegmentActions(",
            "api.transcribeSegment(",
        ),
        REPO_ROOT / "src" / "lib" / "ReviewMode.svelte": ("api.transcribeSegment(",),
    }
    for path, required_tokens in champion_required.items():
        code = _strip_comments(read(path))
        for token in required_tokens:
            if token not in code:
                raise AssertionError(
                    f"{path.relative_to(REPO_ROOT)} lost required champion production token {token!r}; "
                    "the absence policy must not pass by deleting the real review flow"
                )

    frontend_commands = _strip_comments(read(REPO_ROOT / "src" / "lib" / "commands.ts"))
    assert_contains(
        frontend_commands,
        "generatedCommands.transcribeSegment(",
        "src/lib/commands.ts",
    )
    generated_bindings = _strip_comments(read(REPO_ROOT / "src" / "lib" / "generated" / "ipc.ts"))
    assert_literal_invoke(generated_bindings, "transcribe_segment", "src/lib/generated/ipc.ts")

    runtime_forbidden: dict[Path, tuple[str, ...]] = {
        LIB_RS: (
            "mod scribe_api",
            "commands::transcribe_audio_with_scribe",
            "commands::add_scribe_votes",
            "commands::transcribe_segment_constrained",
            "commands::transcribe_segment_finetuned",
        ),
        COMMANDS_RS: (
            "require_cloud_stt_consent",
            "transcribe_audio_with_scribe",
            "SCRIBE_VOTE_MODEL_ID",
            "SCRIBE_VOTES_IN_FLIGHT",
            "crate::scribe_api",
        ),
        COMMANDS_DIR / "jury.rs": ("add_scribe_votes", "crate::scribe_api"),
        COMMANDS_DIR / "transcribe.rs": (
            "pub async fn transcribe_segment_constrained(",
            "pub async fn transcribe_segment_finetuned(",
        ),
        SETTINGS_RS: ("cloud_stt_opt_in",),
        PIPELINE_RS: ("cloud_stt_opt_in",),
        API_KEYS_RS: ("ELEVENLABS_API_KEY", "elevenlabs:"),
        COMMANDS_DIR / "settings.rs": ('"elevenlabs"', "ELEVENLABS_API_KEY"),
    }
    for path, forbidden_tokens in runtime_forbidden.items():
        code = _strip_comments(read(path))
        for token in forbidden_tokens:
            if token in code:
                raise AssertionError(f"{path.relative_to(REPO_ROOT)} still ships removed production token {token!r}")

    frontend_forbidden: dict[str, tuple[str, ...]] = {
        "src/Workstation.svelte": (
            "handleTranscribeConstrained",
            "handleTranscribeFinetuned",
            "handleTranscribeScribe",
            "handleAddScribeVote",
            "transcribe-constrained-btn",
            "transcribe-finetuned-btn",
            "transcribe-scribe-btn",
            "add-scribe-vote-btn",
        ),
        "src/lib/ReviewMode.svelte": (
            "transcribeSegmentFinetuned",
            "retranscribe('finetuned')",
            "review.retranscribeFinetuned",
        ),
        "src/lib/commands.ts": (
            "transcribeSegmentConstrained",
            "transcribeSegmentFinetuned",
            "transcribeWithScribe",
            "addScribeVotes",
            "transcribe_segment_constrained",
            "transcribe_segment_finetuned",
            "transcribe_audio_with_scribe",
            "add_scribe_votes",
            "'elevenlabs'",
        ),
        "src/lib/SettingsPanel.svelte": ("cloudSttOptIn", "settings.cloudSttConsent", "elevenlabs"),
        "src/lib/settingsAdapter.ts": ("cloud_stt_opt_in", "cloudSttOptIn"),
        "src/lib/stores/settingsStore.ts": ("cloudSttOptIn",),
        "src/lib/i18n/en.ts": (
            "transcribeConstrained",
            "transcribeFinetuned",
            "scribe.transcribe",
            "scribe.vote",
            "review.retranscribeFinetuned",
            "settings.cloudSttConsent",
        ),
        "src/lib/i18n/ckb.ts": (
            "transcribeConstrained",
            "transcribeFinetuned",
            "scribe.transcribe",
            "scribe.vote",
            "review.retranscribeFinetuned",
            "settings.cloudSttConsent",
        ),
    }
    for rel, forbidden_tokens in frontend_forbidden.items():
        source = read(REPO_ROOT / rel)
        code = _strip_comments(source)
        if not source.strip():
            raise AssertionError(f"{rel} is empty — release-surface absence check would be vacuous")
        for token in forbidden_tokens:
            if token in code:
                raise AssertionError(f"{rel} still exposes removed production token {token!r}")


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
    # The AUDIO path (whole-file references, the T2 audio panel, the ckb ASR benchmark) must route
    # through this same audited client — a caller that assembled its own request would sit outside
    # this file's key-in-header / redaction guarantees, which is precisely how a key reaches a query
    # string. Pin both halves: the audio entry point exists here, and it redacts provider errors.
    assert_contains(gemini_api, "pub fn transcribe_audio(", GEMINI_API_RS.name)
    assert_contains(gemini_api, "crate::secret_redaction::redact_api_key(", GEMINI_API_RS.name)


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
    test_removed_cloud_stt_and_alternative_retranscribe_surfaces_stay_absent()
    test_t2_audio_cloud_calls_require_jury_opt_in()
    test_api_keys_are_not_persisted_or_returned_to_client()
    test_cloud_error_paths_redact_secrets()
    test_release_docs_keep_cloud_privacy_gate()
    print("cloud privacy policy regression passed")


if __name__ == "__main__":
    main()
