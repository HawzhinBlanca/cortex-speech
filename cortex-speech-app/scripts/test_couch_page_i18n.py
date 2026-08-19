#!/usr/bin/env python3
"""Policy: the Couch Review phone page is Kurdish-first, and its Sorani never drifts.

The phone page (`src-tauri/assets/couch.html`) is served straight out of the Rust binary via
`include_str!`, so it cannot import the Svelte i18n dictionaries. It carries its own small string
table instead — which is the right call (the desktop dictionary is ~700 strings; a phone needs 13),
but it creates two failure modes this gate closes:

  1. **Silent drift.** A phone string copied from `ckb.ts` could be edited later — or its desktop
     original could change — and the two would diverge with nobody noticing. The page's `source` map
     names the desktop key each Sorani string was copied from, and this test asserts they are still
     byte-identical. Text a native speaker reviewed once stays reviewed.

  2. **Unreviewed Sorani creeping in.** Strings with `source: null` are genuinely NEW Sorani that has
     NOT had a native review. That list is PINNED below, so it can never grow silently: adding one
     fails this gate until the owner acknowledges it here.

Plus the same en/ckb key-set parity `test_i18n_consistency.py` enforces for the desktop, and a check
that the page actually renders in Sorani by default (`<html lang="ckb" dir="rtl">`) — a page that
technically has translations but opens in English has not fixed anything.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

APP = Path(__file__).resolve().parent.parent
PAGE = APP / "src-tauri" / "assets" / "couch.html"
CKB_TS = APP / "src" / "lib" / "i18n" / "ckb.ts"

# Phone strings whose Sorani is NEW and has NOT had a native review. Owner-gated: this list is the
# acknowledgement. Adding a key here is a deliberate act; growing it silently is not possible.
UNREVIEWED_SORANI = {
    # The playback refusal (2026-08-19). Shown when a verdict is rejected because the clip was not
    # played far enough — the one refusal a reviewer can fix themselves, so the wording has to be
    # unambiguous in Sorani or it reads as the app being broken. NOT natively reviewed: on the
    # owner's read list.
    "mustListen",
    "heldByOthers",
    "linkExpired",
    "loadingMore",
    "queued",
    "refused",
    "stillSending",
    "audioMissing",
    "skip",
    # Reviewer progress badge: total AUDIO they have completed. New Sorani ("{t} دەنگ تەواو بووە"),
    # NOT natively reviewed — it goes on the owner's read list with the others. It matters that this
    # one reads well: reviewers are paid per hour of audio reviewed, so this is the number they will
    # check against their own count.
    "audioDone",
    # Dialect routing (owner instruction 2026-08-16): shown to a reviewer restricted to a dialect
    # that currently has no clips. New Sorani, NOT natively reviewed — on the owner's read list.
    "noDialectWork",
    # v47 speaker-change badge. New Sorani, NOT yet natively reviewed — it goes on the owner's list
    # with the other seven.
    "speakerChange",
    # R4.4 explicit no-verdict. The BUTTON reuses the existing (already-listed) `skip` string; this is
    # the empty-state line that stops "🎉 all clips reviewed" appearing over clips the reviewer
    # skipped. One new string was the honest floor — the alternative was letting the page lie.
    "skippedByYou",
}


def load_page_strings() -> dict:
    html = PAGE.read_text(encoding="utf-8")
    match = re.search(r'<script id="i18n" type="application/json">\s*(\{.*?\})\s*</script>', html, re.DOTALL)
    if not match:
        raise AssertionError(f"{PAGE.name}: no <script id=\"i18n\" type=\"application/json\"> block found")
    try:
        return json.loads(match.group(1))
    except json.JSONDecodeError as exc:  # pragma: no cover - the failure message is the point
        raise AssertionError(f"{PAGE.name}: the i18n block is not valid JSON: {exc}") from exc


def load_desktop_ckb() -> dict[str, str]:
    """Parse `key: 'value'` pairs out of ckb.ts.

    Deliberately a line regex, not a JS parser: the dictionary is a flat literal of single-quoted
    strings, and every key this gate looks up is one of those. A key that spans lines (a few long
    hints do) simply will not be found — which fails LOUD here rather than silently passing, so a
    phone string can never claim a desktop source this parser cannot actually verify.
    """
    out: dict[str, str] = {}
    for line in CKB_TS.read_text(encoding="utf-8").splitlines():
        m = re.match(r"\s*'?([A-Za-z0-9_.]+)'?:\s*'((?:[^'\\]|\\.)*)',\s*$", line)
        if m:
            out[m.group(1)] = m.group(2).replace("\\'", "'")
    return out


def test_page_is_kurdish_first() -> None:
    html = PAGE.read_text(encoding="utf-8")
    assert '<html lang="ckb" dir="rtl">' in html, (
        "the phone page must OPEN in Sorani RTL like the rest of this Kurdish-first app — a page that "
        "carries translations but renders English by default has not fixed the problem"
    )


def test_en_and_ckb_key_sets_match() -> None:
    strings = load_page_strings()
    en, ckb = set(strings["en"]), set(strings["ckb"])
    assert en == ckb, (
        f"couch.html en/ckb key sets diverge (Kurdish-first app: a gap shows the wrong language).\n"
        f"  in en but MISSING in ckb: {sorted(en - ckb)}\n"
        f"  in ckb but MISSING in en: {sorted(ckb - en)}"
    )
    assert set(strings["source"]) == en, (
        "every phone string must declare a `source` (a desktop i18n key, or null for new Sorani).\n"
        f"  missing a source entry: {sorted(en - set(strings['source']))}\n"
        f"  source entry with no string: {sorted(set(strings['source']) - en)}"
    )


def test_sourced_sorani_still_matches_the_reviewed_desktop_string() -> None:
    strings = load_page_strings()
    desktop = load_desktop_ckb()
    drift = []
    for key, source in strings["source"].items():
        if source is None:
            continue
        assert source in desktop, (
            f"couch.html string '{key}' claims source '{source}', which is not a parseable key in "
            f"ckb.ts. Either the desktop key was renamed/removed, or it is a multi-line value this "
            f"gate cannot verify — in both cases the claim of 'already natively reviewed' is void."
        )
        if strings["ckb"][key] != desktop[source]:
            drift.append(f"  {key}: page={strings['ckb'][key]!r} != ckb.ts[{source}]={desktop[source]!r}")
    assert not drift, (
        "phone Sorani has DRIFTED from the desktop string it was copied from. Text a native speaker "
        "reviewed once must stay reviewed — re-sync the page, or drop the `source` claim and add the "
        "key to UNREVIEWED_SORANI in this file:\n" + "\n".join(drift)
    )


def test_unreviewed_sorani_list_is_exactly_as_acknowledged() -> None:
    strings = load_page_strings()
    actual = {k for k, src in strings["source"].items() if src is None}
    assert actual == UNREVIEWED_SORANI, (
        "the set of phone strings carrying UNREVIEWED Sorani changed.\n"
        f"  acknowledged in this policy: {sorted(UNREVIEWED_SORANI)}\n"
        f"  actually in couch.html:      {sorted(actual)}\n"
        "New unreviewed Sorani must be acknowledged here deliberately (and sent to the owner for a "
        "native read) — it may not appear silently. If a string now reuses a reviewed desktop value, "
        "give it a `source` and remove it from this list."
    )


def test_params_survive_translation() -> None:
    """A {param} dropped in one language silently renders a sentence with a hole in it."""
    strings = load_page_strings()
    for key in strings["en"]:
        en_params = set(re.findall(r"\{(\w+)\}", strings["en"][key]))
        ckb_params = set(re.findall(r"\{(\w+)\}", strings["ckb"][key]))
        assert en_params == ckb_params, (
            f"couch.html '{key}': placeholder mismatch — en has {sorted(en_params)}, "
            f"ckb has {sorted(ckb_params)}. The missing side renders an incomplete sentence."
        )


def test_no_raw_server_english_is_shown_to_the_reviewer() -> None:
    """`e.message` must never reach the screen (plan R3.3).

    Every string on this page is translated, and then the one word that says WHAT WENT WRONG used to
    arrive in English straight from the server — "unauthorized", "Failed to fetch", "another reviewer
    is working on this clip" — dropped inside a Sorani sentence. A reviewer who does not read English
    got a message that looked translated and told them nothing at the exact moment they needed it.

    A status code is the honest substitute: language-neutral, still diagnostic, and it fits the
    reviewed strings' existing `{err}` slot without inventing new Sorani. This gate keeps it that way,
    because the tempting fix when a new error path appears is to interpolate `e.message` again.
    """
    src = PAGE.read_text(encoding="utf-8")
    offenders = [
        f"  line {n}: {line.strip()}"
        for n, line in enumerate(src.splitlines(), start=1)
        # Only lines that actually RENDER something. `e.message` in a comment, or passed to console,
        # is not shown to anyone.
        if "e.message" in line
        and not line.lstrip().startswith("//")
        and ("t(" in line or "textContent" in line or "toast(" in line or "showErr(" in line)
    ]
    assert not offenders, (
        "raw English server text is being rendered to the reviewer:\n"
        + "\n".join(offenders)
        + "\nUse the STATUS (`e.status || '—'`) instead — language-neutral and it fits the existing "
        "{err} slot. A genuinely new message needs a new Sorani string, which is owner-gated."
    )


def main() -> None:
    test_page_is_kurdish_first()
    test_en_and_ckb_key_sets_match()
    test_sourced_sorani_still_matches_the_reviewed_desktop_string()
    test_unreviewed_sorani_list_is_exactly_as_acknowledged()
    test_params_survive_translation()
    test_no_raw_server_english_is_shown_to_the_reviewer()
    strings = load_page_strings()
    reused = sum(1 for s in strings["source"].values() if s is not None)
    print(
        f"couch page i18n OK: {len(strings['en'])} strings, Kurdish-first (lang=ckb dir=rtl); "
        f"{reused} reuse natively-reviewed desktop Sorani byte-for-byte; "
        f"{len(UNREVIEWED_SORANI)} awaiting owner review: {sorted(UNREVIEWED_SORANI)}"
    )


if __name__ == "__main__":
    try:
        main()
    except AssertionError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        sys.exit(1)
