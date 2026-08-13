"""Pre-import check: refuse to import audio the library already holds.

Why this exists (incident, 2026-08-14): 34 hours of new podcast audio were prepared by deduplicating
the SOURCE FOLDER by decoded-audio hash — correctly — and then imported. Nobody asked whether the
LIBRARY already held those recordings. It did. Three files came back a second time and added **493
clips duplicating 494 already-reviewed ones**: the entire original corpus, silently doubled. What it
costs a training set: the same speech can land in train AND test, so any accuracy measured across
that split is partly a memory test and nothing in the number says so; and reviewers judge the same
audio twice for nothing.

Why the check runs HERE and not as a sweep gate. Two cheaper signals were tried and both failed:

  * `audio_fingerprint` and `audio_content_hash` are per-SOURCE-FILE and are computed from the file
    as stored, so a FLAC and its 16 kHz WAV conversion get different values. Neither survives the
    format change that caused the incident. (Verified on the live rows: 0 shared values between the
    two copies of the same recording.)
  * A cheap `(duration_ms, raw_transcript)` heuristic over the whole library collided constantly on
    genuinely different recordings — same speaker, VAD durations clamped to the same bounds, short
    common phrases. It flagged 4 false pairs on a clean library. A gate that cries wolf gets ignored,
    which is worse than not having one.

What is left is the honest test: decode and compare. That is too slow to run every sweep over the
whole corpus, and perfectly affordable once, over the folder about to be imported.

Run:  python scripts/check_import_folder_is_new.py <folder> [db_path]
Exit: 0 when every file is new, 1 when any file is already in the library.
"""

import hashlib
import os
import subprocess
import sqlite3
import sys


def audio_hash(path: str):
    """SHA-256 of the DECODED audio at the pipeline's own rate — container and codec independent."""
    proc = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", path, "-f", "s16le", "-ac", "1", "-ar", "16000", "-"],
        capture_output=True,
    )
    return hashlib.sha256(proc.stdout).hexdigest() if proc.stdout else None


def library_sources(conn: sqlite3.Connection) -> dict:
    """{decoded-hash: (source path, clip count, decided count)} for every source still on disk."""
    rows = conn.execute(
        """
        SELECT audio_path, COUNT(*),
               SUM(CASE WHEN TRIM(COALESCE(human_decision, '')) <> '' THEN 1 ELSE 0 END)
        FROM speech_segments
        GROUP BY audio_path
        """
    ).fetchall()
    out = {}
    for path, clips, decided in rows:
        if not path or not os.path.exists(path):
            # A source that has been moved or deleted cannot be compared. Say so rather than
            # letting it pass as "not a duplicate" — silence here is how the incident happened.
            print(f"  note: library source unavailable, cannot compare: {os.path.basename(path or '?')}")
            continue
        digest = audio_hash(path)
        if digest:
            out[digest] = (path, clips, decided or 0)
    return out


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: check_import_folder_is_new.py <folder> [db_path]")
        return 2
    folder = sys.argv[1]
    db_path = sys.argv[2] if len(sys.argv) > 2 else os.path.join(
        os.environ.get("CORTEX_DB_DIR", os.path.join(os.environ["APPDATA"], "cortex-speech")),
        "cortex-speech.db",
    )
    if not os.path.isdir(folder):
        print(f"FAIL: not a folder: {folder}")
        return 1
    if not os.path.exists(db_path):
        print(f"FAIL: database not found: {db_path}")
        return 1

    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    print("hashing library sources...")
    known = library_sources(conn)
    print(f"  {len(known)} distinct recordings already in the library")

    candidates = sorted(
        os.path.join(folder, f)
        for f in os.listdir(folder)
        if os.path.isfile(os.path.join(folder, f)) and not f.lower().endswith((".xml", ".txt", ".json"))
    )
    print(f"checking {len(candidates)} file(s) in {folder}")

    clashes, unreadable = [], []
    for path in candidates:
        digest = audio_hash(path)
        if digest is None:
            unreadable.append(path)
            continue
        if digest in known:
            clashes.append((path, known[digest]))

    for path in unreadable:
        print(f"  WARNING: could not decode, not checked: {os.path.basename(path)}")

    if clashes:
        print(f"\nFAIL: {len(clashes)} file(s) are ALREADY in the library:")
        for path, (existing, clips, decided) in clashes:
            print(f"  {os.path.basename(path)}")
            print(f"      same audio as {os.path.basename(existing)} — {clips} clips, {decided} already reviewed")
        print("\nRemove these from the import folder. Re-importing them duplicates reviewed work")
        print("and can leak the same speech into both train and test.")
        return 1

    print(f"\nOK: all {len(candidates)} file(s) are new to the library")
    return 0


if __name__ == "__main__":
    sys.exit(main())
