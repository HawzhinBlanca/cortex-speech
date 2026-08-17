"""Shared helpers for the policy tests. Not a test itself — the `test_*.py` glob skips this file."""

from __future__ import annotations

import re


def strip_comments(text: str) -> str:
    """Drop /* */, // and <!-- --> comments.

    Policy tests search source for a pattern that must (or must not) appear. Without this, the
    strongest form of a defensive comment — one that NAMES the API it forbids — makes its own gate
    fail forever. Measured 2026-08-15: `open_audio_file`'s comment explains why `blocking_pick_file`
    must not be used, and the gate read that as a call to it.
    """
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.DOTALL)
    text = re.sub(r"<!--.*?-->", " ", text, flags=re.DOTALL)
    return "\n".join(line.split("//", 1)[0] for line in text.splitlines())


if __name__ == "__main__":
    assert strip_comments("a // b\nc") == "a \nc"
    assert strip_comments("/* x */keep") == " keep"
    assert strip_comments("<!-- x -->keep") == " keep"
    assert "blocking_pick_file" not in strip_comments("// never call blocking_pick_file here")
    print("PASS: _policy_util")
