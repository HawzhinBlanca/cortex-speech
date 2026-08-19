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


def _round2_fixture(tmp: Path) -> None:
    """An ACTIVE focus of host cluster 1 (ids h1,h2), plus a round-2 key over clusters 10 and 17."""
    _fixture(tmp)
    (tmp / "voice_focus.json").write_text(
        json.dumps({"name": "TestVoice", "segment_ids": ["h1", "h2"]}), encoding="utf-8"
    )
    r2 = tmp / "voice_focus" / "round2"
    r2.mkdir()
    # sample order: 1:c10  2:c17  3:CONTROL(h1)  4:c10  5:c17  6:CONTROL(h2)
    key = [("t1", "cluster:10"), ("s1", "cluster:17"), ("h1", "cluster:1"),
           ("t2", "cluster:10"), ("s2", "cluster:17"), ("h2", "cluster:1")]
    (r2 / "blind_sample_KEY.txt").write_text("".join(f"{i}\t{l}\n" for i, l in key), encoding="utf-8")
    (r2 / "cluster_10_segment_ids.txt").write_text("t1\nt2\nt3\n", encoding="utf-8")
    (r2 / "cluster_17_segment_ids.txt").write_text("s1\ns2\ns3\n", encoding="utf-8")


def test_round2_merges_only_the_cluster_the_owner_confirmed_on_every_clip() -> None:
    """Cluster 10 confirmed on both clips -> merged. Cluster 17 confirmed on one of two -> rejected."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        _round2_fixture(tmp)
        r = _run(tmp, "--merge-round2", "--host", "1,4,2,3,6")  # 10: yes,yes  17: yes,no  controls: yes,yes
        assert r.returncode == 0, r.stdout + r.stderr
        focus = json.loads((tmp / "voice_focus.json").read_text(encoding="utf-8"))
        assert set(focus["segment_ids"]) == {"h1", "h2", "t1", "t2", "t3"}, focus["segment_ids"]
        assert "s1" not in focus["segment_ids"], "a half-confirmed cluster must not pollute the host's set"


def test_round2_is_void_if_the_owner_misses_a_control() -> None:
    """Calling an already-confirmed host clip 'not him' means the ear is off; nothing may merge."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        _round2_fixture(tmp)
        r = _run(tmp, "--merge-round2", "--host", "1,4,2,5,3")  # every suspect yes, but control #6 missed
        assert r.returncode == 1, r.stdout
        assert "VOID" in r.stdout
        focus = json.loads((tmp / "voice_focus.json").read_text(encoding="utf-8"))
        assert set(focus["segment_ids"]) == {"h1", "h2"}, "a void round must leave the focus untouched"


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
