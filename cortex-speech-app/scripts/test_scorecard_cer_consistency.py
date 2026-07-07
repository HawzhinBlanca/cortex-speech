#!/usr/bin/env python3
"""Pin the honesty-critical CER-definition CONSISTENCY across the accuracy scorecards.

scorecard_7b.py once computed CER on space-STRIPPED text while scorecard_finetuned.py used jiwer
(interior whitespace KEPT, matching eval.rs's tokenize_chars). That deflated the OmniASR-7B champion's
headline CER — a word-segmentation error (e.g. "هاوڕێ من" -> "هاوڕێمن", a real Sorani ASR error class)
scored 0% instead of its true 12.5% — and made the champion NON-comparable to the fine-tuned 21% it
claims parity with. This gate pins the space-KEPT definition so the two scorecards can never silently
drift apart again.
"""
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCORECARD_7B = REPO_ROOT / "scripts" / "scorecard_7b.py"


def test_scorecard_7b_cer_keeps_whitespace_in_source() -> None:
    src = SCORECARD_7B.read_text(encoding="utf-8")
    # The per-clip CER must score the FULL normalized string (spaces kept) — jiwer-equivalent, matching
    # scorecard_finetuned.py and eval.rs. Reintroducing a `.replace(" ", "")` on the CER inputs would
    # hide word-merge errors and deflate the number.
    assert "edit_distance(list(r), list(h))" in src, (
        "scorecard_7b CER must score the space-KEPT normalized strings (jiwer-equivalent), "
        "not a whitespace-stripped variant"
    )
    assert 'rc, hc = r.replace(" ", "")' not in src, "the old space-stripped CER path must stay removed"


def test_space_kept_cer_matches_jiwer_when_available() -> None:
    sys.path.insert(0, str(REPO_ROOT / "scripts"))
    from scorecard_7b import edit_distance, norm

    r, h = norm("هاوڕێ من"), norm("هاوڕێمن")  # a word-merge (deleted space) error
    kept = edit_distance(list(r), list(h))
    stripped = edit_distance(list(r.replace(" ", "")), list(h.replace(" ", "")))
    assert stripped == 0, "sanity: the stripped form HIDES the word-merge error — exactly why it was wrong"
    assert kept == 1, "the space-kept form scores the deleted space as one error"

    try:
        import jiwer
    except ImportError:
        print("  (jiwer not installed — skipped the numeric jiwer-equivalence check)")
        return
    o = jiwer.process_characters(r, h)
    jiwer_dist = o.substitutions + o.deletions + o.insertions
    assert kept == jiwer_dist, (
        f"space-kept CER distance ({kept}) must equal jiwer.process_characters ({jiwer_dist}) so the "
        "7B number is comparable to scorecard_finetuned.py's jiwer-based CER"
    )


def main() -> int:
    test_scorecard_7b_cer_keeps_whitespace_in_source()
    test_space_kept_cer_matches_jiwer_when_available()
    print("scorecard CER-consistency policy regression passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
