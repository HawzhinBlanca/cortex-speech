"""Policy: VERBATIM training text — machine paraphrase never serves, exports, or grades as the transcript.

Owner rule 2026-08-12: this corpus is exact audio→text for AI fine-tuning. Measured on 492 clips,
the LLM refinement REWRITES the champion's verbatim output (median 11.1% of characters, loanwords
translated, digits verbalized, punctuation invented) — fluent machine paraphrase, not correction.
Therefore the transcript any consumer treats as THE text is exactly:

    human-approved text (a real human decision)  >  human-typed annotation  >  champion raw

and NEVER `normalized_transcript` (the refined/machine-processed text) and NEVER a machine jury
verdict. Refined text and jury proposals remain stored as EVIDENCE (hypotheses, similarity
retrieval) — they just cannot masquerade as the transcript.

Static source checks, sandbox-safe:
  1. quality.rs::training_transcript_with_source — no normalized fallback, no machine-verdict rung.
  2. corrections.rs::loop0_draft_text (aligner/LOOP-0 text) — annotated ▸ raw only.
  3. segmentQuality.ts::effectiveTranscript — the frontend mirror, same order.
  4. ReviewMode.svelte::originalText — the reviewer-facing draft, annotated ▸ raw only.
  5. Flat/Hugging Face/fine-tune primary labels preserve exact stored codepoints; normalization may
     still serve as an explicitly derived field or variant-aware dedup key.
  6. No `normalizedTranscript ??` fallback anywhere in a .svelte file — the tell-tale shape of
     machine-processed text outranking the verbatim draft in a display/align chain.
"""

import re
import sys
from pathlib import Path

APP = Path(__file__).resolve().parents[1]


def balanced_body(source: str, start: int, what: str) -> str:
    depth = 0
    for i in range(start, len(source)):
        if source[i] == "{":
            depth += 1
        elif source[i] == "}":
            depth -= 1
            if depth == 0:
                return source[start:i]
    raise AssertionError(f"could not find a balanced body for {what}")


def fn_body(path: Path, pattern: str, name: str) -> str:
    source = path.read_text(encoding="utf-8")
    match = re.search(pattern, source)
    if match is None:
        raise AssertionError(f"{name} not found in {path.name} — renamed? update this policy test")
    return balanced_body(source, match.start(), name)


def main() -> int:
    failures = []

    checks = [
        (
            APP / "src-tauri" / "src" / "quality.rs",
            r"fn training_transcript_with_source",
            "training_transcript_with_source",
            ["normalized_transcript", "jury_verdict"],
        ),
        (
            APP / "src-tauri" / "src" / "corrections.rs",
            r"pub fn loop0_draft_text",
            "loop0_draft_text",
            ["normalized"],
        ),
        (
            APP / "src" / "lib" / "segmentQuality.ts",
            r"export function effectiveTranscript",
            "effectiveTranscript",
            ["normalizedTranscript"],
        ),
        (
            APP / "src" / "lib" / "ReviewMode.svelte",
            r"function originalText",
            "originalText",
            ["normalizedTranscript"],
        ),
    ]
    for path, pattern, name, banned in checks:
        body = fn_body(path, pattern, name)
        for term in banned:
            if term in body:
                failures.append(f"{path.name}::{name} references {term} — machine text must never be the transcript")

    export_source = (APP / "src-tauri" / "src" / "export.rs").read_text(encoding="utf-8")
    export_tests = (APP / "src-tauri" / "src" / "export_tests.rs").read_text(encoding="utf-8")
    eval_source = (APP / "src-tauri" / "src" / "eval.rs").read_text(encoding="utf-8")
    if "canonical_training_text(&grade.transcript)" in export_source:
        failures.append("export.rs rewrites the grade-selected primary label through canonical_training_text")
    if "canonical_training_text(&report.transcript)" in export_source:
        failures.append("export.rs rewrites the report-selected primary label through canonical_training_text")
    if "let sentence = crate::normalizer::canonical_training_text(&report.transcript)" in eval_source:
        failures.append("eval.rs fine-tune pack rewrites the grade-selected primary label")
    if "export_primary_training_labels_preserve_exact_verbatim_codepoints" not in export_tests:
        failures.append("all primary export formats need an exact-codepoint Verbatim-Law regression")
    if "finetune_pack_preserves_the_selected_verbatim_label_and_dedups_variants" not in eval_source:
        failures.append("fine-tune export needs exact-label plus variant-dedup regression evidence")

    # The tell-tale fallback shape in any display/align chain. `?? ''` is exempt: that is a plain
    # null-to-empty render of the normalized COLUMN itself (a labeled field), not machine text
    # outranking another transcript source.
    forbidden = re.compile(r"\bnormalizedTranscript\s*\?\?(?!\s*'')")
    for svelte in sorted((APP / "src").rglob("*.svelte")):
        rel = svelte.relative_to(APP)
        for n, line in enumerate(svelte.read_text(encoding="utf-8").splitlines(), 1):
            if forbidden.search(line):
                failures.append(f"{rel}:{n}: normalizedTranscript used as a fallback source: {line.strip()}")

    if failures:
        for f in failures:
            print(f"FAIL: {f}")
        print(f"VERBATIM-TRAINING-TEXT: {len(failures)} violation(s)")
        return 1
    print("VERBATIM-TRAINING-TEXT: primary labels preserve human verdict > annotation > champion raw")
    return 0


if __name__ == "__main__":
    sys.exit(main())
