"""Is a fine-tune retrain worth running yet? A report, not a guess.

Every decode-side improvement lever is now MEASURED dead on this stack (ledger iters 205/207:
LM fusion is not bound for offline CTC, LOOP-0 promotion is harmful out-of-sample, hotwords are
refused/fatal). The one lever left is review volume feeding an acoustic fine-tune — so the honest
question after each review session is "enough new material yet?", and it should be answered from
the database, not from memory.

Read-only by construction (SQLite URI mode=ro). Reports what the export would actually publish:
the eligibility SQL below is copied from stats.rs, whose equality with the REAL `export_dataset`
output is pinned by the Rust test `the_dashboards_verified_count_equals_what_the_export_actually_
writes` — this script deliberately does NOT invent a third opinion about what counts (the "tally
counts rows the export drops" defect class has been fixed six times in this repo; mirrors of the
pinned SQL are the only accepted shape).

No threshold is invented either: without --min-pairs it prints numbers and stops. The verdict line
only exists when the owner supplies the bar. The champion CER is read from champion.json rather
than hardcoded, so this report can never contradict the real record.

Usage:
    python scripts/retrain_readiness.py [--db PATH] [--min-pairs N]
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
from pathlib import Path

# Copied from stats.rs (see module doc for why this exact shape and no other).
EFFECTIVE = """TRIM(CASE
        WHEN TRIM(COALESCE(verdict_transcript,'')) <> ''
             AND (LOWER(COALESCE(human_decision,'')) IN ('accept','edit','human_accept','human_edit')
                  OR LOWER(COALESCE(verdict,'')) IN ('human_accept','human_edit'))
            THEN verdict_transcript
        WHEN TRIM(COALESCE(annotated_transcript,'')) <> '' THEN annotated_transcript
        WHEN TRIM(COALESCE(verdict_transcript,'')) <> '' THEN verdict_transcript
        WHEN TRIM(COALESCE(normalized_transcript,'')) <> '' THEN normalized_transcript
        ELSE COALESCE(raw_transcript,'') END)"""
REJECTED = "(COALESCE(human_decision,'') IN ('reject','human_reject') OR COALESCE(verdict,'') = 'human_reject')"
PLACEHOLDER = f"""(substr({EFFECTIVE}, 1, 8) = '[Pending'
      OR substr({EFFECTIVE}, 1, 16) = '[ASR unavailable'
      OR LOWER({EFFECTIVE}) IN ('n/a','null'))"""
ELIGIBLE = f"(verified AND NOT ({REJECTED} OR {PLACEHOLDER}))"


def default_db() -> Path:
    root = os.environ.get("CORTEX_APP_DATA_DIR") or os.path.join(os.environ.get("APPDATA", ""), "cortex-speech")
    return Path(root) / "cortex-speech.db"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--db", type=Path, default=default_db())
    ap.add_argument(
        "--min-pairs",
        type=int,
        default=None,
        help="owner-chosen bar for a retrain to be worth the GPU time; omit for a report with no verdict",
    )
    args = ap.parse_args()

    if not args.db.exists():
        print(f"database not found: {args.db}", file=sys.stderr)
        return 2
    con = sqlite3.connect(f"file:{args.db.as_posix()}?mode=ro", uri=True, timeout=5)
    c = con.cursor()

    eligible, minutes, words = c.execute(
        f"""SELECT COUNT(*),
                   COALESCE(SUM(duration_ms), 0) / 60000.0,
                   COALESCE(SUM(LENGTH({EFFECTIVE}) - LENGTH(REPLACE({EFFECTIVE}, ' ', '')) + 1), 0)
            FROM speech_segments WHERE {ELIGIBLE}"""
    ).fetchone()
    pending = c.execute(f"SELECT COUNT(*) FROM speech_segments WHERE NOT verified AND NOT {REJECTED}").fetchone()[0]
    pairs = c.execute("SELECT COUNT(*) FROM agent_examples WHERE verified_by_human = 1").fetchone()[0]

    # `champion.json` is the app's startup MIRROR of the model registry, so "no champions" is a real
    # answer about this machine, not a missing file. Say that in words. Dumping the raw
    # `{"champions": {}, "schema": 1}` directly above a step that reads "promote ONLY if the measured
    # CER beats the champion above" invites exactly the wrong reading — that there is a champion object
    # up there to beat. There is not, and the reader has to know before they act on step 5.
    champion = "unknown (champion.json missing)"
    champ_path = args.db.parent / "champion.json"
    if champ_path.exists():
        try:
            doc = json.loads(champ_path.read_text(encoding="utf-8"))
            families = doc.get("champions") or {}
            champion = json.dumps(doc) if families else "NONE RECORDED — the model registry has no champion"
        except (OSError, ValueError) as e:
            champion = f"unreadable ({e})"

    print("retrain readiness — read-only report against the export-pinned eligibility rule")
    print(f"  library                     : {args.db}")
    print(f"  export-eligible clips       : {eligible}")
    print(f"  eligible audio              : {minutes:.1f} min")
    print(f"  human-gold words            : {words}")
    print(f"  human-verified DPO pairs    : {pairs}")
    print(f"  still pending review        : {pending}")
    print(f"  champion on frozen gold     : {champion}")

    if args.min_pairs is None:
        print("\nno --min-pairs given: report only, no verdict (a bar this script invented would be a guess).")
    elif pairs >= args.min_pairs:
        print(f"\nREADY by the owner's bar ({pairs} >= {args.min_pairs} pairs).")
    else:
        print(f"\nNOT YET by the owner's bar ({pairs} < {args.min_pairs} pairs — {args.min_pairs - pairs} to go).")

    print(
        "\nthe retrain path, when ready (each step already exists; nothing here trains anything):\n"
        "  1. app: Export -> fine-tune pack (export_finetune_pack; holdout-gold leak guard built in)\n"
        "  2. train on the pack (owner GPU; checkpoint convention: Desktop/Kurdish_ASR_Model_Export)\n"
        "  3. python scripts/export_finetuned_onnx.py  &&  python scripts/quantize_finetuned_onnx.py\n"
        "  4. python scripts/measure_finetuned_cer.py   # frozen gold set — the ONLY promotion evidence\n"
        "  5. promote ONLY if the measured CER beats the champion above; otherwise keep the champion"
    )
    if champion.startswith("NONE RECORDED"):
        # Not a warning about the CODE: `decide_promotion` refuses without a paired baseline, so an
        # empty registry cannot let a worse model through. It is a warning about this MACHINE — step 5
        # has nothing to compare against, so whatever champion exists as files on disk and as a number
        # in the ledger is not something the app can check a challenger against.
        print(
            "\nNOTE: the model registry on this machine holds no champion, so step 5 has nothing to\n"
            "      compare a challenger against. The promotion gate refuses without a paired baseline\n"
            "      (registry::decide_promotion), so nothing can be promoted by accident — but a champion\n"
            "      that lives only as files and prose is not one this app can defend a retrain against."
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
