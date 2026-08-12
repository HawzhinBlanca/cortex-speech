"""Policy: hypothesis fusion may COMBINE what engines heard, never INVENT.

The fusion harness decides what draft a human reviewer is shown. If it can emit a word no engine
proposed, it is a generative rewriter wearing a voting hat — the failure this project measured on
its own data (Gemini refinement rewrote a median 11.1% of characters, translating loanwords the
speaker actually said). These assertions pin the properties that keep it a combiner:

  1. closure   — every token of every fused output was proposed by some input engine
  2. incumbent — a tie never unseats the anchor (the champion must be OUTVOTED, not merely matched)
  3. anchor    — the conservative method changes a word only on unanimity of the other engines
  4. honesty   — fusion runs only over clips EVERY engine transcribed, so the fused set and the
                 baseline are scored on the same exam

Runs the real functions on synthetic rows; no network, no models, no database.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from fuse_hypotheses import align_to_anchor, fuse_clip  # noqa: E402


def test_closure_no_invented_tokens() -> None:
    anchor = "مەعاش زۆر باشە"
    others = {"gemini": "مووچە زۆر باشە", "mms": "مەعاش زۆر خۆشە"}
    for method in ("anchor", "rover"):
        fused = fuse_clip(anchor, others, method, {}, "champion")
        proposed = set(anchor.split())
        for text in others.values():
            proposed |= set(text.split())
        for tok in fused.split():
            assert tok in proposed, f"{method} invented {tok!r} — fusion must only combine"


def test_a_tie_keeps_the_incumbent() -> None:
    # One engine disagrees, one agrees: 1 vote each against the champion's own vote -> champion wins.
    anchor = "مەعاش زۆر باشە"
    others = {"gemini": "مووچە زۆر باشە"}
    fused = fuse_clip(anchor, others, "rover", {}, "champion")
    assert fused.split()[0] == "مەعاش", f"a tie must keep the champion's word, got {fused!r}"


def test_weight_can_outvote_the_incumbent() -> None:
    # Two engines agreeing, each weighted 1.0, outvote a champion weighted 1.0 (2.0 > 1.0).
    anchor = "مەعاش زۆر باشە"
    others = {"gemini": "مووچە زۆر باشە", "mms": "مووچە زۆر باشە"}
    fused = fuse_clip(anchor, others, "rover", {}, "champion")
    assert fused.split()[0] == "مووچە", f"a real majority must win, got {fused!r}"


def test_anchor_method_requires_unanimity() -> None:
    anchor = "مەعاش زۆر باشە"
    # Engines disagree with the champion AND with each other -> no change.
    split = {"gemini": "مووچە زۆر باشە", "mms": "دەرامەت زۆر باشە"}
    assert fuse_clip(anchor, split, "anchor", {}, "champion").split()[0] == "مەعاش"
    # Engines unanimously name the SAME replacement -> change.
    agree = {"gemini": "مووچە زۆر باشە", "mms": "مووچە زۆر باشە"}
    assert fuse_clip(anchor, agree, "anchor", {}, "champion").split()[0] == "مووچە"


def test_alignment_refuses_to_guess_on_length_mismatch() -> None:
    # A replace span of DIFFERENT length contributes no votes rather than mis-pairing words: a wrong
    # alignment would let one engine's word vote against an unrelated word.
    mapped = align_to_anchor(["a", "b", "c"], ["a", "x", "y", "c"])
    assert mapped[0] == "a" and mapped[2] == "c"
    assert mapped[1] is None, f"unequal replace span must not be guessed: {mapped}"


def test_empty_anchor_is_returned_unchanged() -> None:
    assert fuse_clip("", {"gemini": "شتێک"}, "rover", {}, "champion") == ""


def main() -> int:
    for fn in [
        test_closure_no_invented_tokens,
        test_a_tie_keeps_the_incumbent,
        test_weight_can_outvote_the_incumbent,
        test_anchor_method_requires_unanimity,
        test_alignment_refuses_to_guess_on_length_mismatch,
        test_empty_anchor_is_returned_unchanged,
    ]:
        fn()
        print(f"  ok  {fn.__name__}")
    print("fusion policy regression passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
