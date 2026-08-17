#!/usr/bin/env python3
"""Machine pins for docs/OWNER_CANON.md — the owner's approved decisions, greppably enforced.

The canon exists because approved things kept getting 'improved'. A document alone cannot stop
that; this test can: every checkable canon item is asserted against the actual source, so an agent
that silently changes one reds the sweep the same hour. Changing a pin HERE requires the same
authority as changing the canon — the owner's own `change canon: <item>`.
"""

from __future__ import annotations

import sys
from pathlib import Path

APP = Path(__file__).resolve().parent.parent
REPO = APP.parent


def _read(rel: str) -> str:
    return (APP / rel).read_text(encoding="utf-8")


def test_canon_document_exists_and_keeps_its_sections() -> None:
    canon = (REPO / "docs" / "OWNER_CANON.md").read_text(encoding="utf-8")
    for section in ("## Models", "## The verbatim law", "## Rights", "## Review operation",
                    "## Calibrated numbers", "## Working rules", "change canon:"):
        assert section in canon, f"OWNER_CANON.md lost its '{section}' section"


def test_the_calibrated_thresholds_have_not_drifted() -> None:
    d = _read("src-tauri/src/diarization.rs")
    assert "SPEAKER_CHANGE_THRESHOLD: f32 = 0.59" in d, "0.59 was owner-calibrated (blind pass, 15/15)"
    assert "SPEAKER_TURN_REFUSAL_CEILING: f32 = 0.43" in d, "0.43 is the measured turn-group ceiling"


def test_the_backup_pacing_cannot_regress_to_the_doc_example() -> None:
    db = _read("src-tauri/src/db.rs")
    assert "BACKUP_PAGES_PER_STEP" in db and "4096" in db, "backup pacing pinned (the 5/250ms default cost a 20-min cold start)"
    assert "run_to_completion(5," not in db, "the rusqlite doc-example pacing is banned by canon"


def test_the_watchdog_grace_stays_sized_to_the_measured_startup() -> None:
    wd = _read("scripts/ops/cortex-watchdog.ps1")
    assert "else { 10 }" in wd, "grace is 10 min (startup measures 6.4s; 45 was sized to a fixed bug)"


def test_spot_check_rate_and_reviewer_cap_hold() -> None:
    couch = _read("src-tauri/src/couch.rs")
    assert "SPOT_CHECK_EVERY: usize = 8" in couch, "1-in-8 spot-check rate is an owner trade-off"
    assert "MAX_REVIEWERS: usize = 8" in couch, "8 named reviewers is the approved capacity"


def test_kbhp_is_hawleri_and_the_tree_declares_dialect() -> None:
    dialect = _read("src-tauri/src/dialect.rs")
    assert '("KBHP", HAWLERI)' in dialect, "KBHP = Hawleri, owner-confirmed across all 32 episodes"
    assert "sorani-hawleri" in dialect, "the organized-tree mapping must not be dropped"


def test_no_banned_model_enters_the_stack() -> None:
    # Qwen and Voxtral were evaluated by the owner and KILLED for ckb. They must not reappear in
    # dependencies or engine configuration. Comments MAY mention them — the ban is documented in a
    # doc-comment in settings.rs itself — so only CODE is scanned, via the shared comment stripper.
    from _policy_util import strip_comments

    cargo = _read("src-tauri/Cargo.toml").lower()
    for banned in ("qwen", "voxtral"):
        assert banned not in cargo, f"'{banned}' appeared in Cargo.toml — canon forbids it for ckb"
    settings_code = strip_comments(_read("src-tauri/src/settings.rs")).lower()
    for banned in ("qwen", "voxtral"):
        assert banned not in settings_code, f"'{banned}' appeared in settings.rs CODE — canon forbids it for ckb"


def test_the_duplicate_audit_baseline_only_ratchets_down() -> None:
    audit = _read("scripts/check_dataset_duplicates.py")
    line = next(l for l in audit.splitlines() if l.startswith("KNOWN_BASELINE"))
    baseline = int(line.split("=")[1].strip())
    assert baseline <= 70, f"duplicate baseline rose to {baseline} — it may only ever go DOWN"


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for t in tests:
        t()
        print(f"  ok  {t.__name__}")
    print(f"OWNER CANON PINS: {len(tests)} pins hold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
