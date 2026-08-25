#!/usr/bin/env python3
"""The background alignment path must use the REAL aligner and report the REAL quality.

Why this is a gate and not a comment. `pipeline::enqueue_background_alignments` runs on a detached
`move` thread that never captures `self`, so the model root was not in scope — and the path was wired
to a free `aligner::align()` helper that returned `fallback_align` unconditionally and had no channel
to report `AlignmentQuality` at all. The call site therefore hardcoded `EnergyHeuristic`.

Measured consequence on the owner's live library before the fix: 129 of 144 segments stamped
`energy_heuristic` (the background path) against 15 `ctc_forced` (the foreground path) — on a machine
where `mms_aligner.onnx` is installed and forced alignment demonstrably works, and where `quality.rs`
raises a review-risk reason on exactly `energy_heuristic`. So ~90% of the corpus carried heuristic word
timings AND a false risk flag.

Nothing in the Rust test suite can catch a regression here: the persist happens inside a spawned thread
in a private method. The invariant is structural, so it is guarded structurally — the same way
test_real_data_runner_policy.py guards the recursive-delete guards in the e2e harness.
"""
import re
from pathlib import Path

APP_ROOT = Path(__file__).resolve().parents[1]
PIPELINE = APP_ROOT / "src-tauri" / "src" / "pipeline.rs"
ALIGNER = APP_ROOT / "src-tauri" / "src" / "aligner.rs"


def _background_block(text: str) -> str:
    """The body of enqueue_background_alignments, so assertions cannot match some other call site."""
    start = text.find("fn enqueue_background_alignments")
    if start < 0:
        raise AssertionError("enqueue_background_alignments not found — did it get renamed?")
    nxt = text.find("\n    fn ", start + 1)
    end = nxt if nxt > 0 else len(text)
    return text[start:end]


def test_background_alignment_uses_the_real_aligner() -> None:
    block = _background_block(PIPELINE.read_text(encoding="utf-8"))

    if "crate::aligner::align(" in block:
        raise AssertionError(
            "background alignment calls the free aligner::align() helper, which ignores any loaded "
            "model and can only ever produce the energy heuristic"
        )
    if "aligner.align(" not in block:
        raise AssertionError(
            "background alignment must align through a ForcedAligner instance (aligner.align(...))"
        )
    # Built once, outside the per-file loop: ForcedAligner::new loads a ~365 MB ONNX session, so
    # constructing it per segment would make an import unusable.
    if "ForcedAligner::new" not in block:
        raise AssertionError("background alignment must construct a ForcedAligner for the run")
    ctor = block.index("ForcedAligner::new")
    loop = block.index("for (audio_path, jobs) in by_path")
    if ctor > loop:
        raise AssertionError("ForcedAligner must be built ONCE before the per-file loop, not per segment")
    print("[OK]  background alignment aligns through a ForcedAligner built once per run")


def test_background_alignment_persists_the_reported_quality() -> None:
    block = _background_block(PIPELINE.read_text(encoding="utf-8"))
    if re.search(r"AlignmentQuality::\w+\.as_db_str\(\)", block):
        raise AssertionError(
            "background alignment persists a HARDCODED AlignmentQuality. It must persist the quality "
            "the aligner actually achieved — a constant here is a provenance lie, and quality.rs "
            "raises a review-risk reason on energy_heuristic."
        )
    if "quality.as_db_str()" not in block:
        raise AssertionError("background alignment must persist quality.as_db_str() from the aligner")
    print("[OK]  background alignment persists the quality the aligner reported")


def test_background_alignment_is_serialized_and_revision_guarded() -> None:
    block = _background_block(PIPELINE.read_text(encoding="utf-8"))
    if "import_writes.update_alignment_if_unchanged(" not in block:
        raise AssertionError(
            "background alignment must persist through ImportWriteStore with the source alignment "
            "as a compare-and-swap guard"
        )
    for forbidden in ("Database::open(", "db.update_segment_alignment("):
        if forbidden in block:
            raise AssertionError(f"background alignment regained an unguarded raw writer: {forbidden}")
    if "canonical alignment changed during inference" not in block:
        raise AssertionError("a stale background alignment must be reported rather than silently clobbered")
    print("[OK]  background alignment uses serialized revision-CAS persistence")


def test_no_fallback_only_free_aligner_helpers() -> None:
    """A free helper that silently ignores the model is a trap, not a convenience — it sprang once."""
    text = ALIGNER.read_text(encoding="utf-8")
    code = [ln for ln in text.splitlines() if not ln.strip().startswith("//")]
    for sig in ("pub fn align(", "pub fn score_consistency("):
        hits = [ln for ln in code if ln.startswith(sig)]  # module level only, not indented methods
        if hits:
            raise AssertionError(
                f"module-level `{sig}` is back in aligner.rs. The previous one returned "
                "fallback_align/-5.0 unconditionally; use the ForcedAligner methods instead."
            )
    print("[OK]  no module-level fallback-only align/score_consistency helpers")


def main() -> None:
    test_background_alignment_uses_the_real_aligner()
    test_background_alignment_persists_the_reported_quality()
    test_background_alignment_is_serialized_and_revision_guarded()
    test_no_fallback_only_free_aligner_helpers()
    print("background alignment policy regression passed")


if __name__ == "__main__":
    main()
