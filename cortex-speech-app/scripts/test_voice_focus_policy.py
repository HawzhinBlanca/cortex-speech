"""The voice-focus activator must refuse a weak verdict, and must never carry a name into tracked code.

`activate_voice_focus.py` turns the owner's blind-listen judgement into a live queue filter for eight
paid reviewers. A focus activated on a cluster that disagrees with the owner's ear points all of them
at the wrong person's clips. So the activator scores the verdict against the key and REFUSES past one
disagreement — and this pins that it actually does, plus that the scoring is not vacuous (a perfect
verdict activates), and that the tracked sources carry no speaker name.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "activate_voice_focus.py"
TRACKED = [
    REPO_ROOT / "scripts" / "activate_voice_focus.py",
    REPO_ROOT / "src-tauri" / "src" / "voice_focus.rs",
    REPO_ROOT / "src-tauri" / "src" / "bin" / "host_voice_probe.rs",
]


def _fixture(tmp: Path) -> Path:
    out = tmp / "voice_focus"
    out.mkdir()
    # 6-clip key: 1,2,4,5 candidate; 3,6 other.
    key = [("a", "CANDIDATE"), ("b", "CANDIDATE"), ("c", "other"), ("d", "CANDIDATE"), ("e", "CANDIDATE"), ("f", "other")]
    (out / "blind_sample_KEY.txt").write_text("".join(f"{i}\t{l}\n" for i, l in key), encoding="utf-8")
    (out / "candidate_segment_ids.txt").write_text("a\nb\nd\ne\nx\ny\n", encoding="utf-8")
    return tmp


def _run(tmp: Path, *extra: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--data-dir", str(tmp), *extra], capture_output=True, text=True
    )


def test_a_perfect_verdict_activates_the_focus() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = _fixture(Path(raw))
        r = _run(tmp, "--name", "TestVoice", "--host", "1,2,4,5")
        assert r.returncode == 0, r.stdout + r.stderr
        focus = json.loads((tmp / "voice_focus.json").read_text(encoding="utf-8"))
        assert focus["name"] == "TestVoice"
        assert set(focus["segment_ids"]) == {"a", "b", "d", "e", "x", "y"}, "the WHOLE candidate cluster is focused"


def test_one_disagreement_is_tolerated() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = _fixture(Path(raw))
        r = _run(tmp, "--name", "TestVoice", "--host", "1,2,4")  # missed #5
        assert r.returncode == 0, r.stdout
        assert (tmp / "voice_focus.json").is_file()


def test_two_disagreements_are_refused_and_nothing_is_written() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = _fixture(Path(raw))
        r = _run(tmp, "--name", "TestVoice", "--host", "1,2,3")  # missed 4,5 and called 3 host
        assert r.returncode == 1, "a cluster the owner's ear disagrees with must not become the queue"
        assert "REFUSED" in r.stdout
        assert not (tmp / "voice_focus.json").is_file(), "a refused verdict must write no focus"


def test_deactivate_retires_rather_than_deletes() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = _fixture(Path(raw))
        assert _run(tmp, "--name", "TestVoice", "--host", "1,2,4,5").returncode == 0
        r = _run(tmp, "--deactivate")
        assert r.returncode == 0
        assert not (tmp / "voice_focus.json").is_file()
        assert list(tmp.glob("voice_focus.retired-*.json")), "the old focus is kept as history, not deleted"


def test_tracked_sources_carry_no_speaker_name() -> None:
    """The name is the owner's data, not the repo's. Only generic words may appear in code."""
    for path in TRACKED:
        text = path.read_text(encoding="utf-8")
        for forbidden in ("Kawa", "KBHP"):
            assert forbidden not in text, f"{path.name} names a real person or private source ({forbidden!r})"


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"VOICE FOCUS POLICY: {len(tests)} pins")
    return 0


if __name__ == "__main__":
    sys.exit(main())
