#!/usr/bin/env python3
"""Rehearse exact Looks Good reopening on a newly owned SQLite clone, never on the source.

Artifacts are private owner evidence. No cleanup, model calls, audio changes, HTTP submissions,
reviewer config copies, or source writes are performed. A pass is NOT deployment authorization.
"""
from __future__ import annotations

import argparse
from contextlib import closing
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import sqlite3
import subprocess

import prepare_review_reopen as inventory


def write_json(path: Path, value: object) -> None:
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")


def read_only(path: Path) -> sqlite3.Connection:
    conn = sqlite3.connect(path.resolve(strict=True).as_uri() + "?mode=ro", uri=True, timeout=5)
    conn.execute("PRAGMA query_only=ON")
    conn.execute("BEGIN DEFERRED")
    return conn


def copy_snapshot(source: Path, destination: Path) -> int:
    if destination.exists():
        raise ValueError("clone destination already exists")
    with closing(read_only(source)) as src, closing(sqlite3.connect(destination)) as dst:
        version = src.execute("SELECT MAX(version) FROM schema_migrations").fetchone()[0]
        src.backup(dst, pages=4096, sleep=0.001)
        src.rollback()
    return version


def rows_digest(rows) -> str:
    digest = hashlib.sha256()
    for row in rows:
        encoded = json.dumps(row, ensure_ascii=False, separators=(",", ":"), default=repr).encode("utf-8")
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
    return digest.hexdigest()


def preserved_state(database: Path) -> dict:
    with closing(read_only(database)) as conn:
        tables = ("review_events", "review_pool_decisions", "review_pool_reversals",
                  "independent_review_decisions", "independent_review_reversals",
                  "review_pool_owner_adjudications", "review_compensation_ledger")
        result = {table: rows_digest(conn.execute(f"SELECT * FROM {table} ORDER BY id")) for table in tables}
        result["canonical_and_audio"] = rows_digest(conn.execute(
            "SELECT id,raw_transcript,annotated_transcript,verdict_transcript,human_decision,verified,reviewed_by,"
            "audio_path,audio_content_hash,alignment_json,model_version_id,duration_ms FROM speech_segments ORDER BY id"
        ))
        return result


def run_admin(executable: Path, output: Path, name: str, *args: str) -> dict:
    process = subprocess.run([str(executable), *args], capture_output=True, text=True,
                             encoding="utf-8", errors="strict", timeout=600, check=False)
    with (output / f"{name}.stderr.log").open("x", encoding="utf-8") as stream:
        stream.write(process.stderr)
    if process.returncode:
        raise RuntimeError(f"{name} failed (exit {process.returncode}): {process.stderr[-1500:]}")
    value = json.loads(process.stdout)
    write_json(output / f"{name}.json", value)
    return value


def rehearse(source: Path, executable: Path, output: Path, reviewers: list[str]) -> dict:
    source = source.resolve(strict=True)
    executable = executable.resolve(strict=True)
    output.mkdir(parents=True, exist_ok=False)
    output = output.resolve(strict=True)
    clone = output / "cortex-speech.db"
    version = copy_snapshot(source, clone)
    migration = run_admin(executable, output, "migration", "migrate", "--db", str(clone))
    if migration["afterSchemaVersion"] != 71:
        raise RuntimeError("candidate did not migrate its owned clone to schema 71")
    preview = inventory.prepare(clone, reviewers)
    write_json(output / "inventory.json", preview)
    ids = sorted({row["segment_id"] for row in preview["rows"]
                  if row["pool_state"] == "retained" and row["batch"] == "looks_good_confirmed"})
    if not ids:
        raise RuntimeError("no confirmed retained Looks Good targets; refusing an empty success")
    write_json(output / "selected-ids.json", ids)
    baseline = preserved_state(clone)
    plan = run_admin(executable, output, "plan", "plan-reopen", "--db", str(clone),
                     "--segment-list", str(output / "selected-ids.json"), "--priority", "0", "--reason",
                     "Owner disputes historical Looks Good work; require fresh independent verification")
    if [item["segmentId"] for item in plan["items"]] != ids:
        raise RuntimeError("candidate plan does not match exact selected clip IDs")
    applied = run_admin(executable, output, "apply", "apply-reopen", "--db", str(clone),
                        "--manifest", str(output / "plan.json"), "--confirm-quality-hold")
    if applied["reopenedOrAlreadyApplied"] != len(ids) or applied["paymentHistoryChanged"] is not False:
        raise RuntimeError("unexpected apply accounting or membership result")
    if not applied.get("preReopenPinnedSnapshot"):
        raise RuntimeError("no certified pre-reopen pinned snapshot was reported")
    if preserved_state(clone) != baseline:
        raise RuntimeError("reopening changed historical decisions, pay, canonical text or audio")
    retry = run_admin(executable, output, "retry", "apply-reopen", "--db", str(clone),
                      "--manifest", str(output / "plan.json"), "--confirm-quality-hold")
    if retry["reopenedOrAlreadyApplied"] != len(ids) or preserved_state(clone) != baseline:
        raise RuntimeError("exact retry changed history or membership")
    with closing(read_only(clone)) as conn:
        selected_json = json.dumps(ids)
        fresh_effective = conn.execute("SELECT COUNT(*) FROM effective_review_pool_decisions_v62 "
                                       "WHERE segment_id IN (SELECT value FROM json_each(?))", [selected_json]).fetchone()[0]
        revisions = dict(conn.execute("SELECT id,review_revision FROM speech_segments "
                                      "WHERE id IN (SELECT value FROM json_each(?))", [selected_json]))
        if fresh_effective != 0 or any(revisions[i["segmentId"]] != i["revision"] + 1 for i in plan["items"]):
            raise RuntimeError("prior pool authority survived or a retry advanced revisions again")
    probes = {}
    for index, reviewer in enumerate(reviewers):
        probe = run_admin(executable, output, f"reviewer-{index}", "probe", "--db", str(clone), "--reviewer", reviewer)
        if probe.get("passes") is not True or probe["sampleSegmentId"] not in ids or not probe.get("sharedReopenRounds"):
            raise RuntimeError(f"shared queue/audio probe failed for {reviewer}")
        probes[reviewer] = {key: probe[key] for key in ("availableClips", "sampleAudioValidWav", "passes")}
    certification = run_admin(executable, output, "certification", "certify", "--db", str(clone), "--full-integrity")
    if certification["database"]["healthy"] is not True or certification["audio"]["allAvailable"] is not True:
        raise RuntimeError("clone database or audio certification failed")
    result = {"passed": True, "sourceAccess": "SQLite read-only consistent snapshot", "sourceSchema": version,
              "cloneSchema": 71, "selectedClips": len(ids), "inventorySha256": preview["inventory_sha256"],
              "planSha256": plan["planSha256"], "historyAndPayUnchanged": True, "exactRetryPassed": True,
              "reviewerProbes": probes, "poolAdminSha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
              "observedAt": datetime.now(timezone.utc).isoformat(), "certifiesProduction": False,
              "limitations": ["Clone-only: no live activation or reviewer credentials were used.",
                              "Valid sample WAV bytes are not a human listening or phone-speaker test.",
                              "Historical exports remain archived artifacts, not renewed trust.",
                              "Unknown-button and corrected-later groups were not reopened."]}
    write_json(output / "result.json", result)
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-db", required=True, type=Path)
    parser.add_argument("--pool-admin", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--reviewer", action="append", required=True)
    args = parser.parse_args()
    result = rehearse(args.source_db, args.pool_admin, args.output_dir, args.reviewer)
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
