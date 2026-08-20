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
    assert "flushOutbox()" in source[idx_me : idx_me + 600], (
        "once identity is known, load() must re-flush so the held stamped work still lands"
    )


if __name__ == "__main__":
    test_transcript_drafts_are_session_scoped()
    test_outbox_never_flushes_stamped_work_under_an_unknown_identity()
    print("PASS: Couch transcript drafts are session-scoped")
    print("PASS: Couch outbox fails closed on identity")
