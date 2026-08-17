# Text-provenance audit — 2026-08-12

Nine parallel read-only auditors traced every path that serves, exports, grades, or writes
transcript text; every claimed defect was then independently adversarially verified (a verifier was
prompted to REFUTE it against the real code). **27 defects confirmed, 6 claims refuted and dropped.**
Trigger: the incident where 348 review clips served stale machine text from `annotated_transcript`
(see PROGRESS_LEDGER.md iterations 269–270).

Status letters: **F** = fixed in iteration 270 (pending rebuild/deploy) · **O** = open.

| # | St | Sev | Site | Defect (one line) |
|---|----|-----|------|-------------------|
| 0 | F | critical | `src-tauri/src/db.rs:1106` | update_batch_transcription_if_unreviewed still machine-writes the by-law human-only annotated_transcript: it seeds COALESCE(annotated_transcript, ?8) with draft.final_text — the LLM-refined machine paraphrase when ref... |
| 5 | F | critical | `cortex-speech-app/src/App.svelte:1129` | All four curate re-transcribe handlers write MACHINE ASR output into annotated_transcript, the by-law human-only field (annotatedTranscript = result.text at 1113/1129; also 1193 constrained, 1232 finetuned, 1274 Scrib... |
| 6 | F | critical | `cortex-speech-app/src/lib/ReviewMode.svelte:212` | ReviewMode's re-transcribe builds annotatedTranscript: text from machine ASR output (result.text at 208-212, persisted at 222), writing machine text into the by-law human-only annotated_transcript field; any annotated... |
| 7 | O | critical | `cortex-speech-app/src/App.svelte:1436` | Curate Verify (^D / verify-btn) sets verified=true with no recordHumanDecision and no text, while App.svelte renders verdict_transcript NOWHERE (zero occurrences of 'verdict' in the file); since verdict_transcript is ... |
| 9 | O | critical | `cortex-speech-app/src-tauri/src/quality.rs:430` | Machine text written into annotated_transcript (the exact 348-row incident class) is exported as human text by every format: training_transcript_with_source promotes ANY non-empty annotated_transcript on a bare non-em... |
| 15 | O | critical | `cortex-speech-app/src-tauri/src/quality.rs:341` | The GOLD branch fires on seg.verified \|\| seg.is_gold alone, decoupled from the transcript source chosen at lines 421-449: a verified row with empty verdict/annotated grades GOLD with reason 'human_verified' while it... |
| 19 | F | critical | `src-tauri/src/commands.rs:1299` | batch_transcribe passes machine text (draft.final_text: the 7B champion output, or Gemini-refined text when refinement is on, pipeline.rs:3142-3199) as annotated_seed, and db.rs:1106 writes it into annotated_transcrip... |
| 23 | F | critical | `cortex-speech-app/src-tauri/src/commands.rs:1299` | batch_transcribe passes draft.final_text (machine, and LLM-refined when refinement is on) as annotated_seed, so every batch run writes machine text into the human-only annotated_transcript of any unreviewed row whose ... |
| 24 | O | critical | `cortex-speech-app/src-tauri/src/db.rs:2655` | update_segment_consensus_batch's guard checks human_decision/verdict but omits the 'AND verified = 0' clause its siblings added (db.rs:1054), and run_consensus_refinery feeds it ALL hypotheses (commands/agentic.rs:112... |
| 1 | O | major | `src-tauri/src/quality.rs:431` | The spot-check answer key treats non-empty annotated_transcript as 'human_verified' purely because verified\|\|is_gold, with no human-provenance check — and the candidate query (db.rs:1324) explicitly admits reviewed_... |
| 3 | O | major | `src-tauri/src/couch.rs:1800` | (b) Undo's staleness fence refuses only when fresh.reviewed_by differs from the undoing reviewer, but reviewed_by is written ONLY by record_human_decision_by (db.rs:3329) and clear_human_decision (db.rs:3084). Every n... |
| 4 | O | major | `src-tauri/src/couch.rs:1614` | (a) Accept-vs-edit is classified against the row AS IT IS at decision time (`is_edit = text != review_text(&prev).trim()`), and the server keeps no record of the text it actually served. If a background writer (re-tra... |
| 8 | O | major | `cortex-speech-app/src/App.svelte:1695` | 'Verify All Pending' bulk-stamps verified=true on every pending row (ids at 1678, batchVerify at 1695) with no per-clip listening, decision record, or text capture; verified gates the human-gold export (isVerifiedGood... |
| 10 | O | major | `cortex-speech-app/src-tauri/src/quality.rs:341` | The verified flag is not bound to any text snapshot: human_verified is computed from bare seg.verified (quality.rs:341) and then attached to whatever text CURRENTLY occupies annotated_transcript (quality.rs:430-431), ... |
| 11 | O | major | `cortex-speech-app/src-tauri/src/export_audio/mod.rs:334` | export_audio_segments filters only revoked+holdout (export_audio/mod.rs:82-87) — never is_human_rejected — so a human-REJECTED segment (whose verified flag is deliberately set true to finalize it out of the review que... |
| 13 | O | major | `cortex-speech-app/scripts/build_premium_dataset.py:150` | Machine text can be exported as human: the sole human-provenance gate is the export's transcriptSource label, which quality.rs:430-432 mints as "human_verified" for ANY non-empty annotated_transcript on a verified/is_... |
| 16 | O | major | `cortex-speech-app/src-tauri/src/quality.rs:444` | Stale machine text outranks fresher champion text: no recency or engine-provenance comparison exists anywhere in training_transcript_with_source, so after a champion re-transcription refreshes raw_transcript (the exac... |
| 17 | O | major | `cortex-speech-app/src-tauri/src/jury/mod.rs:489` | write_verdict's human-authority guard checks only human_decision and human_* verdicts, never verified, and run_t0_gate's already-decided skip (mod.rs:366) checks only verdict — so a segment a human verified via the Ve... |
| 20 | O | major | `src-tauri/src/bin/batch_processor.rs:183` | The batch processor fetches ALL unverified segments (line 40) and overwrites raw_transcript (183) and normalized_transcript (184) of every one with bundled OmniASR-CTC-300M output, upserting whole rows at 197 and rewr... |
| 21 | O | major | `src-tauri/src/commands/segments_write.rs:118` | update_segment upserts the renderer-supplied WHOLE row (raw/normalized/annotated/verified via db.insert_segment's ON CONFLICT DO UPDATE, db.rs:643-651) with no freshness check and no verified/human_decision guard — un... |
| 22 | O | major | `src-tauri/src/commands/segments_write.rs:261` | record_human_decision commits an accept keyed only on id (db.rs:3324-3337): with corrected_transcript=None the COALESCE at db.rs:3327 retains the prior MACHINE jury verdict_transcript, which eval/gold reads as the top... |
| 25 | F | major | `cortex-speech-app/src-tauri/src/db.rs:1106` | annotated_transcript=COALESCE(annotated_transcript, seed) makes the FIRST machine draft sticky: a later champion re-batch updates raw/normalized but the old machine seed survives, and every annotated-first consumer (c... |
| 26 | F | major | `cortex-speech-app/src/lib/ReviewMode.svelte:212` | doRetranscribe writes the machine transcription result.text into annotatedTranscript (persisted whole-row at line 222 via api.updateSegment), putting machine text in the by-law human-only field with no human decision ... |
| 2 | O | minor | `src-tauri/src/couch.rs:1800` | api_undo's staleness fence detects intervening work ONLY via reviewed_by (fresh.reviewed_by != Some(reviewer)); insert_segment_full at couch.rs:1826 then rewrites every column from the decision-time snapshot, so any w... |
| 12 | O | minor | `cortex-speech-app/src-tauri/src/export_audio/mod.rs:86` | export_audio_segments also lacks the is_effective_placeholder filter every other exporter applies (export.rs:372, export_bundle.rs:317-318, transcript_export.rs:204), so a not-yet-transcribed segment exports its clip ... |
| 14 | O | minor | `cortex-speech-app/scripts/retrain_readiness.py:100` | Machine text is counted under a human label: the "human-gold words" figure sums words of EFFECTIVE (:75) over all export-eligible rows, and EFFECTIVE (:34-42) falls through to normalized_transcript (the Gemini paraphr... |
| 18 | O | minor | `cortex-speech-app/src-tauri/src/jury/mod.rs:481` | jury::write_verdict omits the v48 jury_transcript write that its sibling db::write_segment_verdict makes (db.rs:3028, comment: 'written here and nowhere else' so the machine's own output survives the human's) — so for... |

## Fixed in iteration 270 (commit pending deploy)

The live machine writers of the human-only `annotated_transcript` field:

- `db.rs::update_batch_transcription_if_unreviewed` no longer seeds the field with the machine
  draft (`COALESCE(annotated_transcript, seed)` removed) — this was the writer that produced the
  348-row incident and would have re-poisoned the cleaned rows on the very next batch run.
- All five desktop machine re-transcribe handlers (`App.svelte` champion/constrained/finetuned/
  Scribe, `ReviewMode.svelte` doRetranscribe) now write raw/normalized only; a machine
  re-transcription also NULLs `normalized_transcript` unless recomputed, so stale text cannot
  outrank the fresh draft at the `annotated ?? normalized ?? raw` display precedence.
- Pinned by `scripts/test_machine_never_writes_annotated_policy.py` (fail-before verified: 6
  machine sites flagged pre-fix, 0 false positives on the legal human writers) and the Rust pins in
  `db_tests.rs` (flipped from asserting the seeding to asserting its absence).

## Open defects, grouped (owner decisions / next iterations)

1. **`verified` fabricates human provenance** (#1, 7, 8, 9, 10, 15, 16, 22): `verified=1` alone
   grades text human-verified GOLD; bulk "Verify All Pending" stamps it wholesale; Curate ^D can
   bless an UNSEEN jury `verdict_transcript`; the spot-check answer key trusts it. Needs a design
   decision: gold requires an explicit per-clip human decision (text captured at decision time).
2. **Serve/decide race + undo fence** (#2, 3, 4, 21): decisions are classified against the row at
   submit time with no record of the served text; undo restores whole-row snapshots over fresher
   writes; `update_segment` whole-row upsert has no freshness check.
3. **Export filters** (#11, 12): `export_audio` misses `is_human_rejected` + placeholder filters
   (7th instance of the count-sites class).
4. **Scripts trust labels** (#13, 14): premium builder and retrain-readiness accept
   `transcriptSource=human_verified` at face value — inherits every defect in group 1.
5. **Jury/consensus guards** (#17, 18, 24): `write_verdict` ignores `verified`;
   `update_segment_consensus_batch` misses the `AND verified = 0` clause its siblings have.
6. **Champion-law violation in tooling** (#20): `bin/batch_processor.rs` drafts with CTC-300M and
   overwrites raw on ALL unverified rows — must not be run until aligned with the champion rule.
