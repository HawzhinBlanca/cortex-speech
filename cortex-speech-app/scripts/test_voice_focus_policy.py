"""The voice-focus activator must refuse a weak verdict, and must never carry a name into tracked code.

`activate_voice_focus.py` turns the owner's blind-listen judgement into a live queue filter for eight
paid reviewers. A focus activated on a cluster that disagrees with the owner's ear points all of them
at the wrong person's clips. So the activator scores the verdict against the key and REFUSES past one
disagreement — and this pins that it actually does, plus that the scoring is not vacuous (a perfect
verdict activates), and that the tracked sources carry no speaker name.
"""

from __future__ import annotations

import json
import hashlib
import sqlite3
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


def test_an_empty_candidate_list_is_refused_not_activated() -> None:
    """The server treats a focus naming no ids as BROKEN and 503s every queue (fail-closed,
    2026-08-20). The activator must refuse to write that file, however perfect the verdict."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = _fixture(Path(raw))
        (tmp / "voice_focus" / "candidate_segment_ids.txt").write_text("", encoding="utf-8")
        r = _run(tmp, "--name", "TestVoice", "--host", "1,2,4,5")  # a PERFECT verdict
        assert r.returncode == 1, "zero candidates must refuse: activating would 503 every queue"
        assert not (tmp / "voice_focus.json").is_file(), "and write nothing"


def _import_job_fixture(tmp: Path) -> tuple[str, str, str]:
    """A completed two-file champion import plus an unrelated existing focus."""
    source = tmp / "lamo-final" / "wavs"
    source.mkdir(parents=True)
    champion_id = "omniasr-7b-test"
    # Deliberately STALE. champion.json is the app's startup mirror, rewritten on every launch, so in
    # the register-first/restart-second window it names a model the registry no longer champions. The
    # merge must resolve the champion from model_versions below; a mirror that agreed with the
    # registry could not tell the two sources apart.
    (tmp / "champion.json").write_text(
        json.dumps({"champions": {"omniasr-7b": {"modelVersionId": "omniasr-7b-stale-mirror"}}}),
        encoding="utf-8",
    )
    (tmp / "voice_focus.json").write_text(
        json.dumps({"name": "Existing", "segment_ids": ["old-a", "old-b"]}, indent=2), encoding="utf-8"
    )
    con = sqlite3.connect(tmp / "cortex-speech.db")
    con.executescript(
        """
        CREATE TABLE import_jobs(id TEXT PRIMARY KEY, dir TEXT, total_files INTEGER, status TEXT);
        CREATE TABLE import_job_files(job_id TEXT, path TEXT, PRIMARY KEY(job_id,path));
        CREATE TABLE speech_segments(
          id TEXT PRIMARY KEY, audio_path TEXT, raw_transcript TEXT, verified INTEGER,
          human_decision TEXT, reviewed_by TEXT, cloud_call INTEGER, model_version_id TEXT
        );
        CREATE TABLE segment_hypotheses(segment_id TEXT, model_version_id TEXT, transcript TEXT);
        CREATE TABLE review_events(segment_id TEXT);
        CREATE TABLE model_versions(
          id TEXT PRIMARY KEY, family TEXT, checkpoint_sha256 TEXT, status TEXT
        );
        """
    )
    con.execute(
        "INSERT INTO model_versions VALUES(?,'omniasr-7b',?,'champion')", (champion_id, "c" * 64)
    )
    job = "job-1"
    con.execute("INSERT INTO import_jobs VALUES(?,?,2,'completed')", (job, str(source)))
    selected = []
    for idx in range(2):
        path = source / f"clip-{idx}.wav"
        path.write_bytes(b"RIFF")
        seg_id = f"new-{idx}"
        text = f"text {idx}"
        selected.append(seg_id)
        con.execute("INSERT INTO import_job_files VALUES(?,?)", (job, str(path)))
        con.execute("INSERT INTO speech_segments VALUES(?,?,?,0,'','',0,?)", (seg_id, str(path), text, champion_id))
        con.execute("INSERT INTO segment_hypotheses VALUES(?,?,?)", (seg_id, champion_id, text))
    con.commit()
    con.close()
    digest = hashlib.sha256(("\n".join(sorted(selected)) + "\n").encode()).hexdigest()
    current_sha = hashlib.sha256((tmp / "voice_focus.json").read_bytes()).hexdigest()
    return job, digest, current_sha


def _merge_job_args(tmp: Path, job: str, digest: str, current_sha: str, *extra: str) -> list[str]:
    return [
        "--merge-import-job", job,
        "--label", "Lamo",
        "--expected-count", "2",
        "--expected-source-dir", str(tmp / "lamo-final" / "wavs"),
        "--expected-selection-sha256", digest,
        "--expected-current-sha256", current_sha,
        *extra,
    ]


def test_import_job_merge_is_additive_atomic_and_recoverable() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        job, digest, current_sha = _import_job_fixture(tmp)
        dry = _run(tmp, *_merge_job_args(tmp, job, digest, current_sha, "--dry-run"))
        assert dry.returncode == 0, dry.stdout + dry.stderr
        assert not list(tmp.glob("voice_focus.pre-import-merge-*.json")), "dry-run writes nothing"
        assert set(json.loads((tmp / "voice_focus.json").read_text())["segment_ids"]) == {"old-a", "old-b"}

        applied = _run(tmp, *_merge_job_args(tmp, job, digest, current_sha))
        assert applied.returncode == 0, applied.stdout + applied.stderr
        focus = json.loads((tmp / "voice_focus.json").read_text(encoding="utf-8"))
        assert focus["segment_ids"] == ["new-0", "new-1", "old-a", "old-b"]
        backups = list(tmp.glob("voice_focus.pre-import-merge-*.json"))
        assert len(backups) == 1
        assert set(json.loads(backups[0].read_text())["segment_ids"]) == {"old-a", "old-b"}


def test_import_job_merge_refuses_stale_focus_and_nonchampion_rows() -> None:
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        job, digest, current_sha = _import_job_fixture(tmp)
        stale = _run(tmp, *_merge_job_args(tmp, job, digest, "0" * 64))
        assert stale.returncode == 1 and "SHA-256 changed" in stale.stdout
        con = sqlite3.connect(tmp / "cortex-speech.db")
        con.execute("UPDATE speech_segments SET cloud_call=1 WHERE id='new-0'")
        con.commit()
        con.close()
        bad = _run(tmp, *_merge_job_args(tmp, job, digest, current_sha))
        assert bad.returncode == 1 and "matching local champion" in bad.stdout
        assert not list(tmp.glob("voice_focus.pre-import-merge-*.json"))


def test_import_job_merge_fails_closed_when_the_registry_has_no_champion() -> None:
    """The registry, not the startup mirror, says who the champion is — and an unresolvable registry
    refuses instead of certifying drafts against whatever champion.json happens to still name."""
    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        job, digest, current_sha = _import_job_fixture(tmp)
        con = sqlite3.connect(tmp / "cortex-speech.db")
        con.execute("UPDATE model_versions SET status='rolled_back'")
        con.commit()
        con.close()
        r = _run(tmp, *_merge_job_args(tmp, job, digest, current_sha))
        assert r.returncode == 1 and "exactly one omniasr-7b champion" in r.stdout, r.stdout + r.stderr
        assert not list(tmp.glob("voice_focus.pre-import-merge-*.json"))


def test_tracked_sources_carry_no_speaker_name() -> None:
    """The name is the owner's data, not the repo's. Only generic words may appear in code."""
    for path in TRACKED:
        text = path.read_text(encoding="utf-8")
        for forbidden in ("Kawa", "KBHP"):
            assert forbidden not in text, f"{path.name} names a real person or private source ({forbidden!r})"


def test_every_review_queue_serving_path_applies_the_focus() -> None:
    """The focus must narrow EVERY queue that serves a reviewer clips, or reviewers hear the guests
    the file exists to skip. Found live 2026-08-20: the couch (phone) path was narrowed, the desktop
    review page was not — the owner heard guests on desktop. One greppable anchor per layer:

      * couch.rs      — the phone queue passes the focus into its pending query;
      * db.rs         — the desktop page query joins the allow-list in SQL (json_each);
      * commands.rs   — the desktop command fails CLOSED on a present-but-broken file, like couch;
      * ReviewMode    — the desktop review queue actually ASKS for the focus (curate must not).
    """
    src = REPO_ROOT / "src-tauri" / "src"
    anchors = [
        # Every queue resolves the policy through ONE function, fail-closed and pre-worded. Spelling
        # the three-arm match out per call site is what let the desktop miss it in the first place.
        (src / "voice_focus.rs", "pub fn resolve("),
        (src / "voice_focus.rs", "POLICY_BROKEN_PREFIX"),
        (src / "couch.rs", "crate::voice_focus::resolve(dir.as_deref())"),
        (src / "commands.rs", "crate::voice_focus::resolve(dir.as_deref())"),
        # The Inbox serves the escalation queue: it plays clips, mints receipts and records verdicts,
        # so it is a serving path and the focus governs it (review 2026-08-20 — narrowing the review
        # page alone still left the Inbox handing out guests).
        (src / "commands" / "agentic.rs", "crate::voice_focus::resolve(dir.as_deref())"),
        (src / "db.rs", "id IN (SELECT value FROM json_each("),
        (REPO_ROOT / "src" / "lib" / "ReviewMode.svelte", "focused: true"),
        # A narrowed queue must SAY it is narrowed, and must never claim the library is finished.
        (REPO_ROOT / "src" / "lib" / "ReviewMode.svelte", "subsetScoped"),
        (src / "db.rs", "pub focus_narrowed: bool"),
    ]
    for path, needle in anchors:
        assert needle in path.read_text(encoding="utf-8"), f"{path.name} lost its focus anchor: {needle!r}"
    review = (REPO_ROOT / "src" / "lib" / "ReviewMode.svelte").read_text(encoding="utf-8")
    assert "allReviewed: !subsetScoped" in review, (
        "the completion banner must exclude EVERY subset, not just a search — a drained focus queue "
        "announcing the whole library as reviewed is a false completion claim"
    )
    curate = (REPO_ROOT / "src" / "lib" / "stores" / "segmentStore.ts").read_text(encoding="utf-8")
    assert "focused: true" not in curate, "the library/curate store must stay UNFOCUSED (the queue narrows, the library does not)"


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"VOICE FOCUS POLICY: {len(tests)} pins")
    return 0


if __name__ == "__main__":
    sys.exit(main())
