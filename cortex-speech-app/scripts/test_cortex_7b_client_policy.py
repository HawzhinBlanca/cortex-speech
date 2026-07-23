"""Unit test for cortex_7b_client.resolve_clip_offsets — the whole-file-vs-clip guard on the DEFAULT 7B path.

The WSL-7B transcribe path (the champion/default engine) invokes cortex_7b_client.py, which reads a segment's
alignment_json from the app DB and hands source offsets to the server for clipping. A present-but-offset-less
alignment (a clobbered chunk: a bare {"words": ...} array, unparseable JSON, or only one offset) must be
REFUSED — NOT sent with null offsets. Sending null offsets makes the server transcribe the ENTIRE source file
and store it as THIS one clip's transcript: whole-file-vs-clip training-data corruption. This mirrors the Rust
readers slice_for_export and slice_pcm_by_alignment, which already refuse the same shape. A genuine whole-file
segment carries NO alignment (import always writes source offsets, even for a chunk_count==1 single-file
segment), so refusing present-but-offset-less never rejects legitimate data.

Fail-before: reverting resolve_clip_offsets to the old `m.get(...)`-then-whole-file logic makes the
offset-less / partial / unparseable cases return (None, None), and the asserts below fire.
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cortex_7b_client import ClobberedAlignment, resolve_clip_offsets  # noqa: E402


def test_resolve_clip_offsets() -> None:
    # No alignment at all -> a genuine whole-file segment; whole-file is correct.
    assert resolve_clip_offsets(None) == (None, None)
    assert resolve_clip_offsets("") == (None, None)

    # Valid chunk offsets -> clip to that window.
    assert resolve_clip_offsets(json.dumps({"source_start_ms": 1000, "source_end_ms": 2500})) == (1000, 2500)
    # Offsets can coexist with a merged words array (the normal post-alignment shape).
    both = json.dumps({"source_start_ms": 0, "source_end_ms": 500, "words": [{"word": "x"}]})
    assert resolve_clip_offsets(both) == (0, 500)

    # Present-but-offset-less (clobbered chunk) -> REFUSE, never whole-file.
    for bad in (
        json.dumps([{"word": "x", "start": 0.0, "end": 1.0}]),  # bare word array
        json.dumps({"words": []}),                               # object without offsets
        json.dumps({"source_start_ms": 100}),                    # only start, no end
        json.dumps({"source_end_ms": 500}),                      # only end, no start
        "{not valid json",                                       # unparseable
    ):
        try:
            resolve_clip_offsets(bad)
        except ClobberedAlignment:
            pass
        else:
            raise AssertionError(f"resolve_clip_offsets must REFUSE a present-but-offset-less alignment: {bad!r}")


def main() -> None:
    test_resolve_clip_offsets()
    print("cortex_7b_client offset-guard policy passed")


if __name__ == "__main__":
    main()
