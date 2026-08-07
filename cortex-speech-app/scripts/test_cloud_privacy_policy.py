import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
SETTINGS_RS = REPO_ROOT / "src-tauri" / "src" / "settings.rs"
PIPELINE_RS = REPO_ROOT / "src-tauri" / "src" / "pipeline.rs"
COMMANDS_RS = REPO_ROOT / "src-tauri" / "src" / "commands.rs"
SCRIBE_API_RS = REPO_ROOT / "src-tauri" / "src" / "scribe_api.rs"
LLM_REFINER_RS = REPO_ROOT / "src-tauri" / "src" / "llm_refiner.rs"
T2_LISTENER_RS = REPO_ROOT / "src-tauri" / "src" / "jury" / "t2_listener.rs"
GEMINI_API_RS = REPO_ROOT / "src-tauri" / "src" / "gemini_api.rs"
RELEASE_DOCS = REPO_ROOT / "docs" / "RELEASE.md"


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


# ── P3.2: whole-surface cloud-STT-egress INVENTORY (closes T2) ─────────────────────────────────────────
# The old check counted `require_cloud_stt_consent(&state)?` call sites (>= 2). A frozen count is a floor:
# a THIRD command that uploads audio to ElevenLabs Scribe WITHOUT the consent gate keeps the count at 2 and
# passes. This inventory instead keys on real evidence — every command-surface function that reaches a
# Scribe egress sink must have consent enforced on EVERY path to it, so a new un-gated egress is caught.

# Every audio-upload pub fn in scribe_api.rs is named `transcribe*` (the ElevenLabs HTTP POST itself is a
# PRIVATE helper); the two pure-text helpers below never egress. Pinned by test_scribe_egress_surface_is_
# only_transcribe_fns so the command-surface prefix scan (`scribe_api::transcribe`) stays COMPLETE.
PURE_SCRIBE_FNS = {"dedupe_repeated", "segment_words"}
SCRIBE_EGRESS_CALL = "scribe_api::transcribe"  # matches transcribe / transcribe_wav_bytes / transcribe_segments / transcribe_*
CONSENT_MARKERS = ("require_cloud_stt_consent", "cloud_stt_opt_in")

_FN_RE = re.compile(r"(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_]\w*)\s*(?:<[^>]*>)?\s*\(")


def _strip_comments(text: str) -> str:
    """Drop `/* */` block comments AND `//` line comments so a COMMENTED-OUT consent gate (or a marker
    mentioned only in prose) never counts as enforcement. Over-stripping on a stray `*/`/`//` inside a
    string literal only makes the marker/caller scan STRICTER — it fails closed, never open."""
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.DOTALL)  # block comments (non-nested — the norm here)
    return "\n".join(line.split("//", 1)[0] for line in text.splitlines())  # line comments


def _parse_functions(text: str) -> tuple[dict[str, str], set[str]]:
    """{fn_name: brace-matched body} plus the set of #[tauri::command] fn names, over the whole surface.
    Comments are stripped first, so both the consent-marker check and the caller search see CODE only."""
    text = _strip_comments(text)
    bodies: dict[str, str] = {}
    commands: set[str] = set()
    for m in _FN_RE.finditer(text):
        name = m.group(1)
        brace = text.find("{", m.end())
        semi = text.find(";", m.end())
        if brace == -1 or (semi != -1 and semi < brace):
            continue  # a trait/extern declaration with no body — skip
        depth, i = 0, brace
        while i < len(text):
            if text[i] == "{":
                depth += 1
            elif text[i] == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        bodies[name] = text[brace : i + 1]
        if "#[tauri::command]" in text[max(0, m.start() - 200) : m.start()]:
            commands.add(name)
    return bodies, commands


def _consent_covered(name: str, bodies: dict[str, str], commands: set[str], stack: frozenset[str]) -> bool:
    """True iff consent (require_cloud_stt_consent / cloud_stt_opt_in) is enforced on EVERY path that can
    reach `name`. A co-located gate in the body satisfies it; otherwise a private helper is covered only if
    EVERY caller is covered (recursing through helpers, terminating at commands — a command that reaches an
    egress with no gate is NOT covered)."""
    body = bodies.get(name, "")
    if any(marker in body for marker in CONSENT_MARKERS):
        return True
    if name in commands:
        return False  # a command that egresses (directly or via a helper) with no consent gate
    if name in stack:
        return True  # recursion guard: a cycle is judged by its members' own external callers
    callers = [c for c, b in bodies.items() if c != name and re.search(rf"\b{re.escape(name)}\s*\(", b)]
    if not callers:
        return False  # an egress helper nothing calls and with no gate — cannot prove it is reached only under consent
    return all(_consent_covered(c, bodies, commands, stack | {name}) for c in callers)


# Week-4 decomposes commands.rs into slices under src/commands/. The cloud-consent gates for the jury
# T2 + Scribe egress commands moved with them, so these privacy checks must scan the whole command
# SURFACE (commands.rs + every slice) — otherwise a consent gate could "vanish" from a by-path read and
# the gate would pass vacuously while audio/text egress silently lost its opt-in guard.
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


def test_cloud_stt_scribe_egress_requires_opt_in() -> None:
    # The cloud-LLM and jury-T2 gates are pinned above, but the ElevenLabs Scribe (cloud STT) egress
    # paths upload SEGMENT AUDIO — a stricter privacy surface. Every Scribe egress must be gated behind
    # cloud_stt_opt_in, or a refactor could silently ship audio off-device with no consent.
    settings = read(SETTINGS_RS)
    commands = command_surface()
    pipeline = read(PIPELINE_RS)

    # Opt-out by default.
    assert_contains(settings, "cloud_stt_opt_in: false", SETTINGS_RS.name)
    # The IPC-boundary guard exists and enforces the opt-in.
    assert_contains(
        commands, "fn require_cloud_stt_consent(state: &AppState) -> Result<(), String>", COMMANDS_RS.name
    )
    assert_contains(commands, "if state.lock_settings().cloud_stt_opt_in", COMMANDS_RS.name)
    # WHOLE-SURFACE INVENTORY (P3.2, replaces the old >= 2 count floor): every command-surface function
    # that reaches a Scribe egress sink (`scribe_api::transcribe*`) must have consent enforced on EVERY
    # path to it. A NEW third egress command with no gate is now FAILED, not silently allowed by a frozen
    # count. `command_surface()` already fails vacuously-empty; require at least one egress site so a moved/
    # renamed helper cannot make this pass by finding nothing to check.
    bodies, cmd_names = _parse_functions(commands)
    egress_fns = sorted(name for name, body in bodies.items() if SCRIBE_EGRESS_CALL in body)
    if not egress_fns:
        raise AssertionError(
            "no Scribe egress call site (`scribe_api::transcribe*`) found in the command surface — the "
            "cloud-egress inventory would pass vacuously; did the egress helper move or get renamed?"
        )
    ungated = [name for name in egress_fns if not _consent_covered(name, bodies, cmd_names, frozenset())]
    if ungated:
        raise AssertionError(
            f"Scribe cloud egress reachable WITHOUT a consent gate on the path: {ungated}. Every command that "
            "(directly or via a helper) uploads audio to ElevenLabs must enforce require_cloud_stt_consent / "
            "cloud_stt_opt_in before the egress — audio must never leave the device without opt-in."
        )
    # The import path gates the key itself behind the same opt-in (defense in depth).
    assert_contains(pipeline, "fn scribe_api_key_if_enabled", PIPELINE_RS.name)
    assert_contains(pipeline, "if !self.settings.cloud_stt_opt_in", PIPELINE_RS.name)


def test_scribe_egress_surface_is_only_transcribe_fns() -> None:
    # The cloud-egress inventory scans the command surface for `scribe_api::transcribe*` call sites. That
    # prefix is COMPLETE only while every audio-upload pub fn in scribe_api.rs is named `transcribe*` (the
    # ElevenLabs HTTP POST itself is a private helper). Pin it: a NEW pub fn that is neither a known pure
    # helper nor a transcribe* egress fn forces the inventory's egress scan to be updated, so a differently
    # named upload fn cannot slip past the prefix.
    scribe = _strip_comments(read(SCRIBE_API_RS))
    # `pub fn` AND `pub(crate) fn` (idiomatic here — the poster itself is pub(crate)); any visibility so a
    # non-transcribe* upload helper cannot hide behind a wider `pub(crate)`/multi-space form.
    for match in re.finditer(r"pub(?:\([^)]*\))?\s+fn\s+([A-Za-z_]\w*)", scribe):
        fn = match.group(1)
        if fn not in PURE_SCRIBE_FNS and not fn.startswith("transcribe"):
            raise AssertionError(
                f"scribe_api.rs has a new pub fn `{fn}` that is neither a known pure-text helper "
                f"({sorted(PURE_SCRIBE_FNS)}) nor a `transcribe*` egress fn. If it uploads audio, extend the "
                "cloud-egress inventory's SCRIBE_EGRESS_CALL scan to cover it; if it is pure, add it to "
                "PURE_SCRIBE_FNS."
            )


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
    test_cloud_stt_scribe_egress_requires_opt_in()
    test_scribe_egress_surface_is_only_transcribe_fns()
    test_t2_audio_cloud_calls_require_jury_opt_in()
    test_api_keys_are_not_persisted_or_returned_to_client()
    test_cloud_error_paths_redact_secrets()
    test_release_docs_keep_cloud_privacy_gate()
    print("cloud privacy policy regression passed")


if __name__ == "__main__":
    main()
