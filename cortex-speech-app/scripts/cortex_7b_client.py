#!/usr/bin/env python3
"""Thin client the Cortex app's WSL7B path runs:  python3 cortex_7b_client.py --segment-id <id> --stdout-only

Reads the segment's audio_path + alignment from the app DB, asks the warm cortex_7b_server.py to
transcribe it, and prints the one line the app parses:
    __RESULT__={"raw_transcript":"...","confidence":null,"model_version_id":"...","deployment_sha256":"..."}
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
import time

HOST = os.environ.get("CORTEX_7B_HOST", "127.0.0.1")
PORT = int(os.environ.get("CORTEX_7B_PORT", "8799"))
MAX_RESPONSE_BYTES = 1024 * 1024
HEALTH_TIMEOUT_SECONDS = float(os.environ.get("CORTEX_7B_HEALTH_TIMEOUT_SECONDS", "5"))
TRANSCRIPTION_TIMEOUT_SECONDS = float(os.environ.get("CORTEX_7B_TRANSCRIPTION_TIMEOUT_SECONDS", "280"))
PROTOCOL = "cortex-omniasr-adapter"
PROTOCOL_VERSION = 1
FAMILY = "omniasr-7b"

# Exit codes for the app's stderr preview (any non-zero => marked unavailable / import cancelled).
EX_DB = 4        # could not locate or read the app DB
EX_NOSEG = 5     # segment id not present in the DB
EX_UNREACHABLE = 2  # 7B server not listening
EX_SERVER = 3    # server replied with an error


class DuplicateKeyError(ValueError):
    pass


def _object_without_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateKeyError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


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


def _is_sha256(value):
    return (
        isinstance(value, str)
        and len(value) == 64
        and value == value.lower()
        and all(char in "0123456789abcdef" for char in value)
    )


def validate_identity_response(response):
    """Return the exact served identity or refuse a reachable but untrustworthy service."""
    if not isinstance(response, dict):
        raise ValueError("reply is not a JSON object")
    if response.get("protocol") != PROTOCOL or response.get("protocolVersion") != PROTOCOL_VERSION:
        raise ValueError("reply is not from the compatible Cortex OmniASR deployment protocol")
    if response.get("family") != FAMILY:
        raise ValueError(f"reply has unexpected model family {response.get('family')!r}")
    model_id = response.get("modelVersionId")
    if not isinstance(model_id, str) or not model_id.strip():
        raise ValueError("reply has no modelVersionId")
    deployment_sha = response.get("deploymentSha256")
    if not _is_sha256(deployment_sha):
        raise ValueError("reply has no canonical deploymentSha256")
    component_sha = response.get("componentSha256")
    if not isinstance(component_sha, dict) or set(component_sha) != {
        "base", "adapter", "adapterConfig", "tokenizer"
    }:
        raise ValueError("reply has no complete componentSha256 identity")
    if any(not _is_sha256(value) for value in component_sha.values()):
        raise ValueError("reply contains a non-canonical component SHA-256")
    if response.get("language") != "ckb_Arab":
        raise ValueError(f"reply has unexpected language {response.get('language')!r}")
    if not _is_sha256(response.get("manifestSha256")):
        raise ValueError("reply has no canonical manifestSha256")
    if response.get("provenanceKind") not in {"flywheel", "legacy_bootstrap"}:
        raise ValueError(f"reply has unexpected provenanceKind {response.get('provenanceKind')!r}")
    if not isinstance(response.get("worker"), str) or not response["worker"].strip():
        raise ValueError("reply has no worker identity")
    return model_id, deployment_sha


def validate_health_response(response):
    validate_identity_response(response)
    if type(response.get("schema")) is not int or response.get("schema") != 1 or response.get("status") != "ready":
        raise ValueError("health reply does not declare schema=1/status=ready")
    return response


def validate_transcription_response(response):
    text = response.get("transcript")
    if not isinstance(text, str):
        raise ValueError("transcription reply has no string transcript")
    model_id, deployment_sha = validate_identity_response(response)
    return text, model_id, deployment_sha


def emit(text, model_version_id, deployment_sha256):
    print(
        "__RESULT__="
        + json.dumps(
            {
                "raw_transcript": text or "",
                "confidence": None,
                "model_version_id": model_version_id,
                "deployment_sha256": deployment_sha256,
            },
            ensure_ascii=False,
            separators=(",", ":"),
        )
    )


def emit_health(response):
    print("__HEALTH__=" + json.dumps(response, ensure_ascii=False, separators=(",", ":")))


def fail(code, msg):
    """Infrastructure failure: tell the app (stderr) and exit non-zero WITHOUT a __RESULT__ line,
    so it cancels the force-7B import rather than recording a blank transcript."""
    sys.stderr.write(msg.rstrip("\n") + "\n")
    sys.stderr.flush()
    sys.exit(code)


class ClientFailure(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


class ServerBusy(Exception):
    pass


def _request_server_once(request, timeout_seconds):
    """Send one bounded line request and return one bounded JSON object."""
    payload = (json.dumps(request, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
    deadline = time.monotonic() + timeout_seconds
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as connection:
            connection.settimeout(max(0.001, deadline - time.monotonic()))
            connection.connect((HOST, PORT))
            connection.settimeout(max(0.001, deadline - time.monotonic()))
            connection.sendall(payload)
            buf = bytearray()
            while b"\n" not in buf:
                remaining_time = deadline - time.monotonic()
                if remaining_time <= 0:
                    raise socket.timeout()
                connection.settimeout(remaining_time)
                remaining = MAX_RESPONSE_BYTES + 1 - len(buf)
                if remaining <= 0:
                    raise ClientFailure(EX_SERVER, f"7B engine reply exceeded {MAX_RESPONSE_BYTES} bytes")
                chunk = connection.recv(min(65536, remaining))
                if not chunk:
                    break
                buf.extend(chunk)
                if len(buf) > MAX_RESPONSE_BYTES:
                    raise ClientFailure(EX_SERVER, f"7B engine reply exceeded {MAX_RESPONSE_BYTES} bytes")
    except socket.timeout as exc:
        raise ClientFailure(
            EX_UNREACHABLE,
            f"7B server reachable but timed out after {timeout_seconds:g}s on {HOST}:{PORT}",
        ) from exc
    except ClientFailure:
        raise
    except (ConnectionRefusedError, OSError) as exc:
        raise ClientFailure(
            EX_UNREACHABLE,
            f"7B engine not running: cannot reach the OmniASR-7B server on {HOST}:{PORT} ({exc})",
        ) from exc

    if not buf:
        raise ClientFailure(EX_SERVER, f"7B engine returned no data from {HOST}:{PORT}")
    if b"\n" not in buf:
        raise ClientFailure(EX_SERVER, "7B engine reply was not newline-terminated")
    line, trailing = bytes(buf).split(b"\n", 1)
    if trailing.strip():
        raise ClientFailure(EX_SERVER, "7B engine sent more than one reply")
    try:
        response = json.loads(line.decode("utf-8"), object_pairs_hook=_object_without_duplicate_keys)
    except (UnicodeDecodeError, json.JSONDecodeError, DuplicateKeyError) as exc:
        raise ClientFailure(EX_SERVER, f"7B engine sent an unparseable reply: {exc}") from exc
    if not isinstance(response, dict):
        raise ClientFailure(EX_SERVER, "7B engine reply is not a JSON object")
    if response.get("code") == "BUSY":
        raise ServerBusy(str(response.get("error") or "replica busy"))
    if "error" in response:
        error = response["error"]
        if not isinstance(error, str) or not error:
            error = "unknown server error"
        raise ClientFailure(EX_SERVER, f"7B engine error: {error}")
    return response


def request_server(request, timeout_seconds):
    """Retry BUSY replicas within one total deadline; other protocol/transport failures are final."""
    deadline = time.monotonic() + timeout_seconds
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise ClientFailure(
                EX_UNREACHABLE,
                f"7B server stayed busy for {timeout_seconds:g}s on {HOST}:{PORT}",
            )
        try:
            return _request_server_once(request, remaining)
        except ServerBusy:
            time.sleep(min(0.05, max(0.0, deadline - time.monotonic())))


class ClobberedAlignment(Exception):
    """A segment's alignment_json is PRESENT but has no usable source offsets — a clobbered chunk."""


def resolve_clip_offsets(alignment):
    """Resolve the (start_ms, end_ms) source slice for a segment from its stored alignment_json.

    Returns (None, None) for a genuine WHOLE-FILE segment (no alignment metadata at all) — the server then
    transcribes the whole file, which IS the clip. Raises ClobberedAlignment when alignment is PRESENT but
    lacks usable source_start_ms/source_end_ms offsets (a bare {"words": ...} array, unparseable JSON, or only
    one of the two offsets): sending null offsets in that case would make the server transcribe the ENTIRE
    source file and store it as THIS one clip's transcript — the whole-file-vs-clip training-data corruption
    that the Rust readers slice_for_export and slice_pcm_by_alignment already REFUSE. The caller must refuse,
    not whole-file. (Import always writes source offsets, even for a chunk_count==1 single-file segment, so a
    legitimate whole-file segment carries alignment=None here, never a present-but-offset-less blob.)
    """
    if not alignment:
        return None, None
    try:
        m = json.loads(alignment)
    except Exception:
        raise ClobberedAlignment("alignment metadata is present but is not valid JSON")
    if isinstance(m, dict) and m.get("source_start_ms") is not None and m.get("source_end_ms") is not None:
        return m.get("source_start_ms"), m.get("source_end_ms")
    raise ClobberedAlignment("alignment metadata is present but has no source_start_ms/source_end_ms offsets")


def main():
    ap = argparse.ArgumentParser()
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument("--segment-id")
    mode.add_argument("--health", action="store_true", help="query the exact loaded deployment; never reads the DB")
    ap.add_argument("--stdout-only", action="store_true")
    a = ap.parse_args()

    if a.health:
        try:
            response = validate_health_response(request_server({"op": "health"}, HEALTH_TIMEOUT_SECONDS))
        except ClientFailure as exc:
            fail(exc.code, str(exc))
        except ValueError as exc:
            fail(EX_SERVER, f"7B engine identity error: {exc}")
        emit_health(response)
        return

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
    try:
        start_ms, end_ms = resolve_clip_offsets(alignment)
    except ClobberedAlignment as e:
        fail(
            EX_DB,
            f"7B client: segment {a.segment_id}: {e} (clobbered chunk metadata); refusing to transcribe it "
            f"against the whole source file rather than storing a whole-file transcript for one clip",
        )

    req = {"op": "transcribe", "audio_path": win_to_wsl(audio_path), "start_ms": start_ms, "end_ms": end_ms}
    # 280 s: below the app's own 300 s per-attempt budget (pipeline.rs) but no longer 120 s shorter —
    # the old 180 s made the client the effective (and mislabeled) timeout authority.
    try:
        response = request_server(req, TRANSCRIPTION_TIMEOUT_SECONDS)
        text, model_version_id, deployment_sha256 = validate_transcription_response(response)
    except ClientFailure as exc:
        detail = str(exc)
        if exc.code == EX_UNREACHABLE and "timed out" in detail:
            detail += (
                " - the engine may be busy or still loading. Wait and re-transcribe; "
                "do not restart a server merely because one transcription timed out."
            )
        fail(exc.code, detail)
    except ValueError as exc:
        fail(EX_SERVER, f"7B engine identity error: {exc}")

    # Reachable server, valid reply: a real transcript (possibly legitimately empty for a silent clip).
    emit(text, model_version_id, deployment_sha256)


if __name__ == "__main__":
    main()
