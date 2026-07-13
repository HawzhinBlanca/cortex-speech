#!/usr/bin/env python3
"""Thin client the Cortex app's WSL7B path runs:  python3 cortex_7b_client.py --segment-id <id> --stdout-only

Reads the segment's audio_path + alignment from the app DB, asks the warm cortex_7b_server.py to
transcribe it, and prints the one line the app parses:
    __RESULT__={"raw_transcript": "...", "confidence": null}
Stdlib only (sqlite3 + socket + subprocess), so it runs under the same python the app invokes.

Failure contract (so the app never stores a silent blank — see pipeline.rs `run_primary_wsl_pass_for_import`):
  exit 0   -> printed a real __RESULT__ from a REACHABLE server (an empty transcript here is legitimate,
              e.g. a silent clip).
  exit !=0 + a clear stderr message + NO __RESULT__  -> an INFRASTRUCTURE failure (server not running,
              DB unreadable, segment missing, server-side error). The app turns this into a "force-use
              the Champion" cancel: it rolls back the import and tells the owner to start the 7B server.

This is the repo-tracked, machine-agnostic copy: it carries NO hardcoded user paths. The app DB is found
via (in order): the CORTEX_7B_DB env var, else the Windows roaming profile resolved generically from
%APPDATA% (so it works for any Windows user without a baked-in path). HOST/PORT are overridable via
CORTEX_7B_HOST / CORTEX_7B_PORT. The warm GPU server (cortex_7b_server.py, which loads the 31 GB
OmniASR-7B Champion + LoRA adapter) is machine-specific and stays out of the repo.
"""
import os
import sys
import json
import socket
import sqlite3
import argparse
import subprocess

HOST = os.environ.get("CORTEX_7B_HOST", "127.0.0.1")
PORT = int(os.environ.get("CORTEX_7B_PORT", "8799"))

# Exit codes for the app's stderr preview (any non-zero => marked unavailable / import cancelled).
EX_DB = 4        # could not locate or read the app DB
EX_NOSEG = 5     # segment id not present in the DB
EX_UNREACHABLE = 2  # 7B server not listening
EX_SERVER = 3    # server replied with an error


def win_to_wsl(p):
    """Map a Windows drive path to its WSL mount (a C: path becomes /mnt/c/...)."""
    p = (p or "").replace("\\", "/")
    if p.startswith("//?/"):  # Windows extended-length path prefix \\?\
        p = p[4:]
    if p.startswith("//"):  # any leftover UNC-ish doubling
        p = "/" + p.lstrip("/")
    if len(p) > 2 and p[1] == ":":
        return "/mnt/" + p[0].lower() + p[2:]
    return p


def resolve_db_path():
    """Locate the app's sqlite DB without any baked-in user path.

    1. CORTEX_7B_DB if the app (or launcher) set it.
    2. Otherwise ask Windows for %APPDATA% from inside WSL and map it to /mnt — generic across users.
    """
    env_db = os.environ.get("CORTEX_7B_DB")
    if env_db:
        return env_db
    try:
        appdata = subprocess.check_output(
            ["cmd.exe", "/c", "echo %APPDATA%"], stderr=subprocess.DEVNULL
        ).decode("utf-8", "replace").strip()
        if appdata and len(appdata) > 2 and appdata[1] == ":":
            return win_to_wsl(appdata) + "/cortex-speech/cortex-speech.db"
    except Exception:
        pass
    return None


def emit(text):
    print("__RESULT__=" + json.dumps({"raw_transcript": text or "", "confidence": None}, ensure_ascii=False))


def fail(code, msg):
    """Infrastructure failure: tell the app (stderr) and exit non-zero WITHOUT a __RESULT__ line,
    so it cancels the force-7B import rather than recording a blank transcript."""
    sys.stderr.write(msg.rstrip("\n") + "\n")
    sys.stderr.flush()
    sys.exit(code)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--segment-id", required=True)
    ap.add_argument("--stdout-only", action="store_true")
    a = ap.parse_args()

    db = resolve_db_path()
    if not db:
        fail(EX_DB, "7B client: could not locate the app DB (set CORTEX_7B_DB to the cortex-speech.db path).")

    # The app holds its DB open in WAL mode while it calls us, and over the WSL /mnt 9p filesystem the
    # normal SQLite locks raise "disk I/O error", while an immutable read of the main file MISSES
    # freshly-imported segments that still live in -wal. Robust fix: snapshot-copy the DB + its -wal
    # (+ -shm) to a private temp file and read that — no lock on the live DB, and SQLite applies the WAL
    # so we see fresh segments. (audio_path + alignment never change once a segment exists.)
    import shutil
    import tempfile
    import time
    row = None
    read_err = None
    # The app writes the WAL while we copy, so a single-shot copy can be torn (page copied mid-write)
    # and the open fails — which previously escalated a transient blip into an EX_DB infra-failure.
    # Retry the whole snapshot: a torn copy is momentary. Deliberately do NOT copy -shm — the shared-
    # memory index is only valid for the live connections that created it; a copied one makes SQLite
    # refuse the database outright ("unable to open database file", reproduced on this machine), while
    # a fresh read of db + -wal rebuilds it automatically.
    for attempt in range(3):
        tmpdir = tempfile.mkdtemp(prefix="cortex7b_")
        try:
            tmp = os.path.join(tmpdir, "snap.db")
            shutil.copyfile(db, tmp)
            wal = db + "-wal"
            if os.path.exists(wal):
                try:
                    shutil.copyfile(wal, tmp + "-wal")
                except Exception:
                    pass
            con = sqlite3.connect(f"file:{tmp}?mode=ro", uri=True)
            row = con.execute(
                "SELECT audio_path, alignment_json FROM speech_segments WHERE id=?", (a.segment_id,)
            ).fetchone()
            con.close()
            read_err = None
            break
        except Exception as e:
            read_err = e
            time.sleep(0.2 * (attempt + 1))
        finally:
            shutil.rmtree(tmpdir, ignore_errors=True)

    if read_err is not None:
        fail(EX_DB, f"7B client: could not read the app DB at {db}: {read_err}")
    if row is None:
        fail(EX_NOSEG, f"7B client: segment {a.segment_id} not found in the app DB")

    audio_path, alignment = row
    start_ms = end_ms = None
    if alignment:
        try:
            m = json.loads(alignment)
            start_ms, end_ms = m.get("source_start_ms"), m.get("source_end_ms")
        except Exception:
            pass

    req = {"audio_path": win_to_wsl(audio_path), "start_ms": start_ms, "end_ms": end_ms}
    # 280 s: below the app's own 300 s per-attempt budget (pipeline.rs) but no longer 120 s shorter —
    # the old 180 s made the client the effective (and mislabeled) timeout authority.
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(280)
        s.connect((HOST, PORT))
        s.sendall((json.dumps(req) + "\n").encode("utf-8"))
        buf = b""
        while not buf.endswith(b"\n"):
            d = s.recv(65536)
            if not d:
                break
            buf += d
        s.close()
    except socket.timeout:
        # Distinct from "not running": the server ACCEPTED the connection but did not answer in time
        # (GPU busy with a training run, or the 31 GB model still warming). Saying "not running" here
        # sent the owner to restart a healthy server (true-10 audit 2026-07-09).
        fail(EX_UNREACHABLE,
             f"7B server reachable but timed out after 280 s on {HOST}:{PORT} - it is likely busy "
             f"(GPU shared with a training run?) or still loading the model. Wait and re-transcribe; "
             f"do not restart the server.")
    except (ConnectionRefusedError, OSError) as e:
        fail(EX_UNREACHABLE,
             f"7B engine not running: cannot reach the OmniASR-7B server on {HOST}:{PORT} ({e}). "
             f"Start the 7B server (e.g. 'Start 7B server.bat') and re-transcribe.")

    if not buf.strip():
        fail(EX_SERVER, f"7B engine returned no data from {HOST}:{PORT} (server may be loading or crashed).")
    try:
        resp = json.loads(buf.decode("utf-8").strip())
    except Exception as e:
        fail(EX_SERVER, f"7B engine sent an unparseable reply: {e}")
    if "error" in resp:
        fail(EX_SERVER, f"7B engine error: {resp['error']}")

    # Reachable server, valid reply: a real transcript (possibly legitimately empty for a silent clip).
    emit(resp.get("transcript", ""))


if __name__ == "__main__":
    main()
