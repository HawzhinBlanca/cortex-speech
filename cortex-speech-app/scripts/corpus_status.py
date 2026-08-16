#!/usr/bin/env python3
"""Which recordings in a corpus folder are already in the library, and which are not.

The app ALREADY knows this — it hashes decoded audio on import and rehydrates those identities at
startup ("Audio dedup: rehydrated 179 recording identities"). The owner just cannot SEE it, so the
same folder gets re-imported "just in case" and the library grows duplicates. This surfaces the
state that exists rather than inventing a second one to drift from it.

Matching is by FILE STEM, not full name: a recording enters the library through processing steps that
rename it (`X.mp4` -> `X-esv2-speech-50p.wav` -> `X_16k.wav`), so the stem is what survives.

    python scripts/corpus_status.py "D:\\Kurdish Corpora\\sorani\\ZarPodcast"
    python scripts/corpus_status.py --all "D:\\Kurdish Corpora"

Writes nothing by default. `--write` drops a PROCESSED.md in the folder so the answer is visible in
Explorer without running anything.
"""

from __future__ import annotations

import os
import re
import sqlite3
import sys
from pathlib import Path

AUDIO_EXT = {".wav", ".mp3", ".mp4", ".mov", ".m4a", ".flac", ".ogg", ".opus", ".webm", ".aac", ".wma"}
# The processing suffixes a recording picks up on its way into the library.
STRIP = re.compile(r"(-esv2.*|-speech-.*|_16k.*|-relevel.*|-denoised.*)$", re.IGNORECASE)


def stem(name: str) -> str:
    return STRIP.sub("", Path(name).stem)


def library_stems(db_path: Path) -> dict[str, int]:
    """stem -> number of clips in the library from that recording."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    out: dict[str, int] = {}
    for (path,) in conn.execute("SELECT audio_path FROM speech_segments"):
        out[stem(os.path.basename(path))] = out.get(stem(os.path.basename(path)), 0) + 1
    conn.close()
    return out


def scan(folder: Path, lib: dict[str, int]) -> tuple[list[tuple[str, int]], list[str]]:
    done, todo = [], []
    for entry in sorted(folder.iterdir()):
        if not entry.is_file() or entry.suffix.lower() not in AUDIO_EXT:
            continue
        s = stem(entry.name)
        (done.append((entry.name, lib[s])) if s in lib else todo.append(entry.name))
    return done, todo


def report(folder: Path, lib: dict[str, int], write: bool) -> int:
    if not folder.is_dir():
        print(f"  (not a folder: {folder})")
        return 0
    done, todo = scan(folder, lib)
    if not done and not todo:
        return 0
    print(f"\n{folder}")
    print(f"  already in the library : {len(done)} recording(s), {sum(n for _, n in done)} clips")
    print(f"  NOT yet imported       : {len(todo)} recording(s)")
    for name in todo[:10]:
        print(f"      {name}")
    if len(todo) > 10:
        print(f"      … and {len(todo) - 10} more")
    if write:
        md = [f"# Import status — {folder.name}", ""]
        md.append(f"- **In the library:** {len(done)} recording(s), {sum(n for _, n in done)} clips")
        md.append(f"- **Not yet imported:** {len(todo)} recording(s)")
        md.append("")
        if todo:
            md += ["## Not yet imported", ""] + [f"- {n}" for n in todo]
        if done:
            md += ["", "## Already imported (do not re-import)", ""] + [f"- {n} — {c} clips" for n, c in done]
        (folder / "PROCESSED.md").write_text("\n".join(md) + "\n", encoding="utf-8")
        print(f"  wrote {folder / 'PROCESSED.md'}")
    return len(todo)


def main(argv: list[str]) -> int:
    args = [a for a in argv[1:] if not a.startswith("--")]
    write = "--write" in argv
    every = "--all" in argv
    if not args:
        print(__doc__)
        return 2
    appdata = os.environ.get("APPDATA")
    db = Path(appdata) / "cortex-speech" / "cortex-speech.db" if appdata else None
    if not db or not db.is_file():
        print(f"library database not found at {db}")
        return 1
    lib = library_stems(db)
    print(f"library holds {len(lib)} distinct recording(s)")
    root = Path(args[0])
    folders = [p for p in sorted(root.rglob("*")) if p.is_dir()] + [root] if every else [root]
    for f in folders:
        report(f, lib, write)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
