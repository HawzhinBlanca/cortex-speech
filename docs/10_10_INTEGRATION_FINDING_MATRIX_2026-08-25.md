# Cortex Speech 10/10 Integration Finding Matrix

**Date:** 2026-08-25

**Branch:** `codex/10-10-integration`

**Production base:** `bd581ef` (schema 65)

**Current evidence commit:** `d21f241`

**Status:** integrated source checkpoint; **not a product or model certification**

This matrix is the authority for the audit-remediation replay onto the schema-65 production line.
An old statement that a finding was fixed is not accepted by itself: each row requires integrated
code, reachable regression evidence, and a named proof command. Release certification still
requires the immutable profile manifest and the external evidence listed under Open Gates.

## Audit-remediation replay

| Finding / invariant | Integrated commit | Regression evidence on the integrated line | Proof command / checkpoint | Status |
|---|---|---|---|---|
| A champion hard stop must be reported as a terminal operation failure, never swallowed as ordinary progress. | `86719e3` | `scripts/test_champion_supremacy_policy.py`; typed event handling in `src/lib/events.ts` | Complete Python policy sweep: 112 policy scripts passed at `dd43844` | Integrated |
| Review writes target the selected segment ID; blank annotation text cannot mask a draft; hydration/autosave may not clobber local edits. | `d2e3b0e` | `ReviewInbox.test.ts`, `ReviewMode.test.ts`, `autosave.test.ts`, `segmentStore.test.ts` | Frontend suite: 57 files / 303 tests passed | Integrated |
| A wrong-model champion probe is a failure, and every policy test file must be discoverable and executed. | `3f851a6` | `test_verify10_probe_status_policy.py`; `test_all_policy_tests_execute.py` | Complete Python policy sweep: 112 scripts passed; reachability policy passed | Integrated |
| Duplicate audio remains detectable when copies use different sample rates. | `4729ba9` | `test_check_dataset_duplicates.py`; `test_dataset_duplicates.py` | Focused duplicate proofs: 6/6 and 17/17 passed | Integrated |
| Serving provenance exempts only decision-bearing rows, model identity comes from the registry, and rights repair refuses wildcard authority. | `47e501e` | activation, serving-provenance, rights-stamp, repair and voice-focus policy tests changed in the same commit | Complete Python policy sweep passed | Integrated |
| Playback-evidenced pool work with no ledger credit is surfaced instead of silently treated as paid. | `edf8bbb` | `test_check_review_compensation_readiness.py` | Compensation readiness proof: 38/38 passed | Integrated; no payment minted |
| Halwest train/evaluation partitioning occurs at source-recording identity and transcript pairing remains provenance-bound. | `d5b6fde` | `test_halwest_split_leakage_policy.py` plus dataset builders/finalizer | Complete Python policy sweep passed | Integrated |
| Gold evaluation fails closed when any hypothesis is missing or invalid and never scores only surviving rows. | `e682c67` | Rust unit tests in `commands/gold_eval.rs` and `eval.rs` | Full Rust suite passed | Integrated |
| Hidden checks are not served unless their durable save succeeded; session/receipt and case identity checks fail closed. | `00f1f21` | Couch unit tests colocated with the changed implementation | Full Rust suite passed | Integrated |
| Placeholder authority is centralized; shared database boundaries reject blank transcript truth; money/undo mutations remain transactional. | `eec4d01` | Database unit and integration tests colocated in `db.rs`/`db_tests.rs` | Full Rust suite passed | Integrated |
| Export quality and drop counts describe the data actually included, and excluded gold cannot inflate the bundle report. | `040e04b` | Export, bundle and quality unit tests colocated with the changed code | Full Rust suite passed | Integrated |
| Immediate engine spawn failure is charged immediately; backoff, promotion fences and integrity checks cannot misreport warm-up success. | `51058eb` | ASR/runtime/supervisor unit tests colocated with the changed code | Full Rust suite passed | Integrated |
| Headless imports deduplicate across runs using canonical source identity; concurrent/blank inputs cannot bypass the GUI invariant. | `e7f75e7`, `3f6a482`, `062ba53` | `batch_importer` binary tests now initialize and migrate a disposable database before exercising cross-run dedup | Binary importer suite 6/6; full all-target Rust suite passed | Integrated |
| Pool activation refuses a doubled import generation invisible to the activation triple; runtime panics and stale validation/receipt paths fail closed. | `d759515` | `pool_admin`, command/write, correction, lock and validation regressions; `test_rust_runtime_panic_policy.py` | Full Rust suite and complete Python policy sweep passed | Integrated |
| Schema-65 flexible-pool routing remains fail closed without rewriting established compensation history. | `5bf5e6e` | schema-65 cases added to `test_check_review_compensation_readiness.py` | Compensation readiness proof: 38/38 passed | Integrated; canon-preserving |

## New release-contract slices

| Contract | Commit | Proof on exact integrated tree | Remaining boundary |
|---|---|---|---|
| Self-healing profiled verifier: typed argv gates, isolated workers, explicit timeouts, Job Objects, identity-bound leases, durable journal, immutable manifests, and non-certifying retries. | `4f30df4` | 9/9 supervisor/fault regressions; probe and abnormal-exit policies passed | Timeout calibration from three clean baselines, three complete fault campaigns, and completed profile manifests remain open |
| Generated, versioned IPC plus revision-bound review commit and exact operation-ID replay. | `df861d7` | Binding drift gate passed; full Rust, frontend and Python suites passed; strict Clippy passed | Remaining command domains still require typed migration; zero dynamic/untyped invokes is not yet reached |
| Playback is bound to clip ID and attempt ID through explicit state transitions; stale resolver, play, timer, error and ended callbacks are ignored. | `dd43844` | 10,000 deterministic randomized transitions, every declared phase visited; 19 focused audio tests; full frontend 303/303 | `AudioPlayer.svelte` still requires presentational decomposition and real-device timing/accessibility evidence |
| `DatabaseRuntime` owns serialized writes, a bounded four-connection read pool and restore admission; typed review reads and online backup use restore-gated query snapshots. | `44222f6` | 3/3 runtime regressions; focused restore-admission 2/2, named-restore 4/4 and snapshot-restore 1/1; full Rust and policy suites passed | Connection reopening, domain stores, remaining command SQL removal and the 50,000-segment concurrency/restore proof remain open |
| Segment/library/review queries are routed through a Tauri-free `SegmentQueryStore`; bounded readers use the runtime-owned live path without contending on the serialized writer mutex. | `d554d4a` | 4/4 runtime regressions, 1/1 store regression, new architecture policy, all 113 policy scripts, full Rust 1,578/0 and frontend 303/303 | Only the first query domain is migrated; write stores, remaining raw command access, connection reopening and the 50,000-segment proof remain open |
| Desktop review drafts survive navigation/restart without becoming review truth: schema v66 is additive, writes use FULL-sync revision-CAS storage, stale saves cannot resurrect cleared text, and typed commit/replay clears only the matching revision in the human-truth transaction. | `dff94a4` | 4/4 draft-store tests including injected failure; typed commit/rollback/replay tests; 3 frontend recovery/conflict/debounce tests; generated bindings and non-authority policy; full Rust 1,584/0, frontend 308/308 and all 114 Python policy scripts | Process-kill timing, power-loss behavior, long-session draft churn and full `ReviewWorkspace` decomposition remain certification gates |
| The production build enforces the initial 125 KB JavaScript and 15 KB CSS ceilings over the complete transitive static manifest; secondary workspaces load in explicit chunks with localized pending/failure states, retry, stale-load isolation and raw-error scrubbing. Preview/E2E typed review mocks are explicit and unknown commands fail loudly. | `deddfd3` | Executable oversized-transitive-dependency fault test; 2 lazy-boundary tests; frontend 310/310; Playwright 97/97 with zero retries; standalone preview traversed Insights → Settings → Review with zero console/page errors; all 115 Python policies | Cold-shell/review-usable timing, search/audio latency, long tasks, FPS and 1,000-decision heap proof remain open |
| Desktop playback evidence writes cross a Tauri-free `PlaybackWriteStore`; its observation-only database DTO cannot express review revision, audio hash, source span or authoritative duration. The database resolves all identity and coverage fields while holding the serialized writer. | `08ae275` | 3 adversarial store tests prove client duration cannot shrink coverage, missing server audio identity creates no receipt, and invalid timing creates no partial write; 16 focused playback regressions; architecture policy; full Rust 1,587/0; all 116 Python policies; strict Clippy/rustfmt | Remaining review/payment writes still use the compatibility façade; the 50,000-segment concurrent review/import/backup and restore-reopen proofs remain open |
| Desktop review-effect mutations cross a Tauri-free `ReviewWriteStore`: exact decision undo, review-flag creation, exact flag undo and the retired identity-free clear endpoint all serialize through `DatabaseRuntime`, while input/rate validation remains at the command boundary. | `78f2a1c` | 3 store regressions prove exact flag replay plus idempotent effect-bound undo, immutable-effect decision undo plus replay, and fail-closed identity-free clearing; architecture policy prevents migrated commands from regaining raw database authority; full Rust 1,590/0; all 117 Python policies; strict Clippy/rustfmt | Typed review commit and other review/payment write domains still use the compatibility façade; connection reopening, the 50,000-segment concurrent review/import/backup proof and restore-reopen proof remain open |
| Both desktop human-decision contracts cross `ReviewWriteStore`: the legacy operation-ID boundary and typed revision-CAS boundary resolve exact replay before playback preflight, derive playback identity under the serialized writer, and commit review truth plus matching draft clear transactionally. Commands retain validation and typed DTO/error mapping but cannot acquire raw database authority. | `7370068` | Existing 11 command regressions now execute through an independently opened store writer and cover exact lost-response replay, stale-revision refusal, playback enforcement and injected draft-clear rollback; 3 store regressions; architecture policy; full Rust 1,590/0; all 117 Python policies; strict Clippy/rustfmt | Other review/payment writes and Couch decomposition remain open; connection reopening, the 50,000-segment concurrent review/import/backup proof and restore-reopen proof remain open |
| Recording-scoped rights declaration, irreversible consent withdrawal and rights/provenance listing cross a Tauri-free `RightsStore`. Writes serialize through `DatabaseRuntime`; listing uses a bounded restore-gated read snapshot; commands retain validation and DTO mapping without raw database authority. | `9a43316` | 2 focused store regressions prove one declaration covers exactly the intended recording and that withdrawal survives a later metadata declaration; architecture policy covers all 3 commands; full Rust 1,592/0; all 118 Python policies; generated-binding drift, strict Clippy and rustfmt passed | Remaining segment/job/import stores and Couch decomposition remain open; connection reopening, the 50,000-segment concurrent review/import/backup proof and restore-reopen proof remain open |
| Interrupted-import discovery/discard and Job Center recent-job reads cross a Tauri-free `JobStore`. Discovery and history use bounded restore-gated snapshots; discard remains serialized through `DatabaseRuntime`; all four startup/resume/job handlers retain rate and identifier validation without raw database authority. | `6e72bcc` | 2 store regressions prove bounded newest-first history and read/discard of interrupted import state; architecture policy covers `get_interrupted_import`, `discard_interrupted_import`, `resume_interrupted_import` and `get_jobs`; full Rust 1,594/0; all 119 Python policies; generated-binding drift, strict Clippy and rustfmt passed | Pipeline/background job writers and import orchestration still use the compatibility façade; segment mutations, Couch decomposition, connection reopening, the 50,000-segment concurrent review/import/backup proof and restore-reopen proof remain open |
| Segment deletion, batch deletion and speaker rename cross a Tauri-free `SegmentWriteStore`. Deletes read the authoritative server rows before removal, push exact undo history only after successful deletion, and return an admission token that keeps restore fenced through command-side session autosave. The retired whole-row command now refuses before acquiring database authority. | `d21f241` | 2 store regressions prove exact raw-transcript/speaker restoration and shared serialized batch-delete/rename behavior; architecture policy covers all 3 migrated commands plus the retired endpoint; runtime-panic policy follows the new boundary and requires batch-read/delete error propagation; full Rust 1,596/0; all 120 Python policies; strict Clippy and rustfmt passed | Field-level segment mutations, pipeline/background job writers, import orchestration and Couch decomposition remain open; connection reopening, the 50,000-segment concurrent review/import/backup proof and restore-reopen proof remain open |

## Integrated checkpoint evidence

- `cargo test --all-targets --all-features`: 1,596 library tests passed, 0 failed, 8 explicitly ignored; all integration, soak, binary and benchmark targets exited 0.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- Frontend: 58 files / 310 tests passed; typecheck reported 0 errors and 0 warnings; lint and formatting passed.
- Browser E2E/accessibility: 97/97 passed with zero retries against a gate-owned Vite server.
- Python policies: all 120 reachable policy scripts passed sequentially, including generated-binding drift.
- Production build and its fail-closed manifest budget passed. Initial JavaScript is **111.19 KB gzip**
  against the 125 KB ceiling; initial CSS is **11.20 KB gzip** against the 15 KB ceiling. The gate
  recursively counts the entry and all static imports, excludes on-demand chunks, and was proven to
  reject an oversized transitive dependency. This closes only the declared bundle-size slice, not the
  remaining startup, latency, interaction, scrolling or memory budgets.
- Migrations 1–65 are byte-identical to production base `bd581ef` (normalized catalog SHA-256
  `c47d4be689871b8191c13d96a59dd502a9de4d8868788f8cd9cfa4efca7cc2e3`).
- Work used disposable databases only. The active database and immutable release pointer were not modified.

The eight ignored Rust diagnostics include live/model-dependent evidence and are not counted as
certification passes. A final proof run may not contain unexplained ignores, retries, skips, stale
evidence, lock recovery, or manual status changes.

## Canon boundary

> [!WARNING] Contradiction
> The requested plan says external flexible-pool submissions should return
> `PAY_POLICY_REQUIRED`. The active owner-FINAL schema-65 canon says the flexible pool is active and
> compensation is deferred. This integration preserves schema-65 authority. No pool shutdown,
> compensation backfill, or payment mutation will be made without the literal owner instruction
> `change canon: <item>`.

## Open gates preventing any 10/10 verdict

- Backend store/runtime decomposition and the 50,000-segment concurrency/restore proof are incomplete.
- Frontend workstation decomposition, complete typed IPC migration, typed i18n keys, responsive and
  WCAG 2.2 AA manual evidence, plus runtime performance/memory budgets are incomplete. Initial bundle
  size is now green and build-enforced.
- Three clean calibrated verifier runs and three complete verifier fault campaigns are incomplete.
- Owner workflow, crash/recovery, two-hour/1,000-decision soak, cold-reboot proof, and 30 daily-use
  sessions have not been completed on the release candidate.
- Signed installer, clean Windows 11 VM update/rollback/uninstall proof, NVDA/manual accessibility,
  eight-participant comparator study, five-user pilot, and stable rollout are external and absent.
- Model evidence remains a separate incomplete profile. Historical accuracy numbers are not promoted
  by this source integration.

Therefore the only honest verdict at `d21f241` is **INTEGRATION IN PROGRESS — NOT CERTIFIED**.
