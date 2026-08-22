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
import hashlib
import json
import os
import shutil
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path

from activate_review_pilot import acquire_cortex_lock

MAX_DISAGREEMENTS = 1


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def ids_sha256(ids: list[str]) -> str:
    return hashlib.sha256(("\n".join(ids) + "\n").encode("utf-8")).hexdigest()


def _atomic_write_json(path: Path, payload: dict) -> None:
    tmp = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    try:
        with tmp.open("w", encoding="utf-8", newline="\n") as handle:
            json.dump(payload, handle, ensure_ascii=False, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        # Reparse and pin the only field serving reads before promoting the file.
        check = json.loads(tmp.read_text(encoding="utf-8"))
        ids = check.get("segment_ids")
        if not isinstance(ids, list) or not ids or any(not isinstance(item, str) or not item for item in ids):
            raise ValueError("generated focus does not contain a non-empty string segment_ids list")
        if len(ids) != len(set(ids)):
            raise ValueError("generated focus contains duplicate segment ids")
        os.replace(tmp, path)
    finally:
        tmp.unlink(missing_ok=True)


def merge_import_job(args: argparse.Namespace, focus_path: Path) -> int:
    """Atomically add one completed import job to the existing focus after proving its identity."""
    if not focus_path.is_file():
        print("REFUSED: --merge-import-job requires an existing focus to add to")
        return 1
    if not args.label:
        print("REFUSED: --label is required with --merge-import-job")
        return 2
    for required in ("expected_current_sha256", "expected_selection_sha256", "expected_source_dir"):
        if not getattr(args, required):
            print(f"REFUSED: --{required.replace('_', '-')} is required with --merge-import-job")
            return 2
    if args.expected_count is None or args.expected_count <= 0:
        print("REFUSED: a positive --expected-count is required with --merge-import-job")
        return 2

    expected_current = args.expected_current_sha256.lower()
    actual_current = sha256_file(focus_path)
    if actual_current.lower() != expected_current:
        print(f"REFUSED: current focus SHA-256 changed: expected {expected_current}, got {actual_current}")
        return 1
    try:
        focus = json.loads(focus_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        print(f"REFUSED: current focus is unreadable: {error}")
        return 1
    existing_raw = focus.get("segment_ids")
    if not isinstance(existing_raw, list) or not existing_raw:
        print("REFUSED: current focus names no segment ids")
        return 1
    if any(not isinstance(item, str) or not item.strip() for item in existing_raw):
        print("REFUSED: current focus contains a non-string or blank id")
        return 1
    existing = sorted(item.strip() for item in existing_raw)
    if len(existing) != len(set(existing)):
        print("REFUSED: current focus contains duplicate ids; do not normalize policy silently")
        return 1

    db_path = args.data_dir / "cortex-speech.db"
    champion_path = args.data_dir / "champion.json"
    if not db_path.is_file() or not champion_path.is_file():
        print("REFUSED: live database or champion pointer is missing")
        return 1
    try:
        pointer = json.loads(champion_path.read_text(encoding="utf-8"))
        champion_id = pointer["champions"]["omniasr-7b"]["modelVersionId"]
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        print(f"REFUSED: champion pointer cannot identify omn​​iasr-7b: {error}")
        return 1

    conn = sqlite3.connect(f"file:{db_path.as_posix()}?mode=ro", uri=True)
    try:
        job = conn.execute(
            "SELECT dir, total_files, status FROM import_jobs WHERE id = ?", (args.merge_import_job,)
        ).fetchone()
        if job is None:
            print(f"REFUSED: import job does not exist: {args.merge_import_job}")
            return 1
        job_dir, total_files, status = job
        if status != "completed":
            print(f"REFUSED: import job status is {status!r}, not 'completed'")
            return 1
        if int(total_files) != args.expected_count:
            print(f"REFUSED: job declared {total_files} files, expected {args.expected_count}")
            return 1
        if Path(job_dir).resolve() != Path(args.expected_source_dir).resolve():
            print(f"REFUSED: job source is {job_dir!r}, expected {str(args.expected_source_dir)!r}")
            return 1
        rows = conn.execute(
            """
            SELECT f.path, s.id, s.raw_transcript, s.verified, COALESCE(s.human_decision,''),
                   COALESCE(s.reviewed_by,''), COALESCE(s.cloud_call,0), s.model_version_id,
                   (SELECT COUNT(*) FROM segment_hypotheses h WHERE h.segment_id=s.id),
                   (SELECT COUNT(*) FROM segment_hypotheses h
                     WHERE h.segment_id=s.id AND h.model_version_id=s.model_version_id
                       AND TRIM(h.transcript)=TRIM(s.raw_transcript)),
                   (SELECT COUNT(*) FROM review_events e WHERE e.segment_id=s.id)
              FROM import_job_files f
              LEFT JOIN speech_segments s ON s.audio_path=f.path
             WHERE f.job_id=?
             ORDER BY f.path, s.id
            """,
            (args.merge_import_job,),
        ).fetchall()
    finally:
        conn.close()

    if len(rows) != args.expected_count:
        print(f"REFUSED: journal-to-segment join produced {len(rows)} rows, expected {args.expected_count}")
        return 1
    paths = [row[0] for row in rows]
    selected = sorted(row[1] for row in rows if row[1])
    problems: list[str] = []
    if len(set(paths)) != args.expected_count:
        problems.append("journal paths are not one-to-one")
    if len(selected) != args.expected_count or len(set(selected)) != args.expected_count:
        problems.append("segment ids are missing or not one-to-one")
    if any(not Path(path).is_file() for path in paths):
        problems.append("one or more source WAVs are missing")
    for path, seg_id, raw, verified, human, reviewer, cloud, model, hyp_count, matching_hyp, events in rows:
        if not seg_id or not str(raw).strip():
            problems.append(f"missing segment/transcript for {path}")
            break
        if verified or human or reviewer or events:
            problems.append(f"segment {seg_id} has human/review state and is not a fresh campaign row")
            break
        if cloud or model != champion_id or hyp_count != 1 or matching_hyp != 1:
            problems.append(f"segment {seg_id} is not bound to exactly one matching local champion hypothesis")
            break
    selection_digest = ids_sha256(selected)
    if selection_digest.lower() != args.expected_selection_sha256.lower():
        problems.append(
            f"selection SHA-256 is {selection_digest}, expected {args.expected_selection_sha256.lower()}"
        )
    if set(existing) & set(selected):
        problems.append("import-job ids overlap the existing focus; expected a strictly additive campaign")
    if problems:
        for problem in problems:
            print(f"REFUSED: {problem}")
        return 1

    union = sorted(set(existing) | set(selected))
    union_digest = ids_sha256(union)
    print(f"current focus : {len(existing)} ids, file sha256 {actual_current}")
    print(f"import job    : {len(selected)} ids, selection sha256 {selection_digest}")
    print(f"union         : {len(union)} ids, selection sha256 {union_digest}")
    if args.dry_run:
        print("DRY RUN: validation passed; no focus or backup file was written")
        return 0

    # Compare-and-swap immediately before the only live-policy mutation. A queue tool or operator
    # changing the file while the 6,922-path validation ran must win; this command refuses stale intent.
    current_before_write = sha256_file(focus_path)
    if current_before_write.lower() != expected_current:
        print(f"REFUSED: current focus changed during validation: now {current_before_write}")
        return 1
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    backup = focus_path.with_name(f"voice_focus.pre-import-merge-{stamp}.json")
    suffix = 1
    while backup.exists():
        backup = focus_path.with_name(f"voice_focus.pre-import-merge-{stamp}-{suffix}.json")
        suffix += 1
    shutil.copy2(focus_path, backup)
    focus["segment_ids"] = union
    focus.setdefault("collection_merges", []).append(
        {
            "at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "label": args.label,
            "import_job_id": args.merge_import_job,
            "source_dir": str(Path(job_dir).resolve()),
            "added": len(selected),
            "selection_sha256": selection_digest,
            "prior_focus_file_sha256": actual_current,
            "result_segment_ids_sha256": union_digest,
        }
    )
    _atomic_write_json(focus_path, focus)
    written = json.loads(focus_path.read_text(encoding="utf-8"))
    if written.get("segment_ids") != union:
        # The preimage remains beside it. Fail loudly rather than claiming a policy we did not write.
        print(f"REFUSED: promoted focus did not reparse to the exact validated union; recover from {backup}")
        return 1
    print(f"ACTIVE: {focus_path}")
    print(f"  backup: {backup.name}")
    print(f"  added {len(selected)} ids for {args.label!r}; focus now contains {len(union)} ids")
    return 0


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
    parser.add_argument("--merge-import-job", help="atomically add a completed import job to the existing focus")
    parser.add_argument("--label", help="private collection label recorded in the data-dir focus history")
    parser.add_argument("--expected-current-sha256", help="CAS guard for the current voice_focus.json bytes")
    parser.add_argument("--expected-selection-sha256", help="SHA-256 of sorted imported segment ids, one per line")
    parser.add_argument("--expected-source-dir", type=Path, help="exact import source directory expected for the job")
    parser.add_argument("--expected-count", type=int, help="exact number of imported segment ids expected")
    parser.add_argument("--dry-run", action="store_true", help="validate and print the proposed merge without writing")
    args = parser.parse_args()

    # Every non-dry mode changes generation-coupled queue routing. Hold the same exclusive lock as
    # the desktop from the first validation read through atomic publication, so a named restore can
    # never pair one database generation with another generation's focus file.
    if not (args.merge_import_job and args.dry_run):
        with acquire_cortex_lock(args.data_dir):
            return run(args, parser)
    return run(args, parser)


def run(args: argparse.Namespace, parser: argparse.ArgumentParser) -> int:

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
    if args.merge_import_job:
        return merge_import_job(args, focus_path)
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
