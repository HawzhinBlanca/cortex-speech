#!/usr/bin/env python3
"""Dump ONE engine's per-clip transcripts for a manifest, in the shared hypothesis-TSV format.

    wsl python3 cortex-speech-app/scripts/engine_hypotheses_dump.py <manifest.tsv> <out.tsv> [workers]

Output rows are `<wav_path>\t<reference>\t<hypothesis>` — byte-compatible with what
`gemini_asr_bench.rs` writes, so ONE scorer (`scorecard_gemini.py`, which imports the champion's own
normalization and bootstrap) scores every engine, and the fusion experiment can consume all engines
through one reader. Two formats or two metrics is how a model comparison starts lying.

WHY A SEPARATE SCRIPT. `scorecard_7b.py` writes the historical duplication-weighted champion archive and only
per-clip DISTANCES (`omni7b_results.tsv`), never the text — correct for its job, useless for fusion,
and it is pinned by `test_scorecard_cer_consistency.py`, so widening it to also emit text would edit
a gated artefact for a reason unrelated to its gate. This reads the same warm socket with the same
protocol and writes the text instead; it computes no metric of its own.

Resumable: rows already in <out.tsv> are skipped, so an interrupted run continues. Failures are
reported and NOT written — a missing hypothesis must stay missing rather than become an empty string
that a scorer would count as a 100%-error transcription.
"""
import json
import os
import socket
import sys
from concurrent.futures import ThreadPoolExecutor

HOST = os.environ.get("CORTEX_7B_HOST", "127.0.0.1")
PORT = int(os.environ.get("CORTEX_7B_PORT", "8799"))


def transcribe(audio_path: str, timeout=300.0) -> str:
    """One request/reply against the warm server. Raises on error so nothing is fabricated."""
    with socket.create_connection((HOST, PORT), timeout=timeout) as sock:
        sock.sendall((json.dumps({"audio_path": audio_path}) + "\n").encode("utf-8"))
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = sock.recv(65536)
            if not chunk:
                break
            buf += chunk
    reply = json.loads(buf.decode("utf-8").strip())
    if "error" in reply:
        raise RuntimeError(reply["error"])
    text = reply.get("transcript", "")
    if not text.strip():
        raise RuntimeError("empty transcript")
    return text


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    manifest, out_path = sys.argv[1], sys.argv[2]
    workers = int(sys.argv[3]) if len(sys.argv) > 3 else int(os.environ.get("CORTEX_7B_WORKERS", "2"))

    done = set()
    if os.path.exists(out_path):
        with open(out_path, encoding="utf-8") as fh:
            for line in fh:
                done.add(line.split("\t")[0])

    rows = []
    with open(manifest, encoding="utf-8") as fh:
        for line in fh:
            parts = line.rstrip("\n").split("\t")
            if len(parts) >= 2 and parts[0] and parts[1].strip() and parts[0] not in done:
                rows.append((parts[0], parts[1]))

    print(f"manifest : {manifest} ({len(rows)} to do, {len(done)} already done)")
    print(f"workers  : {workers}")

    ok = failed = 0
    with open(out_path, "a", encoding="utf-8", newline="") as out:
        with ThreadPoolExecutor(max_workers=workers) as pool:
            for (path, ref), result in zip(rows, pool.map(lambda r: _safe(r[0]), rows)):
                text, err = result
                if err:
                    print(f"  FAILED {path}: {err}", flush=True)
                    failed += 1
                    continue
                out.write(f"{path}\t{ref}\t" + text.replace("\t", " ").replace("\n", " ") + "\n")
                out.flush()
                ok += 1
                if ok % 50 == 0:
                    print(f"  {ok}/{len(rows)}", flush=True)

    print(f"\ndone: {ok} transcribed, {failed} failed -> {out_path}")
    if failed:
        print(f"WARNING: {failed} clips have NO hypothesis — score only the rows present and report n honestly.")
    return 0


def _safe(path: str):
    try:
        return transcribe(path), None
    except Exception as exc:  # noqa: BLE001 - the failure is reported per clip, never swallowed
        return "", str(exc)


if __name__ == "__main__":
    sys.exit(main())
