# OWNER CANON — approved decisions no agent may change

**Read this before touching anything.** Every item here was decided by the owner, is considered
FINAL, and is enforced by a named gate or test wherever a machine can check it. An agent may not
alter, weaken, or "improve" a canon item — however good the idea looks — unless the owner writes
**`change canon: <item>`** in his own words. Proposing a change is fine; changing without that
phrase is the defect this file exists to stop. `scripts/test_owner_canon_pins.py` fails the sweep
if a checkable pin drifts from what is written here.

## Models (owner rule 2026-08-06, CRUCIAL)
- The Sorani-adapted **OmniASR-7B champion** and the embedded **fine-tuned MMS-1B** are fixed
  infrastructure. Never propose replacing them. Qwen3-ASR and Voxtral were evaluated and KILLED
  (no ckb support). Only same-family OmniASR v2 300M/1B fallback benchmarking is allowed.
- Cloud ASR/judge for ckb is STRICTLY **Gemini 2.5 Pro**; ElevenLabs Scribe is the only other
  approved cloud STT. Never suggest Qwen for Kurdish.
- The champion drafts EVERY clip when `asr_model_size = WSL7B`; any per-clip failure is a HARD STOP,
  never a silent fallback. Enforced: `test_champion_supremacy_policy.py`, `batch_transcribe` halt.

## The verbatim law (2026-08-12)
- Transcript precedence everywhere: **human verdict ▸ annotated ▸ champion raw**. Refined/LLM text is
  evidence only; `llm_mode` defaults to None. Machine code NEVER writes `annotated_transcript`
  (human-only field). Enforced: `check_review_serving_provenance.py` on the live DB, every sweep.
- A `verified` flag alone never mints gold; gold requires a real human decision.
- Reviewers must transcribe EVERY audible word, whoever speaks it.

## Rights (2026-08-14, FINAL)
- All owner-supplied audio carries full permission including public use; speakers were paid; no
  royalties. Rights clearance is CLOSED — never re-raise it. FLEURS is the frozen eval set (never
  train on it, never delete it); Common Voice carries its own licence.

## Review operation
- Spot checks: 1 in `SPOT_CHECK_EVERY = 8` served clips is a trap with a known answer; the phone must
  NEVER serve a spot check its own answer key (`review_text` stays annotated ▸ raw — pinned by
  `the_phone_never_serves_a_spot_check_its_own_answer_key`). `verdict_transcript` is the answer key.
- Reviewers are paid on DISTINCT clips' audio duration (`reviewed_audio_ms`), never on event rows.
- Max 8 named reviewers; per-reviewer tokens; Stop revokes durably.
- Dialect routing: **KBHP = Hawleri** (all 32 episodes, owner-confirmed). The organized corpus tree
  declares dialect by folder (`Kurdish Corpora\<dialect>\`). Unmapped sources FAIL CLOSED for
  restricted reviewers. WHO may judge WHAT lives in `<data_dir>/reviewer_dialects.json` — names stay
  out of this public document by the repo's own hygiene law. Enforced:
  `check_reviewer_queues_live.py`, `dialect.rs` tests.

## Calibrated numbers (measured, not chosen — recalibrate only with a new measurement)
- `SPEAKER_CHANGE_THRESHOLD = 0.59` — within-clip half-vs-half; owner's blind 15-clip pass, 15/15.
- `SPEAKER_TURN_REFUSAL_CEILING = 0.43` — the chunk-cut judge's bar; the turn group's measured ceiling.
- DB backup pacing `4096 pages / 1 ms` — the 5/250ms doc example cost a 20-minute cold start.
- Watchdog startup grace **10 minutes** (sized to the measured 6.4 s startup, not the old bug).
- Duplicate-content baseline: **70** (2026-08-17 find), ratchets DOWN only —
  `check_dataset_duplicates.py`.

## Working rules
- **No fancy features** — reliability only; fix defects, don't add surface area.
- **Nothing ships on "sounds right"**: changes to how the corpus is CUT require a real-audio
  measurement first (`speaker_change_probe --replan`); iteration 225 and 290 are the precedents.
- Verify at the SERVING path, on the live DB — never only the write path.
- Every mistake found becomes a permanent gate in `scripts/verify_10.py`; a fix without a
  regression gate is incomplete.
- `main` is protected (admins included); land through PRs with the four required checks; never
  `git checkout main` locally.
- Honesty: no number that a real harness run didn't produce; failed runs are reported as failed.
