#!/usr/bin/env python3
"""Does the link each reviewer ALREADY HOLDS still work? (not: does the current token work?)

`check_reviewer_links_live.py` reads the pairing token out of `couch_session.json` and proves that
token authenticates. That is a different claim from the one that matters, and the gap is not
theoretical -- it hid a real outage:

  Alle, Pavel and Roza were dropped from the roster and re-added (2026-08-22..24). `start` mints a
  FRESH token for a name it does not find in the remembered session, so all three links already sent
  died instantly. The live gate stayed GREEN through all of it -- it was checking the new tokens,
  which of course authenticate. Alle's phone showed "link expired"; the record showed OK.

So this gate holds a BASELINE of the fingerprint of each distributed token and reds when one moves.
A moved fingerprint means: that person is holding a dead link and cannot report it, because the only
thing they can see is the same "link expired" page a network blip produces.

  python scripts/check_reviewer_link_continuity.py            # compare against the baseline
  python scripts/check_reviewer_link_continuity.py --accept    # after RE-SENDING, adopt live as truth
  python scripts/check_reviewer_link_continuity.py --selftest  # logic check, no live state

NEVER PRINTS A TOKEN. It compares SHA-256 fingerprints; the baseline stores only those. A fingerprint
is not a credential, but the token it came from is, so the token never leaves this process.

READ-ONLY except for the baseline file, and even that is only written under --accept or when
seeding a baseline that does not exist yet.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_reviewer_links_live import default_data_dir, dpapi_unprotect  # noqa: E402

BASELINE_NAME = "reviewer_link_fingerprints.json"


def fingerprint(token: str) -> str:
    """Identity of a token without the token. Full digest: a prefix invites a birthday argument."""
    return hashlib.sha256(token.encode("utf-8")).hexdigest()


def live_fingerprints(session_path: Path) -> dict[str, str]:
    """reviewer name -> fingerprint of the pairing token the server would honour right now."""
    payload = json.loads(session_path.read_text(encoding="utf-8"))
    reviewers = payload.get("reviewers")
    if not isinstance(reviewers, dict):
        raise ValueError("couch_session.json has no reviewers map")
    out: dict[str, str] = {}
    for protected, name in reviewers.items():
        if not isinstance(name, str):
            raise ValueError("reviewer name is not a string")
        token = dpapi_unprotect(protected)
        try:
            out[name] = fingerprint(token)
        finally:
            del token
    return out


def drift(baseline: dict[str, str], live: dict[str, str]) -> tuple[list[str], list[str], list[str]]:
    """(reminted, unknown, dropped) -- pure, so it is testable without DPAPI or a live app.

    Names match case-insensitively because `same_reviewer` in couch.rs does: the roster preserves the
    casing the owner typed, and treating "rubar" and "Rubar" as two people is exactly the bug that
    minted the fresh tokens in the first place.
    """
    base_ci = {k.casefold(): v for k, v in baseline.items()}
    live_ci = {k.casefold(): v for k, v in live.items()}
    reminted = sorted(n for n, f in live.items() if n.casefold() in base_ci and base_ci[n.casefold()] != f)
    unknown = sorted(n for n in live if n.casefold() not in base_ci)
    dropped = sorted(n for n in baseline if n.casefold() not in live_ci)
    return reminted, unknown, dropped


def selftest() -> int:
    same = {"Rubar": "a" * 64}
    assert drift(same, same) == ([], [], []), "a stable roster must be silent"
    # The real incident: casing preserved, token replaced.
    assert drift({"Alle": "a" * 64}, {"alle": "b" * 64})[0] == ["alle"], "a reminted token must be caught"
    # Casing alone is NOT a remint -- flagging it would cry wolf on every roster retype.
    assert drift({"Alle": "a" * 64}, {"alle": "a" * 64}) == ([], [], []), "casing is not identity"
    assert drift({}, {"Pavel": "c" * 64})[1] == ["Pavel"], "a new reviewer needs a first link"
    assert drift({"Sewa": "d" * 64}, {})[2] == ["Sewa"], "a dropped reviewer must be reported"
    assert fingerprint("tok") != "tok", "the fingerprint must not be the token"
    print("SELFTEST OK")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--accept", action="store_true", help="adopt the live tokens as the distributed ones")
    parser.add_argument("--selftest", action="store_true", help="check the comparison logic only")
    parser.add_argument("--data-dir", help="override the app data directory")
    args = parser.parse_args()

    if args.selftest:
        return selftest()

    root = Path(args.data_dir) if args.data_dir else default_data_dir()
    session_path = root / "couch_session.json"
    baseline_path = root / BASELINE_NAME

    if not session_path.exists():
        print(f"LINK CONTINUITY: FAIL - {session_path.name} is missing; Couch Review has no paired reviewers")
        return 1
    try:
        live = live_fingerprints(session_path)
    except Exception as e:  # a credential we cannot read is a red gate, never a pass
        print(f"LINK CONTINUITY: FAIL - could not read the live pairing tokens ({e})")
        return 1

    stored: dict[str, str] = {}
    if baseline_path.exists():
        try:
            payload = json.loads(baseline_path.read_text(encoding="utf-8"))
            stored = {k: v for k, v in (payload.get("fingerprints") or {}).items() if isinstance(v, str)}
        except Exception as e:
            print(f"LINK CONTINUITY: FAIL - the baseline is unreadable ({e}); re-accept it deliberately")
            return 1

    if args.accept or not baseline_path.exists():
        verb = "accepted" if args.accept else "seeded"
        payload = {
            "note": "SHA-256 of the pairing token last known to be IN a reviewer's hands. Not a credential.",
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "fingerprints": live,
        }
        tmp = baseline_path.with_suffix(".tmp")
        tmp.write_text(json.dumps(payload, indent=2, sort_keys=True), encoding="utf-8")
        os.replace(tmp, baseline_path)
        print(f"LINK CONTINUITY: baseline {verb} for {len(live)} reviewer(s) - {', '.join(sorted(live))}")
        if not args.accept:
            print("  (first run: this assumes every current link has actually been delivered)")
        return 0

    reminted, unknown, dropped = drift(stored, live)

    for name in sorted(live):
        if name in reminted:
            print(f"  {name:8} REMINTED - the link they hold is DEAD; re-send it")
        elif name in unknown:
            print(f"  {name:8} NEW - has never been sent a link from this baseline")
        else:
            print(f"  {name:8} unchanged - the link they hold still works")
    for name in dropped:
        print(f"  {name:8} no longer on the roster - their link is dead by design")

    if reminted or unknown:
        stale = ", ".join(reminted + unknown)
        print(f"LINK CONTINUITY: FAIL - {stale} cannot get in with the link they have. Re-send, then --accept.")
        return 1
    print(f"LINK CONTINUITY: OK - all {len(live)} distributed link(s) still authenticate")
    return 0


if __name__ == "__main__":
    sys.exit(main())
