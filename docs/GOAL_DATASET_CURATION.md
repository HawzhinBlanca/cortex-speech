# GOAL — keep curing the corpus until it is genuinely ready, then prove it

**Standing objective (owner, 2026-08-14).** Bring every clip in the library to a state where a
reviewer can judge it in one pass and a training run can consume it without a caveat. Work in
repeated passes. Re-measure after every pass. Never declare a pass finished on the strength of the
change you made — only on the strength of a check you ran afterwards.

This file is the prompt. Re-read it at the start of every pass.

---

## The laws that outrank the goal

1. **Verbatim.** The transcript is what the speaker said, including disfluencies, repetitions and
   dialect forms. Never a correction, never a translation, never a standardization. `llm_mode` stays
   `None` for anything a reviewer will see.
2. **Provenance.** Text is `human_verified` only when a real human decision produced it. Machine text
   never enters `annotated_transcript`. An `accept` may only freeze text an ASR engine actually
   produced — `check_review_serving_provenance.py`, invariant 3.
3. **The champion drafts everything.** `asr_model_size = WSL7B`. A failure halts the run; it never
   degrades to a smaller engine and never finishes with mixed provenance.
4. **Verify at the serving path.** Every claim about what a reviewer or an export receives is checked
   by reading the row and field the consumer reads. Write-path checks have lied three times.
5. **No unearned claims.** A number comes from a run of the real harness or it does not get stated.
   "0 passed" is not a pass. A gate that cannot fire is not a gate.

---

## What "ready" means, per clip

A clip is ready for review when all of these hold, and each is checkable in SQL:

- audio exists on disk and decodes
- `raw_transcript` is non-empty and came from `omniasr-wsl-7b`
- `annotated_transcript` is empty (no machine text in the human field)
- `speaker_id` is set
- alignment exists and is not `energy_heuristic`
- `duration_ms` is set and within the VAD bounds

A clip is ready for training when, in addition:

- a human decision produced its text
- `training_grade = gold` and `training_ready = true`
- it is not `is_human_rejected`, blank, or placeholder

Anything failing an audio-quality reason (`low_rms_volume`, `near_silence`, clipping) is **held back
honestly**, not fixed by processing. Do not denoise, compress, or EQ training audio.

---

## The pass loop

Each pass is: **measure → find the largest real defect → fix its root cause → re-measure → record.**

1. **Inventory.** Count clips by state. Compare with the previous pass's numbers. A count that moved
   without a known cause is itself a finding.
2. **Serving-path audit.** Run `check_review_serving_provenance.py` and `check_spot_check_pool.py`
   against the live DB. Both must exit 0.
3. **Readiness sweep.** Query the per-clip conditions above. Every failure gets a named reason and a
   count — never a percentage without a numerator.
4. **Audio audit.** For newly imported material: level, clipping, spectral cutoff, dead air. Fix
   format and level only; report everything else.
5. **Reviewer-facing check.** Claim a queue over the live HTTP API with a real token and confirm the
   clips served carry champion text, working audio, and no blanks.
6. **Record.** Append to `PROGRESS_LEDGER.md`: what was measured, what changed, what is still wrong.
   A pass that found nothing says so.

Stop a pass when the next-largest defect needs an owner decision. Do not invent scope.

---

## Known-open, carried forward

- **Speaker identity is per-recording.** `SPEAKER_00..07` are diarizer indices, not people. The split
  fix scopes them to their recording; true speaker-disjointness needs CAM++ embeddings clustered
  across files. Not built.
- **Reviewer QC.** The highest-volume reviewer noticed 1 of 7 known-answer clips while owning the
  largest share of decisions. The cheap next measurement is a blind second pass of their accepts
  through another reviewer, which also builds the double-pass tier the charter still lacks.
- **Dialect disparity 14.79 points.** Measured, unfixed. The Hawleri material is the right kind of
  data to move it; no claim until re-measured.
- **Kappa gate.** Not computable until two reviewers judge the same clips.
- **Enhanced-vs-raw.** 76 clips carry `esv2-speech-50p` in their filename alongside raw clips from the
  same episodes. Once reviewed, this measures whether enhancement helps or hurts champion CER on the
  owner's own audio — an answer, not an opinion.

---

## Overnight standing orders

- Imports run with the app CLOSED, so reviewers are locked out. **The app must be running again by
  morning**, with the Couch Review session resumed and all five tokens unchanged.
- `CortexWatchdog` is disabled during imports and must be re-enabled afterwards.
- The machine must be idle for any benchmark. A concurrent sweep once made a fake 1.41x regression
  CONFIRM.
- Leave the tree committed and formatted. The exe bakes HEAD's SHA: fmt → commit → build.
