"""P1.3b restore-pending RESERVATION gate — source policy.

`AppState::writers_active` is the FENCE: db_restore refuses while a writer is ALREADY running. But that
is check-then-act — a NEW writer could start between prepare_restore's writers_active() check and the
page swap. The RESERVATION closes that window: prepare_restore sets RESTORE_PENDING (via a RAII
RestoreReservation held across the whole restore), and EVERY writer-start refuses while restore_pending().

This policy pins the invariant at every writer-start site so a future writer cannot silently reopen the
window (the same "an added writer forgets the guard" class the P1.3 fence + BgDbWriterGuard closed on the
fence side). Deterministic source scan — no global-flag mutation, so it can't flake concurrent tests.
"""

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SRC = REPO_ROOT / "src-tauri" / "src"


def _read(rel: str) -> str:
    return (SRC / rel).read_text(encoding="utf-8")


def _fn_body(src: str, signature_start: str, span: int = 1400) -> str:
    idx = src.find(signature_start)
    if idx == -1:
        raise AssertionError(f"could not find `{signature_start}` — restore-reservation gate cannot verify it")
    return src[idx : idx + span]


def test_prepare_restore_reserves_before_the_fence_and_returns_the_guard() -> None:
    body = _fn_body(_read("commands.rs"), "fn prepare_restore(")
    if "-> Result<RestoreReservation, String>" not in body:
        raise AssertionError("prepare_restore must return a RestoreReservation the caller holds across the restore")
    reserve = body.find("RestoreReservation::new()")
    fence = body.find("state.writers_active()")
    if reserve == -1 or fence == -1 or not (reserve < fence):
        raise AssertionError(
            "prepare_restore must RESERVE (RestoreReservation::new) BEFORE checking writers_active(), so a "
            "writer racing the check observes RESTORE_PENDING and refuses (the reservation drops on the "
            "early-return if a writer is already active)."
        )


def test_both_restore_callers_hold_the_reservation() -> None:
    commands = _read("commands.rs")
    # Not `prepare_restore(&state)?;` bare (which would drop the guard immediately) — it must be bound.
    if "let _restore_reservation = prepare_restore(&state)?;" not in commands:
        raise AssertionError(
            "db_restore / restore_db_from_snapshot must bind the RestoreReservation (let _restore_reservation "
            "= prepare_restore(...)?) so RESTORE_PENDING stays set across the swap; a bare prepare_restore()? "
            "drops it immediately and reopens the window."
        )
    if "prepare_restore(&state)?;\n" in commands.replace("let _restore_reservation = prepare_restore(&state)?;\n", ""):
        raise AssertionError("a restore path calls prepare_restore without binding the reservation (guard dropped early)")


def _assert_guarded(src: str, signature_start: str, who: str, token: str = "restore_pending()") -> None:
    body = _fn_body(src, signature_start)
    if token not in body:
        raise AssertionError(
            f"{who} does not check {token} before starting — a NEW writer can begin mid-restore and mix "
            f"pre-restore rows into the just-restored library. Add `if {token} {{ return Err(...); }}`."
        )


def test_every_writer_start_checks_restore_pending() -> None:
    lib = _read("lib.rs")
    _assert_guarded(lib, "pub fn try_start_import(", "try_start_import")
    _assert_guarded(lib, "pub fn try_start_batch(", "try_start_batch")

    commands = _read("commands.rs")
    _assert_guarded(commands, 'RATE_LIMITER.check("run_wsl_refinement")', "run_wsl_refinement (7B refine)")

    jury = _read("commands/jury.rs")
    _assert_guarded(jury, "pub async fn add_scribe_votes(", "add_scribe_votes", "super::restore_pending()")
    _assert_guarded(jury, "pub async fn run_dpo_update(", "run_dpo_update", "super::restore_pending()")
    _assert_guarded(jury, "pub async fn run_jury_pipeline(", "run_jury_pipeline", "super::restore_pending()")
    _assert_guarded(jury, "pub async fn run_t2_for_segment(", "run_t2_for_segment", "super::restore_pending()")

    couch = _read("couch.rs")
    # `start_on_port`, NOT `start`. `start` is a one-line delegate that injects the production port;
    # the guard must stay in the function that takes the COUCH lock, because the whole argument for it
    # being airtight is that the check and the `*guard = Some(handle)` register are serialized by the
    # SAME mutex the restore fence reads. Hoisting it into `start` would satisfy a naive scan while
    # moving the check OUTSIDE the lock and reopening the race this gate exists to close.
    _assert_guarded(couch, "fn start_on_port(", "couch::start_on_port", "restore_pending()")
    if "start_on_port(db_path, reviewers, COUCH_PORT)" not in couch:
        raise AssertionError(
            "couch::start must delegate to start_on_port so the guarded path is the only way in; if it "
            "grew its own body, this gate is checking a function the app no longer calls."
        )


def main() -> None:
    test_prepare_restore_reserves_before_the_fence_and_returns_the_guard()
    test_both_restore_callers_hold_the_reservation()
    test_every_writer_start_checks_restore_pending()
    print("restore-reservation gate source policy passed")


if __name__ == "__main__":
    main()
