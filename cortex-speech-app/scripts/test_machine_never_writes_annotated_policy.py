"""Policy: NO machine code path may write annotated_transcript — that field is human-only, by law.

Incident 2026-08-12: the phone review server serves ``annotated_transcript`` (a human's typed
correction) before the champion draft, as it should. But machine writers had filled that field on
348 of 494 rows, so reviewers were served a stale machine paraphrase while every fresh champion
draft sat invisible in ``raw_transcript``. The data was cleaned and a live-DB gate added
(``check_review_serving_provenance.py``); THIS test kills the writers so the state cannot recur:

  1. ``db.rs::update_batch_transcription_if_unreviewed`` (the batch_transcribe persist path) must
     not touch ``annotated_transcript`` at all — it used to seed it with the LLM-refined machine
     paraphrase via ``COALESCE(annotated_transcript, ?8)``, which then outranked every later
     champion re-draft forever.
  2. No Svelte handler may assign machine ASR output into ``annotatedTranscript`` — the remaining
     production re-transcribe paths are the champion actions in App and ReviewMode.

Static source checks: sandbox-safe, no DB required.
"""

import re
import sys
from pathlib import Path

APP = Path(__file__).resolve().parents[1]


def _balanced_body(source: str, start: int, what: str) -> str:
    depth = 0
    for i in range(start, len(source)):
        if source[i] == "{":
            depth += 1
        elif source[i] == "}":
            depth -= 1
            if depth == 0:
                return source[start:i]
    raise AssertionError(f"could not find a balanced body for {what}")


def rust_function_body(source: str, name: str) -> str:
    return _balanced_body(source, source.index(f"pub fn {name}"), name)


def js_function_body(source: str, name: str) -> str:
    # Anchor on the opening parenthesis so similarly prefixed helpers cannot widen the match.
    match = re.search(rf"(?:async\s+)?function\s+{re.escape(name)}\s*\(", source)
    if match is None:
        raise AssertionError(f"function {name} not found — the policy target was renamed; update this test")
    return _balanced_body(source, match.start(), name)


def main() -> int:
    failures = []

    # 1. The batch persist path must never mention the human-only column.
    db_rs = (APP / "src-tauri" / "src" / "db.rs").read_text(encoding="utf-8")
    body = rust_function_body(db_rs, "update_batch_transcription_if_unreviewed")
    if "annotated" in body:
        failures.append(
            "db.rs::update_batch_transcription_if_unreviewed touches annotated_transcript — "
            "the batch path must write ONLY the ASR-derived columns (raw/normalized/confidence)"
        )

    # 2. The two champion re-transcribe handlers must not mention annotatedTranscript AT ALL.
    #    Human writers (submit/save/draft paths persisting the reviewer's editText) stay legal —
    #    provenance is per-function, so the check is function-scoped, not line-scoped.
    machine_handlers = {
        APP / "src" / "Workstation.svelte": ["handleTranscribe"],
        APP / "src" / "lib" / "ReviewMode.svelte": ["doRetranscribe"],
    }
    for path, names in machine_handlers.items():
        source = path.read_text(encoding="utf-8")
        rel = path.relative_to(APP)
        for name in names:
            body = js_function_body(source, name)
            if "annotatedTranscript" in body:
                failures.append(
                    f"{rel}::{name} writes annotatedTranscript — a machine re-transcription "
                    "must write raw/normalized only; the human-only field is off limits"
                )

    # 3. Belt-and-braces net for NEW handlers: the tell-tale machine shape anywhere in a .svelte.
    forbidden = re.compile(r"annotatedTranscript\s*[:=]\s*result\.\w+")
    for svelte in sorted((APP / "src").rglob("*.svelte")):
        rel = svelte.relative_to(APP)
        for n, line in enumerate(svelte.read_text(encoding="utf-8").splitlines(), 1):
            if forbidden.search(line):
                failures.append(f"{rel}:{n}: machine result assigned into annotatedTranscript: {line.strip()}")

    if failures:
        for f in failures:
            print(f"FAIL: {f}")
        print(f"MACHINE-NEVER-WRITES-ANNOTATED: {len(failures)} violation(s)")
        return 1
    print("MACHINE-NEVER-WRITES-ANNOTATED: all machine writers of the human-only field are gone")
    return 0


if __name__ == "__main__":
    sys.exit(main())
