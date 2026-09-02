#!/usr/bin/env python3
"""The flexible-pool pay fence must refuse the UNPAID branch, never the whole review server.

Flexible-pool decisions have no owner-approved compensation contract: `review_pool::record_decision`
never writes `review_compensation_ledger`, so serving them would take playback-evidenced work for
free. Refusing them is correct and must stay.

What must NOT come back is the blast radius. The fence was originally placed twice — at
`couch::start` preflight (`if configured_pool.is_some() { return Err(PAY_POLICY_REQUIRED...) }`) and
at the top of the decision route before any routing ran. A pool registry row is PERMANENT once
activated and the owner's live library carries one, so between them they disabled ALL phone review on
every build from that branch — including the FIRST-pass canonical path, which IS paid under
review-iqd-v1-2026-08-21 and, measured on the live database 2026-08-27, is the only path any reviewer
has ever used (372 credits, ~5,299 IQD, zero pool decisions in the corpus's history).

This gate pins the scope, not the wording:
  1. `couch/lifecycle.rs` must not refuse startup on the mere presence of a pool.
  2. `couch/decisions.rs` must still carry the pay refusal, and it must sit INSIDE the
     second-pass branch (`pool_replay || already_canonical`), not before it.
"""
import re
import sys
from pathlib import Path

SRC = Path(__file__).resolve().parents[1] / "src-tauri" / "src" / "couch"
FENCE = "PAY_POLICY_REQUIRED"


def _read(name: str) -> str:
    """Source with `//` comments stripped.

    Both modules EXPLAIN this fence in prose, so a raw substring search finds the constant in a
    comment and reds on code that is correct. Scan what the compiler sees, not what the reader does.
    """
    path = SRC / name
    if not path.is_file():  # a moved module must fail loudly, never silently pass
        raise AssertionError(f"{path} is missing — this gate cannot verify the pay fence")
    lines = []
    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.lstrip()
        if stripped.startswith("//"):
            continue
        lines.append(line.split("//")[0] if "//" in line and '"' not in line.split("//")[0] else line)
    return "\n".join(lines)


def test_startup_does_not_refuse_on_a_pool_row() -> None:
    text = _read("lifecycle.rs")
    if FENCE in text:
        raise AssertionError(
            "couch/lifecycle.rs refuses startup again. A pool row is permanent, so this takes down the "
            "PAID first-pass path with the unpaid pool one and stops phone review entirely. Refuse the "
            "pool decision branch in decisions.rs instead."
        )
    if "review_pool::load" not in text:
        raise AssertionError("lifecycle.rs no longer loads the pool at preflight — a corrupt registry must still refuse")
    print("[OK] couch::start does not refuse on the presence of a pool row")


def test_decision_fence_is_scoped_to_the_unpaid_second_pass() -> None:
    text = _read("decisions.rs")
    if FENCE not in text:
        raise AssertionError(
            "the pay fence vanished from couch/decisions.rs — flexible-pool decisions are unpaid "
            "(review_pool::record_decision never writes review_compensation_ledger) and must stay refused"
        )
    branch = text.find("if pool_replay || already_canonical {")
    if branch < 0:
        raise AssertionError("the second-pass routing branch (`pool_replay || already_canonical`) is gone")
    fence_at = text.find(FENCE)
    if fence_at < branch:
        raise AssertionError(
            "the pay fence sits BEFORE the second-pass routing in couch/decisions.rs, so it refuses every "
            "decision including the paid first pass. It must live inside `pool_replay || already_canonical`."
        )
    # The refusal must be a 503 (retryable), not a hard failure that looks like reviewer error.
    tail = text[branch:]
    if not re.search(r"err_reply\(\s*503", tail):
        raise AssertionError("the scoped pay refusal must answer 503, the retryable operational code")
    print("[OK] the pay fence is scoped to the unpaid second-pass branch and answers 503")


def test_blinded_second_pass_campaign_is_also_fenced() -> None:
    """The campaign's blinded second pass had the SAME defect with NO fence at all: it demands full
    playback evidence, then records only `independent_review_decisions` — `review_campaign.rs` never
    writes `review_compensation_ledger` — so activating a second pass would take evidenced work for
    free with every request answering 200 (2026-08-30 audit). Pin the fence inside the
    `is_blinded_second_pass` branch, production-only (cfg(test) keeps the lifecycle tests on the real
    recording path)."""
    text = _read("decisions.rs")
    # The ROUTING branch specifically — `is_blinded_second_pass` also appears in the recorder's own
    # revalidation and in the undo route, and the first raw find() landed there instead.
    branch = text.find("early_campaign.as_ref().filter(|policy| policy.is_blinded_second_pass())")
    if branch < 0:
        raise AssertionError("the blinded second-pass routing branch is gone from couch/decisions.rs")
    tail = text[branch:branch + 1500]
    if FENCE not in tail:
        raise AssertionError(
            "the blinded second-pass branch lost its pay fence — record_independent_decision never "
            "writes review_compensation_ledger, so serving it takes playback-evidenced work for free"
        )
    if not re.search(r"err_reply\(\s*503", tail):
        raise AssertionError("the second-pass pay refusal must answer 503, the retryable operational code")
    campaign = (SRC.parent / "review_campaign.rs").read_text(encoding="utf-8")
    if "review_compensation_ledger" in campaign and "never writes" not in campaign:
        raise AssertionError(
            "review_campaign.rs now touches review_compensation_ledger — if a second-pass pay contract "
            "landed, this fence pin and the fence itself must be retired TOGETHER, deliberately"
        )
    print("[OK] the blinded second-pass branch carries the pay fence and answers 503")


def test_the_queue_never_serves_what_the_fence_refuses() -> None:
    """The fence alone is not enough: on 2026-08-31 the queue's decision-first ordering put the three
    OLDEST already-canonical pool clips at every reviewer's position 1, the fence 503'd their saves,
    and skip routes into the same fence — all ten reviewers were walled out of a 19,905-clip savable
    backlog on the reviewers' first day. Served work whose save is refused is a contradiction: the
    queue must mirror the fence, exactly as wide (cfg!(not(test)), so the pool tests that decide via
    `api_pool_decision` still see these clips served). When an owner pay contract prices pool work,
    the fence and this mirror must be lifted TOGETHER."""
    pool = (SRC.parent / "review_pool.rs").read_text(encoding="utf-8")
    if "PAY-FENCE MIRROR" not in pool:
        raise AssertionError(
            "review_pool.rs lost the queue-side pay-fence mirror — already-canonical pool clips will "
            "be served again while their saves answer 503, walling reviewers at the queue front"
        )
    if "if cfg!(not(test)) && already_canonical {" not in pool:
        raise AssertionError(
            "the queue mirror must be exactly as wide as the fence: production-only via cfg!(not(test)), "
            "keyed on the same already-canonical predicate the fence refuses"
        )
    print("[OK] the pool queue never serves a clip whose save the pay fence refuses")


if __name__ == "__main__":
    test_startup_does_not_refuse_on_a_pool_row()
    test_decision_fence_is_scoped_to_the_unpaid_second_pass()
    test_blinded_second_pass_campaign_is_also_fenced()
    test_the_queue_never_serves_what_the_fence_refuses()
    print("PASS: pool pay-fence scope policy")
    sys.exit(0)
