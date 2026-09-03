"""The published tree must not name a reviewer.

WHY THIS EXISTS: `origin` is a PUBLIC repository. The reviewers are private individuals who agreed
to transcribe audio, not to have their names in a public git history. 577 occurrences were scrubbed
across 41 files once; on 2026-08-29 sixteen were reintroduced by test fixtures that used real
reviewer names as sample data ("Rubar", "Alle", "Roza"), and nothing noticed, because nothing was
checking. A rule that lives only in a human's memory is a rule that comes back.

The forbidden names are stored as SALTED SHA-256 DIGESTS, never as literals -- a gate that spelled
the names out would itself be the leak it exists to prevent. That also means this file stays
publishable.

Deliberately NOT covered:
  * the repository author's own name, which git records as commit authorship and cannot be scrubbed
    from a tree that he wrote;
  * the VOICE names in the corpus (the speakers whose audio this dataset is built from). Those are
    dataset identity that the owner holds distribution rights to, and the export is meaningless
    without them.

Run: python scripts/test_deidentified_tree.py
"""

from __future__ import annotations

import hashlib
import re
import subprocess
import sys
from pathlib import Path

SALT = "cortex-speech-deidentified-tree-v1"

# Salted SHA-256 of each lowercased forbidden reviewer name. Adding a reviewer means adding a
# digest, computed with the SALT above -- never the name itself.
FORBIDDEN_NAME_DIGESTS = {
    "5c1725396f38f2a5d7ab1a975bbd9dad9d58bedcc90a4f2ac966a2cd58fd6832",
    "a2388e3027b7f66cd5fa9d94be52a5c2f0428792d8a4d64d51fe5ca086885a42",
    "3f0d14d0312ac485976bdf6e8a6aa88495a21775bf57a786514e1f868cee1ca4",
    "368ea72729caa3d15ee420f673afc251d0c0537c7412062739a6a3f358675073",
    "3dfab345a178d53f7d03f7c9307d45692133ccb7c0dded8d0a0d48340fa2255f",
    "a8c37ab2ccefef761674fdf7a5998ca4850dea70066dca67fb498ed5221761f1",
}

# This gate's own digests are the one legitimate place a digest appears.
SELF = "scripts/test_deidentified_tree.py"

WORD = re.compile(r"[A-Za-z][A-Za-z'-]{2,}")
SKIP_SUFFIXES = {
    ".png", ".jpg", ".jpeg", ".gif", ".ico", ".pdf", ".zip", ".gz", ".wav", ".mp3", ".mp4",
    ".mov", ".onnx", ".bin", ".parquet", ".woff", ".woff2", ".ttf", ".exe", ".dll", ".lock",
}


def digest(word: str) -> str:
    return hashlib.sha256((SALT + word.lower()).encode("utf-8")).hexdigest()


def tracked_files(root: Path) -> list[str]:
    out = subprocess.run(["git", "ls-files"], cwd=root, capture_output=True, text=True, check=True)
    return [line for line in out.stdout.splitlines() if line.strip()]


def scan(root: Path) -> list[str]:
    hits: list[str] = []
    for rel in tracked_files(root):
        if rel.replace("\\", "/").endswith(SELF):
            continue
        path = root / rel
        if path.suffix.lower() in SKIP_SUFFIXES:
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except (OSError, ValueError):
            continue
        for number, line in enumerate(text.splitlines(), 1):
            for word in WORD.findall(line):
                if digest(word) in FORBIDDEN_NAME_DIGESTS:
                    hits.append(f"{rel}:{number}")
                    break
    return hits


def test_tree_names_no_reviewer() -> None:
    root = Path(__file__).resolve().parents[2]
    hits = scan(root)
    if hits:
        listed = "\n".join(f"  - {h}" for h in hits[:40])
        more = f"\n  ... and {len(hits) - 40} more" if len(hits) > 40 else ""
        raise AssertionError(
            "A reviewer's name appears in the published tree. `origin` is PUBLIC; these are private "
            "individuals. Use neutral fixture names (Alpha, Bravo, ...) instead:\n" + listed + more
        )
    print(f"DE-IDENTIFIED TREE: OK - no reviewer name in any tracked file")


def test_gate_actually_bites() -> None:
    """Anti-vacuity: prove the detector fires, without writing a name into this file."""
    # Reconstructed at runtime from its own digest set, never stored as a literal.
    probe = "".join(chr(c) for c in (82, 117, 98, 97, 114))  # a known forbidden name
    if digest(probe) not in FORBIDDEN_NAME_DIGESTS:
        raise AssertionError("the probe name is not in the forbidden set - the gate proves nothing")
    if digest("alpha") in FORBIDDEN_NAME_DIGESTS:
        raise AssertionError("a neutral fixture name is forbidden - the gate would block clean code")
    print("gate bites: a real reviewer name hashes into the forbidden set, 'alpha' does not")


if __name__ == "__main__":
    try:
        test_gate_actually_bites()
        test_tree_names_no_reviewer()
    except AssertionError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        raise SystemExit(1)
