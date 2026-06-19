#!/usr/bin/env python3
"""
Per-engine Sorani ASR evaluation against a human gold reference.

Given the ensemble's per-segment hypotheses and a gold transcript, scores every
engine (OmniASR-7B / Whisper-ckb / XLSR-ckb / ROVER consensus) with corpus-level
CER and WER, ranks them, and checks whether the ensemble's agreement score
actually predicts accuracy (the calibration signal needed to gate the autonomy
dial on real confidence instead of a constant).

Usage:
  python3 sorani_eval.py <ensemble.json> <gold.json>
  python3 sorani_eval.py --self-test
Where ensemble.json is the {seg: {omniasr_7b, whisper_ckb, xlsr_ckb, consensus,
agreement_confidence}} dumped by sorani_ensemble_asr.py, and gold.json is
{seg: "correct sorani transcript"} keyed by the same segment ids.
"""
import sys, json, re

ENGINES = ["omniasr_7b", "whisper_ckb", "xlsr_ckb", "consensus"]


def normalize_ckb(text):
    text = re.sub(r'[كڪڬ]', 'ک', text)
    text = re.sub(r'[يے]', 'ی', text)
    text = text.replace('ى', 'ی').replace('ـ', '')
    text = re.sub(r'[ً-ٰٟ]', '', text)
    return re.sub(r'\s+', ' ', text).strip()


def _lev(a, b):
    """Levenshtein distance over sequences a, b."""
    if not a:
        return len(b)
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[-1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def char_edits(ref, hyp):
    r, h = list(normalize_ckb(ref)), list(normalize_ckb(hyp))
    return _lev(r, h), len(r)


def word_edits(ref, hyp):
    r, h = normalize_ckb(ref).split(), normalize_ckb(hyp).split()
    return _lev(r, h), len(r)


def evaluate(ensemble, gold):
    keys = [k for k in gold if k in ensemble]
    rows = {}
    for eng in ENGINES:
        cd = cl = wd = wl = 0
        for k in keys:
            ce, cn = char_edits(gold[k], ensemble[k].get(eng, ""))
            we, wn = word_edits(gold[k], ensemble[k].get(eng, ""))
            cd += ce; cl += cn; wd += we; wl += wn
        rows[eng] = {
            "cer": cd / cl if cl else 0.0,
            "wer": wd / wl if wl else 0.0,
        }
    # calibration: does agreement predict consensus accuracy?
    pts = []
    for k in keys:
        ce, cn = char_edits(gold[k], ensemble[k].get("consensus", ""))
        pts.append((ensemble[k].get("agreement_confidence", 0.0), 1.0 - (ce / cn if cn else 0.0)))
    calib = None
    if len(pts) >= 2:
        import statistics
        ax = statistics.mean(p[0] for p in pts)
        ay = statistics.mean(p[1] for p in pts)
        num = sum((p[0] - ax) * (p[1] - ay) for p in pts)
        dx = sum((p[0] - ax) ** 2 for p in pts) ** 0.5
        dy = sum((p[1] - ay) ** 2 for p in pts) ** 0.5
        calib = num / (dx * dy) if dx > 0 and dy > 0 else None
    return rows, calib, len(keys)


def report(rows, calib, n):
    print(f"\n=== Per-engine accuracy on {n} gold segment(s) ===")
    ranked = sorted(rows.items(), key=lambda kv: kv[1]["cer"])
    print(f"{'engine':<14}{'CER':>8}{'WER':>8}")
    for eng, m in ranked:
        print(f"{eng:<14}{m['cer']*100:>7.1f}%{m['wer']*100:>7.1f}%")
    print(f"\nbest by CER: {ranked[0][0]}  ({ranked[0][1]['cer']*100:.1f}% CER)")
    if calib is not None:
        verdict = "agreement predicts accuracy" if calib > 0.3 else "agreement is a weak predictor (needs more data)"
        print(f"agreement<->accuracy correlation: {calib:+.2f}  ({verdict})")
    return ranked


def self_test():
    # Known case: gold has 4 words; hyp drops 1 word and substitutes 1 char.
    ens = {"0": {"omniasr_7b": "ئەمە دەنگی خۆش", "whisper_ckb": "ئەمە دەنگی خۆشە",
                 "xlsr_ckb": "ئەمە رەنگی خۆشە", "consensus": "ئەمە دەنگی خۆشە",
                 "agreement_confidence": 0.9}}
    gold = {"0": "ئەمە دەنگی خۆشە"}
    rows, calib, n = evaluate(ens, gold)
    # consensus == gold -> 0 CER/WER; xlsr substituted د->ر (1 char of 13) ~7.7%
    assert abs(rows["consensus"]["cer"]) < 1e-9, rows["consensus"]
    assert abs(rows["whisper_ckb"]["wer"]) < 1e-9, rows["whisper_ckb"]
    assert rows["xlsr_ckb"]["cer"] > 0, rows["xlsr_ckb"]
    # omniasr substituted the final word "خۆش" for gold "خۆشە" -> 1 of 3 words (1 of 15 chars)
    assert abs(rows["omniasr_7b"]["wer"] - 1 / 3) < 1e-9, rows["omniasr_7b"]
    assert abs(rows["omniasr_7b"]["cer"] - 1 / 15) < 1e-9, rows["omniasr_7b"]
    print("self-test OK:", json.dumps({k: {m: round(v, 3) for m, v in d.items()} for k, d in rows.items()}, ensure_ascii=False))


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    if len(sys.argv) == 2 and sys.argv[1] == "--self-test":
        self_test()
    elif len(sys.argv) == 3:
        ensemble = json.load(open(sys.argv[1], encoding="utf-8"))
        gold = json.load(open(sys.argv[2], encoding="utf-8"))
        report(*evaluate(ensemble, gold))
    else:
        print(__doc__)
        sys.exit(2)
