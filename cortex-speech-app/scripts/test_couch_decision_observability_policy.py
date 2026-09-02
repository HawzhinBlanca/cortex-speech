#!/usr/bin/env python3
"""Every phone decision request logs its status and wall time, and a queued clip is never re-served.

MEASURED 2026-09-02: a reviewer reported "the texts I corrected are coming back". The server had
stored every save; the diagnosis needed a join between the app log's connection errors and
review_events by timestamp, because no line said "a decision request took N ms and answered S".
The page also re-served the clip whose save had never been acknowledged, draft restored, which
read as a lost correction. Two pins keep both fixes in place; the jsdom test
tests/couch_page_outbox_hides_queued_clip.test.ts proves the page behaviour.
"""

from __future__ import annotations

from pathlib import Path

APP = Path(__file__).resolve().parents[1]
ROUTING = APP / "src-tauri" / "src" / "couch" / "routing.rs"
PAGE = APP / "src-tauri" / "assets" / "couch.html"


def test_decision_dispatch_logs_status_and_latency_without_identity() -> None:
    text = ROUTING.read_text(encoding="utf-8")
    start = text.index('(tiny_http::Method::Post, "/api/decision")')
    block = text[start : start + 1200]
    assert 'target: "cortex_speech_app_lib::couch::decision"' in block, "the decision log target is gone"
    assert "status = reply.0" in block and "elapsed_ms = started.elapsed()" in block, "status and latency must both be logged"
    assert "reviewer =" not in block and "segment" not in block.split("tracing::info!")[1], "the decision log line must carry no identity"


def test_page_keeps_queued_clips_out_of_the_batch() -> None:
    text = PAGE.read_text(encoding="utf-8")
    assert "queue = res.items.filter((s) => !queuedIds.has(s.id));" in text, (
        "load() must drop clips whose decision is still queued in this reviewer's outbox"
    )
    assert ".filter((s) => !s.reviewer || !who || s.reviewer === who)" in text, "the outbox filter must be scoped to this reviewer on a shared phone"


def main() -> None:
    test_decision_dispatch_logs_status_and_latency_without_identity()
    test_page_keeps_queued_clips_out_of_the_batch()
    print("couch decision observability policy passed")


if __name__ == "__main__":
    main()
