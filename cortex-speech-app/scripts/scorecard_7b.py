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
"""
import json
import os
import random
import socket
import sys
import unicodedata

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
    pairs = []
    for i, row in enumerate(rows):
        wav, ref = row[0], row[1]
        try:
            hyp = transcribe_7b(wav)
        except Exception as e:
            print(f"  SKIP {wav}: {e}")
            continue
        r, h = _NORM(ref), _NORM(hyp)
        if not r:
            continue  # zero-reference clip drops out of the ratio-of-sums (matches eval.rs)
        rc, hc = r.replace(" ", ""), h.replace(" ", "")
        per_clip.append((edit_distance(list(rc), list(hc)), len(rc)))
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

    out_tsv = os.path.join(os.path.dirname(os.path.abspath(manifest)), "omni7b_results.tsv")
    with open(out_tsv, "w", encoding="utf-8") as f:
        f.write("char_dist\tchar_ref_len\n")
        for d, r in per_clip:
            f.write(f"{d}\t{r}\n")
    out_json = os.path.join(os.path.dirname(os.path.abspath(manifest)), "omni7b_eval_summary.json")
    with open(out_json, "w", encoding="utf-8") as f:
        json.dump({"n": n, "micro_cer": micro, "ci_lo": lo, "ci_hi": hi, "examples": pairs[:10]},
                  f, ensure_ascii=False, indent=2)

    print("=" * 60)
    print(f"  OmniASR-7B (warm server) micro CER = {micro*100:.2f}%   95% CI [{lo*100:.2f}%, {hi*100:.2f}%]   N={n}")
    print(f"  (fine-tuned MMS-1B: 21.00% [19.93, 22.04] N=900; stock CTC-300M: 29.40% N=400)")
    print(f"  per-clip -> {out_tsv}")
    print("=" * 60)
    if n < 100:
        print(f"  NOTE: N={n} is a SPOT CHECK, not a publishable/default-decision number. For that, build")
        print(f"        the gold corpus (scripts/build_ckb_gold.py 900) and re-run on its manifest.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
