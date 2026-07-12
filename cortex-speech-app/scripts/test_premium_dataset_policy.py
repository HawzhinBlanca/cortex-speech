#!/usr/bin/env python3
"""Regression gate for build_premium_dataset.py — the premium-tier dataset builder.

Pins the properties that make the premium tier trustworthy (several were adversarial-review
findings against the first version — each is locked here so it can never regress):
  1. filler detection matches the canonical guideline spellings + elongations — in the
     POST-canonicalization byte-forms the app actually exports (ھمم with U+06BE, not only the
     typed همم U+0647) — and never the demonstrative ئەم, lexical ئا, or numbers in any digit
     script (1000 / ۲۲۲۰ / ٢٢٢٠);
  2. fragments and fillers cannot hide behind clinging punctuation (دە-، «ئەمم» …);
  3. legit Sorani reduplication (زۆر زۆر) is NEVER a rejection;
  4. the human gate is transcriptSource == "human_verified" (covers Review-Inbox verdicts,
     which the app exports with verified=false), not the verified flag;
  5. duplicates are keyed on segment id (audioPath is a shared basename across VAD chunks);
  6. every gate rejects with a reason, nothing is dropped silently, thresholds come from the
     real argparse parser (a loosened default fails here), blockword files may carry a BOM;
  7. an empty premium tier and malformed input exit nonzero (failures must be loud).
"""

import importlib.util
import json
import sys
import tempfile
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    "build_premium_dataset", Path(__file__).parent / "build_premium_dataset.py"
)
_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_mod)

HEH_TYPED = "همم"  # ARABIC HEH U+0647 — what an annotator types
HEH_EXPORTED = "ھمم"  # HEH DOACHASHMEE U+06BE — what the app's canonicalizer exports
assert HEH_TYPED != HEH_EXPORTED, "test file must keep the two distinct heh byte-forms"


def _seg(**over):
    """A row that passes EVERY premium gate; tests override one aspect at a time."""
    base = {
        "id": f"seg-{len(str(over))}-{sorted(over) or 'base'}",
        "audioPath": "clip.wav",
        "trainingTranscript": "ئاوەکە بۆ ئاودان بەکاردێتەوە بۆ پیشەسازی",
        "trainingReady": True,
        "trainingGrade": "gold",
        "transcriptSource": "human_verified",
        "verified": True,
        "durationMs": 4000,
        "clippingRatio": 0.0,
        "snrDb": 25.0,
        "rmsDb": -20.0,
    }
    base.update(over)
    base["id"] = str(base["id"])
    return base


def _cfg(**flag_over):
    """Thresholds from the REAL parser — a loosened argparse default fails these tests."""
    cfg = _mod.make_parser().parse_args(["unused.jsonl", "--out-dir", "unused"])
    for key, val in flag_over.items():
        setattr(cfg, key, val)
    return cfg


def _eval(seg, blockwords=frozenset(), **flag_over):
    return _mod.evaluate_segment(seg, _cfg(**flag_over), blockwords)


def test_filler_detection_matches_only_canonical_spellings():
    # Canonical hesitations are caught — including BOTH heh byte-forms: the typed U+0647 and
    # the U+06BE the app's canonicalizer actually exports (adversarial finding: the U+0647-only
    # set missed 100% of real hem-hesitations on the export path).
    for filler in ("ئەمم", "ئاا", "ئەە", "ئمم", HEH_TYPED, HEH_EXPORTED, "ئووو", "ئێێ"):
        assert _mod.filler_tokens_in(f"وشەیەک {filler} وشەیەکی تر") == [filler], filler
    # ...elongations (3+ same letter) are caught even if not in the list...
    assert _mod.filler_tokens_in("ئاااا باشە") == ["ئاااا"]
    # ...but repeated DIGITS are numbers, never hesitations (regression: real "2220" volt draft),
    # in EVERY digit script Kurdish text uses (ASCII, Arabic-Indic, Extended Arabic-Indic)
    assert _mod.filler_tokens_in("نزیکەی 2220 ڤۆڵت") == [], "2220 is a number, not a filler"
    assert _mod.filler_tokens_in("هەزار واتە 1000 دینار") == [], "1000 is a number, not a filler"
    assert _mod.filler_tokens_in("نزیکەی ۲۲۲۰ ڤۆڵت") == [], "Extended Arabic-Indic ۲۲۲۰ is a number"
    assert _mod.filler_tokens_in("نزیکەی ٢٢٢٠ ڤۆڵت") == [], "Arabic-Indic ٢٢٢٠ is a number"
    # ...and the demonstrative ئەم and lexical ئا are REAL WORDS and must never match.
    assert _mod.filler_tokens_in("ئەم کتێبە باشە") == [], "demonstrative ئەم is not a filler"
    assert _mod.filler_tokens_in("ئا بەڵێ وایە") == [], "lexical ئا is not a filler"
    # punctuation cannot hide a filler: Arabic comma, guillemets, ellipsis (adversarial finding)
    assert _mod.filler_tokens_in("باشە ئەمم، بەڵام") == ["ئەمم،"]
    assert _mod.filler_tokens_in("«ئەمم» وتی") == ["«ئەمم»"]
    assert _mod.filler_tokens_in("ئەمم… دوایە") == ["ئەمم…"]


def test_fragments_and_reduplication():
    assert _mod.fragment_tokens_in("دە- دەڕوات بۆ ماڵەوە") == ["دە-"]
    # a pause comma after the hyphen must not hide the fragment (adversarial finding)
    assert _mod.fragment_tokens_in("دە-، دەڕوات بۆ ماڵەوە") == ["دە-،"]
    assert _mod.fragment_tokens_in("دەڕوات بۆ ماڵەوە") == []
    # a bare hyphen token is punctuation, not a fragment
    assert _mod.fragment_tokens_in("وشە - وشە") == []
    # legit reduplication is info, never a gate
    assert _mod.repeated_word_runs("زۆر زۆر باشە") == 1
    reasons = _eval(_seg(trainingTranscript="زۆر زۆر پێخۆشە ئەم کارە"))
    assert reasons == [], f"reduplication must not reject: {reasons}"


def test_human_gate_is_transcript_source():
    # Review-Inbox accept/edit verdicts export verified=false but transcriptSource=human_verified:
    # they ARE human truth and MUST pass (adversarial finding: verified-flag gate rejected them).
    assert _eval(_seg(verified=False, transcriptSource="human_verified")) == []
    # Machine-evidence SILVER rows are trainingReady but NOT human-verified: premium rejects them.
    for source in ("jury_verdict", "annotated", "normalized_asr", "raw_asr", None):
        reasons = _eval(_seg(transcriptSource=source))
        assert any(r.startswith("not-human-verified") for r in reasons), (source, reasons)


def test_every_gate_rejects_with_a_reason():
    cases = {
        "not-training-ready": _seg(trainingReady=False),
        "not-human-verified": _seg(transcriptSource="jury_verdict"),
        "empty-training-transcript": _seg(trainingTranscript="  "),
        "fillers": _seg(trainingTranscript=f"باشە {HEH_EXPORTED} ئەوە دەکەم"),
        "false-start-fragments": _seg(trainingTranscript="دە-، دەڕوات بۆ شار"),
        "too-few-words": _seg(trainingTranscript="بەڵێ باشە"),
        "too-short": _seg(durationMs=500),
        "too-long": _seg(durationMs=60000),
        "cps-out-of-range": _seg(durationMs=29000, trainingTranscript="سێ وشە تەنیا"),
        "clipping": _seg(clippingRatio=0.2),
        "snr": _seg(snrDb=3.0),
        "rms": _seg(rmsDb=-60.0),
        "missing-audio-metric": _seg(snrDb=None),
    }
    for expected, seg in cases.items():
        reasons = _eval(seg)
        assert any(r.startswith(expected) for r in reasons), f"{expected}: got {reasons}"
    # lenient mode keeps unmeasured audio
    assert _eval(_seg(snrDb=None, rmsDb=None, clippingRatio=None), lenient_audio=True) == []
    # blockwords gate — and punctuation must not hide a blocked word either (same PUNCT_STRIP)
    reasons = _eval(_seg(trainingTranscript="وشەیەکی خراپ لێرەدایە بەڕاستی"), blockwords=frozenset(["خراپ"]))
    assert any(r.startswith("blockwords") for r in reasons), reasons
    reasons = _eval(_seg(trainingTranscript="وشەیەکی «خراپ» لێرەدایە بەڕاستی"), blockwords=frozenset(["خراپ"]))
    assert any(r.startswith("blockwords") for r in reasons), f"guillemets must not hide a blockword: {reasons}"


def test_end_to_end_split_dedup_bom_and_loud_failures():
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        rows = [
            _seg(id="ok"),
            _seg(id="ok"),  # same id -> true duplicate -> deduped
            # same text, DIFFERENT id: two chunks of one recording share the basename audioPath,
            # and a short sentence can recur — this is NOT a duplicate (adversarial finding)
            _seg(id="same-text-different-clip"),
            _seg(id="filler", trainingTranscript=f"باشە {HEH_EXPORTED} ئەوە دەکەم ئێستا"),
            _seg(id="inbox-verdict", verified=False, transcriptSource="human_verified"),  # passes
            _seg(id="silver", transcriptSource="jury_verdict"),
            _seg(id="blocked", trainingTranscript="ئەمە وشەیەکی قەدەغەیە بەڕاستی ئێستا"),
        ]
        src = tmp / "export.jsonl"
        src.write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in rows), encoding="utf-8")
        block = tmp / "block.txt"
        block.write_text("قەدەغەیە\n", encoding="utf-8-sig")  # BOM on purpose (adversarial finding)
        out = tmp / "out"
        assert _mod.main([str(src), "--out-dir", str(out), "--blockwords", str(block)]) == 0
        premium = [json.loads(l) for l in (out / "premium.jsonl").read_text(encoding="utf-8").splitlines()]
        rejected = [json.loads(l) for l in (out / "rejected.jsonl").read_text(encoding="utf-8").splitlines()]
        assert [r["id"] for r in premium] == ["ok", "same-text-different-clip", "inbox-verdict"], premium
        assert {r["id"] for r in rejected} == {"ok", "filler", "silver", "blocked"}, rejected
        assert all(r["premiumRejectReasons"] for r in rejected), "every rejection carries reasons"
        assert len(premium) + len(rejected) == len(rows), "nothing dropped silently"

        # empty premium must fail loudly
        bad = tmp / "bad.jsonl"
        bad.write_text(json.dumps(_seg(transcriptSource="raw_asr"), ensure_ascii=False), encoding="utf-8")
        assert _mod.main([str(bad), "--out-dir", str(tmp / "out2")]) == 1
        # missing input and non-object lines must fail loudly
        assert _mod.main([str(tmp / "nope.jsonl"), "--out-dir", str(tmp / "out3")]) == 1
        weird = tmp / "weird.jsonl"
        weird.write_text('123\n', encoding="utf-8")
        assert _mod.main([str(weird), "--out-dir", str(tmp / "out4")]) == 1


if __name__ == "__main__":
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    test_filler_detection_matches_only_canonical_spellings()
    test_fragments_and_reduplication()
    test_human_gate_is_transcript_source()
    test_every_gate_rejects_with_a_reason()
    test_end_to_end_split_dedup_bom_and_loud_failures()
    print("PASS: premium dataset builder policy")
