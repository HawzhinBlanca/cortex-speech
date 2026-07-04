# TRUE-10 GAP AUDIT — 2026-07-04 (5-agent live-tree audit)

What genuinely remains for a true 10/10 (fully reliable, robust, ship-ready, highest
user-friendliness + quality, intelligent). 5 parallel auditors read the live tree at HEAD
(`bb26625`), every finding cites file:line evidence. **This audit falsified two prior claims**
(see Honesty corrections). Findings ranked by real impact on the single daily user.

## BLOCKERS (all automatable — fix before any training/final-test run)

### B1 — Fine-tune training pack bypasses the grade rubric: mark-bad clips ship as training rows
`eval.rs:296` selects `get_segments(Some(true))` (verified=true) with only empty-text/dedup/
undecodable filters — **no `is_human_rejected`, no `training_grade_for_segment`**. Mark-bad sets
`verified=true` (ReviewMode.svelte:179), so a human-REJECTED clip's bad draft becomes a training
`sentence`. **FINAL_TEST_CHECKLIST.md's claim that the pack "drops any that fail the rubric" is
FALSE against live code.** Fix: skip `is_human_rejected` + `!training_ready` rows in
`export_finetune_pack`, report excluded counts, regression tests (mark-bad + severe-clipping never
emitted).

### B2 — Corruption quarantine + snapshot rotation can destroy every good snapshot in ~90 minutes
`db.rs:228-285` quarantines a corrupt DB and opens a FRESH EMPTY one (log-only, no UI banner);
`lib.rs:373-404` then snapshots the empty DB every 600s with keep=10 — 10 empty snapshots evict all
pre-corruption snapshots in ~90 min. Zero frontend callers for `db_restore`; no restore UI. Fix:
(1) quarantine banner event; (2) `take_snapshot` refuses when live DB has 0 segments but non-empty
snapshots exist; (3) Restore-from-snapshot picker.

### B3 (plan) — C1 still open: the daily default engine (WSL 7B) has no measured accuracy number
`settings.rs:292` default WSL7B; EVAL.md:256 admits unmeasured. **Owner-gated** (P2.2 GPU
afternoon, runbook ready). The single highest-leverage owner action.

## MAJOR — dataset quality ("highest grade output datasets")

- **Gold references can contain rejected drafts**: `create_gold_from_verified_file` (eval.rs:114-123)
  includes `human_decision='reject'` rows — the known-wrong draft concatenates into the WER/CER
  reference. Exclude rejects; warn when a rejected chunk's audio remains in the holdout WAV.
- **Validation scores a different hypothesis than quality.rs**: validation/mod.rs:156 scores
  normalized text; quality.rs:522-531 deliberately scores RAW (verbalized-numbers inflation). False
  HighWer/HighCer errors can block the production bundle. Mirror quality.rs.
- **Shipped training text is not Sorani-normalized**: HF `transcription` (export.rs:739) and
  finetune `sentence` (eval.rs:335) skip codepoint unification (ك/ک, ي/ی, ه/ھ) — mixed orthography
  inflates the CTC label space; finetune dedup key (eval.rs:317) misses variant duplicates. Apply
  char-only SoraniNormalizer + `normalize_transcript_for_hash` dedup.
- **Audio thresholds are unvalidated constants** (quality.rs:493-519; SNR estimator in
  audio_quality.rs:53-82 isn't standard-scale). Owner calibration run: dump distributions of
  accepted-vs-rejected clips, set thresholds from observed separation.
- **No composition reporting**: speaker balance / per-split duration / text diversity absent from
  dataset card (export.rs:836-857). Flag any speaker >50% of train.
- **Flat exports ship REJECT/REVIEW rows flagged-only**; `enforce_quality_gates` default false. Add
  a "training-ready only" toggle + include/exclude counts in the toast.

## MAJOR — reliability

- **Silent fine-tuned→stock downgrade per-chunk** (pipeline.rs:1541-1563, 2146-2185): fine-tuned
  model failure falls back to stock (~29.4% vs 21.0% CER) with only a log warn; provenance badge
  can't reveal it (no finetuned hypothesis row). Violates the F2 no-silent-downgrade contract.
  Count fallbacks → completion warning; fail loudly at 100%; record the actual drafting engine.
- **Snapshot failures + disk exhaustion invisible** (lib.rs:387-401 warn-only; health.rs has no
  last-snapshot-age/free-disk). Add to health_check + DiagnosticsPanel + toast after N failures.
- **Memory-pressure check is dead code** (commands.rs:1163-1166 discards the bool; threshold
  "used > 2GB" always true). Delete or make real (<1GB available → pause batch).

## MAJOR — intelligence (why the jury escalates ~everything)

- **The measured-best local engine never votes**: populate_hypotheses (pipeline.rs:2563-2628)
  inserts only 300M/1B/WSL-7B — the fine-tuned MMS (21.0% CER) is absent, and 300M+1B are
  architecturally kin (jury/mod.rs:79-81 admits it). Adding it as an independent third juror is the
  root fix for escalate-everything. (Re-measuring the escalation rate is owner-gated; the wiring is
  automatable.)
- **LOOP-0 shadow log is write-only**: nothing SELECTs loop0_shadow_log; firing_error_delta called
  only by tests; no UI toggle for loop0_firing_enabled. Build the read side (shadow precision vs
  human decisions + gold-set delta) or the C5 go-live decision is impossible.
- **Correction-memory confidence frozen at 1.0** (migrations:390, no UPDATE path): tau_conf gate is
  vacuous; one bad memory would poison transcripts permanently once firing goes live. Beta-posterior
  over confirm/override before any go-live.
- **T0 auto-accept structurally zero**: AutonLevel default Propose converts every AutoAccept to
  escalate (jury/mod.rs:147-154); conformal buckets need ≥10 verified items each (fail-closed);
  decision_verdicts (C4 denominator) is never aggregated/reported. Build the C4 trust dashboard so
  ActConfirm can be justified by evidence.
- **Suspect-first is effectively recency**: escalated rows carry agent_confidence=None →
  COALESCE(0.5) constant (db.rs:861); computed disagreement_score discarded (jury/mod.rs:297-301);
  ood_score unused in ORDER BY; toggle not persisted. Persist disagreement, blend with ood + audio
  quality.
- **T1 judge has no Sorani knowledge**: lexicon = Unicode-block ratio; "perplexity" = self-entropy
  (t1_judge.rs:36-90). Train a small KenLM on export_lm_corpus output.
- **Offline corrections influence nothing at decode time**: few-shot reaches only cloud T2
  (commands.rs:4224-4233). LOOP-0 go-live is the intended fix; consider example-biased consensus.

## MAJOR — UX (per-clip friction × hundreds of clips/sitting)

- **No cancel affordance during batch transcribe** (App.svelte:2859-2879 vs :2914 gate; backend
  cancel EXISTS at lib.rs:228-271) — a mistaken batch locks the app with no visible way out.
- **Autoplay setting ignored in both review surfaces** (ReviewMode.svelte:584, ReviewInbox.svelte:482
  hardcode false; honored only in curate) — the single biggest review-speed lever.
- **No undo in ReviewMode; global Ctrl+Z splits state** (undo stack records update_segment only,
  not record_human_decision — reverts `verified` but the decision row survives).
- **ReviewInbox Undo button permanently disabled** (legacy-mode reactivity: history mutated via
  push/pop, never reassigned — `disabled={history.length===0}` never invalidates). Real defect.
- **Space-key conflict**: ReviewMode Space=play/pause; ReviewInbox Space=SKIP, and inbox has no
  keyboard play at all.
- Minor: mark-bad native window.confirm; review shortcuts invisible to help/palette + no hotkey for
  Review mode; Mac ⌘ glyphs on Windows; raw/untranslated error+consent strings in CKB locale
  (events.ts all-English; SettingsPanel consent copy EN-only); review queue ignores search/filter;
  inbox rail doesn't scrollIntoView; ReviewMode shows no source-file context.

## Plan-state corrections + open automatable plan items

- **P5.2 open (automatable)**: 7B server hardcodes adapter path (cortex_7b_server.py:61) — champion
  promotion can never actually swap engines; no champion.json writer; no Promote button.
- **P2.4 open (automatable)**: check_gold_regression wired to nothing; all 5 gold_wer_eval tests
  #[ignore] — accuracy ungated during code work.
- **P5.4/P5.5 open (automatable)**: no WSL disaster-recovery runbook; no corpus ledger.
- **Owner-gated queue**: P2.2 benchmark (C1) → P1.7 observed-gates + P1.8 review-throughput
  baseline (record BEFORE UX fixes land!) → P3.5/6/9 drills → P4 marathon (3/500 decisions) → P5.6
  retrain → P6 DirectML → P7 re-audit (the only place 10/10 can be called).

## Honesty corrections (this audit falsified prior claims)

1. FINAL_TEST_CHECKLIST.md Part C.1 claimed the fine-tune pack "drops any that fail the rubric" —
   **false** (B1). Corrected when B1 is fixed.
2. The ledger's "automatable surface exhausted" claim was **overstated** — P5.2, P5.4, P5.5, P2.4
   and every automatable finding above were open at the time of the claim.
3. Ledger "P1.1-P1.7 done" overstates P1.7 (its observed-gate half is owner-work, unchecked in
   M2_INSTRUMENTATION_CHECKLIST).

## Sequenced fix plan (automatable, in order)

1. **B1** finetune-pack rubric enforcement (+ checklist correction) — dataset integrity.
2. **B2** quarantine banner + snapshot empty-DB guard + restore UI — disaster recovery.
3. Gold-reference reject exclusion + validation/quality hypothesis unification + Sorani-normalized
   export text/dedup — dataset quality cluster.
4. Silent-downgrade counter + snapshot/disk health surfacing — reliability cluster.
5. UX cluster: batch cancel, autoplay-on-advance, ReviewMode undo (+ drop confirm), inbox undo
   reactivity fix, Space unification, filter-scoped queue, shortcuts registration/help.
6. Intelligence read-side: fine-tuned juror wiring, LOOP-0 shadow report + C4 dashboard,
   disagreement-score persistence, memory-confidence updates.
7. Plan items: P5.2 champion pointer, P2.4 regression gate wiring, P5.4/P5.5 docs.
