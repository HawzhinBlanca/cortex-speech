"""Finish decisions the two-write desktop path left half-written, and stamp owner rights it skipped.

Two live-data repairs, both found 2026-08-20:

  1. NINE rows carried `human_decision` with `verified = 0` — real desktop review the export cannot
     see, because `finalize` was derived from a CAS token only the phone supplies. Each is finalized
     ONLY if its own listening receipt clears the bar, matched by TIME (the decision bumps the
     revision past its own receipt, so a revision lookup finds nothing — the same trap the readiness
     gate hit). A row without evidence is left alone and reported.

  2. Clips from two owner recordings carry no `rights_license`. Canon: every clip the owner supplies
     is `owner-full-rights`, unrestricted, rights clearance CLOSED. These are the owner's own podcast
     episodes sitting beside 23 stamped ones; the gap is a missing stamp, not an open question.

Dry run by default.
"""

from __future__ import annotations

import argparse
import os
import sqlite3
import sys
from pathlib import Path

MIN_COVERAGE = 0.85
OWNER_LICENSE = "owner-full-rights"
OWNER_USE = "unrestricted: train, evaluate, publish, redistribute, commercial"


def data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    return Path(appdata) / "cortex-speech" if appdata else Path.home() / ".local" / "share" / "cortex-speech"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--db", default=str(data_dir() / "cortex-speech.db"))
    ap.add_argument("--apply", action="store_true")
    ap.add_argument(
        "--stamp-like",
        action="append",
        default=[],
        help="SQL LIKE over audio_path naming OWNER-supplied recordings to stamp; repeatable, required",
    )
    args = ap.parse_args()
    con = sqlite3.connect(args.db)
    try:
        print("=== 1. decided-but-unverified rows ===")
        rows = con.execute(
            "SELECT id, human_decision, corrected_at FROM speech_segments "
            " WHERE COALESCE(human_decision,'') <> '' AND verified = 0"
        ).fetchall()
        finalizable, unevidenced = [], []
        for sid, dec, at in rows:
            best = con.execute(
                "SELECT MAX(coverage_ratio) FROM playback_receipts WHERE segment_id = ? "
                "  AND created_at BETWEEN datetime(?, '-10 seconds') AND datetime(?, '+10 seconds')",
                (sid, at, at),
            ).fetchone()[0]
            (finalizable if best is not None and best >= MIN_COVERAGE else unevidenced).append((sid, dec, best))
        for sid, dec, best in finalizable:
            print(f"  finalize {sid[:8]} {dec:7} coverage {best:.3f}")
        for sid, dec, best in unevidenced:
            print(f"  LEAVE    {sid[:8]} {dec:7} coverage {best if best is not None else 'none'} — no evidence, needs re-review")

        print("\n=== 2. clips with no rights stamp ===")
        overall = con.execute(
            "SELECT COUNT(*), COUNT(DISTINCT audio_path) FROM speech_segments "
            " WHERE COALESCE(TRIM(rights_license),'') = ''"
        ).fetchone()
        print(f"  unstamped overall: {overall[0]} clip(s) across {overall[1]} file(s)")
        if not args.stamp_like:
            print("  no --stamp-like given: stamping nothing. Canon covers OWNER-supplied audio only,")
            print("  and a blanket stamp would claim third-party corpora as the owner's.")
            total_unstamped = 0
        else:
            where = " OR ".join(["audio_path LIKE ?"] * len(args.stamp_like))
            targets = con.execute(
                f"SELECT audio_path, COUNT(*) FROM speech_segments "
                f" WHERE COALESCE(TRIM(rights_license),'') = '' AND ({where}) GROUP BY audio_path ORDER BY 2 DESC",
                args.stamp_like,
            ).fetchall()
            for path, n in targets:
                print(f"  stamp {n:5} clips  {path}")
            total_unstamped = sum(n for _, n in targets)

        if not args.apply:
            print(f"\nDRY RUN — would finalize {len(finalizable)}, stamp {total_unstamped}. Pass --apply.")
            return 0

        for sid, _, _ in finalizable:
            con.execute("UPDATE speech_segments SET verified = 1, updated_at = datetime('now') WHERE id = ?", (sid,))
        if args.stamp_like:
            where = " OR ".join(["audio_path LIKE ?"] * len(args.stamp_like))
            con.execute(
                f"UPDATE speech_segments SET rights_license = ?, rights_permitted_use = ?, "
                f"       rights_consent_basis = 'owner declaration 2026-08-14', updated_at = datetime('now') "
                f" WHERE COALESCE(TRIM(rights_license),'') = '' AND ({where})",
                (OWNER_LICENSE, OWNER_USE, *args.stamp_like),
            )
        con.commit()
        print(f"\nfinalized {len(finalizable)} row(s); stamped {total_unstamped} clip(s) as {OWNER_LICENSE}")
        if unevidenced:
            print(f"{len(unevidenced)} row(s) left unverified for re-review — they carry no listening evidence")
    finally:
        con.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
