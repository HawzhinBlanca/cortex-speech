"""A review page must say honestly WHOSE text sits above each play button.

`build_review_page.py` hardcoded "raw machine draft (...) -- not human-verified:". That is exactly
right for an ASR draft and exactly WRONG for an audit page, where the text under review is a human
reviewer's recorded label: calling it unverified machine output misstates its provenance and biases
the auditor about what they are being asked to judge. The label is now a parameter — which means it
can also be set to something untrue, so it needs a gate.

Pins:
  * the DEFAULT wording is byte-unchanged, so every existing page keeps its meaning;
  * the flag really substitutes, so an audit page names the human;
  * a page built for a human label carries no machine-output wording anywhere.

Regression guard: 2026-08-19, after 64.6% of the corpus was traced to one reviewer with a 1/7
spot-check record and needed a blind re-listen.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
BUILDER = REPO_ROOT / "scripts" / "build_review_page.py"
sys.path.insert(0, str(REPO_ROOT / "scripts"))

import build_review_page as page  # noqa: E402

HISTORICAL_DEFAULT = "raw machine draft ({engine}) -- not human-verified:"


def test_the_default_wording_is_unchanged() -> None:
    assert page.DEFAULT_DRAFT_LABEL == HISTORICAL_DEFAULT, (
        "changing the default silently rewrites the provenance line on every existing review page"
    )


def _build(tmp: Path, label: str | None) -> str:
    manifest = tmp / "m.jsonl"
    manifest.write_text(
        json.dumps({"segment_id": "s1", "audio": "missing.wav", "text": "دەقێک", "duration_ms": 1000})
        + "\n",
        encoding="utf-8",
    )
    out = tmp / "page.html"
    cmd = [sys.executable, str(BUILDER), "--manifest", str(manifest), "--out", str(out)]
    if label is not None:
        cmd += ["--draft-label", label]
    result = subprocess.run(cmd, capture_output=True, text=True)
    assert result.returncode == 0, result.stderr
    return out.read_text(encoding="utf-8")


def test_default_page_still_calls_a_draft_machine_output() -> None:
    with tempfile.TemporaryDirectory() as raw:
        html = _build(Path(raw), None)
        assert "raw machine draft" in html, "an ASR draft must still be labelled machine output"


def test_an_audit_page_names_the_human_and_never_says_machine_draft() -> None:
    with tempfile.TemporaryDirectory() as raw:
        html = _build(Path(raw), "LABEL RECORDED BY {engine} - judge it against the audio:")
        assert "LABEL RECORDED BY" in html, "the flag must reach the rendered page"
        assert "raw machine draft" not in html, (
            "an audit page must not describe a HUMAN's label as unverified machine output"
        )


def test_the_engine_placeholder_is_substituted_not_printed() -> None:
    with tempfile.TemporaryDirectory() as raw:
        html = _build(Path(raw), "recorded by {engine}:")
        assert "{engine}" not in html, "the placeholder leaked into the page instead of the name"


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"REVIEW PAGE PROVENANCE: {len(tests)} pins")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
