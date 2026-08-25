# Cortex Speech 10/10 Integration Finding Matrix

**Date:** 2026-08-25

**Branch:** `codex/10-10-integration`

**Production base:** `bd581ef` (schema 65)

**Current evidence commit:** `44222f6`

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

## Integrated checkpoint evidence

- `cargo test --all-targets --all-features`: 1,576 library tests passed, 0 failed, 8 explicitly ignored; all integration, soak, binary and benchmark targets exited 0.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- Frontend: 57 files / 303 tests passed; typecheck reported 0 errors and 0 warnings; lint and formatting passed.
- Python policies: all 112 reachable policy scripts passed.
- Migrations 1–65 are byte-identical to production base `bd581ef`.
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
- Frontend workstation decomposition, complete typed IPC migration, i18n parity, responsive,
  WCAG 2.2 AA manual evidence, and performance/memory budgets are incomplete.
- Three clean calibrated verifier runs and three complete verifier fault campaigns are incomplete.
- Owner workflow, crash/recovery, two-hour/1,000-decision soak, cold-reboot proof, and 30 daily-use
  sessions have not been completed on the release candidate.
- Signed installer, clean Windows 11 VM update/rollback/uninstall proof, NVDA/manual accessibility,
  eight-participant comparator study, five-user pilot, and stable rollout are external and absent.
- Model evidence remains a separate incomplete profile. Historical accuracy numbers are not promoted
  by this source integration.

Therefore the only honest verdict at `44222f6` is **INTEGRATION IN PROGRESS — NOT CERTIFIED**.
