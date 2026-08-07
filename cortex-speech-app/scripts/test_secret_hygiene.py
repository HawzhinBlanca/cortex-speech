#!/usr/bin/env python3
"""No credential may be committed — checked on every sweep, and auditable across all of history.

WHY THIS EXISTS. Before 2026-08-08 this repository had NO credential scanning of any kind. The privacy
policies cover consent and egress; `test_windows_repo_hygiene.py` covers hardcoded profile PATHS; the
DPAPI tests cover keys AT REST. Nothing looked for a key that had been committed. A leaked
OpenRouter/Gemini/ElevenLabs key would have been caught only by luck, and this repository is PUBLIC —
so "caught later" means "caught after it was published, indexed and scraped".

TWO MODES, because they answer different questions:

  (default) TRACKED-TREE scan — every file `git ls-files` reports, right now. Runs in about a second,
            so it belongs in the per-sweep policy suite. This is the one that stops a key BEFORE the
            commit that publishes it becomes the commit everyone pulled.

  --history EXHAUSTIVE scan — every blob in the object database, reachable AND unreachable, which is
            what an audit of a public repo actually requires. Deleting a file does not remove it from
            history: a key committed once and removed in the next commit is still there, still
            fetchable, still indexable. Slower (thousands of blobs), so it is on-demand.

FIRST FULL RUN, 2026-08-08 at 40f8f32: 4052 blobs, 1677 commits back to the first on 2026-06-17 — zero
credentials of any kind. The one `sk-...` string in history is a fixture inside the DPAPI test that
asserts `the plaintext key must never be on disk`, i.e. the test proving keys are NOT stored.

DESIGN NOTE ON FALSE POSITIVES. The patterns are deliberately shaped to real credential FORMATS rather
than to words like `password`, and a BENIGN filter drops placeholders. A scanner that cries wolf gets
switched off or ignored, which is strictly worse than not having one — so the bias here is toward
precision, and the `--history` mode is the belt-and-braces sweep that tolerates being noisier.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]

# Credential FORMATS. Each is a shape a real secret has, not a word that appears near one.
PATTERNS: list[tuple[str, re.Pattern[bytes]]] = [
    ("aws-access-key", re.compile(rb"\bAKIA[0-9A-Z]{16,}")),
    ("github-token", re.compile(rb"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36,}\b")),
    ("github-fine-grained-pat", re.compile(rb"\bgithub_pat_[A-Za-z0-9_]{50,}\b")),
    ("openai-or-openrouter-key", re.compile(rb"\bsk-(?:or-v1-)?[A-Za-z0-9]{32,}\b")),
    ("google-gemini-key", re.compile(rb"\bAIza[0-9A-Za-z_\-]{30,}")),
    ("slack-token", re.compile(rb"\bxox[baprs]-[A-Za-z0-9-]{10,}\b")),
    ("private-key-block", re.compile(rb"-----BEGIN (?:RSA |EC |OPENSSH |PGP |DSA )?PRIVATE KEY-----")),
    ("jwt", re.compile(rb"\beyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}")),
    ("elevenlabs-key", re.compile(rb"\bsk_[a-f0-9]{40,}\b")),
    ("assigned-secret", re.compile(
        rb"(?i)\b(?:api[_-]?key|secret|token|passwd|password)\b\s*[:=]\s*[\"']([A-Za-z0-9_\-]{24,})[\"']"
    )),
]

# A placeholder in a doc, a fixture in a test, or an obviously fake value is not a leak. Matched against
# the whole LINE, so the surrounding context can exonerate a credential-shaped string.
BENIGN = re.compile(
    rb"(?i)(your[_-]?(api[_-])?key|xxx+|placeholder|example|<[a-z_]+>|redacted|dummy|fake|"
    rb"sk-test|super-secret-value|changeme|insert[_-]|TODO|FIXME|noreply|0{8,}|1234567890|abcdef0123)"
)

BINARY_EXT = re.compile(
    r"\.(wav|mp3|flac|ogg|m4a|onnx|pt|bin|png|jpg|jpeg|gif|ico|pdf|zip|gz|tar|exe|dll|so|dylib|woff2?|ttf|parquet)$",
    re.I,
)


def _line_at(data: bytes, pos: int) -> bytes:
    start = data.rfind(b"\n", 0, pos) + 1
    end = data.find(b"\n", pos)
    return data[start : end if end != -1 else len(data)]


def scan(data: bytes, where: str) -> list[tuple[str, str, str]]:
    """Findings as (pattern, where, line). Empty when clean."""
    if b"\x00" in data[:4096]:
        return []
    out = []
    for name, pat in PATTERNS:
        for m in pat.finditer(data):
            line = _line_at(data, m.start())
            if BENIGN.search(line):
                continue
            out.append((name, where, line.strip()[:200].decode(errors="replace")))
    return out


def scan_tracked_tree() -> list[tuple[str, str, str]]:
    files = subprocess.run(
        ["git", "ls-files", "-z"], cwd=REPO_ROOT.parent, capture_output=True, check=True
    ).stdout.split(b"\0")
    findings: list[tuple[str, str, str]] = []
    for raw in files:
        if not raw:
            continue
        rel = raw.decode("utf-8", "replace")
        if BINARY_EXT.search(rel):
            continue
        path = REPO_ROOT.parent / rel
        try:
            data = path.read_bytes()
        except OSError:
            continue  # deleted/unreadable in the working tree; --history covers what was committed
        findings.extend(scan(data, rel))
    return findings


def scan_all_history() -> tuple[list[tuple[str, str, str]], int]:
    """Every blob in the object database — reachable and unreachable."""
    listing = subprocess.run(
        ["git", "cat-file", "--batch-all-objects", "--batch-check=%(objectname) %(objecttype)"],
        cwd=REPO_ROOT.parent, capture_output=True, text=True, check=True,
    ).stdout.splitlines()
    blobs = [ln.split()[0] for ln in listing if ln.endswith(" blob")]

    proc = subprocess.Popen(
        ["git", "cat-file", "--batch"], cwd=REPO_ROOT.parent,
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    )
    assert proc.stdin and proc.stdout
    findings: list[tuple[str, str, str]] = []
    for sha in blobs:
        proc.stdin.write(f"{sha}\n".encode())
        proc.stdin.flush()
        header = proc.stdout.readline().decode(errors="replace").split()
        if len(header) < 3:
            continue
        # ALWAYS drain the payload before deciding to skip. `--batch` emits the bytes regardless, and
        # skipping without reading desynchronises the pipe so the next header parses object data and the
        # loop deadlocks. (Learned the hard way while writing the first version of this.)
        data = proc.stdout.read(int(header[2]))
        proc.stdout.read(1)
        if header[1] != "blob":
            continue
        findings.extend(scan(data, f"blob {sha[:12]}"))
    proc.stdin.close()
    proc.wait()
    return findings, len(blobs)


def main() -> int:
    history = "--history" in sys.argv
    if history:
        findings, n = scan_all_history()
        scope = f"{n} blobs (all of history, reachable + unreachable)"
    else:
        findings = scan_tracked_tree()
        scope = "tracked working tree"

    if findings:
        print(f"SECRET HYGIENE: FAIL — credential-shaped strings in {scope}", file=sys.stderr)
        for name, where, line in findings[:40]:
            print(f"  [{name}] {where}\n      {line}", file=sys.stderr)
        if len(findings) > 40:
            print(f"  ... {len(findings) - 40} more", file=sys.stderr)
        print(
            "\nIf one is a placeholder or fixture, make that legible in the LINE (the BENIGN filter "
            "reads it) rather than loosening the pattern. If one is real: rotate it FIRST — it is "
            "already published — then remove it, and only then decide whether a history rewrite is "
            "worth breaking every commit SHA for.",
            file=sys.stderr,
        )
        return 1

    print(f"secret hygiene passed ({scope})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
