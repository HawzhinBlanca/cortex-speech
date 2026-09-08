#!/usr/bin/env python3
"""Freeze a read-only re-review inventory; never apply it or alter review/pay authority.

The inventory is private operational evidence, NOT a deployable redo policy. Historical button
clicks and semantic decisions are different dimensions. Keep both and report unknowns explicitly.
"""

from __future__ import annotations

import argparse
from collections import Counter
from contextlib import closing
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import sqlite3


def digest(value: object) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False,
                                    separators=(",", ":")).encode("utf-8")).hexdigest()


def reviewer_key(value: str) -> str:
    # Match SQLite lower()/Rust ASCII case-insensitive reviewer identity, not Unicode casefold.
    return value.strip().translate(str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz"))


CORE_FIELDS = ("version", "pool", "reviewers", "rows", "confirmed_looks_good_segment_ids",
               "unproven_accept_segment_ids", "authority_sha256", "schema_sha256")


def verify_inventory(saved: dict, current: dict) -> None:
    """Read-only freshness check. Passing does not authorize or implement activation."""
    if saved.get("version") != 1 or saved.get("activation_allowed") is not False:
        raise ValueError("not a preparation-only inventory")
    try:
        core = {key: saved[key] for key in CORE_FIELDS}
    except KeyError as error:
        raise ValueError("inventory is incomplete") from error
    if digest(core) != saved.get("inventory_sha256"):
        raise ValueError("inventory digest mismatch")
    if saved["inventory_sha256"] != current["inventory_sha256"]:
        raise ValueError("inventory is stale: source identity, review evidence, or selection changed")


def collect(conn: sqlite3.Connection, reviewers: list[str]) -> dict:
    """Caller owns one read-only snapshot. Missing tables/columns fail, never silently truncate."""
    names = sorted({reviewer_key(name) for name in reviewers})
    if not names or any(not name for name in names):
        raise ValueError("at least one nonempty reviewer is required")
    registry = [dict(row) for row in conn.execute("SELECT * FROM review_pool_registry")]
    if len(registry) != 1:
        raise ValueError("exactly one active review pool is required")
    pool_id = registry[0]["pool_id"]
    members = {}
    for row in conn.execute("""
        SELECT m.*, s.review_revision, s.reviewed_by, s.human_decision, s.verified,
               s.verdict_transcript, s.annotated_transcript,
               x.canonical_segment_id AS excluded_in_favor_of
          FROM review_pool_members m JOIN speech_segments s ON s.id=m.segment_id
          LEFT JOIN review_pool_duplicate_exclusions x
            ON x.pool_id=m.pool_id AND x.segment_id=m.segment_id
         WHERE m.pool_id=? ORDER BY m.segment_id
    """, (pool_id,)):
        item = dict(row)
        if item["segment_id"] in members:
            raise ValueError("duplicate pool member identity")
        members[item["segment_id"]] = item
    if len(members) != registry[0]["focus_segment_count"]:
        raise ValueError("pool registry/member count mismatch")

    marks = ",".join("?" for _ in names)
    evidence = []
    for table, timestamp, source in (("review_events", "timestamp_ms", "canonical_history"),
                                     ("review_pool_decisions", "created_at_ms", "pool_history")):
        for row in conn.execute(
            f"SELECT * FROM {table} WHERE lower(trim(reviewer)) IN ({marks}) ORDER BY id", names
        ):
            row = dict(row)
            evidence.append({
                "source": source, "id": row["id"], "segment_id": row["segment_id"],
                "reviewer": reviewer_key(row["reviewer"]), "action": row["action"],
                "requested_action": row["requested_action"], "timestamp_ms": row[timestamp],
                "served_revision": row.get("served_revision"),
                "operation_id": row["operation_id"], "record_sha256": digest(row),
            })
    active_pool = {row[0] for row in conn.execute(
        "SELECT id FROM effective_review_pool_decisions_v62 WHERE pool_id=?", (pool_id,)
    )}
    for row in evidence:
        row["effective_pool_decision"] = row["source"] == "pool_history" and row["id"] in active_pool

    groups = {}
    for row in evidence:
        groups.setdefault((row["reviewer"], row["segment_id"]), []).append(row)
    # Canonical imports may have no surviving event provenance. Do not omit them or invent a click.
    for segment_id, member in members.items():
        name = reviewer_key(member["reviewed_by"] or "")
        if member["verified"] and name in names:
            groups.setdefault((name, segment_id), [])

    rows = []
    for (name, segment_id), history in sorted(groups.items()):
        member = members.get(segment_id)
        buttons = [r for r in history if r["requested_action"] == "accept"]
        unknown_accepts = [r for r in history if not r["requested_action"] and r["action"] == "accept"]
        semantic_accepts = [r for r in history if r["action"] == "accept"]
        current_own = bool(member and member["verified"] and reviewer_key(member["reviewed_by"] or "") == name)
        current_accept = current_own and member["human_decision"] in ("accept", "human_accept")
        current_edits = current_own and member["human_decision"] in ("edit", "human_edit")
        bound_current = [] if not current_own else [r for r in history
            if r["source"] == "canonical_history" and r["served_revision"] is not None
            and r["served_revision"] + 1 == member["review_revision"]
            and r["action"] == member["human_decision"].removeprefix("human_")
            and r["requested_action"] is not None]
        current_button = bound_current[0]["requested_action"] if len(bound_current) == 1 else None
        if buttons:
            batch = "looks_good_confirmed"
        elif unknown_accepts or (current_accept and current_button is None):
            batch = "accept_button_unproven"
        elif current_edits or any(r["action"] == "edit" for r in history):
            batch = "edits_later"
        else:
            batch = "other_history"
        retained = bool(member and not member["excluded_in_favor_of"])
        identity = None if not member else {
            "pool_id": pool_id, "segment_id": segment_id,
            "audio_content_hash": member["audio_content_hash"],
            "source_start_ms": member["source_start_ms"], "source_end_ms": member["source_end_ms"],
            "duration_ms": member["duration_ms"], "review_revision": member["review_revision"],
            "voice_name": member["voice_name"],
            "current_text_sha256": digest([member["verdict_transcript"], member["annotated_transcript"],
                                            member["raw_transcript"]]),
        }
        rows.append({
            "reviewer": name, "segment_id": segment_id, "batch": batch,
            "pool_state": "retained" if retained else ("duplicate_excluded" if member else "outside_pool"),
            "duplicate_target": member["excluded_in_favor_of"] if member else None,
            "identity": identity, "current_own_canonical": current_own,
            "current_canonical_action": member["human_decision"] if current_own else None,
            "current_canonical_requested_action": current_button,
            "confirmed_button_clicks": len(buttons), "semantic_accepts": len(semantic_accepts),
            "corrected_then_looks_good": sum(r["action"] == "edit" for r in buttons),
            "unproven_accept_clicks": len(unknown_accepts), "evidence": history,
        })
    by_reviewer = {}
    for name in names:
        owned = [r for r in rows if r["reviewer"] == name]
        kept = [r for r in owned if r["pool_state"] == "retained"]
        by_reviewer[name] = {
            "historical_distinct_clips": len(owned), "retained_distinct_clips": len(kept),
            "retained_batches": dict(Counter(r["batch"] for r in kept)),
            "retained_current_canonical_accepts": sum(r["current_canonical_action"] in ("accept", "human_accept") for r in kept),
            "retained_effective_pool_accepts": sum(e["effective_pool_decision"] and e["action"] == "accept" for r in kept for e in r["evidence"]),
            "retained_corrected_then_looks_good": sum(r["corrected_then_looks_good"] for r in kept),
            "excluded_duplicate_clips": sum(r["pool_state"] == "duplicate_excluded" for r in owned),
            "outside_pool_clips": sum(r["pool_state"] == "outside_pool" for r in owned),
        }
    primary = sorted({r["segment_id"] for r in rows if r["pool_state"] == "retained" and r["batch"] == "looks_good_confirmed"})
    uncertain = sorted({r["segment_id"] for r in rows if r["pool_state"] == "retained" and r["batch"] == "accept_button_unproven"} - set(primary))
    # Other reviewers can change whether a selected clip is resolved. Bind that evidence too,
    # without publishing their transcripts or expanding this inventory's reviewer scope.
    scoped_ids = json.dumps(sorted({r["segment_id"] for r in rows if r["pool_state"] == "retained"}))
    authority = {}
    for table in ("effective_review_pool_decisions_v62", "effective_independent_review_decisions_v61",
                  "review_pool_owner_adjudications"):
        authority[table] = [dict(row) for row in conn.execute(
            f"SELECT * FROM {table} WHERE segment_id IN (SELECT value FROM json_each(?)) ORDER BY id",
            (scoped_ids,),
        )]
    schema = [tuple(row) for row in conn.execute("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name")]
    core = {"version": 1, "pool": registry[0], "reviewers": names, "rows": rows,
            "confirmed_looks_good_segment_ids": primary, "unproven_accept_segment_ids": uncertain,
            "authority_sha256": digest(authority), "schema_sha256": digest(schema)}
    return {**core, "inventory_sha256": digest(core), "summary": by_reviewer,
            "read_only": True, "activation_allowed": False,
            "limitations": ["Preparation only: no review, pay, queue, export, or model authority is changed.",
                            "Historical Looks Good may precede a later correction; preserve current text.",
                            "Unknown button provenance is not a confirmed Looks Good click.",
                            "Excluded duplicate IDs are reported, never reintroduced or automatically remapped.",
                            "Audio hashes are database identity evidence; this tool does not listen to or rehash WAV files.",
                            "A current snapshot and revision-bound apply must revalidate this inventory before mutation."]}


def prepare(path: Path, reviewers: list[str]) -> dict:
    if not path.is_file():
        raise ValueError("database must already exist")
    with closing(sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True, timeout=5)) as conn:
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA query_only=ON")
        conn.execute("BEGIN")
        result = collect(conn, reviewers)
        result["observed_at"] = datetime.now(timezone.utc).isoformat()
        conn.rollback()
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", required=True, type=Path)
    parser.add_argument("--reviewer", action="append", required=True)
    destination = parser.add_mutually_exclusive_group(required=True)
    destination.add_argument("--output", type=Path, help="new private JSON file; never overwritten")
    destination.add_argument("--verify-against", type=Path, help="read-only check against a saved inventory")
    args = parser.parse_args()
    result = prepare(args.db, args.reviewer)
    if args.verify_against:
        with args.verify_against.open(encoding="utf-8") as stream:
            verify_inventory(json.load(stream), result)
        print(json.dumps({"fresh": True, "read_only": True, "activation_allowed": False,
                          "inventory_sha256": result["inventory_sha256"]}))
        return
    # Exclusive create: never overwrite a prior manifest, source database, or existing evidence.
    with args.output.open("x", encoding="utf-8") as stream:
        json.dump(result, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    print(json.dumps({"inventory_sha256": result["inventory_sha256"], "read_only": True,
                      "activation_allowed": False, "summary": result["summary"]}, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, sqlite3.Error) as error:
        raise SystemExit(f"REOPEN PREPARATION REFUSED: {error}") from error
