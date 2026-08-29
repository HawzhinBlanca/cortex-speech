# OWNER CANON — approved decisions no agent may change

**Read this before touching anything.** Every item here was decided by the owner, is considered
FINAL, and is enforced by a named gate or test wherever a machine can check it. An agent may not
alter, weaken, or "improve" a canon item — however good the idea looks — unless the owner writes
**`change canon: <item>`** in his own words. Proposing a change is fine; changing without that
phrase is the defect this file exists to stop. `scripts/test_owner_canon_pins.py` fails the sweep
if a checkable pin drifts from what is written here.

## Models (owner rules 2026-08-06 through 2026-08-21, CRUCIAL)
- The Sorani-adapted **OmniASR-7B champion** is the sole main, default and production ASR for dataset
  drafting and review. Qwen3-ASR and Voxtral were evaluated and KILLED (no ckb support).
- OmniASR 300M/1B and fine-tuned MMS may remain explicit optional diagnostics or benchmarks. They are
  not release prerequisites, are never selected automatically, and are never fallbacks for WSL7B.
- **ElevenLabs Scribe is not a dependency or production feature.** Its client, commands, key, consent,
  and UI are removed from the shipped app. Historical provenance labels remain readable only so old
  rows stay honest. Gemini 2.5 Pro remains the only
  approved cloud judge when cloud judging is explicitly enabled; it is not an ASR fallback.
- Selecting WSL7B does not grant permission to seize busy GPUs: champion lifecycle supervision is off
  by factory default and is enabled explicitly only when this app should own the server.
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
- **Review compensation canon revision 2 (owner change 2026-08-21, prospective).** Immutable policy id:
  `review-iqd-v1-2026-08-21`. The rate is **18,000 IQD per full-equivalent audio hour**. A valid,
  playback-evidenced durable semantic action earns: `edit = 100%`, unchanged `accept = 10%`, valid
  `reject = 10%`, and `skip = 0%`. An event-row string alone is not payable provenance.
- Activity, correction, and money are separate facts: `reviewed_audio_ms` is activity, not money;
  the corrected-audio projection counts retained human edits only; the exact weighted balance is
  `SUM(review_compensation_ledger.delta_micro_iqd)` under the immutable policy. Neither a paid reject nor a paid unchanged accept becomes
  corrected dataset audio.
- External payout references allocate contiguous ledger ranges through immutable
  `review_compensation_settlements` rows. The same credit range cannot be settled twice; a later
  signed reversal remains visible as outstanding adjustment rather than rewriting paid history.
- The policy is **prospective** from its recorded deployment cutoff. Earlier events are not silently
  repriced. Every new credit snapshots its policy, rate, weight, duration, canonical work identity,
  decision identity and idempotency key; undo/redecision appends an explicit reversal or adjustment.
  Retries and duplicate segment rows may never mint a second credit for the same reviewer/work item.
- **Current campaign scope (owner change 2026-08-21):** add the 6,922 final Lamo ids to the existing
  1,352-id focus, yielding an exact 8,274-id union. This is additive, not replacement. Activation does
  not authorize serving: stale-release, provenance, playback, dialect and hidden-check gates remain
  fail closed, and the focus file is changed only through a validated atomic tool, never by hand.
- **Consensus review canon (owner, 2026-08-29 — "THIS MAKE CANON").** A sentence is decided by **any
  two DIFFERENT reviewers**, never by a named role. No reviewer is designated "first pass" or "second
  pass": a reviewer opens their link, is served an audio, corrects it, and any other reviewer may take
  the next one. Two different reviewers agreeing resolves the clip; when the first two disagree a
  **third** reviewer is served it. Work distributes naturally rather than being assigned — if one
  reviewer judges ten clips, their second opinions may come from several different people (two from
  one reviewer, two from another, and so on). The independence requirement is enforced PER CLIP (the
  same person may never be two of its opinions), never per person, so throughput scales with however
  many reviewers are working. This supersedes the sequential single-reviewer campaign model for
  deciding sentences.
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
- Duplicate-content baseline: **0** (2026-08-17: measured 68, corrected to 170 when the owner’s
  ears exposed the mp4’s shifted clock, then CLEANED — 170 redundant clips removed under
  BEGIN IMMEDIATE with backup pre-dedupe-20260817-162534). Any duplicate from now on is a
  fresh import and a RED sweep — `check_dataset_duplicates.py`.

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
