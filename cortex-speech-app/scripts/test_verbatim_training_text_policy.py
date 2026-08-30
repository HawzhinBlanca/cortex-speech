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
  2. quality.rs::effective_transcript — public product/export projection stays on that same law.
  3. corrections.rs::loop0_draft_text (aligner/LOOP-0 text) — annotated ▸ raw only.
  4. transcribe.rs response/alignment projections — annotated ▸ champion raw only.
  5. reviewTranscriptAuthority.ts::reviewTranscript — the shared frontend authority.
  6. segmentQuality.ts::effectiveTranscript — the frontend mirror, same order.
  7. ReviewMode.svelte::originalText — the reviewer-facing draft delegates to that authority.
  8. jury/learning.rs::export_lm_corpus — primary LM labels are emitted byte-exact, not normalized.
  9. segmentQuality.ts::hasRealTranscript — normalized-only evidence cannot mint shippable content.
 10. No `normalizedTranscript ??` fallback anywhere in a .svelte file — the tell-tale shape of
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
            APP / "src-tauri" / "src" / "quality.rs",
            r"pub fn effective_transcript",
            "effective_transcript",
            ["normalized_transcript"],
        ),
        (
            APP / "src-tauri" / "src" / "corrections.rs",
            r"pub fn loop0_draft_text",
            "loop0_draft_text",
            ["normalized"],
        ),
        (
            APP / "src-tauri" / "src" / "commands" / "transcribe.rs",
            r"fn machine_review_text",
            "machine_review_text",
            ["normalized_transcript"],
        ),
        (
            APP / "src-tauri" / "src" / "commands" / "transcribe.rs",
            r"fn prospective_champion_review_text",
            "prospective_champion_review_text",
            ["normalized_transcript"],
        ),
        (
            APP / "src" / "lib" / "reviewTranscriptAuthority.ts",
            r"export function reviewTranscript",
            "reviewTranscript",
            ["normalizedTranscript"],
        ),
        (
            APP / "src" / "lib" / "segmentQuality.ts",
            r"export function effectiveTranscript",
            "effectiveTranscript",
            ["normalizedTranscript"],
        ),
        (
            APP / "src" / "lib" / "segmentQuality.ts",
            r"export function hasRealTranscript",
            "hasRealTranscript",
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

    learning_path = APP / "src-tauri" / "src" / "jury" / "learning.rs"
    learning_source = learning_path.read_text(encoding="utf-8")
    lm_body = fn_body(learning_path, r"pub fn export_lm_corpus", "export_lm_corpus")
    for term in ("SoraniNormalizer", "canonical_training_text", "normalizer.normalize", "to_nfc("):
        if term in lm_body:
            failures.append(
                f"learning.rs::export_lm_corpus references {term} — a primary LM label must remain byte-exact"
            )
    if not re.search(
        r"DpoPair\s*\{[^}]*chosen:\s*row\.human_fix\s*,\s*rejected:\s*row\.wrong_transcript",
        learning_source,
        re.DOTALL,
    ):
        failures.append(
            "learning.rs DPO emission no longer serializes the stored human/machine pair directly — verify no trim/canonicalization"
        )

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

    # The old normalized-only setter had zero production callers but remained a compiled public
    # mutation path. It may stay as a storage fixture only while cfg(test) removes it from production.
    segments_source = (APP / "src-tauri" / "src" / "db" / "segments.rs").read_text(encoding="utf-8")
    if "fn update_normalized_transcript" in segments_source and not re.search(
        r"#\[cfg\(test\)\]\s*pub\(crate\)\s+fn update_normalized_transcript",
        segments_source,
    ):
        failures.append(
            "db/segments.rs::update_normalized_transcript is compiled outside cfg(test) — normalized-only truth mutation is forbidden"
        )

    jobs_source = (APP / "src-tauri" / "src" / "db" / "jobs_rights.rs").read_text(encoding="utf-8")
    if "fn update_segment_consensus_batch" in jobs_source and not re.search(
        r"#\[cfg\(test\)\]\s*pub\s+fn update_segment_consensus_batch",
        jobs_source,
    ):
        failures.append(
            "db/jobs_rights.rs::update_segment_consensus_batch is compiled outside cfg(test) — machine consensus cannot replace champion raw"
        )

    # `stats.rs` keeps an O(1) SQL mirror instead of calling the Rust helper row-by-row. Pin the
    # mirror itself so dashboard counts cannot drift back to machine-refined text while unit tests
    # happen to exercise only rows whose raw and normalized values match.
    stats_source = (APP / "src-tauri" / "src" / "stats.rs").read_text(encoding="utf-8")
    effective_sql = re.search(r'const EFFECTIVE: &str = "(.*?)";', stats_source, re.DOTALL)
    if effective_sql is None:
        failures.append("stats.rs::EFFECTIVE SQL mirror not found — renamed? update this policy test")
    elif "normalized_transcript" in effective_sql.group(1):
        failures.append("stats.rs::EFFECTIVE references normalized_transcript — dashboard authority violates Verbatim Law")

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
    print("VERBATIM-TRAINING-TEXT: transcript precedence is human verdict > annotation > champion raw everywhere")
    return 0


if __name__ == "__main__":
    sys.exit(main())
