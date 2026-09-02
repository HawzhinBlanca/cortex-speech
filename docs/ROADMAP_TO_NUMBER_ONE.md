# Roadmap to #1 — deep audit, brutal rating, and the plan to the highest reliable/professional grade

> [!WARNING]
> **Historical audit/roadmap, not current runtime direction.** Owner canon now pins production to the
> OmniASR-7B WSL champion and removes Scribe; any older item below proposing Scribe, cloud STT, stock
> CTC/MMS production use, or an offline fallback is rejected. Preserve its dated findings as audit
> provenance only. See [`OWNER_CANON.md`](OWNER_CANON.md).

**Date:** 2026-07-24 · **Branch:** codex/newbranch · **Method:** a 12-agent adversarial Workflow
(9 subsystem auditors + 3 web-research agents), every HIGH/MED finding put through independent
refutation, then the **top findings hand-verified against source by the orchestrator** (agent
verdicts are not evidence — this repo's law). 66 agents, ~5.5M tokens, 0 died.

This is an audit + plan, not a set of applied fixes. Nothing below is "done." Every defect cites
`file:line`; findings I read myself are marked **[HAND-VERIFIED]**, the rest **[agent-reported —
confirm before fixing]**. No metric here is invented.

---

## 1. Scorecard — maturity vs *top professional tools* (VS Code / JetBrains / Prodigy-grade bar)

This is a deliberately harsh bar (not "vs hobby projects"). 5–6 = solid indie; 8+ = would embarrass a commercial tool.

| Dimension | Score | One-line reality |
|---|:--:|---|
| Storage durability | **8/10** | Strongest dimension. WAL in one factory, boot integrity checks that refuse to nuke a healthy DB, tiered auto-snapshots, pinned pre-migration/pre-restore copies, STRICT tables actually migrated. |
| Panic / crash paths | **8/10** | CI-enforced `unwrap/expect` ban (one justified exception), poison-recovering locks *with tests that poison them*, `catch_unwind` belts on every user-facing worker. The 2193 unwraps are test code. |
| IPC surface | **7/10** | 128 commands, 123 route through central validators with rare UNC/NTLM awareness, eager consent gates, systematic anti-clobber writes. But discipline is copy-paste, not structure — and it shows. |
| Pipeline core | **7/10** | Rollbacks, no-silent-downgrade, blank-transcript guards, subprocess reaping better than most commercial tools. But assumes a healthy models dir and lies about diarization provenance. |
| Frontend robustness | **7/10** | Best indie-desktop frontend at this scale: generation guards, flush-before-rekey autosave, freshRow-by-id, wrong-segment bail-outs. Still not honest when the library itself fails to load. |
| Eval honesty | **6.5/10** | Real honesty culture (verbatim ledgers, fail-closed holdout hashing, external jiwer crossval) — but the flagship 3-engine SOTA table mixes normalization bases and a single env var flips the basis untraced. |
| Test quality | **6.5/10** | Genuinely behavioral DB/export tests + a real CER-gated ASR check in CI. But the "41 policy gates" are mostly a museum of past bugs; the IPC product surface is triple-mocked and blind to contract drift. |
| Security / privacy | **6/10** | Layered consent gates, exact-host loopback parsing, https-or-loopback allow-lists, header-only keys — well above hobby grade. Punctured by a plaintext key store advertised as DPAPI and one missed UNC guard. |
| Ops professionalism | **6/10** | Superbly hardened dev-box *appliance*. But factory-default engine points at `/home/ai` on the owner's WSL box, fatal startup errors evaporate, logs grow forever, no user docs. |
| **Weighted average** | **6.9/10** | A professional-grade *core* wrapped in unfinished edges the project's own standards say must be closed. |

**Honest headline:** this is not a 6.9 hobby app with delusions. It is a **genuinely hardened core
(durability + crash-safety are 8s that beat commercial tools) dragged down by a distribution/honesty
tail.** The gap to "10" is narrow but it runs straight through the project's one law.

---

## 2. Rating against the top 3 *reliable & robust* peers

**Category peers (speech/data review & curation):** Prodigy (Explosion), Label Studio (HumanSignal),
CVAT. **Professional-reliability exemplars (the bar):** VS Code, Obsidian.

### Where cortex-speech already BEATS all three category leaders
The #1 documented failure class of every one of them is **losing in-flight work** (web-sourced):
- **Prodigy** loses the un-submitted browser batch on close (support forum, confirmed).
- **Label Studio** loses annotations on network blips (GH issues #6674/#6675: up to ~50% missing).
- **CVAT** ships auto-save **OFF** by default and tells users to press Ctrl+S "to avoid data loss".

Cortex persists every human decision transactionally with freshRow-by-id + generation guards +
tiered auto-snapshots + pinned pre-migration/pre-restore copies. **On durability of review labor it
clears the exact bar all three trip on.** None of the three ships automatic scheduled local backup;
cortex does. That is a real, defensible #1 claim *in its category's hardest dimension*.

### Where it LOSES to the top 3 (as a product)
- **Distribution/updater/signing** — Prodigy pip-wheel, LS/CVAT containerized installs "just work" on
  a fresh machine. Cortex's factory default engine is machine-specific (`/home/ai/...`), and there is
  no signed installer/updater. *(Owner scoped distribution out — see §6 — so this is a product-parity
  gap, not a ship blocker.)*
- **User documentation & ergonomics polish** — Prodigy is the keyboard-review benchmark (cortex
  matches its single-key map, genuinely) but LS/Prodigy ship user docs, hotkey editors, multi-select.
  Cortex has 30+ *engineering* docs and zero end-user docs.
- **IPC contract safety** — none of the three hand-maintains a triple-mocked command surface; the
  mature ones generate their client bindings. Cortex's suite is structurally blind to contract drift.

### Where it LOSES against its OWN one law (the damning column)
Honesty is the whole creed, so these outrank everything above:
1. The pinned **7.03/9.32/11.34 3-engine SOTA table** compares engines on **different normalization
   bases** (stock-300M scored with full Sorani folding; others bare NFC+lower) — the p<1e-131
   significances are cross-basis artifacts of unknown magnitude.
2. Exported **`runConfig` asserts `diarization=true`** even when zero speakers were labeled.
3. A **DPAPI-protected key store advertised in source comments while keys are written in plaintext.**

A tool whose credibility *is* honesty cannot rate #1 while these stand. **They are the top of the plan.**

### ASR results positioning (web-verified, honest)
- **CER 7.03% on FLEURS ckb_iq test (N=922 = full official split):** *no published FLEURS-ckb CER
  from any system is lower.* Nearest anchors — Meta Omnilingual ASR 7B reports CER **6.0** for
  `ckb_Arab` on its **own (unspecified, likely easier read-speech) eval**, not FLEURS; academic Sorani
  CERs are 9.8–13.1% on easier in-domain sets. Defensible claim: **"best published CER on FLEURS
  ckb_iq"** — never "beats Meta" (different test set).
- **WER 32.93%:** a statistical **tie** with ElevenLabs **Scribe v1's published 32.1%** on the same
  benchmark; clearly beats Gemini 2.0 Flash (43.5%), Whisper large-v3 (99.1% — Kurdish unsupported),
  Deepgram (100%). **Caveat:** Scribe **v2** claims a 10–20% WER tier for macro "Kurdish" — if real
  for Sorani-on-FLEURS it would beat 32.93%. **Unverified; measure locally before any WER-leadership
  claim.** (Cloud STT stays consent-gated; Scribe is an approved provider.)

---

## 3. Confirmed defect ledger (fix targets, ranked)

Severity is post-refutation. **[HV]** = hand-verified against source by orchestrator today.

### Honesty-law breaches — these are the creed, fix first
| # | Sev | Location | Defect |
|---|:--:|---|---|
| H1 | HIGH | `docs/MEASUREMENTS.md:13` + `real_audio.rs:815` + `scorecard_7b.py:63` **[HV]** | 3-engine table claims "identical normalization for all engines" but stock-300M is scored through full Sorani-folding `normalize_for_metrics` while 7B/finetuned use bare NFC+lower; and `CORTEX_CER_STRIP=1` silently flips the champion basis with **zero trace** in headline/TSV/JSON. The pinned SOTA comparison + its MAPSSWE p-values are cross-basis. |
| H2 | HIGH | `runs.rs:175` **[HV]** | `runConfig.diarization = settings.enable_diarization` with **no CAM++ loadability check** (one line above, `denoising` *is* guarded). CAM++ absent + flag on → zero speaker labels but bundle asserts `diarization=true`. |
| H3 | MED | `export_bundle.rs:397` | `runConfig` (incl. the *honest* denoising flag) is computed at **export time** from *current* model loadability, then stamped onto segments imported months earlier under different state. Temporal inverse of the #132 fix. No per-segment provenance exists to correct it. |
| H4 | MED | `commands/settings.rs:84` + `api_keys.rs:92` **[HV]** | `set_api_key` calls plaintext `save_key`; the DPAPI `save_key_protected` is built + unit-tested but has **zero production callers**. Keys sit cleartext in `%APPDATA%\cortex-speech\secrets.env`. Capability theater. |
| H5 | MED | `MEASUREMENTS.md:13` | "known-disjoint" FLEURS claim is asserted, never verified — no train/test overlap check exists between FLEURS ckb test and the 7B-LoRA / MMS training corpora (both plausibly FLEURS-derived). SOTA framing outruns the evidence the repo holds. |

### Security / reliability defects — small, high-value
| # | Sev | Location | Defect |
|---|:--:|---|---|
| R1 | HIGH | `commands/segments_read.rs:128` → `db.rs:1324` **[HV]** | `relink_audio` guards only null bytes, hands raw `search_dir` to `is_file()` → a UNC `\\attacker\share` drives the SMB redirector (NTLM forced-auth leak) and on a basename match **persists** the UNC path. The exact #131 class, missed on this sibling. |
| R2 | HIGH | `lib.rs:801` **[HV]** | `fatal_app_error` = `eprintln!` + `exit(1)`, no dialog. With `windows_subsystem="windows"` every fatal startup path (instance lock, unopenable DB, data-dir create fail, **newer-schema refusal** with its user-directed message) is a silent "double-click, nothing happens." |
| R3 | HIGH | `lib.rs:335` | Restore writer fence covers only import/batch/WSL — **jury, Scribe-vote, and couch writers run on unfenced connections** and can write into a just-restored library (identical segment ids on same-library restore). Same torn-restore class already fixed twice. Plus `prepare_restore` is check-then-act with no reservation (`commands.rs:1648`). |
| R2b | MED | `db.rs:205` + `segments_write.rs:66` | `merge_dataset_json` and `restore_segment_snapshot` accept `audio_path` verbatim (no `validate_file_path`) → renderer can plant `\\attacker\share` paths that a dozen downstream `exists()`/decode consumers touch. Same trust-boundary hole as R1. |
| R4 | HIGH | `asr.rs:598` (+ `pipeline.rs:1671` streaming) | A present-but-corrupt/unloadable ONNX model turns "retry when unavailable" into a **full SHA-256 recompute of a 300 MB–1.4 GB file + ONNX load attempt on every call** (per chunk, twice per segment; per 90s window in streaming). No backoff/latch. Comment calls it "a cheap existence probe" — only true when absent. |

### Frontend honesty & robustness
| # | Sev | Location | Defect |
|---|:--:|---|---|
| F1 | HIGH | `stores/segmentStore.ts:74` | Library load failure swallowed to `console.error` — UI renders "No segments loaded" empty state, making a **DB/IPC read error indistinguishable from a wiped library** in an app whose law is honesty. (Impact softened: prior state survives a mid-session reload — but first-load failure = phantom empty library.) |
| F2 | MED | `App.svelte:2821` + `ReviewMode.svelte:577` | Whole-row-upsert clobber (fixed 6×) still open on two paths: Normalize fires mid-batch with no `$isProcessing` guard (reverts freshly-written batch columns); review-unmount draft flush omits the `aligning` guard (reverts real CTC timings to the energy heuristic). |
| F3 | MED | `ErrorBoundary.svelte:43` | **No `unhandledrejection` listener anywhere** — every rejected fire-and-forget IPC promise vanishes with no user trace. The repo's own docs list this as an open TODO. |
| F4 | MED | `RefineryPanel.svelte:121` (+ ModelRegistry, DiagnosticsPanel, ~25 shortcut descriptions, HistoryPanel) | Zero `$t()` on core Insights/Settings surfaces + hardcoded-English command palette & shortcut help → the **default-locale Kurdish reviewer gets a mixed-language UI** on primary panels. |

### Test-suite structural blind spots
| # | Sev | Location | Defect |
|---|:--:|---|---|
| T1 | HIGH | `e2e/helpers/tauri-mock.ts:304` | IPC contract drift is **structurally uncovered**: Playwright runs against a hand-mock covering ~45/132 commands whose default branch returns `null`; nothing diffs frontend `invoke()` names/shapes against the Rust `generate_handler!` registry. A renamed command stays green in vitest + Playwright + cargo test simultaneously. |
| T2 | MED | `scripts/test_cloud_privacy_policy.py:94` + `test_command_main_thread_policy.py` | Policy gates are **floor/enumeration checks, not inventories**: a NEW third cloud-egress command with no consent gate keeps the count at 2 and passes green; a new sync slow command not in the hand-list sails through. The gates harden the past, not the future. *(Companion whole-surface audits exist for some classes — verify coverage per class before trusting.)* |

*(Full LOW list and per-finding refutation reasoning are in the run journal:
`…/subagents/workflows/wf_54f9cedb-461/journal.jsonl`.)*

---

## 4. The ultimate plan — to #1 reliable & professional, at the honesty bar

Sequenced so each tier is independently shippable and adversarially gated. Reliability-first,
**no new feature surface** (owner directive) — every item closes a defect or hardens a proof.

### Tier 0 — Restore the one law (do first; small, surgical)
- **P0.1 Re-pin the engine table on ONE basis (H1).** Decide the canonical normalization for the
  cross-engine comparison, re-score all engines on it (owner-gated GPU run), and re-write
  `MEASUREMENTS.md` verbatim. **Stamp the normalization basis into every scorecard's headline + TSV +
  JSON**, and make `run_measurements.py` record `CORTEX_CER_STRIP` in the ledger. Add a policy test
  that fails if a scorecard emits a metric without a basis tag. *Until re-scored, annotate the current
  table as cross-basis / not-directly-comparable — the honest interim state.*
- **P0.2 Diarization provenance guard (H2).** One-line sibling of the denoising fix: thread a
  `diarization_active` (CAM++ loadable) param through `config_from_settings`; caller passes it.
  Regression test: flag on + model absent ⇒ `runConfig.diarization == false`.
- **P0.3 Wire the DPAPI you already wrote (H4).** Point `set_api_key` at `save_key_protected`; keep a
  read-fallback for existing plaintext files with a one-time re-encrypt-on-read. Fix the comments to
  state the real posture. Regression test: written key file is not readable plaintext.
- **P0.4 Per-segment processing provenance (H3).** Persist per-row `denoised`/`diarized`/`vad_backend`
  at import; export reads the stored truth instead of recomputing from export-day model state. Larger
  than P0.1–3 — schema migration — but it closes the whole "export-time provenance lie" class for good.

### Tier 1 — Close the security/reliability holes (small diffs, high blast-radius)
- **P1.1 UNC guard on `relink_audio` + `merge_dataset_json` + `restore_segment_snapshot` (R1, R2b).**
  Route every renderer-supplied path through `validate::validate_output_path` / `validate_file_path`.
  Then make the class **unrepresentable**: a validated-path newtype so a command *cannot* take a raw
  path string. Body-scan policy test (see P3.2).
- **P1.2 Native fatal-error dialog (R2).** `fatal_app_error` raises a `MessageBoxW` (win32) / dialog
  fallback before exit so locked-instance / unopenable-DB / newer-schema refusal is *seen*. Pairs with
  adopting `tauri-plugin-single-instance` to focus the existing window on second launch.
- **P1.3 Invert the restore writer fence (R3).** Replace the growing `||` chain with **one writer
  registry** (every writer — import, batch, WSL, jury, Scribe, couch — registers/deregisters);
  `writers_active()` reads the registry, and `prepare_restore` sets a **restore-pending reservation**
  that `try_start_*` and the pipeline writers honor (closes the check-then-act gap).
- **P1.4 Model-load circuit breaker (R4).** A failure latch / backoff so a present-but-corrupt or
  unloadable model fails fast once instead of re-hashing gigabytes or re-attempting ONNX load every
  chunk/window. Surface the degraded state to the health panel.

### Tier 2 — Frontend honesty + safety net
- **P2.1 Honest library-load failure (F1).** Distinct error state + retry affordance; never render a
  read error as an empty library.
- **P2.2 Global `unhandledrejection` trap (F3).** Route async-IPC rejections to the notification
  system + the (new) error history. Closes the "errors vanish into console" class wholesale.
- **P2.3 Guard the last two clobber paths (F2).** `$isProcessing`/in-flight guard on Normalize;
  `aligning` guard on the review-unmount flush; both switch to freshRow-by-id.
- **P2.4 i18n the core CKB surfaces (F4).** RefineryPanel, ModelRegistry, DiagnosticsPanel, the
  shortcut/command-palette help, and HistoryPanel entries. Add a policy test: no bare English string
  literal in a rendered `<h*>/<button>/aria-label` outside the i18n system.

### Tier 3 — Turn discipline into structure (so the future is guarded, not just the past)
- **P3.1 Generated IPC contract (T1).** Adopt `tauri-specta`/`ts-rs` to generate `commands.ts` types +
  the e2e mock's command set from the Rust `generate_handler!` registry — one source of truth, kills
  contract drift across vitest/Playwright/cargo.
- **P3.2 Generic body-scan policy gates (T2).** Replace enumerations with structural rules over *every*
  `#[tauri::command]`: touches cloud/audio ⇒ must call a consent helper; path param ⇒ must be the
  validated newtype; body > N lines / heavy call ⇒ must be `async`+`run_blocking`. Gates that harden
  the *next* command, not the last bug.
- **P3.3 Coverage ratchet + mutation testing.** `cargo-mutants` on the durability/honesty core
  (db.rs, eval.rs, pipeline persist paths) + a frontend coverage floor. Measure whether the ~1100
  tests actually kill faults — today's honest catch-probability for a new data-loss bug is ~55% in
  db.rs, ~25% in a command/store (agent estimate; treat as a hypothesis to measure).
- **P3.4 Full import→review→export e2e in CI.** The one loop the suite never runs end-to-end; the
  `e2e_real_app.cjs` driver exists but is kept off CI. Wire it against a disposable profile.

### Tier 4 — The "results" half: earn the SOTA claim honestly
- **P4.1 Clean re-score of the deduplicated 348-clip FLEURS set (owner-gated).** Every pinned headline
  is still duplication-weighted with an admittedly-too-narrow CI. Re-score on the P0.1 basis, re-pin.
- **P4.2 Contamination check (H5).** Build the text + audio-hash overlap check between FLEURS ckb test
  and the 7B-LoRA / MMS training corpora. Either prove disjointness or caveat the claim honestly.
- **P4.3 Measure Scribe v2 + Google Chirp/Chirp-2 on the frozen set.** Settle the one open competitive
  question (Scribe v2's "Kurdish 10–20% WER" tier). Consent-gated; Gemini-2.5-Pro/Scribe only per policy.
- **P4.4 Ship chunk-overlap stitching behind an A/B flag.** It's designed + tested + **unused**
  (`chunking.rs`: "nothing calls this yet"). Wire it, prove stitched ≥ unstitched on gold, ship only
  on measured non-regression. Closes the boundary-word/7B-duplication class properly.

### Suggested execution order for the nightly loop
`P0.2 → P0.3 → P1.1 → P1.2 → P0.1(prep) → P1.3 → P2.1 → P2.2 → P1.4 → P0.4 → P2.3 → P2.4 → P3.2 → P3.1 → P3.3 → P3.4 → [owner-gated P0.1 re-score, P4.*]`
Rationale: cheapest honesty + security wins first (P0.2/0.3/P1.1/P1.2 are hours each and verified),
then the structural fences, deferring the GPU-bound re-scores to owner-gated slots.

---

## 5. What is genuinely excellent (do not "fix", do not regress)
- One-factory WAL/pragma discipline + boot integrity check that refuses to nuke a healthy DB.
- Tiered auto-snapshots (10+7+4) with atomic staging, off-drive copy, pinned pre-migration/pre-restore.
- CI-enforced `unwrap/expect` ban + poison-recovering locks with tests that deliberately poison them.
- `catch_unwind` + RAII state-reset belts on every user-facing worker.
- Layered consent gates + exact-host loopback parsing + https-or-loopback allow-lists + header-only keys.
- Generation-guard / freshRow-by-id / wrong-segment-bail frontend concurrency discipline.
- Verbatim-only MEASUREMENTS ledger with SHA pins and struck-through retractions.
These are why the core rates 8. The plan protects them.

---

## 6. Owner-gated / out-of-scope (surfaced, never faked)
- **Distribution: auto-updater, code signing, macOS, stores** — owner scoped out (CLAUDE.md: "ship =
  personal use"). Real product-parity gaps vs the top 3; **not ship blockers** and not in the plan's
  critical path. If personal use ever becomes distribution, Tier-5 = `tauri-plugin-updater` (mandatory
  minisign signing) + Azure Trusted Signing + `sentry-rust-minidump` crash pipeline.
- **GPU re-scores (P0.1, P4.1–4.3)** — need the owner's dual-3090 rig + WSL 7B server; loop prepares
  the harness/manifests to the run boundary, owner executes.
- **Native Sorani review, consent opt-ins, retrain promotion (≥500 human decisions)** — unchanged.

---

## 7. The honest bottom line
Cortex-speech is **already #1 in its category on the dimension that category is worst at** (durability
of review labor) and holds a **plausibly-best-published CER on FLEURS ckb**. It is not #1 overall
because three honesty breaches sit at the heart of a project whose only law is honesty, and because a
handful of small reliability holes (one missed UNC guard, invisible fatal errors, an incomplete
restore fence) remain in an otherwise superbly hardened core. **None of the blockers are large.** Tier
0 + Tier 1 — days of surgical work, all located to `file:line` above — move it from "superbly hardened
dev-box appliance with an honesty asterisk" to "honestly, verifiably #1 in its class." The distance to
the top is short; it just runs through the creed, not around it.
