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
    python scripts/activate_voice_focus.py --merge-round2 --host 2,5,6,9,11,...
        (round 2: scores each suspect cluster SEPARATELY; a cluster the owner confirms on every one
         of its sample clips is merged into the live focus; any cluster they reject is left out; if
         they reject a CONTROL clip from the already-confirmed host, the round is void)
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


def merge_round2(args: argparse.Namespace, focus_path: Path) -> int:
    """Per-cluster verdict. Each suspect is judged on its own sample clips and merged only if CLEAN."""
    r2 = args.data_dir / "voice_focus" / "round2"
    key = [l.split("\t") for l in (r2 / "blind_sample_KEY.txt").read_text(encoding="utf-8").split("\n") if l.strip()]
    if not args.host:
        print("--host is required: the 1-based sample numbers the owner judged to be the host")
        return 2
    judged_host = {int(n) for n in args.host.split(",") if n.strip()}
    if not focus_path.is_file():
        print("no active focus to merge into — run round 1 first")
        return 1
    focus = json.loads(focus_path.read_text(encoding="utf-8"))
    existing = set(focus["segment_ids"])

    # Which cluster is the confirmed host? It is whichever cluster's ids are ALREADY in the focus.
    by_cluster: dict[str, list[tuple[int, bool]]] = {}
    control_cluster = None
    for n, (seg_id, label) in enumerate(key, start=1):
        by_cluster.setdefault(label, []).append((n, n in judged_host))
        if seg_id in existing:
            control_cluster = label

    print(f"round 2: {len(key)} clips across {len(by_cluster)} cluster(s); control cluster = {control_cluster}")
    void = False
    merged: list[str] = []
    for label, verdicts in sorted(by_cluster.items()):
        said_host = sum(1 for _, h in verdicts if h)
        total = len(verdicts)
        nums = ",".join(str(n) for n, _ in verdicts)
        if label == control_cluster:
            ok = said_host == total
            print(f"  {label:12} CONTROL  {said_host}/{total} called host  (clips {nums})  {'ok' if ok else 'EAR OFF — round void'}")
            if not ok:
                void = True
            continue
        clean = said_host == total
        print(f"  {label:12} suspect  {said_host}/{total} called host  (clips {nums})  {'MERGE' if clean else 'reject'}")
        if clean:
            merged.append(label)
    if void:
        print("\nVOID: a control clip from the already-confirmed host was called 'not him'. Nothing merged. Listen again another time.")
        return 1
    if not merged:
        print("\nno suspect cluster was clean; focus unchanged")
        return 0

    added: set[str] = set()
    for label in merged:
        c = label.split(":", 1)[1]
        ids = (r2 / f"cluster_{c}_segment_ids.txt").read_text(encoding="utf-8").split()
        added.update(i for i in ids if i not in existing)
    focus["segment_ids"] = sorted(existing | added)
    focus.setdefault("merges", []).append(
        {"at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"), "clusters": merged, "added": len(added)}
    )
    tmp = focus_path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(focus, indent=2), encoding="utf-8", newline="\n")
    os.replace(tmp, focus_path)
    print(f"\nMERGED {merged}: +{len(added)} clip(s) -> focus now {len(focus['segment_ids'])} clip(s) of {focus['name']!r}")
    print("  live on every reviewer's next queue refill (no restart needed).")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, default=data_dir())
    parser.add_argument("--name", help="the speaker label the export will carry, e.g. Some_Voice")
    parser.add_argument("--host", help="comma-separated 1-based sample numbers the owner judged to be the host")
    parser.add_argument("--deactivate", action="store_true", help="remove the focus; every queue returns to full")
    parser.add_argument("--merge-round2", action="store_true", help="judge round-2 suspect clusters and merge confirmed ones")
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
    if args.merge_round2:
        return merge_round2(args, focus_path)
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

    if not candidates:
        # The server treats a focus that names no ids as BROKEN and serves NOTHING to anyone
        # (fail-closed, 2026-08-20). Refusing to write it here keeps eight queues alive.
        print("REFUSED: candidate_segment_ids.txt names no clips — activating this focus would 503 every queue.")
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
