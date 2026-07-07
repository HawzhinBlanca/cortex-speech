#!/usr/bin/env python3
"""One-command 10/10 accuracy harness: run the real scorecards on a gold manifest and record the
REAL measured numbers (never fabricated) into docs/MEASUREMENTS.md with the exact command + SHAs.

This orchestrates the existing per-engine scorecards so the owner runs ONE command on the 4090 box
instead of remembering each invocation, and so every number lands in the ledger the way the honesty
rule requires: a real run of the real harness, stamped with the git SHA, the manifest SHA-256 + row
count, and the exact command line. It does NOT invent, estimate, or round anything -- every CER/WER/CI
it writes is parsed verbatim from a scorecard's own stdout. If an engine's prerequisites are missing
or a run fails, it records the FAILURE (and why), never a placeholder number.

Engines:
  * 7b        -> scorecard_7b.py         (OmniASR-7B champion; MUST run inside WSL with the warm
                                          cortex_7b_server.py listening on 127.0.0.1:8799)
  * finetuned -> scorecard_finetuned.py  (fine-tuned MMS-CTC-1B ONNX; needs CORTEX_FINETUNED_MODEL +
                                          CORTEX_FINETUNED_ONNX env pointing at the HF dir + model.onnx)

Usage:
  # build the gold corpus first if you don't have a manifest (see build_ckb_gold.py):
  python scripts/build_ckb_gold.py 900               # -> a <gold>.tsv manifest
  # then, inside WSL (so the 7B server + ONNX are both reachable):
  python scripts/run_measurements.py <gold_manifest.tsv> [--engines 7b,finetuned] [--bootstrap 3000]

Manifest rows: <wav_path>\t<reference>[\t<gender>\t<age>]  (WSL-visible paths, e.g. /mnt/c/...).
Nothing here talks to the network except the scorecards themselves (local 7B socket / local ONNX).
"""
from __future__ import annotations

import argparse
import hashlib
import re
import socket
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SCRIPTS_DIR = REPO_ROOT / "scripts"
RESULTS_DOC = REPO_ROOT / "docs" / "MEASUREMENTS.md"

# Parses the scorecards' own headline lines, e.g.
#   "  FINE-TUNED micro CER = 21.00%   95% CI [19.93%, 22.04%]   N=900"
#   "  OmniASR-7B (warm server) micro WER = 12.34%   95% CI [11.1%, 13.5%]   N=900"
_HEADLINE = re.compile(
    r"micro\s+(?P<metric>CER|WER)\s*=\s*(?P<val>[\d.]+)%\s*"
    r"95%\s*CI\s*\[(?P<lo>[\d.]+)%,\s*(?P<hi>[\d.]+)%\]\s*N=(?P<n>\d+)"
)


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def git_sha() -> str:
    try:
        out = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True, check=True
        )
        return out.stdout.strip()
    except Exception as exc:  # pragma: no cover - git always present in a checkout
        return f"(git sha unavailable: {exc})"


def server_reachable(host: str = "127.0.0.1", port: int = 8799, timeout: float = 2.0) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def parse_headlines(stdout: str) -> dict[str, dict]:
    """Extract every 'micro CER/WER = ... CI [...] N=...' line the scorecard printed. Returns {} if a
    run produced no parseable headline -- which the caller records as a FAILURE, never as a number."""
    found: dict[str, dict] = {}
    for m in _HEADLINE.finditer(stdout):
        found[m.group("metric")] = {
            "value": float(m.group("val")),
            "ci_lo": float(m.group("lo")),
            "ci_hi": float(m.group("hi")),
            "n": int(m.group("n")),
        }
    return found


def run_engine(engine: str, manifest: Path, bootstrap: int) -> dict:
    """Run one scorecard, streaming its output to the console and capturing it. Returns a result dict
    with the exact command, return code, parsed metrics, and the full captured output."""
    if engine == "7b":
        script = SCRIPTS_DIR / "scorecard_7b.py"
        # Prereq: the warm 7B server must be listening, or the scorecard would (slowly) fail / stall.
        if not server_reachable():
            return {
                "engine": engine,
                "status": "SKIPPED",
                "reason": "cortex_7b_server.py not reachable on 127.0.0.1:8799 -- start it in WSL "
                "(/root/cortex_env) before running the 7B scorecard.",
                "command": None,
            }
        cmd = [sys.executable, str(script), str(manifest), str(bootstrap)]
    elif engine == "finetuned":
        script = SCRIPTS_DIR / "scorecard_finetuned.py"
        import os

        missing = [v for v in ("CORTEX_FINETUNED_MODEL", "CORTEX_FINETUNED_ONNX") if not os.environ.get(v)]
        if missing:
            return {
                "engine": engine,
                "status": "SKIPPED",
                "reason": f"missing env {', '.join(missing)} (point them at the HF dir + model.onnx).",
                "command": None,
            }
        cmd = [sys.executable, str(script), str(manifest), str(bootstrap)]
    else:
        return {"engine": engine, "status": "SKIPPED", "reason": f"unknown engine {engine!r}", "command": None}

    print(f"\n>>> [{engine}] {' '.join(cmd)}\n", flush=True)
    # Stream + capture: run with the child's stdout inherited-and-teed so the owner sees progress AND
    # we keep the text to parse. subprocess.run with capture then echo is simplest and lossless.
    proc = subprocess.run(cmd, cwd=REPO_ROOT, capture_output=True, text=True)
    sys.stdout.write(proc.stdout)
    if proc.stderr:
        sys.stderr.write(proc.stderr)
    metrics = parse_headlines(proc.stdout)
    status = "OK" if (proc.returncode == 0 and metrics) else "FAILED"
    reason = None
    if proc.returncode != 0:
        reason = f"scorecard exited {proc.returncode}"
    elif not metrics:
        reason = "no parseable 'micro CER/WER = ...' headline in output"
    return {
        "engine": engine,
        "status": status,
        "reason": reason,
        "command": " ".join(cmd),
        "returncode": proc.returncode,
        "metrics": metrics,
        "output": proc.stdout,
    }


def render_block(results: list[dict], manifest: Path, manifest_sha: str, rows: int, sha: str, stamp: str) -> str:
    lines: list[str] = []
    lines.append(f"\n## Measurement run {stamp}\n")
    lines.append(f"- git SHA: `{sha}`")
    lines.append(f"- gold manifest: `{manifest}`  ({rows} rows, sha256 `{manifest_sha}`)")
    lines.append("")
    lines.append("| engine | metric | value | 95% CI | N | status |")
    lines.append("|---|---|---|---|---|---|")
    for r in results:
        if r["status"] != "OK":
            lines.append(f"| {r['engine']} | -- | -- | -- | -- | **{r['status']}**: {r.get('reason','')} |")
            continue
        for metric, m in r["metrics"].items():
            lines.append(
                f"| {r['engine']} | {metric} | {m['value']:.2f}% | "
                f"[{m['ci_lo']:.2f}%, {m['ci_hi']:.2f}%] | {m['n']} | OK |"
            )
    lines.append("")
    for r in results:
        if r.get("command"):
            lines.append(f"<details><summary>{r['engine']} command + full output</summary>\n")
            lines.append(f"```\n$ {r['command']}\n{r.get('output','')}\n```")
            lines.append("</details>\n")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description="Run the real accuracy scorecards and record the numbers.")
    ap.add_argument("manifest", help="gold manifest .tsv (WSL-visible wav paths)")
    ap.add_argument("--engines", default="7b,finetuned", help="comma list: 7b,finetuned")
    ap.add_argument("--bootstrap", type=int, default=3000, help="bootstrap resamples for the CI")
    ap.add_argument("--stamp", default=None, help="ISO timestamp for the record (default: ask git/OS)")
    args = ap.parse_args()

    manifest = Path(args.manifest).expanduser().resolve()
    if not manifest.is_file():
        print(f"ERROR: gold manifest not found: {manifest}", file=sys.stderr)
        print("Build one first, e.g.:  python scripts/build_ckb_gold.py 900", file=sys.stderr)
        return 2
    rows = sum(1 for line in manifest.read_text(encoding="utf-8").splitlines() if line.strip())
    if rows == 0:
        print(f"ERROR: gold manifest is empty: {manifest}", file=sys.stderr)
        return 2

    manifest_sha = sha256_of(manifest)
    sha = git_sha()
    # Timestamps must not be fabricated in-script; take one from the OS at run time on the owner's box.
    if args.stamp:
        stamp = args.stamp
    else:
        stamp = subprocess.run(
            ["git", "log", "-1", "--format=%cI"], cwd=REPO_ROOT, capture_output=True, text=True
        ).stdout.strip() or "(commit time)"
        # Prefer an actual wall-clock run time when available.
        try:
            import datetime

            stamp = datetime.datetime.now().astimezone().isoformat(timespec="seconds")
        except Exception:
            pass

    engines = [e.strip() for e in args.engines.split(",") if e.strip()]
    results = [run_engine(e, manifest, args.bootstrap) for e in engines]

    block = render_block(results, manifest, manifest_sha, rows, sha, stamp)
    RESULTS_DOC.parent.mkdir(parents=True, exist_ok=True)
    if not RESULTS_DOC.exists():
        RESULTS_DOC.write_text(
            "# Cortex accuracy measurements\n\n"
            "Real, harness-produced numbers only (see scripts/run_measurements.py). Each block is a\n"
            "single run stamped with the git SHA + gold-manifest SHA-256, and embeds the exact command\n"
            "and the scorecard's full output. Nothing here is estimated or rounded by hand.\n",
            encoding="utf-8",
        )
    with RESULTS_DOC.open("a", encoding="utf-8") as fh:
        fh.write(block)

    print("\n" + "=" * 60)
    print(f"Recorded {sum(1 for r in results if r['status']=='OK')}/{len(results)} engine(s) to {RESULTS_DOC}")
    for r in results:
        tag = r["status"]
        print(f"  {r['engine']:10} -> {tag}" + (f"  ({r['reason']})" if r.get("reason") else ""))
    print("=" * 60)
    # A skipped/failed engine is a real outcome, not a hard error of the harness itself; but signal
    # non-zero if NOTHING succeeded so a CI/cron wrapper notices.
    return 0 if any(r["status"] == "OK" for r in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
