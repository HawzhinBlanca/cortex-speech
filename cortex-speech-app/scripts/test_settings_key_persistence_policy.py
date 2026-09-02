"""Source policies for API-key persistence in the Settings panel.

An API key is the one setting the user cannot re-derive by looking at the screen: when it is dropped,
nothing says so, the badge can still claim a key is configured, and the failure only surfaces later as
a cloud call refusing to run. Two separate defects of exactly that shape have already shipped:

  1. (2026-08-10) The Gemini field bound to `localSettings.llmApiKey` and persisted through
     `update_settings`, whose `AppSettings::load` SCRUBS that field — the key was written to
     settings.json and deleted on the next load, while `llmApiKeyConfigured` stayed true. Measured: an
     import failed 3/3 with "Gemini API key is required" while the panel showed the key as configured.
  2. (2026-08-10, same day) With the field rewired to the encrypted store behind its own "Save key"
     button, the panel then had TWO save buttons — and the bottom-right one (bigger, blue, the obvious
     target) still routed through the scrubbing path. Measured: the owner pasted a key, pressed Save,
     and GEMINI_API_KEY stayed empty.

Both are silent credential loss. Like the sibling frontend guards, these are pinned at the source
rather than unit-tested: the project's frontend tests are pure functions with no component-mount
harness. Each check below was fail-before verified.
"""

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
# Repointed 2026-08-30: the panel's persistence logic was extracted into two controllers. The
# invariants are unchanged; they simply live where the code now lives — save() in the persistence
# controller, flushPendingKeys()/the key inputs' own state in the key controller, the onDestroy
# flush and the input bindings still in the panel markup.
PANEL = "src/lib/SettingsPanel.svelte"
PERSISTENCE = "src/lib/settingsPersistenceController.ts"
KEYS = "src/lib/settingsKeyController.svelte.ts"


def _read(rel: str) -> str:
    return (REPO_ROOT / rel).read_text(encoding="utf-8")


def _function_body(src: str, sig: str) -> str:
    """The text of the function starting at `sig`, up to the next top-level (2-space-indented) function."""
    start = src.find(sig)
    if start == -1:
        raise AssertionError(f"{sig!r} not found — this gate would pass vacuously")
    rest = src[start + len(sig):]
    end = len(rest)
    # `onDestroy(` is a terminator too, not just the next function: flushPendingKeys is declared
    # immediately above it, and without this the extracted "body" swallowed onDestroy — whose
    # localSettings reference then failed the encrypted-store check against correct code.
    for marker in ("\n  async function ", "\n  function ", "\n  onDestroy("):
        idx = rest.find(marker)
        if idx != -1:
            end = min(end, idx)
    return rest[:end]


def test_every_exit_path_flushes_a_pending_api_key() -> None:
    """A key typed into a field but not confirmed with that field's own button must still be saved.

    `save()` must flush BEFORE `api.updateSettings(...)`: update_settings is the scrubbing path, so a
    flush after it would race the very clear that loses the key. `onDestroy` (the ✕ / Escape route)
    must flush too — closing the panel is an exit, not a discard.

    Fail-before: deleting either call, or moving the `save()` call below `api.updateSettings(`, fires
    the corresponding assertion.
    """
    src = _read(PANEL)

    save_body = _function_body(_read(PERSISTENCE), "async function save(")
    flush_at = save_body.find("await flushPendingKeys();")
    update_at = save_body.find("await api.updateSettings(")
    assert flush_at != -1, "save() must flush pending API-key inputs before persisting settings"
    assert update_at != -1, "save() no longer calls api.updateSettings — this gate needs rewriting"
    assert flush_at < update_at, (
        "save() must flush pending API keys BEFORE api.updateSettings: update_settings scrubs "
        "llm_api_key, so a later flush races the clear that drops the key"
    )

    destroy_at = src.find("onDestroy(()")
    assert destroy_at != -1, "onDestroy block not found — this gate would pass vacuously"
    destroy_body = src[destroy_at:destroy_at + 1200]
    assert "flushPendingKeys()" in destroy_body, (
        "closing the panel with ✕/Escape must flush a pending API key, not silently discard it"
    )


def test_the_flush_routes_through_the_encrypted_store() -> None:
    """flushPendingKeys must persist via the per-provider set_api_key helpers (which reach
    save_key_protected), never by assigning into localSettings — that is defect #1 above, reintroduced.

    Fail-before: replacing a helper call with `localSettings.llmApiKey = geminiKeyInput` fires this.
    """
    body = _function_body(_read(KEYS), "async function flushPendingKeys(")
    for helper in ("saveOpenrouterKey()", "saveGeminiKey()"):
        assert helper in body, f"flushPendingKeys must persist the pending key via {helper}"
    assert "localSettings" not in body, (
        "flushPendingKeys must not write an API key into localSettings — that field is scrubbed on load"
    )


def test_no_api_key_input_binds_to_the_scrubbed_settings_field() -> None:
    """The key inputs must bind to their own local state, never to `localSettings.llmApiKey`.

    That binding IS defect #1: it persists the key into settings.json, which AppSettings::load then
    clears. Fail-before: restoring `bind:value={localSettings.llmApiKey}` on the Gemini input fires this.
    """
    # The input elements moved into the AI/Jury tab components; the panel forwards the key
    # controller's state into them. Scan the panel AND both tabs so neither can quietly rebind an
    # input to the scrubbed settings field.
    tabs = _read("src/lib/SettingsAiTab.svelte") + _read("src/lib/SettingsJuryTab.svelte")
    src = _read(PANEL) + tabs
    assert "bind:value={localSettings.llmApiKey}" not in src, (
        "an API-key input is bound to localSettings.llmApiKey, which AppSettings::load scrubs — "
        "the key would be written and then deleted, exactly the 2026-08-10 defect"
    )
    for expected in ("bind:value={geminiKeyInput}", "bind:value={openrouterKeyInput}"):
        assert expected in tabs, f"expected an API-key input bound to its own state ({expected})"


def main() -> None:
    test_every_exit_path_flushes_a_pending_api_key()
    test_the_flush_routes_through_the_encrypted_store()
    test_no_api_key_input_binds_to_the_scrubbed_settings_field()
    print("settings API-key persistence source policy passed")


if __name__ == "__main__":
    main()
