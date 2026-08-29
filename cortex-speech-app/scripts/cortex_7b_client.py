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
import shutil
import subprocess
import tempfile
import time
import stat
from pathlib import Path

HOST = os.environ.get("CORTEX_7B_HOST", "127.0.0.1")
PORT = int(os.environ.get("CORTEX_7B_PORT", "8799"))
MAX_RESPONSE_BYTES = 1024 * 1024
MAX_POINTER_BYTES = 1024 * 1024
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


class SegmentSnapshotError(RuntimeError):
    """The live SQLite database could not be copied into a trustworthy read snapshot."""


def read_segment_from_snapshot(db, segment_id, attempts=3, retry_base_seconds=0.2):
    """Read immutable clip inputs from a private DB+WAL snapshot without touching the live DB.

    The DB and WAL cannot be copied atomically while the app is writing. A copy/open failure is
    therefore retried as an infrastructure error, never silently downgraded to a main-file-only
    read. A missing row is also retried while a WAL existed before or after that snapshot, covering
    checkpoint races that can yield a valid-looking but incomplete DB/WAL pair.
    """
    if attempts < 1:
        raise ValueError("snapshot attempts must be positive")

    last_error = None
    for attempt in range(attempts):
        tmpdir = None
        try:
            tmpdir = tempfile.mkdtemp(prefix="cortex7b_")
            tmp = os.path.join(tmpdir, "snap.db")
            wal = db + "-wal"
            wal_was_present = os.path.exists(wal)
            # Observe WAL state before copying the main file. If the requested committed row is
            # already authoritative and no WAL exists here, it must be represented in this main DB.
            shutil.copyfile(db, tmp)
            if wal_was_present:
                # Never continue with only the main file after observing a WAL. That can turn a
                # transient copy/checkpoint race into a false EX_NOSEG for a committed import.
                shutil.copyfile(wal, tmp + "-wal")

            con = sqlite3.connect(f"file:{tmp}?mode=ro", uri=True)
            try:
                row = con.execute(
                    "SELECT audio_path, alignment_json FROM speech_segments WHERE id=?", (segment_id,)
                ).fetchone()
            finally:
                con.close()

            last_error = None
            if row is not None:
                return row
            if not wal_was_present and not os.path.exists(wal):
                return None
            # The row may have landed in a WAL while the independently copied files crossed a
            # checkpoint boundary. Retain an infrastructure error across retries: absence is only
            # provable from a snapshot where no WAL exists before or after the read.
            last_error = RuntimeError("segment absent from a WAL-backed database snapshot")
        except Exception as exc:
            last_error = exc
        finally:
            if tmpdir is not None:
                shutil.rmtree(tmpdir, ignore_errors=True)

        if attempt + 1 < attempts:
            time.sleep(retry_base_seconds * (attempt + 1))

    if last_error is not None:
        raise SegmentSnapshotError(str(last_error)) from last_error
    return None


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


def load_expected_champion_identity(pointer_path):
    """Read the registry pointer as a small, unambiguous identity authority.

    Health validation already proves that the listener speaks the Cortex protocol and reports a
    complete deployment identity.  That is not enough for startup: a stale, otherwise valid
    champion can still own the port.  Bind the listener to the exact model/deployment selected by
    the current registry pointer without hashing the multi-gigabyte model on every health probe.

    The serving process independently verifies the manifest and every component before it binds.
    Here we deliberately read only the pointer's immutable identity fields, using the same size,
    duplicate-key, schema and key-shape rules needed to make that comparison unambiguous.
    """
    path = Path(pointer_path)
    try:
        flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_CLOEXEC", 0)
        # A FIFO passed through the environment must not block startup before fstat can reject it.
        flags |= getattr(os, "O_NONBLOCK", 0)
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as handle:
            if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                raise ValueError("champion pointer is not a regular file")
            # The limit is enforced on bytes read from the already-open handle. A pre-read stat()
            # followed by read_bytes() has a replacement race that can turn a small checked file
            # into an unbounded allocation.
            raw = handle.read(MAX_POINTER_BYTES + 1)
    except OSError as exc:
        raise ValueError("champion pointer is unavailable") from exc
    if not raw or len(raw) > MAX_POINTER_BYTES:
        raise ValueError(
            f"champion pointer must be a non-empty file no larger than {MAX_POINTER_BYTES} bytes"
        )
    try:
        pointer = json.loads(raw.decode("utf-8"), object_pairs_hook=_object_without_duplicate_keys)
    except (UnicodeDecodeError, json.JSONDecodeError, DuplicateKeyError) as exc:
        raise ValueError(f"champion pointer is not unambiguous UTF-8 JSON: {exc}") from exc
    if not isinstance(pointer, dict) or set(pointer) != {"schema", "champions"}:
        raise ValueError("champion pointer must contain exactly schema and champions")
    if type(pointer["schema"]) is not int or pointer["schema"] != 2:
        raise ValueError(f"unsupported champion pointer schema {pointer['schema']!r}")
    champions = pointer["champions"]
    if not isinstance(champions, dict):
        raise ValueError("champion pointer champions must be an object")
    entry = champions.get(FAMILY)
    if not isinstance(entry, dict):
        raise ValueError(f"champion pointer has no {FAMILY} entry")
    required = {"modelVersionId", "deploymentManifestPath", "deploymentSha256"}
    allowed = required | {"source", "license"}
    if not required.issubset(entry) or not set(entry).issubset(allowed):
        raise ValueError(
            f"champion pointer {FAMILY} entry must contain exactly the required identity fields"
        )
    model_id = entry["modelVersionId"]
    manifest_path = entry["deploymentManifestPath"]
    deployment_sha = entry["deploymentSha256"]
    if (
        not isinstance(model_id, str)
        or not model_id.strip()
        or any(ord(char) < 32 or ord(char) == 127 for char in model_id)
    ):
        raise ValueError("champion pointer modelVersionId must be non-empty text")
    if (
        not isinstance(manifest_path, str)
        or not manifest_path.strip()
        or any(ord(char) < 32 or ord(char) == 127 for char in manifest_path)
    ):
        raise ValueError("champion pointer deploymentManifestPath must be non-empty text")
    if not _is_sha256(deployment_sha):
        raise ValueError("champion pointer deploymentSha256 is not a canonical SHA-256")
    return model_id, deployment_sha


def validate_health_against_pointer(response, pointer_path):
    """Reject a valid-but-stale listener that is not the pointer-selected champion."""
    expected_model_id, expected_deployment_sha = load_expected_champion_identity(pointer_path)
    if response["modelVersionId"] != expected_model_id:
        raise ValueError(
            "listener modelVersionId does not match the current champion pointer "
            f"({response['modelVersionId']!r} != {expected_model_id!r})"
        )
    if response["deploymentSha256"] != expected_deployment_sha:
        raise ValueError(
            "listener deploymentSha256 does not match the current champion pointer "
            f"({response['deploymentSha256']} != {expected_deployment_sha})"
        )
    return response


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
    ap.add_argument(
        "--expected-pointer",
        help="with --health, require the listener to match this exact champion registry pointer",
    )
    ap.add_argument("--stdout-only", action="store_true")
    a = ap.parse_args()

    if a.expected_pointer and not a.health:
        ap.error("--expected-pointer requires --health")

    if a.health:
        try:
            response = validate_health_response(request_server({"op": "health"}, HEALTH_TIMEOUT_SECONDS))
            if a.expected_pointer:
                response = validate_health_against_pointer(response, a.expected_pointer)
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
    # to a private temp file and read that — no lock on the live DB, and SQLite applies the WAL
    # so we see fresh segments. (audio_path + alignment never change once a segment exists.)
    # The app writes the WAL while we copy, so a single-shot copy can be torn (page copied mid-write)
    # and the open fails — which previously escalated a transient blip into an EX_DB infra-failure.
    # Retry the whole snapshot: a torn copy is momentary. Deliberately do NOT copy -shm — the shared-
    # memory index is only valid for the live connections that created it; a copied one makes SQLite
    # refuse the database outright ("unable to open database file", reproduced on this machine), while
    # a fresh read of db + -wal rebuilds it automatically.
    try:
        row = read_segment_from_snapshot(db, a.segment_id)
    except SegmentSnapshotError:
        fail(EX_DB, "7B client: could not create a consistent app DB snapshot after three attempts")
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
