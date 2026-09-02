#!/usr/bin/env python3
"""Gate D in one command: snapshot -> train -> eval -> verdict, with zero manual steps.

Every stage here was proven by hand on 2026-08-18 (a challenger trained on 403 corrected labels beat
the champion, WER 33.97% -> 28.22%, paired MAPSSWE p=5.9e-15). What did not exist was a way to RUN
the chain — it took ~15 manual steps across two shells, which is precisely what gate D forbids.

Stages, each refusing to continue on a failure rather than degrading:

  1. snapshot  `export_pack`            -> sealed pack + snapshot id (headless; does NOT take the
                                           instance lock, so reviewers keep their links)
  2. train     `train_challenger.py`    -> hashed consumption evidence + deployment manifest
                                           (via omni7b_trainer_adapter.py)
  3. serve     `cortex_7b_server.py`    -> the challenger on its own port/GPU, verified deployment
  4. eval      `scorecard_7b.py` x2     -> champion + challenger over ONE eval manifest
  5. bind      `emit_scorecard_provenance.py` -> the sidecars the gate requires
  6. verdict   `promotion_gate.py`      -> PROMOTE / REJECT / INVALID

A REJECT is a PASS of gate D (docs/LOOP_TO_10.md): the loop proves the machinery, not that a
particular model won. What is NOT a pass is a stage that could not run — those are reported as
BLOCKED with the tool's own refusal text, never smoothed over.

Known blocker as of 2026-08-19: stage 6 cannot produce a verdict because `build_eval_slices.py`
refuses a manifest whose rows do not resolve to live library segments, and the frozen FLEURS set is
deliberately never imported. The library-resident alternative is 11 held-out clips across 4
recordings against a >=5-per-group floor. That resolves itself as gate B fills; it is not a tooling
gap, and this script reports it honestly rather than pretending the chain completed.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

APP = Path(__file__).resolve().parents[1]
REPO = APP.parent
SNAPSHOT_RE = re.compile(r"snapshot id\s*:\s*([0-9a-f]{64})")
MODEL_CARD = "soranivoice_omniASR_LLM_7B_v2_local"


class Stage:
    def __init__(self, name: str) -> None:
        self.name = name
        self.status = "not-run"
        self.detail = ""

    def ok(self, detail: str = "") -> "Stage":
        self.status, self.detail = "PASS", detail
        return self

    def blocked(self, detail: str) -> "Stage":
        self.status, self.detail = "BLOCKED", detail
        return self

    def failed(self, detail: str) -> "Stage":
        self.status, self.detail = "FAIL", detail
        return self


def run(cmd: list[str], *, cwd: Path | None = None, env: dict | None = None, timeout: int | None = None):
    return subprocess.run(cmd, cwd=cwd or APP, env=env, capture_output=True, text=True, timeout=timeout)


def find_exe(name: str) -> Path | None:
    for profile in ("release", "debug"):
        candidate = APP / "src-tauri" / "target" / profile / f"{name}.exe"
        if candidate.is_file():
            return candidate
        candidate = APP / "src-tauri" / "target" / profile / name
        if candidate.is_file():
            return candidate
    return None


def stage_snapshot(out_root: Path, log) -> tuple[Stage, dict]:
    stage = Stage("snapshot")
    exporter = find_exe("export_pack")
    if exporter is None:
        return stage.blocked("export_pack binary not built (cargo build --release --bin export_pack)"), {}
    pack_dir = out_root / "pack"
    log(f"[1/6] snapshot: {exporter.name} -> {pack_dir}")
    result = run([str(exporter), str(pack_dir)], timeout=3600)
    if result.returncode != 0:
        return stage.failed((result.stderr or result.stdout).strip()[-400:]), {}
    match = SNAPSHOT_RE.search(result.stdout)
    if not match:
        return stage.failed("export_pack printed no snapshot id"), {}
    snapshot = match.group(1)
    return stage.ok(f"snapshot {snapshot[:16]}… at {pack_dir}"), {"snapshot": snapshot, "pack": pack_dir}


def stage_train(ctx: dict, out_root: Path, args, log) -> Stage:
    stage = Stage("train")
    if not args.base or not args.base_sha256:
        return stage.blocked("--base/--base-sha256 not given; real training may not trust an unpinned base")
    if not os.environ.get("CORTEX_OMNI7B_TRAINER"):
        return stage.blocked("CORTEX_OMNI7B_TRAINER is unset; the external trainer's path is machine-specific")
    run_dir = out_root / "challenger"
    trainer_cmd = json.dumps([sys.executable, str(APP / "scripts" / "omni7b_trainer_adapter.py")])
    log(f"[2/6] train: {ctx['snapshot'][:16]}… -> {run_dir}")
    result = run(
        [
            sys.executable, str(APP / "scripts" / "train_challenger.py"),
            "--snapshot", ctx["snapshot"],
            "--pack", str(ctx["pack"]),
            "--out", str(run_dir),
            "--base", args.base,
            "--base-id", args.base_id,
            "--base-sha256", args.base_sha256,
            "--model-card", MODEL_CARD,
            "--trainer", trainer_cmd,
        ],
        timeout=args.train_timeout,
    )
    tail = (result.stdout or "").strip().splitlines()[-6:]
    if result.returncode != 0:
        return stage.failed("\n      ".join(tail) or (result.stderr or "").strip()[-400:])
    record = run_dir / "challenger_run.json"
    if not record.is_file():
        return stage.failed("no challenger_run.json produced")
    ctx["run_record"] = record
    ctx["run_dir"] = run_dir
    model_id = json.loads(record.read_text(encoding="utf-8")).get("model_id", "?")
    return stage.ok(f"trained {model_id}")


def stage_serve(ctx: dict, args, log) -> tuple[Stage, subprocess.Popen | None]:
    stage = Stage("serve")
    record = json.loads(ctx["run_record"].read_text(encoding="utf-8"))
    pointer = ctx["run_dir"] / "challenger_pointer.json"
    pointer.write_bytes(
        json.dumps(
            {
                "schema": 2,
                "champions": {
                    "omniasr-7b": {
                        "modelVersionId": record["model_id"],
                        "deploymentManifestPath": record["deployment_manifest"]["path"],
                        "deploymentSha256": record["deployment_manifest"]["sha256"],
                    }
                },
            },
            indent=2,
        ).encode("utf-8")
        + b"\n"
    )
    wsl_pointer = "/mnt/" + str(pointer).replace("\\", "/").replace(":", "", 1).lower()[0:1] + \
        str(pointer).replace("\\", "/")[2:]
    log(f"[3/6] serve: challenger on port {args.challenger_port}, GPU {args.challenger_gpu}")
    proc = subprocess.Popen(
        ["wsl", "-e", "bash", "-lc",
         f"CORTEX_7B_CHAMPION_POINTER={wsl_pointer} CORTEX_7B_PORT={args.challenger_port} "
         f"CORTEX_7B_DEVICES={args.challenger_gpu} exec {args.wsl_python} "
         f"/mnt/c{str(APP).replace(chr(92), '/')[2:]}/scripts/cortex_7b_server.py"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    deadline = time.time() + args.serve_timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            return stage.failed("challenger server exited during load"), None
        probe = run(["wsl", "-e", "bash", "-lc",
                     f"(echo > /dev/tcp/127.0.0.1/{args.challenger_port}) 2>/dev/null && echo UP || echo DOWN"])
        if "UP" in probe.stdout:
            return stage.ok(f"challenger serving on {args.challenger_port}"), proc
        time.sleep(10)
    return stage.failed("challenger server never came up"), proc


def stage_eval(ctx: dict, out_root: Path, args, log) -> Stage:
    stage = Stage("eval")
    manifest = Path(args.eval_manifest)
    if not manifest.is_file():
        return stage.blocked(f"eval manifest not found: {manifest}")
    scorecard = APP / "scripts" / "scorecard_7b.py"
    results = {}
    for role, port in (("champion", args.champion_port), ("challenger", args.challenger_port)):
        role_dir = out_root / "eval" / role
        role_dir.mkdir(parents=True, exist_ok=True)
        local = role_dir / manifest.name
        shutil.copyfile(manifest, local)  # identical bytes => one eval identity, separate outputs
        wsl_manifest = "/mnt/c" + str(local).replace("\\", "/")[2:]
        log(f"[4/6] eval {role}: port {port}")
        result = run(["wsl", "-e", "bash", "-lc",
                      f"CORTEX_7B_PORT={port} CORTEX_7B_WORKERS=1 {args.wsl_python} "
                      f"/mnt/c{str(scorecard).replace(chr(92), '/')[2:]} {wsl_manifest}"],
                     timeout=args.eval_timeout)
        produced = role_dir / "omni7b_results.tsv"
        if result.returncode != 0 or not produced.is_file():
            return stage.failed(f"{role} scorecard failed: {(result.stdout or '')[-300:]}")
        results[role] = produced
    ctx["scorecards"] = results
    ctx["eval_manifest"] = manifest
    return stage.ok(f"{len(results)} scorecards over {manifest.name}")


def stage_bind(ctx: dict, log) -> Stage:
    stage = Stage("bind")
    emitter = APP / "scripts" / "emit_scorecard_provenance.py"
    log("[5/6] bind: emitting provenance sidecars")
    for role, scorecard in ctx["scorecards"].items():
        cmd = [sys.executable, str(emitter), "--scorecard", str(scorecard), "--role", role,
               "--challenger-run-record", str(ctx["run_record"]),
               "--eval-manifest", str(ctx["eval_manifest"])]
        if role == "champion":
            if not ctx.get("incumbent_manifest"):
                return stage.blocked("champion sidecar needs --incumbent-manifest (bootstrap_legacy_champion.py)")
            cmd += ["--deployment", str(ctx["incumbent_manifest"])]
        result = run(cmd, timeout=3600)
        if result.returncode != 0:
            return stage.failed(f"{role} sidecar: {(result.stderr or result.stdout)[-300:]}")
    return stage.ok("champion + challenger sidecars bound")


def stage_verdict(ctx: dict, out_root: Path, log) -> Stage:
    stage = Stage("verdict")
    slices = out_root / "eval" / "eval_slices.tsv"
    log("[6/6] verdict: building protected slices")
    built = run([sys.executable, str(APP / "scripts" / "build_eval_slices.py"),
                 str(ctx["eval_manifest"]), "--out", str(slices)], timeout=3600)
    if built.returncode != 0 or not slices.is_file():
        refusal = ((built.stdout or "") + (built.stderr or "")).strip().splitlines()
        return stage.blocked(refusal[-1][:300] if refusal else "build_eval_slices produced no slices")
    verdict_path = out_root / "verdict.json"
    result = run([sys.executable, str(APP / "scripts" / "promotion_gate.py"),
                  str(ctx["scorecards"]["champion"]), str(ctx["scorecards"]["challenger"]),
                  "--snapshot-id", ctx["snapshot"], "--eval-manifest", str(ctx["eval_manifest"]),
                  "--slices", str(slices), "--challenger-run-record", str(ctx["run_record"]),
                  "--out", str(verdict_path)], timeout=3600)
    if verdict_path.is_file():
        verdict = json.loads(verdict_path.read_text(encoding="utf-8")).get("verdict", "?")
        # exit 1 == REJECT, which docs/LOOP_TO_10.md calls a PASS of gate D.
        if verdict in {"PROMOTE", "REJECT"}:
            return stage.ok(f"verdict {verdict}")
        return stage.blocked(f"verdict {verdict} (evidence incomplete)")
    return stage.failed(f"promotion_gate produced no verdict: {(result.stdout or '')[-300:]}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=None, help="cycle directory (default runs/cycle_<n>)")
    parser.add_argument("--base", default=os.environ.get("CORTEX_CHALLENGER_BASE", ""))
    parser.add_argument("--base-id", default=os.environ.get("CORTEX_CHALLENGER_BASE_ID", "omniasr-llm-7b-v2-local"))
    parser.add_argument("--base-sha256", default=os.environ.get("CORTEX_CHALLENGER_BASE_SHA256", ""))
    parser.add_argument("--eval-manifest", default=str(REPO / "runs" / "eval" / "fleurs_ckb_iq_frozen.eval.tsv"))
    parser.add_argument("--incumbent-manifest", default="")
    parser.add_argument("--champion-port", type=int, default=8799)
    parser.add_argument("--challenger-port", type=int, default=8798)
    parser.add_argument("--challenger-gpu", default="1")
    parser.add_argument("--wsl-python", default="/home/ai/.venv-wsl-whisper/bin/python")
    parser.add_argument("--train-timeout", type=int, default=7200)
    parser.add_argument("--serve-timeout", type=int, default=1800)
    parser.add_argument("--eval-timeout", type=int, default=7200)
    parser.add_argument("--stop-after", choices=["snapshot", "train", "serve", "eval", "bind", "verdict"],
                        default="verdict")
    args = parser.parse_args()

    out_root = args.out or (REPO / "runs" / f"cycle_{int(os.path.getmtime(APP)) % 100000}")
    out_root.mkdir(parents=True, exist_ok=True)

    def log(message: str) -> None:
        print(message, flush=True)

    log(f"CHALLENGER CYCLE -> {out_root}")
    stages: list[Stage] = []
    ctx: dict = {"incumbent_manifest": args.incumbent_manifest or None}
    server = None
    order = ["snapshot", "train", "serve", "eval", "bind", "verdict"]
    stop_at = order.index(args.stop_after)

    try:
        stage, produced = stage_snapshot(out_root, log)
        stages.append(stage)
        ctx.update(produced)
        if stage.status == "PASS" and stop_at >= 1:
            stages.append(stage_train(ctx, out_root, args, log))
            if stages[-1].status == "PASS" and stop_at >= 2:
                stage, server = stage_serve(ctx, args, log)
                stages.append(stage)
                if stage.status == "PASS" and stop_at >= 3:
                    stages.append(stage_eval(ctx, out_root, args, log))
                    if stages[-1].status == "PASS" and stop_at >= 4:
                        stages.append(stage_bind(ctx, log))
                        if stages[-1].status == "PASS" and stop_at >= 5:
                            stages.append(stage_verdict(ctx, out_root, log))
    finally:
        if server is not None and server.poll() is None:
            server.send_signal(signal.SIGTERM)
            try:
                server.wait(timeout=60)
            except subprocess.TimeoutExpired:
                server.kill()
            log("      challenger server stopped")

    print()
    print("=" * 62)
    for stage in stages:
        print(f"  {stage.status:8s} {stage.name:9s} {stage.detail}")
    print("=" * 62)
    reached = {stage.name for stage in stages if stage.status == "PASS"}
    if "verdict" in reached:
        print("GATE D: the loop completed end to end.")
        return 0
    blocked = [stage for stage in stages if stage.status == "BLOCKED"]
    failed = [stage for stage in stages if stage.status == "FAIL"]
    if failed:
        print(f"GATE D: INCOMPLETE — {failed[0].name} failed. A stage that did not run is never a pass.")
        return 1
    if blocked:
        print(f"GATE D: INCOMPLETE — blocked at {blocked[0].name}. Reported, not smoothed over.")
        return 2
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
