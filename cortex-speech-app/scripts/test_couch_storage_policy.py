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


if __name__ == "__main__":
    test_transcript_drafts_are_session_scoped()
    print("PASS: Couch transcript drafts are session-scoped")
