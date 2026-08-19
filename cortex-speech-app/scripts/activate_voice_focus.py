"""Turn the owner's blind-listen verdict into a live queue focus, or refuse if the verdict is weak.

`host_voice_probe` writes a candidate-host cluster and a blind sample (two-thirds candidate, one-third
other, shuffled). The owner listens and marks each sample clip as the host or not. THIS script scores
that verdict against the key and, only if the candidate cluster really is the voice the owner heard,
writes `<data_dir>/voice_focus.json` so every reviewer's queue narrows to it.

It refuses on a weak verdict rather than activating a focus that would point eight paid reviewers at
the wrong person's clips. The bar: every clip the owner called HOST must be in the candidate cluster
and every clip they called NOT-HOST must be outside it — at most one miss either way, because the
owner's own 15-clip calibration of the speaker-change threshold set 15/15 as the precedent and a
cluster that disagrees with their ear twice is a cluster that needs a better threshold, not a name.

The name goes ONLY into the data-dir JSON. It never enters tracked code (repo hygiene: names stay
out of the public repo).

Usage:
    python scripts/activate_voice_focus.py --name Some_Voice --host 1,3,4,6,7,9,10,12,14,15
        (numbers are the WAV numbers in voice_focus/blind_sample/, 1-based; everything not listed
         is taken as NOT the host)
    python scripts/activate_voice_focus.py --deactivate
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

MAX_DISAGREEMENTS = 1


def data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    return Path(appdata) / "cortex-speech" if appdata else Path.home() / ".local" / "share" / "cortex-speech"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, default=data_dir())
    parser.add_argument("--name", help="the speaker label the export will carry, e.g. Some_Voice")
    parser.add_argument("--host", help="comma-separated 1-based sample numbers the owner judged to be the host")
    parser.add_argument("--deactivate", action="store_true", help="remove the focus; every queue returns to full")
    args = parser.parse_args()

    focus_path = args.data_dir / "voice_focus.json"
    if args.deactivate:
        if focus_path.is_file():
            retired = focus_path.with_name(f"voice_focus.retired-{datetime.now(timezone.utc):%Y%m%dT%H%M%SZ}.json")
            focus_path.rename(retired)
            print(f"focus retired -> {retired.name}; queues are full again on the next fetch")
        else:
            print("no focus was active")
        return 0
    if not args.name or not args.host:
        parser.error("--name and --host are required (or --deactivate)")

    out = args.data_dir / "voice_focus"
    key_lines = (out / "blind_sample_KEY.txt").read_text(encoding="utf-8").split("\n")
    key = [l.split("\t") for l in key_lines if l.strip()]
    candidates = (out / "candidate_segment_ids.txt").read_text(encoding="utf-8").split()
    try:
        judged_host = {int(n) for n in args.host.split(",") if n.strip()}
    except ValueError:
        parser.error("--host must be comma-separated integers")
    if any(n < 1 or n > len(key) for n in judged_host):
        parser.error(f"--host numbers must be 1..{len(key)}")

    # Score the ear against the machine.
    misses: list[str] = []
    for n, (seg_id, label) in enumerate(key, start=1):
        machine_says_host = label == "CANDIDATE"
        owner_says_host = n in judged_host
        if machine_says_host != owner_says_host:
            misses.append(
                f"  #{n:02d} {seg_id[:8]}  owner: {'HOST' if owner_says_host else 'not'}   cluster: {'HOST' if machine_says_host else 'not'}"
            )
    agree = len(key) - len(misses)
    print(f"blind sample : {len(key)} clips, owner and cluster agree on {agree}")
    for m in misses:
        print(m)
    if len(misses) > MAX_DISAGREEMENTS:
        print(
            f"\nREFUSED: {len(misses)} disagreement(s) > {MAX_DISAGREEMENTS}. The candidate cluster is not cleanly the voice you "
            "heard. Re-run host_voice_probe with a different --threshold (lower merges more, higher splits) "
            "and judge again. No focus was written."
        )
        return 1

    record = {
        "name": args.name,
        "activated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "basis": f"host_voice_probe candidate cluster; owner blind-judged {agree}/{len(key)}",
        "segment_ids": candidates,
    }
    tmp = focus_path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(record, indent=2), encoding="utf-8", newline="\n")
    os.replace(tmp, focus_path)
    print(f"\nACTIVE: {focus_path}")
    print(f"  {len(candidates)} clip(s) of {args.name!r} — every reviewer's next queue refill serves only these.")
    print("  Remove with: python scripts/activate_voice_focus.py --deactivate")
    return 0


if __name__ == "__main__":
    sys.exit(main())
