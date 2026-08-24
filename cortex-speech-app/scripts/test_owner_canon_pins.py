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


def test_review_compensation_v2_and_lamo_scope_are_pinned_in_canon() -> None:
    """The owner explicitly changed this canon on 2026-08-21; drift needs another change-canon order."""
    canon = (REPO / "docs" / "OWNER_CANON.md").read_text(encoding="utf-8")
    required = [
        "review-iqd-v1-2026-08-21",
        "18,000 IQD per full-equivalent audio hour",
        "edit = 100%",
        "accept = 10%",
        "reject = 10%",
        "skip = 0%",
        "durable semantic action",
        "`reviewed_audio_ms` is activity, not money",
        "corrected-audio projection counts retained human edits only",
        "`SUM(review_compensation_ledger.delta_micro_iqd)`",
        "prospective",
        "idempotency key",
        "explicit reversal or adjustment",
        "6,922 final Lamo ids",
        "1,352-id focus",
        "8,274-id union",
        "fail closed",
    ]
    missing = [rule for rule in required if rule not in canon]
    assert not missing, f"review compensation/Lamo owner canon drifted: missing {missing}"


def test_champion_only_production_canon_is_pinned() -> None:
    canon = (REPO / "docs" / "OWNER_CANON.md").read_text(encoding="utf-8")
    required = [
        "sole main, default and production ASR",
        "not release prerequisites",
        "never selected automatically",
        "never fallbacks for WSL7B",
        "ElevenLabs Scribe is not a dependency or production feature",
        "client, commands, key, consent",
        "lifecycle supervision is off",
    ]
    missing = [rule for rule in required if rule not in canon]
    assert not missing, f"champion-only owner canon drifted: missing {missing}"


def test_compensation_code_constants_match_owner_canon() -> None:
    db = _read("src-tauri/src/db.rs")
    migrations = _read("src-tauri/src/migrations/mod.rs")
    pins = [
        'REVIEW_PAY_POLICY_VERSION: &str = "review-iqd-v1-2026-08-21"',
        "REVIEW_PAY_BASE_RATE_MICRO_IQD_PER_HOUR: i64 = 18_000_000_000",
        "REVIEW_PAY_EDIT_BPS: i64 = 10_000",
        "REVIEW_PAY_ACCEPT_BPS: i64 = 1_000",
        "REVIEW_PAY_REJECT_BPS: i64 = 1_000",
        "REVIEW_PAY_SKIP_BPS: i64 = 0",
    ]
    missing = [pin for pin in pins if pin not in db]
    assert not missing, f"compiled compensation constants drifted from canon: {missing}"
    assert "CREATE TABLE review_compensation_ledger" in migrations
    assert "review compensation ledger is append-only" in migrations


def test_the_calibrated_thresholds_have_not_drifted() -> None:
    d = _read("src-tauri/src/diarization.rs")
    assert "SPEAKER_CHANGE_THRESHOLD: f32 = 0.59" in d, "0.59 was owner-calibrated (blind pass, 15/15)"
    assert "SPEAKER_TURN_REFUSAL_CEILING: f32 = 0.43" in d, "0.43 is the measured turn-group ceiling"


def test_the_backup_pacing_cannot_regress_to_the_doc_example() -> None:
    db = _read("src-tauri/src/db.rs")
    assert "BACKUP_PAGES_PER_STEP" in db and "4096" in db, "backup pacing pinned (the 5/250ms default cost a 20-min cold start)"
    assert "run_to_completion(5," not in db, "the rusqlite doc-example pacing is banned by canon"


def test_periodic_snapshots_have_a_real_ten_minute_rpo_margin_without_cadence_drift() -> None:
    desktop = _read("src-tauri/src/lib.rs")
    required = [
        "const SNAPSHOT_TARGET_RPO_SECS: u64 = 10 * 60;",
        "const SNAPSHOT_CAPTURE_JITTER_MARGIN_SECS: u64 = 60;",
        "SNAPSHOT_TARGET_RPO_SECS - SNAPSHOT_CAPTURE_JITTER_MARGIN_SECS",
        "next_snapshot_deadline(deadline, interval, Instant::now())",
    ]
    missing = [pin for pin in required if pin not in desktop]
    assert not missing, f"periodic snapshot RPO/cadence safety regressed: {missing}"
    assert "sleep(std::time::Duration::from_secs(SNAPSHOT_INTERVAL_SECS))" not in desktop, (
        "snapshot cadence must advance from a monotonic deadline, not sleep after backup completion"
    )


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
    # 170 = the corrected 2026-08-17 measurement (rules A+B; the first 70 undercounted because the
    # mp4's shifted clock defeated offset matching — the owner's ears found the difference). The
    # cleanup lowers this toward 0; RAISING it is indistinguishable from waving a fresh duplicate
    # import through, so it needs the owner's `change canon:` and a matching edit here.
    audit = _read("scripts/check_dataset_duplicates.py")
    line = next(l for l in audit.splitlines() if l.startswith("KNOWN_BASELINE"))
    baseline = int(line.split("=")[1].strip())
    assert baseline <= 0, f"duplicate baseline rose to {baseline} — it may only ever go DOWN"


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for t in tests:
        t()
        print(f"  ok  {t.__name__}")
    print(f"OWNER CANON PINS: {len(tests)} pins hold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
