# CORTEX deep brutal audit — 2026-08-25

**Audited HEAD:** `1282578` on `integrate/codex-flywheel` (clean tree)
**Method:** 43-agent adversarial workflow — 13 subsystem bug-hunters over the full Rust/Python/Svelte
surface, one adversarial verifier per non-trivial finding (instructed to REFUTE by reading the code),
plus a completeness critic over the subsystems nobody owned. 855 tool uses, ~5.6M tokens, every
surviving finding carries quoted code and a concrete failure scenario. 3 findings were refuted and
are listed as such. In parallel, the real harness ran live: `python scripts/verify_10.py --quick`
(run id `20260825T011914-1282578`) and targeted gate scripts on the production machine.
**Discipline:** no number below that a real run didn't produce; grades are explicitly the auditor's
judgment, not harness output. Reviewer names and private machine paths are deliberately absent per
the repo's hygiene law.

> [!IMPORTANT]
> **REMEDIATION LANDED — see [REMEDIATION.md](REMEDIATION.md) for what was fixed, what was
> corrected, and what remains owner-gated.** 53 of the 55 findings below are fixed across 14
> commits on this branch, each with a regression gate. Two statements in the original report
> are corrected there on evidence: the unpaid pool work is **prospective, not historical**
> (both decision tables are empty), and Phase 4's blocker is **deeper** than "run a cycle".
> The findings below are preserved verbatim as the record of what was found.

---

## Executive verdict

The **code** is the best it has ever been. The **operation of it, tonight, is RED in three
places** — and the machine's own gates say so.

Five of thirteen subsystem hunters came back calling their area "the strongest subsystem audited"
(db-core money paths, segments-write v60 boundary, review-serving precedence, the compensation
ledger core, engine identity checks) — and the adversarial verifiers, told to refute, confirmed
that praise. This codebase's canon machinery (transcript precedence, answer-key isolation,
append-only money, fail-closed dialect routing, champion identity pins) genuinely held under
attack. That is rare and it is earned.

And yet: **51 defects survived adversarial verification** (6 high, 15 medium, 30 low), the live
sweep is failing right now, the champion's hard-stop *report* is silently swallowed by the UI, an
entire class of playback-evidenced review work mints **zero pay**, the duplicate-content gate
cannot see the one duplicate shape the library actually contains (48k vs 16k encodes), and the
gold-eval yardstick will publish a headline CER after silently dropping failed clips — the exact
"looks finished, isn't" shape the 2026-08-11 canon was written to kill.

**Auditor's grade: 8.4/10 as an engineering artifact. Not 10/10, and tonight not even
sweep-green.** The gap to a real 10/10 is enumerated at the bottom, with nothing hidden.

---

## The live REDs — on this machine, tonight (real harness output)

These are not code-review opinions. Each is a real gate run from 2026-08-25, ~01:19–01:25.

| # | Gate | Result | What it means |
|---|---|---|---|
| 1 | `python-policies` → `test_watchdog_enabled.py` | **FAIL** (2 of 101) | **CortexWatchdog is registered but DISABLED right now.** Task history shows it ran healthy until 05:12 the previous morning, then something disabled it and never re-enabled it. The app restarts 4–9×/day; the next wedge leaves every reviewer phone link dead until a human notices — the exact 2026-08-15 incident, re-armed. Fix is one command: `schtasks /change /tn CortexWatchdog /enable`. |
| 2 | `python-policies` → `test_ledger_staleness.py` | **FAIL** | `PROGRESS_LEDGER.md` is **7 commits stale** (limit 3) — everything since `6731258` landed unledgered. The provenance-of-claims mechanism is not being fed. |
| 3 | `spot-check-pool` | **FAIL** | **Hidden-check capacity is fully drained**: 4 active reviewers, 6,920 accessible work clips each, **0 fresh answer keys available of 1,107 required per reviewer**. The 1-in-8 trap-clip QC that the compensation canon leans on cannot currently trap anyone. The gate fails closed and says exactly what to do: add owner-adjudicated/gold keys inside the active focus, or narrow the campaign — never fabricate keys. |
| 4 | `challenger-loop` (last full sweep, `21c639d`, ancestor of HEAD) | **FAIL** | `runs/` contains **zero `promotion_verdict.json`** — the challenger loop has never produced a trained challenger with a byte-linked measured verdict. `docs/STATUS.md` verdict: **RED — NOT ship-ready**. This is the standing full-sweep blocker. |

A `--quick` sweep was running at the time of writing; Tier 0 passed clean, Tier 1 red as above.
Its final verdict line is appended at the end of this file.

---

## HIGH findings (6) — every one adversarially verified against the code

### H1. Watchdog disabled on the production machine — live reliability failure
`cortex-speech-app/scripts/test_watchdog_enabled.py` red, verified independently via `schtasks`.
See live RED #1. This is first because it is the one an outage tonight would trace back to.

### H2. The champion hard-stop event is swallowed — the UI never reports the halt
`cortex-speech-app/src/lib/events.ts:272` ← `src-tauri/src/commands.rs:1511`
The backend honors canon: on any per-clip champion failure it emits the terminal batch event with
`"type": "halted"` and `haltedBy: <cause>`. The frontend's only `batch-progress` listener branches
on `started | progress | completed` — the TypeScript union doesn't even admit `'halted'`. Result:
`isProcessing` stays true forever, the progress pill stays "running" at the last percent,
`endOperation` never fires, segments never refresh, and **the cause the canon demands be reported
is never shown to anyone**. The halt itself works (safe direction — nothing false claims
"completed"), but "halts and reports the cause" is half-implemented, and the policy pin never
checked the frontend half. Verified: both sides quoted, no other listener exists.

### H3. `batch_importer` has zero cross-run duplicate protection — the 2026-08-14 incident class is still open
`cortex-speech-app/src-tauri/src/bin/batch_importer.rs:296`
The GUI import path rehydrates the content-fingerprint map from the DB (`lib.rs:647`);
the headless importer — the owner's primary import lane — constructs `AudioFingerprint::new()`
**empty and never rehydrates**. All its resume/skip mitigations key on exact path strings, while
the directory prefix filter matches case/separator-insensitively. So a re-run typed `d:/KBHP`
instead of `D:\KBHP` prints "Resuming: N file(s) already in the library" **and then re-imports
every one of them** — doubling clips in the library that the duplicate-content baseline (canon: 0,
any dup is a RED sweep) exists to forbid. One missing call: `fingerprint.rehydrate(db.load_audio_identities()?)`.

### H4. The duplicate gate's audio confirmation can't see mixed-rate duplicates — and claims a resample step that does not exist
`cortex-speech-app/scripts/check_dataset_duplicates.py:197`
Rule C divides a raw sample-count difference by a hard-coded `rate = 16000`, with a comment
promising "both clips are resampled to a common length below" — **no resampling exists anywhere in
the file** (`_clip_pcm` reads at native rate). The library holds both 48 kHz masters and 16 kHz
WAVs (owner-pinned fact). The same 3 s sentence imported once from each computes a 6000 "ms"
difference → auto-cleared as "legitimate repeats". Even same-rate 48 k pairs get a tolerance
silently 3× tighter than declared, and the correlation step compares different-rate signals
sample-by-sample, guaranteeing False for mixed-rate pairs. The gate that guards the baseline-0
canon **cannot detect the one duplicate shape this library is most likely to produce**, and H3 is
the machine that would produce it.

### H5. Pool-mode and blinded-second-pass review work mints zero compensation and zero activity
`cortex-speech-app/src-tauri/src/couch.rs:4338` (+ `review_pool.rs:776`)
When a pool is active, every decision on an already-canonical clip routes to `api_pool_decision`;
during a blinded second pass, to `api_independent_decision`. Both enforce the full 428
playback-evidence gate, both commit durably — and **neither touches `review_compensation_ledger`
or `review_events`**. An hour of playback-evidenced, mandated second-pass judging shows a flat
balance and flat `reviewedMs` on the reviewer's phone, silently. The compensation readiness gate
is blind to both tables. Mitigations: the decisions are durable and carry reviewer/action/duration,
so retroactive backfill is computable; exposure is bounded to owner-activated modes. But under the
compensation canon ("a valid, playback-evidenced durable semantic action earns…") these surfaces
either owe money or owe an explicit owner decision that non-canonical passes are unpaid — today it
is neither, and nothing surfaces it.

### H6. The gold-eval yardstick publishes headline CER over whichever clips happened to succeed
`cortex-speech-app/src-tauri/src/eval.rs:1113`
`run_gold_eval_with_transcriber` counts per-clip transcription failures with `tracing::warn!` and
**runs the eval on the survivors anyway**. The persisted `eval_runs` row stores survivor-count
`num_segs` and no `failed`/`total_gold` field — coverage is log-only, invisible to the UI, eval
history, and anything downstream. Champion flakes on the 200 hardest of 348 gold clips → a durable,
history-visible "gold CER" measured on the 148 easiest. This is the hard-stop canon's exact target
shape ("a partly-drafted dataset that looks finished is worse than a run that stopped") applied to
the promotion yardstick itself. One line fixes the canon breach: `if failed > 0 { return Err(...) }`.

---

## MEDIUM findings (15) — verified; compressed to what you need to act

| # | Where | Defect (verified) |
|---|---|---|
| M1 | `src-tauri/src/couch.rs:3198` | Ordinary (non-pilot) hidden-check serve returns 200 with the check embedded even when `persist_session_state` fails — and the failure is logged **only** on the pilot branch. App restarts before a later save → the pair is gone; the reviewer's answer hits 409 and the QC score is silently lost. Pilot namespace has a SQLite reservation; the normal paid path has none. |
| M2 | `src-tauri/src/couch.rs:1203` | Session/spot-check restore filters compare reviewer names **case-sensitively** (`names.contains`, `n == &name`) while the rest of the file and the DB (`COLLATE NOCASE`) are case-insensitive. A re-typed roster casing on restart kills pairing tokens (links answer 401), cookie sessions, and outstanding check receipts. Auto-resume is immune; manual re-entry — which the new roster tooling makes routine — is not. |
| M3 | `scripts/finalize_halwest_dataset.py:71` | Train/val/test splits slice **every source recording across all three splits** — same session, mic, room on both sides; leakage by construction; contradicts `export.rs assign_splits`' own "no source-recording leakage" standard. (Medium not high only because no in-repo eval consumes these splits — FLEURS is the frozen eval.) `create_halwest_gold_subset.py` inherits the same function. |
| M4 | `src-tauri/src/export_bundle.rs:916` | Bundle ships `quality_report.json` computed over the **unfiltered library** (before holdout/rejected/placeholder filters at 941/945/953) next to dataset files that exclude those rows — two artifacts in one directory contradicting each other. `total_segments`, `empty_transcript_count`, quartiles are the unfiltered fields. The 7th sighting of the count-site class. |
| M5 | `src-tauri/src/export.rs:1175` | `droppedUnavailableAudio` undercounts silver rows: the justifying comment claims the readiness gate "never reads the audio" but for silver rows with commit evidence it blake3-hashes the source file, so a missing drive puts rows in the silently-not-counted bucket. Loss-accounting lies; comment is factually false. |
| M6 | `src-tauri/src/engine_runtime.rs:707` | `start_child` failures inside the supervision tick go only to the log; the breaker charges each **instant** spawn failure a full 6-minute warm-up before `on_failure` — a knowable-at-first-spawn condition takes ~50 minutes to reach GaveUp, labeled "starting/recovering" throughout. |
| M7 | `scripts/verify_10.py:844` | The probe contract maps **any** probe reason to SKIP-ENV — including `_probe_champion_7b`'s "the WRONG MODEL is answering the champion port". The one condition the 2026-08-20 strengthening exists for is reported as "environment not ready", never FAIL. (It still can't mint a green — SKIP forces INCOMPLETE — but the operator signal is wrong.) |
| M8 | `scripts/test_all_policy_tests_execute.py:40` | The dead-test meta-gate reads only **top-level** `test_` functions; the 7 unittest-style policy files — including the 32-test compensation gate and the 12-test serving-provenance gate — are invisible to it. Strip their `unittest.main()` tail and the suite still prints "101 policy test scripts passed" forever. Bug-class 5, aimed at the money gate. |
| M9 | `cortex-speech-app` ledger gate | `PROGRESS_LEDGER.md` 7 commits stale — live RED #2 above. |
| M10 | `src/lib/ReviewInbox.svelte:258` | All four decision handlers write `queue[idx] = commit.segment` into a **pre-await index snapshot** (`const idx = currentIndex` before two awaits). A queue reload during the in-flight IPC makes the decided row overwrite a *different* undecided segment, or creates a sparse array that throws in the rail. `undo()` alone does it right (`findIndex` by id) — bug-class 3, four sites, one file. |
| M11 | `src-tauri/src/db.rs:4984` | The couch queue filter and all three decide guards implement placeholder detection as "empty or `[bracketed]`" while the declared authority `quality::is_placeholder_transcript` also catches `n/a`/`null`. A merged `n/a` draft is served to a paid reviewer, blind-acceptable into a verified gold row, or honestly re-typed — finishing the clip without the champion ever drafting it. The pinning test only exercises bracketed strings, which is why nothing caught the drift. |
| M12 | `src-tauri/src/bin/batch_importer.rs:73` | The "feed both champion workers" feature is **inert at default config**: 2 file workers spawn but the process-wide 7B gate still admits 1 (same unset env var read as different defaults in two places), while printing "one file per warm GPU worker". Commit `61b6eb5`'s stated purpose materializes only with `CORTEX_7B_CONCURRENCY=2` exported. |
| M13 | `src-tauri/src/bin/pool_admin.rs:37` | Commit `e021ffe` relaxed `voice_inventory_ready` from strict equality to `>=`, discarding the only check that could refuse a **doubled import generation**. Identical-settings doubles still collide on the (hash,start,end) triple; span-divergent doubles (changed chunk settings between imports) bind both generations into the pool — same audio servable and payable twice. Interlocks with H3/H4. |
| M14 | `scripts/check_review_serving_provenance.py:133` | Invariants 1 and 2 exempt any row bearing ANY `review_events` row — and **skips create review_events rows**. A once-skipped, still-served clip can carry machine-written text and the canon-law gate prints "all invariants hold". The exemption must be decision-bearing events only. |
| M15 | `scripts/activate_named_voice_focus.py:72` | Champion-draft certification reads `champion.json` — the startup **mirror** — not the `model_versions` registry, in the exact register-first/restart-second window the sibling gate documents as the forbidden trust pattern. Certifies "100% champion drafts" against a champion the registry no longer names (or spuriously rejects valid new-champion drafts). |

## LOW findings (30) — real, verified, none reachable in tonight's normal operation

| Where | Area | Defect |
|---|---|---|
| `src-tauri/src/couch.rs:1750` | security | POST `/api/claim` accepts a live session-cookie token as a pairing credential; the probe endpoint explicitly refuses that same token |
| `src-tauri/src/validation/input.rs:100` | security | `validate_identifier` accepts dot-only strings (`.`, `..`) — latent traversal hazard in the app-wide id gate |
| `src-tauri/src/flock.rs:97` | concurrency | Unix branch: a refused second instance deletes the LIVE holder's lockfile → next launch double-runs on one SQLite file |
| `src-tauri/src/db.rs:5865` | concurrency | `create_or_get_job` dedup is check-then-insert without a transaction; a lost race surfaces a UNIQUE error instead of the existing job |
| `src-tauri/src/couch.rs:3167` | serving | Spot-check `rowVersion` is a second fallible read degrading to JSON `null` — a payload shape only a trap clip can have, and one that makes the check undecidable |
| `src-tauri/src/export.rs:404` | export | `is_gold` answer-key exclusion enforced in `export_audio` only; tabular/HF/bundle exporters ship flagged answer keys |
| `scripts/build_halwest_dataset.py:546` | export | Artifacts embed the curator's absolute paths — the leak every Rust exporter strips and regression-tests |
| `scripts/build_halwest_dataset.py:385` | export | Transcript pairing keys collide for double-extension siblings; a stale `X.txt.<anything>` can replace `X.txt` as TRUSTED |
| `src-tauri/src/engine_runtime.rs:638` | engine | Start-button restarts invisible to the supervisor; next tick can tree-kill the owner's still-loading champion |
| `src-tauri/src/engine_runtime.rs:587` | engine | `ensure_start_allowed` doesn't check `PROMOTION_ACTIVE` — lease exclusion is check-then-act |
| `src-tauri/src/asr.rs:299` | engine | Model-integrity gate: WSL7B files carrying the champion's provenance id get a bare `exists()`, no SHA pin |
| `src-tauri/src/commands/segments_write.rs:149` | write | Legacy whole-row `update_segment` IPC (caller-less, still registered) accepts blank `raw_transcript`, stale machine fields, resurrects deleted rows — both recurring classes on one dead endpoint |
| `src-tauri/src/corrections.rs:346` | write | `firing_winner_indices` ignores `cfg.context`; LOOP-0 provenance diverges from `apply_memories` under non-default modes |
| `src-tauri/src/commands/segments_write.rs:444` | write | `record_playback_receipt` accepts an unbounded `session_id` string |
| `src-tauri/src/bin/batch_importer.rs:50` | import | Prepared-import champion-evidence gate accepts a **blank transcript** as valid champion work |
| `src-tauri/src/commands.rs:1489` | pipeline | Post-batch jury adjudication failure is log-only; batch still reports `completed` |
| `scripts/verify_10.py:288` | gates | Probes unguarded: a hung `gh auth status` (no timeout) wedges the sweep; a probe exception aborts all remaining gates |
| `scripts/verify_10.py:862` | gates | LNK1104 retry discards the first attempt's output; adjacent comment claims both land in the log |
| `scripts/test_all_policy_tests_execute.py:74` | gates | `globals()` exemption fires on ANY reference anywhere — 33 of 101 files exempt from dead-test detection |
| `src/lib/stores/segmentStore.ts:91` | frontend | `segments.hydrate()` has no staleness guard; a slow `getSegment` reverts a fresher row and Save-speaker persists the reverted value |
| `src/lib/ReviewMode.svelte:555` | frontend | `originalText` uses `??` — an empty/whitespace `annotatedTranscript` masks the champion raw draft in the editor |
| `src/lib/autosave.ts:121` | frontend | Latent: same-segment edit scheduled during in-flight save droppable on a third schedule (unreachable with today's single autosaved field) |
| `src-tauri/src/db.rs:2653` | db | Shared ASR persist boundary still accepts `""` — twice-fixed blank-overwrite class fenced only at call sites, not in the shared fn |
| `src-tauri/src/db.rs:4515` | db | Reviewer identity case-sensitive in activity/throughput/agreement reads, NOCASE in every money/limit path |
| `src-tauri/src/db.rs:4627` | db | Settlement writer leaks `PRAGMA synchronous=FULL` on early-error paths; the one money writer on a DEFERRED transaction |
| `src-tauri/src/db.rs:2054` | db | Batch-transcription undo can rewrite the machine draft underneath a later human decision |
| `src-tauri/src/db.rs:1767` | db | v60 generic upserts lack the reviewed-baseline guard sibling `merge_dataset_json` enforces |
| `scripts/repair_unfinalized_reviews.py:232` | scripts | Rights stamping accepts a blanket `%` LIKE that would claim third-party corpora as owner-full-rights |
| `src-tauri/src/db.rs:3936` | money | `record_spot_check` is a proof-free paid ledger writer compiled into production, un-gated |
| `src-tauri/src/db.rs:4612` | money | No production surface records a settlement — payouts happen only via ad-hoc SQL against the live ledger |

## Critic extras — reported, NOT adversarially verified (treat as leads)

- `src-tauri/src/commands/gold_eval.rs:40` — the renderer-facing `run_gold_eval` IPC accepts
  caller-supplied hypotheses + model label and durably writes `eval_runs` rows indistinguishable
  from measured champion runs. A fabricated 0.00% CER row is one IPC call away, in the history the
  owner trusts most.
- `src-tauri/src/quality.rs:172` — `empty_transcript_count` / `low_confidence_count` include
  human-rejected and placeholder rows every export drops; with quality gates on, an un-clearable
  dataset-level ERROR over a row that will never ship. (8th sighting of the count-site class.)
- `src-tauri/src/quality.rs:629` — `add_audio_quality_reasons` re-hardcodes the poor-audio
  thresholds as a fourth, unpinned copy directly beneath constants declaring themselves the single
  source of truth; tuning the constant moves three consumers and silently not the fourth.

## Refuted claims (what the skeptics killed — reported for honesty)

1. `atomic_file::replace_file` same-process race — technically present in isolation, but every
   real caller path is serialized; no reachable scenario survived.
2. `settings.json` fixed-tmp race — individual facts correct, but the mutation accounting
   prevents the claimed interleaving in practice.
3. Roster tool deleting the `couch_session.revoked` interrupted-activation marker — blocked by a
   campaign/focus preflight the finder missed.

## What held under attack (credit where due)

- **Money core:** append-only ledger with trigger-enforced immutability, signed delta-entitlement
  math that provably nets retries/duplicates to zero, integer micro-IQD that errors instead of
  rounding, settlement ranges recomputed inside the DB. Attacked directly; held.
- **Serving core:** answer-key unreachable from every serving surface traced (queue, audio, renew,
  undo, error text, accounting); precedence split correct and pinned; dialect/focus gates fail closed.
- **Write boundary:** the v60 effect-bound boundary closes review-owned truth to evidenced human
  decisions, atomically, with forged-column tests.
- **Engine identity:** SHA-256 + protocol + status health checks, Job-Object containment with a
  real kill drill, CAS-guarded crash-recoverable promotion saga.
- **Import span math:** streaming span/carry logic is contiguous and drift-free; champion
  halt/rollback discipline in the pipeline core is airtight.

## The systemic diseases (patterns, not points)

1. **Count-site dishonesty is systemic, not incidental.** Sightings 7 and 8 found (M4, critic
   quality.rs) after six fixes. Every new tally keeps re-growing the same lie. The class needs a
   structural kill: one shared filtered-view function that every counter MUST consume, pinned by a
   gate that greps for raw `segments.len()`/unfiltered aggregation in gate/report code.
2. **Guards live at call sites, not in the shared functions.** The blank-transcript and
   stale-clobber classes are fenced where they last bit, while the shared writers (`db.rs:2653`,
   `update_segment`, v60 generic upserts) still accept the poison. Every future caller re-earns
   the bug. Move the four guards into the shared functions.
3. **The two-headed default:** one env var read in two places with two defaults (M12), one
   placeholder definition in two dialects (M11), one identity matched two ways (M2, db.rs:4515),
   thresholds in four copies (critic). Single-source-of-truth drift is this codebase's most
   reliable defect generator.
4. **Paid-work surfaces shipped after the pay canon without ledger wiring** (H5, `record_spot_check`,
   no settlement surface). The canon is airtight where it was implemented and silent where it wasn't.
5. **Meta-gates with blind spots** (M8, the `globals()` exemption, M14's skip exemption, H4's
   fictional resample). A gate that cannot catch its target is worse than no gate — it prints OK.

---

## TRUE steps to a real 10/10 — in order, nothing hidden

**The honest definition first.** Per `scripts/verify_10.py`, the literal `CORTEX 10/10: ALL GATES
GREEN` is only printable when **nothing** is descoped or owner-gated. Today 9 legs are
owner-descoped (distribution: signing, updater, stores, notarization, scorecard, signed tags —
descoped by the 2026-07-10 "personal use" amendment) and 3 legs are owner-gated on humans. So there
are exactly two honest 10/10s, and both are enumerated:

### Tier A — "GREEN — PERSONAL-USE SHIP-READY" (the reachable 10/10)

**Phase 0 — tonight, minutes, no code:**
1. `schtasks /change /tn CortexWatchdog /enable` — and add the re-enable step to whatever rebuild
   runbook disabled it (H1).
2. Write the 7 owed `PROGRESS_LEDGER.md` entries for `6731258..HEAD` — real commands and SHAs only (M9).
3. Refill hidden-check capacity: adjudicate/mint owner-verified answer keys inside the active
   focus for each reviewer's dialect, or narrow the campaign until capacity covers it. Never
   fabricate keys (live RED #3).

**Phase 1 — the 6 highs, each with its regression gate (canon: a fix without a gate is incomplete):**
4. H2: add `'halted'` to the event union, route through `refreshAfterBatch` + error toast carrying
   `haltedBy`; extend `test_champion_supremacy_policy.py` to pin the frontend half.
5. H3: `fingerprint.rehydrate(db.load_audio_identities()?)` at batch_importer startup, both modes;
   normalize path comparisons; gate: headless re-import of an already-imported file must refuse.
6. H4: carry real sample rates out of `_clip_pcm`, compare true milliseconds, actually resample
   before correlating; pin with a 48k-vs-16k same-audio fixture that must CONFIRM.
7. H5: owner decision required — either wire pool/independent decisions into the ledger (own
   `entry_key` namespace; the delta model handles it unchanged) or write canon that non-canonical
   passes are unpaid AND surface that on the phone. Either way, extend the readiness gate to see
   both tables.
8. H6: `if failed > 0 { return Err }` in `run_gold_eval_with_transcriber` (matching batch canon),
   or persist `failed`/`total_gold` in `meta_json` + UI and refuse the "normal eval" label on
   partial runs; gate: an eval with an injected failing clip must not produce a plain eval row.
9. Close the critic's unverified eval-minting lead (`gold_eval.rs:40`): verify it, and if real,
   make `run_gold_eval` compute hypotheses server-side or mark renderer-supplied rows as such.

**Phase 2 — the 15 mediums, grouped by root cause:**
10. Identity/consistency: one `names_match` helper everywhere (M2); registry-not-mirror in
    activation (M15); placeholder authority called from SQL + all three decide guards (M11).
11. Durability: non-pilot served-check SQLite reservation or 503-on-failed-persist (M1).
12. Honesty of counters/artifacts: filtered `quality_report` or explicit scope stamp (M4);
    `droppedUnavailableAudio` audio-independent counting + fix the false comment (M5).
13. Gates that can't catch: probe FAIL-vs-SKIP split (M7); unittest-aware dead-test meta-gate (M8);
    skip-exempt provenance invariants tightened to decision-bearing events (M14); span-overlap
    check per content hash in pool inventory (M13).
14. Supervisor: feed `start_child` errors into the breaker immediately (M6). Concurrency default:
    derive one number for workers and the 7B gate (M12). Frontend: `findIndex`-by-id in all four
    Inbox handlers (M10). Splits: source-recording-level splitting or train-only + honest card (M3).

**Phase 3 — the 30 lows + 2 remaining critic leads.** None blocks tonight's operation; all are
real. Recommended order: money-adjacent first (`record_spot_check` gating, settlement surface,
NOCASE unification, DEFERRED→IMMEDIATE on the settlement writer), then the shared-function guards
(disease #2), then the rest. Delete the caller-less legacy `update_segment` endpoint outright.

**Phase 4 — the standing full-sweep RED:**
15. Run one complete challenger cycle: sealed snapshot → `train_challenger.py` → trained artifact
    with byte-linked evidence → `promotion_gate.py` measured verdict. The gate needs exactly one
    honest `promotion_verdict.json`. Until then every full sweep stays RED by design.

**Exit criterion for Tier A:** `python scripts/verify_10.py` (full mode) exits 0 printing
`GREEN — PERSONAL-USE SHIP-READY`, on this machine, at the release commit, with the app in its
normal state. That line, from that harness — not this report, not any agent — is the proof.

### Tier B — the literal "CORTEX 10/10: ALL GATES GREEN" (owner-gated by design)

These need humans and owner decisions, not code:
- **`iaa-kappa-ceiling`** — recruit ≥2 independent Sorani annotators; measured κ on the frozen set.
- **`cordi-dialect-fairness`** — CORDI corpus agreement (owner item 53).
- **`refinery-lift-in-product`** — the Gold Marathon: ≥500 real review decisions through the product.
- **The 9 descoped distribution legs** — only if the owner ever re-scopes "ship" beyond personal
  use (signing, SLSA, updater, stores, HF card, notarization, Scorecard, signed tags). While the
  2026-07-10 amendment stands, Tier A **is** the honest 10/10, and claiming the literal line
  without these would be exactly the unearned completion claim this repo's law forbids.

---

## Final word

This is a codebase whose strongest parts would survive review anywhere — the money ledger, the
serving-path isolation, and the v60 write boundary are genuinely excellent, and the adversarial
pass proved it rather than assumed it. It is held back by exactly two things: **operational
discipline around the edges the gates don't watch yet** (a disabled watchdog, a drained trap pool,
an unfed ledger), and **a recurring family of single-source-of-truth drifts** that keeps
re-minting the same five bug classes the project has already fixed six-plus times. Both are
fixable. Neither is fixed tonight. The grade is 8.4 because the harness says RED and because six
verified high-severity defects sit on canon-critical paths; the path to the reachable 10/10 is the
15 numbered steps above, and every one of them ends in a gate, not a sentence.

*Sweep verdict at time of publication: appended below when the running `--quick` sweep completes.*
