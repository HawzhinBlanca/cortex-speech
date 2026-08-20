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


if __name__ == "__main__":
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"PASS: {test.__name__}")
