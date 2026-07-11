#!/usr/bin/env python3
"""Honest wall-clock throughput benchmark for the warm cortex_7b_server.py replica pool.

Fires N real transcription requests at the running server (127.0.0.1:8799) at each requested
client concurrency and reports the measured wall time — nothing simulated, nothing extrapolated.
Run INSIDE WSL (the server binds inside WSL's network namespace):

    wsl python3 cortex-speech-app/scripts/bench_7b_throughput.py /mnt/c/.../clip.wav [N] [conc,conc,...]

Defaults: N=8 requests, concurrency sweep "1,2". Client concurrency 1 reproduces the old serial
server's behavior (one request in flight), so the sweep IS the before/after of the multi-GPU
replica pool. Every response is checked: an error or empty transcript fails the run loudly, and
all concurrency levels must produce identical transcripts (same clip -> same text) or the bench
refuses to report a speedup.
"""
import json
import os
import socket
import sys
import time
from concurrent.futures import ThreadPoolExecutor

HOST = os.environ.get("CORTEX_7B_HOST", "127.0.0.1")
PORT = int(os.environ.get("CORTEX_7B_PORT", "8799"))


def transcribe(audio_path: str) -> str:
    with socket.create_connection((HOST, PORT), timeout=600) as sock:
        sock.sendall((json.dumps({"audio_path": audio_path}) + "\n").encode("utf-8"))
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = sock.recv(65536)
            if not chunk:
                break
            buf += chunk
    reply = json.loads(buf.decode("utf-8").strip())
    if "error" in reply:
        raise RuntimeError(f"server error: {reply['error']}")
    return reply.get("transcript", "")


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    audio = sys.argv[1]
    n = int(sys.argv[2]) if len(sys.argv) > 2 else 8
    sweep = [int(c) for c in (sys.argv[3] if len(sys.argv) > 3 else "1,2").split(",")]

    print(f"bench: {n} requests per level, audio={audio}, sweep={sweep}")
    walls = {}
    texts = {}
    for conc in sweep:
        t0 = time.perf_counter()
        with ThreadPoolExecutor(max_workers=conc) as pool:
            results = list(pool.map(lambda _: transcribe(audio), range(n)))
        wall = time.perf_counter() - t0
        if any(not t for t in results):
            print(f"FAIL: empty transcript at concurrency {conc} — refusing to report a number.")
            return 1
        if len(set(results)) != 1:
            print(f"FAIL: divergent transcripts at concurrency {conc} — replicas disagree on the same clip.")
            return 1
        walls[conc] = wall
        texts[conc] = results[0]
        print(f"  concurrency {conc}: {wall:.2f}s wall, {n / wall:.2f} clips/s")

    if len(set(texts.values())) != 1:
        print("FAIL: transcripts differ BETWEEN concurrency levels — refusing to report a speedup.")
        return 1
    base = walls[sweep[0]]
    for conc in sweep[1:]:
        print(f"speedup at concurrency {conc} vs {sweep[0]}: {base / walls[conc]:.2f}x (identical transcripts)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
