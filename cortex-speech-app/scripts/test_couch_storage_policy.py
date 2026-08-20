#!/usr/bin/env python3
"""Transcript drafts must not live indefinitely on a shared review phone."""

from pathlib import Path


def test_transcript_drafts_are_session_scoped() -> None:
    source = (Path(__file__).parents[1] / "src-tauri" / "assets" / "couch.html").read_text(encoding="utf-8")
    assert "sessionStorage.setItem(draftKey(" in source
    assert "sessionStorage.getItem(draftKey(" in source
    assert "sessionStorage.removeItem(draftKey(" in source
    assert "localStorage.setItem(draftKey(" not in source
    assert "localStorage.getItem(draftKey(" not in source


def test_outbox_never_flushes_stamped_work_under_an_unknown_identity() -> None:
    """The outbox is per-origin, not per-reviewer: on a shared phone two reviewers share one queue.

    The author guard used to be `item.reviewer && me && ...` — disabled exactly when `me` was still
    '' — and the first flush of every page load runs before the queue response sets `me`. So the one
    flush guaranteed to happen on a shared phone was the one that attributed a colleague's offline
    decisions to whichever cookie loaded first (audit fix 2026-08-20). Stamped work must wait until
    the server names this reviewer, and must then actually be sent.
    """
    source = (Path(__file__).parents[1] / "src-tauri" / "assets" / "couch.html").read_text(encoding="utf-8")
    assert "if (item.reviewer && item.reviewer !== me) continue;" in source, (
        "the outbox author guard must hold stamped items whenever they are not provably mine"
    )
    assert "if (item.reviewer && me && item.reviewer !== me) continue;" not in source, (
        "`me &&` disables the author guard exactly when the identity is unknown"
    )
    idx_me = source.find("me = res.reviewer;")
    assert idx_me != -1, "load() must record who the server says this link belongs to"
    assert "flushOutbox()" in source[idx_me : idx_me + 1800], (
        "once identity is known, load() must re-flush so the held stamped work still lands"
    )


def test_a_tab_that_changes_hands_clears_the_previous_reviewers_drafts() -> None:
    """sessionStorage survives navigating from one reviewer's link to another in the same tab, and a
    saved draft OVERRIDES the served text — so reviewer B was shown, and could save, reviewer A's
    half-typed correction as their own (2026-08-20 hunt). An identity change must clear the drafts."""
    source = (Path(__file__).parents[1] / "src-tauri" / "assets" / "couch.html").read_text(encoding="utf-8")
    assert "sessionStorage.getItem('cortex.couch.who')" in source, "load() must remember whose tab this is"
    idx = source.find("prevWho !== null && prevWho !== me")
    assert idx != -1, "an identity CHANGE (not first load) must trigger the draft sweep"
    assert "startsWith('cortex.couch.draft.')" in source[idx : idx + 600], "the sweep removes every per-clip draft"


def test_the_attribution_fence_holds_work_instead_of_deleting_it() -> None:
    """The server 409s a mismatched-author submit so the work can be HELD for whoever made it; the
    page treated every 4xx as final and deleted the verdict — the exact loss the fence exists to
    prevent (2026-08-20 hunt). Both the outbox flush and the live decide path must hold, not drop."""
    source = (Path(__file__).parents[1] / "src-tauri" / "assets" / "couch.html").read_text(encoding="utf-8")
    holds = source.count("/was made by/.test(e.message || '')")
    assert holds >= 2, f"expected the attribution-409 hold on both the flush and decide paths, found {holds}"


def test_cookie_sessions_survive_a_restart() -> None:
    """A reviewer's session must outlive the app restart, or their saved shortcut becomes a dead end.

    MEASURED 2026-08-20: the pairing tokens were durable but the cookie sessions were memory-only, so
    every restart (4-9 a DAY on the owner's machine, 48 in nine days) made the server forget every
    session it had issued. The browser still held a cookie valid for its full 24 h Max-Age, got 401,
    and the page showed the terminal "link expired" - for a link that was fine. Recovery was
    impossible from the page itself: it strips `#t=` from the address bar after claiming, so a
    bookmark or the installed PWA (start_url "/") carries no token to re-claim with. Six of eight
    paid reviewers went silent across nine days and nothing in the log said why.

    Each anchor is a load-bearing half of the fix; losing any one silently restores the amnesia.
    """
    source = (Path(__file__).parents[1] / "src-tauri" / "src" / "couch.rs").read_text(encoding="utf-8")
    required = {
        "sessions are persisted": "struct SavedCookieSession",
        "and restored on resume": "session_issued,",
        "a fresh claim is durable immediately": "persist_session_state(state);",
        "expiry uses the wall clock, which is the only clock that crosses a restart": "issued_unix",
        "a refused reviewer is visible in the log": "Couch Review refused an unauthenticated request",
    }
    for why, needle in required.items():
        assert needle in source, f"couch.rs lost the anchor for: {why} ({needle!r})"
    assert "session_issued: HashMap<String, SystemTime>" in source, (
        "session issue times must be wall-clock; an Instant cannot survive the restart it is saved for"
    )


if __name__ == "__main__":
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"PASS: {test.__name__}")
