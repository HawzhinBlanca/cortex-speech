#!/usr/bin/env python3
"""Vault and restore the reviewer pairing credentials, so no accident can permanently kill a link.

The six distributed links exist in exactly one place: `couch_session.json`. Three things can destroy
them, and only one of them survives today:

  app crash / restart      -> tokens are reused on the next start (proven live 2026-08-28 17:56Z)
  Settings -> Stop         -> STOP IS THE REVOKE: a marker is written and every token is reminted
  file loss / corruption   -> the tokens exist nowhere else; every link dies with the file

This tool closes the last two:

  vault    copy couch_session.json into link_vault/ whenever the token set has CHANGED. Runs from
           the 30-minute health probe, so a new roster is captured within half an hour of existing.
  restore  put the newest (or a named) snapshot back and delete the revocation marker, after
           verifying every token in it still decrypts. The next Start then REUSES those tokens --
           the links already in reviewers' hands keep working, because `start` matches remembered
           names case-insensitively and only mints for names it cannot find.

So the roster-change procedure that does NOT kill anyone's link is:
  Stop -> restore -> Start (with the new name added). The old names keep their tokens; only the new
  name gets a fresh one. Without the restore, Stop's revocation marker remints EVERYONE -- measured
  2026-08-28, when adding two names cost all four existing reviewers their links, including the one
  reviewer actively working.

The vault holds DPAPI-protected bytes only -- unreadable off this machine/user, same protection as
the live file. Nothing here prints a token.

  python scripts/reviewer_link_vault.py vault
  python scripts/reviewer_link_vault.py restore [--snapshot <name>] [--force-live]
  python scripts/reviewer_link_vault.py --selftest
"""

from __future__ import annotations

import argparse
import base64
import ctypes
import ctypes.wintypes
import hashlib
import json
import os
import socket
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_reviewer_links_live import default_data_dir, dpapi_unprotect  # noqa: E402

VAULT_DIRNAME = "link_vault"
COUCH_PORT = 8737


def fingerprint_set(payload: dict) -> frozenset[tuple[str, str]]:
    """{(casefolded name, sha256 of token)} -- identity of the credential set, never the credentials."""
    out = set()
    for protected, name in (payload.get("reviewers") or {}).items():
        token = dpapi_unprotect(protected)
        try:
            out.add((str(name).casefold(), hashlib.sha256(token.encode("utf-8")).hexdigest()))
        finally:
            del token
    return frozenset(out)


def load_validated(path: Path) -> tuple[dict, frozenset[tuple[str, str]]]:
    """Parse + prove every token decrypts. A snapshot that cannot decrypt cannot restore anything."""
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload.get("reviewers"), dict) or not payload["reviewers"]:
        raise ValueError(f"{path.name} holds no reviewers; refusing to treat it as a credential set")
    return payload, fingerprint_set(payload)


def newest_snapshot(vault: Path) -> Path | None:
    snaps = sorted(vault.glob("couch_session.*.json"), key=lambda p: p.stat().st_mtime)
    return snaps[-1] if snaps else None


def couch_is_serving() -> bool:
    try:
        with socket.create_connection(("127.0.0.1", COUCH_PORT), timeout=2):
            return True
    except OSError:
        return False


def cmd_vault(root: Path) -> int:
    live = root / "couch_session.json"
    vault = root / VAULT_DIRNAME
    if not live.exists():
        print("LINK VAULT: FAIL - couch_session.json is missing; there is nothing to protect")
        return 1
    try:
        _, live_fps = load_validated(live)
    except Exception as e:
        print(f"LINK VAULT: FAIL - live session file is not a valid credential set ({e})")
        return 1

    latest = newest_snapshot(vault)
    if latest is not None:
        try:
            _, latest_fps = load_validated(latest)
            if latest_fps == live_fps:
                print(f"LINK VAULT: OK - unchanged ({len(live_fps)} credential(s), latest {latest.name})")
                return 0
        except Exception:
            pass  # an unreadable latest snapshot must never block capturing a good one

    vault.mkdir(exist_ok=True)
    # Microseconds: two captures in the same second must be two files, not one silent overwrite
    # (exactly what the selftest's remint-then-vault sequence does).
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    dest = vault / f"couch_session.{stamp}.json"
    tmp = dest.with_suffix(".tmp")
    tmp.write_bytes(live.read_bytes())
    os.replace(tmp, dest)
    _, roundtrip = load_validated(dest)  # a snapshot that cannot restore is not a backup
    if roundtrip != live_fps:
        dest.unlink()
        print("LINK VAULT: FAIL - written snapshot does not match the live file")
        return 1
    print(f"LINK VAULT: OK - captured {len(live_fps)} credential(s) into {dest.name}")
    return 0


def cmd_restore(root: Path, snapshot: str | None, force_live: bool) -> int:
    vault = root / VAULT_DIRNAME
    source = vault / snapshot if snapshot else newest_snapshot(vault)
    if source is None or not source.exists():
        print("LINK RESTORE: FAIL - no snapshot to restore (run `vault` while the links work)")
        return 1
    if couch_is_serving() and not force_live:
        # Restoring under a live server is at best a no-op (start already read the file) and at
        # worst a lie: the running server keeps honouring ITS tokens, not the restored ones.
        print("LINK RESTORE: FAIL - Couch Review is serving on port 8737. Press Stop first, then restore, then Start.")
        return 1
    try:
        payload, fps = load_validated(source)
    except Exception as e:
        print(f"LINK RESTORE: FAIL - snapshot cannot restore ({e})")
        return 1

    live = root / "couch_session.json"
    if live.exists():
        keep = live.with_name(f"couch_session.pre-restore-{datetime.now(timezone.utc):%Y%m%dT%H%M%SZ}.json")
        keep.write_bytes(live.read_bytes())
    tmp = live.with_suffix(".tmp")
    tmp.write_bytes(source.read_bytes())
    os.replace(tmp, live)

    marker = root / "couch_session.revoked"
    if marker.exists():
        marker.unlink()
        print("  revocation marker removed - Stop's revoke is undone")

    names = ", ".join(sorted({str(n) for n in payload["reviewers"].values()}, key=str.casefold))
    print(f"LINK RESTORE: OK - {len(fps)} credential(s) restored from {source.name} ({names})")
    print("  press Start (same names) and every restored link keeps working; db_path:", payload.get("db_path"))
    return 0


# --- selftest ---------------------------------------------------------------------------------


class _BLOB(ctypes.Structure):
    _fields_ = [("cbData", ctypes.wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_char))]


def _dpapi_protect(secret: str) -> str:
    """Real CryptProtectData, so the selftest exercises the same decrypt path as production."""
    raw = secret.encode("utf-8")
    blob_in = _BLOB(len(raw), ctypes.cast(ctypes.create_string_buffer(raw, len(raw)), ctypes.POINTER(ctypes.c_char)))
    blob_out = _BLOB()
    if not ctypes.windll.crypt32.CryptProtectData(
        ctypes.byref(blob_in), None, None, None, None, 0, ctypes.byref(blob_out)
    ):
        raise OSError("CryptProtectData failed")
    try:
        protected = ctypes.string_at(blob_out.pbData, blob_out.cbData)
    finally:
        ctypes.windll.kernel32.LocalFree(blob_out.pbData)
    return "dpapi:" + base64.b64encode(protected).decode("ascii")


def selftest() -> int:
    import tempfile

    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        session = {
            "reviewers": {_dpapi_protect("tok-rezan"): "Rezan", _dpapi_protect("tok-aram"): "Aram"},
            "db_path": str(root / "cortex-speech.db"),
            "sessions": [],
        }
        (root / "couch_session.json").write_text(json.dumps(session), encoding="utf-8")

        assert cmd_vault(root) == 0, "first vault must capture"
        assert newest_snapshot(root / VAULT_DIRNAME) is not None, "snapshot must exist"
        assert cmd_vault(root) == 0, "unchanged vault must succeed"
        assert len(list((root / VAULT_DIRNAME).glob("*.json"))) == 1, "unchanged set must not duplicate"

        # A remint (same name, new token) must be captured as a NEW snapshot...
        session["reviewers"] = {_dpapi_protect("tok-rezan-2"): "Rezan", _dpapi_protect("tok-aram"): "Aram"}
        (root / "couch_session.json").write_text(json.dumps(session), encoding="utf-8")
        assert cmd_vault(root) == 0
        assert len(list((root / VAULT_DIRNAME).glob("couch_session.*.json"))) == 2, "changed set must snapshot"

        # ...and restore must bring back the exact bytes, undoing the revocation marker.
        (root / "couch_session.revoked").write_text("", encoding="utf-8")
        (root / "couch_session.json").unlink()
        assert cmd_restore(root, None, force_live=True) == 0, "restore must succeed"
        assert not (root / "couch_session.revoked").exists(), "restore must remove the revoke marker"
        _, restored = load_validated(root / "couch_session.json")
        assert ("rezan", hashlib.sha256(b"tok-rezan-2").hexdigest()) in restored, "restore must return the newest set"

        # An empty credential file must never become a snapshot.
        (root / "couch_session.json").write_text(json.dumps({"reviewers": {}, "sessions": []}), encoding="utf-8")
        assert cmd_vault(root) == 1, "an empty reviewers map is a failure, not a backup"

    print("SELFTEST OK")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", nargs="?", choices=["vault", "restore"])
    parser.add_argument("--snapshot", help="restore: a specific snapshot filename instead of the newest")
    parser.add_argument("--force-live", action="store_true", help="restore even while the server is serving")
    parser.add_argument("--data-dir", help="override the app data directory")
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args()

    if args.selftest:
        return selftest()
    if args.command is None:
        parser.error("command required: vault | restore (or --selftest)")
    root = Path(args.data_dir) if args.data_dir else default_data_dir()
    if args.command == "vault":
        return cmd_vault(root)
    return cmd_restore(root, args.snapshot, args.force_live)


if __name__ == "__main__":
    sys.exit(main())
