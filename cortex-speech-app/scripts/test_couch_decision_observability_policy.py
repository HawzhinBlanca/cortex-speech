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


def test_audio_dispatch_logs_status_and_latency_without_identity() -> None:
    """MEASURED 2026-09-16: reviewers reported "slow" and "sometimes doesn't play". The audio route was
    the one hot path with NO log line, so a serve that took four seconds off the library's 5900-rpm disk
    looked exactly like one that took forty milliseconds, and the investigation could measure the disk
    and the caches but never the route. Same shape as the decision line, same no-identity rule."""
    text = ROUTING.read_text(encoding="utf-8")
    start = text.index('if p.starts_with("/api/audio/")')
    block = text[start : start + 2400]
    assert 'target: "cortex_speech_app_lib::couch::audio"' in block, "the audio log target is gone"
    assert "status = reply.0" in block and "elapsed_ms = started.elapsed()" in block, (
        "status and latency must both be logged for audio"
    )
    # Bounded to the macro call itself: the arms that follow legitimately name the reviewer, and a
    # slice that ran past `);` would fail on their text rather than on the log line.
    line = block.split("tracing::info!")[1].split(");")[0]
    assert "reviewer" not in line and "segment" not in line, "the audio log line must carry no identity"


def test_queue_dispatch_logs_status_and_latency_without_identity() -> None:
    """MEASURED 2026-09-16: deriving all ten reviewers' canonical queues out of process took 17.9 s, and
    the page asks for a fresh batch every 25 clips, on every reload and on its one-a-minute retry. Whether
    the in-app route costs anything like that was unknowable because nothing logged it."""
    text = ROUTING.read_text(encoding="utf-8")
    start = text.index('(tiny_http::Method::Get, "/api/queue")')
    block = text[start : start + 1600]
    assert 'target: "cortex_speech_app_lib::couch::queue"' in block, "the queue log target is gone"
    assert "status = reply.0" in block and "elapsed_ms = started.elapsed()" in block, (
        "status and latency must both be logged for the queue"
    )
    line = block.split("tracing::info!")[1].split(");")[0]
    assert "reviewer" not in line and "segment" not in line, "the queue log line must carry no identity"


def test_page_keeps_queued_clips_out_of_the_batch() -> None:
    text = PAGE.read_text(encoding="utf-8")
    assert "queue = res.items.filter((s) => !queuedIds.has(s.id));" in text, (
        "load() must drop clips whose decision is still queued in this reviewer's outbox"
    )
    assert ".filter((s) => !s.reviewer || !who || s.reviewer === who)" in text, "the outbox filter must be scoped to this reviewer on a shared phone"


def main() -> None:
    test_decision_dispatch_logs_status_and_latency_without_identity()
    test_audio_dispatch_logs_status_and_latency_without_identity()
    test_queue_dispatch_logs_status_and_latency_without_identity()
    test_page_keeps_queued_clips_out_of_the_batch()
    print("couch decision observability policy passed")


if __name__ == "__main__":
    main()
