#!/usr/bin/env python3
"""Fuse several engines' hypothesis TSVs into ONE draft, so fusion can be MEASURED against the
champion instead of assumed to help.

    python scripts/fuse_hypotheses.py --out fused.tsv --method rover \
        champion=champion_gold.tsv gemini=gemini_gold.tsv mms=mms_gold.tsv

Every input is the shared `<wav_path>\t<reference>\t<hypothesis>` format written by
`engine_hypotheses_dump.py` and `gemini_asr_bench.rs`; the output is the same format, so
`scorecard_gemini.py` (which imports the champion's own normalization + seed-42 bootstrap) scores
the fused draft with the identical metric. One format, one metric, no second opinion.

THE LAW THIS OBEYS. A fused word must have been PROPOSED BY AN ENGINE THAT HEARD THE AUDIO. No
method here may emit a token no input contained — that is the difference between hypothesis
combination (legitimate, decades old) and generative rewriting (measured on this project's own data
to rewrite 11.1% of characters, translating loanwords the speaker actually said). `--verify-closure`
re-checks this property on the output and fails loudly if any token is unsupported.

METHODS
  anchor      Champion text, changed ONLY where every other engine agrees on the same different
              word. High precision, low recall: the conservative edit.
  rover       Word-level vote across all engines after alignment to the champion, weights supplied
              via --weight NAME=W (default 1.0). Ties keep the champion's word — the incumbent must
              be OUTVOTED, never merely matched.

Both align each engine to the CHAMPION's word sequence (the anchor), so the output length tracks the
champion and no engine can inject or delete a run of words wholesale.
"""
import argparse
import difflib
import sys
from collections import Counter


def read_tsv(path):
    rows = {}
    ref_of = {}
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            parts = line.rstrip("\n").split("\t")
            if len(parts) >= 3 and parts[1].strip():
                rows[parts[0]] = parts[2]
                ref_of[parts[0]] = parts[1]
    return rows, ref_of


def align_to_anchor(anchor_words, other_words):
    """Map each anchor index -> the other engine's word at that slot (or None).

    difflib on the word sequences: 'equal' spans map 1:1; a 'replace' span of equal length maps
    positionally; anything else contributes nothing rather than guessing an alignment. Refusing to
    guess is deliberate — a wrong alignment would let one engine's word vote against an unrelated
    word of another.
    """
    out = [None] * len(anchor_words)
    sm = difflib.SequenceMatcher(a=anchor_words, b=other_words, autojunk=False)
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            for k in range(i2 - i1):
                out[i1 + k] = other_words[j1 + k]
        elif tag == "replace" and (i2 - i1) == (j2 - j1):
            for k in range(i2 - i1):
                out[i1 + k] = other_words[j1 + k]
    return out


def fuse_clip(anchor_text, others_text, method, weights, anchor_name):
    anchor = anchor_text.split()
    if not anchor:
        return anchor_text
    aligned = {name: align_to_anchor(anchor, text.split()) for name, text in others_text.items()}

    fused = []
    for i, word in enumerate(anchor):
        votes = {name: aligned[name][i] for name in aligned if aligned[name][i] is not None}
        if not votes:
            fused.append(word)
            continue

        if method == "anchor":
            alternatives = {v for v in votes.values()}
            # Change ONLY on unanimity of every other engine that has an opinion here, and only when
            # they all name the SAME replacement.
            if len(alternatives) == 1 and len(votes) == len(aligned):
                alt = alternatives.pop()
                fused.append(alt if alt != word else word)
            else:
                fused.append(word)
            continue

        # rover: weighted vote, incumbent included at its own weight.
        tally = Counter()
        tally[word] += weights.get(anchor_name, 1.0)
        for name, w in votes.items():
            tally[w] += weights.get(name, 1.0)
        best_word, best_score = word, tally[word]
        for cand, score in tally.items():
            # Strictly greater: a tie keeps the champion. The incumbent must be OUTVOTED.
            if score > best_score:
                best_word, best_score = cand, score
        fused.append(best_word)
    return " ".join(fused)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("inputs", nargs="+", help="NAME=path.tsv (the FIRST is the anchor/incumbent)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--method", choices=["anchor", "rover"], default="rover")
    ap.add_argument("--weight", action="append", default=[], help="NAME=W")
    ap.add_argument("--verify-closure", action="store_true", default=True)
    args = ap.parse_args()

    named = []
    for spec in args.inputs:
        name, _, path = spec.partition("=")
        if not path:
            print(f"bad input '{spec}' (expected NAME=path.tsv)")
            return 2
        named.append((name, path))
    weights = {}
    for spec in args.weight:
        name, _, val = spec.partition("=")
        weights[name] = float(val)

    anchor_name, anchor_path = named[0]
    anchor_rows, ref_of = read_tsv(anchor_path)
    others = {name: read_tsv(path)[0] for name, path in named[1:]}

    # Only clips EVERY engine transcribed are fused and scored. Scoring a fused set over a different
    # clip population than the baseline would compare two different exams.
    common = set(anchor_rows)
    for rows in others.values():
        common &= set(rows)
    common = sorted(common)
    print(f"anchor        : {anchor_name} ({len(anchor_rows)} clips)")
    for name, rows in others.items():
        print(f"engine        : {name} ({len(rows)} clips)")
    print(f"fusing        : {len(common)} clips present in ALL engines")
    print(f"method        : {args.method}   weights: {weights or '(all 1.0)'}")

    changed = unsupported = 0
    with open(args.out, "w", encoding="utf-8", newline="") as out:
        for path in common:
            others_text = {name: rows[path] for name, rows in others.items()}
            fused = fuse_clip(anchor_rows[path], others_text, args.method, weights, anchor_name)
            if fused != anchor_rows[path]:
                changed += 1
            if args.verify_closure:
                proposed = set(anchor_rows[path].split())
                for text in others_text.values():
                    proposed |= set(text.split())
                for tok in fused.split():
                    if tok not in proposed:
                        unsupported += 1
            out.write(f"{path}\t{ref_of[path]}\t{fused}\n")

    print(f"clips changed : {changed} / {len(common)} ({changed / max(len(common), 1) * 100:.1f}%)")
    if args.verify_closure:
        if unsupported:
            print(f"FAIL: {unsupported} fused tokens were proposed by NO engine — fusion must never invent")
            return 1
        print("closure       : OK — every fused token was proposed by an engine that heard the audio")
    print(f"wrote         : {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
