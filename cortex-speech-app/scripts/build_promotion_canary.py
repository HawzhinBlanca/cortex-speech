#!/usr/bin/env python3
"""Build the canonical promotion canary suite gate E requires.

`champion_promotion_runtime::run_canary` will not promote anything without one: it loads
`<data_dir>/promotion_canaries/<suite_id>.json`, refuses bytes that are not canonical, refuses a
suite whose sha256 does not match the `CanaryIdentity` it was handed, re-hashes every case's SOURCE
AUDIO and ALIGNMENT to catch input drift, then transcribes each case through the real client and
requires a non-empty transcript carrying the exact model and deployment identity.

Nothing created that file, so the saga could never be started with real inputs. This does, and only
from material that can actually satisfy the runtime's own checks:

  * the segment must be verified and not human-rejected — a canary is a known-good clip, so a
    rejected or unreviewed one would make a promotion decision on material nobody vouched for;
  * its source recording must EXIST on disk, because `run_canary` hashes that file;
  * its `alignment_json` must be non-empty, because the alignment hash is part of the drift check;
  * cases are drawn from DISTINCT recordings, so a promotion is not blessed by eight clips of one
    voice in one acoustic condition.

The suite is written as canonical JSON (recursively sorted keys, compact separators, one trailing
newline) byte-for-byte matching `canonical_json` + `serde_json::to_vec` on the Rust side; the printed
sha256 is what a `CanaryIdentity` must carry. Read-only against the library.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sqlite3
import sys
from pathlib import Path

CANARY_SCHEMA = 1
MAX_CANARY_CASES = 8


def data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    return Path(appdata) / "cortex-speech" if appdata else Path.home() / ".local/share/cortex-speech"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def canonical_bytes(value: object) -> bytes:
    """Byte-identical to Rust `serde_json::to_vec(canonical_json(v))` + b'\\n'."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8") + b"\n"


def pick_cases(db: Path, limit: int) -> list[dict[str, str]]:
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        rows = con.execute(
            """
            SELECT id, audio_path, alignment_json
              FROM speech_segments
             WHERE verified = 1
               AND COALESCE(human_decision,'') NOT IN ('reject','human_reject')
               AND COALESCE(verdict,'') != 'human_reject'
               AND TRIM(COALESCE(alignment_json,'')) != ''
               AND TRIM(COALESCE(annotated_transcript, verdict_transcript, normalized_transcript, raw_transcript,'')) != ''
             ORDER BY id ASC
            """
        ).fetchall()
    finally:
        con.close()

    cases: list[dict[str, str]] = []
    used_sources: set[str] = set()
    hashed: dict[str, str] = {}
    skipped_missing = 0
    for segment_id, audio_path, alignment_json in rows:
        if len(cases) >= limit:
            break
        if audio_path in used_sources:
            continue  # one case per recording — see module docstring
        source = Path(audio_path)
        if not source.is_file():
            skipped_missing += 1
            continue
        if audio_path not in hashed:
            hashed[audio_path] = sha256_file(source)
        cases.append(
            {
                "segmentId": segment_id,
                "sourceAudioSha256": hashed[audio_path],
                "alignmentSha256": hashlib.sha256(alignment_json.encode("utf-8")).hexdigest(),
            }
        )
        used_sources.add(audio_path)
    if skipped_missing:
        print(f"  note: skipped {skipped_missing} candidate(s) whose source recording is gone", flush=True)
    return cases


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite-id", default="incumbent-canary-v1", help="identifier; also the filename stem")
    parser.add_argument("--cases", type=int, default=MAX_CANARY_CASES)
    parser.add_argument("--db", type=Path, default=None)
    parser.add_argument("--out-dir", type=Path, default=None)
    args = parser.parse_args()

    if not 1 <= args.cases <= MAX_CANARY_CASES:
        raise SystemExit(f"--cases must be 1..{MAX_CANARY_CASES} (the runtime's own bound)")
    if not args.suite_id.replace("-", "").replace("_", "").isalnum():
        raise SystemExit("--suite-id must be an identifier (validate_identifier on the Rust side)")

    db = args.db or (data_dir() / "cortex-speech.db")
    if not db.is_file():
        raise SystemExit(f"library not found: {db}")

    cases = pick_cases(db, args.cases)
    if not cases:
        raise SystemExit(
            "no eligible canary case: need verified, non-rejected clips with a non-empty alignment "
            "whose source recording still exists on disk"
        )

    suite = {"schema": CANARY_SCHEMA, "suiteId": args.suite_id, "cases": cases}
    payload = canonical_bytes(suite)
    out_dir = args.out_dir or (data_dir() / "promotion_canaries")
    out_dir.mkdir(parents=True, exist_ok=True)
    out = out_dir / f"{args.suite_id}.json"
    staging = out.with_name(f".{out.name}.tmp-{os.getpid()}")
    staging.write_bytes(payload)
    os.replace(staging, out)

    print(f"wrote {len(cases)} case(s) -> {out}")
    print(f"suiteId      = {args.suite_id}")
    print(f"suiteSha256  = {hashlib.sha256(payload).hexdigest()}")
    print("  (a CanaryIdentity must carry exactly this suiteSha256)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
