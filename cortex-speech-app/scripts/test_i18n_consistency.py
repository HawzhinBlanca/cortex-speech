#!/usr/bin/env python3
"""i18n consistency policy — the app is Central-Kurdish-first, so a locale gap is a real, user-visible
regression (Kurdish users fall back to English, or every user sees a raw dotted key). The two locale
maps (`src/lib/i18n/en.ts`, `src/lib/i18n/ckb.ts`) are hand-maintained flat `Record<string,string>`
objects with ~660 keys each and no automated guard. This policy pins three invariants:

  1. No DUPLICATE key within a locale file — a copy-paste dup silently overrides the earlier value
     (the object literal keeps the LAST), so a translation can vanish with no error.
  2. en / ckb key PARITY — a key added to one locale but not the other shows the wrong language.
  3. Every LITERAL `t('x')` / `tr('x')` / `$t('x')` reference in src/ is DEFINED in en.ts — a typo'd or
     forgotten key renders the raw dotted string to EVERY user.

Deliberately does NOT verify dynamic keys (a variable passed to the translator) — those cannot be
resolved statically; the literal references are the ones a static gate can protect.
"""
from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
EN = REPO_ROOT / "src" / "lib" / "i18n" / "en.ts"
CKB = REPO_ROOT / "src" / "lib" / "i18n" / "ckb.ts"
SRC = REPO_ROOT / "src"

# A key at the start of an object entry: `ident:` or `'quoted.key':` or `"quoted":` after leading
# indentation. Anchored to line start so a `:` inside a value never false-matches.
KEY_RE = re.compile(r"""(?m)^\s+(?:'([^']+)'|"([^"]+)"|([A-Za-z_$][\w$]*))\s*:""")
# A translator call with a string literal: t('x'), $t("x"), tr('x'). The negative lookbehind keeps
# `get(t)` / identifiers ending in t from matching; dynamic-key calls simply aren't matched.
REF_RE = re.compile(r"""(?<![\w$])(?:\$?t|tr)\(\s*['"]([^'"]+)['"]""")


def _locale_body(path: Path) -> str:
    text = path.read_text(encoding="utf-8")
    return text[text.index("{") + 1 : text.rindex("}")]


def key_list(path: Path) -> list[str]:
    return [m.group(1) or m.group(2) or m.group(3) for m in KEY_RE.finditer(_locale_body(path))]


def test_no_duplicate_keys_within_a_locale() -> None:
    for path in (EN, CKB):
        keys = key_list(path)
        seen: set[str] = set()
        dups = sorted({k for k in keys if k in seen or seen.add(k)})  # add returns None (falsy)
        if dups:
            raise AssertionError(
                f"{path.name} defines duplicate i18n key(s) — the later entry silently overrides the "
                f"earlier translation: {dups}"
            )


def test_en_and_ckb_have_identical_key_sets() -> None:
    en, ckb = set(key_list(EN)), set(key_list(CKB))
    if en < {"import", "export", "settings"}:  # sanity: the parser found real keys, not zero
        raise AssertionError("en.ts key extraction found no known keys — this gate would pass vacuously")
    missing_in_ckb = sorted(en - ckb)
    missing_in_en = sorted(ckb - en)
    if missing_in_ckb or missing_in_en:
        raise AssertionError(
            "en.ts and ckb.ts key sets diverge (Kurdish-first app: a gap shows the wrong language).\n"
            f"  in en but MISSING in ckb ({len(missing_in_ckb)}): {missing_in_ckb}\n"
            f"  in ckb but MISSING in en ({len(missing_in_en)}): {missing_in_en}"
        )


def test_every_referenced_literal_key_is_defined() -> None:
    defined = set(key_list(EN))
    missing: dict[str, list[str]] = {}
    for path in SRC.rglob("*"):
        if path.suffix not in (".svelte", ".ts") or path.name.endswith(".test.ts"):
            continue
        text = path.read_text(encoding="utf-8", errors="ignore")
        for m in REF_RE.finditer(text):
            key = m.group(1)
            if key not in defined:
                missing.setdefault(key, []).append(str(path.relative_to(REPO_ROOT)))
    if missing:
        lines = [f"  '{k}'  <- {', '.join(sorted(set(v))[:3])}" for k, v in sorted(missing.items())]
        raise AssertionError(
            "translation key(s) referenced in code but NOT defined in en.ts — every user sees the raw "
            "dotted string:\n" + "\n".join(lines)
        )


def main() -> None:
    test_no_duplicate_keys_within_a_locale()
    test_en_and_ckb_have_identical_key_sets()
    test_every_referenced_literal_key_is_defined()
    print("i18n consistency policy regression passed")


if __name__ == "__main__":
    main()
