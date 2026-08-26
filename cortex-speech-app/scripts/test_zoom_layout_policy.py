"""Layout that survives a small viewport — i.e. 200 % browser zoom, or a short window.

Measured 2026-08-17 in the real app at a 640x400 viewport (a 1280x800 screen at 200 % zoom):

  * `main` scrolled, but its centred child was `h-full ... justify-center`. Content taller than the
    box was split evenly above and below centre, and the half pushed ABOVE `scrollTop: 0` could not
    be scrolled to by any means — the icon and heading were simply gone.
  * The Settings dialog's tab column is a fixed-width `nav` inside an `85vh` cap with no scroller of
    its own, so 3 of the 8 tabs (Models, Jury, Diagnostics) were clipped and unreachable.

Both are the same class of bug: a box that cannot grow and cannot scroll silently eats its own
content, and nothing in the app says so. jsdom has no layout engine, so a unit test cannot see any
of this — these source pins are what keeps the fix from being undone by a later "cleanup".
"""

import json
import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]

# `h-full` pins a box to its scroller's height; `justify-center` then splits any overflow in both
# directions. Either alone is fine. Together, inside a scroll region, the top half is unreachable.
CENTRED_FULL_HEIGHT = re.compile(r"h-full[^\"']*\bjustify-center\b|\bjustify-center\b[^\"']*\bh-full\b")

SCROLL_SAFE_CENTRE = "[justify-content:safe_center]"


def _class_attributes(source: str) -> list[str]:
    return re.findall(r"class=\"([^\"]*)\"", source)


def test_full_height_boxes_do_not_centre_their_overflow() -> None:
    offenders: list[str] = []
    for svelte in sorted((REPO_ROOT / "src").rglob("*.svelte")):
        for classes in _class_attributes(svelte.read_text(encoding="utf-8")):
            if CENTRED_FULL_HEIGHT.search(classes):
                offenders.append(f"{svelte.relative_to(REPO_ROOT)}: {classes.strip()[:110]}")
    if offenders:
        formatted = "\n".join(f"- {entry}" for entry in offenders)
        raise AssertionError(
            "`h-full` + `justify-center` centres overflow into unreachable space above scrollTop 0.\n"
            f"Use `min-h-full` + `{SCROLL_SAFE_CENTRE}` instead:\n{formatted}"
        )


def test_the_empty_state_uses_the_scroll_safe_centring() -> None:
    # The positive half: the two boxes that were actually measured clipped must still carry the fix,
    # so this policy cannot be satisfied by deleting the centring altogether.
    for relative_path in ("src/lib/EmptyState.svelte", "src/Workstation.svelte"):
        source = (REPO_ROOT / relative_path).read_text(encoding="utf-8")
        if SCROLL_SAFE_CENTRE not in source:
            raise AssertionError(
                f"{relative_path} must centre its empty state with {SCROLL_SAFE_CENTRE} "
                "(measured clipped at a 640x400 viewport without it)"
            )


def test_settings_tab_column_can_scroll() -> None:
    source = (REPO_ROOT / "src/lib/SettingsPanel.svelte").read_text(encoding="utf-8")
    nav = re.search(r"<nav class=\"([^\"]*)\"", source)
    if not nav:
        raise AssertionError("SettingsPanel.svelte no longer has a <nav> tab column to check")
    if "overflow-y-auto" not in nav.group(1):
        raise AssertionError(
            "The Settings tab column must scroll: the dialog is capped at 85vh, and without its own "
            f"scroller the last tabs are unreachable at 200 % zoom. Got: {nav.group(1)}"
        )


def test_packaged_window_can_reach_every_supported_responsive_tier() -> None:
    """Browser-only viewport proof is irrelevant if the packaged window forbids those widths."""
    config = json.loads((REPO_ROOT / "src-tauri/tauri.conf.json").read_text(encoding="utf-8"))
    windows = config.get("app", {}).get("windows", [])
    if len(windows) != 1:
        raise AssertionError("the desktop viewport policy requires one explicit primary window")
    window = windows[0]
    if window.get("resizable") is not True:
        raise AssertionError("the primary workstation window must remain resizable")
    if window.get("minWidth", float("inf")) > 320:
        raise AssertionError(
            "tauri.conf.json prevents the supported 320–499, 500–799 and 800–1199 responsive "
            f"tiers from being reached in the packaged desktop (minWidth={window.get('minWidth')})"
        )
    if window.get("minHeight", float("inf")) > 480:
        raise AssertionError(
            "tauri.conf.json prevents the compact workstation from reaching its supported short-window "
            f"state (minHeight={window.get('minHeight')})"
        )


def main() -> None:
    test_full_height_boxes_do_not_centre_their_overflow()
    test_the_empty_state_uses_the_scroll_safe_centring()
    test_settings_tab_column_can_scroll()
    test_packaged_window_can_reach_every_supported_responsive_tier()
    print("zoom layout policy passed (4 assertions)")


if __name__ == "__main__":
    main()
