#!/usr/bin/env python3
"""Publishable CER scorecard for the OmniASR-7B Champion via the WARM server (no second model load).

Drives the already-running cortex_7b_server.py over its local socket (127.0.0.1:8799) so it does NOT
re-load the 31 GB model — run this INSIDE WSL (the server binds inside WSL's network namespace):

    wsl python3 cortex-speech-app/scripts/scorecard_7b.py <gold_manifest.tsv> [bootstrap=3000]

Manifest rows: <wav_path>\t<reference>[\t<gender>\t<age>]   (wav paths must be WSL-visible, e.g.
/mnt/c/...). Same NFC+lower+whitespace normalization and seed-42 utterance-bootstrap (Bisani & Ney
ratio-of-sums) as scripts/scorecard_finetuned.py, so the 7B number is directly comparable to the
fine-tuned 21.00% and the stock 29.40%. Zero-reference clips drop out of the ratio (like eval.rs).

This is the reusable harness for the honest default-engine decision (deep-audit F1): point it at the
gold corpus (build one with scripts/build_ckb_gold.py from CORTEX_CORPUS_ZIP + CORTEX_CORPUS_TSV) and
it prints the real 7B micro CER + 95% CI at whatever N the manifest holds. No number is fabricated.

Concurrency: CORTEX_7B_WORKERS=N (default 1) keeps N clips in flight — set it to the server's
replica count (CORTEX_7B_DEVICES) so a multi-GPU server is actually saturated. For N <= replica
count, results/CER/WER/output order are identical to the serial run; only wall-clock changes.
HONEST LIMIT: pushing N beyond the replica count queues clips at the server, and a clip whose
queue wait exceeds the 300 s socket timeout is SKIPped — changing n and the published number.
Ctrl+C aborts cleanly: in-flight clips finish, queued clips are cancelled, no partial number is
reported.
"""
import json
import os
import random
import socket
import sys
import time
import unicodedata
from concurrent.futures import ThreadPoolExecutor

# P2.1: WER + timing telemetry (M1.3 needs CER + WER + RTF). CER stays inline/byte-identical below.
from asr_metrics import bootstrap_ci as boot_ci
from asr_metrics import micro as micro_rate
from asr_metrics import word_pair

HOST = os.environ.get("CORTEX_7B_HOST", "127.0.0.1")
PORT = int(os.environ.get("CORTEX_7B_PORT", "8799"))


def norm(s: str) -> str:
    return " ".join(unicodedata.normalize("NFC", s or "").strip().lower().split())


# Punctuation + digit normalization for a FAIR ckb CER: transcription CER conventionally ignores
# punctuation, and a reference's Arabic-Indic digits (١٠) vs an ASR's verbalized/Latin form are a
# convention difference, not a recognition error. Set CORTEX_CER_STRIP=1 to apply. Kept OPT-IN so the
# default norm() stays byte-identical to scorecard_finetuned.py (comparable to the published 21%).
_AR_DIGITS = {ord(c): str(i) for i, c in enumerate("٠١٢٣٤٥٦٧٨٩")}
_AR_DIGITS.update({ord(c): str(i) for i, c in enumerate("۰۱۲۳۴۵۶۷۸۹")})
_PUNCT = "،؛؟٪…«»“”‘’.,!?:;\"'()[]{}-—–/\\"


def norm_fair(s: str) -> str:
    s = unicodedata.normalize("NFC", s or "").strip().lower().translate(_AR_DIGITS)
    s = "".join(" " if ch in _PUNCT else ch for ch in s)
    return " ".join(s.split())


_NORM = norm_fair if os.environ.get("CORTEX_CER_STRIP") == "1" else norm


def edit_distance(a, b) -> int:
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (0 if ca == cb else 1)))
        prev = cur
    return prev[-1]


def transcribe_7b(audio_path: str, start_ms=None, end_ms=None, timeout=300.0) -> str:
    """One request/reply against the warm 7B server. Raises on error so nothing is fabricated."""
    req = {"audio_path": audio_path}
    if start_ms is not None and end_ms is not None:
        req["start_ms"] = start_ms
        req["end_ms"] = end_ms
    with socket.create_connection((HOST, PORT), timeout=timeout) as sock:
        sock.sendall((json.dumps(req) + "\n").encode("utf-8"))
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = sock.recv(65536)
            if not chunk:
                break
            buf += chunk
    reply = json.loads(buf.decode("utf-8").strip())
    if "error" in reply:
        raise RuntimeError(f"7B server error for {audio_path}: {reply['error']}")
    return reply.get("transcript", "")


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    sys.stdout.reconfigure(encoding="utf-8")
    manifest = sys.argv[1]
    n_boot = int(sys.argv[2]) if len(sys.argv) > 2 else 3000

    rows = [l.rstrip("\n").split("\t") for l in open(manifest, encoding="utf-8") if "\t" in l]
    per_clip = []  # (char_dist, char_ref_len)
    word_clip = []  # P2.1: (word_dist, word_ref_len) for WER, scored on the SAME normalized pairs
    pairs = []
    total_proc_s = 0.0  # P2.1: cumulative per-clip decode time (sum of latencies, all workers)
    workers = max(1, int(os.environ.get("CORTEX_7B_WORKERS", "1")))

    def fetch(row):
        """(hyp, elapsed_s, error) for one clip — runs on a worker thread; scoring stays in order below."""
        t0 = time.perf_counter()
        try:
            return transcribe_7b(row[0]), time.perf_counter() - t0, None
        except Exception as e:  # noqa: BLE001 — the main loop SKIPs and reports, same as before
            return None, time.perf_counter() - t0, e

    wall_t0 = time.perf_counter()
    # Bounded-chunk submission, NOT one whole-manifest map: a full submit made Ctrl+C useless
    # (shutdown drained every queued request — a 900-clip run kept hammering the warm server for
    # hours after the interrupt) and silenced all progress output until the very end
    # (adversarial review 2026-07-12). Chunks keep progress live and bound the abort latency to
    # the clips already in flight. pool.map preserves order within each chunk and chunks are
    # extended in order, so per_clip/word_clip/pairs stay aligned exactly as in the serial version.
    results = []
    pool = ThreadPoolExecutor(max_workers=workers)
    try:
        chunk = max(workers * 4, 8)
        for start in range(0, len(rows), chunk):
            results.extend(pool.map(fetch, rows[start : start + chunk]))
            done = min(start + chunk, len(rows))
            if done < len(rows):
                print(f"  ...{done}/{len(rows)}", flush=True)
        pool.shutdown(wait=True)
    except KeyboardInterrupt:
        pool.shutdown(wait=False, cancel_futures=True)
        print("\ninterrupted — queued clips cancelled; refusing to report a partial number.")
        return 130
    wall_s = time.perf_counter() - wall_t0

    for i, (row, (hyp, elapsed, err)) in enumerate(zip(rows, results)):
        wav, ref = row[0], row[1]
        if err is not None:
            print(f"  SKIP {wav}: {err}")
            continue
        total_proc_s += elapsed
        r, h = _NORM(ref), _NORM(hyp)
        if not r:
            continue  # zero-reference clip drops out of the ratio-of-sums (matches eval.rs)
        # CER over the full normalized string WITH interior whitespace — jiwer's default CER definition,
        # matching scorecard_finetuned.py (jiwer.process_characters) and eval.rs (tokenize_chars keeps
        # whitespace). The previous `.replace(" ", "")` scored space-INSENSITIVELY, so a word-segmentation
        # error (a real Sorani ASR error class, e.g. "هاوڕێ من" -> "هاوڕێمن") scored 0% instead of its true
        # 12.5%, DEFLATING the champion's CER and breaking this script's own "directly comparable to the
        # fine-tuned 21%" contract. Verified: edit_distance(list(r), list(h)) == jiwer S+D+I on that pair.
        per_clip.append((edit_distance(list(r), list(h)), len(r)))
        word_clip.append(word_pair(r, h))
        pairs.append({"ref": ref, "hyp": hyp})
        if (i + 1) % 25 == 0:
            print(f"  ...{i+1}/{len(rows)}")

    n = len(per_clip)
    if n == 0:
        print("no scorable clips (no reachable audio with a non-empty reference)")
        return 1

    dists = [d for d, _ in per_clip]
    refs = [r for _, r in per_clip]
    micro = sum(dists) / max(sum(refs), 1)

    rng = random.Random(42)
    boots = []
    for _ in range(n_boot):
        sample = [rng.randrange(n) for _ in range(n)]
        sd = sum(dists[k] for k in sample)
        sr = sum(refs[k] for k in sample)
        boots.append(sd / max(sr, 1))
    boots.sort()
    lo = boots[int(0.025 * len(boots))]
    hi = boots[int(0.975 * len(boots)) - 1]

    # P2.1: WER (micro + seeded bootstrap CI, same recipe as CER) and processing throughput. RTF
    # (proc/audio) needs a per-clip audio-duration source the generic manifest doesn't carry, so we
    # report throughput (clips/s + mean s/clip) as the honest speed metric here.
    wer = micro_rate(word_clip)
    wlo, whi = boot_ci(word_clip, n_boot, 42)
    mean_proc_s = total_proc_s / n if n else 0.0
    # Throughput is WALL-based: with CORTEX_7B_WORKERS>1 the per-clip latencies overlap, so
    # n/total_proc_s would overstate nothing but describe no real clock. At workers=1,
    # wall_s ~= total_proc_s and this matches the old n/total_proc_s definition.
    throughput = n / wall_s if wall_s > 0 else 0.0

    out_tsv = os.path.join(os.path.dirname(os.path.abspath(manifest)), "omni7b_results.tsv")
    with open(out_tsv, "w", encoding="utf-8") as f:
        f.write("char_dist\tchar_ref_len\tword_dist\tword_ref_len\n")
        for (d, r), (wd, wr) in zip(per_clip, word_clip):
            f.write(f"{d}\t{r}\t{wd}\t{wr}\n")
    out_json = os.path.join(os.path.dirname(os.path.abspath(manifest)), "omni7b_eval_summary.json")
    with open(out_json, "w", encoding="utf-8") as f:
        json.dump(
            {
                "n": n,
                "micro_cer": micro,
                "ci_lo": lo,
                "ci_hi": hi,
                "micro_wer": wer,
                "wer_ci_lo": wlo,
                "wer_ci_hi": whi,
                "total_proc_s": total_proc_s,
                "mean_proc_s": mean_proc_s,
                "wall_clock_s": wall_s,
                "workers": workers,
                "throughput_clips_per_s": throughput,
                "examples": pairs[:10],
            },
            f,
            ensure_ascii=False,
            indent=2,
        )

    print("=" * 60)
    print(f"  OmniASR-7B (warm server) micro CER = {micro*100:.2f}%   95% CI [{lo*100:.2f}%, {hi*100:.2f}%]   N={n}")
    print(f"  OmniASR-7B (warm server) micro WER = {wer*100:.2f}%   95% CI [{wlo*100:.2f}%, {whi*100:.2f}%]   N={n}")
    print(f"  throughput = {throughput:.2f} clips/s ({mean_proc_s:.3f} s/clip; {total_proc_s:.1f}s total)")
    print(f"  (fine-tuned MMS-1B: 21.00% [19.93, 22.04] N=900; stock CTC-300M: 29.40% N=400)")
    print(f"  per-clip -> {out_tsv}")
    print("=" * 60)
    if n < 100:
        print(f"  NOTE: N={n} is a SPOT CHECK, not a publishable/default-decision number. For that, build")
        print(f"        the gold corpus (scripts/build_ckb_gold.py 900) and re-run on its manifest.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
