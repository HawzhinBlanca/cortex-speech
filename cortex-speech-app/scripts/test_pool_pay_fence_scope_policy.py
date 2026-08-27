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


if __name__ == "__main__":
    test_startup_does_not_refuse_on_a_pool_row()
    test_decision_fence_is_scoped_to_the_unpaid_second_pass()
    print("PASS: pool pay-fence scope policy")
    sys.exit(0)
