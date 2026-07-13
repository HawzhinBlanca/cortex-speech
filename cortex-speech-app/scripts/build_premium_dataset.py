#!/usr/bin/env python3
"""Build the PREMIUM training split from a Cortex JSONL export — the "absolute best" tier.

The app exports verbatim, human-verified truth (see docs/ANNOTATION_GUIDELINES.md). This
script derives the premium tier from it, WITHOUT ever editing a transcript: a segment whose
text contains fillers/fragments is *routed out* of premium (audio and text stay matched),
never silently rewritten — rewriting text while the audio still contains the sounds would
recreate the exact audio/text mismatch that poisons ASR training.

Gates (each rejection carries machine-readable reasons; nothing is dropped silently):
  identity   — transcriptSource == "human_verified" (covers the verify button AND Review-Inbox
               accept/edit verdicts, which export verified=false) AND trainingGrade != "reject"
               AND no NON-alignment audio review-risk. NOT gated on trainingReady: that GOLD bar
               also requires MMS-precise word timestamps, which ASR audio->text training ignores,
               and which every clip fails while the aligner model is absent (energy_heuristic).
  text       — no canonical filler tokens, no elongated hesitations, no `X-` false-start
               fragments, optional owner blockword list, minimum word count (full sentences)
  timing     — duration within bounds; chars-per-second within bounds (garbage detector)
  audio      — the app's own MEASURED metrics: clippingRatio / snrDb / rmsDb within bounds;
               metrics missing => rejected in strict mode (default) because "unmeasured" is
               not "clean" (pass --lenient-audio to keep such rows)
  dedup      — duplicate segment IDs keep the first occurrence (audioPath is a shared basename
               across VAD chunks, and a short sentence may legitimately recur — never deduped)

Deliberately NOT a gate: immediate word repetition. Sorani uses real reduplication
(e.g. intensifier doubling), so repeats are counted in the summary as info, never rejected.

Filler detection is safe-by-construction: it matches ONLY the canonical hesitation
spellings mandated by docs/ANNOTATION_GUIDELINES.md (ئەمم / ئاا / ئەە / ئمم / همم / ئووو /
ئێێ) plus any-token elongation (the same letter 3+ times in a row, which no real Kurdish
word uses). It never matches the demonstrative ئەم or the lexical interjection ئا.

Usage:
  python scripts/build_premium_dataset.py EXPORT.jsonl --out-dir OUT/
  python scripts/build_premium_dataset.py EXPORT.jsonl --out-dir OUT/ --blockwords words.txt
  python scripts/build_premium_dataset.py --help   (all thresholds are flags)

Outputs (UTF-8): OUT/premium.jsonl, OUT/rejected.jsonl (each row + "premiumRejectReasons"),
and an honest summary on stdout. Exits 1 if the input is missing/empty or premium is empty —
an empty "best" dataset must fail loudly, not look like success.
"""

from __future__ import annotations

import argparse
import io
import json
import re
import sys
from pathlib import Path

# Canonical hesitation tokens — the spellings docs/ANNOTATION_GUIDELINES.md mandates, in BOTH
# byte-forms of the heh: annotators type همم with ARABIC HEH U+0647, but the app's export
# canonicalizer (normalizer.rs normalize_heh_contextual) rewrites every non-final U+0647 to
# ARABIC LETTER HEH DOACHASHMEE U+06BE, so the exported trainingTranscript contains ھمم.
# Detection must match the post-canonicalization form (adversarial review: the U+0647-only set
# missed 100% of real hem-hesitations); the pre-fold form is kept as defense-in-depth for text
# that never passed through the app. ئەم (demonstrative "this") and ئا (lexical interjection)
# are intentionally absent: they are real words, and matching them would strip genuine Kurdish.
CANONICAL_FILLERS = frozenset(
    [
        "ئەمم",
        "ئاا",
        "ئەە",
        "ئمم",
        "همم",  # ARABIC HEH U+0647 (as typed)
        "ھمم",  # HEH DOACHASHMEE U+06BE (as exported after canonicalization)
        "ئووو",
        "ئێێ",
    ]
)

# A LETTER repeated 3+ times inside one token is a non-lexical elongation in Sorani.
# Letters ONLY ([^\W\d_] = Unicode letters): digits repeat legitimately (1000, 2220 — and the
# Arabic-Indic forms ۲۲۲۰/٢٢٢٠ sit inside the Arabic block!) and must never be mistaken for
# hesitations — the ASCII case was caught on real data (a "2220" voltage draft was flagged).
ELONGATION = re.compile(r"([^\W\d_])\1\1")

# Punctuation that may cling to a token in Sorani text: Arabic + Latin stops, guillemets,
# ellipsis, brackets, ZWNJ. Deliberately EXCLUDES the ASCII hyphen — that is the false-start
# marker fragment_tokens_in must still see after stripping.
PUNCT_STRIP = "،؟؛.!,?;:…«»()[]{}\"'‌"

# App review-risk reasons (quality.rs) that flag a REAL AUDIO problem. These still reject a clip
# even after human verification. The two ALIGNMENT risks — low_confidence_alignment and
# energy_heuristic_alignment — are deliberately NOT here: they concern word-timestamp precision,
# which ASR audio->text training does not use, so a human-verified clip that the app downgraded to
# REVIEW *only* for alignment is still premium (owner decision 2026-07-13). Any audio risk below,
# or the app's REJECT grade, still excludes the clip.
NON_ALIGNMENT_AUDIO_RISK = frozenset(["clipping_warning", "low_rms_volume", "low_snr"])


def tokens_of(text: str) -> list[str]:
    return text.split()


def filler_tokens_in(text: str) -> list[str]:
    """Tokens that are canonical fillers or non-lexical elongations. Empty list == clean."""
    out = []
    for tok in tokens_of(text):
        bare = tok.strip(PUNCT_STRIP)
        if bare in CANONICAL_FILLERS or (bare and ELONGATION.search(bare)):
            out.append(tok)
    return out


def fragment_tokens_in(text: str) -> list[str]:
    """False-start fragments per the guidelines: tokens written with a trailing hyphen.
    Punctuation is stripped first so a pause comma cannot hide the marker (دە-، is still دە-)."""
    out = []
    for tok in tokens_of(text):
        bare = tok.strip(PUNCT_STRIP)
        if len(bare) > 1 and bare.endswith("-"):
            out.append(tok)
    return out


def repeated_word_runs(text: str) -> int:
    """Count immediate same-word repeats — INFO ONLY (legit Sorani reduplication exists)."""
    toks = tokens_of(text)
    return sum(1 for a, b in zip(toks, toks[1:]) if a == b)


def chars_per_second(text: str, duration_ms: float) -> float | None:
    if not duration_ms or duration_ms <= 0:
        return None
    return len(text.replace(" ", "")) / (duration_ms / 1000.0)


def evaluate_segment(seg: dict, cfg: argparse.Namespace, blockwords: frozenset[str]) -> list[str]:
    """All premium-gate failure reasons for one exported row. Empty list == premium."""
    reasons = []
    text = (seg.get("trainingTranscript") or "").strip()

    # identity: human-verified truth the app did not REJECT, with no real audio review-risk.
    #
    # We deliberately DO NOT gate on trainingReady. That GOLD bar (quality.rs) also demands
    # MMS-precise word timestamps, and with the aligner model absent EVERY clip is graded REVIEW
    # for energy_heuristic_alignment — so trainingReady is false for every clip, even ones the
    # human verified. ASR audio->text training does not use word timestamps, so alignment source
    # must not block the premium tier (owner decision 2026-07-13). Instead we require the SUBSTANCE:
    #   - transcriptSource == "human_verified": the human confirmed the text. Covers BOTH the verify
    #     button (verified=true) AND Review-Inbox accept/edit verdicts (verified=false but
    #     human_decision set) — gating on the verified flag alone wrongly rejected inbox verdicts.
    #   - trainingGrade != "reject": excludes human-rejected / blank / placeholder / SEVERE-audio
    #     clips (the app's hard REJECT, which overrides everything).
    #   - no NON-alignment audio review-risk: a clip the app flagged for clipping/low-RMS/low-SNR is
    #     still out, even if human-verified. Alignment-only review-risk is tolerated (see above).
    # The explicit measured-metric audio gates below are kept too (defense in depth).
    if seg.get("transcriptSource") != "human_verified":
        reasons.append(f"not-human-verified:source={seg.get('transcriptSource', '?')}")
    if seg.get("trainingGrade") == "reject":
        reasons.append("app-rejected:" + "|".join((seg.get("trainingReasons") or [])[:5]))
    audio_risk = [r for r in (seg.get("trainingReasons") or []) if r in NON_ALIGNMENT_AUDIO_RISK]
    if audio_risk:
        reasons.append("audio-review-risk:" + "|".join(audio_risk))
    if not text:
        reasons.append("empty-training-transcript")
        return reasons  # nothing further to measure on empty text

    # text gates
    fillers = filler_tokens_in(text)
    if fillers:
        reasons.append("fillers:" + "|".join(fillers[:5]))
    fragments = fragment_tokens_in(text)
    if fragments:
        reasons.append("false-start-fragments:" + "|".join(fragments[:5]))
    words = len(tokens_of(text))
    if words < cfg.min_words:
        reasons.append(f"too-few-words:{words}<{cfg.min_words}")
    if blockwords:
        hits = [t for t in tokens_of(text) if t.strip(PUNCT_STRIP) in blockwords]
        if hits:
            reasons.append("blockwords:" + "|".join(hits[:5]))

    # timing gates
    duration_ms = seg.get("durationMs") or 0
    if duration_ms < cfg.min_duration_ms:
        reasons.append(f"too-short:{duration_ms}ms<{cfg.min_duration_ms}ms")
    if duration_ms > cfg.max_duration_ms:
        reasons.append(f"too-long:{duration_ms}ms>{cfg.max_duration_ms}ms")
    cps = chars_per_second(text, duration_ms)
    if cps is not None and not (cfg.min_cps <= cps <= cfg.max_cps):
        reasons.append(f"cps-out-of-range:{cps:.1f}")

    # audio gates — the app's own measured metrics; unmeasured is not clean (strict default)
    for field, threshold, op, label in (
        ("clippingRatio", cfg.max_clipping_ratio, "max", "clipping"),
        ("snrDb", cfg.min_snr_db, "min", "snr"),
        ("rmsDb", cfg.min_rms_db, "min", "rms"),
    ):
        value = seg.get(field)
        if value is None:
            if not cfg.lenient_audio:
                reasons.append(f"missing-audio-metric:{field}")
        elif op == "max" and value > threshold:
            reasons.append(f"{label}:{value:.4g}>{threshold}")
        elif op == "min" and value < threshold:
            reasons.append(f"{label}:{value:.4g}<{threshold}")

    return reasons


def build(cfg: argparse.Namespace) -> int:
    src = Path(cfg.export_jsonl)
    if not src.is_file():
        print(f"ERROR: input not found: {src}", file=sys.stderr)
        return 1

    blockwords: frozenset[str] = frozenset()
    if cfg.blockwords:
        # utf-8-sig: a Windows-authored list (PowerShell/Notepad) starts with a BOM that would
        # otherwise glue onto the first word and silently disarm it (adversarial review finding).
        blockwords = frozenset(
            w.strip() for w in Path(cfg.blockwords).read_text(encoding="utf-8-sig").splitlines() if w.strip()
        )

    out_dir = Path(cfg.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    premium, rejected = [], []
    # Duplicate detection keys on the segment ID — the only per-clip identity in the export.
    # (audioPath, text) is NOT usable: audioPath is reduced to a basename for privacy, so every
    # VAD chunk of one recording shares it, and a short sentence legitimately spoken twice in
    # the same recording would be wrongly deduped (adversarial review finding).
    seen_ids: set[str] = set()
    repeat_info = 0
    with io.open(src, encoding="utf-8") as fh:
        for line_no, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            try:
                seg = json.loads(line)
            except json.JSONDecodeError as exc:
                print(f"ERROR: line {line_no} is not valid JSON: {exc}", file=sys.stderr)
                return 1
            if not isinstance(seg, dict):
                print(f"ERROR: line {line_no} is not a JSON object: {type(seg).__name__}", file=sys.stderr)
                return 1
            reasons = evaluate_segment(seg, cfg, blockwords)
            seg_id = seg.get("id") or f"line-{line_no}"
            if not reasons and seg_id in seen_ids:
                reasons = ["duplicate-of-earlier-row"]
            if reasons:
                seg["premiumRejectReasons"] = reasons
                rejected.append(seg)
            else:
                seen_ids.add(seg_id)
                repeat_info += repeated_word_runs(seg.get("trainingTranscript") or "")
                premium.append(seg)

    total = len(premium) + len(rejected)
    if total == 0:
        print(f"ERROR: {src} contained no segments.", file=sys.stderr)
        return 1

    for name, rows in (("premium.jsonl", premium), ("rejected.jsonl", rejected)):
        with io.open(out_dir / name, "w", encoding="utf-8") as fh:
            for row in rows:
                fh.write(json.dumps(row, ensure_ascii=False) + "\n")

    # honest tally — every count is from real rows written above
    reason_counts: dict[str, int] = {}
    for row in rejected:
        for reason in row["premiumRejectReasons"]:
            head = reason.split(":", 1)[0]
            reason_counts[head] = reason_counts.get(head, 0) + 1
    print(f"premium: {len(premium)} / {total} segments -> {out_dir / 'premium.jsonl'}")
    print(f"rejected: {len(rejected)} -> {out_dir / 'rejected.jsonl'} (reasons kept per row)")
    for head, count in sorted(reason_counts.items(), key=lambda kv: -kv[1]):
        print(f"  {head}: {count}")
    if repeat_info:
        print(f"info: {repeat_info} immediate word-repeat pair(s) in premium (legit Sorani reduplication; not a gate)")

    if not premium:
        print("ERROR: premium tier is EMPTY — no segment passed every gate.", file=sys.stderr)
        return 1
    return 0


def make_parser() -> argparse.ArgumentParser:
    """The single source of truth for every threshold default. The regression gate builds its
    config through this same parser, so a loosened default can never slip past the tests."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("export_jsonl", help="JSONL export from the app (Export -> JSONL)")
    parser.add_argument("--out-dir", required=True, help="directory for premium.jsonl + rejected.jsonl")
    parser.add_argument("--blockwords", help="optional UTF-8 file, one blocked word per line")
    parser.add_argument("--min-words", type=int, default=3, help="minimum words for a full sentence (default 3)")
    parser.add_argument("--min-duration-ms", type=int, default=1000, help="default 1000")
    parser.add_argument("--max-duration-ms", type=int, default=30000, help="default 30000")
    parser.add_argument("--min-cps", type=float, default=2.0, help="min chars/sec (default 2.0)")
    parser.add_argument("--max-cps", type=float, default=30.0, help="max chars/sec (default 30.0)")
    parser.add_argument("--max-clipping-ratio", type=float, default=0.01, help="default 0.01 (1%%)")
    parser.add_argument("--min-snr-db", type=float, default=10.0, help="default 10 dB")
    parser.add_argument("--min-rms-db", type=float, default=-45.0, help="default -45 dB (rejects near-silence)")
    parser.add_argument(
        "--lenient-audio",
        action="store_true",
        help="keep rows whose audio metrics are missing (default: strict — unmeasured is rejected)",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    return build(make_parser().parse_args(argv))


if __name__ == "__main__":
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")  # Windows consoles default to cp1252
    sys.exit(main())
