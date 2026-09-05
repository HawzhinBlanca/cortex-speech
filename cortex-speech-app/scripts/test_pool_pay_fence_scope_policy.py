#!/usr/bin/env python3
"""Pool second opinions are PAID (owner canon 2026-09-04); the pay fence and its mirror are retired together.

History this gate carries. Flexible-pool decisions had no compensation contract, so
`couch/decisions.rs` refused them with `PAY_POLICY_REQUIRED` and `review_pool::pending_segment_ids`
mirrored that refusal by never serving a clip that already held one canonical opinion. Measured on the
live library 2026-09-04: 1,451 pool clips holding exactly one opinion, ZERO holding two, ten active
reviewers — the consensus canon ("a sentence is decided by any two different reviewers") was unreachable
by construction. The owner then wrote, in his own words:

    change canon: pool second opinions are paid at the same weights as first opinions
    (edit 100%, accept 10%, reject 10%)

What this gate pins now:
  1. `couch/lifecycle.rs` still never refuses startup on the mere presence of a pool row (a pool row is
     permanent; refusing there took down the PAID first-pass path with the unpaid one, 2026-08-27).
  2. `couch/decisions.rs` routes the second-opinion branch (`pool_replay || already_canonical`) to
     `api_pool_decision` in PRODUCTION — no `cfg(not(test))` 503 — and lets a known canonical
     operation fall through to its replay acknowledgement instead of being diverted.
  3. `review_pool::record_decision` mints the compensation credit in its own transaction
     (`append_review_pool_compensation_tx`) and consumes the policy-4 playback authority; the reversal
     path appends the signed inverse. Money and judgement commit together or not at all.
  4. The queue no longer carries a PAY-FENCE MIRROR: a one-opinion clip is served, because it is the
     work nearest a decision.
  5. The blinded second-pass CAMPAIGN is a different, still-unpriced contract and keeps its fence.
"""
import re
import sys
from pathlib import Path

SRC = Path(__file__).resolve().parents[1] / "src-tauri" / "src"
FENCE = "PAY_POLICY_REQUIRED"


def _read(name: str) -> str:
    """Source with `//` comments stripped, so prose about the fence never reads as code."""
    path = SRC / name
    if not path.is_file():  # a moved module must fail loudly, never silently pass
        raise AssertionError(f"{path} is missing — this gate cannot verify the pool pay contract")
    lines = []
    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.lstrip()
        if stripped.startswith("//"):
            continue
        lines.append(line.split("//")[0] if "//" in line and '"' not in line.split("//")[0] else line)
    return "\n".join(lines)


def test_startup_does_not_refuse_on_a_pool_row() -> None:
    text = _read("couch/lifecycle.rs")
    if FENCE in text:
        raise AssertionError(
            "couch/lifecycle.rs refuses startup again. A pool row is permanent, so this takes down the "
            "PAID first-pass path and stops phone review entirely."
        )
    if "review_pool::load" not in text:
        raise AssertionError("lifecycle.rs no longer loads the pool at preflight — a corrupt registry must still refuse")
    print("[OK] couch::start does not refuse on the presence of a pool row")


def test_pool_second_opinions_are_routed_in_production_and_canonical_replays_fall_through() -> None:
    text = _read("couch/decisions.rs")
    branch = text.find("if (pool_replay || already_canonical) && !canonical_replay {")
    if branch < 0:
        raise AssertionError(
            "the second-opinion routing branch must read `(pool_replay || already_canonical) && !canonical_replay`: "
            "a lost-response replay of a canonical decision arrives after its own commit made the clip canonical "
            "and must reach the canonical duplicate acknowledgement, not a 409 from the pool path"
        )
    end = text.find("let early_campaign = match active_campaign_policy(db, reviewer, state) {", branch)
    if end < 0:
        raise AssertionError("the campaign routing that follows the pool branch is gone")
    body = text[branch:end]
    if "return api_pool_decision(db, &parsed, reviewer, session_binding_sha256, state, pool);" not in body:
        raise AssertionError("the second-opinion branch must route to api_pool_decision with the session binding")
    if "cfg(not(test))" in body or FENCE in body:
        raise AssertionError(
            "the pool pay fence is back. Owner canon 2026-09-04 prices pool second opinions; refusing them "
            "again takes paid work away from reviewers and blocks consensus"
        )
    if re.search(r"#\[cfg\(test\)\]\s*pub\(super\) fn api_pool_decision", text):
        raise AssertionError("api_pool_decision must be production code, not cfg(test)")
    print("[OK] pool second opinions are routed in production; canonical replays fall through")


def test_pool_decisions_mint_compensation_and_consume_playback_authority_atomically() -> None:
    pool = _read("review_pool.rs")
    record = pool.find("pub fn record_decision(")
    if record < 0:
        raise AssertionError("review_pool::record_decision is gone")
    body = pool[record:pool.find("pub fn latest_decision(", record)]
    for needle, why in (
        ("Database::append_review_pool_compensation_tx(", "the credit is minted inside the decision transaction"),
        (
            "let Some(authority_id) = input.playback_authority_session_id else",
            "a paid pool judgement without policy-4 authority must be refused, never committed",
        ),
        (
            "consume_couch_playback_authority_for_pool_decision_on(",
            "the policy-4 authority is re-verified and consumed inside the decision transaction",
        ),
    ):
        if needle not in body:
            raise AssertionError(f"record_decision lost: {why} ({needle!r})")
    if "if let Some(authority_id) = input.playback_authority_session_id" in body:
        raise AssertionError("record_decision must not pay a non-skip judgement that carries no playback authority")
    if "tx.commit()" not in body or body.find("Database::append_review_pool_compensation_tx(") > body.rfind("tx.commit()"):
        raise AssertionError("the pool credit must be written BEFORE the decision transaction commits")
    for fn in ("pub fn reverse_decision(", "pub fn reverse_decision_addressed("):
        start = pool.find(fn)
        if start < 0:
            raise AssertionError(f"{fn} is gone")
        segment = pool[start:start + 4000]
        if "Database::append_review_pool_compensation_reversal_tx(" not in segment:
            raise AssertionError(f"{fn} no longer appends the compensation reversal — an undone pool judgement would stay paid")
    review = _read("db/review.rs")
    for needle in (
        "pub(crate) fn append_review_pool_compensation_tx(",
        "pub(crate) fn append_review_pool_compensation_reversal_tx(",
        '"couch_pool",',
        'format!("pool-decision:{pool_decision_id}")',
    ):
        if needle not in review:
            raise AssertionError(f"db/review.rs lost the pool compensation contract anchor {needle!r}")
    core = _read("db/core.rs")
    if '"canonical" | "spot_check" | "independent"' not in core or '"pool"' in core.split("fn consume_couch_playback_authority_on")[1][:600]:
        raise AssertionError(
            "the consumption namespaces are CHECK-constrained by schema 67; a pool second opinion is recorded as "
            "`independent`, never as a namespace the table would refuse"
        )
    pool_consumer = core.split("pub(crate) fn consume_couch_playback_authority_for_pool_decision_on(")
    if len(pool_consumer) != 2:
        raise AssertionError("db/core.rs lost consume_couch_playback_authority_for_pool_decision_on")
    pool_consumer_body = pool_consumer[1][:2500]
    for needle, why in (
        ("has_sufficient_desktop_playback_evidence_v4_on(", "the pool proof is re-verified against the current row"),
        ('"independent",', "the pool consumption is recorded under the CHECK-constrained independent namespace"),
        ("E_NO_PLAYBACK_EVIDENCE", "an insufficient proof is a refusal, not a silent pass"),
    ):
        if needle not in pool_consumer_body:
            raise AssertionError(f"pool authority consumer lost: {why} ({needle!r})")
    audit = core.split("let malformed_consumptions: i64 = conn.query_row(")
    if len(audit) != 2:
        raise AssertionError("db/core.rs lost the startup consumption audit")
    audit_body = audit[1][:3000]
    for needle in (
        "SELECT 1 FROM review_pool_decisions decision",
        "AND decision.action<>'skip'",
        "AND decision.served_revision=session.segment_revision",
        "AND decision.audio_content_hash=session.audio_content_hash",
    ):
        if needle not in audit_body:
            raise AssertionError(
                f"the startup audit no longer links an `independent` consumption to its exact pool decision ({needle!r}); "
                "a reopened database would either refuse every paid second opinion or accept a forged one"
            )
    restore = _read("restore_service/compensation.rs")
    for needle in (
        '} else if ledger.source == "couch_pool" {',
        '"couch_undo" | "couch_pool_undo"',
        "has no exact consumed policy-4 playback authority",
        "does not have exactly one current-policy credit",
    ):
        if needle not in restore:
            raise AssertionError(f"restore_service/compensation.rs lost the pool credit/undo validation anchor {needle!r}")
    print("[OK] pool decisions mint compensation and consume playback authority in one transaction")


def test_the_queue_serves_one_opinion_clips_again() -> None:
    pool = (SRC / "review_pool.rs").read_text(encoding="utf-8")
    if "if cfg!(not(test)) && already_canonical {" in pool:
        raise AssertionError(
            "the PAY-FENCE MIRROR is back in review_pool::pending_segment_ids — one-opinion clips would never be "
            "served and consensus could never converge (1,451 such clips measured 2026-09-04)"
        )
    if "pending.push((\n            distance_to_decision,\n            voice_priority_rank(&voice_name)," not in pool:
        raise AssertionError("decision proximity must remain the queue's first key, ahead of the owner's voice priority")
    print("[OK] the pool queue serves one-opinion clips: the mirror is gone with the fence")


def test_blinded_second_pass_campaign_is_still_fenced() -> None:
    """The campaign's blinded second pass is a DIFFERENT, still-unpriced contract: `review_campaign.rs`
    never writes `review_compensation_ledger`. It keeps its production fence until an owner prices it."""
    text = _read("couch/decisions.rs")
    branch = text.find("early_campaign.as_ref().filter(|policy| policy.is_blinded_second_pass())")
    if branch < 0:
        raise AssertionError("the blinded second-pass routing branch is gone from couch/decisions.rs")
    tail = text[branch:branch + 1500]
    if FENCE not in tail:
        raise AssertionError("the blinded second-pass branch lost its pay fence while it still pays nothing")
    if not re.search(r"err_reply\(\s*503", tail):
        raise AssertionError("the second-pass pay refusal must answer 503, the retryable operational code")
    campaign = (SRC / "review_campaign.rs").read_text(encoding="utf-8")
    if "review_compensation_ledger" in campaign and "never writes" not in campaign:
        raise AssertionError(
            "review_campaign.rs now touches review_compensation_ledger — retire this fence pin and the fence TOGETHER"
        )
    print("[OK] the blinded second-pass branch keeps its pay fence and answers 503")


if __name__ == "__main__":
    test_startup_does_not_refuse_on_a_pool_row()
    test_pool_second_opinions_are_routed_in_production_and_canonical_replays_fall_through()
    test_pool_decisions_mint_compensation_and_consume_playback_authority_atomically()
    test_the_queue_serves_one_opinion_clips_again()
    test_blinded_second_pass_campaign_is_still_fenced()
    print("PASS: pool pay contract policy")
    sys.exit(0)
