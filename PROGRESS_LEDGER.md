# Cortex Speech — Progress Ledger

> **CURRENT PRIVATE-PRODUCTION AUTHORITY — 2026-08-24:** The June Wave-0 scorecard and “Current
> Focus” sections immediately below are historical charter lineage, not the live reviewer-system
> verdict. Current operational authority is
> [`cortex-speech-app/docs/PRIVATE_PRODUCTION_10_COMPLETION_AUDIT_2026-08-24.md`](cortex-speech-app/docs/PRIVATE_PRODUCTION_10_COMPLETION_AUDIT_2026-08-24.md),
> the active immutable release pointer, and the generated schema-2 pool certification. The active
> reviewer line is release `8ef5d3c1b29e-8a999c88e220-2ad63448136e`; it is review-ready, while the
> human-reviewed dataset is correctly not final. Never use an older score, machine-state paragraph,
> hidden-check requirement, model route, focus, or roster below as current production instruction.

> **2026-08-25 — EVIDENCE-BACKED 10/10 INTEGRATION IN PROGRESS (NOT CERTIFIED):** Branch
> `codex/10-10-integration` was created from schema-65 production commit `bd581ef`; the intent of the
> 15 audit-remediation commits from `ea2ec3d` through `889672c` was replayed individually, preserving
> migrations 1–65 and the flexible-pool authority. Conflict integrations retain the production
> vectorized duplicate graph while comparing mixed sample rates, retain pool/staging isolation, and
> retain every audit regression. The commit-to-regression map is
> [`docs/10_10_INTEGRATION_FINDING_MATRIX_2026-08-25.md`](docs/10_10_INTEGRATION_FINDING_MATRIX_2026-08-25.md).
> Measured checkpoint at `7f173fa`: duplicate policies **6/6 + 17/17**, frontend **310/310** across
> 58 files, browser E2E/accessibility **97/97** with zero retries, all **121** reachable Python policy scripts, compensation readiness **38/38**, strict
> Clippy **PASS**, rustfmt **PASS**, and the exact all-target/all-feature Rust command exited 0 with
> **1,612 library tests passed**, 0 failed and 8 explicitly ignored plus green integration, soak,
> binary and benchmark targets. Migrations 1–65 are byte-identical to `bd581ef`. The importer
> fixture now executes the production `Database::initialize` boot step and its binary suite is
> **6/6**. The typed/profiled verify-10 supervisor now has explicit argv/substeps (no `shell=True`),
> per-gate timeouts, streamed
> immutable attempt logs, Windows kill-on-close Job Objects, PID-creation/token/SHA leases with 5 s
> heartbeat and 30 s stale threshold, fsynced event sequencing, non-certifying
> `PASS-AFTER-RETRY`, hash-validated manifests and post-validation latest/status publication. Its
> current fault regressions are **9/9** (concurrency, dead/PID-reused/wedged leases, child+grandchild
> timeout, evidence-write failure, isolated worker and manifest binding). Generated versioned IPC and
> revision-bound review commits landed at `df861d7`; attempt-bound audio with a 10,000-transition
> randomized state-machine proof landed at `dd43844`. The first backend strangler slice landed at
> `44222f6`: `DatabaseRuntime` now owns the serialized writer, a bounded four-connection read pool,
> and restore admission; typed review reads and backup use restore-gated read snapshots, and the
> runtime's four focused regressions prove capacity release, restore draining, writer-independent
> reads and exact online backup migration history. `d554d4a` adds the first Tauri-free domain store: ten
> segment/library/review read handlers now use bounded `SegmentQueryStore` snapshots, readers no
> longer take the serialized-writer mutex just to discover the live database path, and a new
> architecture policy forbids those handlers from regaining raw database authority. `dff94a4`
> appends schema 66 for FULL-sync, revision-bound, non-authoritative desktop drafts; stale writes
> cannot resurrect cleared work, matching drafts clear atomically with typed human decisions and
> lost-response replays, stale drafts are shown beside server truth without automatic merge, and
> exports/evaluation/payment/serving cannot query the draft table. `deddfd3` isolates Review,
> Settings, Refinery, validation, review inbox, keyboard help, command palette, speaker, merge and WSL
> workspaces behind literal dynamic imports with localized loading/failure, retry, stale-load isolation
> and raw-error scrubbing. Both standalone preview and E2E mocks implement the typed review page/draft
> contract and reject unknown commands instead of returning deceptive `null`. The production build now
> walks Vite's complete transitive static manifest and fails over **125 KB JavaScript / 15 KB CSS**;
> its exact result is **111.19 KB JS gzip and 11.20 KB CSS gzip**. A synthetic oversized transitive
> dependency is rejected, so this is an enforced bundle gate rather than a copied measurement.
> `08ae275` adds the first serialized review-adjacent write store: the desktop playback handler now
> validates and delegates to a Tauri-free `PlaybackWriteStore`, whose observation DTO cannot carry a
> client-authored revision, audio hash, source span or authoritative duration. The database resolves
> those values and coverage under the writer lock. Three adversarial store tests prove a one-millisecond
> claimed duration cannot inflate coverage, missing server identity writes no receipt, and invalid
> timing leaves no partial row; the full playback selection passed 16/16 and the architecture policy
> prevents the command from regaining raw database authority. `78f2a1c` adds a second serialized
> write boundary: decision undo, review-flag creation, exact flag undo and the retired identity-free
> clear endpoint now delegate through a Tauri-free `ReviewWriteStore`. Three focused regressions pin
> exact response-loss replay, payload-conflict refusal, one immutable reversal per effect, idempotent
> undo, and continued refusal of authority-free clearing; the architecture policy prevents those
> four migrated commands from regaining raw database access. `7370068` expands that same serialized
> boundary to both desktop decision contracts. Legacy operation-ID replay and typed revision-CAS
> replay are resolved before current playback preflight; playback identity is derived under the
> writer lock; typed draft clearing remains in the human-truth transaction; stale revisions retain
> structured public error details; and navigation cursor persistence no longer gives either command
> raw database authority. The existing 11 command regressions now execute through the store and pin
> lost-response replay, playback enforcement, stale-revision refusal and injected draft-clear
> rollback. `9a43316` adds a Tauri-free `RightsStore`: recording-scoped declarations and irreversible
> consent withdrawal serialize through `DatabaseRuntime`, while rights/provenance listing uses a
> bounded restore-gated read snapshot. Commands retain path/text validation and public DTO mapping
> without raw database access. Two store regressions prove declaration scope and that a later metadata
> declaration cannot resurrect withdrawn consent; a new architecture policy covers all three commands.
> `6e72bcc` adds a Tauri-free `JobStore`: interrupted-import discovery and recent Job Center history
> use bounded restore-gated snapshots, while interrupted-import discard stays serialized behind
> `DatabaseRuntime`. Startup, resume and recent-job commands retain their rate/identifier validation
> but no longer acquire raw database authority. Two store regressions and a four-command architecture
> policy pin bounded newest-first reads and durable read/discard behavior. `d21f241` adds a Tauri-free
> `SegmentWriteStore`: single and batch deletion capture exact server rows for undo before successful
> removal, speaker rename uses the serialized writer, and commands retain input/rate validation without
> raw database access. The deletion store returns an admission token held through session autosave, so
> restore cannot split the database mutation from matching session persistence. Two store regressions
> prove exact raw-transcript/speaker restoration and shared batch-delete/rename serialization; the
> architecture and runtime-panic policies require delegation and explicit database error propagation.
> The retired whole-row endpoint now refuses before acquiring database authority. `20148bb` completes
> the active desktop segment-mutation slice by routing field-level updates through the same store. The
> store now owns the curation whitelist, structural alignment validation, schema-60 human-truth guard and
> exact history persistence; mixed restricted payloads refuse before mutation, and the command keeps
> the restore-admission token alive through session autosave. Four focused store regressions plus the
> expanded architecture policy passed on the same tree. `a0fb4df` expands `JobStore` to own the
> tracked lifecycle of plain and Hugging Face dataset exports.
> Both IPC commands still validate/rate-limit and dispatch off the UI thread, but no longer receive a
> raw database handle or drive queued/running/terminal transitions. Restore admission now spans the
> durable job and output publication. Four store regressions include a real disposable JSON export
> ending in `succeeded` and an injected failure preserving its exact error plus stable failure code;
> both architecture policies forbid regression. `888a2bd` completes the shipped export-command slice:
> transcript, production bundle, reviewed-audio, gold-eval and fine-tune-pack exports now use the same
> store-owned serialized database and durable job lifecycle. IPC retains rate/identifier/path checks
> plus immutable settings, model-manager and ledger-path capture; every export receives a stable
> domain-specific terminal failure code, and `commands/export.rs` contains no raw database or
> job-transition authority. A fifth store regression publishes a real disposable transcript artifact
> and proves its durable succeeded state. `7116887` moves the import recovery journal behind that same
> `JobStore`: begin, per-file progress and completion now serialize through `DatabaseRuntime`, while
> desktop production injects the exact runtime managed by `AppState`. Journal creation fails before
> any clip can publish; progress or completion failure halts the import and leaves a visible running
> journal for exact resume instead of reporting false success. Three focused regressions prove the
> exact lifecycle, idempotent file stamps, progress/completion fault visibility and pre-decode refusal.
> `1c443d9` adds a Tauri/HTTP-free `ImportWriteStore` for atomic segment-batch publication, exact
> rollback and revision-CAS background alignment. Desktop imports reuse the exact `AppState` runtime;
> standalone and cloned headless workers lazily converge on one shared runtime and fail closed if a
> caller supplies a different database identity. The pipeline no longer directly calls the three
> migrated database writers. Store regressions prove all-or-nothing publication and rollback plus
> stale alignment preservation, and a cloned-worker regression proves runtime sharing and mismatch
> refusal. `93ec2db` expands that boundary to source transcripts/provenance, persisted recording
> identity, LOOP-0 evidence, auxiliary/champion hypotheses and targeted rediarization speaker updates.
> Champion transcription now reads through a bounded restore-gated connection and commits through
> the shared serialized runtime; verified human truth and existing votes remain protected. Four store
> regressions cover the complete boundary, and architecture policies ban all ten migrated raw pipeline
> writers. `56e98d0` moves champion target selection into `SegmentQueryStore`, closing the pipeline's
> last raw read-connection escape. Exact alignment resolution, no-row behavior, multi-segment path
> ambiguity and missing-schema propagation are regression-covered under a bounded restore-gated
> snapshot. Other compatibility reads remain. `00f49f8` closes the new-import
> placeholder-before-champion crash window: every champion and enabled-refiner result is completed in
> memory before canonical publication, and one serialized savepoint publishes the file's finalized
> segments, sole champion hypotheses and recording identity together. The boundary rejects blank or
> placeholder transcripts, noncanonical deployment digests, mixed-source batches and any model that is
> no longer the exact registry champion. An injected failure on the second hypothesis proves zero
> segments and zero audio identity survive; infrastructure failure and worker panic leave the entire
> file unpublished rather than depending on compensating deletion. The exact post-format tree passed
> the complete **121-script** Python policy sweep, strict all-target/all-feature Clippy, rustfmt and the
> all-target/all-feature Rust command with **1,608/0/8** library results plus every integration, soak,
> binary and benchmark target green. Legacy interrupted-placeholder cleanup remains for old databases;
> catastrophic cleanup failure in that legacy path and the separation between import-journal progress
> and file publication are not claimed closed. `9a2a0c5` removes the restore-only raw writer escapes
> from `AppState` and `DatabaseRuntime`: both restore commands now publish through one runtime-owned
> boundary that requires the exact active reservation, pre-opens the replacement connection before
> the live commit point, and replaces the sole writer before admission can reopen. A deterministic
> generation test dirties connection-local SQLite state, restores a different database, and proves
> the reopened writer resets its pragmas while both writer and bounded reader see only the restored
> generation. Restore-focused Rust tests passed **54/54**; the exact post-format all-target/all-feature
> command passed **1,609/0/8**, every other target exited zero, strict Clippy and rustfmt passed, and
> all **121** policy scripts passed sequentially. Restore orchestration and validation still reside in
> `commands.rs`; process-kill restore drills are not closed. `8d18260` then extracts process-wide
> restore admission, fail-closed durable marker load/write/complete/clear, atomic restore-state I/O and
> bare-restore pilot refusal into a 254-line Tauri-free `recovery` module. The command layer now only
> resolves its data directory and supplies a writer-activity callback; the recovery module publishes
> or reclaims admission before invoking that fence. Two module regressions prove reservation-before-
> fence ordering with release on refusal and exact one-target/one-completion durable marker behavior.
> The source policy forbids a Tauri, AppState or commands dependency and prevents marker authority
> from returning to `commands.rs`. Exact proof is **1,611/0/8** library tests plus every target,
> restore selection **54/54**, recovery module **2/2**, all **121** policies sequentially, strict
> Clippy and rustfmt. Semantic database validation, snapshot-plan inspection, settings publication and
> startup/named restore orchestration remain in `commands.rs` and are not claimed extracted.
> `c5e3748` moves typed snapshot plans, atomic cross-file routing-state installation, controlled-pilot
> policy publication and strict settings restoration into the same Tauri-free `recovery` module.
> Dataset-coupled settings are restored while current cloud consent, champion/ASR routing and GPU/runtime
> controls are preserved and durably saved before runtime publication or barrier completion. The command
> layer retains AppState publication but no longer owns state-file writes. Three recovery regressions now
> include a cloud-enabled historical snapshot restored under revoked live consent, and source policies
> follow the extracted helpers without becoming vacuous. Exact proof is **1,612/0/8** library tests plus
> every all-target integration, soak, binary and benchmark target, recovery module **3/3**, all **121**
> policies sequentially, strict all-target/all-feature Clippy and rustfmt. Semantic snapshot/pilot
> inspection, durable database validation and startup/named/bare orchestration remain in `commands.rs`;
> process-kill restore drills and the 50,000-segment hammer remain open.
> `ad3085d` moves policy/absence artifact parsing, manifest-bound pilot-to-database inspection and the
> composite optional-state restore-plan builder into `recovery`. Policy-bearing snapshots still require
> a verified manifest, exact controlled focus, schema support for durable hidden keys and a baseline no
> later than their own review-event maximum; legacy manifestless snapshots preserve rather than relax
> live policy. The source policy prevents all three inspectors from returning to commands. The exact
> tree passed **1,612/0/8** library tests plus every target, all **121** policies sequentially, strict
> all-target/all-feature Clippy and rustfmt. Durable review-history/pilot semantic validation,
> authoritative-floor comparison and top-level startup/named/bare restore transactions remain in
> `commands.rs`; process-kill restore drills and the 50,000-segment hammer remain open.
> `7f173fa` moves named-restore artifact staging and final manifest rehash, the mandatory pre-restore
> safety snapshot, exact safety-pin selector/reuse and the durable transaction-begin boundary into
> `recovery`. A new transaction still refuses selector drift or a completed generation, verifies an
> interrupted restore's original pin before reuse, and arms fail-closed restore parking immediately
> before writing the durable pending marker. The source policy requires all four boundaries in
> `recovery`, absent from production commands, and pins arm-before-marker ordering. The exact tree
> passed **1,612/0/8** library tests plus every all-target/all-feature integration, soak, binary and
> benchmark target, all **121** policies sequentially, strict all-target/all-feature Clippy and
> rustfmt. Durable review-history/pilot semantic validation, authoritative-floor comparison,
> database target semantics, the full named transaction and startup/named/bare restore orchestration
> remain in `commands.rs`; process-kill restore drills and the 50,000-segment hammer remain open.
> Import process-kill/resume and performance proof, Couch decomposition,
> export-kill/disk-full campaigns and the 50,000-segment hammer remain open.
> This entry is
> deliberately **not a green
> claim**: timeout calibration, three full fault campaigns, backend/frontend decomposition,
> certification-grade soak/field sessions, Windows signing/VM/a11y/usability/pilot evidence and the
> separate model attestation remain pending. The requested `PAY_POLICY_REQUIRED` external-pool shutdown conflicts
> with active owner-FINAL schema-65 pool canon and is not implemented without literal
> `change canon: <item>` authorization; no pay was minted, backfilled or altered.

## 1. Overall 10/10 Gate Status

* **Stop Condition (`verify-10` checker)**: **GREEN — narrow M0/M1 gate only** (`make verify-10` exits 0: manifest sync, asset presence, ledger schema, license-compatibility). This is **NOT** the full-charter 10/10. **Honest grade as of 2026-07-09: ≈7/10** (36-agent adversarially-verified audit, [docs/TRUE_RATING_2026-07-09.md](docs/TRUE_RATING_2026-07-09.md)); lineage 6.5 (07-02) → 6.5 (07-06 deep-check) → ~7.0 (07-09). The scorecard table below is the ORIGINAL Wave-0 blueprint scorecard, retained for history; the current per-dimension grades live in the 07-09 rating doc. The remaining gap to a declared 10/10 is owner-gated measurement (P2.2 benchmark → marathon → retrain cycle → P7 re-audit).
* **Scorecard Progress**:

| Dimension | Initial Score | Current Score | Exit Criteria Met? |
|---|---:|:---:|---|
| **Proven accuracy** | 2 | 2 | No |
| **Language breadth** | 1 | 1 | Partial |
| **Real-time/latency** | 0 | 0 | No |
| **Data-curation refinery** | 10 | 10 | Yes |
| **Engineering rigor** | 8 | 8 | No |
| **UX/polish** | 5 | 5 | No |
| **Distribution/adoption** | 0 | 1 | No |
| **Trust/proof** | 2 | 2 | No |
| **Ethics & Data Governance** | 0 | 2 | No |
| **Sustainability & Bus-factor** | 1 | 1 | No |

---

## 2. Milestone Status Table (Wave 0)

| ID | Title | Wave | Status | Depends On Met? | Evidence / Done-When Verification |
|---|---|---|---|---|---|
| **M0** | Ethics & Data Governance Foundation | Wave 0 | **DONE** | Yes | `DATA_GOVERNANCE.md` exists; provenance schema validates. |
| **M1** | Local Repo Init & Manifest Sync | Wave 0 | **DONE** | Yes | Version `2.1.0`, license `Apache-2.0`, LICENSE/NOTICE present. |
| **M2** | Sorani-aware Metrics | Wave 0 | **PARTIAL** | — | NFC+Sorani routing, micro/macro, bootstrap+MAPSSWE all real. BUT `jiwer_fixture` is **self-referential** (its own output values), not an external jiwer/SCTK cross-check (blueprint M1.1). |
| **M2b** | Wire language='ckb' hint | Wave 0 | **PARTIAL** | — | Hint wired (`asr.rs:425-427`), but its **effect on CTC output is unproven** — no A/B spike (≥10 clips, with vs without). `set_option` is a generic passthrough (blueprint M1.2). |
| **M3** | ASR-on-gold runner | Wave 0 | **PARTIAL** | — | Runner + recompute built & unit-tested. BUT **no committed gold set, no published scorecard**; `gold_wer_eval` is `#[ignore]`'d (5×) and **not run by CI**, so accuracy is ungated (blueprint M1.3/M1.6). |
| **M3b** | Inter-annotator agreement | Wave 0 | **TODO** | Human-only blocker remains. |
| **M4a** | Acquire + pin datasets | Wave 0 | **TODO** | No committed external SHA yet. |
| **M4b** | Publish commit-pinned scorecard | Wave 0 | **TODO** | Dep: M0, M3, M3b, M4a. |
| **M5** | Holdout integrity & FLAC | Wave 0 | **DONE** | Yes | FLAC export positive test present; hash-based holdout exclusion exists in DPO/LM/HF export paths. |

---

## 3. Current Focus

* **Active Milestone**: blueprint M1.1–M1.3 — real jiwer/SCTK cross-check, prove the `ckb` hint (A/B), then a frozen **CC0 Common Voice ckb** gold set + the first published reproducible CER/WER scorecard. (M4b cannot be "active" until M2b/M3 are genuinely closed.)
* **Branch**: `m02-sorani-metrics`
* **Next Done-When Bullet**: Replace the self-referential `jiwer_fixture` with an external `scripts/crossval_jiwer.py` asserting Rust↔jiwer within 1e-6 on a fixed sample (blueprint M1.1).

---

## 4. Measured Numbers Table

| Date | Metric | Value | Model/Dataset SHA | Command / Source of Truth |
|---|---|---|---|---|
| 2026-06-23 | jiwer fixture (self-referential, not external jiwer) | PASS | workspace HEAD | `cargo test --lib jiwer_fixture_matches_reference_values` |
| 2026-06-23 | FTS determinism | PASS | workspace HEAD | `cargo test --lib search_segments_tie_order_is_deterministic_by_id` |
| 2026-06-23 | eval runtime round-trip | PASS | workspace HEAD | `cargo test --lib run_gold_eval` |
| **2026-06-24** | **ckb micro CER (primary)** | **29.40% (95% CI 26.3–32.5)** | OmniASR-CTC-300M stock / "Comprehensive Central Kurdish Sound Dataset", N=400 (seed=42) | `cargo test --test real_audio ckb_scorecard_on_gold` → `python scripts/scorecard_stats.py results.tsv 3000` |
| **2026-06-24** | **ckb micro WER (secondary)** | **67.62%** | same (N=400) | see `cortex-speech-app/docs/EVAL.md` |
| **2026-06-24** | **fairness: gender CER disparity** | **4.29 pts** (M 29.66% / F 25.37%) | same, N=375/25 | first per-gender Sorani CER slice; `docs/EVAL.md` |
| **2026-06-24** | **real-exe pipeline e2e (import→VAD→OmniASR)** | **PASS** — non-blank `"بروايت مفهومي حديث"` (ref `بە ڕیوایەتی مەفهوومی حەدیس`) | release build 2.1.0 / real ckb clip | `node e2e_pipeline_ipc.cjs` against `target/release/cortex-speech-app.exe` (no-fabrication guard) |
| **2026-06-24** | **Windows installer bundle** | **PASS** — MSI 310 MB + NSIS 291 MB produced | release 2.1.0 (models bundled) | `npm run tauri build` → `target/release/bundle/{msi,nsis}/` |
| **2026-06-24** | **Rust gates after reliability/CSP fixes** | **GREEN** | branch `m02-sorani-metrics` | `cargo clippy -D warnings` + `cargo test` + `npm run test:python-policies` |

> **2026-06-24 hardening pass** (branch `m02-sorani-metrics`): drove the real `.exe` end-to-end and
> fixed real defects found — import-worker panic-guard, unknown-frame audio recovery, CSP block of
> the Windows event IPC (`http://ipc.localhost`), an un-broke of the eval row-propagation policy gate,
> `ship-check` made a superset of CI, honest model-download docs + root README, and a real-app e2e
> harness (`test:e2e:real` + `e2e_pipeline_ipc.cjs`). **Open finding:** the full UI click-through
> e2e does not yet pass because the post-import jury adjudication holds the global DB lock across
> heavy ASR (commands.rs:560-564), starving the UI's `get_segments` so the segment list does not
> render promptly during import — flagged for a focused fix (segment is created and transcribes
> fine; this is a UI-responsiveness defect, not an ASR defect).

---

## 5. Blockers Awaiting Human

> **⭐ OWNER'S INTENDED USE = PERSONAL / SELF-USE (confirmed 2026-06-25), NOT public distribution.**
> The owner runs Cortex on their own machine to do **professional-quality** Central Kurdish (Sorani)
> transcription that no other tool offers. So "highest grade" here means **reliability + accuracy + a
> clean experience for daily use** — NOT a code-signed public release or an academic-publishable
> scorecard. This **reclassifies the items below**: #3 (code-signing cert) and #4 (CC0 public-test
> fixture) are **NOT REQUIRED** for the goal (signing only removes a one-time Windows "unknown
> publisher" prompt that matters solely if the app is ever shared publicly). The only blocker that
> still genuinely raises *daily-use* quality is **#1 (GPU fine-tune → lower CER)** — and the
> fine-tuned MMS-CTC engine is already embedded (~19–21% CER, ≈half of stock), so the app is
> **already professional-grade today**. Reliability is covered: 15 defects fixed across three
> hardening hunts + an enforced real-audio CER gate, all behind a green `make ship-check`.

* ~~External gold-runner execution for the real scorecard table.~~ **DONE** — first real ckb CER/WER measured (34.5% / 79.4%, N=40); see `docs/EVAL.md`. Remaining for a *publishable* scorecard: scale N to ≥900, compute IAA ceiling, fix the Latin-romanization/language-locking, add a real baseline (SeamlessM4T-v2).

* **The four items between the in-sandbox state and a full-charter 10/10 are now EXTERNAL (turnkey for the human):**
  1. **GPU fine-tune (the only real accuracy cure, ~29% → ~8% CER).** Constrained decode (now shipped, opt-in) guarantees Kurdish *script*; only fine-tuning fixes Kurdish *recognition*. Needs a GPU + the dataset. Hand the resulting model back and it gets wired + re-measured through `ckb_scorecard_on_gold`.
  2. **Fresh-clone model fetch (blocker #1).** A decision: **Git LFS** (`git lfs track` the ~300 MB models — consumes repo LFS quota) **vs.** a `scripts/fetch-models` downloader with pinned SHA-256 (needs a one-time ~235 MB OmniASR-archive download to compute the archive hash — `OMNIASR_CTC_300M_ARCHIVE_SHA256` is intentionally empty until then). Pick one and it gets implemented + gated.
  3. **Code-signing certificate** (Authenticode EV/OV) — **NOT NEEDED for personal/self-use** (only matters if the app is ever distributed publicly). The CI signing pipeline is *already fully wired* in `release.yml` (gated on `WINDOWS_CERT_BASE64`/`WINDOWS_CERT_PASSWORD` secrets + a `v*` tag), so it's turnkey IF public distribution is ever wanted — but for the owner's own machine it's a no-op (a one-time "More info → Run anyway").
  4. **CC0 committable audio fixture** — **NOT NEEDED**: a committed **CC-BY** FLEURS `ckb` fixture already powers the in-repo default real-ASR gate (`omniasr_on_committed_fleurs_ckb_fixture`, now enforcing CER < 0.40). A CC0 source would only matter for a *publicly redistributed* default-test, which the personal-use scope doesn't require.

---

## 6. Backlog

* macOS notarization (stretch target in Wave 1/M7).

---

## 7. Decision Log

* **2026-06-23**: Closed M3 and M5 on verified code+test evidence. Shifted active focus to M4b (publish-pinned scorecard) as the next highest-impact 10/0 blocker.
* **2026-06-23 (correction)**: A **root-scoped re-audit** found the prior M2/M2b/M3 "DONE" marks overstated — M2's jiwer check is self-referential, M2b's hint effect is unproven (no A/B spike), and M3's `gold_wer_eval` gate is `#[ignore]`'d (5×) and unenforced in CI with no committed gold set/scorecard. Reverted those three to **PARTIAL**. M5 (FLAC + hash-holdout) and M0/M1 (governance + manifests, verified at the git root) remain genuinely DONE. Confirmed `make verify-10` green is the **narrow** M0/M1 gate, not full-charter 10/10. Full evidence-grounded plan + corrected scorecard: [docs/BLUEPRINT_9_5.md](docs/BLUEPRINT_9_5.md).
* **2026-06-23 (Wave 0 truth-fixes landed)**: Implemented blueprint W0.1–W0.5 — corrected the accuracy baseline (Whisper→SeamlessM4T-v2) in AGENT_CHARTER; fixed the stale OMNIASR_MIGRATION `language='ckb'` status (it IS wired, `asr.rs:425-427`; effect still unverified); corrected AsoSoft (→`LicenseRef-AsoSoft-NonCommercial`, no-redist) and CORDI (→`CC-BY-SA-4.0`) ledger license labels; removed the phantom `kmr` UI option; removed the dead duplicate `cortex-speech-app/.github`. Gates: verify-10 GREEN, typecheck 381/0, lint clean, python policies pass. **Next: blueprint M1.1** — replace the self-referential jiwer fixture with a real external `scripts/crossval_jiwer.py` (Rust↔jiwer ≤1e-6 after identical Sorani normalization) + NIST SCTK MAPSSWE cross-check.
* **2026-06-23 (Wave 1+ milestones landed)**: **M1.1** — WER/CER cross-validated against jiwer 4.0.0 on identical Rust-normalized input (12/12 match; empty-ref convention divergence noted), self-referential fixture replaced. **M2.3** — runtime ONNX SHA-256 verification wired into the ASR load path (tampered model rejected; real 300M model passes its pin). **M3.3** — `RefineryPanel` surfaces eval-runs + escalation trend (DOM-tested). **M3.4/M3.5** — ReviewInbox real audio playback + persisted autonomy dial. **M3.6** — axe-core WCAG 2.2 AA gate built, marked `fixme` (surfaced real a11y debt: aria-required-children, color-contrast ×3-5, scrollable-region-focusable, select-name). Gates: cargo test full ✓, clippy ✓, fmt ✓, verify-10 GREEN, typecheck 382/0, vitest 105, python policies ✓, lint ✓. Follow-on: M1.1 SCTK/KLPT/stats-reference; M1.2/M1.3 (ckb A/B + CC0 gold scorecard) need real-data runs.
* **2026-06-23 (M3.6 COMPLETE)**: Fixed all four WCAG 2.2 AA violation classes (select-name, scrollable-region-focusable, aria-required-children, color-contrast — footer cortex-600 3.49:1 → cortex-500 5.28:1, computed) and flipped the axe gate from `.fixme` to **ENFORCED**: `npx playwright test e2e/axe.spec.ts` = 3 passed, **0 violations** across App root (en + ckb/RTL) + settings dialog. typecheck 383/0/0, vitest 105, lint clean.
* **2026-06-23 (M3.1 + M2.5 COMPLETE)**: **M3.1** — end-to-end measured raw-vs-jury label-quality lift: `compute_label_quality_lift` (micro CER raw vs post-jury verdict against human-confirmed `annotated_transcript` references, CER lift + seeded paired bootstrap 95% CI), `load_lift_triples` (human_decision'd segments), `get_label_quality_lift` IPC command, and the RefineryPanel lift card wired (3 Rust tests + DOM test). **M2.5** — root workflow actions already 100% SHA-pinned (0 tag refs); added `.github/dependabot.yml` (actions+cargo+npm). All gates green (cargo test, clippy, fmt, verify-10, typecheck 383/0/0, vitest 105, lint, python policies).
* **2026-06-23 — IN-SANDBOX CEILING REACHED**: Completed every milestone achievable+verifiable in this environment (Wave 0, M1.1, M2.3, M2.5, M3.1, M3.3, M3.4, M3.5, M3.6). Remaining milestones are externally blocked: **M1.2** ckb A/B (run ASR on ≥10 ckb clips), **M1.3/M1.4** published scorecard + IAA (human Sorani annotators + CC0 dataset), **M1.5** SeamlessM4T baseline (download+run), **M2.1/M2.2** network-sandbox + runtime egress (Linux/CI), **M2.4** fuzz CI (nightly Linux), **M2.6** cargo-mutants (long run), **M4.1/M4.2** RTF + bench gate (named reference machine + audio set), **M5.x** signing/updater/SLSA/winget (Azure cert + live CI run-URLs). Honest full-charter grade: **~6/10** (up from ~4.7), capped on the unmeasured headline scorecard + undistributed signed release.
* **2026-06-23 (M1.2 A/B — measured finding)**: Ran the ckb language-hint A/B on a real 10s Kurdish clip with the live OmniASR-CTC 300M model. **Finding: the `language="ckb"` hint is a confirmed NO-OP** — identical output (`"jóg dú mali"`) with vs without it — and the generalist model emitted **Latin-script, non-Kurdish** text. Implication: the base model needs effective language conditioning or fine-tuning (charter M13) before a real ckb scorecard is meaningful; a from-scratch OmniASR-CTC scorecard would currently measure non-Kurdish output. Harness committed (`ckb_language_hint_ab`, env-gated); audio not committed (unknown provenance).
* **2026-06-24 (M1.3 — FIRST REAL ckb SCORECARD)**: User provided a 1.74M-clip Central Kurdish corpus (audio + verified Sorani transcripts + gender/age). Built reproducible gold-set extraction + `ckb_scorecard_on_gold` harness + `scorecard_stats.py`. **Measured: micro CER 29.40% (95% CI [26.3, 32.5], N=400, seed=42), micro WER 67.62%**, stock OmniASR-CTC-300M. First per-gender/age Sorani fairness slice (gender disparity ~4–10pt, noisy on small female N). **PIVOTAL FINDING (script split, N=200): when the model emits Kurdish script (78% of clips) CER is 19.71%; the ~30% aggregate is dominated by the 21% it romanizes to Latin (94.5% CER vs Arabic refs, despite usually-correct content).** → The headline gap is mostly **script-locking, not recognition**: forcing Arabic-script output (or transliterating the romanized minority) should pull CER ~30%→~20% with no re-train. Roadmap: **script-locking first, fine-tune second**, then scale to ≥900 + IAA + SeamlessM4T baseline. Full scorecard: `cortex-speech-app/docs/EVAL.md`. Eval-only corpus; no audio/refs committed.
* **2026-06-24 (hardening pass — 16 commits, in-sandbox ceiling)**: Drove the real `.exe` end-to-end and hardened it. **Fixed real defects:** import-worker panic-guard; unknown-frame audio no longer false-rejected; **CSP missing `http://ipc.localhost`** (Windows event IPC); **post-import jury adjudication held the global DB lock across ASR → starved `get_segments`** (now runs on its own WAL connection + off the import thread — verified: the CDP reload that timed out now succeeds). **Shipped the script-locking lever (the #1 finding above):** constrained Kurdish-token CTC decode ported to Rust (`constrained_decode.rs`, 6 tests + real-model parity → `"رفايت مفهومي حدي"`, all Kurdish-script), wired as opt-in `transcribe_segment_constrained` IPC command (verified live) + UI "Kurdish-only" button + cached `ort` session. **Gate honesty:** un-broke a red eval policy gate; `make ship-check` made a CI superset; CI mock-e2e honestly labeled + nightly skip made a visible `::warning::`. **Proof:** real-exe pipeline e2e (`e2e_pipeline_ipc.cjs` → non-blank Kurdish), Windows installers (MSI+NSIS) built. All gates green (clippy `-D warnings`, cargo test 0-fail, vitest 105/105, python-policies). **Remaining to full-charter 10/10 is now wholly external** (see §5): GPU fine-tune (the accuracy cure), model-fetch decision (LFS vs script), code-signing cert, CC0 fixture. Honest grade ~6.5/10; capped on the unadapted model + undistributed signed release — NOT on any remaining safe in-sandbox work.
* **2026-06-25 (model-fetch + FINE-TUNED MODEL MEASURED — the accuracy cure)**: (1) **Blocker #1 closed** — `scripts/fetch_models.py` (user chose a fetch script over Git LFS) downloads + SHA-256-verifies the gitignored models into `src-tauri/models/` so a fresh clone can `tauri build`; pins match the in-repo extracted-file pins; `npm run fetch-models`/`verify-models`; wired into `release.yml` before the bundle step. `--check` verified all 5 files green (download path un-run in-sandbox; SHAs authoritative). (2) **The user provisioned a fine-tuned model (MMS-CTC-1B, Wav2Vec2ForCTC, base `facebook/mms-1b-all`) and I MEASURED it** head-to-head on an identical seed-fixed gold sample (N=50, seed=42, same normalization, CPU): **stock OmniASR-CTC-300M 42.06% CER → fine-tuned MMS-CTC-1B 19.77% CER (~53% relative reduction), and all output is Kurdish script (language-lock fixed).** The fine-tune is the real accuracy cure, now measured (not estimated). Caveats: N=50 preliminary (publishable needs ≥900 + CI); measured via a CPU `transformers` harness, **NOT yet wired into the Tauri app** (the HF model needs an ONNX export for sherpa-onnx/`ort`, or a Python inference bridge). Full numbers + caveats: `cortex-speech-app/docs/EVAL.md`. **Remaining to ship the fine-tuned model in-app:** ONNX-export the Wav2Vec2ForCTC (or add a Python bridge) + wire it as the ASR engine; scale the scorecard to ≥900 + CI; then code-signing + a CC0 fixture. Honest grade lifts toward ~7.5/10 on the measured accuracy cure (capped on in-app integration + publishable-N + signed release).
* **2026-06-25 (FINE-TUNED MODEL EMBEDDED + WORKING IN THE APP)**: Integrated the user's fine-tuned MMS-CTC-1B into Cortex end-to-end. **ONNX-exported** the `Wav2Vec2ForCTC` (`scripts/export_finetuned_onnx.py`) + **int8-quantized** to 925 MB; export fidelity verified (fp32 18.57% / int8 **19.29%** CER on the N=50 gold sample, matching transformers 19.77%). Built a Rust **`wav2vec2_asr.rs`** engine (feature norm → `ort` inference → CTC decode vs `vocab.json["ckb"]`, cached session, 4 unit tests + parity test), an opt-in **`transcribe_segment_finetuned`** IPC command (resolves env → active → bundled models dir), a **"Fine-tuned" UI button**, and **embedded** `finetuned-mms-ckb/{model.onnx,vocab.json}` in both bundle configs. **Verified END-TO-END on the real exe with NO env override** (resolves the EMBEDDED model): `transcribe_segment_finetuned` → `"بەڕیڤایەتی مەفومی حەدی"` (accurate Kurdish, ref `بە ڕیوایەتی مەفهوومی حەدیس`; stock OmniASR gave `بروايت مفهومي حديث`). Gates: clippy -D warnings, cargo test 0-fail, vitest 105/105, typecheck/lint, python policies — all green. **The app now ships an opt-in engine that roughly halves Sorani CER.** Not wired: OmniASR_7B (user removed its 31.2 GB base weights). README documents placing the model in `src-tauri/models/finetuned-mms-ckb/`. Honest grade lifts to ~8/10 — proven accuracy lever is now IN the app; remaining: publishable N≥900 + CI, code-signing, a CC0 default-test fixture.
* **2026-06-25 (PUBLISHABLE N=900 FINE-TUNED SCORECARD)**: Ran the embedded int8 fine-tuned engine on a seed-fixed **N=900** gold sample via onnxruntime (`scripts/scorecard_finetuned.py`): **micro CER 21.00%, 95% CI [19.93%, 22.04%]** (3000-sample utterance bootstrap, seed=42), same normalization as the stock baseline. vs stock OmniASR-CTC-300M 29.40% (N=400) — ~8.4 pts / ~29% relative lower, all Kurdish script. This is the first publishable-tier (≥900 + CI) accuracy number for the SHIPPED engine. Full scorecard: `cortex-speech-app/docs/EVAL.md`. Eval-only corpus; no audio/refs committed.
* **2026-06-25 (gate-coverage hardening — 6 commits, all gates green at branch tip)**: A full `make ship-check` run (not just the narrow `verify-10`) surfaced **four real defects the narrow gate silently passed**, each fixed + verified on the real Windows toolchain: (1) **`cargo fmt --check` was red** — five modules added earlier this session (`commands.rs`, `constrained_decode.rs`, `lib.rs`, `wav2vec2_asr.rs`, `real_audio.rs`) were never rustfmt'd; reformatted (pure reflow, `git diff -w` confirmed semantic-neutral). (2) **`is_transient_decode_error` retried *every* I/O error** including permanent ones (NotFound/PermissionDenied) — now gates on `ErrorKind` (Interrupted/WouldBlock/TimedOut only) + unit test. (3) **Flaky test** `search_segments_tie_order_is_deterministic_by_id` — asserted a pure-id order but `created_at` (1s-resolution column default) decides ties first when inserts straddle a second boundary; pinned `created_at` so the `id` tiebreaker is what's tested (production ordering unchanged + correct). (4) **Broken e2e test** `keyboard shortcuts modal opens with ?` — `data-testid="shortcuts-modal"` sat on the inner content div while `Modal` renders the title `<h2>` + Close button in its header (outside that subtree); added a reusable `testid` prop to `Modal` anchored on the dialog root. **Bonus a11y-gate integrity finding:** the e2e Tauri mock returned `null` for `get_dataset_certificate` (real contract: `Result<ConformalCertificate, String>`, always Ok), which (a) logged a misleading `Failed to load conformal certificate` console.error every run and (b) **kept the StatsDashboard conformal panel un-rendered, so the ENFORCED M3.6 axe WCAG gate never actually analyzed it.** Added a faithful heuristic cert to the mock + a `conformal-cert` testid + settle-waits in both a11y tests → the gate now covers the panel (verified **0 violations** when settled, 3/3). Branch tip green: fmt-check, clippy `-D warnings`, `cargo test` (615 lib + integration), typecheck, lint, vitest 105/105, python-policies, `verify-10`, `npm audit` 0, `cargo deny` ok, e2e **39/39** (CI's single-worker+retry config). No production accuracy claim changes; this is reliability/gate-fidelity hardening.
* **2026-06-25 (soundness + offline-install hardening — 2 more commits)**: (1) **UB guard on the ASR transmute** — `asr.rs::transcribe_chunk` transmutes `&OfflineStream` to a `#[repr(transparent)]` pointer mirror to reach sherpa-onnx's `pub(crate)` C ptr (needed for the result JSON's per-token confidences, which the safe `OfflineStream::get_result()` parses away — confirmed by reading sherpa-onnx 1.13.2 source: `OfflineRecognizerResult` keeps only text/tokens/timestamps/durations). Added a `const` size assertion so any future sherpa-onnx layout drift (an added field) becomes a **COMPILE error, not silent UB**, and documented why the safe API is insufficient. (2) **Offline-first WebView2 install** — `bundle.windows` set no `webviewInstallMode`, so the build defaulted to `downloadBootstrapper` (fetches the runtime online at install time) — contradicting the offline-first premise. Set `offlineInstaller` (embeds the runtime; installer +~127MB, consistent with the ~1GB bundled models); verified `webviewInstallMode`/`offlineInstaller` are valid against `@tauri-apps/cli/config.schema.json`. Both verified on Windows: clippy `-D warnings`, fmt-check, `cargo test` 615/615, config-security test 4/4.
* **2026-06-25 (FIRST REAL RTF MEASUREMENT — charter M4.1, partially in-sandbox)**: Built an opt-in (`#[ignore]`) RTF harness (`omniasr_rtf_on_committed_fleurs_ckb_fixture` in `tests/real_audio.rs`) that loads the real OmniASR-CTC-300M int8 model, warms up (excludes model-load), and times 5 transcriptions of the committed CC-BY FLEURS fixture. **Measured on this dev Windows machine: 8.22 s audio, 785.8 ms/inference, RTF = 0.0956 (~10× faster than real-time, CPU int8).** Honest first data point — NOT a named-reference-machine benchmark (M4.1's published bar wants a pinned rig + audio set), so the test asserts only finite-positive RTF and PRINTS the number (no machine-dependent CI threshold). Recorded in `docs/EVAL.md` with the caveat. clippy/fmt/policies green. This converts a previously "fully external" deep gate into a real, reproducible measurement + the harness the user runs on their reference machine for the published number.
* **2026-06-25 (PROPERTY TEST FOUND A REAL BUG — search NUL crash)**: Added `search_segments_never_errors_on_arbitrary_input` (proptest, `db.rs`) asserting the search box returns Ok (results or empty) for ANY input — the in-sandbox slice of the charter's "fuzz" gate. It immediately surfaced a real user-facing defect: an interior NUL (`"\0"`) survived `split_whitespace`, got wrapped into the FTS5 `MATCH` string, and made SQLite raise a hard error — so a user pasting text containing a NUL/control char got an error toast instead of results (same class as the earlier metacharacter regression, control chars missed). **Fixed:** `to_fts5_match` now maps control chars (C0/C1) to separators before tokenizing; pinned the NUL/ESC/DEL cases in the deterministic example test too. Verified: proptest green 3× (~768 fresh cases), `cargo test` 616/616, clippy `-D warnings` + fmt clean. Demonstrates the value of the property-testing approach — a real bug, found and fixed in-sandbox.
* **2026-06-25 (SECURITY: path-traversal in dataset export — CWE-22, fixed)**: Following the search-NUL find into a broader untrusted-input sweep, found a real path-traversal vuln: `export_huggingface_dataset` (`export.rs`) built each clip filename as `format!("{clean_stem}_{seg.id}.wav")` then `dest_dir.join(filename)` — `clean_stem` was sanitized but **`seg.id` was not**, and `validate_segment` only checks id non-empty while `import_gold_segments`/`merge_dataset_json`/`create_gold_from_file` accept user-supplied ids. A crafted id (`"../../x"`) would write the clip OUTSIDE the export target. **Fixed:** extracted a `sanitized_clip_filename` helper (reduces both stem and id to `[A-Za-z0-9_-]`, matching what `export_bundle` already did) so the filename is always a single join-safe component; regression test covers separators/`..`/NUL/mixed-slashes. **Swept all production filesystem write-sinks** for the same class: `export_bundle.rs` (sanitizes both components — safe), `agentic.rs` source-transcript write (`sanitize_filename` on stem+model, stem is a `file_stem()` — safe), the rest are tests or constant paths. One real gap, found + fixed; rest confirmed safe. cargo test 618/618, clippy `-D warnings` + fmt clean.
* **2026-06-25 (PRIVACY: consent-gate audit of ALL cloud egress — 2 bypasses fixed)**: Audited every outbound cloud path against the charter's hardest guardrail (voice = biometric, GDPR Art. 9 — never send audio/transcript to a provider without explicit opt-in). **Two real bypasses found + fixed:** (1) `transcribe_audio_with_scribe` and `add_scribe_votes` uploaded **raw audio** to ElevenLabs after only an API-key-exists check — no `cloud_stt_opt_in`; (2) `run_dpo_update` POSTed **private transcript-derived preference pairs** to a cloud endpoint with only an allow-list (a security control, not consent) — no `cloud_llm_opt_in`. The pipeline STT/LLM paths enforced consent, but these direct IPC entry points bypassed it. Added shared `require_cloud_stt_consent` / `require_cloud_llm_consent` gates (refuse before any key load, data build, or network call) + 2 regression tests. **Verified the rest already gated:** T2 Gemini audio (`listen_and_judge` — `jury_cloud_opt_in`, cmd 3477/3367), agentic whole-file Gemini (`generate_whole_file_reference_transcript` — `jury_cloud_opt_in`, pipeline 517), LLM refine (`effective_llm_mode`). Every cloud egress is now consent-gated. cargo test 620/620, clippy `-D warnings` + fmt clean.
* **2026-06-25 (CONCURRENCY: jury commands held the global DB lock across T2 cloud calls — 3 sites fixed)**: Audited every `lock_db()` site for the lock-across-blocking-work class that caused the original import-jury UI freeze. Found **three foreground paths** still holding the shared `lock_db()` guard across blocking T2 Gemini calls (`n_samples` retries, multiple seconds), starving the UI's `get_segments` for the whole run: (1) `run_t2_for_segment` — held the lock from the segment read through `listen_and_judge` and the verdict write → now drops the lock before the network call, re-acquires only to persist the verdict; (2) `run_jury_pipeline` (the manual "Run Jury") and (3) the post-batch-transcribe worker — ran the entire batch (N cloud calls) under the global lock → now run on a separate WAL connection via a shared `open_jury_db_connection` helper (mirrors the import-jury fix). Verified the `pipeline.rs` callers already use the pipeline's own connection (`self.open_db()`) and the import path was fixed earlier — so all jury cloud paths are now off the shared lock. cargo test 620/620, clippy `-D warnings` + fmt clean.
* **2026-06-25 (IPC-SURFACE AUDIT — 1 vestigial command removed, 17 classified intentional)**: Resolved the long-standing "uncalled IPC commands" open item with a precise, evidence-backed disposition instead of a hand-wave. Methodology: diffed the 105 commands registered in `generate_handler!` against every frontend `invoke('…')` call site, the lone dynamic indirection (`trackedInvoke` — found to be **defined-but-never-called**, so zero dynamic invokes), and all Rust/e2e/test drivers. **18 commands are reached by no caller.** **Removed the one genuinely-dead, unsafe one:** `start_operation` (IPC) — a vestigial duplicate of the cancel-token arming that every long op already does for itself (`commands.rs:420`/`:491`), and an *asymmetric footgun*: it armed only the import slot while `cancel_operation` signals both import+batch. Zero references anywhere (frontend/Rust/e2e/tests/scripts, Grep-confirmed); removing it leaves `start_cancel_token` (still used internally) and cancellation behavior unchanged. **Classified the other 17 as intentional, NOT dead — kept** (deleting would destroy deliberately-built backends; per charter, log roadmap decisions, don't scope-creep): (a) **backend-complete, UI-pending (13):** model registry `list_model_versions`/`get_champion_model`/`import_model_checkpoint` (doc: "what a registry panel lists"; the checkpoint-import ties to the shipped fine-tuned MMS-CTC engine), tracing dashboard `get_tracing_stats`/`get_recent_spans`/`clear_tracing_spans`, session `save_session`/`restore_session`, `get_fingerprint_count`, `get_configured_providers`, consensus `add_segment_hypothesis`, Scribe cloud STT `transcribe_audio_with_scribe`/`add_scribe_votes` (consent-gated this session); (b) **research/eval-harness entry points (4, driven by scripts/integration, intentionally not UI):** `run_gold_eval_asr`, `run_gold_eval_local`, `build_scorecard`, `create_gold_from_file`. Recorded here so future audits don't re-flag the 17 as mystery dead code. Verified on Windows: clippy `-D warnings` green (49.6s), `cargo fmt --check` clean, `cargo test --lib`. No behavior change beyond the dead-command removal.
* **2026-06-25 (UI-WIRING of backend-complete commands — user-directed, 2 features shipped + 1 finding)**: After the IPC audit flagged 13 backend-complete-but-UI-pending commands, the user chose to wire them. Shipped two with full positive tests: (1) **Cloud key status in Settings** — the ElevenLabs Scribe STT opt-in promised a key was required in `secrets.env` but gave no feedback; wired `get_configured_providers` (returns provider *names* only, never key values) to show "key detected / not found" under the opt-in (status by text+symbol, not color alone, for a11y). The Gemini/cloud-LLM key already had `llmApiKeyConfigured`, so this fills the one gap. (2) **Read-only model-registry panel** (`ModelRegistry.svelte` on the AI Models tab) — surfaces `list_model_versions` (id/family, champion-vs-candidate badge, license, checkpoint SHA) so model provenance is auditable in-app; write path `import_model_checkpoint` left as a follow-up. **The axe WCAG gate was extended to cover the AI Models tab and immediately caught a real `color-contrast` violation** (`text-subtle` #6d7c8c on the row bg ≈ 3.65:1) — fixed to `text-muted` (#97a4b6 ≈ 6:1). **Finding on session save/restore (the 3rd requested):** the `save_session`/`restore_session` IPC commands are **redundant as-built** — `SessionState::from_db` persists only `segment_count`/`verified_count` (all UI-state fields are hardcoded to defaults), the backend **already** auto-saves on mutations (`commands.rs` 1114/1132/1153/3635) and **already** restores-and-logs on startup (`lib.rs:398`), and those counts are **already shown** in the top bar. A genuinely-useful version (restore search/sort/selection/layout) needs a session-subsystem refactor (reconcile the counts-only auto-save vs a UI-state save, guard stale `selected_segment_id`) + a stateful e2e mock — real scope beyond "wire the command", so it's flagged for an explicit go-ahead rather than shipped as redundant/cosmetic UI. Verified: typecheck 384/0, eslint clean, vitest 105/105, 2 new positive e2e tests, axe 3/3 (now incl. the models tab), full e2e suite.
* **2026-06-25 (WIRE-AND-COMPLETE-ALL — every uncalled IPC command resolved, user-directed)**: Re-audited the surface and the owner directed wiring all remaining uncalled commands. Independent re-investigation (a 13-agent classification workflow whose recommendations I discounted where they circularly cited this ledger, then verified the load-bearing claims by hand) confirmed **none of the remaining commands were dead** — Scribe's real cloud-STT path is already wired via `import_single_file_via_scribe` on import; `run_gold_eval_asr` is the documented "honest-CER entrypoint"; the tracing trio exposes a real instrumented `Tracer`. Final disposition of the original 13 + 5 docs-only: **removed 1** (`start_operation`, vestigial); **wired 14** with positive tests + a11y coverage — `get_configured_providers` (cloud key status), `save_session`/`restore_session` (real search+sort persistence refactor, not the redundant counts version), `get_fingerprint_count` (stats card), `get_tracing_stats`/`get_recent_spans`/`clear_tracing_spans` (new Diagnostics tab), `transcribe_audio_with_scribe`/`add_scribe_votes` (consent-gated per-segment re-transcribe + jury vote), `list_model_versions`/`import_model_checkpoint` (model-registry panel + import form), `run_gold_eval_asr`/`run_gold_eval_local`/`build_scorecard`/`create_gold_from_file` (Refinery eval actions); **2 kept as documented reserved programmatic API** (`get_champion_model` — redundant with the registry's status badge; `add_segment_hypothesis` — the jury produces hypotheses internally), each now carrying an in-code "reserved" doc comment so future audits don't re-flag them. Supporting fixes: an **i18n English fallback** (`dict[key] || en[key] || key`) so new English labels degrade gracefully under the default ckb locale instead of showing raw keys (+test); the axe WCAG gate extended to the Diagnostics + AI-Models tabs (caught + fixed a real contrast violation; aria-labels added to the import form). New en labels are English-only pending native Sorani translation (the fallback covers ckb). Verified each unit (typecheck 386/0, eslint, vitest 105/105, per-feature e2e) and the full gate at the end. Honest note: the *backends* are now user-reachable, but the cloud/eval/import actions were exercised against the e2e mock, not real runs — the real measured numbers still come only from the harness on the user's machine.
* **2026-06-25 (ADVERSARIAL DEFECT-HUNT — 5 confirmed-real defects fixed, full ship-check green)**: Ran a 5-dimension adversarial hardening sweep over the live tree (consent-egress / honesty / correctness / security / robustness); every finding was re-verified against the actual file before any fix (**5 raised, 5 confirmed, 0 false positives**). Fixed all five: (1) **robustness** — `agentic.rs::delete_gemini_file` was the lone remaining bare `ureq::delete`, using ureq's timeout-less global agent, so a stalled Gemini DELETE would block the worker thread forever; routed through the bounded `crate::http::API_AGENT` like every other call. (2) **security (CWE-1236)** — all three CSV writers (`export_csv`, the HuggingFace `metadata.csv`, `export_audio::write_metadata_csv`) wrote untrusted transcript/speaker/verdict text (incl. third-party imported datasets, which `validate_segment` never content-checks) straight into cells, so a field starting with `=/+/-/@` executes as a live formula when the dataset CSV is opened in Excel/LibreOffice/Sheets — exfiltration/RCE on the reviewer's machine; added a shared `csv_safe_cell` (quote-prefixes formula leads on free-text columns only, structural columns untouched) + 2 regression tests. (3) **perf/concurrency** — `get_waveform` held the global pipeline `Mutex` across an up-to-30s decode, starving other pipeline-lock users (same class as the fixed import-jury freeze); clone-before-decode like the import/rediarize/eval siblings. (4) **honesty** — the T2 jury debate-fallback verdict hardcoded `confidence: 0.85`, surfaced downstream as a precise `agent_confidence=0.850` in the escalation queue + DPO learning prompt as if measured; now reports the real model-assigned confidence of the winning Gemini sample (the debate winner IS judge A's first self-consistency sample), with `votes:1`/`self_consistency_agreement:false` already recording it didn't win by vote. (5) **honesty** — `StatsDashboard` rendered the conformal "Expected Error Bound" in success-green even when uncalibrated (`<10` verified segs), where the value is just the requested target echoed back — reading as an achieved statistical bound; now shows amber "n/a (uncalibrated)" until calibrated (the green measured bound is unchanged). Verified end-to-end via full `make ship-check` (CI single-worker): **verify-10 `CORTEX 10/10: ALL GATES GREEN`**, clippy `-D warnings`, `cargo test` 624/624 (incl. 2 new), vitest 108/108, typecheck 386/0, eslint, python-policies, e2e **46/46**, `npm audit` 0, `cargo deny` ok. Pure reliability/security/honesty hardening — **no production accuracy-claim changes**. 6 commits (5 fixes + this entry) on `m02-sorani-metrics`.
* **2026-06-25 (SECOND ADVERSARIAL DEFECT-HUNT — 4 confirmed-real defects fixed, full ship-check green)**: A second sweep on angles the first pass under-covered (frontend/UI-consent, Sorani normalizer/metric integrity, panic-safety, export data-integrity), each finding re-verified against the actual file (**4 raised, 4 confirmed, 0 false positives**; panic-safety came back clean). Fixed all four: (1) **export data-integrity (HIGH)** — `export_audio::export_single_segment` clamped the slice END to the decoded buffer but not the START, so a present-but-out-of-range alignment window fell through to the WHOLE recording, pairing a multi-minute file with one segment's short transcript (silent training-data corruption); now reuses the HF exporter's `export::slice_for_export` guard, which SKIPS (Err) on an out-of-range window. The HF exporter already had this fix; the audio exporter never got it. (2) **export data-integrity (MEDIUM)** — HuggingFace `process_split` wrote each clip to its final name but never cleaned the split dir, so a shrinking re-export left orphaned WAVs absent from the freshly-written `metadata.csv` (and cemented into SHA256SUMS); now prunes any `*.wav` not in the just-written set, after all current clips succeed. (3) **metric integrity (MEDIUM)** — the quality dashboard / export-bundle manifest / validation gate scored `normalized_transcript` (one-way number-verbalized when `verbalize_numbers` is on, e.g. `١٤`→`چواردە`) against the digit-form human `annotated_transcript`, inflating user-visible WER/CER on otherwise-perfect transcripts and able to spuriously trip a quality gate (the published gold scorecard, scored separately, is unaffected); now scores the RAW transcript so `wer::normalize_for_metrics` canonicalizes both sides symmetrically (+regression test). (4) **UI consent affordance (LOW)** — the Review Inbox "Run Jury" button (T2 can reach Gemini) showed no `jury_cloud_opt_in` cue unlike the gated Scribe buttons; backend already hard-refuses T2 egress when off (no leak), so added a "🔒 Local only" note when cloud T2 is off. Verified end-to-end via full `make ship-check` (CI single-worker): **verify-10 `CORTEX 10/10: ALL GATES GREEN`**, clippy `-D warnings`, `cargo test` 625/625 (incl. 2 new/extended), vitest 108/108, typecheck 386/0, eslint, python-policies, e2e **46/46**, `npm audit` 0, `cargo deny` ok. Reliability/integrity hardening — **no production accuracy-claim changes**; the WER/CER fix corrects a *displayed* analysis metric (not a published scorecard number). 4 commits (3 fixes [two export defects bundled] + this entry) on `m02-sorani-metrics`.
* **2026-06-25 (THIRD ADVERSARIAL DEFECT-HUNT — 6 confirmed-real defects fixed, full ship-check green)**: Swept the grade-credibility dimensions not yet covered (gate/test fidelity, secret/biometric-PII leakage, IPC input-validation completeness, supply-chain/config integrity) — **7 raised, 6 confirmed, 1 correctly filtered as false-positive**; the **secret/PII dimension came back CLEAN** (no key/transcript/audio leak into errors/logs/artifacts found — a reassuring result for the GDPR-Art.9 guardrail). Fixed all six: (1) **test fidelity (MEDIUM)** — `test_corrupt_database_detection_corrupted` nested its only assertion inside two `if let Ok` guards that the injected header corruption defeats, so it passed unconditionally asserting nothing; rewrote it to assert the detection contract in BOTH branches (open rejected, OR opens but integrity_check ≠ "ok"). The production recovery path was already covered correctly by `open_with_retry_quarantines_db_when_integrity_check_fails_after_open`. (2-5) **IPC input-validation (1×MEDIUM `write_segment_verdict`, 1×MEDIUM `merge_dataset_json`, 2×LOW `record_human_decision`, `run_t2_for_segment`+`get_few_shot_examples`)** — five commands accepted user/file input without the `validate_identifier` / `validate_text` cap / JSON-size guard their sibling commands consistently apply; added the missing guards (queries are parameterized so this is defense-in-depth/consistency, not a fix for an exploitable hole, on a local single-user IPC surface). (6) **supply-chain (LOW)** — the `tray-icon` tauri feature was enabled but the app builds no tray/menu; dropped it to shrink native-dep surface. Verified end-to-end via full `make ship-check` (CI single-worker): **verify-10 `CORTEX 10/10: ALL GATES GREEN`**, clippy `-D warnings`, `cargo test` 625/625 (corrupt-DB test now actually asserts), vitest 108/108, typecheck 386/0, eslint, python-policies, e2e **46/46**, `npm audit` 0, `cargo deny` ok. Reliability/credibility hardening — **no production accuracy-claim changes**. 4 commits (3 fixes + this entry) on `m02-sorani-metrics`. **Cumulative across three hunts this session: 15 confirmed-real defects fixed, every one verified against the live tree and behind a green gate. Convergence: the surface is thinning (secret/PII + panic-safety dimensions now clean); the terminal "highest grade" remains externally blocked on the Authenticode cert + a CC0 fixture (§5).**
* **2026-06-25 (ENFORCED REAL-AUDIO CER GATE + cert path confirmed turnkey)**: Per the user's choice to advance the grade via the eval/release path, did the highest-value in-sandbox advance: strengthened the in-repo **default** real-ASR gate `omniasr_on_committed_fleurs_ckb_fixture` from a presence/script-only check into a real **CER regression gate**. Measured stock OmniASR-CTC-300M on the committed CC-BY FLEURS `ckb_iq` clip (verified reference `tests/fixtures/fleurs_ckb_sample.txt`) on this dev box: **micro CER 0.244, WER 0.714** (Arabic-script, deterministic CTC greedy decode → reproducible run-to-run). The gate now asserts `CER < 0.40` — a loose single-clip ceiling that catches romanization/word-salad/near-blank regressions but tolerates a legitimate model-pin change; it runs in plain `cargo test` when the model is present and skips cleanly otherwise (so CI-before-`fetch-models` is unaffected; the nightly-real-audio job + a fresh clone after `npm run fetch-models` enforce it). This delivers the charter's **real-audio CI regression gate** from a committed, redistributable fixture — distinct from the eval-only-corpus N=400/N=900 scorecards (which remain the publishable numbers). **Also confirmed (read, not changed) that the Windows code-signing pipeline is ALREADY fully wired** in `release.yml`: signtool sign (`/fd SHA256`) + DigiCert RFC-3161 timestamp + verify, gated on `WINDOWS_CERT_BASE64`/`WINDOWS_CERT_PASSWORD` secrets, emitting a visible `::warning title=Installers UNSIGNED` when absent — so a signed release is **turnkey**: the user adds those two GitHub repo secrets (the .pfx never touches source) and pushes a `v*` tag. Full `make ship-check` green (verify-10 ALL GATES GREEN, clippy -D warnings, cargo test 625/625 + the enforced real-audio CER gate, vitest 108/108, e2e 46/46, audit/deny ok). docs/EVAL.md updated with the committed-fixture gate row. Eval-only corpus unchanged; the gate's fixture is CC-BY and already committed.
* **2026-06-25 (FINE-TUNED ENGINE AS DEFAULT + correction-learning enabled — user-directed, personal/self-use)**: The owner (uses the app himself; see the §5 scope note) asked to make the best model the default and enable correction-learning. (1) **Corrected a model misconception:** there is no usable "OmniASR 7B" (its ~31 GB weights were removed; never wired). The owner's settings had `asr_model_size="WSL7B"` with an empty `external_asr_script_path`, which silently falls back to **stock OmniASR-300M** locally — so they had been getting stock-quality output, not 7B. (2) **New `use_finetuned_asr` setting (default OFF, additive):** routes `pipeline.transcribe` to the embedded fine-tuned MMS-CTC engine (≈half the CER of stock; docs/EVAL.md), overriding `asr_model_size`, at the single chunk-transcription point — with a fall-through to the configured engine on any failure (model absent / inference error / empty) so transcription never breaks. Long chunks are sub-windowed into ~15 s pieces (the short-utterance model duplicates text on long input); confidence is `None` on this path. (3) **Enabled in the owner's runtime `settings.json`:** `use_finetuned_asr=true`, `loop0_firing_enabled=true` (the LOOP-0 "learn my corrections" auto-substitution memory — already wired in `pipeline.fire_loop0_if_enabled`, opt-in), and fixed `WSL7B→CTC300M` (working fallback). Verified: clippy `-D warnings`, full `make ship-check` GREEN (cargo test 625/625, vitest 108/108, e2e 46/46, audit/deny), a new `#[ignore]` pipeline test (`pipeline_routes_to_finetuned_when_enabled`) proving `transcribe()` uses the fine-tuned engine end-to-end (Kurdish out, `conf=None`) on the committed FLEURS fixture, and a reusable `transcribe_file_with_finetuned` harness (verified on a real 36 s user clip — accurate Sorani). Rebuilt the Windows installer so the owner's GUI app picks up the routing. Honest note: the fine-tuned engine is slower (1B int8 + 970 MB load) and gives no confidence score; it is the right quality/speed trade for daily self-use.
* **2026-06-26 → 07-01 (LEDGER GAP — honestly recorded)**: ~108 commits landed WITHOUT ledger entries (a charter violation itself). Substance from git log: the review-experience line (word-timing persist 00c4883, real CTC forced alignment via the fine-tuned MMS 6c19ead, offline consensus draft 456ca66, re-transcribe + provenance badge + mark-bad 4787d81, non-nagging readiness 673fe0e, gap-aware chunking 75617f5), the WSL-7B champion forced-default + fail-hard rollback (1a9ae00 + test 8763a69), the big origin/main round-25 merge (48bde15), export honesty (human-reject honored in every export + verified count b624339), and the full e2e architecture pack (8b6f297). Per-iteration logging resumes below.
* **2026-07-02 (DEEP AUDIT + F1–F8 IMPLEMENTATION — this session, branch `audit/2026-07-02-deep-audit`)**: Full-tree deep audit (4 subsystem auditors + every gate that runs on this Windows box: typecheck 393/0, vitest 132, clippy -D, cargo test exit 0, verify-10 GREEN). Honest verdict **6.5/10 daily-driver bar**; findings F1–F8 + plan P0–P5 in `cortex-speech-app/docs/DEEP_AUDIT_2026-07-02.md`. Implemented, one verified commit each:
  - **F2 (fix, engine routing)**: WSL7B-with-no-script silently fell through to stock CTC-300M in BOTH import (`build_segments_from_pcm`) and `transcribe()` — the opposite of the documented fail-hard contract. Added `wsl7b_primary_unresolved()` + an actionable error, enforced before any decode work and before the local fallback. ALSO wired the measured-best fine-tuned engine into the IMPORT path (`use_finetuned_asr` was silently ignored on import). 4 unit tests + a real FLEURS import test (coherent Kurdish, no placeholder).
  - **F7 (fix, honesty)**: `accept_refinement()` — every LLM refine site (×4) now rejects an empty/over-edited (CER>0.6 from raw) rewrite so a hallucination can't overwrite good ASR (mirrors T2's `GEMINI_MAX_EDIT_FROM_HYP`). Unit test.
  - **F3 (fix, privacy)**: purged owner PII from the public surface — parameterized the owner-name audio-path LIKE filters in 4 root scripts + build_halwest defaults via env vars; untracked + gitignored the 2 `*_perfect_dataset.json`; removed the hygiene-gate dataset exemption (now ZERO exemptions) + added owner-folder path-fragment detection (name + path separator) that does NOT flag the public `HawzhinBlanca` handle. Hygiene + full python-policies green. (Git history still holds the past strings — a rewrite+force-push is the owner's call.) [This ledger line was itself rewritten to avoid embedding the very fragment the new gate detects.]
  - **F8 (perf)**: migration v26 adds `idx_segments_human_decision` (the one hot filter column still unindexed; verdict/escalated/audio_path/verified already were — the audit overstated this). Test.
  - **F6 (fix, reliability)**: `wsl_7b_server_preflight()` — a bash `/dev/tcp` probe of :8799 inside WSL at import start turns a ~5-minute down-server hang into a ~2s actionable failure. Verified LIVE against the running 7B server (0.16s).
  - **F1 (eval)**: `scripts/scorecard_7b.py` drives the WARM 7B server over its socket (no second 31 GB load; same normalization as the fine-tuned scorecard). Verified on the FLEURS fixture: **7B micro CER 29.33% (N=1 SPOT CHECK)**. HONEST BLOCKER — a publishable/decision-grade number needs the gold corpus (chunk_7.zip + transcription TSV), NOT on disk now; the app DB holds only 3 human-verified segments. Default stays WSL7B (owner's explicit choice); F2 removed the silent-downgrade risk. On the one FLEURS clip the 7B (29.33%) did NOT beat the fine-tuned engine — indicative only; N=1 decides nothing.
  - **F5 (feat, UX)**: keyboard-first single-key review flow in ReviewMode (A/E/X/Space/R/N/P + Ctrl+Enter), guarded so typing is never hijacked and buttons keep native activation. Verified LIVE in the dev preview (accept advanced the queue; typing not hijacked; Escape blurs). DEFERRED honestly: word-click INLINE-edit (splice-back risks corrupting `editText` — needs careful UX, not shipped half-baked).
  - **F4 (build)**: rebuilt the frontend + release exe so the running app contains all the above; added a `make build-app` target that builds frontend-first so a bare `cargo build` can't ship a stale UI (`tauri build` already runs `npm run build` via beforeBuildCommand).
  - **Honest grade after this session**: still ~6.5–7/10 until the publishable 7B measurement + the deeper P2/P3 review-speed & jury-precision work land. These are real fixes to real defects, each behind a green gate — not a grade bump on tests alone.
  - **Full-gate validation (end of session)**: the F2 fail-hard change broke 7 test suites + the headless integration binary that had all relied on the removed silent WSL7B→CTC300M downgrade; fixed each to select the local CTC engine explicitly (import path is unchanged in production). Also fixed a PRE-EXISTING stale i18n e2e test (locale toggle shows endonyms English/کوردی; test asserted old short codes) and triaged 2 NEWLY-published quick-xml advisories (RUSTSEC-2026-0194/0195, build-time-only via Tauri's plist tooling, no in-range patch — documented dated ignore, not a blanket suppression). **Final: every gate green** — verify-10, typecheck 393/0, eslint, cargo fmt --check, clippy -D warnings, cargo test exit 0 (all suites incl. tauri_integration/soak/reliability), vitest 132/132, python-policies (hygiene zero-exemption), Playwright e2e 47/47, npm audit 0, cargo deny ok. Release exe rebuilt so the app the owner runs contains all fixes. Branch `audit/2026-07-02-deep-audit`, 15 commits, NOT pushed (owner's call).
  - **Deferred, honestly (not defects — P3 enhancements)**: IRT ability persistence/warm-start; word-click INLINE-edit (naive splice-back risks corrupting the edit text — needs careful UX). Recorded so they're not lost.
* **2026-07-02 (SECOND WAVE — un-deferred the deferrals + real F1)**: On feedback that honest deferrals ≠ done, exhausted the genuinely-doable:
  - **F1 upgraded from N=1 to a real measurement.** Found the owner's verified Halwest 16 kHz gold set on disk; measured the DEFAULT 7B via the warm server: **59.45% CER (N=66)** — but that is a DATA artifact: stock 300M scores **61.69% on the identical 66 clips** and the harness flags most as "drifted" (manifest text/audio boundaries misaligned). The 7B is coherent, correct Sorani, on-par-to-better than stock on identical data. Publishable 7B CER still needs a boundary-aligned gold set (clean FLEURS is N=1). `scorecard_7b.py` gained opt-in fair digit/punct normalization; docs/EVAL.md has the full honest write-up.
  - **word-click INLINE-edit — shipped the SAFE design**: double-tap a word selects it in the editor (never auto-rewrites), so zero corruption risk. Verified live (double-tap "کوردی" → exactly "کوردی" selected).
  - **IRT ability persistence — shipped OPT-IN**: `irt_ability_learning_enabled` (default off), migration v27 `model_abilities`, `fit_irt_consensus_with_priors` (empty priors ⇒ byte-identical to before), warm-start + persist in `run_t0_gate`. Off by default because its effect on auto-accept precision is measured in P3, not asserted. Tests: warm-start seeding, round-trip, migration.
  - **Bonus**: guarded a null conformal certificate in `refreshConformalThreshold` (dev-mock console error; real backend unaffected).
  - **F3 history leg → turnkey**: `scripts/scrub_git_history.sh` (dry-run default, owner token via env so the script carries no PII) removes the private dataset files + redacts the owner path form from ALL history, then guides the force-push. The agent does not force-push shared history itself (charter).
  - **Gates re-run green after all of the above**: cargo test exit 0 (all suites), clippy -D warnings, cargo fmt --check, typecheck 393/0, vitest 132/132, Playwright e2e 47/47, hygiene zero-exemption. Branch `audit/2026-07-02-deep-audit`, 21 commits, not pushed.
  - **Honestly still open (hard external blockers, not deferrals of doable work)**: a publishable 7B CER (needs a boundary-aligned ≥900 gold set); the git-history force-push (destructive, owner-only — script is ready); and full 10/10 (P3 jury-precision measurement, P4 GPU/accuracy) which need data/hardware/time beyond one session.

* **2026-07-02 (M0 foundations session)**: Implemented M0.1–M0.7 per FINAL_READINESS_10.md. Completed: (1) Sorani normalizer Python/Rust equivalence (fixture + test); (2) Honesty hotfixes (dead .bat advice, LOOP-0 safety, retire Halwest N=66); (3) DB restore IPC; (4) Metric provenance gate; (5) Crash handler + observation; (6) Git SHA baking + exe-is-HEAD assertion; (7) Ledger staleness gate. Gates: cargo test green, verify-10 GREEN, all 8 items verified. Deferred M0.4b–c (auto-snapshot rotation, optional polish) and M0.8 (history scrub, owner action). Branch: audit/2026-07-02-deep-audit, 29 commits. Next: M1 engine decision (FLEURS+CV22, zero owner hours).

---

## Ledger archive

Entries from 2026-07-02 through 2026-07-24 (sessions M2.1 through iteration 166) live in
[docs/ledger/ARCHIVE-2026-07-02_to_2026-07-24.md](docs/ledger/ARCHIVE-2026-07-02_to_2026-07-24.md)
— moved verbatim, nothing edited. The current era continues below.

---

### Iteration 167 — 2026-07-24 — Deep audit + roadmap; P0.2 diarization provenance guard (interactive loop)

**Owner-driven interactive run** (not the 02:00 cron): user asked for a deep audit, a brutal rating vs the
top 3, and the plan to #1, then started a 15-min implement loop (session cron d2bfd940). Reality check
pre-work: exe NOT running, git clean, HEAD 71e0972, lock free (re-acquired for this run).

**Deep audit (committed 15930dd → docs/ROADMAP_TO_NUMBER_ONE.md).** 12-agent adversarial Workflow (9
subsystem auditors + 3 web-research agents; 66 agents, ~5.5M tokens, 0 died), every HIGH/MED finding put
through independent refutation, TOP findings hand-verified against source by the orchestrator (agent
verdicts are not evidence). Scorecard avg 6.9/10 vs the top-professional bar — storage-durability 8,
panic-paths 8 (beat commercial tools); security 6, ops 6. Confirmed defect ledger with file:line + tiered
plan (Tier 0 honesty-law → Tier 1 security/reliability → Tier 2 frontend → Tier 3 test-structure → Tier 4
measured results). Web-cited ASR positioning: 7.03% CER on FLEURS ckb_iq is the best PUBLISHED FLEURS-ckb
CER (none lower exists); WER 32.93% ties ElevenLabs Scribe v1's published 32.1% — Scribe v2's "Kurdish
10-20%" tier is unverified and flagged to measure before any WER-leadership claim.

**FIX P0.2 (HIGH honesty) — runConfig.diarization recorded the raw settings flag, not real CAM++
loadability.** runs.rs:175 read `diarization: settings.enable_diarization` while the sibling `denoising`
one field up was already loadability-guarded (round-23). enable_diarization=true + CAM++ absent/unloadable
→ zero speaker labels produced (diarization.rs:94 emits empty embeddings → every chunk None; the fbank
fallback was deliberately removed as confidently-wrong), yet the exported bundle asserted diarization=true
— a recorded provenance lie. Fix: added ModelManager::diarizer_loadable() (mirrors denoiser_loadable +
the pipeline's own SpeakerEmbeddingService construction at pipeline.rs:1498-1499); config_from_settings now
takes diarization_active and records `enable_diarization && diarization_active`; export_bundle.rs passes
diarizer_loadable(). Regression gate run_config_records_diarization_only_when_actually_applied (fail-before:
the flag-only code recorded true for the requested-but-unloadable case). Adversarially checked the one real
trap — an fbank fallback that would still label speakers without CAM++ — and confirmed CLOSED at
diarization.rs:94 (no labels when is_available()==false), so the guard adds no inverse lie. Committed 10df0eb.

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched; a cold
isolated Tauri+ort build does not fit a 15-min iteration, and cargo test/clippy build debug not release):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (1m27s) ·
`cargo test --lib run_config_records` → `test result: ok. 2 passed; 0 failed; 0 ignored`.

**Full-suite reality check:** `cargo test --lib` → `test result: ok. 1001 passed; 0 failed; 6 ignored`
(89.83s; the 6 ignored are the model-fetch-gated real-audio tests). **NOT verified:** no rebuild of the
shipped exe (shipped provenance behavior changed — rebuild pending, owner's call).

**Next (roadmap execution order):** P0.3 wire DPAPI into set_api_key → P1.1 UNC guards on relink_audio/
merge_dataset_json/restore_segment_snapshot → P1.2 native fatal-error dialog. Tier-0 P0.1 (re-pin the
cross-engine normalization table) is owner-gated (GPU re-score). "Best / real #1" is NOT claimed — Tier-0
and Tier-1 are not yet shipped.

### Iteration 168 — 2026-07-24 — P0.3 DPAPI at-rest key encryption (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD 792a519, lock free (acquired). Next item per
roadmap execution order = P0.3.

**FIX P0.3 (MED security/honesty, audit H4) — API keys were stored in PLAINTEXT while the code advertised
DPAPI.** commands/settings.rs set_api_key (the sole production writer of secrets.env) called the plaintext
ApiKeys::save_key, so Gemini/ElevenLabs/OpenRouter keys sat cleartext in %APPDATA%\cortex-speech\secrets.env
— while save_key_protected (DPAPI CryptProtectData → NAME=dpapi:<base64>) was fully built + unit-tested with
ZERO production callers, and the module header claimed keys "persist via the DPAPI-protected key store."
Capability theater. Fix: wire set_api_key → save_key_protected. Existing plaintext keys keep loading
(parse_env_file decrypts dpapi: AND reads legacy plaintext) and upgrade to a blob on next save; empty value
still clears; on non-Windows a non-empty key errors rather than storing plaintext under a "protected" API
(the app ships Windows-only, so unreachable in production). Also corrected the now-stale "byte-for-byte
identical" header claim. Committed 457d30b.

Regression gate set_api_key_persists_via_dpapi_protected_store_not_plaintext (source invariant — the command
needs full AppState to invoke; scans only the pre-`mod tests` region so the assertion's own literals don't
self-match — a self-reference bug caught during authoring). **FAIL-BEFORE DEMONSTRATED:** reverting line 90
to save_key made the gate FAIL ("must persist keys via the DPAPI-protected store"); restored.

Adversarially verified — 3-skeptic Workflow (data-loss / cross-platform-CI / completeness), ALL refuted=false
severity NONE: no key loss or wrong read-back (round-trip verified; undecryptable blob → unset-and-logged,
never ciphertext); no non-Windows runtime path errors (non-Windows dpapi::protect is a compiling stub; CI
runs cargo test only on windows-latest, linux/mac jobs build only); set_api_key confirmed the ONLY production
secrets writer; protect() failure leaves secrets.env untouched and the frontend retains the paste for retry.

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (8.99s) ·
`cargo test --lib` → `test result: ok. 1002 passed; 0 failed; 6 ignored` (86.26s).

**NOT verified:** no rebuild of the shipped exe (at-rest key storage changed — rebuild pending, owner's call);
keys the owner PASTED into secrets.env by hand stay plaintext until re-saved through the Settings UI (that is
the user editing a file, not a code writer — out of scope; a one-time migration could be a later enhancement).

**Next (roadmap):** P1.1 UNC guards on relink_audio / merge_dataset_json / restore_segment_snapshot →
P1.2 native fatal-error dialog → P1.3 restore writer fence. Tier-0 done except owner-gated P0.1 (GPU re-score).
"Best / real #1" NOT claimed — Tier-1 not yet shipped.

### Iteration 169 — 2026-07-24 — P1.1 UNC/NTLM leak closed at 3 write paths + shared-guard hardening (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD d07782a, lock free (acquired). Next item = P1.1.

**FIX P1.1 (HIGH security, audit R1/R2b) — renderer-supplied UNC paths could drive the SMB redirector
(NTLM forced-auth leak) at three unguarded commands** (relink_audio, merge_dataset_json/all inserts,
restore_segment_snapshot) — the class #131 fixed for exports, left open on these siblings. Root-cause fix
at the SHARED write boundary: new validation::input::reject_unc_path (syntactic null+UNC, no canonicalize —
for write paths where the path is stored/searched as-is and the target may not exist yet); validate_segment
(the gate for insert_segment / insert_segment_full / insert_segments_batch / merge_dataset_json) now rejects
a UNC audio_path, so merge AND every restore-via-insert_segment_full caller (couch undo, history undo) are
covered — broader than the 3 audit-named sites. relink_audio + restore_segment_snapshot also get explicit
boundary guards (relink's raw UPDATE bypasses validate_segment). Committed cec0a1e.

**Adversarial verification FOUND A REAL LATENT GAP (fixed same commit).** 3-skeptic Workflow: over-blocking
NONE, bypass/completeness NONE, but the guard-completeness skeptic (running actual rustc on this box)
discovered the SHARED is_unc_path matched only Prefix::UNC|VerbatimUNC, while `\?\unc\...` (lowercase →
Prefix::Verbatim) and `\.\UNC\...` (device namespace → Prefix::DeviceNS) ALSO drive the redirector yet
slipped the guard under two aliases — a gap PRE-DATING this change (affected #131 export guard +
validate_file_path/validate_output_path too). **Hand-verified independently** with a standalone rustc probe
(scratchpad/unc_probe.rs): current guard=false for both spellings, hardened guard=true, and legit
VerbatimDisk/Disk/empty stay false. Hardened is_unc_path to match the "UNC" keyword case-insensitively in
the verbatim/device forms — closing all 4 spellings across every validator that routes through it.

Regression gates (both FAIL-BEFORE verified): reject_unc_path_blocks_null_everywhere_and_unc_on_windows
(null all-platforms; all 4 UNC spellings rejected; empty + verbatim-disk pass); insert_segment_rejects_unc_
audio_path_ntlm_leak_guard (db_tests — UNC rejected at write boundary + not persisted; local inserts;
FAIL-BEFORE shown by disabling the validate_segment guard → insert_segment(UNC) returned Ok → test failed →
restored).

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (8.52s) ·
`cargo test --lib` → `test result: ok. 1004 passed; 0 failed; 6 ignored` (89.54s).

**NOT verified:** no rebuild of the shipped exe (security behavior changed — rebuild pending, owner's call).
**Bonus beyond P1.1 scope:** the is_unc_path hardening also fixes the two-spelling gap in the pre-existing
#131 export guard and validate_file_path/validate_output_path.

**Next (roadmap):** P1.2 native fatal-error dialog (fatal_app_error is invisible under windows_subsystem) →
P1.3 restore writer fence (jury/Scribe/couch). Tier-0 done except owner-gated P0.1. Tier-1: 1 of ~4 shipped.
"Best / real #1" NOT claimed.

### Iteration 170 — 2026-07-24 — P1.2 native fatal-startup dialog (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD ae9d85e, lock free (acquired). Next item = P1.2.

**FIX P1.2 (HIGH, audit R2) — fatal startup errors were INVISIBLE in the release GUI.** fatal_app_error did
only tracing + eprintln + exit(1); windows_subsystem="windows" discards stdout/stderr and the tracing file
sink may not exist yet (data-dir create is itself a fatal path), so instance-lock-held / unopenable-or-
newer-schema-DB / data-dir-create / Tauri-build failures all presented as "double-click, nothing happens".
Now on Windows fatal_app_error pops a native MessageBoxW (raw Win32 via windows-sys + new
Win32_UI_WindowsAndMessaging feature — no webview exists this early, so tauri-plugin-dialog is unavailable)
with the real reason, then exits. Committed d8b5a29.

**Adversarial verification FOUND A REAL MED REGRESSION (fixed same commit).** 3-skeptic Workflow: FFI
soundness NONE (buffers NUL-terminated+live, null hwnd valid, signatures/constants match windows-sys 0.61,
no UB); always-exit / non-Windows / newer-schema-message-reachable all NONE; but the CI-hang skeptic found
that ALL FOUR CDP e2e drivers (e2e_real_app.cjs, e2e_{pipeline,constrained,finetuned}_ipc.cjs) + e2e_7b_
connect.cjs launch the exe WITHOUT a headless flag, three against the real %APPDATA% profile — so a fatal
startup (classically: InstanceLock fails because the owner's app is already open) would pop a modal and hang
the driver's ~90s connect poll. **Hand-verified**: grepped all drivers — every one sets
WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port. Fix: show_fatal_message_box self-suppresses
under is_headless_mode() OR is_cdp_remote_debug() (that WEBVIEW2 arg). Also softened MB_SYSTEMMODAL →
MB_TOPMOST|MB_SETFOREGROUND (visible without freezing the whole desktop). Only a genuine interactive double-
click reaches the box.

Regression gates: to_wide_null_is_nul_terminated_utf16 (the *W marshalling — asserts NUL terminator, empty
= [0], Sorani UTF-16 round-trip, no interior NUL) and cdp_remote_debug_suppresses_the_fatal_dialog (port arg
+ embedded form suppress; unrelated args / None do not). Both pure-predicate, fail-before by construction.

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (8.52s) ·
`cargo test --lib` → `test result: ok. 1006 passed; 0 failed; 6 ignored` (88.00s). Cargo.lock unchanged.

**NOT verified:** the dialog was NOT rendered live (unit tests can't pop a modal; the marshalling +
suppression logic are gated, the MessageBoxW side-effect is not) — a real fatal-startup render should be
eyeballed once on the owner's desktop. No rebuild of the shipped exe (startup behavior changed — pending).

**Next (roadmap):** P1.3 restore writer fence (invert to one writer registry; fence jury/Scribe/couch) →
P2.1 honest library-load failure → P2.2 unhandledrejection trap. Tier-1: 2 of ~4 shipped ("best/#1" NOT claimed).

### Iteration 171 — 2026-07-24 — P1.3 restore writer-fence completed (jury/Scribe/couch/alignment) (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD 3ed80be, lock free (acquired). Next item = P1.3.

**FIX P1.3 (HIGH, audit R3) — the restore writer fence was incomplete: 5 background writers escaped it.**
AppState::writers_active() (consulted by prepare_restore before db_restore/restore_db_from_snapshot) covered
only import/batch/WSL, so a restore could run while these dedicated-connection (or post-cloud) writers were
mid-write, mixing a late write into the just-restored library: Scribe vote batches, the jury pipeline/T2/DPO
command writers, the post-import single-file jury ADJUDICATION thread (spawned after import-complete → import
guard down), the DETACHED background-ALIGNMENT thread (outlives the import guard), and the Couch phone-review
server. Root-cause fix: one BG_DB_WRITERS counter + RAII BgDbWriterGuard as the single registration point for
dedicated-connection background writers (new writers take a guard, not another || term — closing the recurring
"forgot the new writer" class that CAUSED this bug). writers_active() gains bg_db_writers_active() +
SCRIBE_VOTES_IN_FLIGHT + couch::is_running(). Committed b236f8d.

**Adversarial verification FOUND A REAL MED GAP + I hand-found ANOTHER (both fixed same commit).** 3-skeptic
Workflow: guard-lifetime NONE (all guards named _bindings held across their writes), atomic/deadlock NONE
(balanced counter; writers_active never locks db; couch lock self-contained + acquired outside any other lock).
The completeness skeptic FOUND the detached background-alignment thread (pipeline.rs:2115 — own connection,
update_segment_alignment, outlives ImportGuard; non-default auto_align, alignment-columns-only → MED); I
independently hand-found the post-import adjudication thread (commands.rs:805). **Hand-verified BOTH against
source** (read pipeline.rs:2095-2164 + commands.rs:725-838) and did a FULL sweep of every Database::open +
std::thread::spawn — confirmed no dedicated-connection segment writer remains unfenced (batch/pipeline inline
jury run within Import/Batch state; WSL worker → WSL_REFINE_RUNNING; snapshot loop is a reader).

Regression gate writers_active_fences_background_db_writers (a held BgDbWriterGuard arms the fence + clears on
drop). **FAIL-BEFORE DEMONSTRATED**: removing the bg term made writers_active() false while a guard was held →
test failed → restored.

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (10.40s) ·
`cargo test --lib` → `test result: ok. 1007 passed; 0 failed; 6 ignored` (83.02s).

**NOT verified:** no rebuild of the shipped exe (restore-safety behavior changed — pending). The check-then-act
TOCTOU window in prepare_restore (a NEW writer STARTING between the check and the swap) is a SEPARATE increment
(a restore-pending reservation gate) — NOT closed here; this closes the "writer already running when restore is
clicked" case for every writer.

**Next (roadmap):** P1.4 model-load circuit breaker → P1.3b restore-pending reservation gate (the TOCTOU
window) → P2.1 honest library-load failure. Tier-1: P1.1/P1.2/P1.3-fence shipped (3 of 4 + the reservation
sub-item). "Best / real #1" NOT claimed.

### Iteration 172 — 2026-07-24 — P1.4 ASR model-load circuit breaker (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD 8c12daf, lock free (acquired). Next item = P1.4.

**FIX P1.4 (audit R4) — a present-but-corrupt/unloadable ONNX model re-hashed gigabytes on EVERY call.**
AsrPool::ensure_loaded retried a cached-UNAVAILABLE service unconditionally (round-24 fresh-install
recovery), re-running new_with_config's full-file SHA-256 verify + ONNX load — for a 300 MB–1.4 GB model,
once per chunk during import and twice per segment. Circuit breaker keyed on a cheap file fingerprint
((model size,mtime),(tokens size,mtime)) + a 30 s half-open cooldown: present-but-failed is skipped for the
cooldown then re-probed; absent always retries cheaply (round-24 preserved); a re-download re-attempts
immediately. The AVAILABLE fast path now short-circuits with zero disk access. Committed 45aca9a.

**Adversarial verification CAUGHT A HIGH REGRESSION in my first cut (fingerprint-only, no cooldown) — fixed
before commit.** 3-skeptic Workflow: a TRANSIENT failure (an AV/sharing lock on the freshly-downloaded ONNX
making compute_file_sha256 open/read Err — models.rs:992; this box's own "Windows FS flaky" memory makes it
real) latched a GOOD file as permanently failed, wedging the exact round-24 flow to a restart. **Hand-verified
against source** and matched my own pre-verdict concern. Fix: the half-open cooldown re-probes the same file
after 30 s → self-heals. Wiring + soundness lenses returned NONE (latch set only on present+fail, cleared on
success/absent; body atomic under the pool Mutex; exact truth table; no panic on a mtime-less platform).

Regression gate model_load_breaker_skips_within_cooldown_then_retries_absent_changed_or_elapsed (pure: within-
cooldown skip; absent/changed/elapsed all attempt). round-24 test still passes.

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (9.66s) ·
`cargo test --lib` → `test result: ok. 1008 passed; 0 failed; 6 ignored` (87.20s).

**NOT verified:** no rebuild of the shipped exe (ASR-load behavior changed — pending). Residuals (honest): a
genuinely-corrupt model re-hashes at most once/30 s during an active import (bounded, not per-chunk); a
transient failure yields up to 30 s of ASR-unavailable before self-heal.

**Tier-1 status:** the four MAIN items P1.1/P1.2/P1.3-fence/P1.4 are shipped + adversarially verified. TWO
deferred sub-items remain before Tier-1 is fully closed: P1.3b restore-pending reservation gate (the TOCTOU
window) and P1.4b denoiser/diarization streaming-loop rebuild retry. "Best / real #1" is therefore NOT claimed.

**Next (roadmap execution order — correcting a small out-of-order pick):** P2.1 honest library-load failure
(frontend; npm gates) → P2.2 window unhandledrejection trap → P1.3b reservation gate → P1.4b denoiser retry →
P0.4 per-segment provenance → P2.3/P2.4.

### Iteration 173 — 2026-07-24 — P2.1 honest library-load failure + a red-gate fix from P1.1 (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD d3ce56d, lock free (acquired). Next item = P2.1.

**GATE FIX (honesty) — P1.1 (cec0a1e) left run_python_policies.py RED.** Running the python policy gate this
iteration surfaced that a P1.1 verbatim-disk test string embedded a private user-profile path segment (the
form test_windows_repo_hygiene forbids in a public repo — do NOT write that literal here either). P1.1 was
Rust-only so I ran the cargo gates but not the python policies — which scan Rust too — and missed it. Changed
the string to a profile-free verbatim-disk path (still non-UNC; the test's point is unchanged). Committed
eab9bb0. Lesson logged: run the python policies even on Rust-only changes (several policies scan the whole
tree, not just python) — AND never quote a forbidden profile path in a tracked doc/ledger while describing it.

**FIX P2.1 (audit F1) — a DB/IPC read failure rendered as an empty library.** segmentStore.ts load() swallowed
failures to console.error and never updated the store, so a first-load failure showed "No segments loaded" —
indistinguishable from a wiped library. New libraryLoadError store (cleared on success, set in catch + a toast
via the reused, previously-dead notifications.loadSegmentsFailed key); App.svelte empty-state gains a leading
{#if $libraryLoadError} branch with the real error + a Retry. Committed 4753edc.

**Adversarial verification FOUND A REAL RACE (fixed same commit).** 2-skeptic Workflow: rendering / import-
cycle / i18n-parity / tailwind all NONE; the lifecycle skeptic found the success-path libraryLoadError.set(null)
ran AFTER the awaited conformal-threshold refresh WITHOUT a seq guard, so an older load resuming after a newer
load already failed would clear the newer error (silently dropping the failure — the F1 bug, in a race).
Hand-verified against source; added the same `if (seq !== loadSeq) return;` the page loop uses, before the clear.

Regression gates (both FAIL-BEFORE verified): failing-load-sets-error + retry-clears; and superseded-run-does-
not-clear-newer-error (removing the guard failed it: expected 'a newer load already failed', got null).

Gate (frontend; Rust untouched beyond the hygiene one-liner): `npm run typecheck` → 0 errors ·
`npm run lint` → 0 errors (4 pre-existing warnings) · `npm test` → 209 passed ·
`python scripts/run_python_policies.py` → 41 passed · `cargo test --lib reject_unc_path_blocks` → ok.

**NOT verified:** no rebuild of the shipped exe (UI behavior changed — pending); the error branch renders only
in the segment-list view (a failure on another view is surfaced by the toast, which auto-dismisses in 8 s).

**Next (roadmap):** P2.2 window unhandledrejection trap → P1.3b reservation gate → P1.4b denoiser retry →
P0.4 → P2.3/P2.4. Tier-1 main items done; two sub-items + Tier-2 remain. "Best / real #1" NOT claimed.

### Iteration 174 — 2026-07-24 — P2.2 global unhandledrejection trap + a ledger hygiene self-fix (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD c4cd48d, lock free (acquired). Next item = P2.2.

**LEDGER HYGIENE SELF-FIX (honesty) — my OWN iter-173 ledger entry left test_windows_repo_hygiene RED.**
Running the python gate this iteration flagged PROGRESS_LEDGER.md itself: the iter-173 entry, while
DESCRIBING the P1.1 hygiene fix, QUOTED the forbidden private-profile path literal — and the hygiene scanner
reads every tracked file, ledger included. Rewrote that entry to describe the issue without reproducing the
literal. Lesson compounded: never quote a forbidden profile path in ANY tracked doc, even to explain a fix.
(An earlier agent hit the same trap at ledger line 524 and phrased it safely — precedent I should have followed.)

**FIX P2.2 (audit F3) — un-awaited promise rejections vanished.** No window 'unhandledrejection' listener
existed; ErrorBoundary hooked only synchronous 'error' events, so a rejected fire-and-forget promise (an
onclick invoke() that fails, a teardown write to a closed webview) disappeared with no user trace. New
src/lib/globalErrorTrap.ts (describeRejection + notifyUnhandledRejection + idempotent installGlobalErrorTrap);
main.ts installs it before mount. Routes to a TOAST (notifications.error), never a panel-blanking boundary.
New i18n key notifications.unexpectedError (en+ckb; Sorani flagged for owner verification). Committed 9f07814.

**Adversarially verified — 2-skeptic Workflow, BOTH refuted=false / NONE** (first clean code iteration this
session): no loop/re-entrancy (handler fully synchronous), no double-report (unhandledrejection fires only for
still-unhandled rejections, so the existing per-invoke() catches are unaffected), bounded noise (8 s
auto-dismiss), no empty toast, idempotent, no ErrorBoundary overlap, i18n parity confirmed by tests/lib/
i18n.test.ts, test non-vacuous. The one edge (a pathological non-serializable rejection reason) is
app-unreachable and not a loop — left as-is (hardening it would be over-engineering).

Regression gate tests/lib/globalErrorTrap.test.ts (fail-before: the module did not exist).

Gate (frontend; Rust untouched): `npm run typecheck` → 0 errors · `npm run lint` → 0 errors (4 pre-existing
warnings) · `npm test` → 211 passed · `python scripts/run_python_policies.py` → 41 passed (incl. i18n parity +
windows-repo-hygiene, both green after the ledger self-fix).

**NOT verified:** no rebuild of the shipped exe (UI behavior changed — pending); the Sorani wording of
notifications.unexpectedError is my rendering, not a native review.

**Next (roadmap):** P2.3 → P2.4 (remaining Tier-2) → P1.3b reservation gate → P1.4b denoiser retry → P0.4.
Tier-1 main items done; Tier-2 progressing (P2.1, P2.2 shipped). "Best / real #1" NOT claimed.

### Iteration 175 — 2026-07-24 — P2.3 two whole-row-clobber paths + a NEW 4-path finding queued (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD d2d0ad1, lock free (acquired). Next item = P2.3.

**FIX P2.3 (audit F2, cardinal whole-row-clobber class) — two paths closed.** (1) ReviewMode unmount flush:
whole-row api.updateSegment({...seg, annotatedTranscript}) -> targeted api.updateSegmentFields(seg.id,
{annotatedTranscript}) (never touches alignmentJson -> clobber-safe even mid-align; backend re-reads the fresh
row under the db lock, no TOCTOU). (2) handleNormalize: added `if ($isProcessing) return;` + disabled button
(normalizedTranscript is not field-update-whitelisted, so it must whole-row upsert; the store is stale vs the
DB during a batch, so refuse during one — matching every sibling). Committed f558751.

**Adversarial verification CONFIRMED the two fixes AND found a NEW reachable 4-path instance of the same
class (queued, NOT silently shipped).** Lens 1 (unmount flush) = NONE end-to-end. Lens 2 refuted COMPLETENESS
(MED): the SAME clobber lives in FOUR ReviewMode mutators — submit / markBad / doRetranscribe / go-draft —
which build whole-row upserts from freshRow (the STORE), stale vs the DB during a background batch, and guard
only saving/retranscribing/aligning, never $isProcessing. **Hand-verified reachable**: enterReviewMode
(App.svelte:895) does NOT gate $isProcessing, and batches run on a Rust background thread, so a reviewer can
submit/markBad/re-transcribe a segment a batch is concurrently writing -> the store-based upsert reverts the
batch's write. The freshRow comments only reason about the background ALIGNER (which reloads the store via
ensureWordTimings), not a batch (which does not) — so the existing mitigation does not cover this. These are
PRE-EXISTING (untouched by P2.3) and the P2.3 commit message + this entry DISCLOSE them rather than claiming
the class is closed.

**Regression gate** tests/lib/clobber-guards.test.ts (source invariants; fail-before verified by neutralizing
the handleNormalize guard -> test failed, then restored).

Gate (frontend; Rust untouched): `npm run typecheck` → 0 errors · `npm run lint` → 0 errors (4 pre-existing
warnings) · `npm test` → 213 passed · `python scripts/run_python_policies.py` → 41 passed.

**NOT verified:** no rebuild of the shipped exe (UI behavior changed — pending). The 4 ReviewMode paths above
are NOT fixed yet.

**Next: P2.3b (NEW, adversarial-found) — close the 4 ReviewMode clobber paths:** submit/markBad/go-draft ->
updateSegmentFields (annotatedTranscript/verified are whitelisted); doRetranscribe -> import $isProcessing +
guard (rawTranscript is not whitelisted, and re-transcribe during a batch is a machine op safe to refuse).
Then P2.4 → P1.3b → P1.4b → P0.4. "Best / real #1" NOT claimed (the clobber class is NOT fully closed yet).

### Iteration 176 — 2026-07-24 — P2.3b: the 4 ReviewMode clobber paths — CARDINAL CLASS NOW CLOSED (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD 6c2b524, lock free (acquired). Next item = P2.3b
(the 4 paths the P2.3 adversarial pass found).

**FIX P2.3b — closed the 4 ReviewMode whole-row-clobber paths.** submit/markBad/go -> api.updateSegmentFields
(targeted; annotatedTranscript/verified whitelisted); go also dropped its now-needless `!aligning` skip (the
field update never touches alignmentJson and serializes under the db lock against the aligner's targeted
alignment_json UPDATE — either ordering preserves both). doRetranscribe/retranscribe -> added $isProcessing to
the guard (rawTranscript is NOT whitelisted so it stays whole-row, but is refused during a batch). Committed
e6984b6.

**Adversarially verified — 2-skeptic Workflow, BOTH refuted=false / NONE.** Conversions correct (no persist
lost; the submit wrong-segment guard stays between the store write and the editor write; **go-during-align
does NOT race** — confirmed the aligner's alignment_json UPDATE and go's annotatedTranscript field-update are
serialized under the db lock, either ordering safe). doRetranscribe guard effective. **Completeness: the
whole-row-clobber class is CLOSED** across App.svelte + ReviewMode.svelte — every remaining api.updateSegment(
is $isProcessing-guarded with freshRow-by-id (App transcribe handlers + handleNormalize; ReviewMode
doRetranscribe), and every whitelistable review persist is field-targeted (submit/markBad/go/unmount-flush);
grep confirms no 5th path and no invoke('update_segment') bypass. Hand-verified the same sweep independently.

**GATE UPDATE (STRENGTHENED, not weakened) — scripts/test_frontend_review_guards.py.** My refactor tripped
the policy's own not-vacuous self-check (good design). Updated test_submit's store-write marker to
updateSegmentFields (SAME wrong-segment-guard invariant, moved marker) and REWROTE test_go to require the
targeted update and REJECT any whole-row updateSegment in go() — a structurally-clobber-proof invariant,
STRONGER than the old whole-row+!aligning+freshRow guard. Mirrors the pre-existing
test_app_save_handlers_use_field_level_updates policy.

Regression gate tests/lib/clobber-guards.test.ts (source invariants over the 4 handlers; FAIL-BEFORE verified
by removing doRetranscribe's $isProcessing -> test failed, then restored).

Gate (frontend + policies; Rust untouched): `npm run typecheck` → 0 errors · `npm run lint` → 0 errors
(4 pre-existing warnings) · `npm test` → 214 passed · `python scripts/run_python_policies.py` → 41 passed.

**NOT verified:** no rebuild of the shipped exe (UI behavior changed — pending). **Accepted residual** (pre-
existing, out of scope): doRetranscribe's whole-row upsert is still raceable by the WSL-7B refine loop —
identical to the accepted App transcribe siblings, largely benign (it replaces the very transcript the
metadata describes).

**MILESTONE:** the whole-row-clobber class (the cardinal data-loss bug, fixed 6x before this session, then
found in 6 more paths by the audit + adversarial verification) is now FULLY CLOSED across the review + curate
surfaces.

**Next (roadmap):** P2.4 i18n the core CKB surfaces → P1.3b reservation gate → P1.4b denoiser retry → P0.4.
Tier-2: P2.1/P2.2/P2.3/P2.3b shipped; P2.4 remains. "Best / real #1" NOT claimed.

### Iteration 177 — 2026-07-24 — P1.3b restore-pending reservation gate (interactive loop; reordered ahead of P2.4)

Reality check pre-work: exe NOT running, git clean, HEAD 1c8ecfc, lock free (acquired).

**REORDER (independent judgment):** the roadmap put P2.4 (i18n the core CKB surfaces) next, but assessing it
found RefineryPanel alone needs ~30 Sorani translations incl. technical ASR terms (CER/ASR/eval) — the
CORRECT translations are genuinely OWNER-GATED (native Sorani review), and injecting my best-guess Sorani
into a technical panel would violate "surface owner-gated items, never fake them" (the parity gate also forces
ckb values I can't verify). Per the owner's reliability-first priority, pivoted to the fully-verifiable Rust
reliability item P1.3b. P2.4 is marked substantially owner-gated (the i18n WIRING is doable; the translations
need the owner).

**FIX P1.3b (audit MED) — the restore reservation gate closes the check-then-act TOCTOU the P1.3 fence left.**
New RESTORE_PENDING atomic + RAII RestoreReservation; prepare_restore reserves BEFORE the writers_active fence
and returns the guard (both restore callers bind it across the swap). Every writer-start refuses while
restore_pending(): try_start_import/batch, run_wsl_refinement, add_scribe_votes/run_dpo_update/run_jury_
pipeline/run_t2_for_segment, couch::start; import-spawned threads transitively covered. Committed 11202c8.

**Adversarial verification FOUND + I FIXED a residual TOCTOU in the first cut.** import/batch were airtight
(check under the same mutex writers_active reads). But the 5 atomic-flag writers (WSL/scribe/jury/couch)
checked the reservation BEFORE registering their flag (reversed-order double-check) → a narrow residual race
(the agent rated it LOW real-world on a single-user desktop), AND my prepare_restore comment OVERCLAIMED a
single-lock guarantee for all writers. **Hand-verified** the race (classic lock-free check-then-publish).
Fixed: couch's check MOVED under the COUCH lock (mutex-airtight like import/batch); WSL/scribe/jury use
PUBLISH-THEN-RECHECK (set flag, then re-read the reservation, roll back if set — SeqCst makes the two
orderings unable to both read stale); corrected the overclaiming comment to describe the real per-writer
mechanism.

**Regression gate** scripts/test_restore_reservation_gate.py (auto-discovered → 42 policies): pins reserve-
before-fence + guard-returned + both callers bind it + every writer-start checks restore_pending().
FAIL-BEFORE verified (removing try_start_batch's check failed the policy). [Note: my fail-before demo used
`git checkout src/lib.rs` which reverted my uncommitted lib.rs edits — re-applied both checks; lesson: use
targeted edits for temp reverts, never git checkout.]

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (9.60s) ·
`cargo test --lib` → `test result: ok. 1008 passed; 0 failed; 6 ignored` · `python run_python_policies.py` → 42 passed.

**NOT verified:** no rebuild of the shipped exe (restore-safety behavior changed — pending).

**Tier-1 status:** P1.1/P1.2/P1.3-fence/P1.4/P1.3b shipped; P1.4b (denoiser/diarization streaming retry)
remains. **Next: P1.4b** → P0.4 per-segment provenance → Tier-3. P2.4 i18n is owner-gated (native Sorani).
"Best / real #1" NOT claimed.

### Iteration 178 — 2026-07-24 — P1.4b streaming denoiser/diarization rebuild breaker (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD 8e4edaa, lock held (iter12-P1.4b). Frontend
untouched → npm gates correctly skipped.

**FIX P1.4b (audit R4) — bound the streaming per-window service rebuild to once per file.** The
per-90s-window loop in `process_single_file_streaming` (pipeline.rs) rebuilt a cached denoiser/diarization
service whenever it was unset OR inactive (`map_or(true, |s| !s.is_active())`). For a PRESENT-but-unloadable
model (corrupt/partial ONNX) that meant a full GPU-then-CPU load attempt on EVERY 90 s window — the same
retry-storm P1.4 fixed for the ASR loader, but per window instead of per call. Added a pure helper
`should_rebuild_streaming_service(present, active, already_tried) = !present || (!already_tried && !active)`
and two function-local `*_rebuild_tried` flags: a missing service is still built; a present-but-inactive one
is re-attempted AT MOST ONCE per file, never per window. Flags are locals of `process_single_file_streaming`
(one streaming call per file, verified: only call site pipeline.rs:1469, itself once-per-file in the import
loop), so a model that appears mid-session recovers on the NEXT file — #132's between-file recovery preserved
at file granularity, matching the non-streaming sibling. Committed 510436c.

**Faithful refactor (hand-verified algebra):** for a healthy loadable model the helper yields the identical
decision to the original inline condition across all three REACHABLE states — None→build, Some+active→reuse,
Some+inactive→rebuild (the (present=false, active=true) pair is unreachable because `is_some_and` is false on
None). The only new behavior is the once-per-file latch.

**FAIL-BEFORE:** new unit test `streaming_service_rebuild_is_bounded_to_once_per_file` (pipeline_tests.rs)
pins the truth table; with the latch dropped via a TARGETED edit (`!present || !active`, `let _ = already_tried`
— NOT git checkout) it failed exactly on `present+inactive+already_tried → SKIP`, and passes with the fix.

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (9.57s) ·
`cargo test --lib` → `test result: ok. 1009 passed; 0 failed; 6 ignored` · `python run_python_policies.py` → 42 passed.

**Adversarially verified** (Workflow, 3 independent skeptics tasked to REFUTE against source): faithful-healthy-path,
recovery-132-preserved, no-other-per-window-reload — all returned refuted=false / severity none, each citing
exact lines matching my hand-verification (VAD/ASR/wav2vec2 all use their own session caches or the P1.4-style
cooldown breaker, so no other unbounded per-window ONNX reload remains). No CONFIRMED findings; no survivors.

**NOT verified:** no rebuild of the shipped exe (streaming-loop behavior changed — pending); the breaker is
proven by unit truth-table, not by a live run against a genuinely corrupt on-disk model.

**Tier-1 status:** P1.1/P1.2/P1.3-fence/P1.4/P1.3b/P1.4b all shipped — Tier-1 reliability items COMPLETE.
**Next: P0.4** per-segment provenance (schema migration) → Tier-3. P2.4 i18n owner-gated (native Sorani).
"Best / real #1" NOT claimed.

### Iteration 179 — 2026-07-24 — P0.4 per-segment provenance, WRITE SIDE (denoised/diarized) (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD 0ae584d, lock held (iter13-P0.4). Mapped the
whole change surface first with a 3-agent Workflow (export-runConfig / segment schema+migrations /
import-time provenance) + direct reads before editing.

**THE DEFECT (H3, audit MED).** The export bundle's manifest `runConfig` (export_bundle.rs:398) stamps a
SINGLE denoising/diarization flag computed at EXPORT time from *current* model loadability
(`config_from_settings(settings, denoiser_loadable(), diarizer_loadable())`) onto every segment — the
temporal inverse of the #132 fix. The mapping confirmed there was NO stored per-segment truth to read
instead (the `dataset_runs.config_json` table is DEAD — only ever CREATE/DROP, never read/written), so
closing it needs a persisted per-row provenance first.

**SCOPE (independent judgment — decomposed the roadmap's largest item along a real complexity boundary).**
P0.4 = schema migration + per-row population + export-read. This iteration ships the WRITE side for the
two signals computable inline at the single import construction site with zero new plumbing and airtight
semantics: `denoised` = `enable_denoising && denoiser_service.is_active()`, `diarized` =
`enable_diarization && embedding_service.is_available()` (the setting on AND the model actually loadable —
whether processing RAN, not the bare flag). Deferred: `vad_backend` (the VAD stack computes the backend in
audio.rs and DISCARDS it — threading it up through plan_speech_chunks + 2 call sites + a "none" state + a
present-but-corrupt-model-falls-back-to-energy lie-risk is its own slice) and the export-READ that closes
the visible lie (next slice).

**CHANGE (committed 82eff42).** Migration v41 adds nullable `denoised`/`diarized` INTEGER to speech_segments
(STRICT-compatible; ALTER ADD COLUMN fires no FK cascade → not in FK_OFF_MIGRATIONS). Nullable is
deliberate: NULL = "not recorded" for legacy rows — a fabricated 0 would be its own provenance lie. Threaded
through the struct, the shared SEGMENT_SELECT_COLUMNS + positional map_row (idx 32/33), and all three insert
paths (insert_segment, insert_segment_full — restore stays lossless, insert_segments_batch — the import
path), mirroring the v36 proof-metadata columns. Populated at pipeline.rs build_segments_from_pcm (the ONE
local-import site; both streaming + non-streaming route through it). Scribe cloud path leaves them None (no
local denoise/diarization runs there — honest "not recorded", not a fabricated value).

**FAIL-BEFORE:** db_tests `per_segment_processing_provenance_round_trips_and_stays_unknown_for_legacy_rows`
uses DISTINCT true/false through the import + restore paths + None-stays-NULL; a TARGETED revert of the
map_row read (denoised→None) failed it `left:None right:Some(true)`, passes with the fix. Also fixed the v40
STRICT-recreate test: it re-ran v40 in isolation (which drops the new columns) then read via a HEAD-schema
getter — now re-applies every post-v40 migration first (faithful to a real v40-THEN-v41 upgrade; weakens
none of v40's STRICT/rowid/cascade/index assertions).

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (12.68s) ·
`cargo test --lib` → `test result: ok. 1010 passed; 0 failed; 6 ignored` · `python run_python_policies.py` → 42 passed.

**Adversarially verified** (Workflow, 4 independent skeptics vs source): positional column alignment,
population semantics + missed sites, migration safety/replay/idempotency, serde/consumer back-compat — ALL
refuted=false. No CONFIRMED findings. Two survivor notes (both non-defects, hand-verified): (1) the JSON/JSONL
per-row export flattens SpeechSegment, so it now ADDITIVELY carries denoised/diarized for free (honest; no
consumer depends on the exact key set — export tests green); (2) LOW/optional — Scribe could record
Some(false) rather than None (deferred to the read-side slice, where cloud rows resolve via cloud_call).

**NOT verified / NOT yet closed:** the export manifest STILL recomputes runConfig from export-day model
state — the visible H3 lie is NOT closed until the export-READ slice reads these stored columns. No exe
rebuild (import-write behavior changed). The write side is proven by unit round-trip, not a live import run.

**Tier-0 status:** P0.2/P0.3 shipped; P0.4 WRITE side shipped, P0.4 READ side + vad_backend remain; P0.1
owner-gated (GPU re-score). **Next: P0.4 read side** (export reads stored provenance, closing H3) →
vad_backend → Tier-3. "Best / real #1" NOT claimed.

### Iteration 180 — 2026-07-24 — P0.4 READ side: manifest reads stored provenance, closes H3 (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD eefbd8b, lock held (iter14-P0.4read). Mapped the
export-runConfig site + all consumers first (3-agent map already had it; confirmed no rust test / no
frontend reads runConfig — free to make it honest).

**H3 CLOSED (audit MED).** The export bundle manifest's `runConfig.denoising/diarization` was computed at
EXPORT time from current model loadability (`config_from_settings(settings, denoiser_loadable(),
diarizer_loadable())`) and stamped as one flag on every segment regardless of what actually processed each
clip — the temporal inverse of #132. The write side (iter179) persisted per-segment truth (v41
denoised/diarized); this reads it. Committed c296ddc.

**CHANGE.** New private `ProvenanceCounts { applied, not_applied, not_recorded }` with `tally(iter<Option<
bool>>)` + `all_applied()` (unanimity: true iff every exported row recorded the model as having run) +
`to_json()`. The manifest now tallies over the EXPORTED rows' stored `denoised`/`diarized` and emits
`processingProvenance: { total, denoised:{applied,notApplied,notRecorded}, diarized:{...} }` (the honest
per-segment distribution — a MIXED export is never collapsed), and `runConfig.denoising/diarization =
all_applied()` (a unanimity bool, not a fabricated aggregate). The two `*_loadable()` probes are removed
from the manifest (methods kept as pub capability API; `model_manager` still used for the separate
model_manifest.json installed-model report — a legit export-day capability list, NOT per-segment provenance).
Other runConfig fields (model_version/vad/durations/normalization) remain the settings snapshot.

**SCOPE (independent judgment).** The manifest runConfig IS the audit's H3 finding — now closed. The milder
HF-README sibling (export.rs still prints `settings.asr_model_size`) reads a DIFFERENT signal
(model_version_id) with a written-subset nuance → deferred to its own slice; vad_backend likewise.

**FAIL-BEFORE.** `manifest_reads_stored_per_segment_provenance_not_export_day_model_state` exports a MIXED
set (denoised applied/not/unrecorded = 1/1/1, diarized unanimous 3/0/0) with NO models on disk; a TARGETED
revert of `rc.diarization` to the old `model_manager.diarizer_loadable()` made it `Some(false)` where stored
truth is `Some(true)` → test failed, passes with the fix. Plus a pure `ProvenanceCounts::tally/all_applied`
unit test (empty→false, mixed→false, unrecorded-breaks-unanimity).

**Superseded gate UPDATED, not weakened.** test_rust_runtime_panic_policy.py's bundle-runConfig check
REQUIRED export-day `denoiser_loadable` (the round-23 honest interim, before per-segment provenance existed).
Renamed + rewritten to the STRICTER invariant: REQUIRE stored-per-segment reads (ProvenanceCounts /
denoised_provenance / diarized_provenance / processingProvenance) AND BAN the four export-day probes in
export_bundle.rs. Verified non-vacuous (banned counts 0, required present; removing any fails it).

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (15.03s) ·
`cargo test --lib` → `test result: ok. 1012 passed; 0 failed; 6 ignored` · `python run_python_policies.py` → 42 passed.

**Adversarially verified** (Workflow, 4 independent skeptics vs source): H3 residual-leak, counted-over-
exported-rows, semantics + P0.2 guard intact, policy-not-weakened + siblings — ALL refuted=false / severity
none. Hand-verified: the counted set (post holdout/rejected/placeholder filters) is IDENTICAL to what
export::export_dataset ships (same three filters, no training-ready-only drop) → processingProvenance.total
== segmentCount == shipped rows.

**NOT verified / remaining:** no exe rebuild (export output changed — a live export would show the new
manifest fields). H3 (denoising/diarization) is CLOSED for the bundle manifest; the HF-README model_version
sibling + `vad_backend` per-row provenance are NOT yet done. The Scribe cloud path records None
(not-recorded) for denoised/diarized — honest (no local processing), reads as notRecorded in the distribution.

**Tier-0 status:** P0.2/P0.3/P0.4(write+read, H3 closed) shipped; P0.4 vad_backend + HF-README model_version
remain; P0.1 owner-gated (GPU re-score). **Next: P0.4 vad_backend** → HF-README model_version → Tier-3.
"Best / real #1" NOT claimed.

### Iteration 181 — 2026-07-24 — P0.4 vad_backend: record the VAD backend actually used (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD 4a6cf03, lock held (iter15-P0.4vad). Traced the full
VAD stack (audio::voice_activity_detection → chunking::plan_speech_chunks → 2 pipeline call sites) + all
callers before editing.

**COMPLETES P0.4 per-segment provenance** (denoised/diarized = v41). The VAD backend (Silero vs energy
fallback) was decided inside `voice_activity_detection` and DISCARDED — the export could not say how each
clip's regions were detected, and a path-exists probe would LIE (a present-but-broken Silero falls back to
energy at RUNTIME). Committed 41cdfe1.

**CHANGE (root-cause: surface the ACTUAL backend from the detector, thread it, persist it, report it).**
New `pub enum VadBackend { Silero, Energy, None }` (audio.rs); `voice_activity_detection` returns
`(regions, backend)` — Silero only on a successful Silero detect() (cached/fresh), Energy on every
fallback, None for empty input. `plan_speech_chunks` returns `(regions, backend)` — None for the
needs_chunking()==false whole-buffer path (no VAD ran), else the detector's backend (post-processing only
reshapes regions). Both pipeline call sites (non-streaming + streaming) thread it into
build_segments_from_pcm (new param), stamped on every segment at the single construction site; streaming
stamps each 90 s window's segments with THAT window's backend (honest per-window truth). Scribe cloud path
stays None (no local VAD). Migration v42 (nullable TEXT vad_backend) + struct field + SEGMENT_SELECT_COLUMNS
+ positional map_row (idx 34) + all 3 insert paths (mirror the v41 plumbing). Export processingProvenance
gains `vadBackend: { byBackend: {silero,energy,none}, notRecorded }` — the stored distribution, honest for a
mixed export. The 4 integration-test call sites (audio_integration/e2e_pipeline/real_audio/audiobook_smoke)
updated to the tuple return.

**FAIL-BEFORE:** chunking `plan_short_audio_single_chunk` asserts the short-file whole-buffer path reports
`VadBackend::None`; mislabeling that branch `Silero` (TARGETED edit) failed it `left: Silero right: None`,
passes with the fix. Plus db round-trip (distinct silero via batch + energy via full, None stays NULL) and
the export manifest distribution (silero=2, energy=1, notRecorded=0); `test_vad_empty` asserts None.

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok · `cargo test --lib` →
`test result: ok. 1012 passed; 0 failed; 6 ignored` · `python run_python_policies.py` → 42 passed.

**Adversarially verified** (Workflow, 4 independent skeptics vs source): backend-is-the-one-actually-used,
chunking+pipeline threading, db positional + migration v42, export distribution + consumers — ALL
refuted=false / severity none. The backend skeptic surfaced a DOC-COMMENT inaccuracy (my Energy comment said
"failed integrity → energy", but an integrity/ONNX-load failure `?`-propagates an Err — no regions — never a
false Silero); NOT a code defect, but I corrected the comment for provenance accuracy before commit.

**NOT verified / remaining:** no exe rebuild (import/export behavior changed — a live run would show
per-segment vad_backend + the manifest vadBackend distribution). Backend truth is proven by unit tests, not
a live run against a genuinely corrupt Silero model. Scribe/legacy rows read as notRecorded (honest).

**Tier-0 status:** P0.2/P0.3/P0.4 (write + read/H3 + vad_backend) all shipped — Tier-0 complete EXCEPT the
owner-gated P0.1 GPU re-score. **Next: HF-README model_version sibling** (export.rs reads stored
model_version_id) → Tier-3. "Best / real #1" NOT claimed.

### Iteration 182 — 2026-07-24 — HF dataset-card ASR model provenance (export-day-state sibling) (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD 1d69a81, lock held (iter16-hfmodel). Traced the HF
write loop (exported_ids tracking) before editing.

**FIX (milder cousin of H3).** The HuggingFace dataset card's Provenance line printed
`format!("{:?}", settings.asr_model_size)` — the EXPORT-DAY setting — regardless of which model actually
produced each shipped clip (a corpus assembled across a 300M↔7B switch, exported later, was labeled with
only the current dropdown value). Now the card names the distinct STORED `model_version_id` of the rows
ACTUALLY WRITTEN to the dataset. Committed 9157a74.

**CHANGE (export.rs export_huggingface_dataset).** `written_ids = train_ids ∪ val_ids ∪ test_ids` (the ids
`exported_ids` pushes ONLY after a clip is written to disk + CSV, so not-training-ready / missing-coverage /
unavailable-audio / bad-alignment rows are excluded); `written_models = distinct model_version_id of the
segments whose id ∈ written_ids`, joined + sorted (BTreeSet → byte-reproducible card + SHA256SUMS). Empty
written set (a first-ever export of an empty/all-filtered library still writes the card) → "unknown". README
line "using ASR Model {}" → "ASR model(s): {}". `settings.asr_model_size` now appears only in a comment.

**FAIL-BEFORE.** New test `hf_readme_provenance_lists_stored_model_version_not_export_day_setting` writes a
gold clip stamped model_version_id="omniasr-ctc-300m@sha-test" and exports with default settings
(asr_model_size WSL7B); a TARGETED revert of model_str to the old `settings.asr_model_size` made the card
print "WSL7B" → the test failed, passes with the fix (card lists the stored id, NOT WSL7B).

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (9.77s) ·
`cargo test --lib` → `test result: ok. 1013 passed; 0 failed; 6 ignored` · `python run_python_policies.py` → 42 passed.

**Adversarially verified** (Workflow, 3 independent skeptics vs source): written-set == shipped rows (no
un-shipped model listed, no shipped model missed), no residual export-day leak + honest edges (empty→unknown,
deterministic BTreeSet, no panic), no consumer/test break (grep "using ASR Model" = 0 matches; SHA test
recomputes from bytes). ALL refuted=false / severity none.

**NOT verified / remaining:** no exe rebuild (HF card text changed — a live HF export would show the stored
model(s) line). Proven by unit test, not a live multi-model export.

**Tier-0 status:** P0.2/P0.3/P0.4 + the HF-README model sibling all shipped — every non-owner-gated Tier-0
provenance-honesty item is CLOSED (H1 is P0.1, owner-gated GPU re-score). **Next: Tier-3** — P3.2 body-scan
policy inventories (close T2) → P3.1 generated IPC contract (close T1) → P3.3 → P3.4. "Best / real #1" NOT claimed.

### Iteration 183 — 2026-07-24 — P3.2 cloud-STT-egress inventory (closes T2 cloud half) (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD 6e26954, lock held (iter17-P3.2). Read all three
candidate policies FIRST (the roadmap warns companion whole-surface audits exist) before deciding the gap.

**FINDING (understand-first).** The roadmap T2 names two floor/enumeration gates. Verified per class:
- Cloud-egress (test_cloud_privacy_policy.py): `test_cloud_stt_scribe_egress_requires_opt_in` counted
  `require_cloud_stt_consent(&state)?` sites (>= 2) — a TRUE floor: a 3rd un-gated Scribe egress command keeps
  the count and passes. GENUINE, TRACTABLE gap (egress has a clear sink).
- Main-thread (test_command_main_thread_policy.py + test_ui_thread_blocking_audit.py): BOTH are hand-verified
  RATCHETS/allow-lists — a new sync-heavy command isn't auto-caught either. BUT a robust automated inventory
  is genuinely hard (the audit itself documents that static scanning can't classify "heavy work", hence the
  hand lists). Deferred as a separate item — NOT converted here (avoid a fragile false-positive-prone gate).

**FIX P3.2 (cloud-egress inventory, committed 90c6ec1).** Replaced the count floor with a whole-surface
inventory keyed on real evidence: parse every fn body in the command surface (comment-stripped, brace-
matched) + flag #[tauri::command] names; find every fn calling a Scribe egress sink (`scribe_api::transcribe*`);
require consent enforced on EVERY path via `_consent_covered` (marker in body, OR — for a private helper —
every caller covered, recursing helper→caller, terminating False at an un-gated command). Fails loudly if
zero egress sites found (no vacuous pass). A companion test pins the scribe_api egress SURFACE (every pub
fn is a known pure helper or `transcribe*`; the ElevenLabs POST is private) so the prefix scan stays complete.

**FAIL-BEFORE.** Removing `add_scribe_votes`' `require_cloud_stt_consent(&state)?` (TARGETED edit) made the
inventory FAIL: "Scribe cloud egress reachable WITHOUT a consent gate on the path: ['add_scribe_votes']" —
the exact T2 scenario the old floor allowed; restored → passes clean.

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched; Rust unchanged,
policy-only):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (8.72s) ·
`cargo test --lib` → `test result: ok. 1013 passed; 0 failed; 6 ignored` · `python run_python_policies.py` → 42 passed.

**Adversarially verified** (Workflow, 3 independent skeptics vs source): a real un-gated egress cannot pass
(traced 8 adversarial shapes — all flagged), no missed sink/parse-gap (fail-closed on mis-parse), not
vacuous / not false-positive (clean run passes, recursion terminates). Core refuted=false. Two LOW
future-proofing gaps they surfaced were then FIXED + re-verified: (1) also strip `/* */` block comments (a
gate hidden in a block comment is now flagged — fail-before shown); (2) the surface-pin now matches
`pub(crate) fn` too (was `pub fn ` only), so a wider-visibility non-transcribe* upload helper can't evade it.

**NOT verified / remaining:** the main-thread inventory (the OTHER T2 half) is NOT done — a robust static
"heavy work" classifier is hard; the hand-verified ratchets remain the honest state (flagged for a future
item). String-literal marker decoys share the substring weakness of every assert_contains here (contrived,
review-visible, not the accidental-omission threat).

**Tier-3 status:** P3.2 cloud-egress inventory shipped (T2 cloud half closed). **Next: P3.1** generated IPC
contract (close T1) → P3.3 → P3.4 → (revisit main-thread inventory). "Best / real #1" NOT claimed.

### Iteration 184 — 2026-07-24 — P3.1 generated IPC contract (closes T1) (interactive loop)

Reality check pre-work: exe NOT running, git clean, HEAD 3964df4, lock held (iter18-P3.1). Traced the real
sources first: the sole `generate_handler!` (lib.rs:618, 127 commands) and the sole invoke wrapper
(src/lib/commands.ts, all string-literal invokes); confirmed no `#[tauri::command(rename)]` and no second
registry/invoke_handler before writing the gate.

**FIX P3.1 (closes T1, committed 53e571f).** Nothing diffed frontend `invoke('name')` against the Rust
registry, so a renamed/removed command stayed green in vitest+Playwright+cargo (vitest mocks invoke; the
Playwright tauri-mock returns null for unknown commands; cargo never sees the frontend) — the dangling call
fails only at runtime. New scripts/test_ipc_contract_policy.py (auto-discovered) builds the contract from BOTH
real sources — `registered_commands()` bracket-matches generate_handler! (comment-stripped, last `::` segment
= the Tauri command name) and `frontend_invocations()` scans src/**/*.ts + *.svelte for invoke literals — and
FAILS on any invoked name the registry does not export. `test_no_command_rename_attribute` pins the
fn-name==registered-name mapping. Registered-but-uninvoked (19, genuinely backend/reserved — spot-checked) +
dynamic invokes are INFO only; fails loudly if either set parses empty (never vacuous).

**FAIL-BEFORE.** Renaming a frontend invoke to `'get_segments_RENAMED'` (TARGETED edit — the exact T1
scenario) made the gate FAIL "…does NOT export: ['get_segments_RENAMED']"; restored → passes clean
(108 invoked / 127 registered, 0 dangling, 0 dynamic).

Gate (warm default target — app not running so target/release + %APPDATA% provably untouched; policy-only,
Rust + frontend byte-unchanged so npm not re-run):
`cargo fmt --check` → ok · `cargo clippy --all-targets -- -D warnings` → ok (42.76s) ·
`cargo test --lib` → `test result: ok. 1013 passed; 0 failed; 6 ignored` · `python run_python_policies.py` → 43 passed.

**Adversarially verified** (Workflow, 3 independent skeptics vs source): 2 refuted=false (registry+frontend
parse complete; the single registry is authoritative — slices re-export via `pub use`; plugin invokes
`plugin:…|…` structurally can't be flagged; no false-positive, clean run passes). The 3rd found a REAL gap
(CONFIRMED, low): the tight `[A-Za-z0-9_]+` literal regex left a quoted-but-odd arg — a whitespace-padded
name `invoke('get_segments ')` or a backtick literal `invoke(\`x\`)` — falling through BOTH the literal and
dynamic paths, so a dangling invoke in those forms shipped uncaught. **FIXED + verified end-to-end:** now
every `invoke(` is classified by its first-arg char and the WHOLE quoted string extracted (`'`/`"`/backtick,
skipping `:`/`|` plugin names + interpolated backticks) — a backtick dangling `invoke(\`x_GONE\`)` now FAILS,
`invoke('plugin:dialog|open')` is correctly skipped, clean run unchanged (108/127). Also hardened (their
latent note): the registry bracket-matcher strips comments BEFORE matching so a `[`/`]` in a block comment
can't corrupt depth.

**NOT verified / remaining:** the gate is STATIC — a genuinely-dynamic `invoke(runtimeName)` (0 today) is
reported, not checked (unavoidable without running the app); shape/argument mismatch (only NAMES are
diffed) is out of scope. No exe rebuild needed (policy-only).

**Tier-3 status:** P3.2 (T2 cloud half) + P3.1 (T1) shipped. **Next: P3.3** coverage/mutation → P3.4 e2e →
(revisit the deferred main-thread whole-surface inventory). "Best / real #1" NOT claimed.

### Iteration 185 — 2026-07-24 — Tier-3 remainder assessment + HONEST HAND-OFF (interactive loop; LOOP STOPPED)

Reality check pre-work: exe NOT running, git clean, HEAD d33a347, lock held (iter19-P3.3assess). No code
change this iteration — the mandatory scope is complete, so this is the honest tractability assessment of
the remaining Tier-3 items + the hand-off, per the loop's stop condition.

**REMAINING TIER-3 ITEMS — assessed NOT tractable as clean in-loop increments (verified, not assumed):**
- **P3.3 coverage ratchet + mutation testing** — `cargo-mutants` is NOT installed and is slow (runs the
  full suite once per mutant → hours), and the roadmap itself frames it as a "hypothesis to measure" (a
  measure-first item for owner review, not a fast per-commit gate). Frontend coverage: `test:coverage` is
  scripted but `@vitest/coverage-v8` is NOT installed and the config has NO thresholds — a real floor would
  need a NEW dev-dependency + a measured baseline + CI wiring, and a floor-below-current is a weak guard
  (allows regression to the floor). Against the owner's reliability-first / no-new-surface directive, a weak
  coverage floor is diminishing-returns; the valuable half (mutation score on db.rs/eval.rs) is the slow
  owner-measurement piece. → deferred as owner/measurement-gated.
- **P3.4 full import→review→export e2e in CI** — CI ALREADY runs the mock Playwright e2e (ci.yml:129,
  release.yml:71). The MISSING piece is `test:e2e:real` (e2e_real_app.cjs), which spawns the real .exe and
  REQUIRES `CORTEX_AUDIO` (real audio) + the release build + the multi-GB ONNX models on the runner
  (nightly-real-audio.yml already WARNS it is skipped without them). Wiring it needs a runner provisioned
  with models + audio fixtures — owner/infra-gated; not runnable or verifiable in this loop.
- **Main-thread whole-surface inventory** (deferred iter183) — a robust "fails on a NEW un-offloaded heavy
  sync command" is NOT statically expressible: test_ui_thread_blocking_audit.py itself documents that static
  scanning cannot classify "heavy work" (hence its hand-verified lists), and its migration worklist is
  already EMPTY (every known heavy command is async). A complete sync-command allow-list (~62 entries) would
  be exactly the enumeration the T2 critique rejects, and hand-classifying it is error-prone scope-creep.
  → left as the honest state (TRACER records real per-command timings for owner review; worklist empty).

**Per the loop's stop condition, this is the STOP point.** Final full verification (warm default target; exe
not running so target/release + %APPDATA% provably untouched):
`cargo test --lib` → `test result: ok. 1013 passed; 0 failed; 6 ignored` · `npm test` → `Tests 214 passed
(214)` (39 files) · `python scripts/run_python_policies.py` → 43 passed.

---

## HAND-OFF — state of the audit plan (docs/ROADMAP_TO_NUMBER_ONE.md), 2026-07-24

**DONE + gated + adversarially verified (every non-owner-gated Tier-0/Tier-1/Tier-2 item + the T1/T2
structural test-gaps):**
- Tier-0 honesty: **H2** diarization provenance guard (P0.2), **H4** DPAPI key at-rest (P0.3), **H3**
  per-segment processing provenance — denoised/diarized/vad_backend persisted at import (v41/v42) + the
  export manifest reads STORED truth via `processingProvenance` (P0.4 write+read+vad); plus the HF dataset-
  card ASR-model sibling (reads stored model_version_id, not the export-day setting). **H1** is annotated
  cross-basis; the re-score is owner-gated P0.1.
- Tier-1 reliability: UNC guards (P1.1), fatal-error dialog (P1.2), restore writer fence + reservation
  (P1.3/P1.3b), ASR + streaming denoiser/diarization load-retry breakers (P1.4/P1.4b).
- Tier-2 frontend: library-load-error surface (P2.1), unhandled-rejection trap (P2.2), whole-row-clobber
  class fully closed across App + ReviewMode (P2.3/P2.3b). **P2.4 i18n is OWNER-GATED** (native Sorani).
- Tier-3 structural gates: **T1** generated IPC contract — frontend invoke() diffed vs generate_handler!
  (P3.1); **T2** cloud-egress whole-surface consent inventory replacing the count floor (P3.2).

**REMAINING — OWNER-GATED (surfaced, never faked):**
- **P0.1 / P4.1** re-score all engines on ONE normalization basis + re-pin MEASUREMENTS.md (needs the GPU
  run on the owner's rig). Until then the cross-engine table stays annotated cross-basis / not-directly-
  comparable — the honest interim.
- **P2.4** translate the core CKB surfaces (RefineryPanel/ModelRegistry/Diagnostics/shortcuts) — needs a
  native Sorani reviewer; injecting best-guess Sorani would violate the honesty law + the parity gate.
- **P4.2** FLEURS train/test contamination check (H5); **P4.3** measure Scribe v2 / Chirp on the frozen set
  (consent-gated; gemini-2.5-pro / Scribe only); **P4.4** wire + A/B the built-but-unused chunk-overlap
  stitching, ship only on measured non-regression.

**REMAINING — Tier-3 infra deliberately NOT automated in-loop (with reasons, above):** P3.3 mutation/coverage
(slow / install-gated / measure-first + a new dep), P3.4 real-audio e2e in CI (needs a runner with models +
audio), the main-thread heavy-sync inventory (not robustly statically expressible).

**HONEST BAR:** the app is at its **best verifiable state** — every honesty, reliability, and correctness
defect the audit found that can be fixed + verified WITHOUT owner hardware or native-Sorani review is closed,
gated, and adversarially verified. This is **NOT "#1"**: the SOTA-results claims (P0.1/P4.*) and the native
Sorani UI (P2.4) remain owner-gated and unmeasured/unverified here — so the "number 1 reliable and
professional" bar is not independently earned while those stand. **LOOP STOPPED** (ScheduleWakeup stop).

### Iteration 186 — 2026-07-25 — SHIP PHASE A EXECUTED: build → backup → migrate → live smoke (all real runs)

Per docs/SHIP_FINAL_PHASE.md, driven interactively. Every result below is from a real run on this rig.

**A1 build:** `npm run build` (2.6s) + `cargo build --release` (6m39s) →
`EXE FRESHNESS GATE: OK (exe at HEAD 22cdc7b, newer than all sources)` — the shipped exe was 334 commits
stale (baked 6f8fe3c, Jul 16); it now IS HEAD.

**A2 backup (before any schema change):** cold copy of the whole data dir (app closed; 555 MB incl.
db+wal+shm, media-cache; old nested backups excluded) to Desktop/cortex-speech-backups/pre-v42-20260725-001658;
DB byte-size verified identical.

**A4-first on a DISPOSABLE profile (the driver refuses the real one by design):** `node e2e_real_app.cjs`
with a real Kurdish clip (Nawras - KU.wav), CTC300M → **REAL-DATA RUN OK: 3 segments; first transcript
203 chars** of real Sorani; run.jsonl written. Fresh profile DB: schema **v42**; all 3 segments carry
stored provenance `vad_backend='silero'`, `denoised=0`, `diarized=0` (honest false — not enabled),
`model_version_id=omniasr-ctc-300m`. The session's provenance chain verified LIVE end-to-end. (This also
discharges the B1 e2e:real gate: exit 0.)

**A3 real-library migration (only after the exe was proven):** launched the real exe; migrations v38→v42
applied to the production DB (was v37, 144 segments); closed gracefully via CloseMainWindow. Read-only
verify: schema **42**; **144 segments intact**; speech_segments IS STRICT; all FK children survived the
v40 FK-off recreate (segment_hypotheses 432, decision_verdicts 144, corrections 9, correction_memory 25);
provenance columns present with all 144 legacy rows honestly NULL (= not recorded); spot transcripts
non-empty; `PRAGMA integrity_check` = ok.

**NOT yet done (Phase B remainder):** live export-bundle manifest + HF README checks on the real library;
the full user loop (7B refine via WSL, review, verify, export) driven like a user; `make verify-10`.

### Iteration 187 — 2026-07-25 — SHIP PHASE B: verify-10 driven to 21 PASS / 0 FAIL (real defect found + fixed)

**verify-10 (the charter's done-gate) exposed a REAL production defect.** First run: RED on
ignored-real-model. The lib-target preflight failure (7B server cold) MASKED two deeper failures that
surfaced once the champion server was up: `pipeline_routes_to_finetuned_when_enabled` +
`import_routes_to_finetuned_when_enabled` panicked E_ASR_7B_UNAVAILABLE — the fine-tuned engine was
unreachable. Root cause (the recurring all-or-nothing class, one level up from the round-26 per-file fix):
`select_bundled_models_dir` keys the ONE bundled root on CTC presence, so the partial exe-adjacent copy
(target/release/models: CTC+Silero only) won the root and ORPHANED every repo-only sibling. In PRODUCTION
(user dir has only the aligner) this orphaned the fine-tuned MMS engine, CAM++ diarization, the denoiser,
and CTC-1B. **Fixed at the shared resolution layer** (a7cb10b): `bundled_dir_containing` searches the
candidates PER FILE (selected-dir behavior unchanged when the file is present there);
`model_root_candidates` + finetuned_model_paths require model.onnx+vocab.json in the SAME root.
FAIL-BEFORE: the two real_audio tests (real FLEURS fixture + real fine-tuned ONNX) red before, green after.

**7B champion server:** cold at first (the RED). start_7b_server.ps1's nohup-detach dies under a headless
runner (its own NOTE documents this); launched via the documented headless pattern (harness holds
`wsl -- bash -lc "exec python cortex_7b_server.py"`) → both GPUs serving ckb_Arab on 8799 (17.4 GB VRAM
each); `wsl_7b_preflight_passes_when_server_up` ok. NOTE: this instance dies with the session — for daily
use run start_7b_server.ps1 from an interactive console.

**Final verify-10 (exe rebuilt at HEAD a7cb10b, freshness OK, CORTEX_AUDIO set):**
`kept gates run: 23 - 21 PASS, 0 FAIL, 2 skipped` — incl. test-rust 394s, ignored-real-model 27.7s,
real-app-e2e 23.9s (real exe + real Kurdish audio + real transcript), rtf-bench, egress-runtime, fairness.
`VERDICT: INCOMPLETE - 2 kept gate(s) could not run (fuzz-smoke, refinery-lift). Green cannot be claimed.`

**Honest residue (why not GREEN):** fuzz-smoke — cargo-fuzz cannot link on windows-msvc (ASAN CRT vs
static-MT sherpa; measured 2026-07-11, documented in the probe) → a Linux-CI leg, structurally never
runnable on this rig. refinery-lift — the fixed-seed injected-error synthetic benchmark is NOT BUILT; the
last buildable charter gate. Plus the 5 OWNER-GATED-PENDING items and 8 owner-descoped distribution legs.

**Ship-phase state:** Phase A complete (iter 186) + Phase B verification complete: 0 failing gates, every
gate runnable on this rig runs and passes, the exe at HEAD, the champion engine proven live, the real
user-loop e2e green. Remaining to claim GREEN: build refinery-lift; run fuzz-smoke in Linux CI; owner-gated
items. Remaining human step per docs/SHIP_FINAL_PHASE.md: the owner's real review/export cycle + a quiet
week of daily use.

---

### Iteration 188 — 2026-07-25 — refinery-lift BUILT (last buildable charter gate) + real IRT defect found & fixed → verify-10 at 22 PASS / 0 FAIL

**What was done (commits f3b7c55, 7124724):**
1. **Built the refinery-lift charter gate** (`src-tauri/tests/refinery_lift.rs` + `scripts/verify_10.py`
   stub → real cmd gate): fixed-seed injected-error benchmark on the REAL shipped T0 jury path
   (`jury::run_t0_gate`, ActConfirm — real IRT consensus, real per-SNR-bucket conformal calibration,
   real verdict writes; nothing reimplemented). 5,000 i.i.d. calibration + 400 test segments (above the
   `min_calibration_n` ≈2,334 the shipped 5%-target math requires); references built only from words of
   the committed human-verified FLEURS ckb fixture; 3 voters at measured-ordering error rates; 10% hard
   + 5% vhard so BOTH sides of the conformal frontier are exercised. Verbatim result:
   ```
   REFINERY-LIFT BENCH (seed 0xc0de5eed, 5000 calibration / 400 test)
     raw_micro_cer      = 0.05167
     jury_micro_cer     = 0.02082
     cer_lift (abs)     = 0.03085 [95% CI 0.02765, 0.03386]
     relative_reduction = 59.7% (gate: >=30%)
     escalation         = 22/400 = 5.5% (gate: <=15%)
     [info, not gated] strongest-voter (wsl-7b) micro CER = 0.02141; jury vs strongest lift = +0.00059
   ```
   Gated baseline is raw CTC per the charter's literal definition; the shipped-default raw producer
   (WSL-7B champion) is surfaced via the not-gated transparency line.
2. **The benchmark immediately found a REAL production defect** (first run: 100% escalation, all
   consensus empty): the IRT EM M-step applied the RAW-SUM gradient, which scales with corpus size —
   past the ≥10-segment activation, the first lr×O(n_obs) update slammed every model ability into the
   ±3 clamp (abilities read exactly −3.0; consensus all-deletion; conformal could not calibrate). The
   documented "abilities cannot drift unboundedly" contract was void at every realistic corpus size.
   **Fix (f3b7c55):** mean gradient per entity → strict contraction, fixed point |ability−prior|<1;
   plus warm-start sanitation (persisted ability >1.0 from its heuristic prior = provable raw-sum-era
   clamp poison, discarded at seeding). FAIL-BEFORE demonstrated: with raw-sum temporarily restored,
   `em_ability_update_is_scale_invariant_and_anchored` fails ("wsl-7b ability 0.4409… drifted
   unboundedly from its prior 1.5"); green after.

**Adversarial verification:** 3 independent skeptics (irt-math / bench-rig / gate-wiring), every finding
hand-verified. Both CONFIRMED findings fixed pre-commit: (a) header falsely claimed 300m produces the
shipped raw_transcript (default is WSL-7B) → corrected + transparency line added; (b) the new #[ignore]
tests leaked into ignored-real-model (double-run + stale "37" count) → `--skip refinery_lift` + charter
string fixed. PLAUSIBLE persisted-ability-poison closed via seeding sanitation + regression test.
Accepted residuals stated honestly: the cmd gate is exit-code-only (a renamed filter would vacuously
pass — same idiom as rtf-bench); independent synthetic corruption cannot model correlated 300M/1B kin
errors (the benchmark gates the machinery, not real-audio accuracy).

**Gates (verbatim, this rig):** `cargo test --lib` → 1015 passed, 0 failed; full `cargo test` → exit 0
(31 targets, 0 failed); fmt/clippy clean; python policies 43/43. Exe rebuilt at HEAD 7124724
("EXE FRESHNESS GATE: OK"). Full `verify_10.py` sweep (7B server live, CORTEX_AUDIO=committed FLEURS
fixture):
```
 kept gates run: 23 - 22 PASS, 0 FAIL, 1 skipped (env/not-built)
 owner-descoped: 8   owner-gated pending: 5
 VERDICT: INCOMPLETE - 1 kept gate(s) could not run (fuzz-smoke). Green cannot be claimed.
```
(refinery-lift PASS 36.9s in the aggregate.) One flake noted honestly: test-e2e+a11y failed once inside
the first sweep (conformal-cert visibility timing) and passed 47/47 standalone and in the final sweep —
no code change involved.

**What is NOT verified / remaining:** fuzz-smoke is the ONLY kept-gate residue (measured windows-msvc
ASAN/static-MT linker impossibility — Linux-CI leg). 5 owner-gated items + 8 owner-descoped distribution
legs unchanged. The refinery-lift number is a synthetic machinery benchmark, NOT a real-audio accuracy
claim (the in-product lift gate remains owner-gated on the Gold Marathon).

---

### Iteration 189 — 2026-07-26 — tech-debt pass: mutation + coverage + fuzz gates built; the mutation gate immediately found 11 live mutants in yesterday's fix

Audit-driven pass (11 scored items). Three commits: `df206bc` (rigor infrastructure), `408f9ed`
(e2e flake + dependency security), `383c996` (EM golden tests). No production behavior changed
except the two documented `#[allow(dead_code)]` sites.

**1. Mutation testing BUILT — and it earned its keep on the first run.** The charter has always
required "0 surviving mutants in irt/conformal/ood/diff/normalizer"; no tooling existed.
`src-tauri/mutants.toml` scopes it to those 5 modules ("ood" is this repo's
`quality/signal_anomaly.rs` — no module by that name exists) and the nightly runs `--in-diff`
exactly as the charter specifies. Run against yesterday's M-step fix, verbatim:
```
26 mutants tested in 2h: 11 missed, 14 caught, 1 unviable
```
Every survivor swapped an arithmetic operator in the gradient accumulation, the ability update,
or the difficulty update. Two root causes: the **segment-difficulty update was asserted by
nothing at all** (all 4 of its mutants survived automatically, including `/`→`*` on the
mean-gradient divisor — the exact shape of the bug fixed yesterday), and the ability update was
covered only by the loose bound `|ability − prior| < 1.0`, which the prior-anchoring regularizer
keeps true under most operator swaps. **This is the same blind spot that let the raw-sum bug ship
green through 1,015 tests: the suite asserted shapes, never values.**

Fix: `em_golden_values` pins a deterministic 12-segment / 3-voter fit with EXACT abilities,
difficulties, consensus text and confidences to 1e-12, plus a monotonicity assertion stating the
property the constants only fingerprint. Every constant came from running the fit.

FAIL-BEFORE, **all 11 survivors** re-applied by hand to the real source one at a time and
reverted (full-statement anchors, compile errors distinguished from test failures so an
uncompilable mutant can never be miscounted as caught):
```
11 killed, 0 unviable, 0 STILL SURVIVING (of 11)
```
e.g. `325:48 / → *` → "ability wsl-7b: expected 1.512460322683539, got 1.501093613784506";
`204:70 - → +` → "an in-window persisted ability must warm-start at exactly 1.4, not fall back
to 0.5". Note the discrimination this buys: several survivors now die on differences in the
FOURTH decimal place (`317:59` gives 1.520321958053807 vs 1.512460322683539) — precisely the
size of error a bounds-based assertion can never see. A second `cargo mutants --in-diff` replay
was attempted first but refused: the saved diff no longer matches the tree now that the golden
test shifted line numbers ("Diff content doesn't match source file"), so the per-mutant hand
replay above is the evidence.

**2. Coverage measured for the first time** (cargo-llvm-cov). Two runs merged — the normal suite
plus the `--ignored` real-model suite with the 7B server warm (both exit 0):
`TOTAL 75.06%` lines; irt 97.57%, conformal 98.91%, signal_anomaly 100%, normalizer 92.45%,
normalizer/g2p 93.99%, diff/mod 90.91%, diff/phonetic 93.98%, chunking 95.35%, wer 81.88%,
**audio.rs 78.85%**. The charter's ">80% on normalizer/diff/audio parsers" is MET for normalizer
and diff, **NOT met for audio.rs**.

CORRECTION to my first reading of this: I assumed audio.rs's gap was a measurement artifact —
decode/VAD paths covered by the `#[ignore]` suite but excluded from a `--lib` run. Measured with
that suite merged in, **audio.rs did not move one line (78.85% both times)**. So the gap is not
an artifact: `SileroVad::new`/`detect`, `voice_activity_detection`'s model branch, and the
non-WAV decode branches of `decode_to_pcm`/`decode_pcm_windows` are exercised by NO test at all.
That is a stronger and more actionable finding than the artifact story, and it is the honest one.
Closing it needs either in-process VAD tests against the bundled Silero model or committed
MP3/M4A/FLAC fixtures — neither is a quick win. NOT papered over: no coverage gate was added
while a named module sits under its floor, and no padding tests were written to nudge the number
past 80% without exercising those paths. Tracked follow-up.

**3. Fuzz campaign wired** (nightly, 5 targets x 15 min, accumulating cached corpus). This is the
only place the leg can run — windows-msvc cannot link cargo-fuzz (measured 2026-07-11). verify-10
keeps honestly reporting SKIP-ENV locally; CI is the evidence, not a loosened gate.

**4. e2e a11y flake root-caused.** `playwright.config.ts` had `workers: undefined`, which scales
to ~half the CPU count — ~32 Chromium instances for 47 tests on this 64-core box, making every
visibility wait race the dev server. Capped to 4. Measured: 12/12 idle before; after the cap,
11/12 then 12/12 under 24 concurrent CPU burners (**23/24**). Residual starvation sensitivity
addressed by an explicit 30s timeout on the two async-panel settle-waits — the axe zero-violation
assertions are untouched.

**5. All 5 high-severity npm advisories closed** (one root cause: brace-expansion DoS reachable
only via eslint's dep chain). eslint 10 + @eslint/js 10 + eslint-config-prettier 10 + globals 17
+ eslint-plugin-playwright 2; the repo was already on flat config so the major was clean.
`npm audit` 5 high → **0**. `npm audit --omit=dev` was 0 before and after (that gate was never red).

**6. Ledger archived** 10,705 → 1,142 lines (history verbatim in `docs/ledger/`, accounting
reconciled exactly). **Toolchain pinned** (`rust-toolchain.toml` 1.95.0 — resolves to the version
already in use, so a no-op today and a guarantee tomorrow) and the nightly unified on the same
toolchain action as ci/release. **`docs/STATUS.md` generated** by `verify_10.py --status-md`;
handoff docs now link to it instead of restating gate state, the claim that kept rotting.

**Honest corrections to my own audit.** Two of eleven items were largely invalid and were NOT
forced through: retiring 4 "superseded" docs (3 must stay — `ROADMAP_TO_10.md` and
`RESEARCH_SOTA_2026.md` are named by the required-files gate and the charter still cites the
former as the live north star; only the orphaned wave0 handoff was archived), and the
`#[allow(dead_code)]` cleanup (all 6 are deliberate; `flock.rs`'s is an RAII handle whose removal
would silently disable single-instance locking — now documented so nobody "cleans" it).

**Gates (real runs):** `cargo test --lib` 1016 passed / 0 failed; clippy `-D warnings` clean;
fmt clean; python-policies 43/43 (test_workflow_policy caught real non-ASCII in my YAML — fixed
by complying); vitest 214/214; e2e 47/47; typecheck 426 files 0 errors. Full sweep with the 7B
server warm and the exe rebuilt at HEAD:
```
 kept gates run: 23 - 22 PASS, 0 FAIL, 1 skipped (env/not-built)
 VERDICT: INCOMPLETE - 1 kept gate(s) could not run (fuzz-smoke). Green cannot be claimed.
```
**Unchanged from before this pass — nothing regressed, and fuzz-smoke remains the single
kept-gate residue.** What is genuinely better: the two gates the charter demanded now exist and
have already paid for themselves, one real test-quality defect is fixed, all dependency
advisories are closed, and a load-sensitive gate is measurably steadier.

**NOT verified / still open:** `audio.rs` at 78.85% is below the charter's 80% floor, and the gap
is genuinely untested code (SileroVad, the VAD model branch, non-WAV decode branches) — not a
measurement artifact; fuzz + mutation nightly jobs are authored but have **never executed on a
real runner**, so the first scheduled run is their real proof, not this entry; the full
844-mutant sweep was abandoned at ~8h projected, so only the 26-mutant `--in-diff` slice has a
measured result (all 11 of its survivors are now closed, but the other ~818 mutants across the
5 core modules have never been run); 5 owner-gated legs and 8 owner-descoped distribution legs
unchanged.

---

### Iteration 190 — 2026-07-26 — **verify-10 GREEN: 23/23, zero skips** — fuzz-smoke now genuinely runs (via WSL) and found 2 stale harness defects on its first-ever execution

```
 kept gates run: 23 - 23 PASS, 0 FAIL, 0 skipped (env/not-built)
 owner-descoped: 8   owner-gated pending: 5
 VERDICT: GREEN - PERSONAL-USE SHIP-READY. (Not full-charter 10/10: 8 legs owner-descoped,
 5 owner-gated pending.)
```
Exit 0. **First verdict in this project's history with nothing skipped.**

**How the last skip closed.** windows-msvc still cannot link cargo-fuzz — the 2026-07-11
measurement stands (ASAN dynamic-CRT multiply-defines std:: against the static-MT sherpa prebuilt,
LNK2005; `--sanitizer none` strips libFuzzer's sancov symbols, LNK2001). But WSL on the same
machine is a real Linux toolchain where ASAN + `-fPIC` static libs link fine. Provisioned nightly
`rustc 1.99.0` + `cargo-fuzz 0.13.2` there (plus libdbus-1-dev/libssl-dev/pkg-config and the
Tauri Linux set), and `_probe_fuzz` now detects a capable WSL and lets the gate proceed while
`_fn_fuzz_smoke` runs the targets through it. **Nothing was loosened — the gate does strictly
more work than before.** Measured, 30s per target: cache 136,970 execs / diff 97,153 /
features 151,266 / normalizer 3,301 / validation 2,311,088, 0 crashes; in-gate PASS 273.9s.

**It found 2 defects immediately — both in the harnesses, which had never been executed anywhere.**
1. `validation.rs` asserted `s.len() <= max_len` (BYTES) while `validate_text` deliberately counts
   CHARACTERS — its own comment records that a byte check "rejected valid Kurdish text at roughly
   half the advertised budget" (Sorani ≈2 bytes/char). The harness encoded the very contract
   production had fixed, so it fired on essentially any Kurdish string: it would have reported
   **correct** behaviour as a crash.
2. `features.rs` asserted finite output for arbitrary input. The crash decoded to
   `sample_rate = 128` Hz with a sample of `9.44e21`; squaring that in the power spectrum
   overflows f32 (max 3.4e38) to inf. Unreachable in production — PCM arrives finite, in [-1,1]
   (i16/32768), resampled to 16 kHz — and `FbankExtractor` has **zero production callers** today
   (every call site is a test at 16000). Constrained to that real domain.

Honest framing: **harness bugs, not live production bugs.** The finding that matters is that
targets which had never run accumulated stale assertions — which is exactly why "authored but
never executed" is not the same as "green", and why iteration 189 listed the nightly jobs as
unproven.

**And a vacuous pass I wrote myself, caught live.** My first WSL smoke script used
`bash script.sh`, which does not source the login profile; cargo fell off PATH,
`cargo fuzz list` returned nothing, both loops iterated zero times, and it printed
"all targets clean, 0 crashes" with exit 0. `_fn_fuzz_smoke` now fails LOUD on an empty target
list. The guard is written from having hit it, not from theory — and it is the precise failure
class the charter forbids.

**What GREEN does and does not mean.** `GREEN - PERSONAL-USE SHIP-READY` means every kept gate
runs and passes on this rig. It is NOT the literal `CORTEX 10/10: ALL GATES GREEN`, which
requires nothing descoped and nothing owner-gated: 8 distribution legs remain descoped by the
owner's 2026-07-10 amendment (reversing that costs money + external lead time) and 5 legs remain
owner-gated (2 of which need people other than the owner). See docs/SHIP_FINAL_PHASE.md
"What remains".

**Still open, unchanged:** `audio.rs` 78.85% under the 80% coverage floor (genuinely untested
code, not an artifact); the nightly fuzz/mutation CI jobs have still never executed on a real
runner (this run was local via WSL, which is different evidence); ~818 of 844 core-module mutants
never run; the owner-gated and owner-descoped legs. And the one no gate can measure: a quiet week
of real daily use.

---

### Iteration 191 — 2026-07-27 — first full mutation sweep of the core modules, then the cleanup: **200 survivors → 81** (25% → 10.2%)

Triggered by the owner asking whether the code work was genuinely finished. It was not. GREEN means
every *gate* passes; three charter requirements have **no gate at all**, and checking them found
real defects.

**THE SWEEP.** First full `cargo mutants` run over the 5 charter core modules — 841 mutants,
61 minutes:
```
598 caught, 200 missed, 10 unviable, 33 timeouts     (25% survival)
```
After the cleanup below, re-run at `189a0f9` — 841 mutants, 49 minutes:
```
716 caught,  81 missed, 10 unviable, 34 timeouts     (10.2% survival)
```
**A 60% reduction in surviving mutants**, and the re-run predates the final normalizer commit
(`ee5e640`) which killed 5 more. Survivors by file now: phonetic 21, diff/mod 16, irt 14, g2p 10,
normalizer 7, conformal 7, signal_anomaly 6.

**THREE DEFECTS FOUND BEFORE THE SWEEP EVEN RAN:**
1. **The mutants config I wrote the day before was never read.** cargo-mutants reads
   `.cargo/mutants.toml`; mine was at `mutants.toml`. `cargo mutants --list` returned **9,285**
   mutants (the whole crate) instead of 840. The nightly job would have mutated everything and
   timed out — a gate that looked configured and was inert.
2. **The unwrap budget was unenforced in the file with the most unwraps.** `normalizer.rs` carried
   a file-level `#![allow(clippy::unwrap_used)]`, voiding the charter's "any new production unwrap
   fails CI"; the count had already drifted to **14 against a stated budget of 12**. Fixed at the
   root, not by editing the budget: **12 → 1**, routed through one reviewed `static_regex()` helper,
   blanket allow removed, clippy enforcing again.
3. **The LCS bail-out had no test.** `compute_diff`'s >10,000-word O(n·m) guard: both `||`→`&&`
   and `>`→`==` survived the whole suite. Either silently disables the protection when only ONE
   side is huge — the runaway-ASR case it exists for.

**AND A VACUOUS PASS I WROTE MYSELF**, caught live: my first WSL fuzz script used
`bash script.sh`, which does not source the login profile; cargo fell off PATH, `cargo fuzz list`
returned nothing, both loops iterated zero times, and it printed "all targets clean, 0 crashes"
with exit 0. `_fn_fuzz_smoke` now fails LOUD on an empty target list.

**THE PATTERN, everywhere:** tests asserted *ranges and orderings* rather than *values and edges*.
Concretely — `nonconformity`'s `*`→`/` lived because the one test used `ctc = -1.0`, where
`0.1 * 1.0 == 0.1 / 1.0`. Every `snr_bucket` edge survived `<`→`<=` because the tests sampled
2/10/20/30 dB, never *on* a boundary (a clip at exactly 5.0 dB would calibrate against the wrong
acoustic condition and silently void its coverage guarantee). `signal_anomaly`'s entire arithmetic
sat behind `score < 0.5`. 18 of g2p's survivors were "delete match arm" — a dropped Kurdish letter
falls through to the catch-all and quietly shifts every downstream phonetic distance forever.
9 of normalizer's were the irregular Kurdish TEENS, where a deleted arm makes the number vanish
from shipped training text.

**Modules pinned this iteration** (commits `2074f02`, `62a8ff8`, `df523ed`, `188e3eb`, `510ff45`,
`12fec8d`, `189a0f9`, `ee5e640`), each with a hand fail-before against real source:
conformal 6/8 · signal_anomaly 8/8 · diff/mod 3/7 · phonetic 6/7 · g2p 6/6 · irt 2/2 ·
normalizer 6/6 then 5/5.

**EQUIVALENT MUTANTS, proven not waved away.** "0 surviving mutants" is not literally reachable —
every mutation project has mutants that alter no behaviour. Demonstrated individually:
`i -= 1` → `i /= 1` in the LCS backtrack (the `dp[i-1][j] > dp[i][j-1]` branch decrements `i`
anyway, output byte-identical); both `min_calibration_n` convergence tweaks (tests assert the exact
outputs 2334/206 and PASS under the mutants); `any_non_empty`'s `!` (both formulations evaluate
true on any mixed slot); `total_weight > 0.0` → `>=` (exp2 weights are strictly positive, branch
unreachable); the leading-zero `len() > 1` → `>= 1` (`starts_with('0')` already excludes every
other token). **The achievable bar is 0 UNREVIEWED survivors, not 0 survivors.**

**KNOWINGLY LEFT ALIVE, with the reasoning recorded in the tests** rather than as silent misses:
the >1000-word phonetic fallback and the 12.5M-cell LCS memory guard both need million-cell inputs
to discriminate — measured at ~27s, in a suite that runs in every CI job and once per mutant. And
checked rather than assumed: the phonetic and plain differs were compared directly on reordering,
insertion, deletion and near-homophone inputs and produced IDENTICAL op sequences every time.

**A reconciliation worth recording.** After the first cleanup, `normalizer.rs:411/413` still showed
ALIVE while a hand fail-before said they died. Rather than trust either, I re-checked: the line
numbers had shifted under my own earlier edits, so 411 is the **billions** divisor, not the
hundreds divisor I had been mutating. My table stopped at 2000, so every path above a thousand was
genuinely untested. Fixed (`ee5e640`) — the contradiction was real information, not noise.

**Gates:** `cargo test --lib` **1036 passed / 0 failed** (was 1015 at iteration 190); clippy
`--all-targets -D warnings` clean; fmt clean; python-policies 43/43.

**Still open:** `audio.rs` 78.85% under the 80% coverage floor (genuinely untested code —
SileroVad, the VAD model branch, non-WAV decode); the nightly fuzz/mutation CI jobs have still
never executed on a real runner; 81 survivors remain, now concentrated in the expensive/equivalent
tail rather than in unpinned arithmetic; the owner-gated and owner-descoped legs unchanged.

---

### Iteration 192 — 2026-07-27 — audio.rs clears the 80% coverage floor (77.17% → 86.36%); every charter-named module now above it

**The charter's ">80% line coverage on normalizer/diff/audio parsers" is now MET on all of them**
(`cargo llvm-cov --lib`):
```
audio.rs        86.36%     diff/mod.rs      98.09%     normalizer.rs     94.46%
audio_quality   97.59%     diff/phonetic    99.59%     normalizer/g2p    98.66%
chunking.rs     92.32%     conformal.rs     99.07%     irt.rs            97.47%
wer.rs          80.62%     signal_anomaly  100.00%     calibration      100.00%
TOTAL 73.94%
```

**BOTH numbers are reported for audio.rs, because they differ and the difference matters:**
```
tool-reported (cargo llvm-cov)  77.17% → 86.36%
production-only                 78.32% → 83.36%   (uncovered 258 → 188)
```
Adding tests to a file raises its coverage **mechanically** — `#[cfg(test)]` code counts toward the
total. After the first batch the tool said 80.45% while only **13** production lines had actually
been covered; production-only was still 78.32%, *under the floor*. Reporting 80.45% there would
have been technically true and substantively false, so the work continued until both cleared 80%.

**What was uncovered:** 103 of the 138 never-executed functions were `check_audio_file`,
`decode_to_pcm`, `get_duration_ms` and `decode_pcm_windows` — the audio PARSERS the charter names —
reachable only through the running app or the model-gated integration suite. Ordinary parsing code
with no unit tests. Now covered against real containers decoded by real symphonia (nothing mocked):
metadata for mono-16k and stereo-44.1k, missing/empty/garbage-byte errors, downmix+resample to
mono 16 kHz, the content-hash cache, windowed streaming with strictly-increasing offsets and no
dropped/duplicated samples, a failing callback aborting the decode, the timeout wrappers on both
outcomes (asserting a timeout is classified TRANSIENT so a slow disk retries instead of hard
failing), `ensure_pcm_16khz` in all four modes, `normalize_pcm_rms` (no inversion/clip/NaN, silence
left alone), `voice_activity_detection` over the real entry point, and `SileroVad::new` against the
bundled model **including the non-16 kHz path** where Silero resamples internally and maps segments
back onto the caller's indices — an 8 kHz phone recording is ordinary, and a wrong mapping there
silently misplaces every chunk boundary in the file.

**TWO OF MY ASSUMPTIONS WERE WRONG AND THE CODE WAS RIGHT** — recorded in the tests, not quietly
deleted:
1. I asserted silence yields no VAD regions. It returns the **whole buffer** as one region:
   `probs_to_segments` deliberately falls back to `(0, total)` so a file is never silently dropped.
2. I then asserted that result must not be labelled `Silero`. It must be — `VadBackend::Silero`
   documents *"Silero ran successfully"*, not *"Silero positively detected speech"*, and
   `VadBackend::None` means no VAD ran at all, which is not this case. **Worth knowing when reading
   `vad_backend` in an export: a silero-labelled segment does NOT imply positive detection.**

The VAD test deliberately does **not** skip when the model is absent — it asserts the invariants
that hold on either backend and names the one that ran, so it always exercises a complete real path
and can never pass vacuously by exercising neither.

**Gates:** `cargo test --lib` **1046 passed / 0 failed** (1015 at iter 190, 1036 at iter 191);
clippy `--all-targets -D warnings` clean; fmt clean; python-policies 43/43.

**WHAT REMAINS — the honest list.**
- **81 surviving mutants** (of 841), down from 200. Now concentrated in the *expensive* and
  *equivalent* tail, not in unpinned arithmetic. Five are individually proven equivalent; two are
  knowingly-skipped million-cell performance guards with the reasoning recorded in the tests.
  The rest are unreviewed and are the real remaining triage.
- **The nightly fuzz and mutation CI jobs have still never executed on a real runner.** They are
  authored and locally proven; their first scheduled run is the actual evidence.
- **`wer.rs` at 80.62%** is the thinnest margin of the charter-named set.
- **TOTAL coverage 73.94%** — dragged by `pipeline.rs` (~24%), `models.rs`, `wav2vec2_asr.rs`,
  all of which need real models/hardware. No charter requirement covers the total; stated for honesty.
- **5 owner-gated legs** (2 needing people other than the owner: independent annotators, CORDI
  agreement) and **8 owner-descoped distribution legs** — unchanged, and only the owner can move them.
- **The quiet week of real daily use.** No gate can substitute for it.

---

### Iteration 193 — 2026-07-27 — final pre-use pass: mutation tail triaged (200 → ~72), a broken CI job caught before it ever ran, wer.rs closed

Everything within my control before the owner starts daily use. Three real findings.

**1. THE NIGHTLY MUTATION JOB WOULD HAVE FAILED ON ITS FIRST RUN.** Found by simulating the job
locally instead of waiting for 03:00 to reveal it. The step runs with `working-directory:
cortex-speech-app/src-tauri`, but `git diff` emits REPO-ROOT-relative paths regardless of cwd:
```
+++ b/cortex-speech-app/src-tauri/src/quality/irt.rs   (what it produced)
+++ b/src/quality/irt.rs                               (what cargo-mutants matches)
```
Every hunk would have been rejected with "Diff content doesn't match source file" — the exact
error I hit by hand earlier in this session and then reproduced in the workflow without noticing.
Fixed with `--relative` and verified end to end: the corrected diff over a real commit range now
yields real production mutants instead of a path error. **This is what "authored but never
executed" costs** — the fuzz job had been proven locally via WSL; this one had not, and it was
broken.

**2. MUTATION TAIL TRIAGED.** Full sweep at the start of this iteration: `841 mutants in 54m:
721 caught, 77 missed, 9 unviable, 34 timeouts`. That run predates the last three commits, which
killed 5 more (185:32 tie-break, 170:55 recurrence, 158/159 counters, 164 total), so the current
figure is **~72 of 841 — down from 200 at iteration 191**. Newly closed here, each with a hand
fail-before:
  - **LCS backtrack tie-break** (`>` → `>=`): when `dp[i-1][j] == dp[i][j-1]` there are two
    equal-length LCSs and the comparison decides which alignment the reviewer sees — flipping it
    makes "a b" vs "b a" keep "a" instead of "b" and every op changes. No length- or
    similarity-based assertion can see that. Three reordering cases pin it.
  - **Stats counters** (`+=` → `*=` on added/removed, `+` → `*` on the total): the existing case
    left added and removed at ZERO, where the two operators are identical. Added an alignment with
    every counter non-zero at once, plus a deletion-heavy case.
  - **Phonetic DP first row/column**: a large asymmetric word pair, because if either init loop
    stops one short a deletion-only path scores from an uninitialised 0 and the distance collapses.

**3. wer.rs — the 80.62% was NOT a production gap.** 61 of its 62 uncovered lines are inside the
`#[ignore]`d, opt-in `emit_crossval_vectors` tool (test code that only runs under
`CORTEX_EMIT_CROSSVAL`). The genuinely uncovered PRODUCTION lines were two: `rate()` when
`ref_len == 0` and levenshtein's zero-length early returns — the divide-by-zero and
empty-sequence guards every CER/WER number funnels through. An empty gold reference is what a
blank or not-yet-transcribed row looks like, so a wrong answer there silently poisons an averaged
CER. Now covered; file at 82.27%. **Padding the rest would have been theatre and was not done.**

**Gates:** `cargo test --lib` **1047 passed / 0 failed**; clippy `--all-targets -D warnings`
clean; fmt clean; python-policies 43/43.

**WHAT I COULD NOT DO, and why — the honest list:**
- **The nightly CI jobs still have not run on a runner.** `gh` CLI is not installed and the GitHub
  connector needs interactive auth, so I cannot trigger a workflow. The cron is `0 3 * * *`, so
  the *next* nightly will run them — and thanks to finding #1 above it now has a chance of
  passing. **Until that run is green, "the CI gates work" remains unproven.**
- **~72 surviving mutants.** The remainder is a genuinely hard tail: documented-unreachable
  backstops (the `compute_diff` "only raw remains" branch is commented *"unreachable for a
  well-formed LCS but a safe backstop"* — mutants there are equivalent by design), proven
  equivalents, deep tie-break variants that alter no observable output, and two million-cell
  performance guards that cost ~27s per test run to discriminate. Every one I could kill with a
  meaningful test, I killed. What is left needs either fault injection or a judgement that the
  cost is not worth it — and that judgement is recorded in the tests, not hidden.
- **5 owner-gated legs** (2 needing other people) and **8 owner-descoped distribution legs** —
  unchanged and not mine to move.
- **The quiet week of real daily use.** Still the only signal that matters, and still ahead.

## 2026-07-27 — iter 194 — Couch Review is multi-reviewer: named identities, attributed decisions, leased clips (2ce269c)

**Owner asked whether the app could be used remotely by phone/web and "sent to users".** The honest
answer was half-yes: Couch Review already served a phone page over LAN/Tailscale, but it was built
for exactly ONE reviewer, and handing its link to a second person did not merely lack features — it
silently broke three things. All three are now closed, each with its own fail-before gate.

**1. Attribution (Migration v43, `speech_segments.reviewed_by`).** One shared token meant every
decision landed anonymous: the corpus could not answer "who labelled this?". Each reviewer now has
their own token; the token RESOLVES the identity server-side (an unknown token has no reviewer, so
there is no path on which a decision can be written without a name); the name is written INSIDE the
same transaction as the verdict. `NULL` = not attributed (pre-v43 row, undecided row, or a desktop
decision where there is one human and no token naming them). **A fabricated "owner" was rejected** —
a provenance column that invents its own values is worse than an empty one.

**2. Per-reviewer undo (data loss).** ONE shared undo stack meant reviewer B's ↩ popped whichever
decision was last GLOBALLY — usually A's, on a clip B had never seen, with no indication to either
of them. Keyed by reviewer now.

**3. Leases.** Without them both phones get the same head-of-queue clips: duplicated work, and one
verdict silently overwriting the other. 15-minute lease on serve; another reviewer's queue skips it;
a decision on someone else's live lease is REFUSED (409). Leases expire, so a closed tab never
strands work.

Also: one accept thread per reviewer (one reviewer = exactly the previous single-thread server),
`recv_timeout` so every thread observes shutdown even if `unblock()` reaches only one; a failed
second spawn tears down the threads that did start; export manifest gains
`processingProvenance.reviewedBy` = `{byReviewer, notAttributed}`; the page shows WHICH reviewer it
is recording as; `heldByOthers` stops the page claiming "Queue reviewed" when the work is simply
held by someone else.

**TWO OF MY OWN DEFECTS, found by writing the gates rather than by the compiler:**
- A parallel `decision_log.annotator` column was written and **removed**. `decision_log` rows exist
  only for decisions carrying a `timestamp_ms`, which the phone path does not send — the column
  could never hold anything but NULL. It also broke three migration-replay tests: unlike
  `speech_segments` (which v40 recreates), a second ALTER on an untouched table is not replayable.
- The lease claim initially sat BEFORE validation in `api_decision`, so one malformed request would
  lock a clip away from every other reviewer for the full TTL — **a denial of service by typo**.

An export test also caught a wrong assumption of mine: it set `reviewed_by` as a struct field, but
`insert_segments_batch` correctly carries no human-decision column. The test now EARNS the
attribution through the real decision path.

**Gates (verbatim):** `cargo test --lib` **1055 passed; 0 failed; 6 ignored**; clippy
`--all-targets -D warnings` clean; `cargo fmt --check` clean; `npm run typecheck` 0 errors 0
warnings; `npm test` **39 passed (39) / 214 passed (214)**; `python scripts/run_python_policies.py`
**43 policy test scripts passed**.

**Fail-before demonstrated for all four new guarantees** by targeted reverts (never `git checkout`):
shared undo stack → `undo_is_per_reviewer_...` FAILED; lease check removed →
`deciding_a_clip_another_reviewer_holds_is_refused` FAILED; `reviewed_by` dropped from
`insert_segment_full` → `reviewer_attribution_survives_a_whole_row_upsert` FAILED; claim moved
before validation → `an_unqueued_submit_..._leaves_no_lease` FAILED.

**verify-10 at HEAD 2ce269c: VERDICT RED — 22 PASS, 1 FAIL, 1 SKIP-ENV.**
The single failure is `exe-freshness`, and it is CORRECT: the app was running during this
iteration, so the exe could not be relinked and still carries the previous commit's SHA
(`a9b7b45` != HEAD `2ce269c`). Nothing about the feature is unproven by it — every other kept gate
passed, including `test-rust` (422s), `fuzz-smoke` (671.9s), `refinery-lift` (37.0s), and
`egress-runtime`. **RED is the honest verdict until the app is closed and rebuilt**; it is not
downgraded here. `real-app-e2e` is SKIP-ENV (needs `CORTEX_AUDIO`), unchanged from prior runs.

**WHAT THIS DOES NOT DO, and is not pretended:**
- **Inter-annotator agreement is NOT enabled by this commit.** Leasing deliberately PREVENTS two
  reviewers seeing the same clip, which is the opposite of what IAA needs. Real agreement requires
  deliberate double-assignment plus a per-decision table; this schema holds one row per segment and
  cannot express it. Surfaced, not half-built.
- **Still plain HTTP**, token in the query string. Honest for a home LAN or a WireGuard tailnet;
  it must NOT be port-forwarded to the public internet.
- **No fair-share scheduler.** Whoever loads first takes up to 25 clips — bounded, self-correcting
  (leases expire, batches drain), and now honestly reported via `heldByOthers`.
- **OWNER REVIEW NEEDED:** three NEW Sorani strings (`settings.couchReviewers*` in `ckb.ts`) have
  had no native review. They exist because `test_i18n_consistency.py` requires en/ckb key parity —
  English-only fails the gate, and weakening the gate is not an option. Vocabulary is limited to
  words already used elsewhere in that file.
- The nightly CI jobs, the ~72 triaged mutants, the owner-gated legs, and the quiet week of real
  daily use are all unchanged from iter 193.

## 2026-07-27 — iter 195 — remote review taken to a professional bar: Phase 0-3 of docs/REMOTE_REVIEW_PLAN.md

**Owner: "app is closed, start phase 0 and do all".** Phase 0 (rebuild), Phase 1 (1.1–1.6), Phase 2.1,
Phase 2.7 and Phase 3.1–3.5 are shipped, each gated and each with a fail-before proof.
Commits: `41a037e` (P1), `59c4604` (P2.1), `976d8b8` (P3), plus this entry's gate work.

**P0 — exe rebuilt.** `npm run tauri build` succeeded in 7m57s at `cc7fadf`; the MSI bundled. Note the
exe went stale again immediately, because every commit below changed source — a final rebuild is
required before verify-10 can be GREEN, and that is stated rather than glossed.

**P1.1 — the phone page was ENGLISH.** `<html lang="en">` with hardcoded "Looks good" / "Save & next"
around an RTL textarea: an English chrome on a Kurdish-first app. Now opens in Sorani RTL with an
English toggle. **13 of its 14 strings are copied BYTE-FOR-BYTE from Sorani a native speaker already
reviewed** (review.acceptAsIs, review.saveNext, inbox.reject, review.undone, review.undoFailed,
review.allDone, review.progress, review.editHint, inbox.status.{load,edit}Failed, saved). Only
`heldByOthers` is new and it is flagged. New policy `scripts/test_couch_page_i18n.py` pins that: a
sourced string must still match its desktop value exactly, the unreviewed set cannot grow silently,
en/ckb keys stay in parity, and `{param}` placeholders match across languages.

**P1.2/1.3/1.6 — reliability on a real phone.** Server-side idempotent re-submit (a retry no longer
writes a second decision or distils a second DPO pair); localStorage drafts + an outbox replayed on
reconnect; `POST /api/renew` heartbeat so a 15-minute lease cannot expire mid-correction; per-reviewer
rate limiting (120/min, burst 60) — these HTTP routes were the ONE unthrottled path into the DB.

**P1.4/1.5 — transport + prefetch.** Replay-2s, loop, 0.75–1.5x speed (persisted); next clip's audio
warmed while the current one is reviewed.

**P2.1 — GOLD SPOT-CHECKS, the item this whole plan turns on.** Every gate in this repo asks whether
the MACHINE is honest; none asked whether the REVIEWER was. A clip a human already answered, served
with its RAW (known-wrong) draft, separates listening from tapping with no synthetic data at all.
Migration v44 `spot_checks` (PK (segment_id, reviewer), so a retry upserts). ~1 in 8 of each batch,
unmarked and interleaved. **A spot-check submit writes ONLY to `spot_checks` — `speech_segments` is
untouched**, so a blind accept cannot overwrite the answer key it is graded against. Surfaced in
Settings (noticed/given + mean CER, worst first) and via new IPC `spot_check_report`.

**P2.7 — gates extended to the phone surface.** `couch.rs` added to `.cargo/mutants.toml` (it now
carries real decision logic, not HTTP plumbing). New `e2e/couch-page.spec.ts`: 7 tests including a
WCAG 2.2 AA axe gate in BOTH themes — `e2e/axe.spec.ts` covered the desktop only, and the phone is the
surface handed to people who are not the owner.

**P3.1–3.5 — light/dark via CSS variables, adjustable Sorani text size (16–28px), swipe
accept/reject, a session counter, and iOS home-screen standalone.** Scope corrected honestly: there
is deliberately **no service worker and no manifest**, because service workers require a secure
context and this is plain HTTP on a LAN/tailnet IP — Tailscale does not change that. Offline caching
and Android installability are UNREACHABLE without TLS, which the plan rules out.

**SIX DEFECTS FOUND, every one by a check rather than by reasoning:**
1. **Late-submit overwrite (data loss).** The lease is released the instant a clip is decided, leaving
   an already-decided clip unprotected: a stale page could silently replace another human's verdict
   minutes later. Now 409, naming whose verdict is protected.
2. **`hidden` was a no-op on the review card (pre-existing).** `#card` sets `display:flex`, which beats
   the attribute's `display:none` default — the empty card rendered beside "all reviewed". Caught by
   LOOKING at the page in a browser, not by any test.
3. **Spot checks identified by row STATE broke ordinary reviewing.** A reviewer's own edit makes a row
   human-verified, so their next submit was graded as a test and never written to the corpus. Now
   identified by why the clip was HANDED OUT.
4. **Floor division silently exempted short batches** from spot checks — a reviewer on a nearly-drained
   queue was never measured. Now `div_ceil`.
5. **`list_spot_check_candidates(0)` returned ONE** (limit tested after the push). Found because a
   fail-before revert FAILED TO FAIL; chasing that instead of waving it through is the only reason it
   surfaced.
6. **Three real a11y violations**, found by the new axe gate: heading contrast 3.91:1 in light, meta
   contrast 3.64:1 in dark, and **the transcript textarea had no accessible name at all** — a
   screen-reader user reached an unlabelled edit field. All fixed; the label reuses `review.editHint`,
   so it cost zero new unreviewed Sorani.

**Gates (verbatim):** `cargo test --lib` **1061 passed; 0 failed; 6 ignored**; clippy
`--all-targets -D warnings` clean; `cargo fmt --check` clean; `npm run typecheck` 0 errors 0 warnings;
`npm test` **214 passed (39 files)**; `npx playwright test e2e/couch-page.spec.ts` **7 passed**;
couch-page i18n policy: *"14 strings, Kurdish-first; 13 reuse natively-reviewed desktop Sorani
byte-for-byte; 1 awaiting owner review"*.

**Fail-before demonstrated** for every new guarantee by targeted revert (never `git checkout`):
shared undo stack, missing lease check, `reviewed_by` dropped from `insert_segment_full`, claim before
validation, idempotency removed, renewal ignoring the holder, rate limiter removed, spot-check routing
removed, floor division, limit-after-push.

**NOT DONE, and not implied:**
- **The exe is stale again.** verify-10 cannot be GREEN until a final rebuild; it was RED on
  `exe-freshness` alone at `2ce269c` (22 PASS / 1 FAIL / 1 SKIP-ENV) and nothing since has changed
  that gate's status.
- **Inter-annotator agreement is still NOT enabled** (plan §2.4). Spot checks measure ATTENTION, not
  agreement; leasing prevents the overlap IAA needs on purpose. `scripts/agreement_kappa.py` exists and
  is unit-tested — what is missing is a per-decision table and deliberate double-assignment.
- **Spot checks are only as good as the gold behind them.** With few human-verified clips whose draft
  is wrong, `checks` stays small; the panel reports the real count so that limit is visible.
- **A mutation sweep over `couch.rs` is running and has already reported survivors** (the
  `MAX_BODY_BYTES` and `LEASE_TTL` constants have no test pinning them). Not yet triaged.
- Plan §2.2 (throughput panel + timestamps), §2.3 (audit log), §2.5 (two-browser e2e), §2.6 (soak with
  injected network failure), §3.6–3.8 remain.
- One new Sorani string (`heldByOthers`) plus the three `settings.couchReviewers*` and two
  `settings.couchSpotChecks*` strings await a native read.

## 2026-07-27 — iter 196 — Phase 2.4 / 2.6 shipped; verify-10 GREEN except the ledger gate itself

Continues iter 195. Commits `0256dae` (P2.4 agreement export), `96f6945` (P2.6 soak + gold fix),
`3cde966` (settings crash fix). Exe rebuilt twice; **`exe-freshness` now PASSES**.

**P2.4 — inter-annotator agreement, and a scope correction.** The plan sized this as the LARGEST
remaining item, needing a per-decision table and a double-assignment mechanism. **Both already
existed.** Spot checks are deliberately NOT leased — measuring two people independently is the point —
so the overlap a kappa study needs is already produced as a side effect, and `spot_checks` is already
one row per (clip, reviewer). Only the export was missing. `Database::agreement_sample()` emits the
exact TSV `scripts/agreement_kappa.py` consumes; a new IPC command writes it beside the library.
**No kappa is computed in Rust** — the script is already unit-tested against the textbook κ=0.40
example, and a second implementation would be an unverified copy of a verified one. Verified end to
end: a TSV of the exported shape fed to the real script returned *"Cohen's kappa = 0.5000 (moderate)
N=3 items, 2 raters"* — that proves the FORMAT; the 0.5000 is synthetic test data, not a measurement.
With ≥3 reviewers the pair sharing the most clips is reported and the excluded raters are NAMED.

**P2.6 — soak, and it found a defect I would not have looked for.** Three reviewers concurrently over
real HTTP, every decision submitted twice. Clients reported **61 successes against 60 clips**: a clip
a reviewer had JUST corrected became a spot-check candidate the moment they saved it, so the next
reviewer was graded against **a peer's guess** and marked "did not notice" for merely disagreeing.
Fixed by requiring `is_gold = 1` — an answer key has to actually be an answer key. Honest cost, now
stated in the code: spot-check volume is bounded by the gold set, so a small gold set yields few
checks; `checks` reports the real number.

Two things the soak MEASURED rather than assumed, recorded in the test instead of worked around: the
throttle does NOT fire during ordinary concurrent reviewing (leasing partitions the work), and
`accepted <= decided` rather than equality (requiring equality would fail the test on the very retry
behaviour it exists to prove). Stability confirmed 8/8 isolated + 5/5 whole-suite after the fix; it
was 7/8 before.

**A regression I introduced, caught only by the FULL e2e suite.** verify-10 went RED on
`test-e2e+a11y`, and the failures were NOT my new phone-page spec (7/7 standalone) — they were
`settings modal has aria-modal and role` and `settings panel opens and closes`. The e2e tauri mock had
no case for `spot_check_report`, so `invoke` resolved to undefined, `spotChecks` became null, and
`{#if spotChecks.length}` threw mid-render: **the entire settings dialog failed to mount.** A panel
that merely REPORTS reviewer quality took down the settings the app depends on. Fixed on both sides
(`?? []` in the component, an array in the mock).

**Also fixed:** a cargo-mutants `--in-place` artifact I had committed in `39ccd55` (see `07f5724`) —
`name.chars().count() < MAX_REVIEWER_NAME`, inverted, which would have rejected every reviewer name
shorter than 40 characters. Rule recorded: **cargo-mutants `--in-place` is not a background job.**

**verify-10 at `3cde966`: 21 PASS, 1 FAIL, 1 SKIP-ENV.** The single FAIL is `python-policies`, and it
is *this gate*: `test_ledger_staleness` refusing commits without a ledger entry — which this entry
answers. Everything else passed, including `test-rust` (397.3s), `fuzz-smoke` (966.6s), `test-e2e+a11y`
(16.3s), `refinery-lift`, `egress-runtime`, and **`exe-freshness`**. `real-app-e2e` remains SKIP-ENV
(needs `CORTEX_AUDIO`).

**NOT DONE, and not implied:**
- **verify-10 has not yet been observed GREEN end to end.** The two RED runs were `exe-freshness`
  (stale binary) then `python-policies` (this ledger). Neither indicates a defect in the shipped work,
  but *"GREEN"* is not claimed until a run actually prints it.
- Plan §2.2 (throughput panel + timestamps), §2.3 (audit log), §2.5 (two-browser e2e — partially
  covered by the Rust soak and the phone-page spec), §3.6–3.8 remain.
- **The `couch.rs` mutation sweep has never completed.** The one run was aborted after it corrupted the
  working tree; it reported 2 MISSED constants before dying, both now pinned. The full survivor list is
  UNKNOWN and must not be described as clean.
- Owner review still pending on: `heldByOthers`, `settings.couchReviewers*`, `settings.couchSpotChecks*`,
  `settings.couchAgreement*`.
- The nightly CI jobs and the quiet week of real daily use are unchanged from iter 193/194.

## 2026-07-28 — iter 197 — verify-10 GREEN 23/23 (real audio); first completed mutation sweep; P2.2/2.3/3.7/3.8 shipped

Commits `d1f531b` (STATUS regen), `afd6057` (P2.2/2.3/3.7), `4759030` (mutation kills), `c2302b2`
(P3.8 cookie).

**verify-10 is GREEN — 23/23, ZERO SKIPS, at `ad2d89e`.** The last skip was closed by driving
`real-app-e2e` with real Kurdish audio (`CORTEX_AUDIO`, passed at runtime — never written into a
tracked file). It is not a vacuous pass: the driver spawned the real exe on an ISOLATED data dir (it
refuses the owner's `%APPDATA%` profile by design), applied migrations v1→v44, and reported
*"Wrote run.jsonl with 144 segments … REAL-DATA RUN OK: 14 segments; first transcript 194 chars"*.
`docs/STATUS.md` regenerated from that run — it had been claiming GREEN at `9228618`, many commits
stale, which is dangerous precisely because it happened to be RIGHT.

**THE FIRST MUTATION SWEEP THAT HAS EVER COMPLETED here.** In COPY mode, so the working tree is never
touched — the earlier `--in-place` run is what corrupted the tree and got an artifact committed.
Verbatim: **`946 mutants tested: 98 missed, 789 caught, 23 unviable, 36 timeouts`**. 29 survivors were
in `couch.rs`; after two rounds of gates, **29 → 19 → 13 killed in total**. Each was invisible to
1,066 passing tests:
- **The routing table was never exercised.** Deleting the `/api/renew` or `/api/undo` arms changed
  nothing — every test called the handlers DIRECTLY, so they stayed correct while becoming
  unreachable. `/api/audio` needed a BAD id (its own 400) because "404 for an unknown id" cannot be
  told apart from the router's 404 — which is why that guard survived the first fix.
- **The body cap could TRUNCATE instead of refuse**: `take(MAX+1)`→`take(MAX-1)` makes an over-size
  body read short, pass the `> MAX` check, and fail later as bad json — a truncated transcript
  disguised as a parse error.
- **A re-sent reject was uncovered** (only the edit retry was), so a retried reject would have been
  recorded as a second human decision.
- **The placeholder guard needs `&&` and nothing proved it.** With `||`, any transcript merely ENDING
  in `]` would be refused. A rejects-only loop STRUCTURALLY cannot catch this — the bug refuses MORE —
  so it needed an ACCEPT assertion. I first wrote that case into the rejects loop expecting 400, which
  was wrong.
- **The limits themselves were never tested**, only limit+1: `>`→`>=` would reject a legal 40-char
  name or an eighth reviewer.

**P2.2/P2.3 — checking the code changed the design twice.** The plan said "pass a timestamp into
`decision_log`". Reading `stats.rs` first: it medians over a GLOBALLY ordered stream, so feeding
concurrent reviewers in would time the gap between two DIFFERENT people and report it as one person's
pace — poisoning a shipped number. And an ALTER on `decision_log` is not replay-safe (v40 recreates
speech_segments; nothing recreates decision_log). So Migration v45 adds `review_events` instead:
replay-safe, partitioned per reviewer by construction, existing metric untouched. I also expected
§2.3 to be redundant and CHECKED: `corrections` covers only edits with resolvable audio,
`decision_log` gets no phone rows, and neither records WHO — so it is not redundant.

**P3.7 — my first revoke was a NO-OP and the compiler could not see it.** Each accept thread held an
`Arc<HashMap>` SNAPSHOT of the tokens, so removing one left every serving thread honouring it forever:
revoked in the UI, working in reality. The token map now lives in the one shared state the request
path authenticates against. **And the fail-before for it PASSED at first** — the test had
reimplemented the revocation inline instead of calling it. Extracted `revoke_in()` so the test drives
the real path; only then did the no-op revert fail.

**P3.8 — the token is out of the URL.** The page response plants `cortex_couch` (HttpOnly,
SameSite=Strict, server-set so page JS can never read it) and the page strips `?t=` from the visible
URL and history. Cookies also ride `<audio src>`, which cannot send a custom header — which is why a
cookie and not an Authorization header. NO `Secure` flag, deliberately: on plain HTTP that would stop
it being sent at all.

**A regression I introduced and the FULL suite caught:** a null `spot_check_report` from the e2e mock
made `spotChecks.length` throw mid-render and took the WHOLE settings dialog down (`3cde966`). My new
spec passed 7/7 standalone; only `npm run test:e2e` saw it.

**Gates (verbatim):** `cargo test --lib` **1066 passed; 0 failed; 6 ignored**; clippy
`--all-targets -D warnings` clean; fmt clean; `npm run typecheck` 0 errors 0 warnings; `npm test` 214
passed; `npm run test:e2e` **54 passed**; playwright couch-page 7 passed (zero axe violations, both
themes); python-policies **44/44**.

**NOT DONE / NOT CLAIMED:**
- **The exe is stale again** (every commit since `ad2d89e` changed source). verify-10 was GREEN at
  `ad2d89e`; it has NOT been re-run at the current HEAD.
- **Remaining couch mutants are named, not rounded away:** `tailscale_ip` + its CGNAT guard need a
  live tailnet (the UDP connect returns early without one, so the guard is unreachable); `lan_ip` only
  affects a displayed URL; `start()`'s own wiring (`reviewers: tokens`, `is_running`) is untested
  because start() binds the fixed port 8737 — a flake risk on this box worth weighing, not casually
  adding; the spot-check interleave-position survivors are cosmetic placement.
- **~69 long-standing survivors in diff/normalizer/g2p/conformal/irt/signal_anomaly** are unchanged
  and still the documented hard tail.
- Plan §2.5 (two-browser e2e) and §3.6 (waveform) remain. §2.5 is partly covered by the Rust soak.
- **Seven Sorani strings await a native read** (`heldByOthers`, `settings.couchReviewers*`,
  `settings.couchSpotChecks*`, `settings.couchAgreement*`, `settings.couchRevoke`,
  `settings.couchThroughput`). "Remote 10/10" stays qualified while they do.
- Nightly CI has still never run on a runner; the quiet week of real daily use is still ahead.

## 2026-07-28 — iter 198 — the phone page was broken over the wire in two ways, and only a LIVE run found them (1758d1e, 475a1ca)

**The exe finally matches HEAD.** `npm run tauri build` exit 0, both bundles produced. The stale
`cortex.lock` left by force-killing the old process did NOT block startup — the flock fix from
`4924a88` proved itself in production, which is the first time it has been exercised for real.

**Then I stopped trusting the test suite and drove the real server instead.** Couch Review started
via CDP against the running app (`start_couch_review`), both links issued including the Tailscale
one, and a real headless iPhone-profile browser pointed at `http://<this-pc-tailnet-ip>:8737`. Everything
the suite covers came back green — `ckb`/`rtl`, 25 clips, real Sorani text, token stripped from the
URL, `cortex_couch` HttpOnly cookie set, zero console errors. **Two defects that no test could see
came back with it.**

**Defect 1 — every clip had an INFINITE duration.** `player.duration` and `seekable.end(0)` both read
`Infinity`, so the phone's progress bar showed no total time and tap-to-seek multiplied by Infinity
and silently did nothing. Raw wire capture:

```
HTTP/1.1 200 OK
Content-Type: audio/wav
Transfer-Encoding: chunked          <- and NO Content-Length
```

tiny_http chunks any body at or above its 32 KB default. Clips are 300–500 KB so every one crossed
it; the JSON replies this suite is full of are 2–10 KB and sat comfortably underneath. That gap is
the whole reason 1069 passing tests missed it. Fixed at the single choke point every reply passes
through. The regression test uses a 200 KB body **deliberately** — at 1 KB it passes with the bug
present.

**Defect 2 — `stop()` reported a stopped server while still holding port 8737.** Settings → Stop →
Start answered `os error 10048` and remote review stayed dead until the whole app was restarted.
Measured, not inferred: the listener was **still LISTENing 120 s after `stop()` returned**, and a
five-cycle stop/start loop against the live app failed on cycle 2.

Root cause is Windows-specific and sits in a dependency: tiny_http parks a private accept thread in a
blocking `accept()`, and *that thread*, not the `Server` value, holds the socket. `Server::drop` sets
its close flag and wakes the thread by connecting to its own listening address — which is `0.0.0.0`,
and on Windows connecting to `0.0.0.0` fails outright (it is a wildcard for BINDING, not a
destination). The wake never landed. Worse, the port kept accepting TCP with nobody left to answer,
so an old link didn't fail — it **hung**. My own HTTP probe is what accidentally freed the port
mid-investigation, which is what made the first reading look like a race.

**The existing start/stop test asserted the tokens died but never that the PORT came back.** It now
re-starts the server, because restarting is what the owner actually does and what was broken.

**Gates (verbatim, this machine, this HEAD):** `cargo test --lib` **1070 passed; 0 failed; 6
ignored**; `couch::` **24 passed**; clippy `--all-targets -D warnings` clean; fmt clean; `npm run
typecheck` **426 FILES 0 ERRORS 0 WARNINGS**; `npm test` **214 passed (39 files)**.

**Measured on the live server (127.0.0.1 and the Tailscale address, both):** `GET /` 6.4 ms ·
`GET /api/queue` 1.8 ms · `GET /api/audio/<id>` 82–111 ms for 330–460 KB clips. I had suspected the
per-request full-file decode was a remote-UX problem and it is NOT — measured, not assumed, and the
page already prefetches the next clip, so the cost is hidden anyway. **No cache was added.**

**Live tailnet state:** `<this-pc>` = <this-pc-tailnet-ip> online; `<phone>` = <phone-tailnet-ip>
online, `tailscale ping` **pong via DERP(fra) in 184 ms** (relayed, not direct). Tailscale adapter is
categorised **Private**, and the `cortex-speech-app.exe` inbound rule allows Private — so the path is
open. Review queue depth from the live DB: **128 unverified clips**, 16 verified, single source WAV
present on disk.

**NOT DONE / NOT CLAIMED:**
- **CORRECTION to my own first reading: the spot-check pool is not merely empty, it is UNREACHABLE.**
  I first wrote this up as "data state, not a code defect". That was wrong, and checking rather than
  assuming is what caught it. `list_spot_check_candidates` requires `verified = 1 AND is_gold = 1`,
  and **no production code path anywhere sets `is_gold = 1`** — every write of it lives inside
  `#[cfg(test)]` (`couch.rs:1383`, `history/mod.rs:347,430`, `jury/learning.rs:689`), and the
  migrations only ever declare it `DEFAULT 0`. The `gold_segments` table is a DIFFERENT thing (the
  frozen eval set via `import_gold_segments`) and does not feed this flag. The live DB agrees: 0 gold
  rows. So the proof-of-work centrepiece of plan §2.1 is structurally inert in any real install, and
  no amount of reviewing will change that.

  **FIXED in `3d1c418`, and without touching `is_gold` semantics.** The intent behind the flag was
  right — an answer key must not be a peer's fresh guess — so I kept the guarantee and expressed it
  with a column that is actually populated: `reviewed_by IS NULL`. `record_human_decision_by` sets
  `reviewed_by` unconditionally to the deciding reviewer's name and ONLY the desktop path passes
  `None`, so NULL means "verified here, by the owner". `is_gold = 1` is still honoured. This
  deliberately does NOT mark anything gold, so the learning-set exclusion and export holdout
  quarantine are untouched. The live library yields **15** usable answer keys immediately.

  The old test had expressed "peer" as `is_gold = false` and never set `reviewed_by` — so it passed
  against a query that could match nothing in production, which is exactly how a dead feature keeps a
  green test. Its fixture now carries the column production writes. Fail-before: candidates left 2,
  right 3.
- verify-10 has NOT been re-run at this HEAD yet.
- `start_issues_working_tokens_and_stop_takes_them_away` binds the real fixed port 8737, so it FAILS
  while the app is running with Couch Review on. That is by design (a silent skip would restore the
  blind spot) but it means verify-10 cannot be run during a live review session.
- **No request has yet come from the phone itself.** Everything above was driven from this PC; the
  inbound-from-another-device path is proven only as far as "the tunnel is up and the firewall
  allows it".
- **Seven Sorani strings still await a native read** (`heldByOthers`, `settings.couchReviewers*`,
  `settings.couchSpotChecks*`, `settings.couchAgreement*`, `settings.couchRevoke`,
  `settings.couchThroughput`). "Remote 10/10" stays qualified while they do.
- Nightly CI has still never run on a runner; the quiet week of real daily use is still ahead.

## 2026-07-28 — iter 199 — remote review is LIVE and proven on real data; two hardening passes taken while it runs (bf3c0c5, 8b9abbd)

**Handed over working.** Exe rebuilt at `615501f`, freshness gate OK, app running, Couch Review
serving over Tailscale. All three fixes from iter 198 verified ON THE LIVE SERVER, not in a test:

- `Content-Length: 395340` now present on clip responses; a real headless iPhone-profile browser
  reports **`duration: 12.353`** where it previously reported `Infinity`.
- Five consecutive Stop→Start cycles succeeded (`stopMs` 6/18/26/27/28). Before the fix, cycle 2 died
  with `os error 10048`.
- The batch served **29 clips = 25 real work + 4 spot checks** (`div_ceil(25, 8) = 4`). Every check
  had `reviewed_by = None` (owner-verified) and a draft that differs from the stored answer. One is a
  textbook trap: served `500 لیترە`, answer `پێنج سەد لیترە`.

Live page check over `<this-pc-tailnet-ip>`: page 23 ms, `ckb`/`rtl`, token stripped from the URL,
`cortex_couch` HttpOnly cookie set, **zero console errors**. Tailnet: phone online, `tailscale ping`
pong via DERP(fra) — 184 ms warm, 2.1 s on the first cold packet.

**A concern I chased and DISPROVED, rather than "fixed".** One live spot check differs from its
answer only by `ه` vs `ھ`, so I checked whether reviewers get marked wrong for orthographic variants.
They do not: `noticed` compares the submission against the **draft** they were given, not against the
answer, so any genuine correction counts as attention; agreement is a continuous CER, not pass/fail.
`learning_text_key` deliberately keeps the variants distinct, which is right for CANDIDATE selection.
No change made.

**Hardening 1 (`bf3c0c5`) — the gate could not run while the app was being used.** The one test
covering `start()`'s wiring bound the production port 8737, so `cargo test` failed with
`os error 10048` whenever Couch Review was running: the project could not verify itself during
exactly the daily use the gate protects. `start()` now delegates to `start_on_port()` and the test
takes port 0. **The gate is not weakened** — the re-start assertion rebinds the SAME port, read back
out of the issued URL (asking for another ephemeral port would pass even with the socket leak,
because the OS would just hand out a different one), and `COUCH_PORT` is now pinned to 8737 by the
constants test. Fail-before still catches the leak, now on port 51195. **Proven by running
`cargo test --lib` to 1070 passed WITH the live server holding 8737.**

**Hardening 2 (`8b9abbd`) — the most-repeated defect in this repo had no gate on the phone path.** A
save path that treats `""` as a successful transcript and writes it over a good draft has been found
and fixed twice elsewhere. couch.rs HAD the guards; nothing pinned them, so either could be deleted
in a refactor with the suite still green. Now covered for empty / whitespace-only / newline-only on
both `accept` and `edit`, plus the `[Pending WSL 7B ASR]` placeholder family — and, the half that
actually matters, the stored row is asserted UNCHANGED afterwards. A refusal that still wrote would
look like a working guard from the client's side and lose the data anyway.

**Gates (verbatim, with the live server running):** `cargo test --lib` **1070 passed; 0 failed; 6
ignored** (before iter-199 test additions), `couch::` **25 passed**; clippy `--all-targets -D
warnings` clean; fmt clean; `npm run typecheck` **426 FILES 0 ERRORS 0 WARNINGS**; `npm test` **214
passed**; `npm run test:e2e` **57 passed**; playwright couch-page **10 passed**; python-policies
**44/44**; `cargo test --test soak` **passed in 143.69 s** standalone.

**NOT DONE / NOT CLAIMED:**
- **verify-10 has NOT completed at any HEAD today.** The run I started wedged: under `cargo test
  --jobs 4` the pipeline soak was starved for 30 minutes (it passes in 143 s alone). I killed it. I
  am NOT calling this GREEN, because no run produced that. `real-app-e2e` also cannot run while the
  app is open — it spawns the exe and the single-instance lock refuses it — so a full sweep needs the
  app closed.
- **`npm run tauri build` exited 1 on the final rebuild** — MSI bundling only, `os error 32`, because
  I launched the app while `light.exe` still held the exe. My race, not a code fault. The exe itself
  compiled clean and the freshness gate confirms it is at HEAD. The NSIS/MSI bundles on disk are from
  the PREVIOUS build; installer artifacts are descoped from personal use, but they are stale and I am
  saying so rather than letting them look current.
- **The exe is now behind HEAD again** (`bf3c0c5`, `8b9abbd` touched couch.rs). Both are test//
  test-support changes and production behaviour on port 8737 is unchanged, so the running binary is
  functionally current — but exe-freshness will fail until the next rebuild, which needs the app
  closed.
- **Still no request from the phone itself.** Everything is driven from this PC. The inbound path is
  proven only as far as "tunnel up, firewall allows, adapter is Private".
- **Seven Sorani strings still await a native read.**
- Nightly CI has still never run on a runner; the quiet week of real daily use is still ahead.

## 2026-07-28 — iter 200 — three more data-loss / work-loss paths on the phone page, found by reading failure paths rather than happy ones (ef3a7b7, b8bc3a3, 3fa7acb)

All three were found the same way the iter-198 defects were: by asking what a REMOTE reviewer
actually experiences when something goes wrong, rather than by running the suite. All three are fixed
with a fail-before. The app stayed running and serving throughout — nothing here touched the live
session.

**`ef3a7b7` — an expired link silently destroyed queued work, and MY OWN FIX made it more reachable.**
Fixing Stop→Start (iter 198) means restarting Couch Review is now easy, and every restart regenerates
every token. A reviewer who was offline with decisions in the outbox reconnects to a 401 — and
`flushOutbox` treated ANY status as "the server gave a real answer, drop it". Their queued decisions
were discarded without a word. **401 is not a verdict on the decision; it says the LINK died**, and a
fresh link replays it. 401 and 5xx are now kept; 409 (taken) and 400 (invalid) are still dropped,
because those genuinely are answers about that decision. Fail-before: outbox 0, expected 1.

The page also rendered this as `Failed to load queue: unauthorized` — the operative word in ENGLISH,
on a Kurdish-first page, with no hint that the fix is to ask for a new link. 401 now gets its own
message, which can honestly promise nothing is lost only BECAUSE of the outbox change above.

**`b8bc3a3` — the sibling, and the likelier half.** A link does not usually die while the reviewer is
idle; it dies WHILE THEY ARE WORKING, because the owner restarted the server or added a reviewer.
`flushOutbox` had been taught to keep a 401, but `decide()` never queued one at all: a 401 on submit
only raised a toast. The reviewer's typed text survived as a draft, and the VERDICT — accept, reject,
edit — was recorded nowhere and simply lost, for every tap after that moment. Now held exactly like
the offline case, with the reviewer moved on so they can keep working. Fail-before: held 0, expected 1.

**`3fa7acb` — an ACTIVE reviewer could still lose their clip.** Renewal ticks are skipped while the
page is hidden (deliberate — an idle reviewer should release clips), but the tick is 4 minutes
against a 15-minute lease, and on a phone backgrounding is constant. A reviewer returning at minute
13 has the lease lapse under them at 15 while they are typing, with no renewal due until 16; another
reviewer takes the clip and their correction is refused 409 at save. Now renewed on return. The test
pins BOTH directions — a listener that also fired on hide would quietly defeat the release-when-idle
property. Fail-before: renewals 0, expected 1.

**A gate I had to change, stated plainly rather than buried.** The port refactor (`bf3c0c5`) moved
`start()`'s body into `start_on_port`, and `test_restore_reservation_gate.py` scans a named function
for the restore fence — so it failed. I did NOT hoist the check back into `start()`: that would
satisfy the scan while moving the check OUTSIDE the `COUCH` lock, and the entire atomicity argument
is that the check and the handle register are serialized by the same mutex the restore fence reads.
The gate now scans `start_on_port` and additionally asserts `start` still delegates to it. **Verified
the gate still bites** by deleting the guard (it fails).

**Gates (verbatim):** playwright couch-page **13 passed**; `npm run test:e2e` **60 passed**;
python-policies **44/44**; `cargo test --lib couch::` **25 passed**; `cargo test --lib` **1070
passed** (run WITH the live server holding 8737 — the point of `bf3c0c5`); clippy `--all-targets -D
warnings` clean; fmt clean.

**NOT DONE / NOT CLAIMED:**
- **Still zero remote reviews.** `review_events` = 0, `spot_checks` = 0, 128 clips pending. The
  inbound-from-the-phone path remains the one thing nothing on this PC can prove. A monitor is armed
  on the audit trail to catch the first one.
- **verify-10 has not completed at any HEAD today.** It needs the app CLOSED — `real-app-e2e` spawns
  the exe and the single-instance lock refuses it.
- **The exe is behind HEAD.** The running binary has the three iter-198 fixes; everything in iter 199
  and 200 (`linkExpired`, both outbox fixes, the lease renewal) needs a rebuild, which is not being
  done under a live session.
- **`linkExpired` is NEW Sorani** and is acknowledged in `UNREVIEWED_SORANI` — now 2 keys awaiting a
  native read, up from 1. The other seven desktop-sourced strings are unchanged.
- The on-disk NSIS/MSI installers are stale (the last `tauri build` exited 1 on MSI bundling, my race
  with launching the app).

## 2026-07-29 — iter 201 — the durable-remote-use ask: links now survive closing the app; plus a returning-reviewer lockout and three loop finds (74437c6, 8ef685a, 120ff6f, 2a50a49, d151afa)

**Owner ask, verbatim intent: "whenever I want, from my phone or laptop, I open it and review — without
coming back to this PC."** Two separate defects stood in the way; both are fixed with fail-befores.

**1. `74437c6` — a returning reviewer was locked out by an EMPTY token shadowing their cookie.**
Reported as "I close the browser on my iPhone and go back and it doesn't open." Reproduced with a real
browser: the page strips `?t=` after the first load and relies on the HttpOnly cookie — but it kept
appending `?t=` (empty) to every request, and the server reads query-before-cookie, so `""` was
treated as a supplied-but-WRONG credential and the valid cookie in the SAME request was ignored.
Measured on the wire: `cookie, no t=` → 200; `cookie + empty t=` → 401. Every piece was individually
correct, which is why no test saw it — the defect only exists in the combination a second visit
produces. Fixed both sides (server: empty `t=` counts as absent; page: a single `withToken()` omits
the param entirely), and pinned end-to-end. A NON-empty wrong token still fails — falling back to the
cookie there would let a revoked link keep working.

**2. Session persistence — closing the app no longer kills every link.** Tokens were per-session by
design ("never persisted"), which is airtight and unusable: every app restart meant walking back to
the desktop for a fresh URL. The new distinction is between the two ways a session ends: **closing
the app is not an access decision** (nothing calls `stop()` on exit) so the session is remembered
(`couch_session.json`, tokens DPAPI-protected at rest like API keys, atomic replace on write);
**pressing Stop IS the decision** and deletes the file, so "stopping revokes every link" stays
literally true — and the revoke now provably survives a restart. Resume happens in `.setup()` at app
launch, best-effort (a bind failure must not stop the app opening). A session remembered against a
DIFFERENT library refuses to resume. Pinned by `a_link_survives_closing_the_app_but_not_pressing_stop`.

**Honest cost, stated not hidden:** links are now long-lived credentials. Anyone holding one can
review until Stop/revoke. The file is DPAPI-bound to this Windows user so copying it off-machine
yields nothing.

**A test-interaction bug I introduced and fixed in the same sitting:** the new start/stop test and the
existing one both drive the global `COUCH` singleton; in parallel they steal each other's server.
Reproduced 3-for-3, serialized with a test-local lock, stable 3-for-3 after.

**Also this ledger entry: loop iterations 1–4** (details in each commit): `8ef685a` the dashboard
counted clips the export refuses to publish — 7th instance of the recurring count bug, caught by
cross-checking stats against the REAL export output; `120ff6f` v46 — a reviewer's spot-check score
now survives deleting the clip it was measured on (the v45 principle applied to the table it named);
`2a50a49` the aligner now reports a vocabulary with no word-delimiter token (found: two copies of
mms_aligner_tokens.txt differing by ONE trimmed byte); `d151afa` pinned the UI-thread retry budget in
stop()'s listener release (~600 ms measured worst case, 1.5 s budget).

**Live-session verification this morning:** rebuild at `d151afa` → v46 applied on the real library
(score preserved byte-identical, FK gone, integrity ok) → fresh couch session served **4 fresh spot
checks, zero repeats** (the answered trap correctly excluded — with the old binary it would have been
re-served and overwritten the honest score).

**Gates (verbatim):** `cargo test --lib` **1078 passed; 0 failed; 6 ignored** (couch:: 28, 3× stable);
clippy `--all-targets -D warnings` clean; fmt clean; playwright couch-page **14 passed**; `npm run
test:e2e` **61 passed**; python-policies **44/44**.

**NOT DONE / NOT CLAIMED:**
- **The exe predates everything in this entry** — the durable-link system, the returning-reviewer
  fix, and the aligner guard are source-only until the next rebuild.
- **Resume-at-launch has not yet been observed in the running app** (unit-tested only); it will be
  verified on the next real launch cycle.
- The two MSI bundle failures today were MY race (launching the app while light.exe still held the
  exe); installers on disk are stale. The compiled exe itself was verified at HEAD both times.
- verify-10 still has not completed at any HEAD since `7d77fb5`.
- `linkExpired` + `heldByOthers` still await a native Sorani read.

## 2026-07-29 — iter 202 — public-links plan phases 1/2/3/4/6 executed and live-verified; phase 5 at the owner's gate (625e8b1, 525ece3, 1efd8d4, d1f9663, ee8a106)

The plan (`docs/REMOTE_PUBLIC_LINKS_PLAN.md`, from the 12-agent research workflow) went from paper to
running system in one sitting. Everything below was verified against the LIVE server after one
rebuild, not claimed from tests alone.

**Phase 1 (`625e8b1`)** — the repo stopped lying: couch.rs header + runbook said tokens are "never
persisted" and closing the app revokes links — the opposite of shipped behaviour. Acceptance grep
went 2 hits → 0 (the plan estimated 3; the real count was 2, recorded as counted).

**Phase 2 (`525ece3`) — fragment links.** Issued URLs now carry `#t=`, which a browser never sends to
any server: a link pasted into WhatsApp/Telegram hands the platform's preview bot the EMPTY shell,
not a durable credential to biometric audio. `GET /` and `POST /api/claim` are the only
credential-free routes (the shell embeds nothing; the claim moves the fragment token into the
existing HttpOnly cookie, once). Sliding cookie on authenticated page loads. Deliberately NOT
single-use — the preview bot fetches before the human taps, so a one-shot link would be burned by
the bot. Legacy `?t=` stays until the same commit that enables public exposure. LIVE: bot's-eye
`GET /` → 200, 35,564 b shell, **no Set-Cookie**; claim → cookie → `/api/queue` 200 with 29 items.

**Phase 4 (`1efd8d4`) — restarts stopped eating spot-check scores.** Durable links made restarts
routine, and a check served before one became unanswerable after it (409 "already reviewed at the
desktop", score silently lost — the fail-before reproduced EXACTLY that). The served set now rides
the session file (plaintext by design — it is a list of which clips were handed out, not a
credential) and rehydrates at start. Undo stacks and leases deliberately NOT persisted, per plan.

**Phase 6 (`d1f9663`)** — 512px icon + manifest served by the router (public like the shell;
`start_url` is `/` and NEVER a token, pinned by test — an installed app must die with its revoke),
and Screen Wake Lock on play (inert on plain HTTP, alive on the coming ts.net URL). Service worker
deliberately OUT. LIVE: `/icon.png` 200 image/png 3,598 b; `/manifest.json` start_url "/".

**Phase 3 (`ee8a106`) — the watchdog, with my own first-draft bug caught before it shipped.** A dead
port with a live process has two meanings only `couch_session.json` distinguishes: file present =
wedged (kill + relaunch; flock clears the stale lock), file absent = the owner pressed Stop, and
killing a healthy app every 5 minutes would make Stop feel haunted — which is precisely what my
first draft did. Registered via schtasks (every 5 min, interactive-only; the CIM path is
access-denied unelevated). **Crash drill passed live:** force-killed the app → watchdog script →
same link serving 29 clips, watchdog.log showing "session expected but app not running -
relaunching". The task was DISABLED during the rebuild window — a watchdog launch mid-bundle is the
exact os-error-32 race hit twice yesterday — and re-enabled after.

**Resume proved twice more, the hard way:** the pre-phase-4 session file (old schema, no
spot_checks field) resumed under the new binary via serde(default) with the SAME token — the owner's
saved link survived a schema change, a rebuild, a force-kill, and a watchdog revival unchanged.

**Gates (verbatim):** `cargo test --lib` **1080 passed; 0 failed** (couch:: 30); clippy
`--all-targets -D warnings` clean (after factoring a `RememberedSession` type alias); fmt clean;
playwright couch-page **15 passed**; `npm run test:e2e` **62 passed**; python-policies **44/44**;
exe-freshness OK at `ee8a106`; `npm run tauri build` exit 0 WITH bundles this time (the watchdog
disable closed yesterday's race).

**NOT DONE / OWNER-GATED:**
- **Phase 5 is parked at a literal URL only the owner can click:** `tailscale serve` answered
  "Serve is not enabled on your tailnet" with an enablement link (surfaced in chat). After that:
  serve → verify over ts.net → funnel (its own approval) → remove legacy `?t=` in the same commit →
  Settings shows the ts.net URL. Until then, "send to someone with nothing installed" is DESIGNED
  and CODE-READY but NOT true.
- `cortex-once-admin.ps1` (fast startup, NIC power, active hours, disk timeout) needs one elevated
  run by the owner; auto-login is the owner's personal step by design.
- The watchdog's Stop-is-respected branch is reviewed but NOT drilled live — the drill would revoke
  the owner's real link (Stop deletes the session). Stated, not skipped silently.
- The reboot drill (full restart → link back within ~2 min) awaits a natural reboot; the crash drill
  stands in until then.
- Two Sorani strings still await native review; verify-10 still not run at today's HEADs.

---

## Iteration 203 — the "are you sure?" audit: 33 confirmed defects, 2 fixed, 1 false all-green corrected

**Trigger:** the owner asked whether the system was actually robust. Answering from memory would have
been worthless, so the answer was re-derived from the live machine and an adversarial audit.

**A claim of mine was wrong and is corrected here.** `test_restore_reservation_gate.py` was RED **at
HEAD**, and had been since durable sessions gave `couch::start` its `data_dir` parameter: the gate
asserts on the literal one-line body `start_on_port(db_path, reviewers, COUCH_PORT)`, which stopped
matching and made the gate *raise on every run instead of checking anything*. Iteration 202 reported
"python-policies 44/44". That report was false. Verified by `git show HEAD:...couch.rs | grep -c` =
**0**. Repaired to the real signature — kept exact rather than loosened to `"start_on_port(" in couch`,
which would pass even if `start` grew a real body (the exact bypass the gate exists to catch) — and
extended to `resume`, the other way into a running server and the one the app takes unattended at
every watchdog launch. Now genuinely **44/44**.

**Adversarial audit:** 8 undrilled failure modes (queue exhaustion, token revocation, lease collision,
DB contention, audio delivery, authz boundary, resume integrity, watchdog holes), each finding faced
3 independent refuters on distinct lenses. 149 agents, 47 raw findings, **33 confirmed**, 1 contested,
0 unjudged. The top findings were then re-verified by hand against the source — the agents were not
taken at face value, and one of their two "critical" DB-contention findings was correctly refuted on
reachability by its own skeptic.

**Fixed 1 — revoke was not durable (critical, security).** `save_session` had exactly two call sites
(start, batch-serve) and `revoke_in` was neither, so a revoked token stayed in `couch_session.json`
and `resume()` **re-issued the same token to the same name**: a lost phone regained access to Art. 9
audio at the next launch, which the watchdog performs unattended every 5 minutes. It was also
nondeterministic — the batch-serve save fires only when some *other* reviewer's batch serves a
brand-new spot check, so the same owner action sometimes stuck. Now persists under the
snapshot-under-lock / write-outside-lock shape; `save_session` returns `Result` so a failed write is
reported (access is already denied in memory, so this is not a rollback — it is "revoked" vs "revoked
until the next restart", and an owner revoking a lost phone must be told which). `resume` gained a
port-injected twin for the same reason `start` did. **Fail-before proven:** without the persist the
new test's `resume()` returns `["Hemn", "Sara"]`.

**Fixed 2 — reviewers capped at 25 clips and were told the corpus was finished (high).** `load()` had
two call sites, neither on batch drain, so the page went straight to "All clips reviewed!". Measured
against the real library: **116 pending**, so a reviewer did 25 and was lied to about 91. The page is
an installable standalone PWA with no address bar, so the only escape — a manual reload — was not
reachable either. `show()` now refetches; `exhausted` breaks the recursion and only an empty answer
draws the finished state. **Fail-before proven** (spec line 189). Test pins both halves, including
that it STOPS rather than spinning a phone radio on a drained corpus.

**ARSO (ops).** `cortex-once-admin.ps1` gained auto-sign-in-after-update-restart plus a read-only
BitLocker pre-boot probe. Measured first: `ARSOUserConsent` unset, `AutoAdminLogon=0`,
`HiberbootEnabled=1`, active hours unset. ARSO gives the necessarily interactive-only watchdog a
logged-on-but-LOCKED session after update reboots — recovered and still secure, with no password
stored. **Correction to iteration 202:** autologin was called "optional"; for full reboot coverage it
is mandatory, and ARSO covers only the update-initiated case. Nothing in this repo touches the
account password; autologin stays an owner step.

**Live verification after rebuild:** the pre-rebuild link still authenticates (same token, `Reviewer H`,
29 items = 25 work + 4 interleaved checks); the page served by the running binary is **byte-identical**
to the source asset (sha256 `ae21d65463c62aeb`, 37220 bytes both sides), which is the real proof the
`include_str!` fix shipped rather than a stale binary. Watchdog disabled across the build window and
re-enabled (Enabled, last result 0).

**Gates (verbatim):** `cargo test --lib` **1081 passed; 0 failed**; clippy `--all-targets -D warnings`
exit 0; `npm run test:e2e` **63 passed**; couch-page **16 passed**; python-policies **44/44** (for the
first time honestly); `svelte-check && tsc` 0 errors; exe-freshness OK at `487f736`; `npm run tauri
build` exit 0 with both bundles.

**NOT DONE — 31 confirmed findings remain unfixed.** Notably: the outbox deletes a reviewer's typed
correction on a 409 with no notification after having told them "Saved"; `held_by_others` skips the
un-leased remainder without counting it (the refetch hides this rather than correcting it); a clip
whose audio file is missing traps the reviewer with no skip, where both exits write a false verdict;
a stale persisted spot-check pair silently swallows a real review and answers 200; undo restores a
stale whole-row snapshot that clobbers a later desktop edit; and the watchdog's 5 s probe is shorter
than the server's own 10 s DB `busy_timeout`, so a healthy-but-busy app can be force-killed. None of
these are fixed and none should be assumed benign.

**Still owner-gated:** Tailscale Serve enablement (Phase 5, `tailscale serve status` = "No serve
config", so sharing outside the tailnet remains untrue); one elevated run of `cortex-once-admin.ps1`;
autologin if power-cut coverage is wanted. `loadingMore` joins `heldByOthers` and `linkExpired`
awaiting native Sorani review. verify-10 still not run at this HEAD.

---

## Iteration 204 — working the audit list: 20 of 33 findings closed, every fix fail-before proven

Continuation of iter 203's audit. Each fix below was proven by breaking it first and watching the new
test fail with the predicted symptom, then restoring; the fail-before output is quoted per item.

**The submit/outbox cluster — where the corpus was losing human labour.**
- *A busy database gave the clip away.* A failed write released the lease ("nothing was written, so
  hand it straight back"), so the next batch took it within seconds and the reviewer's outbox replay
  was refused 409 — the branch that discards the decision. Transient error, permanent loss. Driven by
  the real trigger: a second connection holding the write lock until `busy_timeout`. FAIL-BEFORE:
  holder `None` instead of `Sara`.
- *An interrupted decision was replayed whole.* One decision is two non-atomic writes; the retry guard
  keyed on `verified`, which only the SECOND sets, so it could not recognise its own half-written row.
  FAIL-BEFORE: learning pairs **1 -> 2** — a duplicate DPO pair from one human edit, plus a
  `correction_memory` hit_count bumped as though an independent segment had confirmed it, which with
  the self-confirmation would carry an unconfirmed memory to 0.667 and past BOTH firing gates. Made
  deterministic via the validation asymmetry the fault exploits (write one is a plain UPDATE; write
  two runs `validate_segment`, which rejects a UNC `audio_path`). Now the interrupted write is
  FINISHED, not repeated.
- *Attribution could be stolen.* `localStorage` is per-origin, not per-reviewer. Queued decisions now
  name their author and the server refuses a mismatch. Verified live on the running server:
  `409 this decision was made by Hemn, not Reviewer H`, with the clip left `verified=0`, `reviewed_by=NULL`.
- *"Saved" was a lie and the draft was deleted.* An offline save now says QUEUED. A refused replay
  still drops the queued decision (retrying cannot change a real answer) but KEEPS the typed text and
  raises a persistent banner.

**Undo and spot checks.**
- Undo restored a never-refreshed whole-row snapshot, erasing any later desktop edit. Now refuses 409
  naming who would lose work, and keeps the undo entry rather than consuming it on the refusal. The
  restore-failure branch also dropped the only copy of the pre-decision row; pushed back now.
- A persisted spot-check pair could outlive its answer key (un-verify or re-transcribe clears
  `verified`), after which a REAL review was graded against a key that no longer existed, answered
  200, and wrote nothing — permanently, every batch. Gated on `prev.verified`; stale pairs dropped.

**Leases.** `api_queue` leases the whole batch with ONE timestamp while the page heartbeat renewed
only the clip on screen — 36 seconds per clip at QUEUE_BATCH=25. The tail lapsed under the reviewer
mid-session. A renew now refreshes everything that reviewer holds. FAIL-BEFORE: `s1` holder `None`.

**The watchdog — five defects in the thing whose whole job is availability.**
- Single 5s probe against a server with a 10s `busy_timeout` and one accept thread per reviewer: a
  transport timeout read as "dead", so the BUSIER the reviewer the likelier a healthy app was
  force-killed mid-review. Now 3 attempts at 20s; worst-case detection 70s against a 300s repetition.
- Kill loop: a present session file does not mean couch CAN start (port taken, library moved), so it
  killed and relaunched into the same condition every 5 minutes forever. Capped at 3, then it stops
  and says the owner is needed; the counter resets when the server answers.
- Killed by process NAME, so a second checkout or an installed copy was fair game while the relaunch
  only ever started this one. Matched on full path now.
- A failed log write aborted the run BEFORE the kill/relaunch — the condition most likely to take the
  app down also disabled its healing.
- **Measured on the live task, not inferred:** `DisallowStartIfOnBatteries` and
  `StopIfGoingOnBatteries` were both **True**, which disables the watchdog whenever Windows believes
  it is on battery — a desktop behind a UPS included. Flipped on the running task and in `-Register`.

**The drained state.** Undo lived inside the card and vanished exactly when the reviewer wanted to
take back their last decision; the audio kept playing behind the empty state (forever with loop on);
the header read "Clip 26 of 25"; and "All clips reviewed!" was shown OVER unsent outbox work — telling
someone they were finished at the moment closing the page would have cost them everything. All four
closed. A clip whose audio will not load now offers a SKIP that writes nothing, instead of a trap
whose only two exits are accept (an unheard draft promoted to gold) or reject (a good clip excluded).

**Learning system, measured on the real library (owner asked whether corrections actually teach it):**
22 human edits -> 22 `agent_examples` pairs, 1:1, none lost, and those ARE used as few-shot exemplars
in the refine path. LOOP-0 correction memory IS wired into live transcription (`pipeline.rs`), but is
dormant: **101 memories, every one at confidence 0.500, hit_count 0, `last_fired_at` NULL on all 101**
— nothing has recurred across only 22 edits, and a new memory starts below `tau_conf` on purpose. DPO
is EXPORT-only; model improvement is a deliberate offline retrain, not automatic. Reviewing the 116
pending clips is what starts the memory loop.

**Gates (verbatim):** `cargo test --lib` **1087 passed; 0 failed** (couch:: 37); clippy
`--all-targets -D warnings` exit 0; fmt clean; `npm run test:e2e` **68 passed**; couch-page **21**;
python-policies 44/44; `npm run tauri build` exit 0 with both bundles; exe verified at HEAD after each
rebuild, and the pre-rebuild link re-authenticated every time (same token, 29 items).

**NOT DONE.** ~13 findings remain, now mostly medium/low: torn `couch_session.json` write from
concurrent accept threads (refuted 2/3 on reachability, asymmetry real); a couch thread that cannot
open the DB exits leaving 8737 bound with no responder; undo history is process-local so a relaunch
strands the last decision; `held_by_others` still does not count the un-leased remainder (the refetch
hides this rather than corrects it); no spinner before the first queue resolves. Owner-gated as
before: Tailscale Serve enablement, one elevated `cortex-once-admin.ps1` run, autologin if power-cut
coverage is wanted. **Eight** Sorani strings now await native review. verify-10 still not run at HEAD.

**Addendum to iter 204 — three more closed.**
- *A couch thread that could not open the library left port 8737 bound and mute.* Its only recourse
  was `return`, so connections landed in the listen backlog and were never answered: every phone hung
  instead of failing fast, and the watchdog saw an unreachable port that restarting could not fix. The
  library is now opened BEFORE anything binds, turning it into an honest error on Start, and a
  per-thread failure signals shutdown rather than leaving a socket nobody serves.
- *`save_session` wrote through one fixed temp filename from any accept thread, unlocked.* Refuted 2/3
  on reachability and shipped anyway as cheap insurance: an unparseable session file is unrecoverable
  (load_session gives up silently, resume returns None, every link already sent is dead). Unique temp
  name per write, plus a concurrency test — 8 threads x 10 saves with deliberately different payload
  lengths, asserting the promoted file always parses and no temp files accumulate. Confirmed stable
  5-for-5 standalone before shipping, per the known Windows FS write-then-read flakiness.
- *Offline decisions replaying under whichever reviewer opened the browser next* was closed by the
  attribution stamp above; counted once, not twice.

**Gates after the addendum:** `cargo test --lib` **1088 passed; 0 failed** (couch:: 38); clippy exit 0;
fmt clean; `npm run test:e2e` **68 passed**; couch-page **21**; python-policies 44/44.

**Second addendum to iter 204 — the blank first screen.** Both panels start hidden, so until the first
`/api/queue` resolved the reviewer saw nothing at all. On a phone on weak signal — the normal case for
this surface — a blank screen is indistinguishable from a dead link, at the moment someone is most
likely to give up and message the owner. It now says it is loading, reusing the refill string (retitled
"Loading clips…" so it reads correctly in both places rather than adding a ninth unreviewed Sorani
string). couch-page **22 passed**; python-policies 44/44.

**Judged NOT worth changing, with reasons, rather than left as silent debt:**
- `held_by_others` skipping the un-leased remainder is now COSMETIC. The empty state only renders when
  the server returns zero items, and at that point the remainder is zero by construction — the refetch
  closed the user-visible half. The counter is still semantically wrong and is left recorded here.
- A spot-check answer queued across Stop/Start is still lost. Stop is a deliberate full revoke that
  clears the session, so a new session legitimately knows nothing about the old check; the reviewer is
  now TOLD (refused-decision banner) instead of losing it silently, which was the actual defect.
- Undo history dying with the process is benign in practice: the button is hidden while
  `doneThisSession` is 0, so a relaunched page does not offer an undo it cannot honour.

---

## Iteration 205 — sherpa-onnx LM fusion: NOT available. LOOP-0: measured, and it is inert by design.

Both items the owner asked for, and both overturned a recommendation I had made in chat.

**LM fusion is NOT reachable on this stack — my "biggest gap" advice was wrong.** Verified against the
vendored crate source, not documentation:
- `sherpa-onnx 1.13.2`, and `asr.rs:315` builds `OfflineRecognizerConfig { decoding_method: "greedy_search" }`
  around `OfflineOmnilingualAsrCtcModelConfig`.
- `OfflineLMConfig` EXISTS but is `{ model: Option<String>, scale: f32 }` — a neural ONNX LM for
  transducer beam search, not an n-gram, and not wired to the CTC path.
- The n-gram/HLG route is `CtcFstDecoderConfig`, and in 1.13.2 it exists **only** as
  `OnlineCtcFstDecoderConfig` (streaming) in both the safe crate and `sherpa-onnx-sys`. There is no
  offline equivalent to bind.
So "build a KenLM from the corrections and fuse it into decoding" is not a small change here; it needs
a different decode path (e.g. an offline re-transcribe pass outside sherpa), not a config flag.
Also found and previously unnoticed: `hotwords_file`/`hotwords_score`, `rule_fsts`/`rule_fars`,
`blank_penalty`, and an `hr` homophone-replacer are all exposed on the offline config. Whether hotwords
do anything for a CTC model is UNVERIFIED and must be measured before being claimed.

**`src-tauri/src/bin/loop0_eval.rs` (new) — the correction layer is now measured.** Read-only, opens
the library read-only on purpose, and scores raw ASR against human gold before and after firing.
**Leave-one-out is enforced**: every memory was extracted FROM some segment, so a memory is excluded
from the clip it came from — otherwise every number is self-confirmation.

Measured on the real library (26 scorable clips, 101 memories, mean CER of the raw draft **0.0562** —
this library's own verified clips, NOT the frozen eval set, so it is not comparable to the 7.03%
champion figure):

| rule | clips rewritten | improved | worsened | mean CER after | verdict |
|---|---|---|---|---|---|
| ARMED (shipped gates) | 0 / 26 | 0 | 0 | 0.0562 | INERT |
| gates bypassed, exact context | 0 / 26 | 0 | 0 | 0.0562 | INERT |
| either neighbour | 0 / 26 | 0 | 0 | 0.0562 | INERT |
| neighbours ignored | **26 / 26** | **0** | **26** | **0.1383** | **HARMFUL (+0.0820)** |

**What this proves, and it is not what I expected.** The confidence/hit gates are NOT what keeps the
layer quiet — bypassing them changes nothing. The `slot_key` is `"{left}|{right}"` and the firing rule
demands an EXACT bigram-context match, so a memory can only fire when the same two neighbouring words
recur; at 764 corrected words that never happens out-of-sample. And loosening it is not the fix:
dropping the context requirement fires on every clip and makes **every one of them worse**, more than
doubling CER. The exact-context rule is not over-caution — it is the thing protecting the corpus from a
pool of one-off substitutions that do not generalise.

`ContextMode` was added to `FiringConfig` (default `Exact`, **zero behaviour change**) purely so those
alternatives could be measured through the REAL firing function instead of a reimplementation of it —
the repo already learned that lesson with `revoke_in`. A unit test pins the default and quotes these
numbers, so it cannot drift loose without someone deleting an assertion that says why.

**Honest conclusion for the owner's question "does the app learn well?":** the capture layer is
excellent and the few-shot path genuinely uses corrections. The LOOP-0 symbolic layer contributes
exactly nothing today and cannot be switched on without harming the corpus. The real lever is acoustic
fine-tuning on the accumulated pairs — not this.

**Process defect found in my own gates:** `cargo clippy ... | tail -N && echo OK` masks clippy's exit
code behind `tail`'s, so a failing clippy printed "CLIPPY OK". Caught when 3 real lint errors surfaced
in this iteration's test code. Re-verified with unmasked exit codes: clippy 0, fmt 0, `cargo test --lib`
0 with **1089 passed**.

---

## Iteration 206 — verify-10 is GREEN at the merge commit, for the first time since 7d77fb5

```
VERDICT: GREEN - PERSONAL-USE SHIP-READY. (Not full-charter 10/10: 8 legs owner-descoped, 5 owner-gated pending.)
VERIFY-10 EXIT=0
kept gates run: 23 - 23 PASS, 0 FAIL, 0 skipped (env/not-built)
```

Reproduced twice back to back at `aa9ce42`, the second run regenerating `docs/STATUS.md`.

**Nothing was fixed to reach green — the gate had never been given its input.** `real-app-e2e` had
been the sole blocker, and its own skip probe said why in one line: `set CORTEX_AUDIO=<absolute wav
path> to drive the real app`. Pointed at the committed `fleurs_ckb_sample.wav`, it passes in ~18s,
driving the real binary end to end and emitting a real Sorani transcript (77 chars, 1 segment). The
driver defaults to a disposable temp profile and REFUSES `%APPDATA%\cortex-speech`, checked before
running it against the owner's machine. `CORTEX_OUT` was pointed at scratch so `run.jsonl` and the
debug log never touch the tree — the sweep left the repo at 0 changes.

**The intermediate RED was harness contention, and the claim was tested rather than asserted.** Run 2
failed `test-e2e+a11y` with **empty stdout, exit 1, 24.7s** — Playwright never reported a single test,
so nothing asserted and failed. Standalone immediately after: exit 0, **69 passed, 17.2s**; port 1420
free; no stray dev server. Re-running the sweep was the test of "this is contention, not a defect",
and it passed at 19.1s and again at the status run. A no-output failure flanked by three clean passes
is the harness, not the suite. It is recorded here rather than silently re-rolled.

**Real timings, so this reads as the full sweep it was:** test-rust 379.4s; fuzz-smoke 281.9s (5
targets, 0 crashes); egress-runtime 23.1s (zero outbound TCP across a REAL offline transcription, with
the in-run positive control); ignored-real-model 30.8s; refinery-lift 38.0s; rtf-bench 17.9s;
real-app-e2e 18.2s.

**`docs/STATUS.md` regenerated: `37769fa` -> `aa9ce42`, a one-line diff.** The old file already said
GREEN — attributed to a commit that is not what shipped, which is the precise failure the file's own
header warns about. It is generated FROM the run so a doc can never assert a gate state no run
produced; the single-line diff also confirms it is deterministic per commit as documented.

**GREEN IS NOT 10/10, and the script refuses to say otherwise.** 8 legs owner-descoped by the
2026-07-10 amendment (installer signing, SLSA, updater, stores, HF card, macOS notarization,
Scorecard, signed tags) and 5 owner-gated pending: iaa-kappa-ceiling (>=2 independent Sorani
annotators), cordi-dialect-fairness, refinery-lift-in-product (Gold Marathon, >=500 real review
decisions — the corpus stands at 28), branch-protection, asosoft-600-licensing.

**Outside the gate entirely and still true:** Tailscale Serve is not enabled, so sharing outside the
tailnet does not work; `cortex-once-admin.ps1` has not had its elevated run, so FastStartup=1 and
ARSO=unset and a reboot still leaves the system down; eight Sorani strings await native review.

---

## Iteration 207 — hotwords on CTC: measured CLOSED, and the per-stream call is process-fatal

The one unverified lever left from the iter-205 crate inspection ("whether hotwords do anything for a
CTC model is UNVERIFIED") is now measured, on the real model and the committed FLEURS fixture.

**New ignored gate `asr::tests::hotwords_are_refused_on_the_offline_ctc_path`.** Probe design: decode
the fixture with an absurd bias (score 50) toward a Sorani word the baseline does not emit. Measured
outcomes (sherpa-onnx 1.13.2, CTC-300M int8, CPU):

- config `hotwords_file` + greedy_search: **construction REFUSED** — upstream Validate says "Please
  use --decoding-method=modified_beam_search if you provide --hotwords-file".
- `create_stream_with_hotwords` on the CTC recognizer: **HARD PROCESS ABORT**, exit 0xffffffff,
  "Only transducer models support contextual biasing" (offline-recognizer-impl.h:38). A C++ abort is
  uncatchable from Rust: ONE call to this crate-public method kills the entire app.

So contextual biasing is closed on this stack even more definitively than LM fusion: the knobs sit on
the shared config struct, and the CTC implementation refuses or aborts. The test pins the refusal at
runtime and pins the abort by SOURCE SCAN — zero call sites of `create_stream_with_hotwords` allowed
anywhere in the crate — because a runtime probe of an abort would kill the test runner itself.

Honesty note on process: the first draft of this test pinned my EXPECTED outcome ("silently ignored")
before running. Reality was refusal + abort; the pin was rewritten to what was measured, and the
failing first run is what caught it. The improvement avenue list for the owner is now, with all three
decode-side levers measured dead (LM fusion, LOOP-0 promotion, hotwords): review volume -> acoustic
fine-tune. There is no decode-side shortcut on this stack.

**Gates (unmasked exits):** fmt 0; clippy 0; `cargo test --lib` 0 — **1089 passed, 7 ignored** (the
new probe is the 7th). Exe rebuild required by the freshness gate (asr.rs changed, test-only) and run
after commit.

---

## Iteration 208 — the session-shaped soak: a real defect found the night before real use

The owner starts reviewing tomorrow with 116 pending clips. Every existing soak either capped its
rounds (the three-reviewer test stops at 4) or used a backlog smaller than QUEUE_BATCH, so "the queue
actually reaches zero" had never been proven — and both defects that shaped this surface lived PAST
the first batch. So the highest-value thing to harden was the exact shape of tomorrow's session.

**New `one_reviewer_can_drain_a_backlog_larger_than_a_batch_to_genuine_zero`:** 130 clips, one
reviewer, real HTTP against real threads and a real SQLite file, an UNBOUNDED round loop (a capped one
cannot tell "drained" from "gave up"), asserting every clip decided exactly once, nothing handed out
twice, nothing stranded, and the server itself eventually answering with an empty queue. Measured:
**130 clips over 7 rounds, all verified, zero duplicates.**

**It failed on the first run at clip 73 with HTTP 429** — and the interesting part was not the test.
`COUCH_RATE_LIMITER` is 120/min per reviewer with a 60 burst, keyed by reviewer and covering EVERY
endpoint. A machine-speed drain reaches it, which is a test artifact. But it exposed a genuine client
defect: **the page treated 429 as a permanent verdict.** 429 is < 500 and not 401, so in `decide()`
the decision was dropped and the reviewer was left stranded re-submitting the same clip, and in
`flushOutbox` a throttled replay was discarded. 429 is the canonical "later, not no" — the one status
that must always be retried. A reviewer moving fast through obvious clips spends three requests each
(audio, prefetch, decision), so this was reachable in a real session, at exactly the moment they were
working fastest.

Fixed at both sites: 429 now joins undefined/401/5xx in the hold-and-retry set. Playwright pin
`a throttled decision is held for retry, not thrown away` — FAIL-BEFORE: outbox length 0 (decision
destroyed) versus 1 with the fix; it also asserts the reviewer is advanced rather than stranded, is
told "queued" rather than "saved", and that throttling is NOT recorded as a refused decision, because
it is not a verdict. The soak now backs off on 429 rather than asserting it away, so it proves the
drain completes UNDER throttling.

Process note: a `replace_all` on a comment line matched inside a deeper-indented copy of itself
(the 6-space string is a substring of the 8-space line) and mangled the indentation in `flushOutbox`.
Caught by re-reading the file, repaired before any gate ran.

**Gates (unmasked exits):** fmt 0; clippy 0; `cargo test --lib` 0 — **1090 passed, 7 ignored**;
`npm run test:e2e` 0 — **70 passed**; couch-page **23**; python-policies 0 — 44/44.

**Iteration 208b — pre-flight on the real data, and an unbounded undo stack.**

*Data pre-flight for tomorrow (measured, read-only):* all **116 pending clips** have their audio
present on disk, from a single source file, with valid durations — 0 missing, 0 empty `audio_path`,
0 zero-duration. Worth checking because the whole corpus carries the extended-length `\?\C:\...`
prefix, and `validate_segment` calls `reject_unc_path` — the exact rule the interrupted-write test
uses to FORCE a failure. It is not tripped: that shape parses as `VerbatimDisk`, not UNC, and
`input.rs:269` already pins it ("verbatim-disk local path must pass"). No new test added — the
existing pin covers it, and a duplicate would be noise. Verified rather than assumed, because a naive
future tightening of that check would make every clip in the library undecidable.

*Defect: the undo stack was unbounded.* Every decision pushed a full `SpeechSegment` clone and nothing
ever trimmed it, so a 116-clip session retained 116 whole rows for the life of a process the watchdog
keeps alive for weeks across many sessions. Capped at `UNDO_DEPTH = 20`, trimming the OLDEST — the ↩
button always reaches for the most recent decision, so trimming the wrong end would break the one
thing undo is for. Bounded at the single growth site; the pushes in `api_undo` restore an
already-popped entry and cannot exceed what the stack held. FAIL-BEFORE: depth **27 vs 20**. The test
also pins that the newest decision is still undoable and that a trimmed entry leaves its decision
intact rather than corrupting anything.

**Gates (unmasked):** fmt 0; clippy 0; `cargo test --lib` 0 — **1091 passed, 7 ignored**;
python-policies 0 — 44/44.

---

## Iteration 209 — a restart drill that found nothing, and a false banner of my own making

**Pinned, no defect: a mid-session server restart.** The watchdog can force-restart this app at any
5-minute tick — that is its job — so a real session WILL sometimes be interrupted mid-batch. Existing
tests covered a restart BETWEEN sessions; none covered one DURING a session with a phone still working
from the queue it already holds. New `a_mid_session_server_restart_loses_no_work_and_double_decides_
nothing`: 40 clips, one batch handed out, five decided, then threads down / server dropped / a FRESH
CouchState on a new port against the SAME database (so in-memory leases and undo are lost exactly as a
process restart loses them). Every remaining clip still lands, attribution survives, and a replay of a
pre-restart decision is answered as already-done with **no second learning pair**. It passed first try
— a regression pin, not a fix, and recorded as such.

**Defect, and it was mine.** The refused-decisions banner added earlier today had no way down:
`noteRefused` only ever appended, and nothing removed. One 409 pinned the warning for the life of the
browser profile — still insisting work had failed after the reviewer went back and re-reviewed it,
which is the same class of lie the banner exists to prevent, aimed the other way. `clearRefused` now
retracts an id when its decision lands, on both the live-submit and outbox-flush paths. It compares
`#err`'s text against the refused template computed BEFORE the removal, so it only ever retracts its
OWN banner: `#err` is shared with the link-expired notice, which is more urgent and needs an action
from the reviewer. Two Playwright pins, including one that asserts the link notice survives a
retraction.

**A test I wrote last iteration was wrong, and the fix is a finding in itself.** The audio-skip test
asserted `#skip` hidden after skipping. On `file://` the page's `<audio src="/api/audio/...">` is a
REAL request — `window.fetch` is stubbed, media loads are not — so every clip's audio genuinely fails
and the handler correctly fires for the next clip too. The assertion was racing correct behaviour. The
reset is now asserted in the SAME TICK as `show()` (synchronous clear, async error event), which is
deterministic. Confirmed stable 3-for-3.

**Gates (unmasked):** `cargo test --lib` 0 — **1092 passed, 7 ignored** (couch:: 41); clippy 0;
`cargo fmt --check` 0 (after an initial exit 1 — rustfmt reflowed two of the new test lines);
`npm run test:e2e` 0 — **72 passed**; couch-page **25**, stable 3-for-3; python-policies 0 — 44/44.

**Iteration 209b — the gap my own 429 fix opened.** Self-audit, which the loop now mandates because
two of the previous three findings were in changes made earlier the same night.

Making 429 a HOLD instead of a drop created a dependency nothing satisfied: `flushOutbox` ran on
exactly two triggers — the `online` event and `load()`. **Throttling never fires `online`** (the phone
was never offline), so a rate-limited decision sat in localStorage until the batch happened to drain,
up to a whole batch later, while the reviewer watched a "not sent yet" counter. Work was never lost —
localStorage outlives the page — but "queued" has to mean it actually goes.

A 30-second retry timer now drains the outbox unaided, deliberately NOT folded into the 4-minute lease
heartbeat: the limiter refills at 120/min so a throttle clears in seconds, and unlike `renewLease` this
runs while the page is HIDDEN — a backgrounded phone finishing its sends is exactly the case where a
reviewer has walked away believing they were done. No cost when the outbox is empty.

Pinned with Playwright fake timers (`page.clock`), asserting the drain happens with NO reload, NO
`online` event and NO batch drain — the timer is the only thing that could have done it. FAIL-BEFORE:
outbox stays at **1** with the timer removed, **0** with it. Stable 3-for-3.

**Gates (unmasked):** `npm run test:e2e` 0 — **73 passed**; couch-page **26**, stable 3-for-3;
python-policies 0 — 44/44.

**Iteration 210 — the retry timer's own second-order defect.** Self-audit again, and again it was the
change from the previous iteration.

`flushOutbox` had no re-entrancy guard, and after the 30s retry timer landed there were THREE callers:
the `online` event, `load()`, and the timer. An overlapping run re-POSTs items another is already
sending. The server's dedup guard means nothing corrupts — but every duplicate spends a
COUCH_RATE_LIMITER token, which is perverse: throttling is the entire reason that timer exists, so
re-entrancy made being throttled worse, and could sustain it.

Guarded with an in-flight flag, added as a thin wrapper (`flushOutbox` -> `flushOutboxOnce`) so the
loop body stays untouched and the diff stays reviewable. FAIL-BEFORE, measured against a deliberately
slow server with every trigger fired at once: **4 identical POSTs for one decision** without the guard,
1 with it. Stable 3-for-3.

This is the fourth defect in a row found in code written earlier in the same loop, and the third that
was a second-order consequence of the previous fix (429-drop -> unflushed outbox -> re-entrant flush).
Recorded because the pattern is the useful part: each fix moved the failure one layer out rather than
removing it, and only running the system exposed the next layer. Re-reading code has found nothing all
night.

**Gates (unmasked):** page script parses clean (node Function check); `npm run test:e2e` 0 —
**74 passed**; couch-page **27**, stable 3-for-3; python-policies 0 — 44/44.

---

## Iteration 211 — hunt widened to untouched code: three probes, three clean

The second-order chain from tonight's own changes terminated last iteration, so this one probed areas
NOT touched all night. Reporting the null results, because "we looked and found nothing" is only worth
anything if it is said as plainly as a defect would be.

**Audio serving under a long session — MEASURED, fine.** The library's single source file is
**172,764,670 bytes**, and a session costs ~2 audio requests per clip (the player plus the next-clip
prefetch), so ~232 fetches. Timed against the live server: **~83ms each** (390–472 KB payloads), so the
decode seeks rather than reading the file — about 19 seconds of audio serving across the whole 116-clip
session. No defect, no change made.

**Reloading mid-batch — PINNED, no defect.** The most common thing a phone reviewer does (a stutter, a
rotation, a background cycle, a pull-to-refresh) and nothing covered it. Two quiet failures were
possible: a FRESH batch, abandoning the in-progress work leases exist to protect, or a clip already
decided coming back, asking the reviewer to judge the same audio twice. New
`reloading_mid_batch_returns_the_remainder_and_never_a_decided_clip`: 60 clips, batch out, five decided,
re-fetch — no decided clip returns and the whole undecided remainder is still theirs. Passed first try.

**Desktop and phone at once — PINNED, no defect.** The owner uses both surfaces, so a clip the phone
holds can be decided at the desktop meanwhile; the phone's late submit then arrives against a row that
already carries a judgement. This leans on the LATE-SUBMIT guard, not the collision guard, because the
desktop attributes with annotator None. New
`a_desktop_decision_is_never_silently_overwritten_by_the_phone` asserts 409, a message naming the
desktop, and the desktop's correction intact. Passed first try.

**The honest signal from tonight as a whole:** five defects, all five in code written earlier the same
night, three of them second-order consequences of the previous fix. Three probes into code NOT touched
tonight found nothing. New code is where the defects were, which is why the loop audits its own diffs
first — and it is also why these three null results are worth trusting rather than treating as
insufficient effort.

**Gates (unmasked):** `cargo test --lib` 0 — **1094 passed, 7 ignored**; clippy 0; `cargo fmt --check`
0; python-policies 0 — 44/44.

---

## Iteration 212 — the watchdog finally has a test, including the branch that could never be drilled

The watchdog is the entire availability story — the only thing that brings the review server back after
a crash, a wedge or a reboot — and it had **no test of any kind**. Its most dangerous branch force-kills
the app, and the branch that must LEAVE A HEALTHY APP ALONE (the owner pressed Stop) had been reviewed
but never verified. It could not be: proving it for real means pressing Stop, which deletes
`couch_session.json` and revokes the owner's live link. That is why it stayed "reviewed but not drilled"
across several iterations, stated each time rather than quietly dropped.

**Made testable, then tested.** `cortex-watchdog.ps1` gained `-DryRun` (decide and REPORT
`WATCHDOG-ACTION: ...`, kill and launch nothing) plus `CORTEX_WATCHDOG_DATA_DIR` /
`CORTEX_WATCHDOG_PORT` overrides. Production behaviour is unchanged when neither is set. The
`$env:APPDATA` hardcodes in the session-file and dead-man-ping paths now route through `$dataDir`, so a
drill cannot touch the real profile.

**`scripts/test_watchdog_decisions.py` (new, auto-discovered by the policy runner: 44 -> 45 scripts).**
Runs against a throwaway data dir and a port nothing is listening on, so it is safe **while the owner is
mid-review** — and it was: the real app was up and serving throughout. Measured:

```
OK   no session + app running -> must NOT touch it: leave-alone (deliberate Stop)
OK   session + app running -> kill and relaunch:    kill-and-relaunch (attempt 1/3)
OK   session + 3 prior kills -> give up:            give-up (kill cap reached)
```

FAIL-BEFORE: breaking the Stop branch the way the original draft had it produces
`launch (no session, not running)` and the drill FAILS. That is precisely the bug caught by eye in
review weeks ago — an app resurrected every 5 minutes after a deliberate Stop — now caught by a gate
instead of by luck.

Two branches are honestly reported as SKIP rather than silently passed when the real app is not running
(the script matches by exe PATH, which a drill cannot fake), and the live-port leg is deliberately not
drilled: an accept-only socket is exactly the wedged case the 3x20s probe exists to wait out, so
asserting it would cost a minute of real sleep per gate run, while the owner's own server proves the
alive path on every real 5-minute tick.

**Gates (unmasked):** watchdog parses clean; python-policies 0 — **45/45**; hygiene 0.

---

## Iteration 213 — the Reviewer UX 10/10 plan, researched and adversarially verified

Owner directive: Tailscale may stay on the PC, but reviewers must get "true best user experience,
Apple-quality smoothness" — write the #1 plan to a genuine 10/10.

**docs/REVIEWER_UX_10_PLAN.md (new).** Built by a 10-agent workflow: full line-cited inventories of
couch.html and couch.rs, extraction of settled decisions (no service worker, Funnel-over-Cloudflare,
fragment tokens, phase-6 scope cap), 2026 platform research with sources, a three-lens design panel
(first 60 seconds / flow state / trust & recovery), and an adversarial verify pass over both the
platform claims and the proposals themselves.

**The verify pass earned its cost:** it killed two duplicate proposals (two timeout/retry designs in
the same batch), exposed two fake gates (a drill that cannot execute with the server down because the
shell itself is served by the app; a swipe network-gate the unmodified page already passes), rejected
undo-survives-reload as specified (it corrupts the shared pace counter and conflates three distinct
undo-409 meanings), and — the standout — **found a real defect in the shipped page**: the fragment->
cookie claim is a one-shot const promise, so a FIRST-EVER visitor whose claim POST fails transiently
(server restarting, cellular blip) is shown "link expired" — a false terminal state for a valid link —
with no recovery except a manual reload an installed app cannot perform. Scheduled as R3.2.

**Verified platform facts the plan rests on (each with a source):** Safari probes media with a 2-byte
range request and expects 206 (today couch.rs ignores Range — playback measured working, scrubbing
contract not met); iOS never shipped navigator.vibrate and the checkbox-switch haptics hack was patched
away in 26.5 (haptics DEAD, dropped); Web Audio routes through the silent switch while <audio> ignores
it (Web Audio playback REJECTED); iOS does NOT share cookies between Safari and an installed standalone
app (install-nudge trap, device-test scheduled before the nudge ships); wake lock works from iOS 16.4,
fixed in standalone 18.4; iOS 26 opens A2HS sites as web apps by default.

**Shape:** an operational definition of 10/10 (N1 link-tap->audio <=15s/2 taps; N2 ONE tap per clip;
N3 <=1s to next clip; N4 zero loss visible; N5 stranger completes first clip unaided), then R0 Funnel
go-live (owner click) -> R1 transport (Range/206+HEAD, immutable caching+ETag, server byte-cache,
single-fetch client buffer, pendingTotal, limiter-doc truth-fix) -> R2 one-tap flow (welcome gate as
iOS audio unlock, autoplay-next, pause-on-edit+rew2s, keyboard-safe row via visualViewport, progress
bar, safe-area) -> R3 visible trust (sync pill, the claim-retry defect fix, readable failures, a11y
floor, swipe snap-back) -> R4 install & polish (owner-gated nudge + the cookie trap test, refused-
banner follow-through, "unsure" as an owner data-policy question) -> R5 the real-device hour over
Funnel with every number logged. Explicit rejected/descoped section so nothing is silently relitigated.
Hard gates no code can substitute: native Sorani review of every source:null string, and real-device
measurement — nothing claimable from emulators.

Two truth items folded in: the couch limiter comment says 120/min but the bucket refills ~120/second
(verified in throttle.rs — doc fix scheduled, behavior kept), and the batch-lease gap the inventory
listed was already closed server-side in iteration 208's whole-batch renew.

---

## Iteration 214 — R3.2: the false "link expired" dead end, fixed

First item built from `docs/REVIEWER_UX_10_PLAN.md`. The defect was found by the plan's own adversarial
verify pass, not by a test — and it was live in the shipped page.

**The defect.** The fragment->cookie claim was a one-shot `const` promise created at script parse:
`const claimed = fetch('/api/claim'...).then(r => r.ok, () => false)`. Any failure — server restarting,
a cellular blip on the very first tap — resolved it `false` forever. The queue fetch then 401'd and the
page told the reviewer **the link had expired**. False, and unrecoverable: `history.replaceState` has
already stripped the fragment, so even a manual reload has no token, and an installed standalone app has
no address bar to reload from. The only escape was finding the original chat message again. The token
sat in page memory the entire time.

**Fixed.** `ensureClaimed()` is a state machine — `none | pending | ok | rejected` — where **only an
explicit 401 from the claim itself** is a verdict; a network error or 5xx stays `pending` and the next
`load()` re-claims. The 401-means-expired test now also requires `claimState !== 'pending'`, so a race
is never read as an expiry.

**Also landed (same dead-end family, all from plan R3.2):**
- `api()` gained a 15s AbortController timeout. A stalled cellular fetch used to hang "Loading clips…"
  forever with no timeout anywhere in the file; an abort throws with no status, which every caller
  already treats as retryable.
- A localized **Retry** button (`#retry`) in every non-verdict failure state, plus a gentle 60s
  auto-retry while unreachable. `retry` is COPIED from the desktop `retry` key, so it is native-reviewed
  Sorani — `source: "retry"`, NOT a new owner-gated draft.
- Retrying a real verdict would be a lie, so the button is hidden when the claim was genuinely refused.

**A regression I introduced and had to fix properly.** Clearing `#err` on a successful load and hiding
`#done` on failure broke two existing tests — correctly. `#err` is shared by three messages with
different lifetimes (load failure, link-expired verdict, refused-work banner), and my blunt clear wiped
the refused banner on the next refetch. Introduced `errKind` (`'' | load | expired | refused`) with
`showErr`/`clearErr`, so no path can retract a message it did not write; link-expired now outranks the
refused banner explicitly. This also replaced the fragile `textContent`-comparison hack `clearRefused`
had been using — a named state was what that code always wanted.

**One pre-existing test adjusted, and why that is not weakening it.** "the empty state is actually empty"
drove `show()` with an empty queue, which now correctly triggers a refill whose fetch fails on
`file://` — and the failure path takes down the "Loading clips…" placeholder, which IS the dead-end fix.
Its stated subject is that `[hidden]` beats `display:flex`, so it now sets `exhausted = true` to reach
the true empty state directly instead of relying on a placeholder that used to sit there forever.

**FAIL-BEFORE:** restoring the one-shot claim makes the new test fail with the page showing
`"ئەم بەستەرە بەسەرچووە..."` — this link has expired — to a reviewer holding a valid link.

**Gates (unmasked):** page script parses clean; couch-page **29 passed, stable 3-for-3**;
`npm run test:e2e` 0 — **76 passed**; python-policies 0 — 45/45; typecheck 0 — 426 files, 0 errors.

## Iteration 215 — R1: transport. Ranges, immutable caching, a clip cache, honest totals

**Plan item R1** of `docs/REVIEWER_UX_10_PLAN.md`, items 1/2/3/5/6. Item 4 deliberately not shipped —
see the plan's R1 status block for why, and for a correction to the plan's own rationale.

Every `/api/audio` reply carried exactly one header — `Content-Type` — verified live against the
owner's running server before touching anything:

```
audio_status=200 bytes=390060 ctype=audio/wav
audio_headers=Content-Length,Content-Type,Date,Server
```

No `Accept-Ranges`, no `ETag`, no `Cache-Control`. So a `Range` request was answered with an
unconditional full body (a client that trusts 206 then reads the wrong offsets), `HEAD` fell through
to 404 while the `GET` beside it worked, and every replay of a clip a reviewer had already heard cost
the whole 300–500 KB again over cellular.

**What landed.** `Reply` grew a fourth slot for response headers (every other route passes `vec![]`).
`/api/audio` now answers single-range `bytes=` requests with 206 + `Content-Range`, refuses
unsatisfiable ones with 416 + `bytes */len` rather than a misleading 200, advertises `Accept-Ranges`,
ships `Cache-Control: private, max-age=31536000, immutable` + a strong `ETag`, answers a matching
`If-None-Match` with 304, and is reachable by `HEAD` as well as `GET`. `private` is not decoration:
this is biometric audio under GDPR Art. 9 and must never sit in a shared cache.

The ETag is derived from a fingerprint of everything that determines the bytes (id, audio path,
alignment, duration) rather than from the bytes themselves — which is what makes a 304 free, since it
can be answered without materialising anything. The same fingerprint keys a 32 MB byte cache, so a
re-alignment naturally invalidates both at once.

**A finding that made the byte cache more valuable than the plan claimed, for a different reason.**
The plan said each request "re-decodes and re-slices the source file". Reading `audio.rs` rather than
trusting that: the decode *is* already LRU-cached. But `pcm_cache_key` opens the source file and
blake3-hashes **all of it** before the cache can be consulted, and a hit then does `cached.clone()` on
the entire decoded PCM. So every single `/api/audio` request re-read the whole source (172 MB on the
owner's corpus), memcpy'd ~172 MB more, then re-sliced and re-encoded the WAV sample by sample. The
plan's conclusion was right; its reason was wrong, and the real reason is worse.

**Truth fix (item 6), comment-only, behaviour unchanged.** The limiter doc said "120/min sustained
with a burst of 60". `throttle.rs` computes `tokens += elapsed.as_secs_f64() * rate`, so the rate is
per **second** — the comment was wrong by 60×, in three places. Corrected, and corrected honestly:
120/s does *not* throttle a merely chatty page, it bounds a machine-speed runaway. The existing soak
test's own measurement confirms it (429 first appears at clip 73, which is where a ~0.4-token-per-
request deficit exhausts a 60-token burst — consistent with per-second, not per-minute).

**`pendingTotal` (item 5)** now travels with the queue, and the page counts against the backlog
instead of the batch. "Clip 7 of 25" was true of the clips in hand and useless as progress: the server
hands out at most 25, so a reviewer working a long corpus watched that denominator fill and reset with
no way to tell nearly-done from barely-started. No new Sorani string — the existing native-reviewed
`progress` key just gets an honest denominator.

**Self-audit of this loop's own diffs, reported honestly.** The `if (loading) return` guard I added in
iteration 214 resolves `await load()` **without loading** — and two callers await it for its data:
`refill()` (drained batch) and undo (needs to find the restored clip). Replaced with promise
coalescing so concurrent callers join the in-flight load instead of skipping it. **Null result, stated
plainly: I could not construct a user-reachable path to it.** Every concurrent trigger is gated behind
`retryable`, which only becomes true after a load has already failed, and in that state neither
`refill()` nor undo is reachable. It is a latent hazard closed on principle, not a caught defect — the
seventh finding in this loop's own code, and the first that is not provably live.

The *reachable* half of the same code was real: tapping Retry changed nothing on screen — same label,
same enabled button, failure message still up — so a reviewer on bad signal could not distinguish a
working retry from a dead button. Now disabled and relabelled while in flight.

**FAIL-BEFORE (four separate reverts, each with real output).**
* Pre-R1 `api_audio` (unconditional 200, no headers) → `2 failed`:
  `a_matching_etag_is_answered_without_ever_touching_the_audio` ("a matching If-None-Match must be
  answered 304, not re-sent") and `a_phone_asking_for_part_of_a_clip_gets_exactly_that_part_over_real_http`
  ("ranges must be ADVERTISED, not just honoured").
* Dispatch reverted to GET-only → the HEAD assertion panics (`agent.head(...)` gets 404).
* `pendingTotal` removed → "pendingTotal must be the whole pending backlog, not the 25 clips in this batch".
* Progress reverted to `queue.length` → `Expected substring: "407" / Received string: "پارچەی 1 لە 2"`.
* Retry affordance removed → `expect(locator).toBeDisabled() failed / Received: enabled`.

Noted for honesty: the double-tap-costs-one-fetch assertion in that last test passes under the old
guard too (skipping and joining both yield one fetch). It is a regression guard on the coalescing, not
a proof of it. The discriminating assertions are the affordance ones.

**New tests (6 Rust, 3 Playwright).** The HTTP-level one writes a real decodable 16 kHz WAV and drives
it through `tiny_http`, so HEAD's body suppression and its `Content-Length` are *measured*, not read
off the crate source; range slices are compared against the real bytes at those offsets, so a wrong
offset fails on content rather than length. `parse_range` is pinned across every form a media stack
sends, including the suffix and clamping cases where an off-by-one would silently serve the wrong audio.

**Gates (unmasked exit codes).** `cargo test --lib` **0** — `1100 passed; 0 failed; 7 ignored`
(was 1094). Whole-workspace `cargo test` **0** — 25 suites, 1167 passed, zero FAILED. `cargo clippy
--all-targets --all-features -D warnings` **0**. `cargo fmt --check` **0**. couch-page Playwright
**32 passed**, stable 3-for-3 (runs 1/2/3 all exit 0). `npm run test:e2e` **0** — **79 passed**
(was 76). `npm run test:python-policies` **0** — 45/45. `npm run typecheck` **0** — 426 files,
0 errors.

**Also fixed: a rebuild gate that was reporting red and being ignored.** The previous iteration's
bundled rebuild logged `build exit=1` while the freshness gate passed. Cause: `npm run tauri build`
has `"targets": "all"`, so it runs the MSI/NSIS bundlers after linking the exe — and their failure
masks a perfectly good build. Confirmed the Rust side is clean (a fresh `cargo build --release` fails
only with `os error 32`, the running app holding the exe lock — not a compile error). The loop's
rebuild step now uses the existing bundle-free `tauri:build:smoke`, so exit 0 means what it says. The
installer bundlers are out of scope per the owner's "ship = personal use" decision.

**Iteration 215, live verification after the rebuild.** Rebuild via the bundle-free
`npm run tauri:build:smoke`: `BUILD_EXIT=0` (release, 9m00s), `EXE FRESHNESS GATE: OK (exe at HEAD
df5d4d1, newer than all sources)`, `FRESHNESS_EXIT=0`, app relaunched, watchdog re-enabled, server up
after 10 s. Then every R1 behaviour driven over real HTTP against the owner's own server with a real
reviewer token:

| Probe | Result |
|---|---|
| `POST /api/claim` | **200** |
| `GET /api/queue` | **200** — reviewer `Reviewer H`, 29 items, `pendingTotal=116`, `heldByOthers=0` |
| `GET /api/audio/<id>` | **200**, 390060 bytes, `ETag: "b386c3e8ff2e48a3"`, `Cache-Control: private, max-age=31536000, immutable`, `Accept-Ranges: bytes` |
| `Range: bytes=0-1` | **206**, `Content-Range: bytes 0-1/390060`, 2 bytes returned |
| `HEAD` | **200**, `Content-Length: 390060`, 0-byte body |
| `If-None-Match: "b386c3e8ff2e48a3"` | **304**, 0-byte body |
| `Range: bytes=99999999-` | **416**, `Content-Range: bytes */390060` |

`pendingTotal=116` is the real backlog, so the progress denominator is now the corpus rather than the
batch — the exact number this plan's R1 motivation cites.

**A harness bug that briefly looked like a server bug, recorded so it is not repeated.** The first
verification pass reported `range:` / `head:` / `conditional:` as blank failures. Not the server:
Windows PowerShell 5.1's `Invoke-WebRequest` refuses restricted headers passed via `-Headers` ("The
'Range' header must be modified using the appropriate property or method"), and the thrown exception
then poisoned the two probes after it, so one unsettable header produced three false negatives.
Re-driven with `System.Net.Http.HttpClient` + `HttpRequestMessage.Headers.Range`, all six pass. Nothing
was claimed from the blank pass.

**Still not measured, and not claimed:** the ≤ 11-audio-GETs-per-10-clip-drill count, and whether a real
browser elides the repeat fetch (the 304 is proven server-side; the client cache hit is device
behaviour). Both belong to the R5 real-device hour.

## Iteration 216 — the watchdog's kill branch stops being covered by luck, and the drill's own false pass

The drill added in iteration 212 SKIPped its three most important assertions whenever the real app was
down, because it matched the process by exe PATH and could not fake one. So the force-kill decision —
the line most able to destroy a reviewer's in-flight work — had coverage that depended on machine
state, which for that line is no coverage at all.

`cortex-watchdog.ps1` now honours `CORTEX_WATCHDOG_EXE`, the same shape as the existing DATA_DIR/PORT
overrides and inert in production. The drill starts its own decoy — a copy of `powershell.exe` at
`<tmp>\cortex-speech-app.exe` running `Start-Sleep`, so it matches both the hardcoded process NAME and
a path the test controls — and points the override at it. The owner's live app is excluded by the
watchdog's own path filter, so this is safe to run mid-review, and `-DryRun` means nothing is killed
regardless. **3 conditional assertions → 8 unconditional ones.**

**The fail-before found a defect in the drill itself, and it was a false pass.** With the override
removed, one line printed `OK   session + not running -> relaunch: kill-and-relaunch (attempt 1/3)`.
The matcher was `want not in got`, and `"relaunch"` is a substring of `"kill-and-relaunch"` — so a drill
asserting a plain relaunch went green on a decision to FORCE-KILL the app first. That is the worst
failure mode a test can have, it was in code written in this loop, and only running the fail-before
surfaced it. `expect` now compares the leading action token exactly.

Two assertions were added for a related reason: with the real app running, the three alive-branch
results could have come from the real app rather than the decoy, making them vacuous. The drill now
runs the same state twice — override pointing at an empty path (must report a DEAD process) and at the
decoy path (must report ALIVE) — so the decoy is proven to be what matched. A third check asserts the
decoy is still alive after three kill decisions, i.e. that `-DryRun` really is dry.

**FAIL-BEFORE (real output, exit 1).** Reverting the `CORTEX_WATCHDOG_EXE` override:
```
  FAIL no session + not running -> launch: leave-alone (deliberate Stop)
  FAIL session + not running -> relaunch: kill-and-relaunch (attempt 1/3)
  FAIL decoy alive but override points elsewhere -> DEAD: kill-and-relaunch (attempt 1/3)
```
(Before the matcher fix, the middle one of those printed `OK`.)

**Gates (unmasked).** `python scripts/test_watchdog_decisions.py` **0** — `watchdog decision drill
passed (8 branch assertion(s))`, every branch OK, with the real app running throughout.
`npm run test:python-policies` **0** — 45/45. No rebuild needed (`.ps1` + `.py` only), so the reviewer
link never dropped: re-verified after the change at claim 200 / queue 200 / 29 items / pendingTotal=116.

## Iteration 217 — R2: one tap per clip, and the keyboard stops covering the save buttons

**Plan item R2** of `docs/REVIEWER_UX_10_PLAN.md`, items 2/3/4/6. Items 1 and 5 deliberately not built.

**Item 1 (welcome/Start gate) is not needed, and dropping it costs nothing.** Its only job in the plan
was to unlock the shared `<audio>` element for programmatic play on iOS, by calling `play()` inside a
touch handler. But the FIRST CLIP'S OWN PLAY BUTTON does exactly that — iOS permits programmatic
`play()` on an element a gesture has already started. So auto-advance works from clip 2 onward with no
new screen at all. The entire cost is that clip 1 takes two taps instead of one; the saving is a new
screen between a reviewer and their work, three new unreviewed Sorani strings, first-visit localStorage
keying, and an identity-confirmation surface nobody asked for. **Net new Sorani strings for all of R2:
zero** — so nothing here is owner-gated and nothing waits.

**Item 5 (3 px progress bar) skipped as decoration.** It fixes no defect; the text counter already
gives honest progress and iteration 215 made it count the real backlog. Logged, not built, per the
standing "reliability first, not new surface" constraint.

**R2.4 — the keyboard occlusion, and it took two attempts to actually fix.** On iOS the on-screen
keyboard does not resize the layout viewport, so a reviewer who tapped the transcript to correct a word
had Save/Accept/Reject and the "Saved" toast sitting underneath it: they typed the correction and could
not see the button that commits it. `interactive-widget=resizes-content` handles Android declaratively;
a `visualViewport` listener feeds `--kb` for iOS, and body padding + the toast offset consume it.

My first version stopped there and the test caught that padding alone is only HALF the fix: it creates
scroll room, but the browser scrolls the FOCUSED element into view — the transcript, which sits above the
buttons — so the reviewer was left having to discover they could scroll. Measured: `#save` at y+h = 604px
with only the top 400px visible.

The second attempt used `scrollIntoView({block:'end'})` and **failed identically, 604px again**, for a
reason worth writing down: `scrollIntoView` aligns to the LAYOUT viewport, and on iOS the layout
viewport's bottom edge IS the part behind the keyboard — so it lands the row precisely where it cannot
be seen, with the padding it needs to clear sitting off-screen below it. The fix computes the overshoot
against the VISUAL viewport (`rect.bottom - (vv.offsetTop + vv.height)`) and scrolls by exactly that.
Both halves now provably cooperate: the test asserts the document gained exactly 320 px of scroll room
AND that all three decision buttons end up inside the visible band.

**R2.2 — auto-advance.** Deciding a clip is the reviewer saying "next", so `show(true)` starts the next
one. Applies to a successful decision AND to one queued offline (they judged it and moved on); NOT to
the 409 forced-skip, a skip, an undo, or a page load — navigation is not a request for sound. The
`play()` promise is `.catch()`-swallowed, so a browser that refuses degrades to exactly today's
behaviour: a loaded clip waiting for a tap. Never a thrown error, never a page that thinks it is playing.

**R2.3 — pause on edit, rewind 2 s on resume.** Reaching for the textarea while audio runs loses the
tail of what was just heard. Focus pauses; the next play gives back the ↺2s amount automatically at the
one moment it is always wanted. Only when THIS mechanism caused the pause — a manual pause is a position
the reviewer chose. `again` clears the flag before playing, or the ↺2s button would silently rewind 4 s.

**R2.6 — safe-area insets.** The page already shipped `viewport-fit=cover`, which is what lets it draw
edge to edge and therefore also under the notch and home indicator once installed to the home screen
(`display:standalone`, no browser chrome to protect it). `env()` is 0px where there are no insets, so
desktop and non-cutout Android are unchanged.

**Self-audit of iteration 215's own diff found one more.** `parse_range` had a `.max(1)` on the suffix
length, which quietly turned `bytes=-0` — "give me the last zero bytes" — into a request for the final
byte. RFC 9110 says a zero suffix-length is unsatisfiable, and removing the `.max(1)` makes it fall out
correctly AND deletes code: `start == len` already fails the guard. Tenth finding in this loop's own
code. The test that asserted the wrong behaviour was corrected with it.

**FAIL-BEFORE (five reverts, real output).**
* Auto-advance removed (`show()` for both decision paths) → `Expected: 1 / Received: 0` plays.
* `visualViewport` listener disabled → `Expected: "320px" / Received: ""`.
* Rewind-on-resume handler removed → `Expected: 8 / Received: 10` (no rewind).
* `again`'s flag clear removed → `Expected: 8 / Received: 6` — the exact −4 s double-rewind.
* Skip made to autoplay → `Expected: 0 / Received: 1` plays.

**Gates (unmasked).** couch-page Playwright **36 passed**, exit 0, stable 3-for-3.
`npm run test:e2e` **0** — **83 passed** (was 79). `cargo test --lib couch::` **0** — 49 passed.
`cargo clippy --all-targets --all-features -D warnings` **0**. `cargo fmt --check` **0** (with
`--manifest-path`; a bare `cargo fmt --all` from the app dir fails on `cargo metadata`, which is a cwd
artifact and not a formatting result — worth knowing before reading that exit code as red).
`npm run test:python-policies` **0** — 45/45. `npm run typecheck` **0** — 426 files, 0 errors.

## Iteration 218 — R3/R4: failures you can read, a banner that follows through, live regions

Plan items **R3.3, R3.4 and R4.3**, all chosen because they need no new Sorani and each closes a
defect rather than adding surface.

**R3.3, the honesty defect in the error path.** Every string on this page is translated, and then the
one word that says WHAT WENT WRONG arrived in English straight from the server, dropped inside a Sorani
sentence. Measured, from the fail-before run:

```
بارکردنی ڕیز سەرکەوتوو نەبوو: another reviewer is working on this clip
```

A reviewer who does not read English got a message that *looked* translated and told them nothing, at
the exact moment they needed it. Fixed by passing the STATUS instead of the message — language-neutral,
still diagnostic, and it fits the existing reviewed `{err}` slot, so **no new Sorani was invented**. A
transport failure has no status and gets a dash, which is more honest than "Failed to fetch"; the Retry
button is what actually helps there. `undoFailed` stopped concatenating `e.message` entirely — the
Sorani sentence reads fine alone. A proper "no connection" string is R3.1's owner-gated pill.

New policy gate `test_no_raw_server_english_is_shown_to_the_reviewer` in `test_couch_page_i18n.py`, so
the tempting fix next time an error path appears cannot reintroduce it. Policy scripts stay at 45; the
gate is a new assertion inside an existing script.

**R3.3, second half: failure toasts wait to be read.** 1.4 s is right for "Saved" — the reviewer knows
what they did and is already moving. It is wrong for "could not save", which now arrives while their
attention is on the NEXT clip (auto-advance, shipped last iteration, made this worse). Failures are
sticky, styled as failures, and dismissible by tap. The shared timer is cleared on every toast, so a
success arriving after a failure is not frozen on screen by the failure's absent timer.

**R4.3, the banner that asked for something it did not enable.** "find those clips and review them
again" — for ids the reviewer was never shown, in a queue they must scan by eye on a phone. Tapping the
banner now jumps to the first refused clip present in the batch. If none is present — the usual case,
since a refusal normally means someone else took it — the tap does **nothing**, deliberately: landing on
the wrong clip would be worse than not moving, because the reviewer would trust it.

**R3.4, live regions.** `aria-live="polite"` on progress/warn/done/toast: each changes without the
reviewer touching anything, which is exactly what a screen reader cannot discover. `#err` gets
`role="alert"` instead — it carries the link-expired verdict and the refused banner, both of which need
an action before work can continue.

**Self-audit of iteration 217's own diff, one fix.** The keyboard nudge ran from the visualViewport
`scroll` handler as well as `resize`, so a reviewer scrolling up to re-read the transcript would be
dragged straight back down to the buttons. The page fighting the finger is worse than the problem the
nudge solves. Nudge is now resize-only; `--kb` still tracks scroll. Eleventh finding in this loop's own
code — and, like the last one, found by reading the diff rather than by any test failing.

**FAIL-BEFORE (four reverts, real output).**
* Sticky toasts reverted to always-transient → `Expected pattern: /show/ / Received string: "toast sticky"`.
* `e.message` restored in the load failure → `Expected substring: "409" / Received: "بارکردنی ڕیز سەرکەوتوو نەبوو: another reviewer is working on this clip"`.
* Refused-banner click handler removed → stayed on clip 1 instead of jumping to clip 2.
* `aria-live`/`role` attributes stripped → `Expected: "polite" / Received: null`.

**Gates (unmasked).** couch-page Playwright **40 passed**, exit 0, stable 3-for-3.
`npm run test:e2e` **0** — **87 passed** (was 83). `npm run test:python-policies` **0** — 45/45.
`npm run typecheck` **0** — 426 files, 0 errors. `test_couch_page_i18n.py` standalone **0** — 22
strings, 14 reusing natively-reviewed desktop Sorani, 8 awaiting owner review (unchanged: this
iteration added none).

Noted honestly: the first `npm run test:e2e` invocation exited **127** with no test output at all —
command-not-found, not a failure. Re-run gave 0 / 87 passed. Reported rather than quietly retried.

## Iteration 219 — the spot check was at the tail of EVERY batch; and two of the untested cases closed

Three of the four behaviours on this loop's untested list. One of them found a real defect in code that
predates this loop, and it defeats the measurement it exists to protect.

**THE DEFECT: a spot check landed LAST in 5 of 5 batches.** The code above the position calculation
said, in as many words, "Interleave rather than append: a run of traps at the tail of every batch is a
pattern a reviewer would learn within a session." It did not interleave. `wanted` is
`queue.len().div_ceil(8)`, so a 25-clip batch asks for **4** checks — but only three multiples of 8 fall
inside 25 (8, 16, 24). The fourth computed 32, hit `.min(queue.len())`, and was appended to the end.
Every batch. Every session.

That is not a cosmetic issue: **a reviewer who noticed the last clip of every batch is a trap could pass
every honesty test in a session by listening to exactly one clip per batch** — while tapping accept on
the other 24. The one mechanism in this repo that measures whether a human is actually listening had a
tell, and the comment asserting otherwise is what stopped anyone looking.

Fixed by dividing the batch into `wanted + 1` gaps: 25 work clips and 4 checks now land at 5, 11, 17, 23
(the `+ idx` accounts for earlier insertions shifting later indices). The `.min` is kept purely so the
index can never exceed the length — it no longer binds, and `Vec::insert` panicking is not an acceptable
way to discover that. The RATIO is unchanged; only the positions move, so the live batch is still 25 + 4
= 29 items. Twelfth finding of this loop and the first in code it did not write.

**FAIL-BEFORE** is the failing run itself, against the old positions:
```
assertion `left == right` failed: a spot check landed last in 5 of the batches — the tail is the one
position a reviewer can learn without trying
  left: 5
 right: 0
```

**Covered now (spot-check accounting across many batches).** Everything about spot checks had been
tested on ONE batch; a real sitting is a dozen. The new test drives six rounds over a 125-clip backlog
with 30 answer keys and asserts, per batch and in aggregate: the ceiling ratio holds every round; no
check is ever served twice to the same reviewer (re-serving one teaches the answer and then scores them
on knowing it); every check answered produces exactly ONE score row, not one per submit; and no check
lands at the tail. Plus the corpus invariant — every answer key still has `reviewed_by = None`.

**Covered now (lease expiry with a slept/backgrounded phone).** A phone that sleeps stops the renew
heartbeat, and after LEASE_TTL the lease lapses. The two requirements pull opposite ways and both are
now pinned: if nobody else took the clip, the reviewer who still has it open KEEPS it (refusing there
would destroy a correction typed before the phone slept, for no benefit); if somebody else did take it,
the refusal arrives at RENEW time — while the text can still be copied — as a 409 naming the reason, and
must not quietly steal the lease back. `Instant` cannot be fabricated, but it can be walked backwards
with `checked_sub`, which is what makes this testable at all without a 15-minute wait.

**NULL RESULT: audit #25 was already covered.** I wrote a test for "a spot check served before a restart
is still scored after it" and the compiler rejected it as a duplicate — an existing test of that exact
name has covered it since phase 4, including the answer-key-intact assertion. Mine added a blind-accept
variant of the same mechanism, which is not worth a second test. Deleted rather than kept. Reported
because the loop's own untested list named #25, and the honest answer is that the list was stale.

**Still not done:** audit #28 (check-then-act race, previously refuted 2/3 by adversarial verify) is
untouched this iteration. Not claimed as covered.

**Gates (unmasked).** `cargo test --lib` **0** — **1102 passed**; 0 failed; 7 ignored (was 1100; +3 new,
−1 duplicate removed). `cargo clippy --all-targets --all-features -D warnings` **0** (two
`unnecessary_cast` errors in my new test on the first run, fixed, not suppressed). `cargo fmt --check`
**0**. No page change, so no Playwright/e2e delta this iteration.

## Iteration 220 — R3.5, and the last two audit items were already closed

**R3.5 — the swipe gesture stops being invisible.** It worked and gave no feedback of any kind: nothing
moved while the finger moved, so a reviewer whose thumb happened to travel 90 px cast a verdict with no
warning, and a reviewer who wanted the gesture had no way to discover it or to see they had crossed the
threshold. The card now tracks the finger (damped 0.35, so it reads as pulling something weighted rather
than flinging the clip away), shows an inset ring in the colour of the verdict a release would cast, and
snaps back under the threshold. An inset ring rather than a background wash, so the transcript stays
readable at the moment the reviewer is deciding. `prefers-reduced-motion` drops the transition.

Two thresholds that had to agree were unified: the commit distance was a bare `90` inside `touchend`, and
feedback that appeared at a different distance from the one that commits would be worse than no feedback,
because the reviewer would trust it. `SWIPE_COMMIT_PX` / `SWIPE_ABORT_DY` are now defined once and used by
both paths, along with a shared `isForward()` so the RTL direction rule cannot diverge either. A gesture
that turns vertical drops the feedback immediately rather than leaving the card mid-drag while the page
scrolls under it, and `touchcancel` resets too.

**A bug in my own test, worth recording because it is the reusable kind.** The harness recomputed
`getBoundingClientRect()` on every synthetic touchmove — but the rect moves WITH the transform, so each
`dx` was measured from the card's new position instead of the finger's origin. A 120 px swipe arrived as
78 px and fell silently under the commit distance, and the failure looked exactly like a missing
`willReject` class. The origin is now captured once at touchstart. The page was correct throughout.

**FAIL-BEFORE:** removing the `touchmove` handler → `Expected substring: "translateX" / Received: ""`.

**NULL RESULTS, both audit items on this loop's list are already closed.**
* **#25** (spot-check answer across Stop/Start) — covered since phase 4; my duplicate test was rejected by
  the compiler and deleted (iteration 219).
* **#28** (check-then-act race) — closed by P1.3b: `RESTORE_PENDING` + an RAII reservation, couch's check
  moved under the COUCH lock, publish-then-recheck for the four atomic-flag writers. Verified still intact
  today: **15** `restore_pending()` call sites, `couch.rs:440` among them, and
  `scripts/test_restore_reservation_gate.py` passes standalone (exit 0). Nothing to do; the loop's
  untested list was stale on both.

**Gates (unmasked).** couch-page Playwright **41 passed**, exit 0, stable 3-for-3.
`npm run test:e2e` **0** — **88 passed** (was 87). `npm run test:python-policies` **0** — 45/45.
`npm run typecheck` **0** — 426 files, 0 errors. No Rust change this iteration.

## Iteration 221 — the trim path stops being a whitelist (and the honest correction that it lost nothing yet)

Found while researching the chunking architecture. `rebound_alignment_json` (`chunking.rs`) REBUILT a
segment's alignment JSON from `SegmentSourceMeta`'s four fields and then re-merged exactly ONE
whitelisted key, `words`. Every other key was dropped. Its production caller is `update_segment_bounds`
(`commands/segments_write.rs:275`) — the trim action, which is the reviewer's most-used edit.

The shape is the bug: a whitelist has to be extended by hand every time any writer adds a key, and the
failure mode is invisible — no error, no log, the key is simply gone the next time someone nudges a
boundary. It was also the odd one out; `merge_word_timestamps` immediately above it already
preserves-and-inserts. The history shows why: `words` was itself lost this way once, and the fix at the
time was to add `words` to the whitelist rather than to delete the whitelist.

**HONEST CORRECTION, and it changes the severity I reported.** I described this to the owner as a "live
data-loss risk". Measured before touching it, against the real library
(`%APPDATA%\cortex-speech\cortex-speech.db`, opened `mode=ro`):

```
segments with alignment_json: 144
shapes: {'object': 144}
keys seen: {'chunk_count': 144, 'chunk_index': 144, 'source_end_ms': 144, 'source_start_ms': 144, 'words': 144}
KEYS THAT rebound_alignment_json WOULD DROP: none
```

Every key present is on the old whitelist, so **nothing was actually being lost**. It is a latent hazard,
not live loss, and the ledger says so rather than letting the more alarming first description stand.

**The fix deletes the whitelist rather than extending it.** Parse the existing object, update
`source_start_ms`/`source_end_ms`, default `chunk_index`/`chunk_count` ONLY when absent, keep everything
else untouched. Shorter than what it replaces. It also now lifts the legacy bare-array shape (rows that
predate the object form and ARE a word array) under `words` instead of discarding it — those are the
oldest segments in the library and would have lost their words on the first trim.

**FAIL-BEFORE:** restoring the whitelist rebuild →
`assertion left == right failed: an unknown scalar key must survive / left: Null / right: "mms-onnx"`.

The new test deliberately asserts on keys **no current writer produces** (`alignment_backend`,
`overlap_detected`, a nested object). That is the point: they stand in for whatever gets written next,
and the assertion is that nobody has to remember to extend a list for them to survive. It also pins the
legacy-array lift and the absent/unparseable/scalar inputs.

**Gates (unmasked).** `cargo test --lib` **0** — **1103 passed**; 0 failed; 7 ignored (was 1102).
`cargo clippy --all-targets --all-features -D warnings` **0**. `cargo fmt --check` **0**.

## Iteration 222 — the cut criterion was wrong, not the search window

Second of the three fixes the owner asked for. The chunker's own comment promised it "never slices
through a word", and measurement says that promise was already kept — but for a narrower reason than
anyone had checked, and the residual failure was real.

**What was measured first** (`Sound_From_AP_Part02.wav`, 1,799,631 ms, the 144 live rows, 143 internal
boundaries; DB opened `mode=ro`, WAV read-only, nothing written):

| Measurement | Result |
|---|---|
| Cuts landing at speech level | **0 / 143** — every cut ≥37.7 dB below local speech, median −51.7 dB |
| Gain from widening the energy search to ±1 s | **median 0.0 dB**, p90 2.5 dB |
| Cuts in a gap ≤100 ms (too short to be a pause) | **23 / 143 (16.1%)** |
| Chunks starting with ≤30 ms of leading silence | **39 / 143 (27.3%)** |
| Boundaries where a WIDER silence run existed in the same 4.5 s band | **56 / 143** |
| Boundaries with a ≥300 ms pause already inside the cap | 106 / 143 (74%) |
| …allowing +3 s / +5 s overrun | **122 (85%)** / 132 (92%) |

So the "guillotine through a word at full volume" fear was unfounded, and widening the search window —
the obvious fix — buys **nothing**. The criterion is what is wrong: `find_quietest_cut` took the single
lowest-energy FRAME, and a 30 ms inter-syllable dip beats a 400 ms real pause two seconds later.

**The change.** `find_pause_cut` marks frames below the region's own speech level (median 15 ms frame
RMS, −25 dB) and takes the **widest contiguous run**, cutting at its **centre** so there is air on both
sides. A run must be ≥120 ms to qualify at all, which is precisely what rules out the 16%. When no real
pause exists inside the cap, the chunk may run up to **3 s** long to reach one (the measured knee: +3 s
buys 85%, +5 s only 92%) before falling back to the old quietest-frame rule for genuinely continuous
speech, music or noise.

**The trap, and it is why the third test exists.** Both cap passes had to move from `max_samples` to
`cap_with_overrun`. Miss either and the safety re-split in `plan_speech_chunks` faithfully undoes every
overrun `silence_aware_split` just chose — the change compiles, every unit test on the splitter passes,
and the shipped behaviour is unchanged. `the_overrun_survives_the_final_cap_pass` drives the whole
planner for exactly that reason.

**FAIL-BEFORE (three tests, all three fail on the old criterion).**
* `a_wide_pause_beats_a_deeper_but_narrower_dip` →
  `got 11010ms (11010ms means it chose the 30ms dip — the exact defect this replaced)`.
  The synthetic dip is DEEPER than the pause (0 vs 50), so a rule that merely tied would not
  discriminate; the old rule must prefer it and the new one must not.
* `a_chunk_may_run_slightly_long_to_reach_a_real_pause_but_stays_bounded` →
  `it must reach past the cap to the real pause, got 10500ms`.
* `the_overrun_survives_the_final_cap_pass` →
  `a chunk should have run past the cap; lengths were [10500, 10500, 9000]`.

**Honest limits, unchanged from the research.** n = 1 recording, one speaker set, one edited-broadcast
style. It is an acoustic-silence measurement, not word verification — nobody listened to confirm that any
specific clip clips a word, and the DB's word arrays cannot arbitrate because `aligner::align`
(`aligner.rs:647`) is a stub returning the energy heuristic. `PAUSE_THRESHOLD_DB` and `MIN_PAUSE_MS` are
calibration knobs set from ONE file and both need re-checking on a second recording. **This affects
future imports only** — the 144 existing segments are already cut and are untouched.

**It does not fix overlapping speakers.** That is a separate problem and its clean solution was refuted
(see iteration 223).

**Gates (unmasked).** `cargo test --lib` **0** — **1106 passed**; 0 failed; 7 ignored (was 1103).
`cargo clippy --all-targets --all-features -D warnings` **0** — two errors on the first run, both fixed
rather than suppressed, and one mattered: `is_none_or` is stable only since Rust **1.82** while this
crate's MSRV is **1.81**, so the widest-run tracking was rewritten without it rather than raising the
MSRV. `cargo fmt --check` **0**.

## Iteration 223 — the speaker labels do not survive their first control

Third of the three fixes the owner asked for. It was meant to be a cheap win and it turned into the most
important negative result of the session.

**The plan.** The owner hit a clip holding two speakers. The clean fix — diarize first, then cut on
speaker boundaries — was refuted by the architecture workflow (`sherpa_onnx::OfflineSpeakerDiarization`
can `SHERPA_ONNX_EXIT(-1)`, uncatchable from Rust, and a mid-import abort discards the whole file). So
the cheap alternative: `SpeakerEmbeddingService::compute_embedding` takes an ARBITRARY sample slice, so
CAM++ — already downloaded, already loaded — can embed a clip's first half and second half and compare
them. Zero new models, zero downloads, zero licence questions.

**Measure before building.** `src/bin/speaker_change_probe.rs`, read-only, run against the live library.

**FIRST RESULT WAS WRONG, AND SAYING SO IS THE POINT.** It reported "SEGMENTS WITH A SPEAKER CHANGE:
144 / 144 (100.0%)". A 100% detection rate whose maximum observed similarity (0.845) never once crosses
the threshold (0.85) is the signature of a broken measurement, not of a corpus where every clip has two
speakers. The error was mine: 0.85 is `online_cluster`'s threshold for comparing a chunk against an
AVERAGED, re-normalised centroid — not two raw ~6 s embeddings against each other. Nothing was reported
to the owner from that run.

**Then the controls, which is what the probe should have had from the start.** Same embeddings, two
reference distributions: halves of DIFFERENT clips CAM++ gave the SAME label (same person, different
moment — the realistic ceiling), and halves of clips it gave DIFFERENT labels (the floor).

```
WITHIN-clip (half vs half)    n=144    min 0.305  p10 0.559  median 0.753  p90 0.817  max 0.845
SAME speaker, other clip      n=2377   min 0.106  p10 0.318  median 0.647  p90 0.778  max 0.877
DIFFERENT speaker, other clip n=7919   min 0.066  p10 0.276  median 0.638  p90 0.768  max 0.874
```

**The same-speaker and different-speaker controls are 0.009 apart.** They are the same distribution.
CAM++ is not separating speakers on this material, so no within-clip number can be interpreted and the
probe says INCONCLUSIVE rather than inventing a percentage.

Two hypotheses fit this equally and the probe cannot distinguish them, so neither is claimed:
  1. CAM++ genuinely does not separate these voices (recording conditions, language, clip length);
  2. the labels are wrong because chunks contain multiple speakers — which makes BOTH control groups
     mixtures, and mixtures always look alike. This is circular by construction: the probe uses CAM++'s
     own labels as ground truth for CAM++.

Also worth noting: the within-clip median (0.753) is HIGHER than either control, which is expected and
is another reason the first threshold was meaningless — two halves of one clip share channel, room and
background, so that number is inflated by recording conditions rather than by speaker identity.

**What this actually establishes.** The `SPEAKER_00…07` labels in the corpus are **unvalidated**, and
this is the first evidence bearing on them either way. It is consistent with the architecture research,
which found no DER or diarization-accuracy measurement anywhere in `eval.rs`, `wer.rs` or
`scorecard.rs`. The cheap CAM++ path is refuted: it cannot answer the owner's question, and building the
chunk-splitter on top of it would have shipped a splitter driven by a signal that does not discriminate.

**OWNER-GATED, and it is small.** Nothing in this repo can settle it without ground truth. The cheapest
possible: listen to ~15 clips and record, per clip, "one speaker" or "more than one". That calibrates
the probe, decides whether the labels are salvageable, and decides whether the pyannote-via-`ort` path
(overlap-aware, MIT, 5.99 MB, ungated) is worth its footprint. Fifteen minutes of listening replaces an
unbounded amount of guessing.

**Gates (unmasked).** `cargo clippy --all-targets --all-features -D warnings` **0**.
`cargo fmt --check` **0**. The probe is a read-only measurement binary and adds no library code: it opens
the library with `SQLITE_OPEN_READ_ONLY` rather than `Database::open` (which opens read-write, runs
`PRAGMA journal_mode=WAL` — itself a write — and whose `Connection::open` does not enable URI parsing, so
a `file:…?mode=ro` string would have quietly created a stray empty database).

## Iteration 224 — the ear settles it: the signal was always good, the CONTROL was circular

The owner listened to the blind 15-clip set. The result reverses iteration 223's conclusion, and the
reversal is the interesting part.

**Ground truth (blind: shuffled, no similarity score, no CAM++ label, no band shown until submitted):**

| band | answers |
|---|---|
| low (5) | **5 × multi-speaker** — 0.305, 0.412, 0.415, 0.427, 0.428 |
| mid (5) | **5 × one speaker** — 0.753, 0.753, 0.754, 0.757, 0.757 |
| high (5) | **4 × one speaker**, 1 × genuine OVERLAP — 0.841 … 0.845 |

**Perfect separation.** Every turn-taking clip scored ≤ **0.428**; every single-speaker clip ≥ **0.753**.
Nothing landed in between — an empty band **0.325 wide**. Threshold 0.59 (its midpoint) misclassifies
**0 / 15**.

**Why the probe's own controls said INCONCLUSIVE, and why that was not the signal's fault.** The
controls were built from CAM++'s CHUNK labels, and a chunk holding two speakers gets exactly one label.
So the "same speaker" group and the "different speaker" group were both mixtures of the same material,
and mixtures always look alike — 0.009 apart. Iteration 223 listed this as hypothesis 2 of 2 and said it
could not distinguish them. It could not: the circle is only breakable from outside, by an ear. Holding
the INCONCLUSIVE line instead of picking the flattering hypothesis is what made this recoverable.

**MEASURED ON THE WHOLE LIBRARY at the calibrated threshold: 17 / 144 clips (11.8%) hold a speaker
change.** Not the 100% the first broken run claimed, and not negligible either.

**Overlap is invisible to this method, and now that is measured rather than predicted.** The one clip
with genuinely simultaneous speech scored **0.841** — right among the single-speaker clips. Two voices at
once blend into one consistent texture across both halves, so the embedding sees no change. The probe
docstring predicted this before the listening pass; the pass confirmed it with a real example. 1 / 15
clips carried overlap. Detecting it needs an overlap-aware model (pyannote powerset); nothing already on
this machine can do it.

`GROUND_TRUTH` and `SPEAKER_CHANGE_THRESHOLD = 0.59` are now constants in the probe, with the 15 labelled
clips recorded inline, so the calibration is reproducible and any future change to the embedding path can
be re-checked against a real human answer instead of a remembered one.

**Corrected in the same commit:** the module docstring still asserted "CAM++ does not separate these
speakers at all", which the ground truth disproved; and two `println!` em-dashes rendered as mojibake on
the Windows console.

**Gates (unmasked).** `cargo clippy --all-targets --all-features -D warnings` **0**.
`cargo fmt --check` **0**. `npm run test:python-policies` **0** — 45/45.

## Iteration 225 — speaker-aware chunking: built, measured on real audio, NOT shipped

The owner asked to start with speaker-aware splitting and to reality-check it against the goal. Built,
tested, evaluated on his actual recording — and **reverted, because it could not be shown to help.**

**The design was sound on paper.** Candidate split points are the real pauses (`chunking::find_pauses`,
from iteration 222), so a speaker split lands mid-silence for free instead of solving word-clipping a
second time. Fast path first: one half-vs-half comparison per chunk, so the 88% single-speaker case costs
two embeddings. Injected embedder (as sample INDICES, not slices) so the decision logic is unit-testable
without synthesising speech a real model would have to agree about. Six tests covered split-at-pause,
never-split-one-speaker, no-pause-means-no-cut, too-short-to-judge, no-embedder-means-no-change, and
three-way recursion. All passed.

**Then the reality check, against `Sound_From_AP_Part02.wav` through the real planner.** Three variants:

| variant | chunks | mixed chunks | min duration |
|---|---|---|---|
| baseline (silence only) | 139 | 12 | 10607 ms |
| whole-side compare, 1500 ms half | 155 | **10** | **1547 ms** ← violates the 3000 ms floor |
| whole-side compare, 3000 ms half | 151 | 11 | 3075 ms |
| local-window compare, 3000 ms half | 151 | **14** | 3125 ms |

The best result fixed 2 of 12 while emitting chunks **below the library's own configured minimum** — my
`MIN_SPLIT_HALF_MS = 1500` silently overrode `min_segment_duration_ms`, which is a settings violation
regardless of the rest. Honouring the setting fixed that and made the primary metric worse.

**The local-window variant was a reasoned hypothesis and it was wrong.** Reasoning: the probe's controls
showed CAM++ cannot match a speaker ACROSS clips (0.009 apart) while the blind listening pass showed it
detects a change WITHIN one — so it behaves as a local contrast detector, and comparing whole sides makes
both operands mixtures for every candidate but the true one. Comparing short windows either side of each
pause should therefore have been better. Measured: 12 -> 14. Recorded because the reasoning is worth
having on file even though the prediction failed.

**AND THE EVALUATION IS CONFOUNDED, which is the real reason nothing here can be claimed.** The 0.59
threshold was calibrated on ~14 s clips whose halves are ~7 s. A split piece is ~6 s, so its halves are
~3 s — noisier embeddings, systematically lower cosine. Shorter chunks get flagged as "mixed" more often
regardless of who is speaking. So "12 -> 14" cannot be read as worse any more than "12 -> 10" could have
been read as better. **The metric cannot compare chunks of different lengths, and every variant changes
chunk length.** Fixing this needs a length-matched calibration, which needs more ground truth than the
15 clips that exist.

**Shipping an unproven change to how the entire corpus is cut, on a reliability-first project, is not a
close call.** `pipeline.rs` was reverted with `git checkout` — the import path is byte-identical to
before this iteration — and the splitter and its tests were deleted rather than left as dead code. The
`--replan` probe mode went with the code it existed to evaluate.

**What survives, because both are improvements independent of the splitter:**
* `chunking::find_pauses` — `find_pause_cut` is now `.first()` of it, so the widest-run scan has one
  implementation and the all-pauses form exists without duplicating it.
* `diarization::cosine_similarity` returns `Option<f32>` instead of a `-1.0` sentinel. That sentinel is
  numerically "maximally different", so any future caller that cannot distinguish it from a real score
  treats an ABSENT embedding as proof of two speakers. `online_cluster` restores `-1.0` at its own call
  site, so its behaviour is unchanged.
* `SPEAKER_CHANGE_THRESHOLD` now carries the negative result in its doc comment, pointing here, so the
  next person to try this reads the measurements before repeating them.

**Gates (unmasked).** `cargo test --lib` **0** — **1106 passed**; 0 failed; 7 ignored (1112 with the
splitter, 1106 after removing its 6 tests — back to the iteration-222 count, as it should be).
`cargo clippy --all-targets --all-features -D warnings` **0**. `cargo fmt --check` **0**.
`npm run test:python-policies` **0** — 45/45.

**Caught along the way:** I inserted the new method between `#[allow(clippy::too_many_arguments,
clippy::type_complexity)]` and the `build_segments_from_pcm` it was written for, so clippy failed on a
function I never touched. Same class as the E0119 from putting a type between a doc comment and its
`#[derive]` earlier in this loop. Re-attached rather than duplicated.

## Iteration 226 — the Option contract was half-built; and verify-10 measured, not assumed

**Where "10/10" actually lives.** `docs/REVIEWER_UX_10_PLAN.md` has nothing left in it that is not
owner-gated, so continuing to polish that plan would be polishing a finished list. The repo's own
definition of done is `scripts/verify_10.py`, and the last state this ledger recorded for it was
**RED** (iteration 194, on exe-freshness). Its best *reachable* verdict is
`GREEN — PERSONAL-USE SHIP-READY`; the literal
`CORTEX 10/10: ALL GATES GREEN` line is unreachable by construction while 8 legs are owner-descoped
and 5 owner-gated, and no amount of engineering changes that. So this iteration went after the
reachable one.

**The defect, and it is mine.** Iteration 225 changed `diarization::cosine_similarity` from a `-1.0`
sentinel to `Option<f32>`, for a good reason: `-1.0` reads as "maximally different", so a caller that
cannot tell it from a real score treats an ABSENT embedding as proof of two speakers. But the contract
was only half built — **a NaN or infinite input still returned `Some(NaN)`.**

That is worse than the sentinel it replaced. Every comparison against NaN is false, in BOTH
directions: `sim < T` reads "same speaker" and `sim >= T` reads "different speaker" off the identical
value. Which way a chunk falls then depends on how the caller happened to phrase its comparison, not
on the audio. A second silent no-verdict value inside a function whose whole point is to have exactly
one is not a rounding error in the design; it is the design not landing.

Fail-before, measured (only the `is_finite` filter reverted, the test kept):

```
test diarization::tests::cosine_similarity_has_no_verdict_rather_than_an_uncomparable_one ... FAILED
assertion `left == right` failed: NaN input has no verdict
  left: Some(NaN)
 right: None
test result: FAILED. 0 passed; 1 failed          exit 101
--- restore the filter, file byte-identical ---
test result: ok. 1 passed; 0 failed              exit 0
```

Three inputs are covered: a NaN sample, an infinite sample, and finite-but-huge samples whose squares
overflow f32 to Inf (a real path — the quotient there is NaN or a meaningless 0.0, not a score).
**Production behaviour is unchanged**: `best_centroid` maps `None` to `-1.0`, which is exactly where
NaN already landed (`NaN > best_sim` is false, and so is `-1.0 > -1.0`), so clustering is
byte-for-byte as before.

**Three comments left stale by my own iteration 225**, all still describing the `-1.0` return that no
longer exists. The guards they justify are still load-bearing — a degenerate embedding would otherwise
be pushed as a permanent phantom centroid that wastes a `max_speakers` slot — so this is a comment fix,
not a behaviour fix. One doc sentence had lost its subject entirely in an editing accident and named
the wrong function: it is `best_centroid`, not `online_cluster`, that restores the sentinel. **The
iteration-225 entry above says `online_cluster` too, and it is wrong there for the same reason.**

**Null result, measured rather than assumed.** `pendingTotal` (iteration 215) is a new reviewer-facing
count built on `db.get_segments(Some(false))`, which filters on `verified` alone — the exact shape of
the count-includes-rejected-rows honesty bug this repo has now fixed six times. So it was measured
against the live library rather than reasoned about:

| | |
|---|---|
| total segments | 144 |
| unverified (what the couch queue serves) | 112 |
| human-rejected (any of `human_decision`/`verdict`) | 6 |
| **rejected AND unverified** | **0** |
| **unverified with a blank draft** | **0** |

`pendingTotal = 112` is honest, and it equals the unverified count exactly. The invariant holds because
`api_decision` sets `verified = true` for every decision *including* reject (couch.rs:1627), so a
rejected clip leaves the queue. Reported as plainly as a defect would have been; nothing was fixed here
and nothing was invented.

### The full sweep, and the one gate it caught

`python scripts/verify_10.py` with `CORTEX_AUDIO` set at the committed FLEURS fixture, exe at HEAD:

```
kept gates run: 23 - 22 PASS, 1 FAIL, 0 skipped (env/not-built)
VERDICT: RED - 1 kept gate(s) failed (real-app-e2e). NOT ship-ready.
```

Zero skips: all 23 actually ran, including `fuzz-smoke` 698.5s (5 targets through WSL, 0 crashes),
`test-rust` 473.1s, `ignored-real-model` 32.3s, `egress-runtime` 23.2s, `rtf-bench` 21.2s,
`refinery-lift` 41.9s. A RED with nothing skipped is worth more than an INCOMPLETE: the failure had
nowhere left to hide.

**A correction I owe this ledger, because I nearly published the opposite.** I read the older line
"verify-10 has not yet been observed GREEN end to end", took it for the current state, and was one
sentence from calling today's result the first GREEN on record. **It is not.** `git log -- docs/STATUS.md`
shows GREEN 23/23 with zero skips from **`5fe085d` (2026-07-26)** onward — `9c437a2`, `4568e2a`,
`a9b7b45`, `d1f531b`, `4924a88`, `82fc450` — six more before this loop began. That ledger line was true
when written and was never retracted, which is **exactly** the failure mode `docs/STATUS.md` exists to
end: a hand-written doc asserting a gate state it did not measure. I trusted the prose over the
generated file. Today's result is a **restoration** of GREEN from the RED this loop's own work caused,
not a first — and the lesson is to read `docs/STATUS.md` and `git log`, never a ledger sentence, for
"where does this gate stand".

**And the failure was in the gate, not the product.**

```
failed to create webview: WebView2 error:
  WindowsError(Error { code: HRESULT(0x8007139F),
  message: "The group or resource is not in the correct state..." })
==> REAL-DATA RUN FAILED: WebView2 debug port 9222 did not come up within 90s.
```

`e2e_real_app.cjs` isolates `CORTEX_APP_DATA_DIR` into a disposable profile and refuses the production
one — a contract this repo already has a policy test for. But the WebView2 browser profile is a SECOND
shared resource, and it was not isolated. Tauri keys it on the bundle identity
(`%LOCALAPPDATA%\com.cortex.kurdish-speech\EBWebView`), **not** on the data dir, so a run spawned while
the owner's own Cortex is open lands in the same folder. WebView2 honours
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` only when it CREATES the browser process for a folder; when one
already exists it fails the environment with `ERROR_INVALID_STATE` and **silently drops
`--remote-debugging-port`**. The harness then polls 90 s for a port nobody opened and reports a launch
timeout — pointing at the app, which was fine.

Confirmed on the machine rather than reasoned about: `EBWebView` under
`com.cortex.kurdish-speech` last written by the running app, 34 live `msedgewebview2` processes.

**This is why the gate has been a coin toss on machine state.** It passed on 2026-07-29 because the app
happened to be down for a rebuild. Same class as the watchdog drill before `CORTEX_WATCHDOG_EXE`: a
verification whose green depends on what else is running is not a verification.

Fix: give the run its own WebView2 folder (`<disposable profile>/webview2`), overridable, created up
front. Measured with the owner's app **running** both times:

| | |
|---|---|
| before | `FAIL real-app-e2e 92.0s` — port never came up |
| after (env var, proving the mechanism) | exit **0**, real Sorani out of OmniASR |
| after (harness default, proving the shipped path) | exit **0** |

The transcript the passing run produced, from the real fixture through real VAD + real CTC:
`بوو پێش هاتنی سوپا هایەتی لەوتەی ساڵی ١ە تووشی کەشەی پیوەست بەنەخۆشەکەن نەبوو`

The isolation contract in `test_real_data_runner_policy.py` gains its fifth leg, and it is a real guard
— removing the line from the harness fails it:
`AssertionError: e2e_real_app.cjs is missing: WEBVIEW2_USER_DATA_FOLDER: WEBVIEW2_DIR` (exit 1 → 0).

**No rebuild was needed for this fix** — `e2e_real_app.cjs` and the policy script are not on
`check_exe_freshness.py`'s source surfaces, so the app and the reviewer link stayed up throughout.

**A trap worth writing down, because it nearly shipped.** The fail-before harness rewrote
`diarization.rs` with `Set-Content -Encoding utf8`, and **PowerShell 5.1's `utf8` writes a BOM**. The
script's own "file restored byte-identical" check compared `Get-Content -Raw` strings — which strip the
BOM — so it reported True while the file on disk had gained three bytes. Only a byte-level check caught
it (`git diff --numstat` 65/26 → 64/25 after stripping). Same family as the PS 5.1 restricted-header
trap already recorded in `REVIEWER_UX_10_PLAN.md`: on this box, **verify a PowerShell round-trip at the
byte level or not at all.** No other `.rs` in `src-tauri/src` carries a BOM (swept).

**And it bit a second time, in the same iteration.** Appending this entry with
`Get-Content | Add-Content -Encoding utf8` double-encoded every em-dash to `â€”` — `Get-Content`
without `-Encoding utf8` reads a UTF-8 file as ANSI, and the write then re-encodes the mis-read
characters. 16 occurrences, all inside the 133 appended lines; `git diff --numstat` showed `133 0`
(pure additions) and the committed file had zero, so the damage was bounded and the append was redone
through `[IO.File]::ReadAllText(..., UTF8)` + `AppendAllText`. Twice in one iteration is not bad luck,
it is the tool: **PowerShell 5.1 is not safe for round-tripping UTF-8 text files here.**

### Closing state

`python scripts/verify_10.py --status-md docs/STATUS.md`, `CORTEX_AUDIO` set, **and the owner's app
deliberately left RUNNING** — the exact condition that produced the RED:

```
kept gates run: 23 - 23 PASS, 0 FAIL, 0 skipped (env/not-built)
owner-descoped: 8   owner-gated pending: 5
VERDICT: GREEN - PERSONAL-USE SHIP-READY. (Not full-charter 10/10: 8 legs owner-descoped, 5 owner-gated pending.)
```

`real-app-e2e` **18.4s PASS** where it was 92.0s FAIL an hour earlier, on the same machine, with the
same app running. That delta is the whole proof.

**Other gates (unmasked).** `cargo test --lib` **0** — 1107 passed, 0 failed, 7 ignored (1106 + the new
one). `cargo clippy --all-targets --all-features -D warnings` **0**. `cargo fmt --check` **0**.
`npm run test:python-policies` **0** — 45/45. `node --check` **0**.

**App and link, verified after the rebuild and still up at close:** `EXE FRESHNESS GATE: OK (exe at
HEAD 4bf3695…)`, claim **200**, queue **200**, items **29**, `pendingTotal` **112**, watchdog re-armed.
The WebView2 fix needed no rebuild — neither file is a `check_exe_freshness.py` source surface — so the
reviewer link never dropped for it.

**What is still owner-gated, unchanged by this iteration:** Tailscale Serve → Funnel; one elevated
`cortex-once-admin.ps1` run (a reboot still leaves the review server down); the R5 device hour; native
Sorani review of the 8 `source: null` strings; the Phase-6 scope amendment and the "unsure" verdict
decision. And from the diarization work: flagging the 17 known mixed clips, and overlap detection.

## Iteration 227 — which clips, by name; and two claims that did not survive checking

The owner's second request from the diarization work: **flag the mixed clips so he knows before opening
one.** Iteration 225 established the rate (17/144, 11.8%) but the probe printed only the ten
lowest-similarity clips — which cannot answer "which ones", while looking like it does.

**The measurement, live and read-only (`SQLITE_OPEN_READ_ONLY`, app running throughout):**

```
APPLIED TO THE WHOLE LIBRARY: 17 / 144 clips (11.8%) score below 0.59
of those, still awaiting review: 13
```

| id | cosine | s | stored label | state |
|---|---|---|---|---|
| 0817584d | 0.3054 | 14.5 | SPEAKER_03 | reviewed |
| f684c691 | 0.4121 | 15.0 | SPEAKER_07 | pending |
| 97370a88 | 0.4149 | 14.8 | SPEAKER_03 | pending |
| 6f23f57d | 0.4268 | 14.4 | SPEAKER_07 | pending |
| 290f5f58 | 0.4275 | 10.6 | SPEAKER_07 | reviewed |
| e6052156 | 0.4372 | 14.4 | SPEAKER_03 | pending |
| 2f746f49 | 0.4600 | 12.2 | SPEAKER_07 | reviewed |
| 420f9c07 | 0.4695 | 13.5 | SPEAKER_03 | pending |
| 629bfd3b | 0.4853 | 11.2 | SPEAKER_03 | pending |
| 492f59a8 | 0.5036 | 13.0 | SPEAKER_00 | pending |
| 22cf0ec7 | 0.5161 | 14.1 | SPEAKER_03 | reviewed |
| 50c6f552 | 0.5233 | 11.0 | SPEAKER_03 | pending |
| e614cb77 | 0.5359 | 12.8 | SPEAKER_07 | pending |
| 4aa82a8b | 0.5559 | 12.0 | SPEAKER_07 | pending |
| f09609e9 | 0.5591 | 10.7 | SPEAKER_04 | pending |
| bc7e301e | 0.5593 | 11.3 | SPEAKER_07 | pending |
| 47f438a1 | 0.5734 | 11.1 | SPEAKER_03 | pending |

**14 of 17 carry SPEAKER_03 or SPEAKER_07** — the corpus's two dominant labels (52 and 35 segments).
That is exactly the shape a two-person conversation produces when a turn boundary falls inside a
silence-planned chunk, which is the mechanism this probe exists to detect. The stored label is printed
*because* the measurement contradicts it: every one of these rows asserts a single speaker where two
are talking, and `speaker_id` ships in the CSV/JSONL/Parquet/HuggingFace exports and in the dataset-card
composition stats.

**Deliberately not done: rewriting those labels.** The 0.59 threshold is calibrated on 15 clips the
owner labelled by ear. Rewriting a live corpus on that basis is his decision, and recording the flag
durably needs a schema migration — the riskiest change class in this repo (see the STRICT
`speech_segments` work, still owner-gated). This iteration makes the finding reproducible and
reversible; it changes no data.

### Two claims that did not survive checking

Both were written up in my head as defects before being verified. Neither is one.

1. **"Wrong speaker labels leak voices across the train/test split."** `export::assign_splits` groups by
   connected components of the bipartite (recording, speaker) graph, and **all 144 segments share one
   `audio_path`** — so they form one component and land in one split regardless of any label. The
   recording-level grouping already defends against this; a mislabelled speaker *within* one recording
   cannot straddle splits. **No leakage.**
2. **"The P0.4 provenance trio is not exported."** `export.rs` indeed has no `vad_backend`/`denoised`/
   `diarized` column — but `export_bundle.rs` reports all three in the bundle's provenance JSON, with
   tests pinning `applied`/`notApplied`/`notRecorded`. `audio.rs`'s "reported in the export" is
   accurate. **Not a defect.**

Recorded as plainly as fixes, because the alternative was two confident, wrong entries. (Measured while
there: `diarized`, `denoised` and `vad_backend` are NULL for all 144 rows — correct, not a bug: these
segments were imported before migrations v41/v42 added the columns, so "not recorded" is the truth.)

### Root cause, not symptom

Adding `verified` to the probe's row broke two unrelated call sites and a `let Some((..)) =` — the
segment row was a bare 6-tuple spelled out in three signatures. **Same shape as the `Reply` 3→4 tuple
breakage earlier in this loop (~39 test destructurings).** Fixed as a named `SegRow`, with the
speaker/pending lookup moved into the function that prints it, matching how `export_listening_set`
already resolves rows. Twice is a pattern, so it went to memory rather than just the ledger.

**Two process wins worth keeping:**
* `cargo check` runs `build.rs`, which copies `onnxruntime.dll` — locked while the app is up
  (`os error 32`). Pointing `CARGO_TARGET_DIR` at a scratch dir type-checks with the app running and
  the reviewer link up. That caught the second compile error without spending a restart.
* The bundled rebuild script's fallback fired for real for the first time: the build failed on my broken
  edit, and it relaunched the old exe and re-armed the watchdog anyway. **The link came back with a
  broken build in the tree** — claim 200, queue 200, items 29, pendingTotal 112.
* Ordering lesson learned the expensive way: run `cargo fmt` **before** the bundled rebuild. Formatting
  after it re-stales the exe by mtime and buys another restart.

**Gates (unmasked).** `cargo test --lib` **0** — 1107 passed, 0 failed, 7 ignored. `cargo clippy
--all-targets --all-features -D warnings` **0**. `cargo fmt --check` **0**.

## Iteration 228 — the cleanup that passed every gate and deleted nothing

Self-audit of iteration 226's own WebView2 isolation, following the rule that has held all loop: **the
defects are in what this loop just wrote.**

Isolating the browser profile was right, but it made an existing leak grow faster. Nothing has ever
removed the harness's per-run profile, and each run now leaves a ~11 MB WebView2 profile on top of the
DB, media cache and settings. Measured before touching anything: **34 stale `cortex-e2e-*` directories,
764 MB** on the owner's box.

**The first fix passed every gate and did nothing.** `node --check` **0**, the new policy test **0**, the
real-app e2e gate **exit 0** — and the leak went **34 → 35**. The only thing that caught it was counting
directories on disk before and after:

```
==> Could not remove the disposable profile ...\cortex-e2e-l9TgG7:
    EPERM, Permission denied
```

`taskkill /F /T` returns when the kill is **signalled**, not when Windows has released the SQLite
`-wal`/`-shm`/`cortex.lock` handles and the `msedgewebview2` children. This is the DELETE side of the
write-then-read flakiness already recorded for this machine. A bounded retry (500 ms to a 15 s deadline)
fixes it, and is **non-fatal on timeout** — a leaked temp directory must never turn a passing
verification run into a failure.

Proven by observable effect rather than by the test passing, 3-for-3, owner's app running throughout:

| run | exit | dirs before → after | cleanup line |
|---|---|---|---|
| 1 | 0 | 35 → 35 | `Removed the disposable profile …c3BepH` |
| 2 | 0 | 35 → 35 | 1 |
| 3 | 0 | 35 → 35 | 1 |

**Three guards, mirroring `Remove-TemporaryFixtureDir` in `scripts/test-real-data.ps1`** — same repo,
same reasoning, and a recursive delete earns a guard here for the same reason it does there: only a
directory THIS run minted (never a caller-supplied `CORTEX_APP_DATA_DIR`); only strictly BELOW the temp
root (equality rejected, or a bare `%TEMP%` would be removable); and **never on the failure path**,
because the profile is the only copy of that run's DB and a gate that destroys its own evidence is worse
than one that leaves a directory behind. The failure handler prints where it kept it.

**The policy test needed its own fix.** It matched `rmSync` inside the COMMENT explaining `rmSync`, so it
failed on prose — brittle enough to block a legitimate future edit. It now strips comment lines first,
and the guard assertion is proven to bite: replacing the temp-root check with `if (false)` fails it with
`AssertionError: e2e_real_app.cjs is missing: target === root || !target.startsWith(root + path.sep)`
(exit 1 → 0 restored).

**The lesson, stated once because it cost two rounds tonight:** a guard test proves the guards EXIST; it
can never prove the operation WORKED. For cleanup, deletion, or anything whose product is a side effect
on the world, assert the observable effect. "Tests pass" was necessary and not sufficient here, exactly
as CLAUDE.md says.

**Not done, deliberately:** the 764 MB already on disk is the owner's to clear
(`Get-ChildItem $env:TEMP -Directory -Filter 'cortex-e2e-*' | Remove-Item -Recurse -Force`). The fix
stops the growth without deleting anything that predates it.

**No rebuild needed** — neither file is on `check_exe_freshness.py`'s source surfaces, so the app and the
reviewer link stayed up for the whole iteration.

**Gates (unmasked).** `node --check` **0**. `npm run test:python-policies` **0** — 45/45. Real-app e2e
**exit 0 ×3**, each with the disposable profile removed.

## Iteration 230 — the aligner was installed, loaded, and structurally unreachable

Target (1) of the new loop, and it is a **live** defect rather than a latent one: `auto_align` is `true`
in the owner's settings, so `enqueue_background_alignments` runs on every import.

**The chain, each link measured rather than assumed.** That method spawns a detached `move` thread which
never captures `self`, so the model root was never in scope — and the path was wired to a free
`aligner::align()` helper that returned `fallback_align` **unconditionally** and had no channel to report
`AlignmentQuality` at all. Its call site therefore hardcoded `EnergyHeuristic`.

**The fail-before is the owner's own library:**

| `alignment_quality` | segments |
|---|---|
| `energy_heuristic` | **129** |
| `ctc_forced` | 15 |

The 15 came from the FOREGROUND path (`Pipeline::align` ← `commands/transcribe.rs`), which proves forced
alignment works on this exact machine, models and corpus. So ~90% of the library carried heuristic word
timings — and `quality.rs` raises a review-risk reason on exactly `energy_heuristic`, so those clips
carried a **false risk flag** on top.

**A correction to my own earlier claim.** When I first read this I said `ForcedAligner::align` might have
NO production caller. It does — `pipeline.rs:3369`, reached from `commands/transcribe.rs:201`. Only the
BACKGROUND path was bypassing it. Stated here because that claim went to the owner before it was checked.

**The fix.** Resolve the model root before the spawn (that absence is *why* the stub was used), build ONE
`ForcedAligner` above the per-file loop, and persist the quality the aligner actually achieved. Building
once matters: `new` loads a ~365 MB ONNX session, so the per-call construction `Pipeline::align` can
afford for a single clip would make a whole import unusable. `MAX_ALIGN_SECS` is 600 and the owner's
clips are 10–15 s, so they are well inside the CTC path — verified before claiming the fix helps. A
missing model is still not an error: `new` succeeds with no session and `align` reports
`EnergyHeuristic` honestly, which is the correct answer when nothing better exists.

**Both free stubs deleted** rather than left as "convenience wrappers":
* `align()` — fallback-only, one caller, no quality channel.
* `score_consistency()` — returned the constant `-5.0` with **zero** callers, one `use` away from being
  persisted to `ctc_score` as a fake acoustic score. Under this repo's honesty law that is the worst kind
  of dead code: a fabricated metric waiting for a caller.

**Proofs.**
* New real-model test pins the MODEL-PRESENT case, which nothing covered — `aligner.rs` pinned only the
  no-model negative: `[aligner-gate] 2 words, quality=ctc_forced`, `1 passed`, 1.85s.
* New policy gate (`test_background_alignment_policy.py`) bites on the pre-fix source, and it exists
  because **nothing in the Rust suite can** — the persist happens inside a spawned thread in a private
  method: `AssertionError: background alignment calls the free aligner::align() helper, which ignores any
  loaded model and can only ever produce the energy heuristic` (exit 1 → 0, restored byte-clean).

**Gates (unmasked).** `cargo test --lib` **0** — 1107 passed, 0 failed, 7 ignored. `cargo clippy
--all-targets --all-features -D warnings` **0**. `cargo fmt --check` **0**. `npm run test:python-policies`
**0** — **46/46** (was 45; the runner auto-discovers `scripts/test_*.py`).

**Cost of a mis-ordered rebuild, recorded so it stops recurring.** I rebuilt while the changes were still
uncommitted: the exe bakes HEAD's SHA, so the following commit would have made a perfectly good exe read
as "NOT HEAD" and turned verify-10 RED. The recipe is fixed and now in memory: **`cargo fmt` → commit →
bundled rebuild**, with ledger/STATUS commits after (they are not source surfaces, and the gate forgives
HEAD advancing via non-source commits). Cost one extra stop-app/build/relaunch cycle.

**NOT done, and it is the owner's call:** the 129 already-stamped segments keep their heuristic timings.
Re-aligning existing rows rewrites his library; the fix only changes what happens from here.

## Iteration 231 — target (2) resolved: the COMMENT was wrong, not the UI

`update_segment_bounds` has no frontend caller and no trim control exists anywhere in `src/`. The loop
asked which of those two facts was the defect. `git log -S` answers it: the frontend wrapper was deleted
on **2026-07-15 by `1167504`** ("chore(audit): batches 2+3") as one of *"20 dead command wrappers"* — and
it was **already unused then**. No trim UI has ever shipped.

So the claim in `chunking.rs` — that the trim path is "the reviewer's most-used edit" — was **invented**,
and invented two weeks AFTER the caller was deleted, in a doc comment written to justify iteration 224's
hardening change. The hardening itself is still worth having (the whitelist-rebuild really was a
silent-data-loss shape for whoever wires a trim control later); the traffic claim was not measured and is
now **retracted in place**, with the function's real status recorded: reachable only from an orphaned IPC
command. Retracted rather than quietly deleted, because a silently-corrected lie teaches nothing.

### Target (3), the two cuts verification supports

| cut | why |
|---|---|
| `features.rs` — 473 lines + 16 unit tests | 80-bin mel-filterbank extractor. Its production consumer was the fbank diarization fallback, deleted earlier for not being speaker-discriminative. After that its ONLY caller was an `#[ignore]d` test that tested `FbankExtractor` **itself**. sherpa-onnx computes its own features for every model this app runs. |
| `flate2` | Zero references under any name (`GzEncoder`/`ZlibEncoder`/`flate2::`). Its manifest comment claimed "compression ratio for ASR repetition detection"; no such code exists. |
| `rustfft` | Only `features.rs` used it. |

**CORRECTION to my own ponytail audit, and it matters.** This is a **code cut, not a build cut**. Both
crates remain in the dependency tree transitively — `flate2` via `png` and `ureq`, `rustfft` via
`symphonia-core` — so nothing compiles faster and the supply-chain surface is unchanged. Removing them
from `Cargo.toml` drops a direct declaration this crate no longer earns, and nothing more. The audit's
"net: -3 deps possible" was wrong in exactly the sense a reader would care about, and is retracted here.

**The lib test count DROPS 1107 → 1091, and that is correct.** `features.rs` carried 16 unit tests and
they went with the module. Recorded explicitly because a falling test count normally means a regression,
and a future reader diffing these entries deserves the reason rather than a mystery.

Diff: **-514 / +13** across 6 files.

**Gates (unmasked).** `cargo test --lib` **0** — 1091 passed, 0 failed, 7 ignored. `cargo clippy
--all-targets --all-features -D warnings` **0**. `cargo fmt --check` **0**. `npm run test:python-policies`
**0** — 46/46. Exe rebuilt at HEAD; `EXE FRESHNESS GATE: OK`; claim **200**, queue **200**, items 29,
`pendingTotal` 112.

**NOT done, deliberately deferred:** the 17 orphaned IPC commands (including `update_segment_bounds`
itself). That is a 597-line sweep across many files where each removal needs its own caller check and may
strand private helpers; batching it onto the tail of this iteration would be exactly the kind of
unverified bulk change this loop exists to avoid. It gets its own iteration.

## Iteration 232 — the deletion that turned verify-10 RED, and why the search was wrong

Iteration 231's `features.rs` cut broke `fuzz-smoke`:

```
error[E0432]: unresolved import `cortex_speech_app_lib::features`
 --> fuzz_targets/features.rs:4:28
=> FAIL   fuzz-smoke   425.9s
VERDICT: RED - 1 kept gate(s) failed (fuzz-smoke). NOT ship-ready.
```

**The miss is mine and it is worth naming precisely.** The caller search behind that cut covered `src/`,
`tests/` and `benches/` and stopped there. `src-tauri/fuzz/` was never looked at. So "the only caller is
an `#[ignore]d` test of itself" was **wrong — not because the reasoning was wrong, but because the search
was incomplete.** There was a fifth consumer the whole time.

This is the SECOND incomplete-search miss in this session. The first (`src/**/*.svelte` without globstar,
while counting uncalled IPC commands) I caught before reporting; this one I did not, and only the gate
caught it. The rule that generalises, now in the loop prompt: **a deletion claim needs a repo-wide search
with NO path filter, and the compiler is the only real authority on reachability.** A grep proves
presence, never absence.

**The resolution was still to delete, not to restore.** The fuzz target's own comment already said it:
*"FbankExtractor has no production callers at all today (every call site is a test, at 16 kHz)"* — it was
fuzzing known-dead code. Keeping 473 lines of dead production code alive to satisfy a fuzz-target count
would be the tail wagging the dog.

**The charter edit, flagged rather than buried.** `AGENT_CHARTER.md` required "5 fuzz targets in CI". That
number is NOT quietly lowered to 4 — it is made **count-agnostic** ("EVERY fuzz target"), which is
*stricter*: a fixed count can be satisfied while coverage rots, and it goes stale the moment a target
legitimately retires with its code. The same wording now appears in `verify_10.py`'s gate description and
the nightly workflow comment, so the three cannot disagree with each other. **This edits a normative
document and is the owner's to confirm or revert.**

Also corrected: `CORTEX_APP_FLOW_GUIDE.html` advertised `rustfft (mel features)` in the current
architecture. That path no longer exists.

**Measured before committing** (not "the tests passed"):

```
cargo +nightly fuzz list  ->  cache, diff, normalizer, validation      exit 0
fuzz normalizer exit 0   fuzz diff exit 0
fuzz validation exit 0   fuzz cache exit 0                             (30s each)
```

**And the gate itself, after:** `23 - 23 PASS, 0 FAIL, 0 skipped` —
`VERDICT: GREEN - PERSONAL-USE SHIP-READY`. `fuzz-smoke` **PASS 557.3s** where it was **FAIL 425.9s**.

**No rebuild was needed** — `src-tauri/fuzz/`, `scripts/` and the docs are not on
`check_exe_freshness.py`'s source surfaces, so the app and the reviewer link stayed up throughout the
RED and the fix. A `docs/STATUS.md` carrying the RED verdict was deliberately NOT committed; it was
regenerated after the gate went green, so the generated record never asserts a state that was superseded
minutes later.

## Iteration 233 — "17 dead IPC commands" was wrong: 11 of them are load-bearing for gates

Target (3) was to cut the 17 orphaned IPC commands my own ponytail audit found. **The audit was wrong,
and the correct action this iteration was to cut nothing.**

**What the audit actually measured.** It searched for frontend `invoke(...)` callers and found 17
commands with none. That is a true statement about the frontend and a false proxy for "dead". Re-run
repo-wide with **no path filter** — the discipline that cutting `features.rs` cost a RED gate to learn —
the picture inverts:

| commands | referenced by | cuttable? |
|---|---|---|
| 11 of 17 | `test_command_main_thread_policy.py`, `test_ui_thread_blocking_audit.py`, `test_rust_runtime_panic_policy.py`, `test_agentic_pipeline_policy.py`, `test_restore_reservation_gate.py` | **NO — pinned by a gate** |
| `get_blocking_validation_issues` | sole caller of `export_bundle::blocking_issues` | no — strands real export-gating logic |
| `get_import_status`, `update_segment_bounds`, `db_wal_checkpoint` | behaviour/comment coupling; `update_segment_bounds` guards the deliberately-hardened `rebound_alignment_json` | not worth a rebuild for ~10 lines |
| `clear_cache`, `get_cache_info` | nothing (2-line bodies) | yes, but see below |

`test_command_main_thread_policy.py` is a **ratchet**: it pins command names and asserts each exists and
is `async`. Deleting a pinned command either fails that gate or gets "fixed" by trimming the list —
silently shrinking a ratchet whose entire purpose is to never shrink. That is weakening a gate by
accident, which this loop forbids, and I would have done it on the audit's say-so.

**Nothing was cut.** The only genuinely-free cuts left are `clear_cache` and `get_cache_info` — two
2-line bodies. Cutting them costs a full stop-app → rebuild → relaunch cycle (a real reviewer-link
outage) to remove ~10 lines. Chasing the audit's headline number at that price would be exactly the
behaviour the "prefer changes needing no rebuild" rule exists to prevent. Left in place, deliberately.

**What shipped instead: the finding made durable.** A header block in
`test_command_main_thread_policy.py` records why "no frontend caller" ≠ "dead" in this repo, names the
three ways a command can be load-bearing, and requires a no-path-filter repo-wide search before any
command is cut. The next audit — human or agent — reads it before proposing the same 597-line deletion.
No rebuild: `scripts/` is not an exe-freshness source surface.

### Null result: the nightly mutation job's health is NOT observable from here

Target (4) asked whether the nightly mutation gate is actually healthy. **I cannot tell, and am not
guessing.** `gh` is not installed on this machine, so the Actions run history is unreachable; that part
is owner-gated (the Actions tab answers it in seconds).

What I *did* verify by reading `.github/workflows/nightly-real-audio.yml`:
* The `git diff --relative` fix the ledger records is present — the failure mode where every hunk was
  rejected with "Diff content doesn't match source file" is closed.
* The vacuous-pass paths are guarded and annotated: no commits in 24 h emits
  `::warning ... This is not a pass.`, and an empty core-module diff emits `::notice ... idle`.
* `set -o pipefail` is set, so a failing `cargo mutants` cannot be masked by the pipeline.

**The honest weakness, stated rather than fixed:** both idle paths `exit 0`, so a quiet day shows a
GREEN mutation gate with only an annotation to say otherwise. Making idle days RED would be worse (every
quiet day fails), so this is a designed trade-off, not a defect — but "green" there means "nothing to
mutate", not "mutants killed", and nobody reading the badge would know.

**Gates.** `npm run test:python-policies` **0** — 46/46. No Rust change, so no rebuild and no other gate
was disturbed; the app and reviewer link stayed up for the whole iteration.

## Iteration 234 — target (4) was already done; the loop was carrying a stale worklist

Target (4) listed four "STILL UNTESTED" couch-review scenarios. **All four are covered by named,
non-vacuous tests that pass today.** Nothing was untested, and the honest output of this iteration is
that correction rather than four duplicate tests.

| "untested" item | the test that covers it | result |
|---|---|---|
| spot-check accounting across many batches | `spot_check_accounting_survives_a_long_multi_batch_session` | ok, 0.37s |
| lease expiry with a backgrounded page | `a_lease_that_lapsed_while_the_phone_slept_is_reclaimable_but_never_stolen_silently` | ok, 0.09s |
| spot-check answer across Stop/Start | `a_spot_check_served_before_a_restart_is_still_scored_after_it` | ok, 0.11s |
| the check-then-act race | `three_reviewers_hammering_at_once_never_double_decide_or_lose_a_clip` | ok, 0.62s |

**Checked for vacuity rather than trusting the names**, because a test named for a scenario need not
exercise it:
* the multi-batch one is 88 lines with 10 assertions, forces `work_done >= QUEUE_BATCH * 4` so the
  session genuinely spans several batches, and asserts both that no check is served twice to the same
  reviewer and that an answer-key clip never acquires a `reviewed_by`;
* the lapsed-lease one is 55 lines with 7 assertions covering reclaim-by-holder, availability to a
  second reviewer after expiry, a 409 on renew once someone else holds it, and that the refused renew
  does not steal the clip back;
* the race one uses REAL `std::thread::spawn` concurrency, not a simulated interleaving.

**This is the failure mode the loop prompt itself warns about, and I committed it.** The rule is "verify
gate state from `docs/STATUS.md` and `git log`, never a ledger sentence" — and the STATE block listing
these four items was carried forward verbatim across many iterations without ever being re-checked
against the test module. Had I acted on it, I would have written four duplicate tests and reported them
as new coverage.

Two supporting facts confirmed while verifying, both by reading the code rather than assuming:
* Cross-batch spot-check accounting cannot double-count: `list_spot_check_candidates` excludes
  `id NOT IN (SELECT segment_id FROM spot_checks WHERE reviewer = ?1)`, so a scored clip is never
  re-served to that reviewer. The served-set is also bounded — an unanswered check is re-served rather
  than accumulating, because `HashSet::insert` returns false for one already present.
* The check-then-act class is guarded structurally, not by luck: `api_decision` reads `prev` for its
  validation but builds the upsert from a **freshly re-read row** (couch.rs:1619), and the collision
  guard checks and claims under ONE lock. couch.rs:1662 names this explicitly.

**Gates.** The four tests above, run individually: 4/4 ok. No source change, so no rebuild, no other gate
disturbed, and the app and reviewer link stayed up for the whole iteration.

**The loop's target (4) is now closed** and the prompt updated so the next iteration does not re-derive
it. Remaining genuinely-open engineering: the ungated criterion benches. Everything else on the list is
owner-gated.

## Iteration 235 — a "baseline-regression gate" that could not regress

The stated open item was the ungated criterion benches. Verifying that claim turned up something worse
in the leg right next to it.

`scripts/verify_10.py` registers `rtf-bench` as **"Latency: RTF on this rig (baseline-regression gate:
WS4)"**. Its only RTF assertion was:

```rust
assert!(rtf.is_finite() && rtf > 0.0, "RTF must be a finite positive measurement, got {rtf}");
```

True of **any** positive number. Proven vacuous by injection rather than argued:

| | injected RTF | assertion | result |
|---|---|---|---|
| A | 99.0 | old only | **exit 0 — PASSED** |
| B | 99.0 | new budget | exit 101 — FAILED, budget fired |
| C | real 0.1784 | new budget | exit 0 — passed |

**Row A is the finding: a 560× latency regression passed a gate advertised as catching regressions.**
This is the same vacuous-pass shape the repo guards against explicitly elsewhere — the fuzz leg refuses
to report a pass on an empty target list for exactly this reason, and the mutation job annotates its
idle path. This leg had no such guard.

**The original reasoning was sound as far as it went, and is worth preserving rather than dismissing.**
The doc comment said a hard threshold belongs on a NAMED reference machine (charter M4.1), not the
default suite — correct, absolute timing is machine-dependent. What it missed: the test is `#[ignore]`,
so it never runs in the default suite anyway, and its only automated caller **is** verify-10, which is by
definition the personal-use gate on that named rig. The precondition for asserting was already met.

**The number is the charter's, not one invented here.** `AGENT_CHARTER.md`: *"CTC-300M RTF<=0.3 CPU,
<=0.1 GPU on the named reference machine"*. Measured on this rig before wiring it, five runs total
across the proof: **0.1765 / 0.1771 / 0.1775 / 0.1784 / 0.1784** — stable to ±0.6%, **59% of budget with
1.69× headroom**. The threshold is nowhere near the noise floor, so it fails on a real regression rather
than on jitter. GPU is deliberately not asserted: this leg runs CPU int8, as its own printed line says.

**Gates.** `cargo fmt --check` **0**; `cargo clippy --all-targets --all-features -D warnings` **0**; full
sweep `23 - 23 PASS, 0 FAIL, 0 skipped` — `VERDICT: GREEN`, with `rtf-bench` PASS 19.0s under the live
budget. **No rebuild**: `src-tauri/tests/` is not on `check_exe_freshness.py`'s source surfaces
(`SOURCE_DIRS` is `src` + `src-tauri/src`), so the app and the reviewer link stayed up throughout.

### The criterion benches: NOT deleted, and not half-wired

The loop asked to decide between wiring them and deleting them. **Deleting is not available**:
`AGENT_CHARTER.md:67` requires *"criterion benches gated on every PR with a >5% wall-clock regression
budget via github-action-benchmark against a committed baseline"*, and line 122 repeats it in the
per-PR protocol. Wiring them properly needs a new CI workflow plus a committed baseline — real
infrastructure, and owner/infra-gated.

Half-wiring it locally (a `make bench` target nothing enforces) would produce precisely the artefact
this iteration just spent its time removing: a gate-shaped thing that cannot fail. Surfaced as infra
work rather than faked.

### Where this leaves the loop

**No non-owner-gated engineering remains.** Everything still open needs the owner: Tailscale
Serve→Funnel; the elevated `cortex-once-admin.ps1` run (a reboot still leaves the review server down);
the R5 device hour; native Sorani review of the 8 `source: null` strings; the Phase-6 scope and "unsure"
verdict decisions; whether to re-align the 129 `energy_heuristic` segments; the 17 mixed-speaker clips;
the count-agnostic fuzz requirement from iteration 232 pending confirmation; and the criterion-bench CI
wiring above.

## Iteration 236 — the 129 heuristic segments re-aligned, on the owner's word

Owner decision on the item surfaced in iteration 230: re-align the backlog the old stub stamped. Iteration
230 fixed the code, but a code fix only changes what happens from now on — the already-written rows kept
their heuristic timings until something re-aligned them.

**The tool** (`src/bin/realign_segments.rs`) is a deliberate one-off, DRY RUN by default, and uses
`ProcessingPipeline::align` — the production path, which prefers real CTC forced alignment from the
fine-tuned MMS-CTC model and falls back to the bundled `mms_aligner.onnx`. Settings and models resolve
exactly as `lib.rs` does them, so it cannot align with a configuration the app could never reproduce.

**Dry run first, app left running (it writes nothing):**

```
144 segments, 129 not ctc_forced
result after 392s:  ctc_forced 129 | skipped 0 | no words 0 | align failed 0
DRY RUN — nothing was written.
```

**Then the apply, with the app STOPPED so the library never had a second writer**, and a snapshot taken
through the SQLite **backup API** rather than a file copy — one consistent file, no `-wal`/`-shm`
juggling, and the script refuses to write at all if the snapshot fails:

`%APPDATA%\cortex-speech\backup-before-realign-20260801-133646\cortex-speech.db` (144 rows).

**Observable effect, verified against the snapshot rather than trusted from the label:**

| | |
|---|---|
| `alignment_quality` | 129 `energy_heuristic` + 15 `ctc_forced` → **144 `ctc_forced`, 0 heuristic** |
| word arrays CHANGED | **129** |
| word arrays unchanged | 15 — exactly the already-forced ones, untouched |
| **source offsets moved** | **0** |
| **transcripts differing** | **0** |
| verified rows | 32, unchanged |
| rows with a human decision | 32, unchanged |
| `pendingTotal` | 112, unchanged |

**A label flip would have satisfied the first row and none of the rest**, which is why the word arrays
were diffed. One example of what actually changed, first word of `19d21006`:

```
before  {"word":"بەو","start":0.34,  "end":0.8328, "confidence":0.5}
after   {"word":"بەو","start":0.8208,"end":0.9409, "confidence":0.9784}
```

`confidence: 0.5` is the flat constant `fallback_align` writes for every word — the heuristic had no
opinion. Forced alignment gives 0.978 and moves the word ~0.48 s later. Linear interpolation had placed
the first word half a second off; that is the size of the error the whole corpus carried.

**Why this was safe by construction, not by care:** `update_segment_alignment` writes
`alignment_json` + `alignment_quality` + `updated_at` and nothing else, and the new JSON is built with
`merge_word_timestamps`, which preserves `source_start_ms`/`source_end_ms`. Every reader that slices a
clip by those offsets — phone playback, dataset audio export, the 7B re-transcribe client, jury acoustic
scoring — sees identical bytes. The tool also refuses to write an empty word list, since replacing real
timings with nothing is worse than leaving heuristic ones; that guard was never exercised (0 empty).

**Cost, measured:** 389 s for 129 segments (~3 s each). `ProcessingPipeline::align` re-decodes the source
per call and all 129 clips come from one 172 MB file, so most of that is redundant decode+hash work. Left
alone deliberately: this is a one-off, and optimising it would mean diverging from the production path,
which is the one property that makes its output trustworthy.

**Gates.** `cargo fmt --check` **0**; `cargo clippy --all-targets --all-features -D warnings` **0**; exe
freshness OK at HEAD; claim **200**, queue **200**, items 29, `pendingTotal` 112, watchdog re-armed.

**One owner-gated item closes.** The 17 mixed-speaker clips, the R5 hour, Tailscale, the admin script,
the 8 Sorani strings, the two verdict decisions and the charter fuzz edit all remain.

## Iteration 237 — the 17 mixed-speaker clips flagged, where the decision is made

Owner instruction: flag them. They were a number in a console printout; nothing on the row said so, and
nothing the reviewer sees said so.

**Re-measured first, read-only, with the app up.** 17 / 144 (11.8%), 13 still pending — identical to the
figure from before iteration 236's re-alignment, and the five ground-truth "multi" clips came back at
their recorded cosines (0.3054 / 0.4121 / 0.4149 / 0.4268 / 0.4275 against 0.305 / 0.412 / 0.415 / 0.427
/ 0.428). That is an independent second confirmation that the re-align moved no clip boundaries.

**The fail-before is the finding, and it was live rather than staged.** The server was asked for the
batch it is actually serving:

```
items 29   keys: durationMs, id, speakerId, text        <- no speakerChange, on any item
4 of the 13 pending flagged clips were IN that batch
```

A reviewer opening the link today meets four two-speaker clips presented as ordinary work, each carrying
one authoritative `SPEAKER_xx` — because chunks are cut on SILENCE and the label is attached to the whole
chunk afterwards, however many people are in it. "Looks good" walks two voices into a single-speaker
corpus with a confident wrong label.

**After, on the same live queue:**

| | |
|---|---|
| item keys | + `speakerChange` |
| flagged in the served batch | **4** — `420f9c07`, `47f438a1`, `492f59a8`, `4aa82a8b` |
| clips whose text / duration / speakerId changed | **0** |
| `pendingTotal` | 112 → 112 |
| library | 144 measured, **17 flagged, 13 pending** |

**Migration v47 stores the SCORE, not a verdict.** Same reason `snr_db` and `clipping_ratio` are numbers:
0.59 is a calibration derived from the owner's blind listening pass and can be re-derived, while a stored
boolean would freeze today's threshold into the data. Readers compare against
`diarization::SPEAKER_CHANGE_THRESHOLD` — which the probe had been redefining locally, two copies of one
number that had to agree with nothing making them; it now imports the one whose doc comment carries the
derivation.

**NULL means NOT MEASURED and never "measured, one speaker."** Every pre-v47 row is NULL and so is every
future import — the import path does not run this (two extra CAM++ embeddings per chunk, and the 0.59
calibration was measured on ~14 s clips, so applying it to whatever length the planner emits would be a
threshold used outside its measured range). The phone draws nothing for NULL. Absence of a measurement is
not evidence of a single speaker, and the badge never implies it is; that is the third page test.

**Two write paths, deliberately opposite:**
* `insert_segment` does NOT carry the column — its upsert would write a caller's `None` over a real
  measurement on every ordinary edit.
* `insert_segment_full` DOES — a restore after delete runs as a fresh INSERT, so omitting it would
  silently un-flag a two-speaker clip on delete+undo, and re-measuring costs a full library pass nobody
  would know to run.

**Fail-before, both guards broken at once:** all three Rust tests failed (exit 101) — the phone got
`Bool(false)` for a measured clip, and the restore path came back `None`. Restored byte-identically
(`git diff --numstat` unchanged).

**Persisted with the app UP, and that is a departure from iteration 236 worth stating.** The re-align
rewrote `alignment_json`, a column the app itself writes, so it stopped the app. This writes one column
nothing else writes, `Database::open` sets `busy_timeout=10000`, and stopping the app would have cost 6.5
minutes of review-server downtime for no gain. A backup-API snapshot was taken anyway.

### The badge is DOM, so a Rust test cannot see it

`tests/couch_page_speaker_change_badge.test.ts` runs the REAL `couch.html` — the same bytes `include_str!`
embeds — in jsdom and calls the page's own `show()`. Fail-before: `if (false && seg.speakerChange)` fails
the badge assertion (exit 1); the file restored byte-identically.

Two designs were tried and rejected first, and the comment in the test says so: vitest's ambient jsdom
does not execute injected scripts, and eval'ing the source instead scopes the page's top-level
`let queue` to the eval — leaving `show()` reading a queue nothing can assign. A real window running it
as a classic script is the only form that runs the page rather than a copy of it.

### The first sweep came back INCOMPLETE, and the tool was right

`real-app-e2e` — described in its own registration as *"THE daily-use reliability gate: real exe, real
audio, real transcript"* — reported `SKIP-ENV`, and the verdict was **INCOMPLETE, 22 PASS, 0 FAIL, 1
skipped**, with "Green cannot be claimed." My fault, not the gate's: it needs `CORTEX_AUDIO`, and I ran
the sweep without it. Worth recording precisely because the shape of the output invites the wrong read —
"22 PASS, 0 FAIL" looks like success, and only the verdict line says otherwise. It said otherwise.

Re-run against the committed FLEURS ckb fixture: **REAL-DATA RUN OK: 1 segments; first transcript 77
chars**, disposable profile removed. The leg passes; it was never broken, only unasked.

### Second finding: the staleness gate could not see the review page

`check_exe_freshness.py` watched `src` and `src-tauri/src`. Three assets are COMPILED IN and were on
neither list: `assets/couch.html` (68 KB of reviewer-facing behaviour, its Sorani strings included),
`assets/couch-icon.png`, `migrations/001_initial.sql`.

Caught live and unforced, on this working tree:

```
before:  EXE FRESHNESS GATE: OK (exe at HEAD 3708f1b…, newer than all sources)      exit 0
         ...while couch.html sat 15 minutes newer than the binary
after:   STALE EXE: source src-tauri\assets\couch.html is newer than the built exe  exit 1
```

Editing a Sorani string on the review page would have shipped a silently stale exe. `scripts/` needs no
rebuild, so this cost nothing but the rebuild the widened gate then correctly demanded.

### And a native crash that must not be filed as "flaky" on one sighting

Sweep 2 came back **RED**. `test-rust` aborted:

```
Running tests\e2e_pipeline.rs
fatal runtime error: Rust cannot catch foreign exceptions, aborting
exit code: 0xc0000409, STATUS_STACK_BUFFER_OVERRUN
```

A C++ exception crossed the FFI boundary out of the native ONNX / sherpa stack. Rust cannot catch it, so
the process died before the harness flushed a single test name — there is no "which test" in the log.

What is established, and what is not:

* `cargo test --test e2e_pipeline` in isolation: **3 / 3 clean**, 10 passed each, ~17.5 s.
* Sweep 1 had `test-rust` **PASS 375.5 s** on identical Rust code — the only change between sweeps was a
  `scripts/` edit, which cannot reach a Rust test. Not caused by this iteration's work.
* Sweep 3, conditions identical to sweep 2: **PASS 368.9 s**.
* The signature appears NOWHERE in this ledger or docs. First sighting: **1 abort in 3 full sweeps**.

That is not enough to characterise it, and "intermittent native crash in the ASR stack" is exactly the
class of thing that gets waved through as flakiness and then bites in real use. Recorded with its exact
signature so the second sighting is recognised as the second, not the first. Not fixed, not explained,
and not called flaky.

**Gates.** `cargo fmt --check` **0**; `cargo clippy --all-targets --all-features -D warnings` **0**;
`cargo test --lib` **1094 passed, 0 failed, 7 ignored**; vitest **40 files / 217 tests, 0 failed** (the
three new ones among them, confirmed by name in `vitest list` rather than assumed); `npm run typecheck`
**0 errors 0 warnings**; `npm run lint` **0 errors**; python policies **46/46**.

**verify-10: `23 - 23 PASS, 0 FAIL, 0 skipped` — VERDICT GREEN**, with `real-app-e2e` PASS 19.0 s. Exe at
HEAD, `claim: 200  queue: 200  items: 29  pendingTotal: 112`, and `speakerChange` present on the payload.

**Owner-gated, and one item grew.** The badge needed a new Sorani string — "زیاتر لە یەک قسەکەر" — which
is acknowledged as UNREVIEWED in `test_couch_page_i18n.py`. The list the owner has to read natively is
now **9**, was 8. Not laundered through a `source` claim: sourcing it from a brand-new desktop string
would have made the policy call it reviewed when nobody had read it.

**What this does NOT do.** The clips are flagged, not resolved — whether each is split, rejected or kept
is the owner's call, and 4 of the 17 he has already reviewed. The desktop app shows no badge (the phone
is where the 13 pending ones are served). Nothing counts or gates on the flag: no export summary, no
validation issue. Overlap stays invisible: the one genuinely simultaneous clip in the ground truth scored
0.841, among the single-speaker group, and no model on this machine can see it.

One consequence worth stating rather than discovering later, and this is READ FROM THE CODE, not
measured: `ExportSegmentRecord` holds `#[serde(flatten)] segment: SpeechSegment`, so JSON and JSONL
exports now carry `speakerChangeScore` alongside `snrDb` / `vadBackend` / `denoised` — honest
per-segment provenance, and a consumer reading 0.41 learns something true. CSV, Parquet and the
HuggingFace exporter build explicit column lists and are unchanged. No gate objected, because none
pins the flattened export schema.

## Iteration 238 — Couch Review had no end-to-end test at all

Measured before building anything: **none of the five `e2e_*.cjs` harnesses mentions `8737`, `couch`, or
`/api/queue`.** The path clips are actually reviewed through — multi-reviewer tokens, DPAPI at rest,
leases, spot checks, an offline outbox and a 68 KB phone page — was tested only by Rust unit tests
calling `api_queue` / `api_decision` as FUNCTIONS. Nothing ever started the real binary, bound a real
port, claimed a token over the wire, streamed audio bytes, submitted a decision and read the library
back. Iteration 237's `speakerChange` field is served by exactly that path, and the only thing that had
ever exercised it end to end was me, by hand, in PowerShell, against the live library.

**The blocker was the port, and the fix was already half-written.** `start_on_port` exists because a Rust
test "cannot bind 8737 while the owner's own server is using it". An end-to-end harness has the identical
problem and cannot reach that function at all — it goes through the `start_couch_review` IPC command,
which calls `start()`. `CORTEX_COUCH_PORT` now overrides the bind port for `start()` and `resume()`
alike (or a remembered link would come back on a different port than `start` uses). Unparseable or `0`
falls back to 8737: an env var must never be able to stop the owner's phone link coming up.

**It rides in `e2e_real_app.cjs` rather than a sixth harness**, so it inherits what is already hard-won
there: the disposable profile with its refusal to touch the real `%APPDATA%` library, the WebView2
isolation, and the retrying cleanup. One app, one profile, one place that knows how to kill it.

```
session up for "E2E Harness" on port 18737
queue: 1 item(s), pendingTotal 1
audio: 263084 bytes, RIFF header present
decision persisted: verified=true, reviewedBy="E2E Harness"
server stopped and the port released
```

Fragment token -> `POST /api/claim` -> `Set-Cookie` -> `GET /api/queue` (asserting every field the page
renders, `speakerChange` included, and that the server names the right reviewer) -> `GET /api/audio`
(bytes present, RIFF header) -> an UNAUTHENTICATED `/api/queue` must not be served -> `POST
/api/decision` -> and then the assertion that matters: **the library**, read back through the app's own
IPC rather than a hand-rolled query that could agree with a broken write. A server can answer 200 and
persist nothing.

**Fail-before**, decision POST removed:

```
REAL-DATA RUN FAILED: the correction did not persist:
  stored "…کەشەی پیوەست بەنەخۆشەکەن نەبوو"   sent "…نەبوو ✔"
```

exit 1. Restored byte-identically (`git status` clean).

### The first mutation did not fail, and the code was right

The initial fail-before attempt sent `action: 'accept'` carrying the corrected text, expecting the
library assertion to catch a mismatch. It passed — because `api_decision` does not take the phone's word
for what kind of decision was made:

```rust
let is_edit = text != review_text(&prev).trim();
(if is_edit { "edit" } else { "accept" }, Some(text))
```

The verdict is derived from whether the text ACTUALLY changed. A client that mislabels its own action —
or a reviewer who edits the box and then taps "Looks good" — still lands the right provenance. That is
stronger than the design assumed here, and the bad mutation was mine, not a gap in the code.

### The 3x rule earned its keep on run 2: the new gate HUNG

Run 1 passed. Run 2 stopped dead, and the app's own log says exactly where:

```
[App] INFO cortex_speech_app_lib::couch: Couch Review stopped
```

`stop_couch_review` completed. What never returned was the line after it — a `fetch` whose entire
purpose is to find NOBODY listening, written with no timeout:

```js
const afterStop = await fetch(`${base}/api/queue`, …).then(() => true, () => false);
```

Node's `fetch` has no default timeout. When the closed port DROPPED the SYN instead of refusing it, that
promise never settled. Run 1 got a fast refusal and passed; run 2 did not. **A gate that hangs is worse
than one that fails** — a failure gets reported, a hang just stops the loop, and it would have stopped
verify-10 the same way. One run would have shipped this.

All six requests now go through a bounded helper (`AbortSignal.timeout(20000)`). For the post-stop probe
a timeout counts as NOT answering, which is the outcome it asserts — so the abort is the difference
between proving the port was released and hanging on the question.

### The new production function had no test either

`configured_port` went in an hour before with nothing exercising it. Reading the variable is the
untestable half (process-global, and mutating the environment mid-test is unsound with other threads
running); deciding what a BAD value means is the half that matters, because getting it wrong takes the
phone link down rather than merely ignoring a typo — the owner would see Couch Review "start" and find
nothing at the URL he has bookmarked. Split into `port_from(Option<&str>)` and pinned: unset, valid,
surrounding whitespace, empty, `0`, garbage, `70000` (out of u16 range), `-1`.

### An empty directory kept "for the post-mortem"

`cleanupProfile` deliberately keeps the disposable profile on failure, because it holds the only copy of
the DB a post-mortem can read. But a PRECONDITION failure fires before the app is ever spawned, so what
it kept was an empty directory. Measured on this box: **2 of 40 stale `cortex-e2e-*` directories held 0
files**, both minutes old, from two precondition exits; the 40 together are **865 MB**.

`die()` now removes the profile when this run minted it and it sits under the temp root. Fail-before /
after on the same precondition (`CORTEX_AUDIO` pointing at a missing file): **leak 1 -> 0**.

The 865 MB of older profiles is left alone — they predate this session and are not mine to delete.

**Gates.** `cargo fmt --check` **0**; `cargo clippy --all-targets --all-features -D warnings` **0**.

### The sweep went RED on a real defect, not a flake

`verify-10` failed `commands::tests::agreeing_source_references_preserve_per_model_evidence`:

```
left:  Some("multi-reference-consensus:gemini-2.5-flash+gemini-2.5-pro")
right: Some("multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash")
```

The same test had passed standalone twenty minutes earlier, which is exactly what a flake looks like. It
is not one. `get_source_transcripts_for_audio` ordered by `datetime(updated_at) DESC, datetime(created_at)
DESC` — and BOTH columns have one-second granularity while two reference transcripts for one clip are
written back to back. The tie let SQLite return them in either order, so
`multi-reference-consensus:a+b` — **persisted on the row and shipped in exports as `referenceModelId`** —
differed between identical runs. A corpus diff would show changes nobody made, and any grouping or count
by that value splits one category in two. Same class as the seed bug `export.rs` already documents: *"the
old code shuffled `HashMap` keys, whose iteration order is randomised per run"*.

**Which fix carries it is measured, not assumed:**

| state | reversed-order test |
|---|---|
| neither change | **FAILS** (exit 101) — the original bug, reproduced deterministically |
| `model_id ASC` tiebreaker alone | passes |
| tiebreaker + sorted join | passes |

So the tiebreaker is the root-cause fix and the sorted join is defence in depth. The comment says that,
rather than implying both were needed. The join stays sorted anyway: the string names a SET of models
that agreed, and its canonical form must not depend on why some query happened to order its rows.

**The new test pins the property instead of relying on luck.** The sibling test asserts one exact string
and only caught this because one run happened to tie the other way — passing it five times proves
nothing, since nothing in it controls the tie.
`the_consensus_provenance_string_is_canonical_not_insertion_ordered` writes the same two models in the
OPPOSITE order and demands the identical value.

Live library unaffected: `source_transcripts` holds **0 rows**.

`cargo test --lib` **1096 passed, 0 failed, 7 ignored**.

### Three gates caught this iteration's own work — one per sweep, because the runner stopped at the first

**1. A second recursive delete.** `die()`'s new cleanup added its own `fs.rmSync` with its own copy of
the guards, and `test_real_data_runner_policy.py` — written earlier in this same session — refused it:
*"Recursive delete must appear exactly once."* That policy is right, and the reason is the reason it was
written: two delete sites means two places to get the guards right. The single delete moved into
`removeDisposableProfile()`, shared by `cleanupProfile` (which retries past Windows' asynchronous handle
release) and `die()` (which has nothing to wait for). The policy was **strengthened**, not loosened: it
now also pins that the shared helper exists, so neither caller can be re-inlined.

**2. An exact literal that no longer matched.** `test_restore_reservation_gate.py` pins `couch::start`'s
one-line body so the restore guard cannot be bypassed by `start` growing a real body, and
`configured_port()` had replaced the bare `COUCH_PORT`. The gate's own note says *"update this literal to
its new one-line body — do not loosen it"*, and that is exactly what was done, for `resume` too. `resume`
matters as much: coming back on a different port than `start` binds would resurrect the session at a URL
the owner's bookmark does not point at.

**3. The policy runner hid two of them.** `run_python_policies.py` was fail-fast, so a stale-ledger
failure masked the duplicated delete, which masked the couch literal. **Three verify-10 sweeps were spent
learning what one run could have said.** Every policy now runs and every failure is named; strictness is
unchanged — any failure still exits non-zero — only the reporting is. `py_compile` stays fail-fast: it is
the syntax floor every policy was just executed on, not a policy itself.

Fail-before, two policies broken at once:

```
Python policy regressions FAILED: 2 of 46
  - test_gitignore_policy.py
  - test_workflow_policy.py          exit 1
```

Previously only the first was ever visible. Both restored, tree clean.

### And a slip of my own: a RED STATUS.md reached a commit

`git add -A` in the jury-fix commit swept in `docs/STATUS.md` reading **RED — 2 kept gate(s) failed**, and
four later commits inherited it. Nothing had been pushed, so the five unpushed commits were rewritten to
carry the last status a real run actually produced. Every code change survived the rewrite (verified by
spot-check, working tree clean afterwards).

The rule exists so the repo never advertises a verdict no run produced, and this would have published
precisely that. Caught by reading the commit back rather than by the discipline that should have stopped
`git add -A` in the first place.

**The rewrite then staled the exe, which is worth knowing:** a rebase rewrites the WORKING TREE, so
`db.rs` came back with a fresh mtime, newer than a binary built from byte-identical source.
`exe-freshness` failed and was right to — the gate cannot know the content is unchanged. One rebuild is
the honest cost of rewriting history in a repo whose staleness gate is mtime-based.

## Iteration 239 — four harnesses ran against the owner's real library, and one of them found a live bug

Started from a one-line audit: which e2e harnesses does any gate actually execute?

```
e2e_7b_connect        -> NOTHING RUNS IT
e2e_constrained_ipc   -> NOTHING RUNS IT
e2e_finetuned_ipc     -> NOTHING RUNS IT
e2e_pipeline_ipc      -> NOTHING RUNS IT
e2e_real_app          -> verify_10.py, package.json, ci.yml
```

Reading them turned up something worse than "unwired". All four spawned or attached to the app with a
bare `{...process.env}` — **no `CORTEX_APP_DATA_DIR`** — so they ran against the real
`%APPDATA%\cortex-speech` library and imported audio into a corpus holding 32 human review decisions.
Three also killed by IMAGE NAME (`taskkill /F /IM`), which takes down the owner's own running Cortex,
and none isolated the WebView2 profile. `e2e_real_app.cjs` has all three protections and a policy
pinning them; **they were built for one harness and its four siblings were left behind.**

**One alarm corrected before it stood.** The three spawning harnesses call `python clear_db.py` as a
"clean slate" step, which looked like a library-wipe. It is not: they pass neither `--yes` nor
`CORTEX_DB_CLEAR_CONFIRM`, so `clear_db.py` refuses (exit 2) and the `try/catch` swallows it. The
safety contract living in the dangerous script rather than its callers is exactly why a careless caller
could not defeat it — defence in depth working as designed. What it also means is that the clean-slate
step **never once cleaned anything**.

### The fix, and why it is not one shared module for all five

`e2e_profile.cjs` gives the three spawning harnesses a disposable profile with a production refusal,
WebView2 isolation, a PID-tree kill, a busy-port precondition, retrying cleanup, and offline-engine
provisioning. `e2e_real_app.cjs` keeps its own: it is the only harness wired into a gate, it works, and
its guards are pinned literal by literal. Rewriting the one thing that gates the repo so it can share
code with three that nothing runs is the wrong risk trade, and the module says so.

`e2e_7b_connect` needed a different remedy entirely: it launches nothing, so a disposable profile is
not available to it — it writes to whatever library the app it attached to has open. Isolation cannot
be the answer, so an explicit `CORTEX_ALLOW_LIVE_PROFILE=1` acknowledgement is.

**Provisioning was not incidental.** With the profile isolated, `e2e_pipeline_ipc` sat in its
`get_segments` poll forever: a fresh profile takes the app's default engine, the OmniASR-7B champion,
which needs the owner's warm WSL server. These harnesses only ever worked because they were borrowing
his configured profile. Left alone they would have blamed VAD for an import that could not decode.

### The bug the isolation exposed: the champion model was unreachable

`transcribe_segment_finetuned` returned *"fine-tuned model not found
(models/finetuned-mms-ckb/{model.onnx,vocab.json})"* for a **970 MB model present the whole time**.

`pipeline.rs` had ALREADY been fixed for this, and its comment names the scenario exactly:
`select_bundled_models_dir` keys the ONE bundled root on OmniASR-CTC presence, so a partial copy beside
the exe wins and orphans every sibling that lives only in the full repo models dir. Measured here:

| root | contents |
|---|---|
| `target/release/models` | omniasr-ctc-300m, onnxruntime.dll, silero_vad_v4.onnx |
| `%APPDATA%\cortex-speech\models` | mms_aligner.onnx + tokens |
| `src-tauri/models` | **finetuned-mms-ckb**, campp, denoiser, ctc-1b, aligner |

The import path loaded it; the IPC command could not. **Third time in two iterations** that a fix was
applied at one site and the identical logic next door was left behind — after the guarded delete and
the harness isolation. One implementation now: `models::finetuned_model_paths`, called by both.
Observable: same harness, same machine, before "not found", after a real hypothesis from the champion.

### A gate that rejected its own ground truth

Both IPC harnesses asserted every non-space character of the output be Arabic-script. The committed
reference, `tests/fixtures/fleurs_ckb_sample.txt`, reads `…ساڵی 1800ــەوە…` — and `1`, `8`, `0` plus a
left-to-right mark all fail that predicate. Proven by running the old check over the reference file
rather than arguing about it:

```
committed ground truth passes the harness assertion: false
chars it rejects: "180‎"
```

`e2e_constrained_ipc` passed only because CTC-300M happened to emit Arabic-Indic `١`; the fine-tuned
model emits Western digits, exactly as the reference does. The check now requires Kurdish script and
forbids Latin letters — which still fails an English hypothesis, the failure mode it was written for.

### Wired in, so this cannot rot again

All three default to the committed fixture and are registered as verify-10 legs (**23 -> 26**), with
probes that skip only on a missing binary, fixture or model — never on a forgotten env var. The policy
now covers **every** harness: a spawning one must isolate and must not kill by image name; a
connect-only one that imports must demand the live-profile acknowledgement. Fail-before for both
branches — reverting `e2e_pipeline_ipc` fails on the image-name kill, stripping the `e2e_7b_connect`
guard fails on "a casual run would add clips to the owner's corpus".

**Gates.** `cargo fmt --check` **0**; `cargo clippy --all-targets --all-features -D warnings` **0**;
`cargo test --lib` **1096 passed, 0 failed, 7 ignored**; python policies **46/46**; verify-10
**26 - 26 PASS, 0 FAIL, 0 skipped — GREEN**, the three new legs at 15.3s / 17.3s / 19.2s.

**The live library was never touched:** 144 rows, 17 flagged, 13 pending, before and after.

## Iteration 240 — a gate that reported "21 passed" while twelve tests asserted nothing

Three items, each RUN before it was wired, because every static audit this session produced a false
positive and every real find came from executing something.

### 1. The model-integrity check nobody ran

`fetch_models.py --check` calls itself *"the CI/dev integrity gate"* in its own docstring, and nothing
in `verify_10.py`, any workflow or the Makefile referenced it. Ran it first: **5 files SHA-256
verified**, green — so it is now a tier-1 leg. A swapped or truncated model still LOADS and decodes to
the wrong graphemes, which is the silent-corruption class the runtime pin in `asr.rs` exists for.

### 2. `ignored-real-model` was partly vacuous, and this is the third of its family

The leg reports **"21 passed"**. Without `CORTEX_REAL_AUDIO_DIR`, `real_audio.rs`'s
`discover_real_audio_files()` returns an **empty vec** and one test returns early printing *"set
CORTEX_REAL_AUDIO_DIR"* — so **twelve of those twenty-one asserted nothing**. Measured both ways
rather than argued:

| | skip lines |
|---|---|
| without the variable (what the leg did) | **12** |
| pointed at the committed fixtures | **9** |

Three tests — decode-any-format, single-file decode and the pipeline import test — become real
assertions from a single `os.environ.setdefault`. The other nine need formats the repo does not carry
(flac, mov, mp4, the gold podcast) or their own env vars, and stay honestly skipped. After the fix the
leg's log contains **zero** `[...] skip` lines.

Third member of this family, after the fuzz empty-target-list (2026-07-26) and the tautological RTF
assertion (iteration 235). It survived because both the leg's NAME and its test count looked healthy:
the count was real, what the tests did with their run was not.

I had predicted this item would yield nothing. The audit was worth more than the wiring would have been.

### 3. The bench budget: four corrections, one retraction, all found by running it

Three criterion benches existed and nothing ran them. `scripts/bench_gate.py` compares against a
committed baseline and **cannot pass vacuously** — zero parsed benchmarks is a FAIL, and a bench that
vanished from the run is a FAIL rather than a silent skip. Proven to fire by injecting 3x regressions
against benches with a 1.05x and a 1.52x limit: both named, exit 1.

Everything else about it was wrong at first, and each error was caught by running it:

1. **`cargo bench` bare also runs the lib's libtest harness**, which rejects `--output-format` and
   takes the whole run down — the gate would have been red for a reason unrelated to performance.
   Only the criterion targets are invoked now.
2. **`build.rs` copies `onnxruntime.dll`** and the running app holds a lock on it: `os error 32`. The
   benches get their own `CARGO_TARGET_DIR` so the leg runs with the app up, which is what a sweep is.
3. **The baseline was measured app-STOPPED** because that was the quiet, convenient state. The first
   real run — app up, as always — reported five regressions of 1.16x to 1.41x with not one line of
   code changed. The gate was right; the baseline was wrong.
4. **The thresholds were a coin flip.** 2x the spread from a 3-run calibration gave
   `diff/identical_100_words` a 1.088x limit; it then measured **1.0897x inside a sweep and FAILED**,
   minutes after measuring 1.09x standalone and passing.

The same twelve benches on the same machine have now read three different spreads:

| calibration | median | max |
|---|---|---|
| 3 runs, app stopped | 5.8% | 25.9% |
| 3 runs, app running | 5.6% | 61.3% |
| 5 runs, app running | **9.2%** | 34.5% |

That is the lesson in one table: **the spread you measure depends on how many samples you take and
what else is running, and the smaller, quieter measurement is the flattering one.** I took the
flattering one twice.

Settled: 5-run app-up calibration, 3x multiplier, floor 1.10x, cap 1.50x. **10 of 12 gated, tightest
limit 1.13x.** The two past the cap — the 67 us audio bench where scheduler jitter dominates, and
`diff/identical_1000_words` — are printed **NOT ENFORCED with their noise figure on every run**,
rather than handed 2.04x/1.67x limits that would look gated while a real 2x regression sailed through.
Two stability runs produced byte-identical summaries.

**A CLAIM RETRACTED.** I reported that one bench was gated at the charter's 5%. It is not, and none
is: 5% is inside this machine's noise for all twelve. `BUDGET = 0.05` stays in the file as the
STANDARD so the gap between what the charter asks and what this hardware can measure stays visible
instead of being quietly redefined, and the gate prints the tightest limit it actually enforces so the
comment can never drift from the truth.

**Still UNMET and named:** `github-action-benchmark` on every PR. That needs CI I can neither run nor
watch, and a workflow file nobody has seen execute is a claim, not a gate. The committed baseline it
would consume now exists, so the remaining step is smaller than it was.

### What the session's evidence actually says about method

Every genuine find came from executing something in the condition it really runs in. Every static grep
audit produced a false positive — the all-or-nothing model root that was correct by construction, the
insert-path asymmetry that was deliberate and documented, the `#[ignore]` count that matched
doc-comment prose. The two audits that DID pay (`which harnesses run`, `which npm scripts run`) asked
an EXISTENCE question, not a correctness one. Existence questions survive a regex; correctness
questions do not.

**Gates.** `model-integrity` PASS 0.7s; `bench-budget` PASS 568.1s; verify-10 **30 legs**.

### Owner decision 2026-08-02: the count-agnostic fuzz requirement is CONFIRMED

`AGENT_CHARTER.md`'s engineering-rigor line has said "EVERY fuzz target run in CI" since iteration 232,
replacing a hardcoded "5" that went stale the moment iteration 231 deleted the `features` target along
with the unused `FbankExtractor` it fuzzed. It was carried as PENDING and nothing was built on it. The
owner has now confirmed the wording, and the charter line records the confirmation inline. It leaves
the owner-gated list.

The gate already behaves this way and always did: `_fn_fuzz_smoke` enumerates targets with
`cargo fuzz list` and FAILS LOUD on an empty list rather than reporting a vacuous pass — so the
charter now describes what the code does instead of a number that could drift away from it.

## Iteration 195 — the explicit no-verdict, and the 17 flagged clips decided

### The 17 mixed-speaker clips are decided

13 of the 17 clips the speaker-change probe measured were still pending; the other 4 already carried a
human decision and were left exactly as they were. `src/bin/reject_speaker_change_clips.rs` applies the
owner's decision in bulk through `record_human_decision` — the same call the desktop and the phone
make — with a dry run by default.

**It took two writes, and the first version only did one.** The tool printed `rejected 13 clip(s)` and
the clips came back correctly rejected AND still pending: every one of them would have been served to a
reviewer again. Nothing in the tool's own output said so. A diff against a backup-API snapshot did:

```
rows: 144 -> 144            rows changed: 13
  human_decision / verdict / verified    changed on 13 rows
  raw_transcript / annotated_transcript
    / alignment_json / speaker_change_score  changed on  0 rows
rejected rows: 6 -> 19
flagged AND still pending: 13 -> 0
previously-decided flagged clips: 4, of which changed: 0
```

The real couch path does two writes for exactly this reason (`record_human_decision_by`, then the
whole-row upsert that sets `verified`) and now so does this. `pendingTotal` on the live phone link went
112 -> 99.

### R4.4: a reviewer can now say "I cannot judge this"

Someone facing a clip they genuinely cannot call — two people talking over each other, an accent they do
not have, audio that will not play — had exactly two ways forward, and BOTH write a judgement they
cannot stand behind: "Looks good" promotes an unheard draft to gold, "Reject" permanently excludes a
clip that may be perfectly fine. A guess is worse for the corpus than an honest "I don't know", because
nothing downstream can tell the two apart.

`action: "skip"` writes NOTHING to the row. It records the act in the audit trail, releases the lease,
and takes the clip out of that reviewer's queue so it reaches somebody who can judge it. The button
existed but was revealed only on an audio error and only advanced the page locally — so after the batch
drained, the next refill served the same clip straight back.

Two things it would have broken silently, both closed:

- `reviewer_throughput` counted every audit row as a clip reviewed. A skip would have credited someone
  for work they explicitly did not do. Now a WHITELIST of decision actions, so the next non-decision
  event on this trail cannot re-open it.
- A reviewer who skipped through a small backlog would have been congratulated with "all clips
  reviewed" over unjudged work. The queue reports `skippedByYou` and the empty state says so.

One new Sorani string (`skippedByYou`); the button reuses the already-listed `skip` text. The
owner's native-read list goes 9 -> 10.

### What the session's evidence says about method

Three defects in this iteration, and **not one of them was found by reading the diff.**

1. The half-written rejection — found by diffing the live DB against a snapshot, not by the tool's count.
2. An offline skip deleting the reviewer's typed draft — found by reading the OTHER route to the server
   after writing the guard on the first one. `decide()` had it; `flushOutboxOnce()` inherited the bug.
   Now pinned at the source for BOTH routes in `test_frontend_review_guards.py`.
3. Autoplay-on-skip — found by verify-10's a11y leg, which failed 2 of 88 because an existing spec had
   already decided this question, with better reasoning than mine: "a skip is the reviewer saying 'I
   cannot judge this' ... deserves a loaded clip and silence." Reverted to the prior decision rather
   than rewriting the test to agree with the change.

The third is the one worth keeping. A test that fails because it encodes a deliberate earlier decision
is not an obstacle to route around — it is the only remaining record of reasoning nobody was around to
restate. The default when a gate contradicts a change must be to read WHY before touching either.

**Fail-before proofs, unmasked.** skip path: new unit test asserted 200, got 400 (unknown action),
exit 101. Draft guard: removed -> AssertionError naming `flushOutboxOnce`, exit 1; file restored
byte-identical (SHA256 compared); pass-after exit 0.

**Gates.** verify-10 **GREEN: 30 kept legs, 30 PASS, 0 FAIL, 0 skipped** (8 owner-descoped, 5
owner-gated pending). couch unit tests 54/54; lib 1097 passed / 0 failed; `couch-page.spec.ts` 41/41;
python policies 46/46; clippy `-D warnings` clean. Exe at HEAD, app running, phone link answering
(claim 200, queue 200, 29 items, pendingTotal 99, `skippedByYou` present in the live payload).

### Follow-up in the same iteration: the skipped spot check

Noticed while designing the skip and deliberately parked, then chased down properly. Two facts, each
fine alone: the skip filter lives in `api_queue`'s loop over PENDING rows, and spot checks are inserted
*after* that loop — so the filter never saw them; and the DB-level exclusion is `id NOT IN (SELECT
segment_id FROM spot_checks WHERE reviewer = ?)`, which lists clips the reviewer was SCORED on, while a
skip writes no score. Together: the one clip somebody said they could not judge was re-inserted into
every batch forever.

The fix filters inside `list_spot_check_candidates`, not in the caller, and that difference is the whole
design: it still returns `limit` candidates, just DIFFERENT ones. Skipping a check costs you that clip,
never your place in the measurement — otherwise the honest exit doubles as a way to never be tested
again. Both halves are asserted.

Reading the code said it was real. Reading is not evidence, so it was proved on the real selection path
first: skip `gold-a`, next batch returns `["w0", "w1", "gold-a", "w2"]`, exit 101.

### The bench gate was reporting on its own measurement method

verify-10 went RED on `bench-budget` right after the spot-check fix. Four regressions, in `audio`,
`diff` and `normalizer` — modules the commit does not touch, and which the benches reach directly
(`benches/*.rs` import `audio`, `diff`, `normalizer` and nothing else). So the RED could not be the
change, and the question became what it actually was.

Two tells, both already in the output:

- the failing SET moved between runs on the same commit — `audio/waveform_decode_1600000_samples` read
  1.91x, then 1.08x;
- across two consecutive sweeps ALL TWELVE benches read above baseline and **not one read below**.

The second is the signature. The committed baseline stores the FASTEST of 5 runs; the gate defaulted to
`--runs 1`. Best-of-1 against best-of-5 compares sampling, not code — a single run is slower than the
best of five essentially always, so every ratio was biased upward by construction. My bug, from when
this gate was written, and I read past it twice before the "none below" pattern registered.

Matching the counts, same commit, same machine, nothing else changed:

```
single sample : all 12 above baseline, 1.07x - 1.50x, 2 FAIL
matched (5)   : 10 of 12 BELOW baseline, 0.82x - 0.88x; worst 1.06x; PASS
```

That is the whole distribution moving, which is the one thing a sampling mismatch does and a code
regression cannot. The run count is now read FROM the baseline, so a regeneration with a different
`--runs` keeps both sides matched instead of drifting apart again.

**The budget and every per-bench threshold are unchanged.** This removed a bias so the ratio means what
it claims; it did not widen a limit. Worth being explicit about, because "the gate went green after I
touched the gate" is exactly the shape of a gate being quietly weakened — the distinguishing evidence is
that unrelated benches moved from 1.32x to 0.83x, which no threshold change would produce. Cost: the leg
goes ~130s -> ~620s, in line with the 568s it took historically.

**Gates.** verify-10 **GREEN: 30 kept legs, 30 PASS, 0 FAIL, 0 skipped**. couch 55/55; lib 1098 passed /
0 failed; `couch-page.spec.ts` 41/41; python policies 46/46; clippy `-D warnings` clean. Exe at HEAD,
app running, phone link answering.

### The retrain-readiness mirror had no gate

Hunted the defect class the ledger records six fixes for — a tally that counts rows the export drops —
now that 13 clips have just become rejected, which is when such a site would be wrong.

The Rust side is well defended: `stats.rs` is pinned against the real `export_dataset` output by
`the_dashboards_verified_count_equals_what_the_export_actually_writes`. The gap was in Python.
`scripts/retrain_readiness.py` answers the one question that spends GPU time using a HAND-COPY of that
eligibility SQL, and rests its entire safety argument on being a faithful mirror. Nothing enforced the
mirror, and nothing ran the script — no test, no policy runner, no workflow, no Makefile target.

**Checked before claiming.** The three fragments (`EFFECTIVE`, `REJECTED`, `PLACEHOLDER`) are identical
modulo whitespace today, so this pins a correct mirror rather than repairing a broken one — worth
stating plainly instead of dressing a non-finding up as a catch. The risk was future drift, and it is
one-directional: `stats.rs` gains a rule (it gained the placeholder axis exactly this way), the Python
keeps the old one, and the owner is told there is more eligible material than the export would publish.

`test_retrain_readiness_policy.py` extracts the rule from `stats.rs` and compares. Extraction failure is
an assertion, not a skip: a gate that cannot find what it compares against must fail loud rather than
pass vacuously. Fail-before: one token changed -> FAIL naming `REJECTED` with both sides printed, exit 1;
restored byte-identical (SHA256); pass-after exit 0. Python policies 46 -> 47.

**First real run of the report**, on the live library: 26 export-eligible clips, 5.5 min eligible audio,
764 human-gold words, 22 human-verified DPO pairs, 99 still pending. A retrain is not close.

**Owner-visible, not fixed by me:** the report prints `champion on frozen gold: {"champions": {}}` — the
champion record in the live data dir is EMPTY. Step 5 of the retrain path promotes only on beating the
champion, and there is no champion there to beat. The script is honest about it; the data is missing.
Writing a number in would be fabrication, so it is surfaced instead.

### The champion this machine cannot name

The readiness report surfaced `champion on frozen gold: {"champions": {}, "schema": 1}`. Followed it:
`champion.json` is the app's startup MIRROR of the model registry (`registry.rs`), and `model_versions`
in the live library holds **zero rows**. So the empty object is a true answer about this machine, not a
missing file.

The obvious worry — that "promote ONLY if it beats the champion" degenerates into "promote" when there
is no champion — was **checked and is wrong**. `registry::decide_promotion` sets `promote = false` and
reports "no paired baseline comparison in the challenger scorecard" whenever `vs_baseline` is absent.
The gate is correct; nothing can be promoted by accident. Worth recording as a non-finding rather than
leaving it phrased as a near-miss.

What is real is smaller and still worth saying: the champion described in the ledger exists as files on
disk and a number in prose, and **not** as anything the app can compare a challenger against. The report
now says `NONE RECORDED — the model registry has no champion` in words, plus a note explaining that the
gate is safe but has nothing to defend a retrain with. No number was invented and none was backfilled
into the registry — transcribing a CER out of prose into a database is precisely the "remembered metric"
the honesty law forbids. Registering it needs the real eval run, which is owner-gated.

**Measured, on the live library, why that gate is not theoretical.** The 13 clips rejected earlier this
iteration all became `verified = 1` (that is what takes a clip out of the review queue), so a count site
missing the reject clause would now *inflate*:

```
total rows                          : 144
rejected                            :  19   (13 today + 6 prior)
rejected AND verified=1             :  19
naive "verified" count              :  45   <- what a rule without the reject clause reports
export-eligible (the pinned rule)   :  26   <- gap of exactly 19
eligible AND rejected   (must be 0) :   0
eligible AND placeholder (must be 0):   0
still pending                       :  99
```

A 73% overstatement of "how much training material do I have", available to anyone who copies the rule
and drops a clause. That is the whole argument for pinning the mirror rather than trusting the copy.

**Gates at the close of iteration 195.** verify-10 **GREEN: 30 kept legs, 30 PASS, 0 FAIL, 0 skipped**
(8 owner-descoped, 5 owner-gated pending). couch 55/55; lib 1098 passed / 0 failed;
`couch-page.spec.ts` 41/41; python policies **47/47** (was 46); clippy `-D warnings` clean;
`bench-budget` PASS 1016.9s with matched sampling. Exe at HEAD, app running, phone link answering.

### Two fault drills existed and nothing ran them

Asked the existence question again — which scripts does no gate, workflow or Makefile reference? Twelve
came back. Ten are legitimately hand-run tools in the retrain path. Two were **drills**, whose entire
purpose is to prove a property, sitting unrun: `durability_drill.py` and `export_kill_drill.py`. Same
shape as the retrain-readiness mirror earlier this iteration, and this pair covers the property daily
review depends on most — the app dying must never cost work that was already saved.

Ran both before wiring anything, on real binaries and disposable profiles:

```
durability : 25 hard kills (20 write-phase, 5 boot-phase), 19,463 rows committed,
             0 journaled edits lost, contiguous id space, integrity_check ok at every verify
export-kill: 15 mid-export kills, 134 journaled exports all complete, zero torn final files
```

**Why they get their own cargo target dir.** `tauri_build`/`ort` copy `onnxruntime.dll` next to the
built artifacts, and the RUNNING app holds it open, so `cargo build --bin durability_writer` against
`target/` dies with `os error 32` — measured today, exit 101. The app is up during every real sweep, so
a drill leg failing for that reason would be failing for something it does not test. A sibling dir under
the already-ignored `target/` has its own copy that nothing holds; proven with the app running (271s +
221s cold, cached after). I first reached for build.rs as the root cause and was wrong — the copy comes
from dependency build scripts, not ours.

The build is inside the leg deliberately. A probe that SKIPS turns a reliability gate into a no-op
exactly when someone forgot to build; a pre-built binary requirement silently proves durability for code
that is no longer shipped.

**Gates.** verify-10 **GREEN: 32 kept legs, 32 PASS, 0 FAIL, 0 skipped** (was 30). New legs measured in
the sweep: `durability-drill` 225.0s, `export-kill-drill` 41.1s.

### Pre-flight on the 99 clips waiting for the owner

Read-only, against real files and the LIVE couch server, because the next review session is the thing
this iteration is for:

- all 99 fetched over real HTTP: **39.4 MB decoded + sliced + WAV-encoded**, every one a 200, a
  parseable RIFF/WAVE, and a PCM length agreeing with its advertised duration (a short slice would make
  a reviewer reject a good clip);
- audio present on disk, no zero-byte files, no non-positive durations, no placeholder or blank drafts;
- speaker-change safety net **measured on all 99** — none silently unscored — and 0 still-flagged;
- 0 duplicate drafts inside the queue, 0 drafts identical to already-approved text.

Library health, read-only on the live file: `integrity_check` **ok**, `foreign_key_check` **0
violations**, **0 orphans** across all eight child tables, WAL 0.58 MB.

### Measured: the speaker-change badge does NOT catch what the owner rejects

The owner reviewed 22 clips on the phone and reported rejecting mainly for **two or more people talking**
and **a transcript too wrong to fix**. Their 14 rejections are the first real labels this badge has ever
been scored against, so it was scored.

**AUC 0.47 — no signal.** (0.50 is a coin flip.)

```
speaker_change_score   rejected n=14: q1 0.650  med 0.774  q3 0.794
                       kept     n=40: q1 0.720  med 0.753  q3 0.778

threshold   rejects flagged   keeps wrongly flagged
   0.59      1/14   (7%)         3/40   (8%)
   0.70      5/14  (36%)         6/40  (15%)
   0.80     12/14  (86%)        36/40  (90%)
```

Raising the threshold makes it WORSE. At 0.80 it fires on 90% of good clips — a warning that fires on
almost everything is noise a reviewer learns to ignore inside one session, which is worse than no
warning at all. The threshold stays at 0.59.

**Why it misses them, and this is a design limit not a bug:** the probe compares a clip's first half
against its second half. That detects a clean handover at the midpoint. It is structurally blind to two
people OVERLAPPING, and to a turn near either edge — which is most of what a real conversation does.

**What the badge is still worth:** it was 15/15 correct in the owner's blind listening calibration when
it fired. High precision, low recall. Trust it when it speaks; never read its silence as "one speaker" —
which is what the `holds_a_speaker_change` comment already says for the unmeasured case, and now also
holds for the measured-but-above-threshold case.

**Deliberately NOT built yet:** a sliding-window change-point scan. Calibrating one against 14 labels,
roughly half of which are the OTHER rejection cause, is building on sand. Revisit at ~100 decisions,
when there is enough real signal — and evaluate SPLITTING multi-speaker clips at the change point rather
than discarding them, which converts waste into corpus instead of merely flagging it.

**Also measured, same session:** `agent_confidence` DOES predict rejection (AUC 0.77; rejected median
0.54 vs kept 0.72). Serving highest-confidence first would make the first 10 clips 10/10 usable against
a 26% baseline waste rate — but it only DEFERS the waste, it removes none of it, and n=14 makes the
figure suggestive rather than established. Not acted on.

## Overnight run, 2026-08-02/03 — phase 1: the flakiness hunt

### verify-10 was not repeatable, and only repetition could show it

Three sequential 32-leg sweeps, nothing else running. All three RED:

```
sweep 1: test-e2e+a11y, pipeline-ipc-e2e   exit 1   2845s
sweep 2: real-app-e2e,  pipeline-ipc-e2e   exit 1   1701s
sweep 3: real-app-e2e,  pipeline-ipc-e2e   exit 1   1683s
```

The failing legs took **0.5s** against a normal 15–39s — they never ran:
`PRECONDITION FAILED: debug port 9222 is already answering`.

Port 9222 is held permanently by the owner's Antigravity IDE browser
(`--remote-debugging-port=9222 --user-data-dir=…\.gemini\antigravity-browser-profile`). Nothing leaked
and nothing in the app was broken; this is ordinary desktop tooling. The refusal is CORRECT and stays —
attaching would drive somebody's real browser. The defect is that a GATE reached for the one port every
other developer tool also grabs by default.

**The fix was already written in this repo; two files never got it.** Ports were the perfect control:

```
constrained-ipc  9281 private  PASS x3     real-app-e2e     9222 shared  FAIL
finetuned-ipc    9291 private  PASS x3     pipeline-ipc-e2e 9222 shared  FAIL x3
heartbeat-probe  9333 private  PASS x3
jobs-probe       9334 private  PASS x3
egress-probe     9335 private  PASS x3
```

`real_app` → 9271, `pipeline_ipc` → 9261, `e2e_7b_connect` → 9251. The last is not a gate leg so nothing
had exposed it — fixed anyway, because leaving the final copy of a bug whose two siblings were just
repaired is how it returns.

**Proof.** Fail-before: three sweeps, exit 1, same message. Pass-after with 9222 STILL held by
Antigravity: `e2e_pipeline_ipc` ran the whole import → VAD → ASR chain, 77 chars of Kurdish, exit 0.
Full sweep after the fix: the three legs pass at 22.0s / 39.1s / 14.4s — normal durations, not 0.5s.

**What this says about method.** Every sweep run yesterday passed. A green gate is only worth what its
REPEATABILITY is worth, and nothing had ever measured that. One extra run per day on a machine with an
IDE open would have reported RED for a reason unrelated to the code — and the natural response to a
confusing RED is to stop trusting the gate.

**Still open:** `test-e2e+a11y` crashed once in the three sweeps — exit `3221226505` (`0xC0000409`,
stack buffer overrun) at 1.5s — then passed in sweeps 2, 3 and the verification sweep. 1 in 4. Separate
cause from the port collision, being sampled across further sweeps rather than guessed at.

**Gates.** verify-10 **GREEN: 32 kept legs, 32 PASS, 0 FAIL, 0 skipped**.

### Phases 2–4: fuzz, drills at scale, and a mutation gate that could not see its riskiest module

**Phase 2 — long fuzz.** The gate budgets 30s per target; each got 1800s.

```
target      executions    exec/sec   new corpus inputs
cache        6,807,918      3,780        0
diff         2,294,465      1,272      596
normalizer     742,338        412    1,807
validation 289,431,028    160,705       50
```

**~299 million executions, zero crashes.** Stated plainly: no bug found. The value is coverage —
the normalizer gained 1,807 inputs reaching code the 30-second budget had never touched. That corpus is
gitignored (`.gitignore:116`, 0 files tracked), so the gain is local-only and CI still starts from
empty every run. NOT committed: reversing a deliberate gitignore unattended is not the assistant's call;
recorded for the owner (~11 MB unminimized; `cargo fuzz cmin` would cut it).

**Phase 3 — drills at scale.** `DURABILITY DRILL PASS: 300 hard-kill cycles (240 write-phase, 60
boot-phase), 162,466 rows committed, 0 journaled edits lost, contiguous id space, integrity ok at every
verify.` Twelve times the gate's 25 cycles, same invariants.

**Phase 4 — the mutation gate could never mutate `couch.rs`.**

`.cargo/mutants.toml` added `src/couch.rs` to `examine_globs` on 2026-07-27, with a six-line comment
naming it the highest-risk module in scope. The nightly job builds its `--in-diff` input from an
explicit file list that never included it, so `--in-diff` dropped every couch mutant. The two halves of
one config had drifted apart with nothing comparing them.

Fail-before on a REAL commit range (`282efd5..HEAD` — the night `couch.rs` gained 298 lines of new
decision logic):

```
old filter -> diff 0 bytes      -> "Mutation gate idle: No core-module changes", exit 0
                                   A VACUOUS PASS, on the exact night the riskiest module was rewritten.
new filter -> diff 18,390 bytes -> 6 mutants found, 6 caught, 0 survived (8m)
```

Two things worth separating. The gate defect is real and is fixed (`498e31f`). The *result* — 6 of 6
caught — is a clean bill for the skip-path tests written the previous evening: they kill every mutation
in the lines they cover, rather than merely looking thorough. Verified the config itself was never the
problem: `cargo mutants --list` reads `.cargo/mutants.toml` and enumerates exactly the eight scoped
files, 1,077 mutants.

Caught by the repo's own `test_workflow_policy.py` mid-edit: the first draft of the fix used an em-dash,
and workflow YAML must stay ASCII-clean.

### The gate destroyed the evidence for its own only unexplained failure

`run_gate` writes each leg's full output to `LOG_DIR/<gate>.log`, under a comment promising "every
failure stays diagnosable after the run". The path is fixed, so that promise expires the moment the
gate runs again — and the failure it expires for is the INTERMITTENT one, the single case where the log
is the only evidence that will ever exist.

Measured: `test-e2e+a11y` crashed with exit `3221226505` (`0xC0000409`, stack buffer overrun) in one
sweep of three. Two further sweeps then overwrote the log with passing runs. **That fault is still
unexplained specifically because the gate's own success deleted the proof.**

A failure now also gets `LOG_DIR/<gate>.FAIL.<timestamp>.log`. Copy rather than move — the stable path
is what the FAIL line prints and what people look for, so it keeps meaning "most recent run" — and
best-effort, because bookkeeping must never turn a diagnosable failure into a crash.

Proven on the real `run_gate`, reproducing the exact destroying sequence:

```
failing run                        -> stamped copy created (1)
later PASSING run of the same gate -> stable <gate>.log records exit 0; the failure is GONE from it
                                   -> timestamped copy still present (1); the evidence survives
```

### Honest non-finding: the export kill drill is quadratic in cycles

Ran it at 400 cycles against the gate's 15. Throughput decayed measurably — 24 exports/min at 05:29,
19/min three minutes later — because each cycle re-verifies every export accumulated so far. At 15
cycles this is invisible (41s); at 400 it had not finished in 90 minutes. Stopped at ~297 cycles, 20x
the gate's count, with no failure (it exits non-zero per cycle on failure and did not).

Not a defect in the app and not fixed: the gate runs 15 and is unaffected. Recorded so that anyone who
raises the cycle count knows the cost is O(n^2), not linear.

**Gates.** verify-10 **GREEN: 32 kept legs, 32 PASS, 0 FAIL, 0 skipped**, at `14966be`.

### The a11y gate tested whatever happened to hold port 1420

Same shape as the 9222 collision found hours earlier: a gate trusting a shared fixed port it did not
create. `playwright.config.ts` set `reuseExistingServer: !process.env.CI`, TRUE locally, so Playwright
attached to whatever answered on 1420 without ever checking what it was.

Reading the config only SUGGESTED this, and reading is not evidence. Demonstrated instead: a trivial
impostor server serving `not the app` was placed on 1420, and the accessibility spec ran against it.

Both directions matter and the quiet one is worse:

```
foreign server         -> leg goes red. Confusing, but honest.
STALE but valid server -> leg goes GREEN about code that is not under test
                          (another branch, or a watcher that died) = a vacuous pass.
```

`CORTEX_GATE` is now set by `verify_10.py` for everything it runs, meaning exactly one thing: this is
the gate, never quietly reuse a resource you did not create. It is the generalisation of what the e2e
harnesses already do in `refuseIfDebugPortBusy`.

Measured, all three cases:

```
port free  + gate                       -> 88 passed, 20.7s, exit 0   (was 22s; no cost)
port busy  + gate                       -> "Error: http://localhost:1420 is already used ..."
port busy, interactive npm run test:e2e -> reuses; local convenience deliberately preserved
```

### Honest negative: the 0xC0000409 crash did not reproduce

Chasing it through 45-minute sweeps costs one observation each; the leg alone costs 22s. Ran it
directly **40 times: 40 clean, 0 failures**, every run's output kept.

So it is NOT diagnosed and NOT fixed, and nothing here should be read as closing it. Two things did
change: failure logs now survive (so the next occurrence is finally diagnosable), and both port defects
that were live during the sweep where it fired have been removed — the failing sweep was also the sweep
where 9222 refusals were happening. That is a hypothesis about a shared cause, not a finding.

### The pattern across the whole run

Four of the five findings are one defect wearing different clothes: **a gate trusting a resource it did
not create, or a config half-applied.** 9222, 1420, the mutation filter that had drifted from its own
`examine_globs`, and a log path that promised durability it could not deliver. Each was individually
green. None survived being run twice, or run against the thing it was written for.

**Gates.** verify-10 **GREEN: 32 kept legs, 32 PASS, 0 FAIL, 0 skipped**; a11y leg 21.8s.

### The two biggest test legs passed while running zero tests

MEASURED, not suspected:

```
cargo test --lib -- <filter matching nothing>
  test result: ok. 0 passed; 0 failed; 1105 filtered out    exit 0
npx vitest run -t <matching nothing>                        exit 0
```

Both harnesses exit 0 on an empty run, so `test-rust` (1,193 tests across 35 binaries) and
`test-frontend` (217) would have reported PASS if discovery ever broke — a stray filter, a renamed
module, a cfg excluding the test tree. Green with nothing behind it, on the two legs carrying the most
weight. `_fn_fuzz_smoke` already guards this exact class on an empty `cargo fuzz list`; these two simply
had no floor.

`scripts/assert_ran.py` wraps them: the command's own failure is reported first (never masked), then a
minimum count. Floors from real measurements with headroom — 1100 against 1193, 200 against 217. The
subtle part is the checker's OWN vacuity: if it cannot FIND the count line it FAILS rather than skipping
the check, because a guard that silently stops understanding its input still looks like protection.

Proven: zero tests → FAIL exit 1 (was a silent PASS); no count line → FAIL exit 1; real run → 217 ran,
floor 200, exit 0. Live in the sweep: `assert_ran: 1193 test(s) ran (floor 1100) across 35 binaries`.

### STILL OPEN: an undiagnosed 0xC0000409 in the Node/Playwright probes

The fix above for preserved failure logs earned itself back within hours. The crash recurred — this time
on `heartbeat-runtime`, not `test-e2e+a11y` — and for the first time the evidence survived:

```
$ node scripts\heartbeat_probe.cjs
(exit 3221226505, 4.7s)
--- stdout ---  ==> Heartbeat probe. profile=...cortex-heartbeat-35jtZ5 ...
--- stderr ---  (empty)
```

What is now known: it is NOT specific to one leg; it hits Node/Playwright probes generally; stderr is
empty and Windows Error Reporting logs nothing (consistent with a controlled abort, not an unhandled
exception); and it happens only inside a FULL SWEEP — the a11y leg ran 40 times standalone, 40 clean.

Ruled out by measurement, not assumption: orphaned WebView2 processes (0 orphans; the 31 present belong
to WhatsApp, M365Copilot and SearchHost) and disk pressure (583 GB free).

**Deliberately NOT done: adding this code to `run_gate`'s retry.** That retry exists and is correct, but
it is scoped to ONE named, understood flake (LNK1104) and announces itself. Retrying a crash nobody can
explain is how a real bug gets buried, and a gate that reds honestly on it is doing its job. The RED
`docs/STATUS.md` from that sweep was restored, never committed; the re-run went GREEN 32/32 with
`heartbeat-runtime` back at 13.4s — **and a green re-run is not evidence the crash is fixed.**

**Gates.** verify-10 **GREEN: 32 kept legs, 32 PASS, 0 FAIL, 0 skipped**.

### phase 4: three probes promised to clean up after themselves and none of them did

`heartbeat_probe.cjs`, `jobs_probe.cjs` and `egress_probe.cjs` each open their header with the same
sentence — *"a DISPOSABLE `CORTEX_APP_DATA_DIR`, a per-run `WEBVIEW2_USER_DATA_FOLDER`"*. Disposable was
the claim. Nothing disposed of them. Every run of every probe left **two** directories behind, and the
gate runs all three on every sweep. This is the same class the repo had already measured once and fixed
once: `e2e_profile.cjs` carries the note that `e2e_real_app` had left *34 stale profiles totalling
764 MB* before it got `cleanupProfile`.

**The first fix (`db94f2c`) was wrong, and wrong in the way that matters.** It cleaned only
`heartbeat_probe`, and it did so with a bespoke twenty-line copy instead of the helper sitting one
directory up:

- it deleted `DATA_DIR` **unconditionally** — a caller who passes their own `CORTEX_APP_DATA_DIR` would
  have had that directory destroyed by a tidy-up they never asked for;
- it waited a fixed 1.5s where `cleanupProfile` retries to a 15s deadline, because Windows releases a
  killed process tree's handles asynchronously — the exact reason the helper has a deadline at all;
- it had no tmpdir guard, so a path mistake could point the delete anywhere.

Writing a third copy of a function whose two existing callers already encode two hard-won lessons was
the actual defect. `dce6b69` deletes the copy and gives all three probes `cleanupProfile(dir, ours)`,
with `OWNS_DATA_DIR` captured **before** the `mkdtempSync` — afterwards `DATA_DIR` is set either way and
the distinction (did we create it, or were we handed it?) is unrecoverable.

**Proof, both directions.** "It printed Removed" is not evidence a count fell, so the counts were taken
before and after each run:

```
=== 1. NORMAL RUN: must leave nothing ===
heartbeat_probe.cjs    exit=0  data 0  webview2 0   CLEAN
jobs_probe.cjs         exit=0  data 0  webview2 0   CLEAN
egress_probe.cjs       exit=0  data 0  webview2 0   CLEAN
=== 2. CALLER-SUPPLIED PROFILE: must be left alone ===
heartbeat_probe.cjs    exit=0  caller's dir survived: True   CORRECT
jobs_probe.cjs         exit=0  caller's dir survived: True   CORRECT
egress_probe.cjs       exit=0  caller's dir survived: True   CORRECT
```

All three were +1/+1 before. The second block is the one `db94f2c` would have failed.

**Deliberately NOT fixed: the `cortex-integration-*` / `cortex-smoke-*` trees.** Those are created by the
*app* — a per-PID data dir when it starts headless with no `CORTEX_APP_DATA_DIR` — which is correct
behaviour for the app. Whichever harness launches it without an override owns the cleanup, and naming
that harness would be a guess. Recorded here instead of patched.

**Gates.** verify-10 at `dce6b69`: **GREEN — 32 kept legs, 32 PASS, 0 FAIL, 0 skipped**; 8 owner-descoped,
5 owner-gated pending.

### phase 5: "naming the owning harness would be a guess" — it took one grep

The previous entry recorded the `cortex-integration-*` / `cortex-smoke-*` trees as unfixable-without-
guessing. That was wrong, and wrong in a lazy way: the owner is nameable, and finding it cost one search.

`lib.rs::get_app_data_dir` falls back to `TEMP\cortex-{smoke,integration}-<pid>` whenever the app starts
headless with no `CORTEX_APP_DATA_DIR`. Exactly two harnesses start it that way, both `cargo test`
integration tests that the `test-rust` leg runs on **every sweep**:

```
src-tauri/tests/shell_smoke.rs:9        .env("CORTEX_SMOKE_TEST", "1")
src-tauri/tests/tauri_integration.rs:25 .env("CORTEX_INTEGRATION_TEST", "1")
```

Neither set a data dir; nothing removed one. `tauri_integration.rs` already wrapped its **fixture** dir in
`tempfile::TempDir` — the disposable-directory pattern was in the file, one line above the omission.

A third leak sat in the product: `integration_runner.rs` wrote its export to the **TEMP root**, outside
any data dir, so it would have survived even after the tests were fixed.

**Measured before touching anything:**

```
cortex-smoke-*            dirs=122   76.0 MB
cortex-integration-*      dirs=124   84.4 MB
cortex-integration-*.json files=58
```

**Fail-before, then after — same two tests, counted around the run:**

```
BEFORE FIX   smoke-dirs 122 -> 123   integration-dirs 124 -> 125   stray-json 58 -> 59
             test result: ok. 1 passed   (x2)            DELTA +1 / +1 / +1
AFTER  FIX   smoke-dirs 123 -> 123   integration-dirs 125 -> 125   stray-json 59 -> 59
             test result: ok. 1 passed   (x2)            DELTA  0 /  0 /  0
```

Both tests passed in both runs — which is the point. The leak was never going to redden anything; it
just grew. 246 stale directories and 58 files (**160.7 MB**) were reclaimed after the fix landed.

Fix: both tests pass a `TempDir` as `CORTEX_APP_DATA_DIR` (deleted on drop), and the integration export
moves from the TEMP root into the app data dir, so it is disposed of with the dir that owns it.

**Blocked on the owner, not on the work.** `integration_runner.rs` is a product source file, so the
release exe is now stale and `exe-freshness` FAILS — correctly:

```
STALE EXE: source src-tauri\src\integration_runner.rs is newer than the built exe. Rebuild.
```

Relinking needs the running app closed, and the running app is the phone-review server. The commit is
held locally, unpushed, until the owner can spare the app for ~10 minutes. `cargo fmt` clean,
`cargo clippy --all-targets --all-features -D warnings` clean.

### phase 6: the probes cleaned up after success and after failure — but not after refusing to start

A temp-dir census after phase 5 turned up something the phase-4 proof had not: dirs matching the
probes' own patterns, created AFTER the phase-4 fix landed (`dce6b69`, 09:55).

```
cortex-egress-ctk60F   10:16:34   0 files
cortex-egress-EMZZbq   10:16:34   0 files
cortex-egress-tzYMcu   10:16:34   0 files
```

**Zero files** is the tell. Each probe `mkdtemp`s its profile at module scope, then checks preconditions
inside `run()`. A `die()` — wrong exe path, debug port already answering — calls `process.exit(1)`
without touching the directory it just made. Success cleans up. A thrown failure deliberately keeps the
profile for diagnosis. The refuse-to-start path did neither: it left an empty directory nobody would
ever look in.

Reproduced deterministically rather than inferred, by occupying the port the probe requires:

```
BEFORE cortex-egress-* dirs = 148
PRECONDITION FAILED: debug port 9335 already answering — another instance is running
AFTER  cortex-egress-* dirs = 149        newest: cortex-egress-8wA5wx   files inside = 0
```

Fix: an `ownedTemp` list, declared above `die` (so it can be read without a temporal-dead-zone access
to the consts below it) and appended to at each `mkdtemp`. `die` empties the list before exiting. It is
deliberately NOT wired into the failure handler — a probe that got far enough to launch the app has
something worth keeping.

**After, all three, ports occupied:**

```
egress     exit=1   dirs 149 -> 149   delta=0
jobs       exit=1   dirs 50  -> 50    delta=0
heartbeat  exit=1   dirs 0   -> 0     delta=0
```

Exit 1 is preserved — the refusal still fails loud, it just stops littering. The success and
caller-supplied-profile paths were re-proved unchanged (6/6 CLEAN / CORRECT).

One false alarm worth recording: the first regression re-run reported `exit=1 ... LEAKED / FAILED` on all
three. That was the port-occupier fixture from the fail-before still listening, not a regression — every
probe was correctly refusing to start. Re-run with the ports free: 6/6 green.

**Unexplained and left open:** what invoked three egress probes at 10:16:34 against an occupied 9335.
The refusal itself is correct behaviour (attaching would drive somebody else's real browser). Recorded,
not guessed at.

**Reclaimed:** 203 more stale dirs, **848.6 MB** — 1,009 MB total across phases 5 and 6.

### phase 7: the availability watchdog could be switched off and nothing would ever say so

Relinking the exe (owner said "close it") failed at first with `Access is denied` — the app was already
running again. `CortexWatchdog`, a scheduled task that probes port 8737 every 5 minutes and relaunches
the app when the review server is unreachable, had done exactly its job within ~80 seconds. The
watchdog script documents its own pause switch, so that was used rather than inventing one:

```
schtasks /change /tn CortexWatchdog /disable   ->   relink   ->   /enable
```

**The gap that exposed.** The watchdog is the entire availability story for the phone review, and
turning it off is a one-liner that leaves no trace. `docs/REMOTE_PUBLIC_LINKS_PLAN.md` documents the
disable step; nothing anywhere notices when the matching re-enable never happens. The only reason it is
on right now is that this agent remembered. A crash, an interrupted session, or a sweep that died in
between would have left the owner's link with no healer and no warning — found out whenever it next
failed, which is precisely when it matters.

`scripts/test_watchdog_enabled.py` fails on the one state that is unambiguously wrong: the task EXISTS
and is Disabled. Absent is NOT a failure — a clean checkout, CI, a non-Windows box and a machine that
never opted into the availability stack all legitimately have no watchdog, and reddening on those
trains people to ignore the gate. `Get-ScheduledTask`'s State enum is read rather than parsing
`schtasks /query`, whose display text Windows localizes; a check that silently stops matching on a
non-English machine would report healthy forever.

**Fail-before, restored after:**

```
1. watchdog ON    WATCHDOG GATE: OK (CortexWatchdog state=Ready)                      exit 0
2. disabled       WATCHDOG GATE: FAIL - CortexWatchdog is registered but DISABLED.    exit 1
3. re-enabled     WATCHDOG GATE: OK (CortexWatchdog state=Ready)                      exit 0
```

No wiring was needed: `run_python_policies.py` globs `scripts/test_*.py`, so the leg went from **47 to
48 policy test scripts passed** on its own. (That glob was checked, not assumed — the suspicion that a
test could exist and never run is what started this, and it turned out `test_watchdog_decisions.py` was
already collected and already passing.)

The printed failure is ASCII-only on purpose: the first draft's em-dash rendered as a replacement
character in the Windows console, and the one line somebody reads at 3am must not be mojibake.

### phase 8: five relaunches in a week, and no way to know whether the app crashed

The watchdog log is the only record of the review server going down:

```
2026-07-29 13:24:09  session expected but app not running - relaunching
2026-07-30 13:22:18  session expected but app not running - relaunching
2026-08-02 00:12:18  session expected but app not running - relaunching
2026-08-02 14:12:18  session expected but app not running - relaunching
2026-08-03 11:37:19  session expected but app not running - relaunching   <- this one was the relink
```

That line reads identically whether the owner closed the window or the process died. And the app logs
cannot settle it: **16 log files spanning 2026-07-09 to 2026-08-03 contain ZERO shutdown lines of any
kind.** Every one of them ends mid-startup. The `RunEvent::Exit` handler exists and runs
`begin_shutdown()`; it just never said so.

Its own comment names the distinction it was failing to make observable:

> An abnormal app death never reaches this handler

So the difference between "closed" and "crashed" was real, load-bearing for the availability story, and
invisible. Four of those five relaunches remain unattributable and always will.

**The marker.** The app clears `logs\last-exit.txt` at every start and writes it only from
`RunEvent::Exit`. Present = the app reached shutdown. Absent while not running = it did not. No time
window, no heuristic: the clear-on-start is what makes absence mean something.

Written with `std::fs::write`, NOT `tracing`. The file layer is a `tracing_appender::non_blocking`
whose `WorkerGuard` is deliberately leaked (so logging survives the whole process), which means nothing
flushes it at exit and a line emitted there can die with the process. A diagnostic that is sometimes
missing is worse than none — then "absent" no longer means "crashed".

**Proof, all three states:**

```
FAIL-BEFORE  release exe (pre-change), clean smoke exit   exit=0   marker: NO
A. clean exit, patched exe                                exit=0   marker: orderly exit 2026-08-03T12:24:49.014814300+00:00
B. hard kill (taskkill /F /T), patched exe                         marker: <ABSENT>
```

The watchdog now reads the marker at the moment it relaunches, so the attribution lands in the same
line as the event: `session expected but app not running - relaunching [clean exit (...)]` or
`[NO exit marker - died without reaching shutdown]`. `test_watchdog_decisions.py` still passes all 8
branch assertions.

The drill hard-killed only the debug build; the owner's app (PID 6476, started 11:48:29) was untouched
and the phone link answered 200 throughout — checked, because a process filter that matched too broadly
is exactly how a drill would take down a live review session.

**Held, not pushed.** `lib.rs` is product source, so the release exe is stale and `exe-freshness` fails
correctly. Needs one more ~6-minute app-close window. `cargo fmt` clean, `cargo clippy --all-targets
--all-features -D warnings` clean (checked in an isolated CARGO_TARGET_DIR so the running app was never
disturbed).

### phase 9: three fixes shipped today with hand proofs and no gate — my own debt, paid

`CLAUDE.md` is explicit: *"A fix without a regression gate is incomplete."* Phases 6, 7 and 8 each ended
with a measurement pasted into this ledger and nothing in the suite that would notice the fix being
undone. Delete the `fs::write` in `lib.rs` and 1,193 Rust tests still pass. Remove one
`ownedTemp.push` and every probe still exits 0 on the happy path. That is the same shape as the two
vacuous passes found earlier this week, authored by the fix for them.

**1. The orderly-exit marker, asserted end-to-end on the real binary.** `shell_smoke.rs` already
launches the real exe and exits cleanly — and since phase 5 it has a disposable `CORTEX_APP_DATA_DIR`,
which is exactly what makes the marker findable. It now reads the file and checks its CONTENT, not just
its presence: an empty file is still a file, and the watchdog prints that line to the owner.

```
FAIL-BEFORE  fs::write removed   panicked: no orderly-exit marker ... after a CLEAN exit    FAILED
AFTER        restored            test result: ok. 1 passed
```

**2. Every probe temp dir must be registered.** Counted, not pinned by literal: the failure mode is
ADDING a temp dir and forgetting to register it, which is precisely how the third one appeared. A
substring check passes with two of three registered.

```
FAIL-BEFORE  one ownedTemp.push removed
             jobs_probe.cjs creates 2 temp dir(s) but registers 1                            exit 1
AFTER        restored                                                                        exit 0
```

**3. The watchdog gate could disable itself.** `task_state()` returned `None` for both "not registered"
and "could not ask", and `None` meant SKIP. On the one machine that actually has a watchdog, a broken
query and a healthy watchdog looked identical — the self-disabling guard this repo keeps finding,
reintroduced inside the fix for it. Broken queries now raise and FAIL.

```
FAIL-BEFORE  cmdlet renamed to break the query
             WATCHDOG GATE: FAIL - could not read CortexWatchdog's state                     exit 1
AFTER        restored                                                                        exit 0
```

One slip worth recording: restoring #3's fail-before with `git checkout --` also discarded the
uncommitted improvement, because HEAD predated it. Caught by re-reading the file rather than trusting
the restore, and re-applied. `git checkout` restores to HEAD, not to "before my probe" — those are only
the same thing when the file was clean to begin with.

Policy suite: **48 scripts passed**.

### phase 10: a 0.0% post-jury CER that could not have been anything else

The owner's own product/UX audit (`docs/audits/2026-08-03-…`, now gitignored — its screenshots are of
the live library and its REPORT.md embeds absolute user-profile paths the hygiene gate rejects in
tracked files) flagged the Refinery card as untrustworthy: *"5.4% mean CER, 17.8% mean WER, 21.0% CER fine-tuned, 0.0%
post-jury CER, 'No eval runs yet' … together they undermine trust."*

Two of those are not in conflict — "post-jury CER" and "No eval runs yet" are separate cards answering
different questions. Chasing the third found something worse than inconsistency.

**The metric could not report anything but zero.** `load_lift_triples` scores
`(reference, raw, jury)` where the reference is `annotated_transcript` — the human's confirmed text.
Accepting a clip copies the jury's verdict into it. So an accepted row compares the jury with itself,
and its char distance is zero whatever the jury produced. It cannot tell "the jury was right" from "the
reviewer rubber-stamped it".

Measured with the app's own query against the real library:

```
rows feeding the lift             : 39
rows where reference == jury text : 39   <- structural zero
share                             : 39/39 = 100.0%
```

Every scored row. So `Post-jury CER 0.0% · 95% CI [..]` was arithmetic wearing the clothes of a
measurement, and `cer_lift = raw - jury = raw - 0` handed the jury 100% of the credit for the raw ASR
error by construction.

**Fix.** `LabelQualityLift` gains `self_referential_n`, counted over SCORED rows only (`rl > 0`, the
same exclusion `micro` already applies — a row in neither numerator nor denominator is not evidence
either way). The card withholds the numbers entirely when that equals `n`, and discloses the count when
only some rows are affected. Nothing is deleted or recomputed: the honest reading is that this data
contains no independent evidence about the jury, and the card now says so.

The existing test `label_quality_lift_rewards_jury_corrections` turned out to BE the circular case —
both its rows have reference identical to jury, and it read that as "the jury restores the reference".
It now also asserts `self_referential_n == 2`, so the fixture cannot be mistaken for evidence that a
zero jury CER means a good jury.

**Fail-before, source guard:**

```
guard removed              AssertionError: RefineryPanel.svelte lost the self-referential guard   exit 1
branches swapped           expected `{#if …selfReferentialN >= lift.n}` then `{:else if …}`       exit 1
restored                   frontend review-guard source policy passed                             exit 0
```

The second case is worth recording: the FIRST version of that guard compared string offsets, which the
swap does not change — both conditions keep their positions relative to the grid. **It passed the exact
regression it was written for.** Pinning the two conditions to their roles is what actually holds. That
is the third decorative-check of the week, and the second I authored myself.

Also recorded: `git checkout --` was used twice to undo a fail-before probe and twice discarded
uncommitted work alongside it, because HEAD predates the edit. Restoring from a file copy is the
technique; the lesson had to be learned twice to stick.

Gates: policy suite 48/48, typecheck 427 files 0 errors, vitest 217/217, `cargo fmt` + `clippy
--all-targets --all-features -D warnings` clean.

### phase 10a: the ledger entry about forbidden paths contained a forbidden path

The sweep gating phase 10 came back **RED on `python-policies`**, and the offender was this ledger:

```
test_windows_repo_hygiene.py
AssertionError: Tracked files must not hardcode a private local profile path (public repo):
- PROGRESS_LEDGER.md:5607: ... REPORT.md embeds absolute `<the literal path>` paths ...
```

The sentence explaining that the audit report embeds absolute user-profile paths had written one out.
The gate is right and stays exactly as it is: a public repo must not carry that string, and prose
*about* the string is still the string. Rewritten to describe it instead of quoting it. `docs/STATUS.md`
was restored, never committed — a RED status does not get pushed.

Worth noting how much trouble the scrub itself was. Three attempts failed: a heredoc'd script, then two
regexes where `chr(92) + '+'` reads as an escaped literal plus rather than one-or-more backslashes, so
the pattern silently matched nothing and reported "no occurrence". The self-test that finally exposed it
(`search` on a known-bad probe string) should have been the first thing written, not the fourth. The fix
that worked replaces the two lines by INDEX — no pattern to get wrong.

Also confirmed while there: the two sweeps overlapped (`bga12kgwl` was still finishing when the phase-10
sweep started), which is worth remembering because a sweep stamps `docs/STATUS.md` with HEAD **at write
time, not at run start** — so the older run labelled its result with a commit it had never tested. Its
green is not evidence for anything and is not being counted.

### phase 11: the headline mean WER/CER counted rows the export throws away

Same hunt as phase 10, next number along. `compute_wer_cer_metrics` scores every segment with a
non-empty `annotated_transcript` and applies no other filter. Two kinds of row slip through:

**Rejected clips.** `record_human_decision` leaves the transcripts populated on a reject, so a clip the
reviewer discarded still looks annotated and folds its error into the dashboard's mean WER/CER — over a
row `export_dataset` drops. This is not an inference; `is_human_rejected`'s own docstring states the
intent the function was not honouring:

> this shared predicate lets the plain JSON/JSONL/CSV/Parquet exports **and quality counts** honor a
> reject exactly the same way

**Placeholder hypotheses.** When the ASR never produced output the raw transcript is
`[Pending WSL 7B ASR]`. Scored against a real reference that is ~100% error, which reads as "the engine
was completely wrong" when the truth is "the engine did not run" — a fabricated measurement, biased in
the direction nobody double-checks.

**Measured on the live library before touching anything — both are LATENT, not firing:**

```
total segments                          : 144
counted by mean WER/CER (annotated<>'')  : 40
  ...of those, HUMAN-REJECTED           : 0     <- export drops these
human-rejected overall                  : 27
scored rows with a placeholder/blank hypothesis : 0
```

Recorded plainly: today this changes no number the owner sees. 27 rejects exist and none currently
carries an annotation, so the dataset is one reject-after-an-edit away from the mean silently moving.
This is the seventh time this exact class (a tally counting rows the export drops) has been fixed here;
the previous six were also found before they fired.

**Fail-before, both guards removed at once:**

```
assertion `left == right` failed: a rejected clip and a never-transcribed clip must not be
counted as annotated evidence                                                        FAILED
restored                                                                             1 passed
```

The test asserts the mean does not MOVE when the junk rows are added, rather than pinning a magic
number — a fixture that hardcodes 0.037 passes for the wrong reasons the moment the CER routine changes.

Full lib suite 1100 passed / 7 ignored, `cargo fmt` and `clippy --all-targets --all-features -D
warnings` clean.

### Handed to the owner, not guessed at: a disclosure that names the wrong cause

`stats.conformalHeuristicBasis` fires correctly (no calibration confidence is a real posterior) but
explains it with:

> The local engine emits no token posteriors, so certification reflects the acoustic (CTC) score

Every one of the owner's 144 segments has `confidence_source = external_provider`. The confidences came
from a cloud provider, not the local CTC engine, so the app is stating a specific and wrong cause for
its own uncertainty. The Rust side lumps `external_provider` in with `heuristic`, which is right for
calibration (neither is a real posterior) and wrong as provenance.

NOT fixed unilaterally: correcting it means rewriting the sentence in English **and Sorani**, and this
agent does not write Sorani. Queued with the owner's existing 10-string Sorani check.

### phase 12: "117 Certified Segments" was the count of non-rejected rows

The owner's second audit (deep, `docs/audits/2026-08-03-cortex-deep-audit/`, gitignored with the first)
confirms the phase-10 fix landed — *"The fabricated post-jury win is gone"* — and flags the next one:
*"Calling 117 items 'Certified Segments' in the same panel is still too strong."*

It is not too strong. It is vacuous, and the arithmetic says so exactly.

```
confidence IS NULL   144 / 144
ctc_score  IS NULL   144 / 144
```

`compute_nonconformity_score` is `nonconformity(seg.confidence.unwrap_or(0.5), seg.ctc_score)` and
`nonconformity` uses `ctc_score.unwrap_or(-5.0)`. With both columns empty, EVERY segment scores
`(1 - 0.5) + 0.1·5 = 1.0` — identical, from two defaults, carrying no per-segment information at all.

What follows is forced. All scores tie, so the only cut point is `k = n`; the Hoeffding slack alone is
`sqrt(ln(40/0.05) / 80) = 0.289` against a 5% target, so no `k` satisfies the bound, `best_k == 0`, and
`calibrate_threshold` returns the uncalibrated fallback `sorted[0].0` — which is that same 1.0. Every
non-rejected segment scores ≤ 1.0 and is therefore "certified": **144 − 27 rejects = 117**, reproduced
from first principles before any code was touched.

So the panel showed a bold cyan 117 beside its own `n/a (uncalibrated)`. The count of rows that are not
rejected, wearing a statistical word.

**Fix.** The tile shows `—` while `isCalibrated` is false — this panel's existing idiom for
undefined-not-zero, already used by the threshold tile beside it and by `RefineryPanel`'s `metric()`.
Success-cyan is bound to `isCalibrated` too, so an uncalibrated readout cannot look healthy at a glance.
The payload is unchanged; what was wrong was presenting it as an achievement.

**Fail-before:**

```
unconditional count restored   AssertionError: StatsDashboard.svelte must withhold the certified
                               count while the certificate is uncalibrated                    exit 1
restored                       frontend review-guard source policy passed                     exit 0
```

Plus a Rust test that pins the tautology itself: 40 segments with no confidence and no ctc_score must
all score identically, must report `is_calibrated == false`, and must certify ALL of them — the last
assertion existing precisely to document why the count cannot be shown.

**A process mistake, recorded because it is the second of its kind today.** The sweep gating phase 11
was still running while these edits were being made — the exact contamination criticised two entries
above. It was killed rather than read; a verdict over files that changed underneath it is not evidence.
No orphaned processes, `docs/STATUS.md` untouched (the run never reached its write), phone link 200
throughout.

Gates: typecheck 427 files 0 errors, lint 0 errors, policy suite 48/48, Rust lib 1101 passed.

### phase 13: the top bar was 2067px wide and clipped its own controls at every real screen size

The deep audit's P0 #3: *"At 1024-2560 px and 200% zoom, no header action may leave the viewport. In
Review mode, Validate/Inbox/Settings currently overflow."* Measured rather than taken on trust, with a
new `e2e/header-overflow.spec.ts` that reads each action's bounding box against the viewport:

```
FAIL 1024   FAIL 1280   FAIL 1440   FAIL 1920   pass 2560   FAIL 512 (200% zoom)
the top bar is 2067px wide inside a 512px viewport and does not scroll
locale-toggle spans x=-42..27 (viewport 1920)
```

Worse than reported: it fails at **1920** too — only 2560 fits. And `justify-between` on an overflowing
row pushes content off BOTH edges, so at 1920 the locale toggle sat at **x = -42**, off the left of the
screen, with no scroll to reach it. A control that is silently unreachable is worse than a missing one,
because nothing tells the user it exists.

Fix: `flex-wrap` + `gap-y-2` on the bar, and on the right-hand action group as well — wrapping only the
outer bar moves the whole ~1500px group to a second line that is still wider than a 1024 viewport.
Wrapping beats a horizontal scroller or an overflow menu here because nothing ends up hidden.

```
after: 6 passed (1024, 1280, 1440, 1920, 2560, 200% zoom)
```

The spec needs no wiring: the gate's `test-e2e+a11y` leg runs bare `playwright test`, which picks up
everything in `e2e/`.

### Checked and NOT a defect: the 21.0% CER in Settings

The same audit calls the fine-tuned model's `21.0% CER, N=900` an unreconcilable number. A first pass
over `docs/MEASUREMENTS.md` and the root `docs/EVAL.md` found nothing and nearly produced a much
stronger claim than the evidence supported. Searching properly found it:

> **2026-06-25 (PUBLISHABLE N=900 FINE-TUNED SCORECARD)**: … `scripts/scorecard_finetuned.py`:
> **micro CER 21.00%, 95% CI [19.93%, 22.04%]** (3000-sample utterance bootstrap, seed=42) …
> Full scorecard: `cortex-speech-app/docs/EVAL.md`

So the number is earned, reproducible, and has a CI. The audit is right about the narrower point — the
UI shows the point estimate with no CI, corpus, date or build, next to Insights' 5.4% / 17.8%, which are
annotation-drift figures over the owner's own clips and a completely different kind of measurement. Two
incomparable numbers, no scope labels.

NOT fixed unilaterally: the fix is words (corpus, date, "on the frozen gold set" vs "on your library"),
in English **and Sorani**, and inserting a bracketed CI into an RTL string risks bidirectional rendering
this agent cannot verify. Queued with the owner's Sorani check, alongside the
`stats.conformalHeuristicBasis` cause from phase 11.

### phase 14: three library reads answered "your library is empty" when they meant "I could not read it"

Not from the audit — the audit cannot see this one, because on screen it looks like an ordinary empty
state. Found while chasing a `console.error` that appeared during the header-overflow run.

`getSegments`, `getSegmentsPage` and `getSegmentsSuspectFirst` each validated the IPC payload and, on a
mismatch, logged to the console and returned an empty collection:

```ts
console.error('getSegmentsPage: expected page payload, got', typeof data);
return { items: [], total: 0, nextCursor: null };
```

That converts a failed read into a successful-looking one. Downstream:

* **ValidationPanel** filters an empty array and shows no signal anomalies — a clean bill of health
  issued by a read that never happened.
* **ReviewInbox** finds no unverified clips and reports there is nothing left to review.
* **segmentStore** renders an empty library.

`console.error` is the only trace, and no user opens a console.

The silence bought nothing. Every one of the four call sites is already inside a `try` with a
user-visible failure path: `segmentStore` raises a PERSISTENT banner with a Retry plus a toast,
`ValidationPanel` and `ReviewMode` call `notifications.error`, `ReviewInbox` writes a status line. The
fallback bypassed all of them. All three now throw with the received type in the message.

This is the same principle `scripts/assert_ran.py` already states about itself — *"a guard that quietly
disables itself when it stops understanding the output is worse than no guard, because it still looks
like protection"* — applied to the read path instead of the test path.

**Fail-before:**

```
silent empty-page fallback restored   AssertionError: getSegmentsPage no longer throws on a
                                      malformed payload                                    exit 1
restored                              frontend review-guard source policy passed            exit 0
```

Gates: typecheck 427 files 0 errors, vitest 217, policy 48/48, Playwright **94 passed** (including the
six new header-overflow cases).

### phase 15: the gate could be corrupted by a second copy of itself — twice, today

Not from any audit. This one has two incidents behind it, both self-inflicted on 2026-08-03:

* A sweep was launched while an earlier one was still finishing. They fight over the same fixed debug
  ports, so the loser's probes die on `PRECONDITION FAILED: debug port already answering` — a leg
  failing for a reason that has nothing to do with the code. **Three empty `cortex-egress-*` profiles
  timestamped 10:16:34 went unexplained for hours**; this is what they were.
* `docs/STATUS.md` is stamped with HEAD at **write** time, not run start. The earlier sweep therefore
  labelled its verdict with a commit it had never tested. A green attributed to the wrong code is
  exactly the claim this repo exists to prevent, and the gate was generating it.

The second time it happened the run was killed and discarded rather than read, which was right but is
not a fix: the next tired hand does the same thing.

`single_instance()` refuses to start while a live sweep holds the lock. Refusing is **not a pass** — it
exits 2 (INCOMPLETE), never 0. A stale lock (the holder is gone: killed run, crash) is taken over
rather than blocking forever, because a gate nobody can start is its own outage. `--static` is
deliberately exempt: it runs no legs, opens no ports and writes no STATUS.md.

**Proved against a real second sweep, not a simulation:**

```
1. live incumbent      REFUSING TO START: another verify-10 sweep is already running (pid 62416)
                       exit=2          STATUS.md touched: 0
2. stale lock (dead)   (taking over a stale verify-10 lock from dead pid 999999)   -> proceeds
3. own pid re-entry    silent, no false "stale" claim
4. clean run           lock removed
```

Two flaws in the first cut of this work, both caught by running it rather than reading it: the exit
code was read through a `| tail` so it reported 0 when the process returned 2, and the refusal message
mojibaked its em-dash on the Windows console — the same defect fixed in the watchdog gate hours
earlier. Case 3 was worse than cosmetic: re-entering under our own pid printed "taking over a stale
lock from dead pid <ours>", a small lie in the one message somebody reads when the gate behaves oddly.

### Checked and NOT changed: the write path

Hunted the same silent-failure class as phase 14 on the WRITE side, where the stakes are the owner's
data rather than a wrong screen. It is already hardened:

* autosave failures reach `notifications.error` via `onError`;
* a rejected settings write ROLLS BACK local + store to the last-persisted state and toasts — the
  comment there already names the consent mismatch as safety-critical;
* the close-request handler falls back from `destroy()` to `close()` and surfaces it if both fail.

One genuinely silent path remains: if registering the close-request flush throws, the app runs with no
flush-on-close and only logs it. Deliberately left alone — autosave debounces at **1000 ms**, so the
exposure is at most one second of typing, and a user-facing error for that would be noise. Recorded
rather than "fixed" so the next pass does not re-litigate it.

Policy suite 48/48.

### phase 16: a stalled import would have been reported as a duplicate-transcript problem

Eighth site of the recurring class (a tally counting rows the export drops), found by enumerating every
function that aggregates over `&[SpeechSegment]` and checking each one's membership rule rather than
waiting for a symptom.

`find_duplicate_transcripts` hashed the effective transcript of every segment, skipping only empty text.
Two consequences:

* **Placeholders all share one string.** Every clip stalled at `[Pending WSL 7B ASR]` carries that exact
  text, so a stuck import hashes them into ONE enormous "duplicate transcripts" group. That is not a
  duplicate-content problem, it is an absent-content problem, and it would send the owner hunting a
  data-quality issue that does not exist while the real fault — the stall — is reported elsewhere. Not
  hypothetical: `is_effective_placeholder`'s own docstring records "a stuck-placeholder incident".
* **Rejected clips** counted toward a signal about the dataset that will ship, over rows `export_dataset`
  drops — the same rule already applied in export, conformal calibration, the lift triples, and (as of
  phase 11) the mean WER/CER.

**Measured on the live library before changing anything:**

```
duplicate groups reported        : 0
segments counted as duplicates   : 0
  ...of those HUMAN-REJECTED     : 0
  ...of those PLACEHOLDER text   : 0
```

Latent again — nothing the owner sees moves today. Recorded plainly rather than dressed up.

**Fail-before, both guards removed:**

```
assertion `left == right` failed: only the real duplicate pair is a duplicate group   FAILED
restored                                                                              2 passed
```

**Checked and deliberately NOT changed:** `find_duration_outliers`. A duration outlier is a property of
the AUDIO, and a rejected clip with a suspicious length is still worth seeing — the reject may even be
because of it. Excluding there would hide evidence rather than remove noise, so the same rule does not
transfer. Written down so a future sweep does not "fix" it for symmetry.

Also checked and left alone: `couch::save_session`'s three `let _ =` call sites. They are not silent —
`save_session` emits `tracing::warn!` on every failure path, and each call site carries a comment
explaining why the Result is advisory there ("a session that fails to persist still serves every link it
just issued... Only `revoke` treats this as an error worth raising"). The reasoning holds.

Full lib suite 1102 passed, `cargo fmt` and `clippy --all-targets --all-features -D warnings` clean.

### phase 17: a pass that found almost nothing, written down so it is not re-walked

Four areas probed with a specific failure in mind. Three hypotheses were wrong, and reading beat
guessing every time. Recorded because an unwritten non-finding gets re-investigated by the next pass.

**`couch::save_session`'s discarded Results — not silent.** Three `let _ = save_session(...)` call
sites looked like the swallow-an-error class. They are not: `save_session` emits `tracing::warn!` on
every failure path, and each call site carries the reasoning ("a session that fails to persist still
serves every link it just issued... Only `revoke` treats this as an error worth raising"). The failure
only bites on the NEXT restart, where the reviewer meets the link-expired banner, which is a designed,
understandable path. Left alone.

**Restore-over-a-hot-WAL — structurally impossible here.** The feared bug is a raw file swap leaving a
stale `-wal` that SQLite replays over the restored pages, resurrecting exactly what the owner restored
to escape. `Database::restore` does not swap files: it drives `sqlite3_backup` INTO the live open
connection, so SQLite owns the WAL throughout. It also integrity-checks the snapshot first and refuses
a snapshot from a newer schema. The concurrency window is separately fenced by `RESTORE_PENDING` and
pinned at every writer-start site by `test_restore_reservation_gate.py`. Nothing to add — and no new
drill was written, because building a gate for a gap with no evidence is how a suite gets slower
without getting stronger.

**The write path — already hardened.** Autosave failures reach `notifications.error`; a rejected
settings write rolls back local + store to the last-persisted state and toasts (its comment already
names the consent mismatch as safety-critical); the close handler falls back from `destroy()` to
`close()` and surfaces it if both fail. One genuinely silent path remains — a throw while REGISTERING
the close-request flush is logged only — and is deliberately left: autosave debounces at **1000 ms**,
so the exposure is at most one second of typing.

**0xC0000409 — still one occurrence, still unexplained.** Every preserved log since the failure-log
fix was searched: the ONLY hit is the original `heartbeat-runtime.FAIL.20260803-080631.log`. It has not
recurred across roughly fifteen full sweeps today. That is a measured base rate, not a diagnosis, and
the leg still reds honestly if it happens again.

**The preserved-failure-log fix earned itself twice more.** Alongside the crash evidence, the stamped
logs held `test-rust.FAIL.20260803-232756.log` — a failure never seen live, from the sweep that was
killed for overlapping this agent's edits:

```
error[E0425]: cannot find function `seg` in this scope
assert_ran: command failed (exit 101) -- reporting that, not the count.
```

That was a transient half-edited test (`seg` before it became `mock_segment`), not a product defect —
which independently confirms the decision to kill and discard that run rather than read its verdict.
And `assert_ran.py` behaved exactly as written: the command's own failure reported ahead of any count.

### phase 18: the jury's verdict is now kept where a human edit cannot erase it (migration v48)

Owner instruction: "record the jury verdict separately, then rerun the eval".

**The defect, stated correctly this time.** `verdict_transcript` holds whichever verdict is CURRENT:
`write_segment_verdict` writes the machine's text, then `record_human_decision_by` overwrites it with
the reviewer's correction. `corrections.rs` states the end result as settled fact — *"verdict_transcript
... is the human's ANSWER (the reference/target the evidence is scored AGAINST), never the model
draft"*. `load_lift_triples` passed that column as the JURY hypothesis, so the label-quality lift
compared the human's answer with the human's answer on every decided row.

Phase 10 blamed ACCEPTING a clip and told the reviewer that editing would fix it. Both wrong, and
measured wrong: **34 of the 35 clips the owner had edited are self-referential too**. The product was
giving him work that could not help. Corrected in the card, in `LabelQualityLift`'s doc, and here.

**v48** adds `jury_transcript`, written only by `write_segment_verdict` and by no human path.
`load_lift_triples` now reads it, and excludes rows where the jury never committed one — no machine
text means nothing to score, not a score of zero.

**No backfill, deliberately.** On decided rows the machine's text was overwritten and is gone; on the
77 undecided rows `verdict_transcript` is EMPTY. Copying the current column would either duplicate the
human's answer (re-creating the exact defect) or copy nothing. NULL is the truth for every existing row.

**Fail-before, through the real write paths:**

```
jury column write reverted   assertion failed: the machine's verdict SURVIVED the human's edit
                             left: None   right: Some("دەقی جوری")                       FAILED
restored                                                                                 1 passed
```

The reject-exclusion test's fixture now sets `jury_transcript` AND `verdict_transcript` to different
texts, so it reproduces the shape that made the metric self-referential — the query only survives it by
reading the jury column. Two new assertions pin that the two sides are independent texts.

**What this will and will not do on the owner's library — measured, not assumed.**

```
decision_verdicts.auto_accept_verdict:  144  T1_ESCALATE   (all 144)
verdict distribution:                    77  escalated · 34 human_edit · 27 human_reject · 5 human_accept
undecided rows carrying a machine verdict: 0
```

**The jury escalated every single clip and never committed a verdict of its own.** So the new column
will stay empty until the jury actually commits one, which needs the T0 gate to calibrate — and
`min_calibration_n` at the shipped constants is ~2,334 perfect clips PER SNR BUCKET, already recorded
here as unreachable at one user's volumes. The schema is now correct and the metric honest; the data to
fill it does not exist yet and no amount of reviewing creates it.

**"Rerun the eval" — cannot run, and why.** `gold_segments` = 0 and `eval_runs` = 0. The frozen
348-clip manifest is committed but the FLEURS ckb_IQ audio is not (1 sample clip on disk); fetching it
is `scripts/build_fleurs_ckb_manifest.py`, ~1–2 GB, one-time, and a download needs the owner's
go-ahead. Surfaced rather than silently skipped.

Full lib suite 1103 passed, `cargo fmt` and `clippy --all-targets --all-features -D warnings` clean.

### phase 19: the certificate scored 144 clips on a number nobody measured

Phase 12 hid the "117 Certified Segments" tile while the certificate is uncalibrated. That treated the
symptom. This is the cause, and `pipeline.rs` names it in its own comment:

> Scribe returns no per-segment confidence, so `confidence` stays [None]

Every clip in the owner's library came from that path — `confidence_source = external_provider` on all
144, and `confidence` / `ctc_score` NULL on all 144. `compute_nonconformity_score` defaults a missing
confidence to 0.5 and a missing ctc_score to -5.0, so **every clip scored exactly
`(1 - 0.5) + 0.1·5 = 1.0`** — a constant manufactured from two fallbacks, identical for all of them.

Not a missing writer: the pipeline persists a confidence whenever the engine returns one (288 of 432
`segment_hypotheses` rows carry one, and `agent_confidence` is set on 143/144). The cloud STT engine
simply returns none, and the per-call fallback that is sensible in isolation becomes ruinous as a
population.

`has_scoreable_confidence` now excludes a clip with NEITHER signal from BOTH the calibration set and
the certified set. Such a clip is not an error and is not removed from the library — it carries no
evidence, and **zero is the honest count of what a no-confidence dataset certifies.**

Checked before changing anything: `certified_segment_ids` has no consumer in the UI, the exports or any
gate, so the fabricated certification was never reaching a dataset decision.

**A decorative guard, caught by running the fail-before rather than trusting it.** The first attempt
reused the all-empty fixture: removing the certify-side guard changed nothing, because the calibration
guard alone empties the set, `calibrate_threshold` takes its `<10` cold-start path and returns 0.35,
and the manufactured 1.0 fails that anyway. **The fail-before PASSED.** A guard that is only ever
redundant is not a guard.

The replacement calibrates on 40 real clips whose scores run above 1.0, at a target the Hoeffding slack
can actually meet (0.5, since the slack alone is ~0.29 at n=40), so the threshold reaches the fabricated
score and only the certify-side guard keeps the blind clips out:

```
AFTER                              1 passed
certify-side guard removed ONLY    FAILED  (a clip with no measured confidence certified
                                            against an invented score)
restored                           1 passed
```

The phase-12 test's assertion CHANGED rather than weakened: it used to pin `total_certified ==
flat.len()` to document the tautology; it now pins `== 0`, because the tautology is gone.

Full lib suite 1104 passed, `clippy --all-targets --all-features -D warnings` clean, policy 48/48.

### phase 20: a RED that was the machine, and the habit that would have buried a real one

The sweep gating phase 19 failed:

```
audio/waveform_decode_1600000_samples   411,878 ns vs 126,831  (3.25x, limit 1.36x) REGRESSION
audio/waveform_decode_160000_samples    345,729 ns vs  64,627  (5.35x, limit 1.23x) REGRESSION
```

`docs/STATUS.md` restored, never committed.

**What ruled out the code, before any re-run.** The entire diff between the last green sweep and this
one is `quality/conformal.rs` plus the ledger:

```
git diff --stat a43dcfa..df6e01b -- audio.rs chunking.rs benches/   ->  (empty)
```

A 3-5x decode regression cannot come from a change to conformal scoring. Corroborating: all THREE
members of the decode family moved together (the third reads 11.63x and is already marked "NOT ENFORCED
- 35% run-to-run noise on this machine"), while the other nine benchmarks came in FASTER than baseline.
That is a loaded machine, not a regression. The release exe had been linked minutes earlier and
Defender real-time protection is on — a freshly written 100 MB+ binary gets scanned.

Re-measured on a quiet machine: **0.81x, 1.00x, 1.03x.** Not reproduced.

**The dangerous part is what comes next.** "Re-run it and it goes green" is exactly how a real
regression gets waved through, and this workflow's normal path — relink, then immediately sweep —
guarantees the false RED recurs. Leaving it as a habit would have made every future bench RED
negotiable.

So the second reading is now part of the gate's evidence rather than a private decision:
`bench_gate.py` re-measures ONLY the benchmarks that exceeded budget and fails if the second reading
agrees. **No threshold moves.** A benchmark is a statistical measurement and re-measuring a suspected
regression is what the measurement is for — the opposite of the 0xC0000409 stance two dozen entries
above, where retrying an unexplained CRASH was refused precisely because a crash is not a sample.

A benchmark that vanishes on re-measure fails on the first reading; a missing benchmark is not a clean
bill.

**Proved in the direction that matters — that it cannot wave a real regression through.** The baseline
for `normalizer/1000_words` was doctored to a quarter of its true value, a regression no machine state
can un-see:

```
1 bench(es) beyond budget on the first reading; RE-MEASURING those only ...
  normalizer/1000_words   3,695,207 ns  (3.50x, limit 1.24x) CONFIRMED
BENCH GATE: FAIL - 1 bench(es) beyond their budget          exit 1
```

Baseline restored byte-clean (`git diff` empty). The noise direction is evidenced by the live pair
above rather than by a simulation: 3.25x then 0.81x, same code, minutes apart.

### phase 21: the first real gold evaluation — a measured accuracy record, and what it is NOT

Owner instruction: "download the gold audio and run the eval". Both done. The audit's P0 #1 asks for a
canonical accuracy record; this is the first one this app has ever produced.

**Provenance (charter: every number carries its command, dataset and model).**

```
command    node <scratch>/run_gold_eval.cjs   ->  IPC import_gold_segments + run_gold_eval_asr
corpus     google/fleurs ckb_IQ test, fetched with scripts/build_fleurs_ckb_manifest.py
           922 recordings of 348 distinct sentences, 332 MB, 0 skipped
eval set   the COMMITTED frozen record docs/eval/fleurs_ckb_iq_frozen.rel.tsv
           sha256 4063da0309b11046069bb40f865a75f56053199b28fd37580c4312049c4dd3ce
           348 rows = ONE recording per sentence, 0 re-recordings, 0 of its clips missing from the fetch
engine     omniasr-ctc-300m (the ACTIVE local engine; run_gold_eval_asr refuses a mislabelled engine)
exe        HEAD 49f258b
run id     20933d16-cf52-4302-85d5-2d6bca52e0fd     run at 2026-08-04 07:54:01
N          348
CER        12.58%  (micro; macro 12.578%)
WER        52.05%  (micro; macro 52.002%)
```

**Why 348 and not 922.** FLEURS ckb_IQ ships multiple recordings of the same sentence; the fetch names
them `<id>.wav`, `<id>.1.wav`, ... Scoring all 922 would weight some sentences 3x and understate the
CI, so the eval used the frozen one-per-sentence record the repo already guards with
`test_frozen_eval_manifest_integrity.py`.

**What this number is NOT, stated because it is easy to misuse.** It is the STOCK CTC-300M on FLEURS
ckb_IQ. It is **not comparable** to the two numbers already in this ledger — 29.40% CER for stock
CTC-300M (N=400) and 21.00% CER for the fine-tuned model (N=900) — both measured on DIFFERENT sets.
Placing 12.58% beside either is exactly the "never mix ... without scope labels" the audit objects to.

The one comparison on the same corpus family is unflattering and is reported anyway: ElevenLabs Scribe
publishes **32.1% WER on FLEURS-ckb**; this offline engine scored **52.05% WER** here. Expected for a
300M offline CTC against a cloud model, and not a reason to omit it.

**P0 #1 is substantially, not fully, met.** Present: dataset, N, model, build, date, CER/WER, run ID,
exclusions. **Missing: the confidence interval and the dialect/noise/speaker slices.** The app's eval
path computes micro/macro only — the bootstrap CI lives in `scripts/scorecard_finetuned.py`, which needs
jiwer + transformers + onnxruntime and measures a DIFFERENT code path than the app runs. Recorded as
open rather than claimed.

**Safety around the owner's live library.** The eval writes `gold_segments` and `eval_runs` into the
real profile — that is the point (the panel must show a real run), so it was done deliberately and with
a backup taken first, app cleanly closed: `Desktop\cortex-library-backup-20260804-103931` (db + wal +
shm). After the run: 144 segments and 67 decisions intact, `eval_runs` 1, `gold_segments` 348.

**A repo defect the fetch exposed.** `build_fleurs_ckb_manifest.py` downloaded the whole corpus and then
died: `datasets` >= 4 decodes audio through **torchcodec**, pulling a PyTorch runtime (GBs) purely to
turn a 16 kHz WAV into samples. soundfile is already a hard dependency there (it WRITES the clips), so
the fetch now decodes the raw bytes in-process and hands `write_manifest` — the unit-tested core —
exactly the shape it documents. Smoke-tested at `--limit 5` (5 rows, 0 skipped) before the full run. No
gate would have caught this: a 1-2 GB one-time fetch cannot sit in a sweep, and saying so is more honest
than inventing a test that would not have run either.

**Corpus never reaches git.** The clips are covered by `*.wav`, but the GENERATED manifest was not
ignored and carries the corpus references plus absolute local paths, against the ledger's own
"eval-only corpus; no audio/refs committed". `.gitignore` now excludes
`cortex-speech-app/scripts/fleurs_ckb_iq/`; the committed portable record stays tracked.

### phase 22: the accuracy record gets its confidence interval — and a third decorative test caught

P0 #1 asks a published accuracy record to carry a CI, and phase 21 landed a bare point estimate
(`CER 12.58%`, N=348). A point estimate at that N invites exactly the false-precision comparison the
same audit objects to.

`bootstrap_micro_ci` resamples whole UTTERANCES with replacement (Bisani & Ney ratio-of-sums) — the
unit of independence is the clip, not the character, so a per-character bootstrap would badly
understate the interval. Each replicate recomputes `sum(distance)/sum(ref_len)` over the resampled
clips, matching how the headline micro rate is computed, clamp included. 3000 replicates, seed 42 —
the same constants `scripts/scorecard_finetuned.py` used, so an interval from the app and one from the
published scorecard are directly comparable. When there is nothing to resample the keys are ABSENT
rather than null-filled: a `[0, 0]` on an empty run would be a fabricated interval.

**A THIRD decorative check, and this one was mine twice over.** The fail-before passed, twice:

1. The first sabotage never landed — `str.replace` reports nothing when it matches nothing, so the
   "proof" ran against unmodified code. Now every sabotage asserts its target exists AND that the
   replacement is present before the test runs.
2. With the sabotage verified in the file, the test STILL passed. The fixture gave every clip
   `ref_len = 20`, so `sum(ref_len)` is constant under resampling and the micro rate can only take a
   handful of discrete values — the 2.5/97.5 percentiles land on the same ones whatever the seed. A
   coarse fixture had made the determinism assertion decorative.

Fixed by varying the denominator, after which the guard is load-bearing:

```
AFTER                                   1 passed
seed defeated (verified in the file)    assertion failed: the bootstrap must be deterministic
                                        under a fixed seed                              FAILED
restored (SALT occurrences: 0)          1 passed
```

Three decorative guards have now been caught this way — the string-offset ordering check, the
redundant certify-side guard, and this one — every single time by RUNNING the fail-before rather than
reading it. That is the entire argument for the practice.

Full lib suite 1105 passed, `clippy --all-targets --all-features -D warnings` clean.

### phase 23: the 0xC0000409 crash — instrumented, not fixed, and one inference retracted

Owner asked for it fixed. It is not fixed, and this entry says so.

**Second occurrence** (sweep gating `df6e01b`): `finetuned-ipc-e2e`, exit 3221226505, **0.6s**, stdout
empty. First was `heartbeat-runtime`, 4.7s, first line printed. Still never standalone — 3 more clean
runs today on the exact leg that crashed, on top of the previous 40.

**RETRACTED: "it died before printing its first line, so the crash is at Node startup."** Node's stdout
to a PIPE is buffered, and `run_gate` captures through a pipe — so buffered output is simply lost when
the process dies abnormally. An empty stdout is therefore evidence of an abnormal death, NOT evidence
of when it happened. The 0.6s duration still points early, but the stronger claim was wrong and is
withdrawn.

**Reproduction attempted and FAILED.** The hypothesis was resource pressure during Playwright's module
load (a heavy require tree, and a sweep has many processes alive):

```
rapid repeated `require('@playwright/test')` + e2e_profile        0/25 non-zero
same, with 8 concurrent memory-churning node processes            0/25 non-zero
```

50 clean. Memory pressure at module load is not the mechanism. (The machine has 256 GB, so a system OOM
was always implausible; this rules out the per-process V8 path too.)

**What was done instead: make the next one diagnosable.** `verify_10.py` now exports
`NODE_OPTIONS=--report-on-fatalerror --report-directory=<LOG_DIR>`. Node writes a JSON diagnostic report
— native and JS stacks, heap and resource counters, loaded libraries, the OS error — when V8 or the
runtime dies fatally, which is the class 0xC0000409 belongs to (a CRT/V8 `abort()` on Windows surfaces
as `__fastfail`). Nothing is written on a healthy run.

Explicitly NOT a retry: the leg still reds honestly. This is the same stance as the preserved
timestamped FAIL logs, which are the only reason either occurrence has any record at all.

**Honest status: an open, unreproduced, undiagnosed defect at roughly 2 occurrences in ~20 sweeps.**
Claiming it fixed because a later sweep goes green would be exactly the reasoning this ledger refuses
elsewhere.

### phase 24: the warning count is now actually zero

Owner asked for "any other bugs or warnings" after the crash work. Measured across the three surfaces
that emit them:

```
cargo build --all-targets   0 warnings
npm audit --omit=dev        0 vulnerabilities
eslint src/                 4 warnings   <- the only outstanding ones
```

Four dead type imports: `GoldSegment`, `FewShotExample`, `T0GateReport` in `commands.ts` and `derived`
in `historyStore.ts`. Checked before deleting rather than trusting eslint — each appears exactly ONCE
in its own file (the import itself), is referenced by no other file, and `commands.ts`'s three
`export type` lines re-export none of them. So removal cannot break a downstream import.

After: **eslint clean, 0 warnings.** typecheck 427 files 0 errors, vitest 217 passed.

A warning nobody intends to fix teaches everyone to scroll past the warnings, which is how a real one
gets missed. Zero is the only count that keeps the signal.

### phase 25: the owner's Sorani pass, and the panel stops giving advice that cannot work

**The 10 phone-page strings are approved.** The owner read all ten and passed every one — the last
un-reviewed Sorani on the review page. That item is closed.

**Two new strings, drafted here and rendering-checked rather than assumed.**

`stats.conformalNoConfidence` (A) and the fine-tuned accuracy label (B) now carry, in both locales, the
interval/corpus/date the deep audit asked for:

```
before  Transcribe with the fine-tuned Kurdish model (measured 21.0% CER, N=900)
after   Transcribe with the fine-tuned Kurdish model (21.0% CER [19.9-22.0], N=900 gold clips,
        measured 2026-06-25)
```

**The bidi worry was unfounded, and it took a measurement to find that out.** The concern raised to the
owner was that a bracketed CI and an ISO date would flip inside RTL text. Two attempts to check it by
eye were WRONG in opposite directions: the first render was mojibake (page served with no charset, so
Chromium decoded UTF-8 as Latin-1) and told us nothing; reading glyph order off the second screenshot
suggested the range and date were reversed. Measuring the laid-out client rects in Chromium settles it —
each run read in its OWN direction:

```
tokens, RTL reading order:   ٢١٫٠٪ | ١٩٫٩٪ | ٢٢٫٠٪ | N=900 | ٢٠٢٦-٠٦-٢٥
date, arabic digits, RTL:    ٢٠٢٦ - ٠٦ - ٢٥   = 2026-06-25   correct
date, latin in dir="ltr":    2026 - 06 - 25   = 2026-06-25   correct
```

Nothing flips. B keeps Arabic-Indic digits, matching the numeral style already shipping in ckb.ts.

**A regression this agent introduced, now fixed.** `has_scoreable_confidence` (phase 19) excludes
no-confidence clips BEFORE the provenance counters, so on the owner's library both counters went to
zero — which silently removed the only note explaining why nothing certified, leaving the generic
fallback: *"Uncalibrated fallback. Verify at least 10 segments."* He has **67** verified clips and
**0/144** carry a confidence or ctc_score, because the cloud engine returns none. The panel was sending
him to do work that cannot help. `calibration_no_confidence` now counts those exclusions and the panel
names the real cause.

**A FOURTH decorative check, same offset-comparison mistake as the RefineryPanel guard.** The ordering
assertion compared the position of a CONDITION against the position of a MESSAGE, so swapping only the
two conditions left the no-confidence expression earlier in the file than the fallback text inside its
own branch — and the fail-before passed. Pinning the exact branch text catches it:

```
AFTER                    passed
conditions swapped       AssertionError: ... expected `{#if cert.calibrationNoConfidence > 0 ...}`
                         as the opening branch                                            exit 1
branch removed           AssertionError: no longer distinguishes 'no confidence data' ...  exit 1
restored                 passed
```

(And the repair itself broke the file first: `\n` escapes in a heredoc became real newlines inside
string literals. Caught by the very next run.)

Gates: lib suite 1105 passed, clippy clean, typecheck 427 files 0 errors, eslint 0 warnings, policy
suite 48/48, i18n key sets identical.

## Iteration 241 — audit #10's latency evidence, and three probes that failed before one told the truth

P2 #10 asks for published p50/p95 on import, first transcript, search, review-save and export **on long
files**. The repo already measures RTF, twelve microbenchmarks and durability under hard kill — none of
which is the number a human experiences. `scripts/latency_probe.cjs` measures that number through the
REAL IPC on the REAL exe, with a disposable profile, timing each `invoke` in-page so the sample is the
IPC hop + backend work + reply and nothing else.

**Three runs failed first, and every failure was the harness, not the app.**

1. *No engine provisioned.* A fresh disposable profile has no ASR engine selected, and
   `import_audio_file` only SPAWNS the decode/VAD worker before returning Ok — a failed preflight
   surfaces on the event channel, never in the invoke's result. The probe polled 12 minutes for clips
   that could never arrive. Its preserved profile showed `speech_segments: 0` and jobs rows for exports
   only. That reads exactly like *"import silently does nothing"*, which would have been a serious and
   WRONG bug report. Fixed by calling `provisionEngine` first, as `e2e_real_app.cjs` already did.

2. *A stray `node -c ""` in the launch command* blocked on stdin, and the `;` chain meant the probe never
   started at all. The background task reported `running` for 1h49m — its shell genuinely was running.
   Caught only by noticing that no second `cortex-speech-app.exe` and no `cortex-latency-*` profile
   existed. Taking "status: running" at face value would have meant waiting indefinitely.

3. *Rate limited.* `export_dataset` and `update_segment_fields` sit behind `STRICT_RATE_LIMITER`
   (burst 5, refill 10/s — `throttle.rs`). Twelve iterations with zero think time drained the bucket:
   exports `.0`–`.4` on disk, `.5` missing, `.6` back again. **This is the limiter working, not a
   defect** — the editor autosaves on a 1 s debounce (`autosave.ts`), so a reviewer produces ~1 call/s
   per key against a limit of 10 and cannot reach it. Raising the limit to make the probe pass would
   have weakened a working guard to flatter a measurement. The probe now paces 250 ms BETWEEN samples,
   never inside a timed region.

**Two honesty defects in the probe's own output, fixed before publishing.** The import happens once per
run, so `import_enqueue` and `import_to_first_clip` have n=1 — and the first table printed that single
observation under `p50` and `p95` columns, dressing one measurement up as a characterised distribution.
They now print in a separate block labelled *no percentile is defined*. Separately, the first run used
the 8-second committed fixture, which answers none of what #10 asks: a 3.3 ms library page over a
handful of clips from one short import says nothing about a real session.

**Measured 2026-08-04**, exe built at `61069e5` (only `docs/STATUS.md` differs at HEAD `bf52116`, so the
binary is code-identical to HEAD). Audio: 30.2 min, 57.9 MB, 16 kHz mono Sorani, 153 gold FLEURS clips
concatenated, sha256 `1034ccb8a190f016db13860587f7441b00e0e4296a206d42a2129f78ca4cd1ef`. The library
settled at **135 segments** before sampling began — a page latency is meaningless without saying how
many rows it paged, and sampling while VAD is still emitting clips measures a moving target.

```
  operation             n     p50        p95        min        max
  library_page         12      7.7ms     12.5ms      6.9ms     12.5ms
  search               12      3.9ms      9.2ms      2.7ms      9.2ms
  review_save          12      3.2ms      8.6ms      2.8ms      8.6ms
  waveform             12     60.7ms    277.4ms     53.2ms    277.4ms
  export_jsonl         12     15.1ms     18.1ms     11.6ms     18.1ms

  single observations — one import per run, so NO percentile is defined:
  import_enqueue            4.8ms   (n=1)
  import_to_first_clip  235070.7ms  (n=1)
```

Reproduce — the long file is built under `%TEMP%` and never enters the repo:

Run from `cortex-speech-app/scripts/fleurs_ckb_iq/clips`:

```python
import wave, glob, os
out = os.path.join(os.environ['TEMP'], 'long_ckb_30min.wav')
target, tot = 30 * 60 * 16000, 0
w = wave.open(out, 'wb'); w.setnchannels(1); w.setsampwidth(2); w.setframerate(16000)
for f in sorted(glob.glob('*.wav')):
    if tot >= target:
        break
    with wave.open(f) as r:
        assert (r.getframerate(), r.getnchannels(), r.getsampwidth()) == (16000, 1, 2), f
        w.writeframes(r.readframes(r.getnframes())); tot += r.getnframes()
w.close()
```

```
CORTEX_AUDIO="$TEMP/long_ckb_30min.wav" npm run test:latency
```

**The finding: no progressive availability on long imports.** 3 min 55 s before the first reviewable
clip from a 30-minute file, against 4 s for 8 s of audio — it scales with file length. Both import paths
(`pipeline.rs` `process_single_file` and `process_single_file_streaming`) accumulate every segment in
memory and call `persist_segments` ONCE at the end; "streaming" there means streaming *decode* (bounded
memory), not progressive *persistence*. The UI does receive phase and progress events throughout that
window (`commands.rs` forwards every pipeline event except the terminal one), so it is not a blank
screen. Making persistence progressive is not a small change: whole-file speaker clustering needs every
embedding before any label exists, so clips would have to be inserted unlabelled and back-filled.
**Owner-gated redesign, same class as #4–#8 — not started.**

The probe is REPORTED, NOT GATED, deliberately. An end-to-end latency budget on a developer desktop
reds on background load rather than on regressions — the cry-wolf failure `bench-budget`'s
confirm-re-measure exists to avoid — and a gate people learn to re-run is worse than no gate. Publish
the evidence; gate the microbenchmarks that are actually stable.

## Iteration 242 — the dashboard leads with a decision, and two accuracy claims are withdrawn

Owner go-ahead for deep-audit #7, then "fix H1 and H5". Three fixes and one crash finding.

**#7 — Insights leads with a verdict, diagnostics move behind Advanced.** The panel answers one
question first (can I export a training set?), then names the blockers, the next action, and one
canonical accuracy record. Conformal certificate, inference internals, intelligence report, duration
histogram, top speakers and dataset tools moved into a collapsed native `<details>` — keyboard-
operable and screen-reader-exposed with no JS, which the accessibility gate needs anyway.

The verdict is sourced from `get_training_grade_breakdown`, which runs the SAME
`training_grade_for_segment` the export gates on, so it can never disagree with what an export would
write. `training_grade_summary` now delegates to it: two copies of the grade bucketing is how a count
starts lying.

**A claim in that commit was withdrawn before it shipped.** The first draft said "measured on the
live library: training_ready = 0, no word aligner installed, every clip carries
`energy_heuristic_alignment`". That was never measured. A read-only census the same day shows the
opposite premise — all 144 clips carry `alignment_quality = 'ctc_forced'`, so the aligner IS present
and working. The design still holds; the sentence did not. Amended out of the unpushed commit.

Live census, 2026-08-05, read-only against the real profile: 144 segments · 0.5 h audio ·
`human_decision` 77 undecided / 35 edit / 5 accept / 27 reject (so verified-GOOD is 40, not 67) ·
`confidence` and `ctc_score` NULL on all 144 · 144/144 jury verdicts `T1_ESCALATE` · `is_gold` 0.
Two eval runs, byte-identical (a deterministic re-run, not independent corroboration).

**The a11y gate caught a real coverage loss.** Moving those panels into a collapsed `<details>` made
`axe.spec.ts` red: it used `conformal-cert` becoming visible as its settle signal. Repointing that
wait at an always-visible element would have gone green while SILENTLY narrowing the gate — axe does
not analyze hidden content, so every rule covering those four panels would have stopped running. The
disclosure is opened instead, preserving the coverage the original wait was written for ("and
actually covers that panel"). 94 e2e pass, and the new markup has zero WCAG 2.2 AA violations in en
and ckb/RTL with the section expanded.

**H1 — "identical normalization for all engines" was false.** `MEASUREMENTS.md` introduced the pinned
three-engine table with that claim. The 7B and MMS-1B rows came from Python harnesses (`norm()` =
NFC + lower + whitespace). The stock-300M row came from the RUST harness
(`cargo test --test real_audio ckb_scorecard_on_gold`), whose `wer::normalize_for_metrics` also
applies `normalize_hamza`, `remove_diacritics` and `normalize_numbers`.

Direction, stated rather than buried: extra folding is MORE forgiving, so stock-300M's 11.34% is
flattered and the champion's −4.3 pt lead is UNDERSTATED. 7.03% is not puffed up by this. What is
damaged is the MAPSSWE p-value pairing champion-7B against stock-300M (z = −26.26, p = 5.8e-152):
computed across two normalizations, it measures the normalizers too and is not interpretable as
stated. Flagged and retained — the run really happened.

Prevention rather than documentation: every scorecard stamps `norm_basis` (derived from the ACTIVE
normalizer) into stdout, its JSON summary and a per-row TSV column; `mapsswe_compare.py` REFUSES
(exit 1) to pair two TSVs whose bases differ and warns when a legacy TSV cannot prove its basis. The
stamp is a COLUMN, not a `#` comment — mapsswe's loader reads line 1 as its header.

**H5 — the mandated permanent caveat had gone missing.** `FINAL_READINESS_10.md` §M1.1 requires:
*"Both carry a permanent caveat: training-set overlap with the 7B LoRA is unverifiable (its manifest
is on the offline drive) — stated, never hidden."* `MEASUREMENTS.md` instead called the set
"known-disjoint". Restored. Using the official FLEURS **test** split makes overlap unlikely by
construction — that is why it was chosen — but unlikely is not verified, and only verified licenses a
SOTA claim.

Fail-before, `scripts/test_eval_basis_policy.py` (suite 48 → 49):

```
BASELINE (unmodified)                        exit=0  policy passed
scorecard stamp removed                      exit=1  no norm_basis in JSON summary
stamp decoupled from active normalizer       exit=1  stamp can disagree with basis used
cross-basis refusal disabled                 exit=1  ACCEPTED two different bases
legacy unknown-basis warning removed         exit=1  did not warn basis unverified
H5 disjointness claim reinstated             exit=1  asserts 'known-disjoint'
H1 cross-basis p-value flag removed          exit=1  unsound test would read as sound
RESTORED                                     exit=0  policy passed
```

TWO of those guards were decorative and their own fail-before caught it. The cross-basis check first
asserted the string `CROSS-BASIS REFUSAL` was present — which also appears in the COMMENT above the
branch, so deleting the refusal left the gate green; it now RUNS the script against fixture TSVs and
asserts the exit code. The H5 check first banned the substring outright, forbidding the correction
text that cites the retired phrase; it now distinguishes a bare assertion from a quoted citation.

**0xC0000409, third occurrence — and the instrumentation did NOT capture it.** `heartbeat-runtime`
died with exit 3221226505 (STATUS_STACK_BUFFER_OVERRUN) at 6.7 s inside the sweep, banner printed and
nothing else. Prior instances: `heartbeat-runtime` 2026-08-03 at 4.7 s, `finetuned-ipc-e2e`
2026-08-04 at 0.6 s. All three are Node probe processes that spawn the app and connect over CDP; none
reproduces standalone.

`--report-on-fatalerror` was added for exactly this and wrote NO report. That disproves the recorded
hypothesis (a V8/CRT abort Node could intercept) and points at a hard OS fastfail terminating the
process before Node's handler runs.

Phase markers added to the probe, and the first instrumented run already narrows it: the debug port
takes **8.2 s** to come up, so both crash times (4.7 s, 6.7 s) fall inside the
`spawned, waiting for debug port` window — a `fetch()` poll loop against a not-yet-listening port.
Still NOT a retry: the leg reds honestly, and the next occurrence will name its phase.

## Iteration 243 — a gate that stops lying about a crash, and rights that attach to the recording

Two owner asks: "fix the crash", then "fix #6 per-clip rights schema".

**The crash is NOT fixed, and was not even reproduced.** 60 heartbeat runs under 12 CPU burners with a
rotating debug port came back 60/60 clean — with the 43 prior standalone runs that is 103 clean runs
outside a sweep. CPU pressure alone is ruled out as the trigger.

What the evidence now establishes, each independently:

- the process that exits 3221226505 is **node.exe, the harness** — not the app under test;
- it dies **before measuring anything** (phase markers put both heartbeat deaths inside the 8.2 s
  debug-port wait, a `fetch()` poll against a port that is not listening yet);
- `--report-on-fatalerror` wrote **no report**, and two of the three crashes PREDATE that flag existing
  (added 2026-08-04 13:00; crashes 08-03 08:06 and 08-04 10:07), so it is neither cause nor cure;
- Windows Error Reporting logged **nothing** across four days — an access violation or stack overflow
  would have written an Application Error 1000. Its absence fits a native `__fastfail`, which bypasses
  the reporting path.

So the leg produced NO measurement, and reporting that as "the app failed its responsiveness gate" was
itself a false claim about the app. `run_gate` now re-runs ONCE on `ABNORMAL_EXIT_CODES` and stamps
`<gate>.CRASH.<ts>.log` for the dead attempt FIRST, so an occurrence stays counted even when the retry
passes. Same shape as the `LNK1104` AV-file-lock retry already directly above it.

This REVERSES the "deliberately NOT a retry" stance recorded in `verify_10.py`. That stance was written
when the cause was unknown and the crash might have been the app dying. It is not the app.

Deliberately narrow, and tested as such: a probe that RAN and exceeded its threshold exits 1 and never
reaches the path. `test_abnormal_exit_retry_policy.py` drives `run_gate` for real and pins BOTH halves.

```
BASELINE (unmodified)              exit=0  policy passed
retry removed entirely             exit=1  OS-terminated gate ran 1 time, expected 2
retry widened to ALL failures      exit=1  ordinary failing gate ran 2 times
crash evidence no longer kept      exit=1  no CRASH log kept
RESTORED                           exit=0  policy passed
```

Two earlier versions of that fixture silently exercised NOTHING: Python CLAMPS a large exit value to
0xFFFFFFFF (measured — `SystemExit(3221226505)` yields returncode 4294967295), and wrapping `cmd /c
exit` in Python clamps it again on the way out. The payload is now a `.cmd` that carries the code
directly. A speculative `net.Socket` replacement for the `fetch()` poll was written and DISCARDED: it
addresses only two of the three occurrences, and with 103 clean runs a real fix and a useless one look
identical — shipping it would have let us believe the crash was solved.

**#6 — rights now attach to the recording (migration v49).** The only `license` column in this schema
was on `model_versions`; rights for the AUDIO lived at dataset level and nowhere per recording. Voice
is Article 9 biometric data: lawful basis, permitted use and the ability to honour a withdrawal attach
to the individual recording.

Six nullable TEXT columns on `speech_segments` — licence, consent basis, permitted use, attribution,
provenance, revoked-at. Stored per SEGMENT though the unit is the RECORDING: a join keyed on
`audio_path` orphans the moment `relink_audio` rewrites that path, and the export iterates segments so
enforcement needs the values where the decision is made. `set_recording_rights` covers every segment of
one audio path in a single statement.

NO BACKFILL. All 144 clips become rights-UNKNOWN, which is the truth — inventing "CC-BY-4.0" because
the EVAL corpus happens to be CC-BY would be fabricated provenance.

`RecordingRights::disposition()` is the one place that decides, failing closed at every step:
`Revoked > Unknown > PrivateOnly > Redistributable`. A licence string alone is not consent to republish
a voice — `permitted_use` must NAME redistribution.

Enforcement scoped on evidence, not preference:

- **Withdrawal is honoured on EVERY path**, local JSON/JSONL/CSV/Parquet included. A revocation that
  only blocked publishing while clips kept flowing into local tables would not be a withdrawal.
- **Undeclared rights still export.** Wiring `permits_redistribution()` as a hard refusal was tried
  first and **blocked 18 HuggingFace export tests** — i.e. it would block this entire undeclared
  library the moment the migration lands, silently redefining a working command. That is an owner
  decision, not a schema-change side effect. The predicate exists and is tested for the caller that
  genuinely publishes.

Speaker identity deliberately absent: `speaker_id` already carries a pseudonymous label, and a
real-identity column would create the re-identification risk this schema exists to bound. UI not
included — three IPC commands are the surface.

Verified against the LIVE library after the relink: schema_version 49, all six columns present, 144
segments and 67 decisions intact.

**A staging mistake, caught and undone.** The first rights commit used `git add -A` and swept in an
`audit-2026-08-05/` folder this agent did not create — 16 screenshots of the live library plus a
markdown file embedding absolute user-profile paths that the hygiene gate rejects in tracked files.
Reset, unstaged, and the folder added to `.gitignore` beside the existing `docs/audits/` entry, which
exists for exactly this reason. Files untouched on disk.

## Iteration 244 — production release provenance and fail-closed publication

The 2026-08-05 production audit confirmed that a `v*` tag could publish Windows installers after
printing only a warning when Authenticode credentials were absent. The same matrix also let every
platform builder write to the GitHub Release independently, creating an avoidable publication race
and exposing a repository-write token to build and test steps.

The release path now fails before upload when either Windows signing secret is absent, requires at
least one installer, verifies every signature with `signtool /pa`, and removes the temporary PFX in a
`finally` block. All platforms remain build smoke checks, while the supported Windows bundle gets a
deterministic, streaming SHA-256 manifest and a SHA-pinned GitHub/Sigstore build-provenance
attestation. Builders retain read-only repository access; one dependent publisher downloads the
supported signed bundle and updates the release after every build
finishes. Static policy regressions pin the signing, permission, ordering, checksum, and attestation
contracts.

The audit also found two stale Vite E2E mock contracts: training-grade readiness fell through to an
invalid payload and Tauri window metadata was absent, producing repeated console errors and a
load-sensitive accessibility timeout. The mock now returns the real readiness shape, supplies window
metadata, and exercises the successful media-grant path. The prior readiness-state change's Python
policy was updated to protect the new neutral `not_required` state rather than an obsolete test name.

Verification on this checkout: frontend unit tests 217/217; Playwright 94/94 including axe WCAG 2.2
AA; Rust `--all-targets` 1,114 library tests plus every integration target and the soak test passed
(expected live/model tests remained ignored); all three benchmark harnesses completed; build,
Svelte/TypeScript checks, ESLint, rustfmt and clippy `-D warnings` passed; npm audit found zero
production vulnerabilities; cargo-deny passed advisories, bans, licenses and sources. Public signing,
GitHub-hosted attestation, installer verification, and notarization still require the owner-held
credentials/platform release run and are not claimed from local tests.

## Iteration 245 — two vacuous gates, and a RED verdict that measurement dissolved

Three fixes from the 2026-08-05 deep audit, each with a fail-before proof, plus the resolution of a
bench failure that turned out to be an artefact of how it was measured.

**Counters (`76c90bf`).** The audit read `Clip 1 of 144`, `61/144` and `67 of 144 reviewed` as three
different positions. Two were genuinely inconsistent, not merely worded badly: the position counter
counted `queue.length` while the progress text counted `$segments.length`, and review mode scopes its
queue to an active curate search — so the moment the reviewer searched, the two lines described
different sets. Both now come from one `reviewProgress(queue, $segments)` call. The middle fraction
was `segmentChunkLabel` — position INSIDE the source recording — rendered bare in the same text
weight; the owner's corpus is a single 144-chunk recording, so its denominator collides with the
queue total by coincidence. It now renders with the existing `chunk` key. What deliberately did NOT
move: `allReviewed` stays corpus-scoped, because scoping it to the queue would make a fully-reviewed
SEARCH SUBSET announce that the dataset is finished. The math left the component so that rule has a
real test. Sabotaging it to `total > 0 && done === total` fails exactly one test and nothing else.

**Confidence gate (`b4b1d6e`).** The audit said the constant 0.90 heuristic "must not drive
unattended acceptance". It already did. `training_grade_for_segment` read
`agent_confidence.or(confidence)` and promoted to SILVER on `confidence >= 0.85`, consulting nothing
about provenance. The shipped OmniASR CTC path exposes no token posteriors, so every non-empty
transcript carries a constant 0.90 — the clause was VACUOUSLY TRUE for the entire corpus, a gate that
reads as a quality bar and filters nothing. Engine confidence now counts only with
`ConfidenceSource::RealPosterior`, compared through `as_db_value()` so it cannot drift from the
writer in asr.rs. Fails closed on `None` and legacy `'unknown'`. `agent_confidence` is untouched.
Measured blast radius: all 144 live segments have `confidence` NULL, so no stored row re-grades —
this closes the hole ahead of the next import, it does not rewrite history.

**Policy pin (`679aa70`).** `11332d2` renamed a readiness test and left the policy pinning the old
name; the sweep caught it exactly as designed.

**bench-budget was not a regression.** The 2026-08-05 sweep failed with
`audio/waveform_decode_160000_samples` at 1.41x against a 1.23x limit, CONFIRMED on the gate's own
re-measure. Re-run alone on an idle machine: **1.14x, within budget, BENCH GATE: OK**, with every
other benchmark at 0.80–0.87x. The cause was concurrency, not code — a second agent session ran the
full Rust suite and all three benchmark harnesses between 22:43 and 22:55 while the sweep was
measuring wall-clock, which `docs/bench_baseline.json` warns against in its own comment. Recorded
because the honest reading is "this gate is contention-sensitive and its RED was an artefact", NOT
"benches are noisy, ignore them". It is still the only bench above 1.0x and is worth watching.

That same concurrent session also left eight tracked files uncommitted mid-sweep, which is why the
sweep's `python-policies` leg failed against a tree that had already changed underneath it. Its work
is committed at `113cb38`, attributed to it rather than claimed.

Not claimed: no full sweep has passed since these commits. `docs/STATUS.md` still records the RED.
The CycloneDX/SPDX SBOM and the publication-boundary rights gate remain open.

## Iteration 246 — the publication boundary, an SBOM, and a lockfile npm cannot resolve

**Rights gate (`56d8855`).** Audit #3: "undeclared redistribution rights do not prevent local
export; public publication needs an explicit rights-completeness gate."
`RecordingRights::permits_redistribution()` was written, tested, and had NO production caller. It has
one now. `export_dataset_bundle`'s existing `production: bool` IS the publication boundary — it
already refuses on validation errors, zero training-ready rows, missing hypothesis coverage and stale
source-reference identity — so the rights check joined those rather than being invented somewhere
new. Local export (`production=false`) is untouched and stays revocation-only; wiring redistribution
rights there would block the owner's entire 144-clip library and silently redefine an everyday
command, the objection recorded in export.rs:149-158 when that narrower gate was written.
`local_export_still_works_without_declared_rights` pins the boundary so it cannot quietly widen.
BLOCKS rather than dropping, because a production bundle that shipped 40 of 144 rows would present a
count the operator never agreed to. Fail-before: deleting the gate makes the export SUCCEED and write
a complete 18-file publishable bundle. Consequence, stated plainly: a production bundle of the
current library is now blocked until rights are declared. That is intended for biometric data under
GDPR Art. 9, and it is an opt-in path, not the default export.

**SBOM (`b7cb908`).** Audit #5. 1,142 components (498 npm + 644 cargo), every one with a real hash
from the lockfile that resolved it. No licences are declared — neither lockfile records them reliably
and guessing one in a compliance artifact is the confident-unverified claim the honesty law forbids;
`cargo deny check licenses` still owns that. Written into the bundle BEFORE the checksum manifest and
attestation so it is covered by both, with that ordering pinned in `test_workflow_policy.py`.

**Finding, NOT fixed: the committed lockfile resolves to a tree npm itself calls invalid.** A clean
`npm ci` yields `picomatch@2.3.2` where `svelte-check`'s `fdir@6.5.0` requires `^3 || ^4`.
Reproduced from the committed lockfile in a scratch copy, so it is not local drift. `npm install`
does not fix it; `npm install --package-lock-only` produces a byte-identical lockfile; a targeted
`overrides` entry (`svelte-check > fdir > picomatch: ^4`) changes the reported constraint but npm
still hoists 2.3.2. Consequence: `npm sbom` exits ESBOMPROBLEMS and refuses, which is why the SBOM
is generated from the lockfiles instead. It does NOT break the build — svelte-check runs clean
(typecheck 429 files, 0 errors) — so this is latent, not active. Left unfixed deliberately: repairing
it means moving dependency versions (root `picomatch@2` is pinned there by lint-staged's micromatch
and tailwind's chokidar), and that is not a change to make unattended overnight. It needs an owner
decision.

Not claimed: the release workflow is static content. Signing, attestation, installer execution and
the SBOM step RUNNING on a real runner still need a tag run with owner-held credentials.

## Iteration 247 — two labels that claimed more than the data, and an audit finding corrected

**Validation wording (`a7a6ae0`).** Audit #6. The `passed` COUNT was already right — Round-18 fixed
it to attribute failures by segment id. The words around it were not: `validation.allClear` said
"dataset looks good!" on a corpus where nothing was exportable, and the backend subtitle said "All
144 segments passed validation". Both now name what was checked, and allClear points at the readiness
verdict in Insights, which is the surface entitled to answer that question. The guard is scoped to
`validation.*` VALUES and parses them; two drafts were thrown away first — a raw substring scan
matched the commit's own explanatory comment (failing before anything was sabotaged), and it flagged
`stats.ready: 'Ready to export'`, which is precisely the string allowed to say that. It also refuses
to pass if it parses fewer than 5 validation strings, so a matcher that stops understanding its input
fails loudly.

**Fingerprints (`699118b`) — the audit's diagnosis was wrong, and the truth is worse in one way.**
Audit #4 read "0 audio fingerprints" beside a 144-clip corpus and concluded legacy data was never
backfilled, recommending a blocking migration task. There is nothing to backfill TO: `AudioFingerprint`
is an in-memory `Mutex<HashMap<u64, String>>`, built empty by `lib.rs:571` at every launch, and NO
migration among the 49 has ever created a fingerprint column. The stat reports what the current
session imported; it read 0 because that session had imported nothing, and it sat in the corpus-stats
grid where every neighbouring number is corpus-wide.

Better than reported: no data missing, no migration pending. WORSE than reported, and absent from the
audit: cross-session duplicate detection DOES NOT EXIST. `check_and_register` compares only within one
run, so a restart plus a re-import under a different path is accepted silently. The struct's doc
comment asserted "duplicate content from different paths is still rejected" with no scope qualifier —
a guarantee the code does not provide. `duplicate_detection_does_not_survive_a_restart` now pins the
gap by building the same fresh map `lib.rs` builds at startup and watching the duplicate sail through.
It is written to FAIL when fingerprints are persisted, so the next author is told to replace it rather
than left to find an obsolete test.

Not fixed, deliberately: durable detection needs a persisted per-segment fingerprint plus a backfill
over existing rows — a schema migration on the live library, not a side effect of a labelling fix, and
not unattended work. Raised as a task with the full diagnosis, alongside the npm lockfile defect from
iteration 246.

Sweeps this cycle: GREEN 32/32 at 43d12ee (pushed d6a6677) and GREEN 32/32 at a7a6ae0 (pushed
00efc01). Both on an idle machine with `.month-loop.lock` held, after the 2026-08-05 contention that
made a bench reading untrustworthy.

## Iteration 248 — a blank waveform was a failed read wearing a silent clip's face

Audit #5 reported "the top waveform was blank during inspection" as a cosmetic gripe alongside
fatigue and layout notes. It is not cosmetic. `loadWaveform`'s catch set `waveformData = []` and said
nothing, and an empty array renders exactly like genuinely quiet audio — a flat strip. A failed
decode therefore presented itself to the reviewer as "this clip is silent", with no toast, no
message, nothing in the UI to distinguish "we could not read your file" from "your file is quiet".

The consequence is specific to WHERE it happens: review mode is where clips are verified. A reviewer
can accept a clip whose waveform they believe they inspected and never actually saw. That is the same
failure-looking-like-success class as the commands.ts read guards ("an empty ValidationPanel reads as
no anomalies found"), and the fix is the same shape — the component already had five handlers using
`notifications.error`, and this catch was the one bypassing that path.

`waveformError` now records the failure, the toast fires, and the strip is replaced in place by a
message that says the audio could not be READ and explicitly not that it is silent, with a Retry.
Success clears the flag so one bad clip cannot poison the rest of the queue. The existing
last-call-wins stale-response guard is untouched, so a late failure from an abandoned clip still
returns early rather than raising a toast about a clip the reviewer has already left.

The guard pins four properties rather than one — flag recorded, failure surfaced, flag CLEARED on
success, template actually branching on it. A flag the UI never reads would be dead state that a
naive one-line check would happily accept. Fail-before verified against the original silent catch.

Not in scope, not started: the same audit paragraph asks for sticky primary actions and
sentence-level screen-reader summaries of the consensus words. Both are review-surface redesigns.

## Iteration 249 — the sibling I missed, found by grepping my own fix

`b554515` fixed the failed-waveform-reads-as-silence bug in ReviewMode. Grepping the CLASS afterwards
found `App.svelte`'s curate view doing exactly the same thing — `} catch { waveformData = []; }`, no
toast, no state, a flat strip indistinguishable from quiet audio. Fixed at `3a95c88`, and the guard
now pins BOTH sites and names which one regressed.

Recording this because the miss is the lesson: I fixed the instance the audit pointed at and moved on.
The ledger already holds six separate count-honesty fixes and two blank-transcript fixes, each found
one at a time, before shared guards were written. The habit that catches these is grepping the class
immediately after fixing an instance, not waiting for the next audit to find the next copy.

Curate deserves it as much as review — it is the DEFAULT view, it is where a clip is first inspected,
and it has its own Verify button. Accepting a clip from a flat strip believed to be silence is the
same wrong outcome from either surface.

Two other swallows examined in the same sweep and deliberately NOT changed:

- `ensureWordTimings` catch — a documented best-effort degradation with an honest VISIBLE fallback
  (whole-clip playback, labelled by `review.playingWholeClip`). The user is told what they are getting.
  Not a defect.
- `loadConsensus` catch — on failure `draftModels = []` silently hides the "Draft by <engine>"
  provenance badge. Real, but a different shape from the waveform bug: it REMOVES information rather
  than asserting something false, and the consensus card is legitimately absent on single-engine clips
  anyway, so its absence carries no false claim. Judged milder and left for a decision made awake
  rather than fixed at 05:30 on my own judgement. Named here so it is not silently skipped.

## Iteration 250 — swept the count-honesty class; one live instance found and measured

Following the waveform-sibling lesson from iteration 249, swept the OTHER recurring class in this
repo: tallies that count rows the export drops. Six prior fixes of that class are in this ledger, so
the sweep was overdue rather than speculative.

Most of it holds up. `stats.rs` guards `verified_count` and `pending_count` with explicit REJECTED
and placeholder predicates, documents the SQL-mirror risk, and pins itself against the REAL export
path via `the_dashboards_verified_count_equals_what_the_export_actually_writes` rather than against a
second copy of the rules. `quality.rs` and `validation/mod.rs` attribute by segment id. That is the
right shape.

One live instance found: `total_chars` / `avg_chars_per_segment`. MEASURED on a copy of the live DB
(144 segments):

    shipped            rows=144  chars=42679  avg=296.4
    export-consistent  rows=117  chars=34685  avg=296.5
    delta              7994 chars (18.7%) from 27 human-rejected rows

The averages coinciding is chance — the rejected clips happen to be average length. It is not
evidence of no impact, and recording it that way would be the kind of comfortable reading this
ledger exists to prevent.

The char SUM sits in the SAME query as the two counters that DO exclude rejects, and uses
`COALESCE(annotated, normalized, raw)` rather than the `EFFECTIVE` expression defined twenty lines
above it. The frontend `buildLocalStats` repeats the omission with a THIRD selection order
(`normalized || annotated || raw`), directly between `verifiedCount` and `pendingCount`, both of
which carry comments explaining why they exclude rejects.

NOT fixed here, deliberately. Excluding rejects gives totalChars a denominator (117) different from
the `totalSegments` (144) displayed beside it — which is precisely the mixed-denominator defect
`76c90bf` fixed in ReviewMode this same night. Choosing between "exclude and relabel the tile" and
"keep corpus-wide and say so" is a product-semantics call, and making it unattended risks
reintroducing the bug I had just removed. Raised as a task with the measurement and both options.

Also examined and left alone: `total_duration_ms` and `COUNT(*)` are corpus-wide by design and are
labelled as such, so they are consistent rather than inflated.

## Iteration 251 — an adversarial sweep found a withdrawn voice still shipping, and a false claim of mine

Ran a 10-agent read-only hunt (5 lenses, each paired with a refuter told to assume the report wrong)
over subsystems tonight's work had not touched: couch, jury/IRT, the export family, eval metrics, and
cloud-consent enforcement. Findings below are ones I re-verified myself before acting — the agents
located them, they did not license them.

**SEVERE, fixed (`be64cf5`): `export_audio_segments` and `eval::export_finetune_pack` wrote the audio
and transcripts of recordings whose consent had been WITHDRAWN.** A grep of `is_revoked` /
`rights_for_segment` across `src-tauri/src` returns three sites only; those two exporters had none.
Biometric data under GDPR Art. 9, written to disk after a withdrawal.

**And a correction I owe.** `56d8855`'s message asserted "A withdrawal carries no such ambiguity: it
must be honoured on every path, and it is." It was not. I verified the paths that commit touched and
generalised. The claim was false when I wrote it and stayed false for seven commits.

Fixed inside the shared filter every export path already calls, not at five call sites — a rule
enforced per-caller is one the sixth caller skips, which is how this happened. Renamed
`exclude_holdout_segments` -> `exclude_unexportable_segments`: the old name under-described what it
drops, and that mis-naming is precisely why five dutiful callers still leaked. Fail-before verified —
removing the filter writes the withdrawn WAV back.

**Open, verified, NOT fixed** (recorded so they are not lost, not deferred silently):

1. Non-production bundle ships revoked transcripts, and its manifest/dataset-card counts disagree
   with its own data files (`export_bundle.rs:306-318` missing the filter its siblings have).
   Partially addressed by the shared filter above — needs re-measuring against the count sites.
2. A cloud opt-in revoked MID-RUN never reaches a running worker: `ProcessingPipeline` holds settings
   by value in an `Arc` that `update_settings` REPLACES rather than mutates, so an in-flight import
   keeps uploading audio after the user turns cloud off. Contradicts the fail-safe doctrine stated at
   `commands.rs:1877-1881`. Needs `Arc<Mutex<..>>`/ArcSwap — a concurrency change, owner-gated.
3. `couch.rs:1621` grades a spot check whose answer key was removed against `""`, fabricating a 1.00
   CER (accept/edit) or 0.00 (reject) for a reviewer, averaged into `spot_check_report` with no
   filter. Guard tests `verified` where it should test `human_verified_text(&prev).is_none()`.
4. `jury/mod.rs:331` writes the IRT AGREEMENT confidence onto rows escalated for poor AUDIO, so a
   distrusted clip sorts to the BACK of the riskiest-first queue and renders a green "97% confident"
   chip in the inbox.
5. `couch.rs:1805` `api_undo` claims a lease with no holder check, unlike its two siblings; a
   legitimate holder is then 409'd on a conflict the undo created.

Items 2-5 are real and re-verified but each needs either a concurrency redesign or a product call.

## Iteration 252 — a stop instruction that never reached the worker

`71340a4`. From the iteration-251 sweep, re-verified by reading the code first.

Every long entry point runs on `state.lock_pipeline().clone()`; `update_settings` swapped the `Arc`
on the STORED instance only. A clone taken earlier keeps the old pointer. So an import started before
the user switched cloud off kept uploading audio and transcripts for every remaining file — while the
save succeeded, `get_settings` reported off, and the toggle rendered off. The user has every reason
to believe they stopped it.

Consent is now a shared `Arc<LiveConsent>` (three atomics, never replaced) applied BEFORE the snapshot
swap. Only consent moved: the other 77 `self.settings` reads keep the snapshot, because a model or
threshold changed mid-import should not retroactively apply — the import is one coherent unit. A
withdrawal is not a preference, it is a stop instruction, and it is honoured at the moment of the
call. Retyping the field would have touched all 80 sites for no privacy gain.

Fail-safe both ways, and tested that way: revoking mid-run halts egress; GRANTING mid-run does not
retroactively enable a run the user began understanding nothing would leave the machine. The live
check applies only where the path actually uses cloud — local refinement is untouched, pinned by its
own test.

Fail-before: deleting the single propagation line fails the revocation test while both fail-safe
tests still pass, so they are checking a different property rather than echoing the fix.

Remaining from iteration 251, unchanged and still open: the non-production bundle count mismatch, the
couch spot-check graded against an absent answer key, the jury agreement-confidence-on-audio-escalated
rows, and `api_undo`'s missing lease-holder check.

## Iteration 253 — four findings from the external review, closed with evidence

External assessment 2026-08-06 rated the app 8–8.5/10, "strong personal-use release / controlled
pilot, still not general public-production". Its four in-code correctness findings are now closed;
each was re-verified by reading the code before acting, and each has a fail-before proof.

**#1 revocation was contingent on free disk space (`f30774f`).** `71340a4` made a withdrawal reach
in-flight pipeline clones, but `commands::update_settings` applied it only AFTER a successful save.
On a full or read-only disk the save fails, the command returns Err, and egress stayed enabled for the
running import. Fixed asymmetrically rather than by reordering: GRANTING still commits only after the
save (enabling cloud on a change that will not survive a restart is the mirror hazard, and
`revoke_consent_now` cannot grant — pinned), WITHDRAWING takes effect first. The residual divergence
is deliberate and one-directional: after a failed save the UI can show cloud ON while egress is
stopped, never the reverse. BOTH ways of breaking the ordering — deleting the call and moving it after
the save — left the crate compiling and every other gate green, including the cloud-privacy gate,
which is exactly why the guard is a source-shape test on the ORDER.

**#4 spot check graded against an absent answer key (`3a08d01`).** The guard tested `!prev.verified`,
strictly weaker than the predicate that minted the key. The desktop "mark bad" strips the answer while
KEEPING verified, so `human_verified_text` returns None and `.unwrap_or_default()` scored the reviewer
against "" — a fabricated 1.00 CER for a correct transcription, averaged into spot_check_report, with
an HTTP reply byte-identical to success. Now carries the KEY rather than a bool, so "no key, no grade"
is structural. Fail-before is exact because the block runs before the already-reviewed guard: old code
returns 200 having graded; fixed code records nothing and falls through to an honest 409.

**#3 undo stole an active lease (`5e39034`).** `api_undo` inserted unconditionally, unlike api_renew
and api_decision which both refuse when `holder() != reviewer`. Reachable from the write-two failure
state already pinned in this file: Sara's undo took Hemn's lease and HIS save was then 409'd over a
conflict the undo manufactured — and per couch.html a 409 discards the queued decision, so his typed
correction was lost. `holder()` already expires stale entries, so it is the right predicate. The
companion test passes BOTH before and after, proving the fix does not overcorrect into "never grant",
which would reintroduce the queue-grabs-it-back problem the line existed to prevent.

**#7 bundle closure evidence (`3f5edf4`).** Logically fixed by be64cf5, not demonstrated. The new test
compares the manifest, dataset.json's rows, its embedded metadata total, and the dataset card against
one another, and scans EVERY file for the withdrawn content. Removing the filter fails it naming
`training_grade_details.json` — an artifact a count-only assertion would never have caught. #7 is now
closed in this ledger.

Still open and NOT claimed: jury confidence conflating agreement with acoustic quality (#2, needs
separate fields — a schema change), fingerprint persistence (#5), the npm lockfile (#6), and
cancellation of an HTTP request already in flight, which means a withdrawal stops the NEXT call rather
than aborting one mid-transfer.

## Iteration 254 — #6 partly, #4 landed: duplicate detection now survives a restart

**#6 (`ae8d344`).** Retired `SessionState::from_db`, which materialised every segment — transcripts,
alignment JSON, evidence blobs — to compute two counts, at BOOT, so startup scaled with the corpus.
Two SQL counts now. `verified` is counted RAW there deliberately (session breadcrumb, not the
dashboard figure compute_stats owns) and the test pins that with a rejected clip present.

The larger contribution is `test_unbounded_segment_reads_policy.py`, which CLOSES the set of
whole-library reads: BY DESIGN (an export or validation report is defined over the corpus) vs
TO RETIRE (the real backlog). It found `session/mod.rs` and `validation/mod.rs`, both missed by my own
grep. 7 by design, 4 to retire. `get_active_learning_queue` is the hard one and no cursor helps it.

**#4 (this iteration).** Migration v50 adds `audio_fingerprint INTEGER` with a partial index, and the
mechanism is wired end to end:

- pipeline.rs had `let _fp = check_and_register(...)` — the fingerprint was computed on every import
  and THROWN AWAY. That is why nothing survived a restart. It is now stamped onto the recording's rows
  after persist_segments, best-effort so a failed stamp cannot fail an import already committed.
- lib.rs rehydrates the map from the library BEFORE the pipeline is built, so the first import of a
  session already knows every recording previous sessions saw.
- `duplicate_detection_does_not_survive_a_restart` was written to FAIL once this landed. It has been
  replaced by its inverse, as intended.

`fp as i64` / `as u64` are BIT-CASTS, and the round-trip test uses a fingerprint with the high bit set
(past i64::MAX) — a numeric conversion would saturate half of all possible values and half the corpus
would silently stop deduping.

NOT done and not claimed: rows imported before v50 have a NULL fingerprint and do not participate
until a backfill pass computes theirs (computing one requires DECODING the audio, which a schema
migration may not do). Those rows are exactly as protected as they were before — no regression, just
not yet covered. The owner's 144 are all in this state. The backfill binary, modelled on
`realign_segments.rs`, and the question of what to REPORT (never delete) if it finds duplicates
already in the library, remain open.

The v40 STRICT-recreate test's index count moved 10 -> 11. Raised deliberately rather than relaxed to
`>=`: that assertion exists to prove the table recreate rebuilt every index.

## Iteration 255 — the external re-audit's P0/P1/P2 head: identity, backlog reads, and the fold

Source: `docs/TRUE_10_REMAINING_2026-08-06.md`. Seven commits. What was actually closed, and what was
deliberately not.

**P0.2 — `npm audit` was green while the installed tree was invalid.** `npm ls --all` exited
`ELSPROBLEMS` at the same commit the audit called clean. A clean audit only says "no known CVE in what
resolved"; it says nothing about whether the tree resolved at all. Root cause was HOISTING, not a bad
version: `fdir@6.5.0` declares `picomatch ^3 || ^4` as an OPTIONAL peer, npm hoisted picomatch@2.3.2 to
the root for micromatch/anymatch/readdirp, and the hoisted copy could not satisfy it. Declaring
picomatch ^4 top-level moves v4 to the root and nests v2 under its real consumers, whose resolved
versions are unchanged — NOT an override forcing micromatch onto a major it does not support.
`npm ls --all` now runs beside `npm audit` in the verifier gate, the Makefile target and both CI
workflows.

**P1.1 — a spectral collision could discard legitimate audio (migration v51).** v50 made the 64-bit
fingerprint durable, which fixed its TIME scope and not its SEMANTICS. Eight per-band mean energies
XOR-folded into a u64 was the definitive content key; the shifts discard most of each band's ~30
significant bits, so two unrelated clips at the same level in the same room can collide — and when they
did, import returned `Err("Duplicate audio content")` and REFUSED the second recording. Two tiers now:
the spectral value is a candidate BUCKET, blake3 over canonical PCM + sample rate is the definitive key,
and only a cryptographic match may reject. Prefer a duplicate over silent loss of legitimate speech.

The honest gap, stated rather than hidden: a v50-era row has a bucket but NO content hash. It loads, it
counts, and it can NEVER reject an import — a value that cannot distinguish content must not discard a
recording. Startup WARNs the count and names the backfill; `backfill_fingerprints` now fills both
columns and reports duplicates on the CONTENT hash, so its report means "byte-identical", not "similar
loudness".

**Two defects found reviewing my own work in the same session, both real:**
- The streaming path's whole-file identity was persisted but never CHECKED. Its per-window checks
  compare window hashes, which by construction never equal the whole-file hash, so a long recording
  re-imported in a LATER session sailed past every one of them. Cross-session dedup LOOKED implemented
  and did nothing for exactly the files that path exists to handle. `register_identity` is now private:
  there is no public "register without checking" entry point, because that API existed for one commit
  and was immediately used wrongly.
- `content_hash` fed blake3 one sample at a time. MEASURED on a 90 s 16 kHz window (1,440,000 samples):
  195.4 ms per-sample vs 4.5 ms blocked, same digest byte-for-byte — ~8 seconds of pure hashing added to
  importing a one-hour recording, on a path the user waits on. Blocked through a 4 KiB stack buffer.
  Still explicit `to_le_bytes`, never a slice transmute: this digest is PERSISTED and compared across
  runs, so it must not silently acquire a dependency on host endianness.

**P1.3 — `commands.rs` retired from the unbounded-read backlog (1 of 4).** The 7B refinement driver, the
CTC-score backfill and the signal-anomaly backfill each read the WHOLE library and then `continue`d past
every row already done. The work list is a WHERE clause. All three now use
`Database::get_pending_segments(PendingWork::_)`; after the first pass the result is empty, where the old
code still paid for the whole corpus every time. The 7B predicate keeps `segment_awaits_wsl7b` as its
sole authority and uses SQL only as a deliberate SUPERSET —
`sql_placeholder_prefilter_is_a_superset_of_the_rust_predicate` compares the two through real SQLite,
because under-selecting would make those clips invisible to the driver forever with nothing reporting an
error.

NOT retired, with sharper reasons recorded in the policy rather than the backlog shrinking by
reclassification: `dataset_analytics.rs` needs a STREAMING fold and must NOT reimplement the grading
policy in SQL (that would fork the rule the export gates on, which is the P1.2 drift bug in a new
place); `couch.rs` should count on pending IDs and hydrate only the QUEUE_BATCH it hands out;
`segments_read.rs` is P1.4's conformal threshold.

**P2.2 — the four review decisions fell below the fold at 1280x720.** Deciding a clip and moving on is
the hot path of the product, and accept / save & next / mark bad audio / undo were all off-screen.
Mark-bad was worse: grouped with the re-transcribe TOOLS, so the four were never visible together at
all. One sticky bar reusing `ReviewInbox`'s `.verb-bar` pattern. `sticky`, not `fixed` — sticky keeps
its space in the flow, so it can never sit on top of the waveform, the transcript or a 200%-zoom
reflow. The keyboard hint moved INSIDE the bar, because a sticky element overlays whatever follows it.

**P0.1 — a killed sweep now leaves a durable record.** "A result that ran but cannot be retrieved is
operationally indistinguishable from no result." The summary printed only after the LAST gate, so a
caller that gave a ~40-minute sweep a ~30-minute timeout discarded every gate that had already passed.
`verify_10.py` appends one fsync'd JSON line per gate AS IT FINISHES. It proved itself the same hour:
the sweep's stdout was pipe-buffered and unreadable while the JSONL gave live per-gate status.

**Environment, no longer assumed.** The WSL OmniASR-7B champion is up on both 3090 Tis (17.5 GB each)
and was preflighted the way the audit asks — a real Sorani transcript from a committed FLEURS fixture
through the actual line protocol, not an open-port check.

**Two gates caught this work, and both were right to.** The v40 STRICT-recreate index count moved
11 -> 12 for v51's partial index (raised deliberately, never relaxed to `>=`). The runtime-panic policy
pinned `lock_known`'s full generic type and fired when the map's VALUE became `Vec<KnownRecording>`; it
now matches the signature and COUNTS `self.known.lock()` occurrences instead — the invariant it is
actually about, and stronger than the string it replaced.

NOT done and not claimed: P1.2's versioned risk contract (the `snr < 5 / clipping > 0.1` threshold still
lives in three hand-synced copies — `jury/mod.rs`, the suspect-first SQL and `ReviewInbox.svelte`),
P1.4's calibrated active learning, P2.1/P2.3, and every P3/P4/P5 leg that needs human annotators,
speaker-disjoint evaluation or a signing identity.

## Iteration 256 — GREEN 32/32 twice, and the unbounded-read backlog down to one

Continuing `docs/TRUE_10_REMAINING_2026-08-06.md`.

**P0.1 CLOSED, and reproduced.** Two full sweeps, both 32 PASS / 0 FAIL / 0 skipped: `c4fb8aa`
(3693.3 s) and `39e22b8` (after P1.2 + the couch retirement). The reproduction is the point — the audit
asked for a result a rerun on the named machine can produce again, not one lucky green.

`ignored-real-model` was the ONE kept gate with no current passing evidence. It failed for an
environmental reason: nothing was listening on 127.0.0.1:8799. The WSL OmniASR-7B champion now loads one
replica per GPU on both 3090 Tis and the leg passes in ~45 s. Preflighted the way the audit asks — a
real Sorani transcript from a committed FLEURS clip through the actual line protocol, not an open-port
check. `start_7b_server.ps1` warns its nohup detach dies under a non-interactive runner; a headless
session must hold the `wsl -- bash -lc "exec python ..."` child alive itself, which is what was done.

**P1.2, first slice.** `snr < 5 dB` / `clipping > 0.1` existed in THREE hand-written copies — the jury
veto, the suspect-first SQL, and `hasPoorAudio()` in ReviewInbox.svelte — each with a comment saying it
was "kept identical on purpose". The drift is not cosmetic: if the UI's copy rises above the jury's, the
review screen shows a reassuring green agreement badge on exactly the clip the gate refused to trust,
which is the failure 6028824 already had to correct once. The two Rust sites now share
`quality::POOR_AUDIO_*` and SUSPECT_FIRST_ORDER is BUILT from them.
`suspect_first_sql_and_the_jury_veto_agree_on_poor_audio` pulls the CASE arm out of the live SQL string,
runs it through SQLite and compares against `has_poor_audio` at values exactly on either side of both
thresholds — two constants that merely happen to be equal today would pass a weaker test. TypeScript
cannot import a Rust const, so the third copy is GATED rather than synced; 5/5 drift mutations caught,
including a vacuous-pass guard, and the gate tells the next reader to DELETE it if the UI ever takes the
verdict from the backend.

NOT done: the rest of P1.2 — persisted distinct agreement/acoustic/alignment/OOD risk, machine-stable
reason codes, policy and calibration versions, and renaming `agent_confidence`.

**P1.3: three of four retired (was zero).**

- `commands.rs` — three background passes read the whole library then `continue`d past every finished
  row. Now `get_pending_segments(PendingWork::_)`. The 7B predicate keeps `segment_awaits_wsl7b` as its
  sole authority and uses SQL only as a deliberate SUPERSET, pinned by
  `sql_placeholder_prefilter_is_a_superset_of_the_rust_predicate` through real SQLite — under-selecting
  would make those clips invisible to the driver forever with nothing reporting it.
- `couch.rs` — `api_queue` walked every pending row's FULL record to hand out at most 25. Its
  heldByOthers/skippedByYou/pendingTotal counts genuinely need every pending row (they depend on
  IN-MEMORY lease state, which no SQL aggregate can see), but a row being merely COUNTED needs nothing
  but its id. Now leases on ids and hydrates the served batch OUTSIDE the state lock.
  `the_queue_serves_clips_in_the_order_it_leased_them` pins the re-index, because `get_segments_by_ids`
  imposes its own ordering and would otherwise change which clip is "clip 1 of 25". All 58 couch tests
  pass untouched.
- `commands/dataset_analytics.rs` — and here "aggregate it in SQL", which the audit suggested, was the
  WRONG retirement. Each of the three statistics is computed by a shared Rust policy function; the
  training-grade one is literally the function the EXPORT gates on, so a SQL copy would fork the rule and
  let the dashboard disagree with what an export writes. Retired by bounding the ROW, not the rule:
  `for_each_segment` streams and each command folds into a small accumulator. The conformal case made
  TWO passes with the second gated on the first's threshold, so the second pass's input is captured
  during the first rather than re-reading the corpus.

Every pre-existing test for those statistics exercises the SLICE entry point and stayed green without
touching what actually changed, so
`streaming_the_corpus_statistics_equals_collecting_them_first` compares streamed vs collected as
serialized JSON over a varied 24-segment fixture — and asserts the fixture is non-degenerate first,
because comparing two empty results is a vacuous pass. The seeded bootstrap means equality also proves
both saw the same clips in the same order.

Left: `segments_read.rs` alone, and it is P1.4's problem — `get_active_learning_queue` calibrates its
threshold from the corpus. Doing that honestly needs a frozen human-labelled calibration split, which
does not exist until the Gold Marathon, so it is NOT something to fake now.

**Three defects found reviewing this session's own work, none caught by a gate.** A streamed recording's
identity was persisted but never CHECKED (cross-session dedup looked implemented and did nothing for
long files); `content_hash` fed blake3 two bytes at a time (195.4 ms vs 4.5 ms per 90 s window, same
digest — ~8 s added to importing a one-hour recording); and a runtime-panic policy pinned `lock_known`'s
full generic type and fired on a legitimate design change, so it now counts `self.known.lock()` sites
instead — stricter than the string it replaced.

**Two things flagged for the owner rather than silently changed.** `api_queue`'s doc comment says
"oldest first" while its SQL is `created_at DESC`; which clips a reviewer meets first is a product
decision. And `npm run tauri build` exits non-zero on this rig at the NSIS BUNDLING step (os error 32,
the 512 MB installer still being written) even when the exe compiles and links correctly — harmless
while `signed-installer` is descoped, and a trap the moment it is not.

## Iteration 257 — escalations now say why, and four consecutive GREEN sweeps

Continuing `docs/TRUE_10_REMAINING_2026-08-06.md`.

**P1.2 core CLOSED: escalated clips record WHY.** Every T0 escalation wrote
`write_verdict(..., None /*rationale*/, None /*evidence_json*/, ...)`. The row recorded THAT a clip was
escalated and its agreement score, and nothing else — a reviewer opening one saw
`escalated=1, agent_confidence=0.73, rationale=NULL`. Four genuinely different causes collapsed into
that same empty record: `low_snr`/`clipping` (a bool), `single_recognizer` (the same bool),
`model_disagreement` (nonconformity over threshold), and `uncalibrated_bucket` — a separate fail-closed
branch that was not even in the audit's own list of codes, and which is not a statement about the audio
or the models but about the evidence available to judge them. They call for opposite reviewer actions,
so a queue that cannot tell them apart cannot be triaged.

NO MIGRATION: `evidence_json` was already persisted, already threaded through `write_verdict`, already
parsed by `export.rs`. A new column would have been schema for data that already had a home.

`has_hard_distrust_veto` is now DERIVED from `hard_distrust_reasons` rather than computed beside it, so
the routing decision and the recorded explanation cannot disagree — the test asserts that relationship
instead of trusting it. Codes are `const &str`, not an enum: they are persisted and read back, and
renaming an enum variant would silently reinterpret history. `low_snr` and `clipping` are SEPARATE
because clipping means re-record while low SNR may only need a denoise pass. `threshold` is null when
the bucket is uncalibrated rather than a borrowed number — there was no threshold, and recording one
would invent evidence for a decision that did not use it. `policyVersion` and the threshold must be
stored because the conformal threshold RECALIBRATES as the corpus grows: without them, "this clip had
no low_snr" is indistinguishable from "the policy did not check low_snr yet".

The distinction to keep: stored codes answer "why was this DECIDED"; the live review badge answers
"what is TRUE now". A clip escalated for low_snr whose audio was later replaced should still show why it
was escalated while the badge shows clean. Making the badge read stored codes would be the three-copy
threshold bug in reverse.

**P2.3, the real defect.** `type Blocker = { ...; action?: 'relink' | 'review' }` and `pendingReview`
had carried `action: 'review'` since it was written, but the template only ever branched on `'relink'`.
The one blocker a reviewer can always act on rendered as a dead sentence: the readiness card named the
problem and offered no way to fix it. A dropped-INTENT bug — data model and template each made sense
alone and disagreed only with each other, so nothing failed. It survived because StatsDashboard took no
props at all and had no route out of itself. The gate now compares declared actions against
branched-on actions in BOTH directions (an unreachable branch is the mirror-image bug). Fail-before 4/4,
including a literal replay of the shipped bug.

NOT claimed: the other four blockers still have no action. Honest deep-linking needs their exact
filtered clips and no such filter API exists; sending the reviewer to the generic queue and calling it a
deep link would be the gesture this audit exists to catch.

**Documentation/governance.** Six planning docs dated 2026-06-17 to 2026-07-24 read as live plans with
nothing marking them overtaken. Each now leads with the canonical hierarchy — the gate, its GENERATED
STATUS.md, this dated re-audit, this ledger — archived rather than deleted, because the reasoning is why
later decisions were made.

**Four consecutive GREEN sweeps** at c4fb8aa, 39e22b8, e54d07c, 8bfbf5d — 32 PASS / 0 FAIL / 0 skipped
each. P0.1 asked for a result the named machine can REPRODUCE; four agreeing runs at four commits is a
property of the build rather than a lucky green.

**Five defects found reviewing this session's own work, none caught by any gate, all mine.** A streamed
recording's identity persisted but never CHECKED; `content_hash` feeding blake3 two bytes at a time
(195.4 ms vs 4.5 ms per 90 s window, same digest); a runtime-panic policy pinning a generic type it was
never about; a sweep script that polled for `cargo.exe` before the build had spawned it and would have
swept a STALE binary; and a monitor that read `runs.jsonl` before it was archived and reported the
PREVIOUS run's GREEN as fresh evidence. One shape underneath all five: trusting a proxy — a process
name, a file's contents, a bool that had discarded what it knew — instead of checking the thing itself.
The freshness guard caught the fourth and `run_id` caught the fifth, and both exist only because P0.1
demanded reproducible evidence.

**Outside every audit category, recorded so it is not rediscovered:** `heartbeat-runtime` passed on a
RE-RUN after an OS kill (exit 3221226505 = 0xC0000409, STATUS_STACK_BUFFER_OVERRUN) produced no verdict.
The gate refusing to score a crashed run is correct, but the ledger records that same signature at phase
23 as "instrumented, not fixed". Still live, still intermittent, currently visible only because a retry
absorbs it. And `npm run tauri build` now fails 4/4 at the NSIS bundling step (os error 32) while the exe
itself compiles, links and passes exe-freshness — makensis DOES complete the 488 MB installer and the
handle is taken immediately after close. Harmless while `signed-installer` is descoped; a release blocker
the day it is not.

## Iteration 258 — the agreement rename, the full reason vocabulary, and a privacy gate made stronger

Closing out `docs/TRUE_10_REMAINING_2026-08-06.md` P1.2.

**`agent_confidence` -> `agreement_score` (migration v52).** The old name invited exactly the reading the
jury rejects: every recognizer can confidently agree on the same garbage, so a HIGH value is fully
compatible with a wrong transcript — which is why `has_hard_distrust_veto` refuses to auto-accept such a
clip. 6028824 had already had to stop the review UI rendering this as a green confidence badge; the name
now states what the number IS so the next reader cannot make that inference from the schema alone.
86 occurrences across 16 files, plus an integration test the first pass missed.

`migrations/mod.rs` was deliberately NOT rewritten. v11 created the column and two table-recreate
migrations re-list it; those describe what actually ran on this database, and editing them would make the
chain describe a past that did not happen. v52 does `RENAME COLUMN` — preserving the data, the column's
ORDINAL position (so the index-based `map_row` is unaffected) and every value's history.

Three migration tests failed, and the failure was worth having. All three synthesize an "old" database by
creating one at HEAD and selectively reverting, which leaves HEAD's column names under an old version
number; replaying v40 then fails because its INSERT...SELECT names `agent_confidence` explicitly. BOTH
real upgrade paths were traced and hold — a DB at v51 renames at v52 and never replays v40; a DB at v30
replays v40 while the column still has its old name and reaches v52 after. So the fix belonged in the
synthesis, and the repo already had the precedent: the same test renames `signal_anomaly_score` back to
`ood_score` to be "a faithful pre-v39 snapshot rather than a records-only fake". No assertion was
weakened. Fragility recorded for whoever renames a column next: v40's hardcoded 34-column INSERT...SELECT
means ANY future rename breaks replay-from-synthesized-old.

**The reason vocabulary is complete — nine codes across both jury stages.** T0 records `low_snr`,
`clipping`, `single_recognizer`, `model_disagreement`, `uncalibrated_bucket`; T1/T2 now record
`policy_hold`, `missing_audio`, `t2_no_majority`, `t1_unresolved`. The last four are not statements about
the CLIP at all, which is why they need their own codes: a `policy_hold` needs a dial change, a
`missing_audio` needs a file, and a reviewer listening harder fixes neither. `t2_no_majority` vs
`t1_unresolved` is the pair most easily conflated and is deliberately split — a panel ANSWERING "no
consensus" is a different fact from a panel never being consulted, and only one is fixed by turning a
setting on.

Prose `rationale` is kept at every site. It carries the specific IO error or model set a fixed vocabulary
cannot; codes carry what can be counted, filtered and translated. Neither replaces the other. Codes are
pinned BY LITERAL VALUE in test: they are persisted, so a rename is a data migration, not a refactor.

**A privacy gate was made STRONGER, not accommodated.** `test_cloud_privacy_policy.py` matched the exact
single-line source of the cloud-OFF escalation call. Adding an `evidence_json` argument made rustfmt wrap
it, and the gate failed on a formatting change with the privacy behaviour identical. The tempting move
was to relax the string. Instead it now pins SEMANTICS: `reason::T1_UNRESOLVED` (written only on the
cloud-disabled branch) proves that branch still escalates, and an explicit absence check proves it does so
without touching `t2_listener::`, `reqwest`, `api_key` or `segment_audio_as_wav_base64`. Fail-before 4/4 —
including egress smuggled into the cloud-off branch, WHICH THE OLD ASSERTION WOULD HAVE PASSED. Matching
source layout was brittle in the direction that mattered least and silent in the direction that mattered
most.

**Process error, twice now.** The ledger-staleness gate turned two sweeps RED (4136883 and c638e6e)
because the entry was written after launching the sweep instead of before. Each cost a full ~50-minute
re-run. In both cases the shortcut — re-running only the failed gate and combining it with the other 31 —
was available and refused: "31 from one run plus 1 from another" is composite evidence, which is exactly
what these gates exist to reject.

Six consecutive complete GREEN sweeps preceded this: c4fb8aa, 39e22b8, e54d07c, 8bfbf5d, 1c01ec6,
22ae99b. The 22ae99b run is the one that matters for the rename — v52 applied against a real database
through real-app-e2e, 25 hard kills in durability-drill and 15 mid-export kills, not only unit tests.

## Iteration 259 — 0xC0000409 is TWO populations, and only one of them was ever instrumented

Sweep `20260807T152120-103fd9a` came back **RED** on `test-rust`:

```
Running tests\e2e_pipeline.rs
fatal runtime error: Rust cannot catch foreign exceptions, aborting
exit code: 0xc0000409, STATUS_STACK_BUFFER_OVERRUN
```

Not caused by this session's work — the commits in flight touched jury reason codes, a Python policy and
this ledger, none of which go near the ONNX path — and `cargo test --test e2e_pipeline` passes 10/10
standalone immediately after. But "not mine" is not "not real", and re-running until green is exactly the
move this project refuses.

**The finding: the crash has two distinct populations, and the ledger has been treating them as one.**

  1. NODE HARNESS deaths — `heartbeat-runtime` (x2), `finetuned-ipc-e2e`, `test-e2e+a11y`. The process
     that dies is `node.exe`, the harness, before it measures anything. `run_gate` re-runs these ONCE,
     after stamping a timestamped CRASH copy, because reporting "the app failed its responsiveness gate"
     when no measurement exists would be a false claim.
  2. APP NATIVE-STACK deaths — `test-rust` / `e2e_pipeline`, today and at the very first sighting
     (ledger line 4342). What dies is the app's OWN test binary: a C++ exception out of sherpa-onnx
     crossing the FFI boundary, which Rust cannot catch. This IS a verdict about the app.

The retry correctly did NOT fire today: `test-rust` exited 4294967295 (-1), because cargo surfaces the
child binary's abnormal death as an ordinary failure, and only 3221226505 is in ABNORMAL_EXIT_CODES. The
gate redded honestly, which is the designed behaviour working.

**And the gap that follows from the split:** phase 23's instrumentation is
`NODE_OPTIONS=--report-on-fatalerror`. That is Node-only. It cannot capture population 2 at all — so the
MORE serious population, the one that is a statement about the shipped app rather than the test harness,
has never had any instrumentation. Every reproduction attempt logged in phase 23 (50 clean runs, memory
pressure at module load ruled out) was aimed at population 1.

**A correction to something this session said earlier.** The retry was described as "a gate whose green
depends on silently re-running an OS-level crash". That was unfair and is withdrawn: it preserves a
CRASH log first, prints a loud warning, stamps `[OS-terminated ... re-ran once]` into the gate result,
fires once, and only on abnormal exit codes. Nothing about it is silent, and it cannot turn a real
regression green — a probe that RAN and exceeded its threshold exits 1 and never reaches that branch.

**Not fixed, and not claimed as fixed.** What is newly established is the population split and the
instrumentation gap, not a cause. Naming the failing test inside `e2e_pipeline` would need that binary
run single-threaded so libtest prints a name before the abort — a permanent cost on the longest gate, for
a crash seen twice in this population over weeks. That is an owner call, not something to spend the
sweep budget on unilaterally.

## Iteration 260 — the crash is not binary-specific, and the diagnostic proved it by being wrong

Owner asked for the single-threaded diagnostic. It was built, armed on `e2e_pipeline`, and immediately
disproved the assumption it was built on.

**What was measured before building it.** `--test-threads=1` on the whole `test-rust` gate costs
365 s vs 140 s for the lib suite alone (2.6x) — roughly +14 minutes on every sweep to instrument one
file. So the serialisation was scoped to the crashing binary with a mutex (+14.5 s: 31 s vs 16.5 s), and
paired with an explicitly FLUSHED breadcrumb file, because libtest writes `test <name> ... ` WITHOUT a
trailing newline and stdout to a pipe is line-buffered — the same trap this ledger already documented for
Node. Verified under a real SIGKILL, not argued: four completed START/END pairs and exactly one dangling
START naming the in-flight test.

**Then the next sweep redded on `test-rust` again — and the breadcrumb said `e2e_pipeline` finished
cleanly.** The abort was in `pipeline_integration.rs`, same signature, different binary.

That is the finding. The crash is NOT an `e2e_pipeline` bug. It is in the shared native ONNX/sherpa path
and lands on whichever test binary happens to be exercising it when the C++ exception escapes. Three
occurrences in population 2 now: `e2e_pipeline` twice, `pipeline_integration` once. Arming a single file
did not narrow the problem — it MOVED the blind spot, and the only reason that is now known rather than
suspected is that the instrumented binary could prove its own innocence.

**Response:** the helper moved into `tests/fixtures/mod.rs` and labels every entry `binary::test`, so a
shared log answers both questions. `pipeline_integration` is armed alongside `e2e_pipeline`. Six more
binaries touch the ASR path (`batch_asr`, `gold_wer_eval`, `real_audio`, `reliability`, `soak`,
`user_data_test`) and are NOT yet armed — named here so the next occurrence in one of them is
recognised as coverage rather than mystery.

**Still not fixed, and still not claimed as fixed.** What is established: the population split
(iteration 259), the instrumentation gap, and now that population 2 is a property of the native stack
rather than of one test file. What is not established: which call throws, or why it only happens inside a
full sweep.

Two sweeps redded honestly to learn this (20260807T152120 and 20260807T172029), both preserved. Neither
was papered over with a re-run, and the second one is the one that paid.

## Iteration 261 — complete instrumentation coverage, decided by measurement not caution

Iteration 260 armed two binaries and named six more as deliberately unarmed, on the reasoning that
instrumenting eight files against a crash seen three times was speculative. That reasoning was checked
rather than kept, and the numbers overturned it.

**Measured cost of arming everything:** `reliability` is the only binary with a large live test count
(23) and serialising it costs **+3 s** (19.6 s vs 16.7 s). Every other candidate runs 1-2 live tests, the
rest being `#[ignore]`d. So complete coverage costs seconds, not the minutes the caution assumed — and
"cheap enough to be worth it" is a measurement, not a judgement call.

**And the ignored tests are the ones that matter most.** `real_audio` (21 ignored) and `batch_asr`
(1 ignored) run under the `ignored-real-model` gate against REAL models — the heaviest native ONNX/sherpa
usage in the entire suite, and therefore the likeliest place for a C++ exception to escape. Leaving them
unarmed would have left the blind spot exactly where the crash is most likely.

All 69 tests across all 8 native-path binaries are now armed: e2e_pipeline 10, pipeline_integration 4,
batch_asr 1, gold_wer_eval 6, real_audio 22, reliability 23, soak 1, user_data_test 2.

**A bug in the first arming pass, found by checking coverage instead of trusting the script.** The
original regex keyed on the function NAME (`fn test_*`), so it silently missed every test not following
that convention — `gold_wer_real_omniasr`, `soak_import_many_minutes_of_synthetic_audio`,
`user_dataset_export_path_stays_under_target`, and 9 of real_audio's 22. It reported "armed 0" for two
files and nobody would have noticed until a crash in one of them produced nothing. Rewritten to key on
the `#[test]` ATTRIBUTE and made idempotent, then verified by counting armed-vs-total per binary rather
than trusting the reported number.

That is the same failure shape this session keeps finding in its own work: trusting a proxy (a naming
convention) instead of the thing itself (the attribute that actually defines a test).

Still not fixed and still not claimed as fixed. What this buys: the next occurrence, in any of the eight,
names its binary and its test instead of costing a sweep and yielding nothing.

## Iteration 262 — the unbounded-read backlog reaches zero, by completion

`commands/segments_read.rs` was the last entry, and it had been recorded as blocked. It was not — the
audit conflated two separable problems and the block only applied to one of them.

`get_active_learning_queue` read the WHOLE library as full records (transcripts, alignment JSON,
evidence JSON) to compute one conformal threshold and return at most `limit` clips. The audit filed it
under "compute the threshold in SQL". But the SELECTION RULE and the MEMORY SHAPE are independent:

  - the rule really is naive — rank by distance to a single threshold — and fixing it needs the frozen
    human-labelled calibration split that does not exist until the Gold Marathon. That is P1.4 and it
    STAYS OPEN.
  - the memory shape needs none of that, and is now fixed.

ONE streaming pass does both jobs: the `ConformalTally` accumulates the certificate while every
unverified row's nonconformity is captured alongside it. `q_hat` is not known until the pass ends, but it
is a GLOBAL constant applied afterwards, so the per-segment score is all that must be carried. What
survives the pass is `(id, score)` per unverified row — tens of bytes — and only the `limit` clips
actually returned are hydrated, the same shape couch.rs already uses.

`streamed_active_learning_ranking_matches_collect_then_sort` compares the streamed order against the
ORIGINAL algorithm computed from a collected Vec on the same corpus, including the stable-sort tie
behaviour that keeps equal-uncertainty clips in corpus order. That equivalence is the entire risk: this
order decides which clip a reviewer is asked to judge first. Fixing the memory shape does not make the
ranking good — it makes it affordable.

**The emptiness assertion in the policy gate was deleted DELIBERATELY, which is what it asked for.** It
existed to stop the backlog vanishing by attrition — entries quietly dropped until the gate looked
satisfied. That is not what happened here. Four entries, four commits, four regression tests:

  commands.rs                   get_pending_segments(PendingWork::_) + SQL-superset gate
  couch.rs                      lease on IDs, hydrate the batch, served-order == leased-order test
  commands/dataset_analytics.rs for_each_segment folds + streamed-vs-collected equivalence test
  commands/segments_read.rs     one streaming pass + ranking-equivalence test

The gate keeps its teeth: a NEW unbounded read still fails (verified by injecting one), and every
BY_DESIGN entry must still genuinely read the whole library or the `stale` check fires. 7 by design,
0 to retire.

P1.3 is complete. P1.4 remains open and is honestly blocked on data, not effort.

## Iteration 263 — branch protection as code, and the first credential audit this repo has ever had

**Branch protection: could not apply, did not fake.** No `gh` CLI on this machine and no
GH_TOKEN/GITHUB_TOKEN; the only credential lives in Windows Credential Manager, and extracting a stored
secret to drive the API is not something to do quietly. The GitHub connector needs an OAuth flow a
headless session cannot run. So the part that IS mine to do is checked in:
`scripts/setup_branch_protection.sh`, dry-run against a stubbed `gh` with the emitted JSON verified
field by field.

Hand-clicked protection is invisible — it lives in a settings page, nobody can review it, nobody can
tell when it drifted. As code it is auditable and reproducible on a fork or a restored repo.

Two decisions recorded rather than buried. `required_approving_review_count = 0`, because GitHub will
not let you approve your own PR and `1` makes `main` permanently unmergeable by its only maintainer —
so this enforces "the gates passed", NOT "somebody else looked", and that distinction is stated in the
script. And `enforce_admins = true`, without which the protection is theatre; the consequence is that
the direct `git push origin <branch>:main` this repo documents STOPS WORKING, including for me, after
37 fast-forwards this session with nothing enforcing anything. The script deliberately does not touch
verify_10.py's OWNER_GATED list: configuring the thing and claiming the gate are different acts.

**The repo had NO credential scanning of any kind.** Privacy policies cover consent and egress,
`test_windows_repo_hygiene.py` covers hardcoded profile PATHS, the DPAPI tests cover keys AT REST —
nothing looked for a key that had been committed. On a PUBLIC repo, "caught later" means "caught after
it was published, indexed and scraped".

FIRST FULL AUDIT, at 40f8f32: **4052 blobs, reachable AND unreachable, 1677 commits back to
2026-06-17 — zero credentials.** No AWS/GitHub/OpenAI/OpenRouter/Google/Slack/ElevenLabs keys, no
private-key blocks, no JWTs. The real Windows username appears in NO blob, ever — the hygiene gate has
been doing its job. The one `sk-...` string in history is a fixture inside the DPAPI test asserting
"the plaintext key must never be on disk", i.e. the test proving keys are not stored.

PII found and already remediated: four files once carried `/Users/hawzh/` machine paths; all four are
gone from the working tree (two deleted outright, two cleaned) and survive only in history. NOT
rewriting history for it: that would break every commit SHA — including the eleven `chore(status)`
commits whose entire value is naming the exact commit a GREEN sweep verified — to remove a username
already public in the repository's own name.

`test_secret_hygiene.py` is now a policy (auto-discovered; the suite is 54, was 53). Tracked-tree mode
runs in 1 s so it gates every sweep; `--history` does the exhaustive 4052-blob pass in 41 s on demand.

**Two bugs in my own tooling, both found by checking rather than trusting.** The first scanner HUNG:
`git rev-list --objects` returns TREES as well as blobs, and skipping a non-blob without draining its
payload desynchronises `cat-file --batch` so the next header parses object data. And the coverage line
said "scanned 3979 of 8434", which needed explaining rather than reporting — chasing it is what
surfaced the unreachable-object gap and closed it. An audit that quietly scans less than it claims is
worse than no audit.

Fail-before on the gate found a real weakness too: the AWS and Google patterns pinned EXACT lengths, so
a key of any other length slipped through. For a scanner that is the dangerous direction — over-matching
costs a glance, under-matching costs a leak — so both are length-tolerant now. 8/8 real-shaped keys
caught, 3/3 placeholders correctly ignored.

**P5.6 is NOT closed.** Its history-and-secrets half is clean and now permanently gated; signed
commits/tags, Scorecard >= 8 and the self-hosted runner are untouched, and the leg stays descoped.

## Iteration 264 — protection applied, and three gates that were red or vacuous for over a week

**Branch protection is live**, closing what 263 could only prepare. `gh` 2.97.0 installed, owner
authenticated (the OAuth device flow is interactive and stays the owner's to run — the code `gh` prints
in the TERMINAL is the one the browser asks for, which is where two attempts went wrong). Applied and
verified three independent ways: the API read-back, the PUBLIC `protected` flag (`false` this morning,
`true` now), and a real push refused —

    remote: error: GH006: Protected branch update failed for refs/heads/main.
    remote: - Changes must be made through a pull request.
    ! [remote rejected] codex/newbranch -> main (protected branch hook declined)

A settings page saying "protected" is not proof; a rejected push is.

**Checked the gates BEFORE requiring them, and that is what stopped a self-inflicted lockout.** Of the
four contexts the script pins, `Linux Build Smoke` and `macOS Build Smoke` had passed **0 of the last
8 runs** and `Windows Release Gate` 3 of 8. Applying protection as written — with `enforce_admins` —
would have made `main` permanently unmergeable for everyone. The script's footer treats lockout as a
recovery scenario; here it was the guaranteed first outcome.

Both Build Smoke gates died at the same step, `npm run test:python-policies`, which passes on Windows.
Two policies were Windows-only:

- **`generate_release_checksums.py` sorted Path OBJECTS.** `PurePath.__lt__` compares case-INSENSITIVELY
  on Windows and case-SENSITIVELY on POSIX, so the same bundle produced different manifests depending on
  the builder — in a module whose first word is "deterministic". Measured:
  Windows `['app.tar.gz', 'msi/cortex.msi', 'SHA256SUMS']`, Linux `['SHA256SUMS', 'app.tar.gz', ...]`.
  No shipped manifest is wrong (`release.yml` runs it under `if: runner.os == 'Windows'`), but a manifest
  whose bytes depend on the builder cannot back a reproducibility claim. Now keyed on the relative POSIX
  string — the exact text each line carries.
  The old test asserted the WINDOWS order, so it **encoded the bug instead of catching it**. The new
  guard pins names the two sort rules disagree about; fail-before on Windows shows the pre-fix sort
  yields `['apple.bin', 'nested/Inner.bin', 'Zebra.bin']` and both assertions reject it.
- **`test_watchdog_decisions.py` drives a `.ps1` and copies `powershell.exe`** to build its decoy.
  Neither exists on Linux/macOS, so it could only ever fail there. Skips now, as its sibling
  `test_watchdog_enabled.py` already did — honest here precisely because the subject is ABSENT, not
  unverified.

54/54 on Windows and 54/54 on Linux after; Linux was 52/54. Real evidence, not prediction: run #655 came
back `completed/success` with all four contexts green, and only then was protection applied. That also
settled the single `Rust tests` failure at `8118933` as intermittent — that commit changes one line of
`docs/STATUS.md`.

**The nightly has failed 10 of 10 nights, back to at least 2026-07-30, and nobody was reading it.**
Three separate faults, found only because authenticated `gh` could finally read CI logs:

1. **The mutation gate has never tested a single mutant.** cargo-mutants copies the PACKAGE to `$TMPDIR`,
   so `frontendDist "../dist"` resolved to `/tmp/dist` and `tauri::generate_context!` panicked at compile
   time. `outcomes.json`: `total_mutants 0` against "Found 16 mutants to test"; caught/missed/timeout/
   unviable all 0 bytes. So the charter's **"0 surviving mutants in irt/conformal/ood/diff/normalizer"
   has been VACUOUS** — not passing, never running. The fix was already written down: the Makefile's
   `mutants` target has used `--in-place` since it was added, with a comment saying it is REQUIRED for
   exactly this reason. CI simply never got the flag.
2. **The real-audio job is killed at its timeout inside the soak test.** I nearly "fixed" the cache for
   this — it does always miss, and `Post Cache` is skipped because `actions/cache` will not save on a
   failing job, so it can never warm. The timings killed that theory: compilation was **2m 45s**. The
   soak ran ~70 minutes without finishing.
3. **The real-audio suite itself never runs** — that step takes 0 seconds for want of `CORTEX_REAL_AUDIO_DIR`
   fixtures. The workflow is honest about it (it emits a warning annotation), but "Nightly Real Audio"
   currently exercises no real audio. Owner-gated: it needs fixtures on a runner.

**The soak, and three wrong answers before the right one.** What is measured: on this rig the import
takes **174.9 s for 12 clips, and the per-file cost is FLAT** — 28.2 s for the first (it pays for model
load) then 13.2 s each for the remaining eleven. Nothing accumulates. The modified test still passes
here at 175.4 s against its unchanged 300 s bound, clippy clean.

What is NOT known: why the same test is killed at the timeout in CI. Three attempts to answer it from
this machine, and the discipline is in discarding them rather than in having had them:

1. *"The cache always misses."* True — `Post Cache` is skipped because `actions/cache` will not save on a
   failing job, so it can never warm. Killed by the timings: compilation was 2m 45s of a 75-minute job.
2. *"Wall time scales with cores; a 4-core runner needs ~15 min."* Written into this ledger before the
   measurement came back, and wrong: it assumed total CPU work is constant across hosts.
3. *"It does not converge on a small host — 9,502 CPU-seconds, unfinished at 40 minutes."* Also wrong,
   and this one was my own instrument. `available_parallelism()` reports **64 on this box whatever
   affinity mask the process is given**, because Windows splits >64 logical CPUs into processor GROUPS
   and reports one group. So pinning to 4 CPUs never made the code believe it had 4 — it built 64-wide
   pools and then contended on four. Both pinned runs measured the harness. The tell was visible twice
   before I read it: 133 threads at 128 cores, 136 at "4", and 136 again with the workload cut to a
   quarter. A thread count that ignores every variable you are changing is the instrument telling you it
   is not connected.

So the honest position is that the CI cost must be measured IN CI. What shipped serves exactly that: the
callback was `|_| {}`, which is why the job printed **nothing between "running 1 test" and being killed
69 minutes later** — a hang and slow work are indistinguishable from silence. It now prints per-file
progress, and that output is what exposed both the sequential per-file processing and my own broken
emulation within one run of reading it. The budget scales below 18 cores purely so CI fails with a
legible assertion instead of an opaque timeout; it is a hedge, not a model, and says so.

**A policy test that was a change-detector, not a policy.** `test_workflow_jobs_have_timeouts` pinned
exact strings ("timeout-minutes: 75"), so it broke on a legitimate change AND — far worse — would have
passed for a NEWLY ADDED job with no timeout at all, since it only asked whether those strings appeared
anywhere in the file. An untimed job hangs to GitHub's six-hour ceiling. It now parses each job (text
only; adding PyYAML would have re-introduced the very portability class fixed above) and asserts every
job has a timeout under a 180-minute cap, so "just raise it" cannot quietly hide a hang. Fail-before
confirms it catches both an untimed job and an over-cap one.

## Iteration 265 — a review session that could not be reached, and a key that was being deleted

**The couch server is public, and the credential no longer travels.** Tailscale Funnel is on for
`<this-pc>.<tailnet>.ts.net`, which is what lets two reviewers on laptops with nothing installed reach
the queue — the shipped "from anywhere" link is a tailnet CGNAT address and would have needed Tailscale
on their machines. (Hostname redacted: this repo is public, and test_windows_repo_hygiene is right that
a device name adds nothing to the evidence. It caught this entry on the first run.)

That was gated on closing the query-token leak FIRST, which is the order REMOTE_PUBLIC_LINKS_PLAN
prescribes and the reason it was done before flipping Funnel rather than after. `token_from_request`
now reads the HttpOnly cookie and nothing else; `couch.html` no longer reads or appends a query token
anywhere. Verified from the public internet, not just locally:

    GET /                      -> 200      (shell is public by design)
    GET /api/queue             -> 401      (no credential)
    GET /api/queue?t=<REAL>    -> 401      <- the leak, closed
    POST /api/claim {token}    -> 200 + Set-Cookie
    GET /api/queue (cookie)    -> 200, reviewer=Reviewer G / reviewer=Reviewer E

35 test call sites authenticated with `?t=` and would have gone on proving a door the product no
longer has; they present cookies now. Two tests changed SUBJECT rather than syntax and are recorded in
the code rather than deleted quietly: the empty-vs-wrong query-token unit test (a question that is
unaskable once the query is unread), and the returning-reviewer test's "a wrong token must not fall
back to a cookie" assertion — a hazard that is GONE rather than unguarded, with revocation now
asserted where it actually lives, in the cookie.

**A `couch.html` reference I removed took the whole page down, and only a test caught it.** Deleting
`queryToken` left one live use of it in the address-stripping branch. The script threw at load, so
`queue` was never initialised and the page rendered nothing — a total outage of the reviewer UI,
invisible to typecheck and to eslint, caught by `couch_page_speaker_change_badge.test.ts` failing with
"Cannot access 'queue' before initialization". Three tests that look like they are about a speaker
badge were the only thing standing between a security fix and a dead review page.

**The Gemini key was being DELETED by design, and the UI reported it as configured.** Measured
2026-08-10: an import failed 3/3 with "Gemini API key is required" while Settings showed a key. Two
independent defects, either one fatal:

  * `AppSettings::load` CLEARS `llm_api_key` and rewrites settings.json (P0.3: a plaintext key must
    never persist). The Settings field wrote there. So the key was stored, scrubbed on next load, and
    gone — while `llm_api_key_configured` stayed true, which is the worst possible signal: the UI
    reporting a credential that no longer exists.
  * `ensure_source_reference_transcripts` read that same scrubbed field, so even a correctly stored
    key would not have been used. Every other cloud path (scribe, jury, OpenRouter) already read
    secrets.env via `ApiKeys`; this was the one caller reading a field guaranteed empty.

Both fixed: the pipeline reads `ApiKeys::load(data_dir).gemini` (falling back to the in-session typed
value, so "I just entered it" still works), and the UI's two Gemini fields now call `set_api_key`,
which the backend already accepted for `"gemini"` and which nothing had ever called. The badge reads
CONFIGURED PROVIDERS rather than the input box — the only honest signal that a key survived.
Fail-before: with the settings-only read restored the new test fails; file restored byte-identical.

**Two more measurement lessons, both cheap and both mine.** The new key test passed alone and failed in
the full suite — the Windows write-then-read visibility flake this repo has now hit five times; it
carries the settle loop the `resolve_file_in` tests already use. And `models_status` reported 4 of 7
models missing while the pipeline was using them: `target/release/models` held a PARTIAL copy, which
wins the bundled-root selection and orphans every sibling that exists only in the repo models dir —
the exact hazard models.rs documents. 494 of 494 segments carrying speaker IDs is what proved CAM++ was
running while the panel called it missing. Hardlinked; 7/7 now.

Queue for the session: 494 clips, 0 blank, 3-10 s chunks (a 10 s cap reaches for a pause sooner and is
easier to verify by ear; the chunker splits at the quietest point, so it does not slice words), all
from the champion 7B with speaker IDs. Gold set 348 and eval results 696 preserved across two queue
wipes — checked before each, because the first instinct was to move the whole DB aside, which would
have detached the frozen eval basis behind every measured CER in this repo.

> **CORRECTION (2026-08-11).** "All from the champion 7B" above is FALSE, and it was never checked
> before it was written. The DB says every one of the 494 rows carried
> `model_version_id = finetuned-mms-ckb`, `confidence_source = fine_tuned_no_posterior` and
> `cloud_call = 0`: the drafts came from the fine-tuned MMS-CTC-1B int8, no 7B pass and no LLM
> refinement ran at all. `use_finetuned_asr = true` overrides `asr_model_size = WSL7B` by design
> (pipeline.rs), so the 7B server sat up and idle throughout, and the Gemini refiner returned None
> because the API key had been deleted by the settings-scrub bug fixed the same day. The owner caught
> it by reading the transcripts and asking what produced them — not by any gate. See Iteration 267.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 266 — the review session went live, and the save button that discarded the key

**Two reviewers are working over the public internet, verified end to end from a browser.** The
session is up for Reviewer G and Reviewer E on the Funnel host; 15 checks pass against the public URL, not
localhost: shell 200, `/api/queue` and `/api/audio/<id>` 401 without the cookie, a REAL token in
`?t=` still 401, claim 200 with `HttpOnly; SameSite=Strict`, an unknown token refused identically,
and a 373 KB clip actually streaming to the reviewer.

Backend checks were not enough on their own — last iteration a dead `queryToken` reference rendered
the reviewer page blank while every API check passed. So the page was loaded in a real browser this
time: "Cortex · Reviewer G", clip 1 of 494, RTL intact, the audio element carrying a real 11.7 s duration,
zero console errors, and the token gone from the address bar (claimed into the cookie and stripped by
`history.replaceState`, so a screenshot cannot leak it).

**The Gemini key had a SECOND way to be lost, and it was the obvious one.** With the field rewired to
the encrypted store behind its own "Save key" button, the panel then had two save buttons — and the
bottom-right one, bigger and blue and the one anybody reaches for, still routed through
`update_settings`, whose `load()` scrubs `llm_api_key`. Measured: key pasted, Save pressed,
`GEMINI_API_KEY` still empty. Same trap on the OpenRouter field.

Fixed at the convergence point rather than by relabelling a button: `flushPendingKeys()` runs from
`save()` (before `updateSettings`, since that is the scrubbing path) and from `onDestroy` (the
✕/Escape route), so every exit persists a pending key through `save_key_protected`. Fail-before, all
four mutations caught: flush deleted from `save()`, flush moved after `updateSettings`, flush deleted
from `onDestroy`, and flush rewritten to assign into `localSettings`. Panel restored byte-identical.

**The new gate reported success while executing nothing.** `test_settings_key_persistence_policy.py`
had no `__main__` block, so `run_python_policies` ran it, got exit 0, and counted it among "55 policy
test scripts passed" — a vacuous gate, which is worse than no gate because it reports a safety it is
not providing. This is the same class as the i18n reference check that passed having inspected zero
references. It self-runs now, and its helper no longer overruns into `onDestroy` (which made the
encrypted-store check fail against correct code).

**An OpenRouter key is sitting in plaintext.** `OPENROUTER_API_KEY` is 73 raw characters in
`secrets.env`, not a `dpapi:` blob — it never went through `set_api_key`, which encrypts. Reported to
the owner rather than silently rewritten: a credential store is theirs to change. Gemini is now a
358-char DPAPI blob, and it was verified by calling `CryptUnprotectData` directly — the exact Win32
entry point `dpapi.rs` uses — rather than trusting .NET's wrapper to be format-compatible with it.

Gates: typecheck 431 files 0 errors, eslint clean, 236 frontend tests, python policies green. The fix
is source-only so far — rebuilding restarts the app, and the couch server lives in that process, so it
was deliberately deferred rather than dropped under two people mid-review. Reviewer page is served by
Rust from `couch.html` and is unaffected by the frontend bundle either way.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 267 — the review set was drafted by the wrong engine, and nothing said so

**The owner found this by reading the transcripts, not from any gate.** He asked why they looked poor.
The DB answered: every one of the 494 rows was `model_version_id = finetuned-mms-ckb`,
`confidence_source = fine_tuned_no_posterior`, `cloud_call = 0`. No 7B pass and no LLM refinement had
run at all, while the 7B server sat up and idle on both GPUs and `asr_model_size` said `WSL7B`.

`use_finetuned_asr = true` overrides the configured engine by design (pipeline.rs), and the Gemini
refiner returned None because the API key had been deleted by the settings-scrub bug fixed the same
day. Two independent silent downgrades stacked on one queue. Iteration 265's claim that the queue was
"all from the champion 7B" was written without checking and is corrected in place above.

Measured on identical FLEURS ckb clips (docs/MEASUREMENTS.md): 7B champion 7.03% CER [6.53, 7.55]
N=922 vs fine-tuned MMS-CTC-1B 9.32% — and that 9.32% is the fp32 reference while the app runs the
int8 build, whose own recorded baseline is 21.00%.

**The champion refused every .mp4, so the fix half-worked and hid it.** Re-running on the 7B left
462 clips converted and 25 still on the weaker engine:

    Error opening '.../A1-0032_PODCAST-001.mp4': Format not recognised

libsndfile — torchaudio's soundfile backend — handles no MPEG-4/AAC, while the app's own import path
decodes it fine. So those clips imported, chunked and transcribed locally, then silently could not be
re-transcribed by the champion. A review set at two different quality levels, with nothing on screen
saying which clip came from where, is exactly what corrupts a measurement later. ffmpeg fallback
added (already a hard import-path dependency); 462 -> 487 on the real audio that had failed.

**Serial-by-default cost ~8 hours and left both GPUs idle.** 22.2 s in the 7B and 52.1 s in
refinement per clip, one clip at a time, with the cards at 18% and 9%. Two fixes, each defaulting to
the old behaviour and opted into by env:

    serial                       74.0 s/clip   ~8 h     GPUs 18% / 9%
    + concurrent batch           23.1 s/clip   2.7 h    GPUs 19% / 12%
    + WSL_7B_GATE given permits  13.7 s/clip   89 min   GPUs 25% / 39%

The second was a STALE COMMENT: the gate admitted one request because "the champion server is a
single-threaded accept loop", which stopped being true when the server started pre-forking one replica
per GPU. The bound is still needed (surplus requests queue and burn their client timeouts, which reads
as "server down" and rolls back a healthy import), so it is a counting semaphore now, not no gate.

**Final queue state, verified:** 487 on `omniasr-wsl-7b`, 482 Gemini-refined, 7 rows left on the old
engine because a human had already corrected them (`update_asr_transcript_if_unreviewed` refuses those,
and they were excluded from the batch as well — human text is gold, no model output overwrites it).
0 blank transcripts, 494/494 speaker IDs, gold 348 and eval 696 preserved.

**Two things measured and NOT fixed, reported instead.** Under `jury_autonomy_level = propose` the
jury runs but leaves an already-staged row's verdict and IRT confidence untouched by design — so after
a re-transcription those scores still describe the previous text, and they drive riskiest-first review
ordering. And 59 of 487 refinements failed with "Invalid response format from Local LLM": with an
OpenRouter key present, `LlmMode::Gemini` routes through OpenRouter's OpenAI-compatible endpoint, so
the label is wrong and ~12% of clips kept raw 7B text. Neither is data loss; both are the owner's call.

**`branch-protection` is no longer owner-gated.** It was item 49, "repo-admin clicks", whose only
evidence was that somebody said they had clicked — and nothing would have noticed protection being
weakened later. It is an API call: 4 required contexts, strict, enforce_admins, linear history, no
force pushes, no deletions, all verified against the live remote every sweep. OWNER_GATED 5 -> 4.

**The kappa gate was blocked on tooling, not only on annotators.** `agreement_kappa.py` reads a TSV of
two rater columns and nothing produced one, so item 44 required hand-assembling a file out of SQLite.
`spot_checks` already holds one row per (segment, reviewer) because Couch Review serves spot checks
unleased on purpose. The extractor refuses to emit anything misleading: zero overlap (guarded twice,
independent of the floor), below --min-items, or more than two raters pooled into two columns.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 268 — the champion is no longer optional, and failure is no longer survivable

**The owner's rule, in code and in law.** After finding the queue drafted 494/494 by the smaller model
while the champion idled, the owner ruled: use the 7B champion every time, never fall back to a
smaller model, and HARD STOP on any failure. Written into CLAUDE.md and enforced in three places.

`should_use_wsl_primary_asr` no longer consults `use_finetuned_asr` — selecting WSL7B outranks the
flag. `batch_transcribe` cancels on the first failure and emits `type: "halted"` with the cause,
never `"completed"`. And a failed refinement is now a failure: both paths used to log "Falling back to
raw transcript" and store the unrefined draft as finished, which is exactly how 59 of 487 clips
silently kept raw text.

**The hard stop proved itself before it was ever tested on purpose.** Its first act was to halt a
487-clip run at clip 1 because the champion server was down — 0 succeeded, nothing written. Under the
previous code that run would have produced 487 inferior transcripts and reported success. It also
exposed that the batch accepted the job before preflighting, so the preflight moved ahead of
`try_start_batch`.

**Retry, because strict must not mean brittle.** With the hard stop in place, a single throttled reply
would halt a whole run. There was no retry anywhere — `API_AGENT` is a bare ureq agent — and the 59
failures were ~12% of a run using 8 concurrent workers, the signature of provider rate limiting
introduced by our own concurrency change. Now 4 attempts with 1s/2s/4s backoff for TRANSIENT classes
only; a 401 or an unknown model still stops immediately. The permanent checks run first so a 401 that
mentions rate limiting in prose stays permanent.

**Re-run under the rules, and it held:** 487/487 champion-drafted, 487/487 Gemini-refined,
**72 transient failures absorbed and zero silent degradations**. 0 blank, 494/494 speaker IDs, the 7
human-corrected rows untouched, gold 348 and eval 696 preserved. Jury verdicts refreshed
(2026-08-11 10:56) so they no longer describe deleted text, then autonomy returned to `propose` so
every clip stays in the human queue.

**First real dialect measurement — and it is RED.** CORDI (CC BY-SA 4.0, 3.94 GiB, sha256
`ff0ea0cb…`, 186,126 clips) scored through `scorecard_7b.py`, 60 clips per variety:

    Mehabad  27.63% [22.56, 33.38]    Sine     36.31% [31.00, 43.65]
    Silemani 29.81% [23.12, 37.79]    Hewler   39.19% [33.50, 46.30]
    Kalar    31.24% [25.83, 38.17]    Serdest  42.42% [36.71, 48.95]

max-min disparity **14.79 pts** against a 10.00 pt budget. Not noise: Mehabad and Serdest have
non-overlapping CIs. Two artifacts ruled out rather than assumed — `text == text_original` for all
12,683 utterances checked (so the references are the dialectal transcriptions), and the absolute CERs
are NOT comparable to the 7.03% FLEURS headline because CORDI is spontaneous film dialogue with music
and overlap. The disparity is the within-domain comparison and it is fair. Recorded as a measurement;
whether it becomes a blocking gate is the owner's call, because raising `budget_pts` to make it pass
would be weakening a gate to fit the data.

**Two owner-gated legs retired, on evidence.** `branch-protection` became a real gate (4 required
contexts, admins, linear history, no force-push, verified against the live remote; 9/9 weakenings
caught). `asosoft-600-licensing` was descoped: neither the site nor the GitHub repo carries a LICENSE
file, terms text, or a contact address — there was nothing to verify and nobody to ask. OWNER_GATED
5 → 3.

**Failures found by the sweep, all fixed.** `RUSTSEC-2026-0253` (unsound `LruCache::pop()`) published
today against our direct `lru 0.16.4`; bumped to 0.18 and `advisories ok`. Two real-model tests
asserted that `use_finetuned_asr` diverts the champion — the behaviour the owner just banned — so they
now select a non-champion engine, and a new test pins the refusal itself.

**Mistakes worth recording.** A fail-before harness restored `pipeline.rs` with the wrong line endings
(content intact, verified byte-wise). A watcher grepped the whole log and re-reported an old
HARD-STOPPED entry as a fresh alarm — a false alarm is as damaging as a missed one. `2>&1` on cargo
under PowerShell 5.1 turned build output into a terminating error and left the app stopped mid-session.
And the CORDI manifest builder emitted RELATIVE paths that resolve to nothing inside WSL: the scorecard
would have scored an empty set and still printed a per-dialect CER — a fabricated accuracy number
produced by a path bug. It now refuses any path that did not become `/mnt/...`.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 269 — the reviewers were served text no engine had just produced, and no gate looked

**The owner caught it, again, by reading transcripts.** Words changed, words "corrected," words that
read like translations. Measured cause: `review_text()` in couch.rs serves `annotated_transcript` —
the field reserved for a HUMAN's typed correction — before the raw champion draft. An old code path
(the one `batch_processor.rs:177-182` documents banning, because it graded pure ASR output as
human-verified GOLD) had left machine text in that field on **348 of 494** rows. The champion re-draft
of 2026-08-11 wrote `raw_transcript` and `normalized_transcript` — fields the couch page never shows
when `annotated_transcript` is set. So the fresh champion + refinement work sat invisible while the
phone served a stale machine paraphrase, and iteration 268's "reviewers get champion quality" claim
was FALSE at the serving path while true at the write path. Provenance check on the served text of
the 348 untouched rows: 45 matched the champion draft, 22 the Gemini refinement, **281 matched
neither** — text from a superseded refinement pass, heh-form fingerprint (747× U+0647, 1× U+06BE)
matching neither current engine's output.

**Also measured while diagnosing: the Gemini refinement REWRITES, it does not correct.** Against the
champion on 492 refined clips: median 11.1% of characters changed, p90 30.3%, max 60%. Concrete:
`مەعاشی سابت` → `مووچەیەکی سابتیان` (the spoken loanword replaced by the "proper" word — a
translation), digits `2000` spelled out as `دوو ھەزار`, punctuation 9 → 892 marks, every ه → ھ. For
a VERBATIM ASR corpus that is fabrication with fluent grammar — the most dangerous kind, because a
skimming reviewer accepts it.

**Fix, executed live with reviewers quiet 44 min (leases 15-min, all expired), no restart, links
intact.** WAL-safe `sqlite3 .backup` to `cortex-speech.db.bak-annotated-clear-20260812`, then one
targeted UPDATE clearing `annotated_transcript` on rows with no review event, no human_decision,
verified=0: **348 rows cleared**. Human work byte-identical before/after: 145 decisions, 145
verified, 189 review events, 130 corrections. Serving-path re-verification: 349 unverified clips in
queue → 348 serve champion-verbatim (raw == stored `omniasr-wsl-7b` hypothesis), 1 serves
human-touched text, 0 unexplained.

**The permanent gate — verified fail-before on the live data.**
`cortex-speech-app/scripts/check_review_serving_provenance.py`, registered as verify-10 tier-1 leg
`review-serving-provenance`. Two invariants at the serving path's own precedence: (1)
`annotated_transcript` non-empty ⇒ human evidence on that row (review event, human_decision, or
verified); (2) every untouched row's `raw_transcript` byte-matches its stored champion hypothesis —
a missing hypothesis FAILS (hard-stop law: an undrafted clip may not look drafted). Run BEFORE the
fix: `FAIL [human-field]: 348 rows`, exit 1. After: both invariants PASS, exit 0. Deliberately named
`check_*`, not `test_*` — the policy-suite namespace is sandbox-safe static checks, and a live-DB
gate there would red the suite off-rig (the vacuous-skip alternative is worse).

**Law added to CLAUDE.md honesty section:** verify at the SERVING path, never the write path. Three
incidents now share this shape (494/494 wrong engine; 25 silently-degraded clips; this). A
nine-path adversarially-verified audit of every transcript consumer/writer (couch serve, decision
handlers, desktop UI, Rust + script exports, grading, jury writers, pipeline writers, precedence
map) is running; findings land next iteration.

**Still open, owner's call:** (a) the 45 `accept` decisions predating the fix approved the stale
text — re-queue or keep; (b) the refinement-as-rewriter finding argues `normalized_transcript`
should be jury evidence only, never reviewer-facing or export-facing text for a verbatim corpus —
that is a policy change beyond this fix.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 270 — the writer was not dead, and iteration 269 said it was

**Correction of the record.** Iteration 269 attributed the 348 poisoned rows to "an old code path …
never cleaned from the data." A nine-path adversarial audit (42 agents, every claimed defect
independently verified against the real code; 27 confirmed, 6 refuted —
docs/TEXT_PROVENANCE_AUDIT_2026-08-12.md) proved the writer was LIVE:
`db.rs::update_batch_transcription_if_unreviewed` seeded `annotated_transcript` with the machine
draft via `COALESCE(annotated_transcript, seed)` on every batch run — `db_tests.rs` even pinned the
seeding as intended ("annotation seeded when empty"). The very next batch_transcribe would have
re-poisoned all 348 cleaned rows. Five desktop handlers (App.svelte champion/constrained/
finetuned/Scribe re-transcribe, ReviewMode doRetranscribe) wrote machine `result.text` into the
same human-only field.

**Fixed, fail-before verified.** New sandbox-safe policy gate
`scripts/test_machine_never_writes_annotated_policy.py`: function-scoped so the LEGAL human writers
(submit/draft persisting editText) stay legal — pre-fix it flagged exactly the 6 machine sites with
0 false positives; post-fix 0 violations. The batch persist path no longer mentions the annotated
column; the five handlers write raw/normalized only, and NULL `normalized_transcript` unless
recomputed so stale text cannot outrank the fresh draft at the `annotated ?? normalized ?? raw`
precedence. The Rust pin flipped and was renamed for what it now proves
(`…_and_never_writes_annotation`); first run of the renamed filter reported "0 tests ran, 1171
filtered out" — a vacuous pass caught by reading the count line, per the standing rule.

**Gates actually run:** `test_machine_never_writes_annotated_policy.py` FAIL(6)→PASS,
`check_review_serving_provenance.py` PASS, full python-policies 60/60, vitest 238/238, typecheck 0
errors, `cargo test --lib batch_transcription_update` 1 passed (scratch target dir; the running app
was never stopped). NOT run this iteration: clippy, full cargo test, the full verify-10 sweep — the
Rust/Svelte fixes are committed but NOT DEPLOYED; the running exe still carries the seeding writer
until the next rebuild+restart window, so batch_transcribe must not be run before then.

**Audit remainder:** 20 confirmed defects remain open, grouped in
docs/TEXT_PROVENANCE_AUDIT_2026-08-12.md — the largest cluster is `verified=1` alone fabricating
human provenance (bulk Verify All Pending exports machine drafts as human gold; Curate ^D can bless
an unseen jury verdict; the spot-check answer key trusts machine-seeded text). Those are grading
semantics redesigns and owner decisions, not quick patches.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 271 — gold requires a human decision, and the transcript is verbatim everywhere

**Two owner rules landed together (owner, 2026-08-12: "we wanna be sure we get the exact
transcription of the audio and not get corrected/translated/changed by other models. this is
training dataset for ai should be exact audio to text").**

**Rule 1 — gold provenance.** `verified`/`is_gold` alone never mint human provenance again. The
single source of truth is `training_transcript_with_source`: "human_verified" exists iff a real
human decision (accept/edit/human_*) produced the text — the text captured at decision time, else
the typed annotation, else the raw draft the accept was made against. `training_grade_for_segment`
now derives `human_verified` FROM that source label, so grade and label cannot drift. Kills audit
defects #7 (^D blessing an unseen jury verdict), #8 (bulk Verify-All exporting machine drafts as
human gold), #15, #10, #1 (spot-check keys: `human_verified_text` now refuses flag-only rows —
NOTE: the audit's suggested `reviewed_by`-based SQL fix was WRONG and was reverted after the pin
test showed desktop decisions legitimately carry `reviewed_by = NULL`; the key tightening lives in
the quality layer where all callers route). #22 fixed at the shared root: an accept arriving with
no text now snapshots what the serving law showed (annotated ▸ raw) instead of COALESCE-keeping a
machine jury proposal as the human's answer. Measured data impact on the live db: exactly 6
verified-no-decision rows demote to REVIEW; all 139 decided-with-text rows keep gold.

**Rule 2 — the VERBATIM LAW.** The transcript every consumer serves, exports, grades, aligns, or
displays is: human-decided verdict ▸ annotated ▸ champion raw — never `normalized_transcript` (the
LLM refinement, measured rewriting a median 11.1% of characters: loanwords translated, digits
verbalized) and never an undecided machine jury verdict. Changed in one sweep, mirrors kept
byte-consistent: `quality::training_transcript_with_source`, `stats.rs::EFFECTIVE` (SQL mirror),
`scripts/retrain_readiness.py` (script mirror), `corrections::loop0_draft_text` (aligner/LOOP-0 —
timings are no longer computed against words the speaker never said), `jury/learning.rs` LM-corpus
SQL, frontend `segmentQuality.effectiveTranscript`, ReviewMode `originalText`, App card list,
DiffView base, and the desktop align-after-transcribe text. SILVER semantics tightened for free:
machine-consensus rows are training-ready only when the jury/t2 evidence certifies the RAW text
the export now actually ships (`evidence_transcript_matches` against raw). Refined text remains
stored as jury EVIDENCE only.

**Gates, all fail-before or count-verified:** new `test_verbatim_training_text_policy.py`
(11 violations pre-fix → 0; function-scoped, with the `?? ''` labeled-column exemption). Full
suites after the pin flips: **cargo --lib 1164 passed / 0 failed** (37 → 23 → 4 → 1 → 0 across the
fixture surgery — the export/eval fixtures minted gold via the bare flag, exactly the fabrication
the law bans, and `insert_segment` silently DROPS `human_decision`, so fixtures switched to
`insert_segment_full`), clippy --all-targets clean, vitest 238/238, typecheck 0 errors, python
policies **61/61** (both mirror scripts re-copied per their own instructions). Two vacuous-pass
traps caught during the work: a `| tail` pipe masking cargo's exit code (37 failures read as
EXIT=0), and a renamed test filter matching zero tests ("0 passed" read as green).

**Deployment:** committed and REBUILT this window (reviewers paused by the owner); the running app
now enforces both laws. The refinement setting stays available but its output can no longer reach
a reviewer, an export, or an aligner.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 272 — the brutal pass: every reachable audit defect closed before the reviewers return

**Owner ask:** one last brutal reality check for a true 10/10 before recalling the reviewers. A
10/10 claim cannot stand over open confirmed defects, so this window closes every remaining
text-provenance-audit finding reachable from the review pipeline:

- **#2/#3 (undo fence):** undo now refuses unless the row still carries the exact `updated_at`
  fingerprint captured when the decision's final write landed — `reviewed_by` alone let every
  non-decision writer (batch normalize, refine loop, desktop edit, champion re-draft) be silently
  reverted. New `UndoEntry` carries the stamp; `db.segment_row_stamp` is the fingerprint reader.
- **#4 (serve/decide race):** the queue stamps each clip with `rowVersion` (the row's `updated_at`
  at serve time), couch.html echoes it on every decision, and a decision against a replaced draft
  is refused 409 — never recorded, never minted into a DPO pair. Replays finishing an
  already-recorded decision are exempt (their write one legitimately moved the stamp); a page from
  before this deploy sends nothing and is fenced by the existing verified/lease guards only.
- **#17/#18 (jury):** `write_verdict` gains the `AND verified = 0` guard its siblings had, and
  writes `jury_transcript` for parity with `db::write_segment_verdict` — the machine's own proposal
  now survives a later human decision on both machine-verdict writers.
- **#24 (consensus):** `update_segment_consensus_batch` gains the missing `AND verified = 0`.
- **#11/#12 (export):** rejected + placeholder rows are dropped in `exclude_unexportable_segments`
  — the shared root, per that function's own "a rule enforced per-caller gets missed by the sixth
  caller" doctrine — so export_audio can no longer ship them. The finetune-pack counter that
  silently absorbed the new drops was renamed `excluded_unexportable` end to end (Rust field, JSON
  key, TS type, dashboard) rather than left lying as "holdout".
- **#20 (champion law in tooling):** `batch_processor` HARD-STOPS at startup when the configured
  primary engine is the WSL 7B champion — it drafts with the offline CTC engine and would have
  silently downgraded the whole queue.

**Proof:** mutation fail-before on BOTH couch fences (fences neutered with `if false &&` → both new
tests FAIL → file restored byte-identical, sha256 2ad04b9624bd1c43 before and after). Five new
regression tests, each discriminating by construction (0-changed/None-verdict asserts cannot pass
against the old code). Full suites: **cargo --lib 1169 passed / 0 failed**, clippy --all-targets
clean (exit captured directly this time, not through a pipe), vitest 238/238, typecheck 0 errors,
python policies 61/61.

**Remaining open, disclosed:** #21 (desktop `update_segment` whole-row upsert has no freshness
check — desktop-only, the owner's own surface, same-second granularity caveat applies) and #13/#14
collapsed to informational (the scripts trust `transcriptSource`, which since iteration 271 is
minted only under the gold-provenance law). Nothing else from the 27 confirmed findings remains
reachable.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 273 — the full sweep on the deployed app: 34/34 GREEN

`python scripts/verify_10.py` on the deployed exe (HEAD 965aacb), machine idle, reviewers paused:
**kept gates run: 34 — 34 PASS, 0 FAIL, 0 skipped.** Includes both new provenance gates
(review-serving-provenance on the LIVE db, machine-never-writes-annotated + verbatim-training-text
in the policy suite), the real-exe e2e legs, egress-runtime, fuzz (933.9s), bench-budget (1511.5s),
and both crash drills. Verdict line verbatim: "GREEN - PERSONAL-USE SHIP-READY. (Not full-charter
10/10: 9 legs owner-descoped, 3 owner-gated pending.)" The 9 descoped legs are the distribution
items the owner ruled out of "ship" on 2026-07-10; the 3 owner-gated legs (iaa-kappa-ceiling,
cordi-dialect-fairness, refinery-lift-in-product) are the ones that need HUMANS reviewing — the
reviewers returning is itself the path to them. Log:
%LOCALAPPDATA%\Temp\cortex-verify10\runs.jsonl + the session sweep log.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 274 — blueprint step 1 shipped; step 2 blocked on billing, and the attempt found a live defect

**Step 1 (done): the verbatim conventions.** `docs/SORANI_VERBATIM_CONVENTIONS_v1.md` — a versioned
Sorani style guide covering the rules reviewers were previously left to guess: repetitions/stutters
kept, fillers kept, LOANWORDS WRITTEN AS SPOKEN (مەعاش stays مەعاش, never "corrected" to مووچە —
the exact substitution the Gemini refiner made), numbers as spoken, no invented punctuation, no
grammar fixes, dialect preserved, reject-don't-guess. Research basis: transcription policy left
implicit makes up to 60% of measured error style-mismatch noise (arXiv 2607.18934). Includes the
20-clip calibration protocol and states the guide changes when a reviewer is right.

**Step 2 (BLOCKED, honestly): Gemini-as-ASR benchmark.** Built
`src-tauri/src/bin/gemini_asr_bench.rs` — transcribes the frozen FLEURS ckb manifest (922 clips,
sha256 c040242390ba… verified) and writes ONLY a hypothesis TSV; scoring stays in the existing
scorecard so the champion's 7.03% and Gemini's number share one normalization + seed-42 bootstrap by
construction, not by hope. Pilot run: **0 of 12 transcribed — HTTP 429 "Your prepayment credits are
depleted."** The owner's Gemini key has no credits. No number was produced and none is claimed;
the benchmark is ready to run the moment billing is restored.

**Defect the failed pilot exposed (fixed):** `llm_refiner::is_transient` treated EVERY 429 as
retryable, including billing/quota exhaustion — which no backoff can fix. In a live batch that is
4 attempts and 7 s of wasted backoff PER CLIP, and the run then reports a retry exhaustion instead
of "you are out of credits." Billing/quota 429s are now permanent (checked before the 429 rule, like
the other permanent classes), mirrored in the bench binary as a hard stop. Fail-before verified: the
guard removed → `transient_errors_retry_and_permanent_ones_stop_immediately` FAILS on the real
provider string; restored byte-identical (sha 9ccabe81aff4474b before and after) → passes.

**Privacy gate caught a shortcut, and the fix strengthened it.** The bench binary first reached for
`gemini_api`'s `pub(crate)` helpers by widening them to `pub`;
`test_cloud_privacy_policy.py` pins that declaration and went red. Widening lib surface so a
benchmark could hand-assemble a cloud request was the wrong answer: the audio call now lives INSIDE
the audited module as `gemini_api::transcribe_audio` (key in the `x-goog-api-key` header, never the
URL; provider errors redacted), the binary is a thin driver, and the gate gained two assertions
covering the new audio path. A gate going red is a gate working.

**Verified:** cargo --lib 1169/0, clippy --all-targets clean, python policies 61/61.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 275 — the listening-QC was dead across 283 decisions

**Hunt result (loop iteration 1).** The hidden spot-check mechanism — the ONLY instrument that
measures whether a reviewer is listening rather than tapping accept — has been structurally
incapable of firing. Measured on the live corpus: **0 answer keys against 283 human decisions and 4
active reviewers.**

Cause, and it is by design colliding with practice: `list_spot_check_candidates` mints keys only
from `is_gold = 1 OR reviewed_by IS NULL`. The `reviewed_by IS NULL` arm exists so a peer's fresh
correction never becomes the key another reviewer is graded against — correct. But every decision in
this corpus arrived from the phone, which stamps `reviewed_by` unconditionally, and nothing is
flagged `is_gold`. So the population is empty by construction. Nothing errored, no counter moved,
and `spot_checks` still holds 5 rows from an earlier era — the QC LOOKED alive. Same shape as the
vacuous-gate class this repo keeps finding, and it silently spanned every decision made to date.

NOT caused by the gold-provenance law (iteration 271): the clause predates it. An earlier attempt to
widen it to `reviewed_by <> requesting_reviewer` was reverted when the pin test showed that would
promote peer corrections to answer keys — the right call, and it left this defect standing.

**New gate `scripts/check_spot_check_pool.py`** (verify-10 tier-1 leg `spot-check-pool`): mirrors the
candidate query + `human_verified_text` + the wrong-draft requirement, and FAILS on an empty or
too-thin pool. Currently RED by design — exit 1, "ZERO answer keys". It is red because the system is
wrong, not because the gate is.

**The fix is an owner action, not code:** adjudicate a batch at the DESKTOP (which leaves
`reviewed_by` NULL) or flag adjudicated clips `is_gold = 1`, so keys exist. This is the same
mechanism the blueprint's double-pass + adjudication stage would replenish continuously.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 276 — MEASURED: the champion drafts alone; Gemini fusion is not a win

The owner's question ("champion + Gemini, then human — true?") now has a number instead of an
opinion. All engines scored by ONE code path (`scorecard_gemini.py` imports `scorecard_7b`'s `_NORM`,
`edit_distance` and the seed-42 bootstrap), paired on the IDENTICAL 890 clips.

| draft | CER | 95% CI | WER |
|---|---|---|---|
| **champion alone** | **7.02%** | [6.45, 7.65] | 32.70% |
| Gemini 2.5 Pro alone | 9.58% | [8.83, 10.37] | 33.43% |
| fusion (Gemini overrides on disagreement) | 7.32% | [6.75, 7.94] | 31.70% |
| fusion (ROVER, ties keep champion) | 7.02% | — | 32.70% (0 clips changed) |

MAPSSWE matched-pairs, N=890: champion beats Gemini on CER at **p=2.4e-30**. Champion beats the
fused draft on CER at p=5.4e-4 — while the fused draft beats the champion on WER at p=2.5e-4. Real
trade, both directions significant: Gemini recovers whole words the champion mangles, but when it is
wrong it is wrong by more characters.

**Decision: the champion drafts alone.** CER is the metric this project publishes and the basis of
the 7.03% claim; adopting fusion would move the headline the wrong way for a 3%-relative WER gain,
while adding a cloud dependency to every clip. Practical seal: gemini-2.5-pro is capped at **1000
requests/day** (the run hard-stopped at 890/922 on that quota), so it could never draft a corpus of
this size regardless of quality.

Honest scope: measured on FLEURS = READ speech; the ranking on spontaneous dialectal audio is
untested. Only two engines voted, so ROVER had no tiebreaker (0 changes by construction). Gemini's
number is under this project's verbatim prompt, which is the task that matters here.

Three traps the harness caught rather than swallowed: (a) the champion ran in WSL and Gemini on
Windows, so joining on raw paths gave ZERO overlap — the zero-overlap guard refused instead of
scoring an empty set; (b) an early 27-clip read showed Gemini at 7.78%, which the full 890 corrected
to 9.58% — small-n optimism, flagged at the time; (c) the daily-quota 429 hard-stopped instead of
retrying, per the fix in iteration 274.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 277 — the disagreement display, a real model-resolution bug, and a corrected claim

**Measured, correcting myself.** I had been quoting the local acoustic engines as "3-4x worse" than
the champion from stale figures (21% / 29.4% — different models). Measured on the frozen gold set
through the shared scorer: **omniasr-ctc-300m = 11.65% CER [11.07, 12.28]**, n=921. That is 1.66x the
champion's 7.02%, not 4x — the same order as Gemini's 1.36x, which makes multi-engine fusion a live
question again rather than a foregone conclusion. CTC-1B is running.

**A real user-facing bug the measurement exposed.** CTC-1B failed on ALL 922 clips with "ASR service
unavailable (models missing?)" — while its 1 GB copy sat in the bundled models dir. Cause:
`pipeline.rs` loaded the ASR pool through `model_manager.resolved_dir()`, which flips all-or-nothing
between the user root and the bundled root. `%APPDATA%/cortex-speech/models/omniasr-ctc-1b/` exists
but is EMPTY, so the user root won — and the engine offered in the UI could not load. This is the
round-26 class the denoiser/aligner/campp were already fixed for; the ASR engines were the one place
still using the all-or-nothing root, and the availability PROBE had it twice over (one root asked
about several engines silently downgrades a bundled-only engine). Now `root_for_size` /
`size_present` resolve PER ENGINE. Pinned in `test_rust_runtime_panic_policy.py` beside the existing
per-file assertions; fail-before verified (reverted one call site -> gate FAILS naming it; restored
byte-identical, sha cec14701ca5d22ea).

`wsl_without_script_uses_local_asr_fallback` then failed — because it PREDICTED the expectation using
the buggy `resolved_dir()`. The test had encoded the defect as truth; it now probes per-file like
production.

**The disagreement display (owner-requested).** The couch queue now carries `unsupportedWords`: the
served draft's word positions that NO other engine produced anywhere in the clip. The phone lists
them under the clip ("⚠ بە وردی گوێ بگرە لە: …"). This exists because the champion is an LLM-based
ASR and was measured writing the standard form where all three acoustic engines agreed on the
pronounced one (سێری -> سەیری, خۆدە -> خۆت) — fluent, plausible, and therefore invisible to a
skimming reviewer. It LISTS words and never replaces them; the champion cannot vouch for itself
(its own hypothesis is excluded from support, mutation-verified: with self-support the flag goes
silent exactly when it matters); and a clip with no stored hypotheses flags nothing rather than
everything. New Sorani string acknowledged in the i18n gate — awaiting the owner's native read.

**Concurrency hazard, recorded.** Mid-iteration the lib test count dropped 1177 -> 1161. Not my
change: ANOTHER session removed two dead modules (`align_text.rs`, `perf/mod.rs`) together with
their `pub mod` declarations — 16 tests, a coherent refactor. Their deletion is left UNCOMMITTED and
untouched; this commit stages only its own files by name. `git add -A` here would have shipped
another session's refactor under this message.

**Verified:** cargo --lib 1154 passed / 0 failed (1161 total, the reduced count explained above),
python policies 62/62.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 278 — the benchmark lied about the domain; the display was measured and REVERTED

**The FLEURS number does not transfer, and that reverses two conclusions.** Every engine scored
through the shared scorer on 889 clips both ways — the frozen FLEURS gold set (READ speech) and the
owner's OWN reviewed clips (spontaneous dialectal, verbatim human answers):

| engine | FLEURS CER | owner's edited clips |
|---|---|---|
| champion (7B) | 6.95% | **10.59%** |
| omniasr-ctc-1b | 8.13% | **39.99%** |
| omniasr-ctc-300m | 11.59% | 23.42% |
| finetuned-mms-ckb | — | 19.05% |
| gemini-2.5-pro | 9.50% | (no stored per-clip hypotheses) |

CTC-1B looks like an 8% engine on read speech and is a **40%** engine on real dialectal audio — a 5x
collapse. FLEURS references were WRITTEN first and then read aloud, so they are standard orthography
by construction: a benchmark that rewards standardizing cannot evaluate a verbatim corpus.

Consequence 1: **4-engine ROVER fusion beat the champion on FLEURS** — 6.57% vs 6.95% CER
(MAPSSWE p=2.7e-28) and 30.32% vs 32.63% WER (p=5.9e-57), closure-verified, a genuine result. On the
owner's domain the same fusion changed **0 of 135 clips**, because any weight large enough to let a
20-40% engine outvote a 10% one imports its errors. **The champion drafts alone** — now on
domain-matched evidence rather than the earlier guess.

Consequence 2: **the disagreement display was built, measured, and reverted.** Against the owner's
own corrections (179 clips, 4458 champion words, 20.6% base rate of words the human changed):

| flagging rule | words flagged | precision | lift |
|---|---|---|---|
| no engine produced it (as built) | 36.6% | 32.1% | 1.56x |
| no STRONG engine produced it | 40.2% | 31.2% | 1.52x |
| >=2 others agree on a replacement | 2.6% | 37.6% | 1.83x |
| ALL others agree on a replacement | 0.6% | 46.2% | 2.24x |

The best rule is a coin flip firing once per seven clips; the shipped rule flags a THIRD of every
clip and is wrong two times in three. A warning that cries wolf trains reviewers to ignore it,
including when it is right — so this is worse than nothing and it does not ship. Reverted in full
(couch.rs, couch.html, the i18n acknowledgement). The champion's standardization tendency is real
and still unaddressed; these engines are simply too weak on this domain to detect it.

Honest bias note on the owner-domain numbers: the references are the reviewers' corrections, typed
while looking at the CHAMPION's draft, so the champion is flattered by anchoring — the draft-free
slice the blueprint calls for is what would remove it. Accept rows were excluded for exactly this
reason (the champion's own text as its own reference scores 0 by construction).

**Kept from this line of work:** the per-engine model resolution fix (real user-facing bug), the
`local_asr_dump` harness, and four engines now measured on both domains.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 279 — Review UI: audio against the text box, one section fewer

**Owner feedback while reviewing:** the waveform and play button sat far above the correction area,
and three text sections competed for the screen.

**Reordered.** The audio is now ONE card (waveform + playback-scope hint + transport) sitting
directly above the editor, so listening and correcting no longer require scrolling between them.
New order: audio -> transcript editor -> listen strip -> fix-the-draft tools. Previously the editor
sat below the waveform, the hint, the player, the listen strip AND the consensus card — roughly
600px down, on a laptop screen usually out of view while the play button was in view.

**One section removed, on evidence.** Of the three text sections, the CONSENSUS DRAFT card is gone.
It rendered an ability-weighted vote across this clip's ASR hypotheses — precisely the fusion
measured in iteration 278 as changing 0 of 135 clips on the owner's own audio, built from engines at
19-40% CER against a champion at 10.6%. Its "Use draft" button therefore replaced the champion's
text with a measurably worse blend: not merely clutter, a one-tap downgrade. The now-dead
`CONTESTED` threshold and `SegmentConsensus` type import went with it; `loadConsensus` is KEPT
because the same call feeds the honest "drafted by <engine>" provenance badge.

**The listen strip STAYS**, moved below the editor. Tapping a word to hear exactly that word is the
core verbatim interaction and the reviewer's only tool for the champion-standardization case the
owner caught by ear (سێری vs سەیری) — the disagreement display that was supposed to help with that
measured as noise and was reverted, which makes the ear the remaining instrument.

**Verified:** typecheck 0 errors, eslint clean, vitest 232/232. NOT yet visible to anyone: the
frontend is bundled into the exe, so this lands on the next rebuild — deliberately deferred while
the owner is mid-review rather than interrupting a session to ship a layout change.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 280 — the 348-row clear left 36 laundered accepts, and the gate could not see them

**The sweep was GREEN and the data was not.** The full 35-gate `verify-10` finished GREEN
(35/35 PASS, 0 FAIL, VERDICT: GREEN — PERSONAL-USE SHIP-READY) on the same live database that was,
at that moment, holding 36 rows of LLM-refiner text labeled `human_verified`. Both statements are
true, which is the finding: no gate covered this class until this iteration.

**Measured, on the live DB (read-only), 2026-08-13.** Of 180 `accept` decisions, **36 freeze text
that matches NO ASR hypothesis on file** — not the champion `omniasr-wsl-7b`, not
`finetuned-mms-ckb`, not `omniasr-ctc-300m`, not `omniasr-ctc-1b`. All 36 are pre-fix (before the
2026-08-12 11:54 clear); 33 by Reviewer A, 3 by Reviewer G. Character divergence from the champion's own
transcript: median 5.2%, max 14.2%. Split by era, the control is clean: **0 of 31 post-fix accepts**
diverge, which is what proves the accept path copies the served text verbatim — so a diverging
accept means the text served was never the champion's.

Sample (id `3330566c`, Reviewer A, 2026-08-11):

* champion RAW: `خێزانەکەمان بێ بە کۆمەڵگاکە بێ بە مامۆستایانی ئاینی بێت کە...`
* APPROVED:     `خێزانەکەمان، کۆمەڵگاکە، یان مامۆستایانی ئایینی، کە...`

Inserted commas, `بێ بە` → `یان`, `ئاینی` → `ئایینی`. Elsewhere `زیای ناکردووە` → `زیادی نەکردبوو`
— repaired grammar. This is refiner output, and under the verbatim law it is now the training text.

**Why the existing gate passed it.** Invariant 1 says a populated `annotated_transcript` requires
human EVIDENCE on the row, and accepts a recorded `human_decision` as that evidence. A decision is
evidence a human clicked a button. It is not evidence a human authored the words. Pre-fix, the
machine wrote the paraphrase into the human-only field, the reviewer was shown it, clicked accept —
and the click retroactively made the row look legitimately human. The 2026-08-12 clear removed the
machine text only from rows that had NOT yet been decided; every row a reviewer had already accepted
kept it, permanently, in `verdict_transcript`.

**Fixed: invariant 3 in `check_review_serving_provenance.py` (verify-10 gate
`review-serving-provenance`).** An accept may only freeze text some ASR engine actually produced —
the stored text, read at the export's own precedence (`verdict ▸ annotated ▸ raw`, mirroring
`quality::training_transcript_with_source`), must match one of that segment's own hypotheses.
Whitespace-normalized so spacing is not a finding. Edits are deliberately exempt: an edit is a human
typing, and its text is expected to match no engine.

**Fail-before / pass-after, both run:** live DB → `FAIL [accept-provenance]: 36 accepts freeze text
no ASR engine produced`, exit 1. Same gate against a consistent copy with those 36 requeued →
`PASS [accept-provenance]`, exit 0, all three invariants hold. The live database was NOT modified;
the remediation destroys 36 human decisions and is the owner's call. `npm run test:python-policies`:
62 policy scripts passed.

**Still true and not yet fixed:** those 36 rows export today as `transcript_source =
human_verified`. Any dataset built before the requeue carries them.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

**Remediated same day, on the owner's go.** Backup first
(`cortex-speech.db.bak-requeue-laundered-20260813`, 6,279,168 bytes, taken with the sqlite backup API
because reviewers were deciding clips during the write and a file copy would have torn). The target
set was recomputed INSIDE a `BEGIN IMMEDIATE` transaction rather than reused from the measurement 20
minutes earlier — in that gap the reviewers had turned 180 accepts into 196, and acting on the stale
list would have missed or mis-hit rows. Under the lock: still exactly 36, and all 36 already carried
the champion's own transcript in `raw_transcript` (asserted before the write, with a rollback on
mismatch, so clearing a decision could not expose some other engine's leftovers).

Cleared on those rows: `verified`, `is_gold`, `human_decision`, `verdict`, `verdict_transcript`,
`annotated_transcript`, `reviewed_by`, `corrected_at`. **Kept deliberately:** `review_events` (the
audit trail of who decided what and when — deleting it would erase the evidence of this incident)
and `decision_verdicts` (a MACHINE verdict feeding the C4 auto-accept-precision denominator;
deleting it would silently move a measured number).

**Verified at the serving path, not the write path:** pending 6 → 42 (+36 exactly);
`review-serving-provenance` all three invariants PASS (exit 0); `check_spot_check_pool` healthy (22
answer keys, 436 human decisions, 4 active reviewers). On the LIVE couch API with a real reviewer
token: `pendingTotal 42`, and **25 of the 36 requeued clips are already in a reviewer's batch, all 25
serving text byte-equal to the champion's own transcript, none blank** (the other 11 sit behind the
25-clip batch cap).

**Corrected, for the record:** the "122 pending" figure reported earlier today is stale. The
reviewers have decided all but 42 of the 494 clips — 436 human decisions and climbing during this
very remediation.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

**Self-inflicted, found and corrected the same session: the requeue wrote LOCAL time into a UTC
column.** Every app writer stamps `updated_at = datetime('now')`, which SQLite evaluates in **UTC**.
The remediation script above used Python's `datetime.now()` — local, UTC+3 on this machine — so the
36 requeued rows carried a timestamp about three hours in the FUTURE relative to every other row in
the table.

Impact, stated precisely rather than dramatically: `segment_row_stamp` (the rowVersion serve/decide
fence) compares the stamp for EQUALITY, so the fence was never wrong and no decision could be
mis-accepted. What it did corrupt is every time-ordered read of those rows — "recently modified"
ordering, any incremental/backlog pass keyed on `updated_at`, and the displayed last-changed time —
for as long as it took real time to catch up.

Corrected with the shape of the defect, not the shape of the incident: `UPDATE ... SET updated_at =
datetime('now') WHERE updated_at > datetime('now')` — a row stamped in the future is the bug, whoever
wrote it. **16 rows** still carried it; the other 20 had already been re-reviewed, and the app's own
UTC write had overwritten them. Zero future-stamped rows remain, and all three provenance invariants
still PASS on the live DB.

The lesson generalizes past this script: any direct write to this database must use
`datetime('now')`, never a Python-side clock, or it silently disagrees with every row the app wrote.

**Reviewer progress at this point:** 22 clips pending — 16 of the requeued 36, plus the 6 escalated
clips that were never decided. **20 of the 36 have already been redone** against the champion's own
text. Last human decision 16:17 local; no activity for ~83 minutes when this was written.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 281 — the listening-QC fired, and one reviewer is an outlier on two instruments

Measured during the evening idle window (reviewers stopped at 16:17 local, 22 clips pending).

**Spot-check scorecard.** `noticed` is the blind-accept signal: the reviewer was served a clip whose
answer is already known and a draft that differs from it, and `noticed` records whether they changed
it rather than tapping accept.

| reviewer | checks | noticed | mean CER vs the known answer |
|---|---:|---:|---:|
| Reviewer E | 16 | 14 (88%) | 0.102 |
| Reviewer H | 5 | 5 (100%) | 0.045 |
| **Reviewer A** | **4** | **1 (25%)** | 0.069 |
| Reviewer G | 2 | 1 (50%) | 0.080 |

**Second, independent instrument — the real corpus.** Share of each reviewer's decisions where the
final text is byte-identical to the draft they were handed:

| reviewer | decisions | accept | edit | reject | text unchanged |
|---|---:|---:|---:|---:|---:|
| Reviewer E | 152 | 39 | 106 | 7 | 30% |
| **Reviewer A** | **266** | **133** | 130 | 3 | **51%** |
| Reviewer G | 27 | 2 | 19 | 6 | 30% |

Reviewer A hands the draft back untouched on half their clips against 30% for both peers, and on the
known-answer clips they changed 1 of 4 — leaving 5.2%, 5.8% and 13.7% CER against text a human had
already established. The 20 requeued clips they redid today came back 19 accepts with ZERO character
change and 1 edit.

**Held against this, honestly.** Four checks is a small sample and this file's own docstring says to
interpret nothing from a handful — the finding is a signal, not a verdict. The 33 laundered accepts
were NOT her fault: the app served her refiner-polished text, and polished text is exactly what a
careful reviewer would accept. And under the verbatim law, accepting the champion's raw output
unchanged is often CORRECT, because what the refiner used to "fix" was frequently the speaker's own
disfluency. None of that explains 1-of-4 on clips where the answer was already known.

**Why it matters more than the numbers suggest:** Reviewer A has made 266 of the corpus's decisions —
more than Reviewer E and Reviewer G combined. The dataset's quality is dominated by the reviewer with the
weakest listening signal.

**The instrument is not exhausted:** answer keys are owner-desktop decisions, excluded per reviewer
only once used, so Reviewer A can still receive up to 18 more checks. The cheap next measurement is more
of her own clips, not an argument about these four.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 282 — the queue reached zero: 494/494 decided, and what the finished corpus actually is

**Every clip is decided. 0 pending.** 494 segments: 278 edits, 199 accepts, 17 rejects.

**The export, run through the real `export_dataset` command on the live app** (not a SQL
approximation of it):

| | |
|---|---:|
| rows exported | 477 (the 17 rejects correctly dropped) |
| `transcriptSource` | **477/477 human_verified**, 0 raw_asr |
| training grade | 454 gold · 18 review · 5 reject |
| training-ready | **454 clips = 1.15 hours** |
| blank transcripts | 0 |

Everything held out of training is held out for AUDIO, not for process: 18 `low_rms_volume`, 5
`near_silence`. No provenance, blank, or placeholder failures remain.

**All three provenance invariants PASS on the finished corpus**, including the accept-provenance
invariant added today — so no accept in the shipped dataset freezes text an ASR engine did not
produce. Spot-check pool healthy (22 keys).

**State the size honestly: 1.15 hours is a small ASR corpus.** It is clean, verbatim and
human-decided, which is what it was built to be, but nobody should call 454 clips a fine-tuning set
without saying how small it is.

**The QC signal got STRONGER, not weaker, with more data.** Reviewer A received 3 more known-answer clips
after the earlier scorecard and changed none of them:

| reviewer | checks | noticed | decisions owned |
|---|---:|---:|---:|
| Reviewer E | 16 | 14 (88%) | 152 |
| Reviewer H | 5 | 5 (100%) | 27 (desktop) |
| **Reviewer A** | **7** | **1 (14%)** | **288** |
| Reviewer G | 2 | 1 (50%) | 27 |

**Do NOT read `mean_cer` as a quality ranking** — Reviewer E's is HIGHER than Reviewer A's (0.102 vs 0.049)
while Reviewer E notices 88% and Reviewer A notices 14%. The answer keys are owner-desktop decisions, and a reviewer
writing true verbatim can legitimately diverge from a key that was standardized. `noticed` is the
robust signal because it measures engagement, not agreement.

**The exposure, stated plainly:** Reviewer A decided 288 of 494 clips — 58% of the corpus, more than every
other reviewer combined — and is the one reviewer whose listening signal is weak. The cheap next
measurement is a blind second pass: route a sample of their 153 accepts through Reviewer E and measure the
disagreement rate. That also builds the double-pass adjudication tier the charter still lacks.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 283 — the whole original corpus was silently imported a second time

**Found at 02:30 by the pass-1 readiness sweep**, not by a gate — no gate covered this.

**What happened.** The 34-hour Hawleri source folder was deduplicated by decoded-audio hash before
import — correctly, 3.57 h on disk collapsing to 2.28 h of unique audio. The question never asked was
whether the LIBRARY already held those recordings. It did. Verified by decoded-audio SHA, three
sources came back a second time:

| already in the library | re-imported copy |
|---|---|
| `Lamofull2_00086400_A01.flac` — 429 clips, **429 reviewed** | `.wav` — 428, none reviewed |
| `Lamofull00086400_A02.flac` — 40 clips, **40 reviewed** | `Lamofull00086400_A01.wav` — 40, none reviewed |
| `A1-0032_PODCAST-001.mp4` — 25 clips, **25 reviewed** | `.wav` — 25, none reviewed |

**493 duplicate clips against 494 already-reviewed ones — the entire original corpus, doubled.** Note
the second row: the names differ (`A01` vs `A02`), so a filename comparison misses it entirely; only
the decoded audio matches. What it would have cost: the same speech in train AND test, making any
accuracy measured across that split partly a memory test with nothing in the number to say so, plus
reviewers judging the same audio twice.

**Removed** under `BEGIN IMMEDIATE` with a pre-flight assertion that none of the 493 carried a human
decision (rollback if any did — the surviving copy must be the reviewed one). Backup first:
`cortex-speech.db.bak-dedupe-reimport-20260814`. Cascade clean: 0 orphan hypotheses. Library 1854 →
**1361 clips, 867 pending**.

**Two candidate gates were built and BOTH rejected on measurement**, which is the part worth keeping:

1. *`(duration_ms, raw_transcript)` collisions.* Caught the incident (188 colliding clips on the true
   pair) and then FAILED on a clean library — 4 false pairs between recordings proven distinct by
   hash. Same speaker, VAD durations clamped to the same bounds, short common phrases. A gate that
   cries wolf gets ignored, so this one was deleted rather than shipped.
2. *Reusing `audio_fingerprint` / `audio_content_hash`.* Both are per-SOURCE-FILE and computed from
   the file as stored: a FLAC and its 16 kHz WAV conversion get different values. Measured on the
   live rows: **0 shared values** between the two copies of the same recording. Neither field can see
   the format change that caused the incident.

What shipped instead is `scripts/check_import_folder_is_new.py` — decode and compare, run ONCE before
an import rather than every sweep, which is exactly where that cost is affordable. Fail-before
verified against the folder that caused this: exit 1, naming `Lamofull00086400_A01.wav` as the same
audio as `Lamofull00086400_A02.flac` with 40 reviewed clips.

**Also observed:** `[Pending WSL 7B ASR]` placeholder rows are servable to reviewers — the couch queue
filters on `verified = 0` and nothing else. During a normal import the app is closed so nobody can
hit them, but an INTERRUPTED import leaves them behind. 36 such rows were left by killing the stalled
importer and were removed with the duplicates. The `champion-fallback` invariant also reads RED for
the duration of any import, because placeholders legitimately have no champion hypothesis yet — worth
knowing before treating a mid-import gate run as a finding.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 284 — the import is latency-bound on the champion, and two speed hypotheses died

**Older non-Hawleri material: READY.** All 471 pending clips pass every readiness condition — audio
on disk, champion draft, champion hypothesis stored, speaker, duration, real `ctc_forced` alignment,
nothing machine-written in the human field, zero cloud calls. **0 blocking defects.** 107 measure
below -35 dB and will be held back from training by the volume gate, reported rather than processed
until they look acceptable.

**Import throughput, measured rather than assumed — and the first number was wrong.**

The "55 clips/min" reported earlier measured segment CREATION. Segments are created carrying the
`[Pending WSL 7B ASR]` placeholder and the champion fills them afterwards, so that number described
an intermediate, not the outcome. **The real rate is 8.5 clips/min**, both before and after the
concurrency change.

Three hypotheses, each measured, two dead:

1. **Only one GPU works.** TRUE and real: `CORTEX_7B_CONCURRENCY` defaults to 1, and pipeline.rs's
   own comment records this exact failure from 2026-08-11. Measured 43% / 3% across the two cards.
   Setting it to 2 changed the rate by nothing — because the gate LIMITS concurrency, it does not
   CREATE it. The importer's per-clip loop is serial, so the second permit is never filled and the
   second GPU never receives work.
2. **A process is spawned per clip.** TRUE (the app runs `wsl` -> a fresh `cortex_7b_client.py` for
   every clip) but NOT the cost: measured `wsl` spawn 0.11 s, `wsl` + python3 0.11 s, + sqlite/socket
   imports 0.13 s. Discarded before any code was touched.
3. **The champion call itself.** Measured: **4.62 s for one round trip** on a ~9 s clip. That is the
   cost. A 7B decoding a short clip is latency-bound, which is also why both cards read as near-idle
   while being the bottleneck — nvidia-smi is sampling the gaps.

**The fix is real parallelism in the importer's per-clip loop** — two clips in flight would use both
cards and roughly halve wall clock. NOT done: it is a change to the champion path, whose client
failure contract exists specifically to stop a silent blank reaching the DB, and rewriting it
unattended at 3am is how that contract gets broken. Owner's call, with the number attached.

**Consequence for the 34-hour corpus:** at 8.5 clips/min it is ~27 hours of import, not the ~6 stated
earlier. About 5-6 episodes land per overnight window. Reviewers are unaffected — 471 clips are
queued and ready.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 285 — a servable placeholder, and 107 good clips held back by a level defect I introduced

**Fixed: the review queue could serve a clip the champion had not drafted.** `pending_segment_ids`
filtered on `verified = 0` and nothing else. `api_decision` already refuses to VERIFY `[...]` text, so
the visible outcome is a 400 — but the path that SUCCEEDS is the damaging one: the reviewer types the
transcript themselves, the clip is finished without the champion ever drafting it, and it permanently
has no baseline for any CER measurement. Demonstrated on the live DB mid-import: the old query offers
**867** clips, the new one **695** — 172 undrafted clips it stops reaching a reviewer right now.
Matched on the same `[...]` shape the decide guard rejects, so serving and deciding cannot drift
apart. Test covers all four cases. NOT YET BUILT: any build into `target/release` fails while the
importer holds `onnxruntime.dll`, so this must be compiled after the import stops and BEFORE the app
is handed back — stopping an import mid-file is precisely what creates placeholders.

**Found: 107 clips (22.7% of the older pending material) sit below -35 dB, and it is my defect.** The
quiet clips are not scattered — they concentrate in whole source files: `A1-0037_Sound01` 10/10,
`A1-0042_Sound01 (2)` 9/9, `A1-0031_PODCAST-001` 6/6, `A1-0049_Sound02` 10/11. That is a per-FILE
level problem, and the cause is that the first staging pass converted those files with `-ac 1 -ar
16000` and NO gain correction. The Hawleri batch got proper EBU R128 normalization; this batch did
not.

**Measured before treating it as damage:** the champion coped. Quiet clips produce **0 empty drafts**
and a median 12.63 chars/sec against 14.27 for normal-level clips — an 11% gap that is within
speech-rate variation. A clip at -45.8 dB returned a clean Kurdish sentence. The audio is fine for a
human to review and fine for training; only the `low_rms_volume` gate holds it back.

**The gate is NOT being relaxed.** Weakening a quality gate to make data pass is the one move this
project forbids outright. The fix belongs at the cause: re-prep those source files with the same
static-gain normalization used for the Hawleri episodes and re-import them. They carry no human
decisions, so nothing is lost — but it costs champion time at 8.5 clips/min and it changes the
corpus, so it waits for the owner rather than happening overnight.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 286 — 55 under-levelled files re-prepped, and a "fix" that measured as no fix at all

**Measured the cause directly instead of inferring it from clip stats.** Of the 145 older staged
files, **55 measure below -26 LUFS** — the worst at **-49.2 LUFS with a true peak of -27 dBTP**. That
is not quiet recording; it is 25-30 dB of headroom left unused, and it is my defect: this batch was
converted with `-ac 1 -ar 16000` and no gain while the Hawleri episodes got a proper EBU R128 pass.

**Re-levelled all 55** with the same static-gain rule (measure, one volume multiplier, never a
limiter, never turn a file down for a peak-margin policy). Output -29.0 .. -19.8 LUFS, max true peak
-1.43 dBTP. **14 remain below -26 LUFS** — high crest factor, no headroom to reach the target without
dynamics processing, reported rather than forced.

**The part worth recording is the correction.** The first pass applied up to +25 dB to my own 16-bit
16 kHz intermediates, and a file at -49 dBFS occupies roughly 8 of 16 bits — so amplifying it should
amplify quantization noise. I redid all 55 from the 48 kHz masters on that reasoning, then MEASURED
both:

    noise floor from the 16-bit intermediate : -42.11 dBFS
    noise floor from the 48 kHz master       : -42.11 dBFS
    difference                               :  -0.00 dB

**No difference at all.** The recording's own noise floor sits about 30 dB above where 16-bit
quantization noise lives, so quantization was never the limiting factor. The theory was sound and the
effect was absent. Recorded because a plausible improvement that measurement cannot find is exactly
the kind of thing that otherwise gets banked as a win.

**Prepared, NOT imported.** `_relevelled_16k/` holds the 55 files. Importing them means deleting their
existing clips first (all undecided, so nothing human is lost) and spending champion time at 8.5
clips/min. It changes the corpus, so it waits for the owner.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 287 — the handover, and an "APP IS UP" that was not true

**Final overnight state:** 3,767 clips / 9.20 h in the library, **3,273 reviewable / 7.95 h**, 494
already decided, 8 of 32 Hawleri episodes imported, 24 prepped and waiting. All five reviewer links
verified by CLAIMING each one over the live API and checking the server returned that reviewer's own
name; queue serves 29 clips with 0 blanks and 0 placeholders; audio streams HTTP 200.

**The incident, in order.**

At 07:30 the handover stopped the importer, deleted 99 leftover placeholders, rebuilt, and re-enabled
the watchdog. The app relaunched at 07:42 and I reported **"APP IS UP"** from a process check. It was
not up. Its log:

    CORTEX_STARTUP_FAIL: Another instance is already running.
    Failed to remove stale Windows instance lock file cortex.lock

The app had died into an error dialog — 37 MB resident, 0.14 s CPU, no log output after startup. A
RUNNING PROCESS IS NOT A WORKING APP, which is the same write-path/serving-path error this project has
now been bitten by four times. What caught it was refusing to call the links good until one returned a
reviewer's name; a fingerprint comparison or a process check would both have passed.

**Root cause was mine.** `TaskStop` on the original overnight chain killed its wrapper but left the
detached shell alive. It waited all night on `while running; do sleep 60; done`, saw the handover kill
the importer at 07:30:35, and started its own import of `_batch_rest` 22 seconds later — holding
`cortex.lock` against the app, and re-importing `KBHP-EP08`, already imported. **348 duplicate clips**,
removed with the usual decision guard (0 carried one). EP08 back to its correct 348.

**Also learned:** the watchdog launches the app WITHOUT the CDP debug port, so the Playwright/IPC
harness used all week is unavailable on a watchdog-started app. Every verification after a watchdog
relaunch has to go through the HTTP API instead — which is closer to what a reviewer actually does,
and is why the link test was possible at all.

**Second build outcome:** `cortex-speech-app.exe` rebuilt at 07:37:38 and carries the queue guard and
the split fix. `batch_importer.exe` failed to relink (`Access is denied` — its file handle had not been
released seconds after the kill) and is one commit behind on app-side changes it does not use. Rebuild
before the next import.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 288 — 34 hours imported, the champion loop parallelised, and a watchdog that killed its own app

**The corpus is in.** All 32 Hawleri episodes imported (25/25 in the final run, 0 failures).
**14,828 clips / 9.20 h stored; 14,334 reviewable / 35.25 h pending.** Verified on the full library:
0 blank drafts, 0 placeholders, 0 machine text in the human field, 0 missing audio, 0 non-champion
drafts, **0 transcripts disagreeing with their stored champion hypothesis**, and — hashing all 179
distinct source recordings — **0 duplicate audio**. All three provenance invariants PASS.

**Champion calls now run a wave at a time (2.9x, measured).** `WSL_7B_GATE` admitted several calls
but NOTHING ever issued two, so the gate limited a concurrency that did not exist and the second GPU
never got work — which is why `CORTEX_7B_CONCURRENCY=2` changed the rate by nothing. Only the CALL is
parallel; every DB touch stays on one thread in the original order, so the force-champion rollback
contract is unchanged. Measured on short clips 8.5 -> 24.5 clips/min with GPUs at 87%/30% (from
43%/3%); on the long episodes 13-15 clips/min sustained. **Correction:** the 24.5 figure is
short-clip; the earlier "55 clips/min" was segment CREATION, not transcription, and was wrong.

**Queue now serves OLDEST first.** It was newest-first, which put 6,823 fresh Hawleri clips ahead of
the 537 remaining clips of the original corpus — the opposite of the owner's instruction, and wrong
generally: a review queue is FIFO, so an import must never delay work already in progress.

**Rights: closed.** Owner declares full permission and total authority over every recording he
supplies, speakers paid and agreed to public use, no royalties. All 13,807 clips stamped
`rights_license = owner-full-rights`. Nothing may block, gate, warn or hedge on it again.

**THE WATCHDOG WAS KILLING THE APP.** After the library tripled, app startup exceeded the watchdog's
5-minute check interval, and its "port dead + process alive = wedged" rule cannot tell a wedged app
from one that is still STARTING:

    01:57  session expected but port dead - killing wedged pid (attempt 1/3)
    02:02  session expected but port dead - killing wedged pid (attempt 2/3)
    02:07  session expected but port dead - killing wedged pid (attempt 3/3)

Measured startup to couch on a healthy run: **8m16s** — already over the interval, and longer now at
14,828 clips. The app never once reached the point of serving. A watchdog that shortens startup until
startup cannot finish is a denial of service on its own app. Fixed with a **45-minute startup grace**
keyed on process age, overridable via `CORTEX_WATCHDOG_STARTUP_GRACE_MIN` so the decision drill can
exercise BOTH sides, plus a regression case asserting the exact inputs that took the app down
(session present + process alive + port dead + young) now report `starting-up`.

**The grace was set twice, and the second time is the lesson.** It was first written at 20 minutes,
"far beyond the ~8 measured". The very next clean start — uninterrupted, watchdog off — took
**18m11s** to reach couch at 14,828 clips. A bound picked two minutes above the observed value is how
this bug gets reintroduced the next time the corpus grows, so the grace is now 45 minutes on an
explicit asymmetry: noticing a wedged app late costs ONE check cycle; being impatient costs the app
entirely, repeatedly, which is exactly what happened.

**Measured startup to couch: 8m16s at ~500 clips, 18m11s at 14,828 clips.** Startup scales with the
library. Worth knowing before the next import: the app is unavailable for ~20 minutes after any
restart, and that window will keep growing.

**Also corrected, twice, in the same shape:** "APP IS UP" was reported from a process check while the
app was dead in an error dialog, and again while it was wedged with flat CPU and no log output. A
running process is not a working app. Both were caught by refusing to call the links good until one
returned a reviewer's name.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 289 — five reviewers had empty queues, and the 20-minute cold start was one line

Overnight reliability session. The brief was "get to a true trustworthy reliable system to start real
work", so the measurement that mattered was not whether the tree is correct but whether a named
reviewer opening their link tonight can actually work. Two of the four things found are answers to
questions earlier iterations asked and got wrong.

**FIVE OF EIGHT REVIEWERS HAD A QUEUE OF ZERO, and two independent bugs hid each other.** The 1,031
recovered clips were relinked into `D:\Kurdish Corpora\sorani\ZarPodcast`, but `dialect.rs` still
mapped only their pre-recovery path (`SoraniVoice_PC_`), so all 535 pending Sorani clips were
UNMAPPED — and the dialect check fails closed, so every Sorani-only reviewer (Reviewer B, Reviewer D, Reviewer F,
Reviewer G, Reviewer E) was served nothing. Simultaneously the roster file itself was inert: it shipped with a
`"_comment"` string explaining how to edit it, and a strict `HashMap<String, Vec<String>>` parse
rejects that outright, whose failure path is "unrestricted" — so the dialect protection was OFF for
everyone from the moment it was installed, including the three reviewers it was written for. A
helpful comment turned the safety feature off. Neither bug is visible from the tree: the database
rows, the JSON and the Rust all read correctly in isolation.

**THE 20-MINUTE COLD START IS `run_to_completion(5, 250ms)`.** Iteration 288 measured startup-to-couch
at 8m16s at ~500 clips and 18m11s at 14,828, called it "startup scales with the library", and raised
the watchdog grace to 45 minutes — the second time the grace was raised to accommodate it. The cause
is one line: `Database::backup` paced the SQLite online backup at 5 pages per 250 ms, the literal
rusqlite doc example, copied without arithmetic. That is 80 KB/s. `take_snapshot` runs SYNCHRONOUSLY
on the startup path, so the 84 MB library held the review port shut for ~16 minutes every launch, and
since the periodic snapshot timer is 10 minutes — SHORTER than one snapshot took — a copy was
essentially always running against the database reviewers were writing to. Measured fail-before:
1,371 pages took **68.6 s** at the old pacing, which extrapolates to 18 minutes for the real library
and matches 18m11s. At 4096 pages/step it is under a second. "Scales with the library" was the true
observation and the wrong conclusion.

**Spot checks bypassed the dialect filter.** They are injected after the queue's own filter, on a
separate path, so they were the one remaining way to hand someone a dialect they do not speak — and
the most damaging one, because a check is SCORED: an honest reviewer fails a test they had no way to
pass and is recorded looking exactly like someone tapping "looks good" without listening.

**A gate for the class, not the instance.** `reviewer-queues-live` computes, against the LIVE database
and roster, what each NAMED reviewer would actually be served, and fails when anyone holding a link
has nothing. `supervision-live` could not see this: the server answers 200 for an empty queue. The
gate reads the dialect map out of `dialect.rs` rather than restating it, because the drift between
map and reality IS the bug — and it caught a raw-string parsing error in its own reader that way.

**Sweep at 01:39:** 36 gates, 32 PASS / 4 FAIL. `typecheck` was a real regression (the merged commit
dropped `asr_provider` from `BackendSettings` and left it in two fixtures); `python-policies` was this
ledger being 4 commits stale; `ignored-real-model` was the WSL 7B server being down, which red-ed the
whole leg and took six PASSING real-model tests with it — now split into its own env-probed leg;
`spot-check-pool` remains genuinely RED at 22 of 24 keys and is owner-gated. `bench-budget` PASSED
despite a concurrent GPU ingest job, and `test-rust` passed, confirming both earlier failures as
environmental.

**Still owner-gated:** the answer-key pool needs a few desktop adjudications to reach 24, and the
Sorani backlog is 535 clips (1.2 h) against 13,777 Hawleri (34.0 h) — five Sorani-only reviewers will
drain it in a sitting, so more Sorani material needs importing to keep them working.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 290 — the knife is speaker-aware where it measurably matters, and nowhere else

The owner hit two-voice clips while reviewing — the same complaint that created iteration 225 — and
asked for the cut itself to be fixed. 225's post-mortem was the mandatory reading: it built speaker-
aware splitting, measured it on real audio, could not show it helped (12→10/11/14 mixed under a
length-confounded metric), and reverted with a warning to read the numbers before repeating them. So
this iteration's rule was: every wiring decision comes from a fresh measurement on real audio, and
anything unmeasured does not ship.

**MEASURED FIRST: the merge step is the wrong organ for this corpus.** The first design judged
speaker identity where VAD regions MERGE. Instrumented and run on KBHP-EP01 (58.5 real minutes of the
two-host podcast): the judge was consulted **ZERO times** — dense podcast speech produces long
continuous VAD regions, so the planner's work is SPLITTING oversized regions, not merging. The
gluing of two voices happens at `silence_aware_split`, which chose the widest pause with no idea who
was speaking. The merge-time judge stays (pure-tested, fail-open, structurally inert here but live
protection for sparse-speech corpora), and the real fix moved to the split.

**THE FIX THAT SHIPPED: relocate mandatory cuts to the pause where the voice changes.** When an
over-cap region must be cut anyway, `speaker_turn_cut` auditions the band's widest pauses (top 6)
with CAM++ windows either side and takes the first where the two sides are DIFFERENT voices;
otherwise behaviour is byte-identical to silence-only. Refusal bar is 0.43 — the ceiling of the
turn-taking group in the owner's blind listening pass, NOT the 0.59 midpoint that 225 used — so the
length confound pushes borderline windows toward the historical behaviour, never toward shredding
one voice. Because the cut happens either way, chunk count and length distribution are untouched,
which is exactly the property that makes 225's confound inapplicable.

**MEASURED ON KBHP-EP01: 265 chunks → 265 chunks, min duration unchanged, 0 floor violations, 62
boundaries judged (sims spanning −0.06 to 0.83 — real signal), 4 cuts relocated**, all in one
turn-dense passage (41:19–41:55), each printed with a timestamp for the owner to listen to. The
verification unit is one audible claim per relocation, not a statistical metric.

**THE EXISTING 14,828 CLIPS get the validated protection**, not the new knife: the probe is running
`--persist`, after which every measured clip carries `speaker_change_score`, the phone badge ("⚠ more
than one speaker" / "⚠ زیاتر لە یەک قسەکەر") fires at the calibrated within-clip scale, and a new
tri-state `speaker_turn` column (true/false/NULL) rides every export format — with NULL meaning NOT
MEASURED, never "single speaker", because those clips sat unmeasured for weeks while every one wore a
confident SPEAKER_0x.

**Honest ceilings, unchanged from the probe's own doc:** a sub-1.5s interjection cannot be judged by
any embedding (the stable-embedding floor), and simultaneous cross-talk blends into one embedding
CAM++ cannot report as "both" — that class needs an overlap-aware model and stays out of scope. The
reviewer instruction that covers both: type every word you hear, whoever says it.

Also: the reviewer page now shows each clip's own length beside the position ("Clip 7 of 535 (9s)"),
per the owner's ask, next to the running paid-audio total.

Gates: 1211 lib tests (28 chunking, 65 export), clippy -D warnings, fmt, 69 python policy scripts,
typecheck 443 files — all green before commit.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## Iteration 291 — the owner's ears audited the dataset, and the dataset is now clean

The owner reviewed clips by ear and caught, in one sitting, what no gate had: the same sentence
served three times. The chain from there: one recording lived in the library under THREE encodes
(Lamofull full-recordings + A1-00xx episode cuts + an mp4 with a constant 137.8s clock shift), all
invisible to the byte fingerprint. First audit (offset+text) measured 68 redundant clips; the owner
heard twins on a page that audit had "deduplicated", which exposed the mp4's shifted clock and a
letter-drifted pair — audit v2 (content rules A+B, union-find) measured the honest number: **170
redundant clips across 113 groups, 117 of them carrying redundant human decisions** (reviewer time
paid twice).

**CLEANED, e6aaf93's discipline.** Backup `pre-dedupe-20260817-162534`, keep-rule preferring the
verified+decided copy (106 groups) then decided (6) then the canonical fuller file (1), children
deleted in the same BEGIN IMMEDIATE transaction, `review_events` kept as append-only history.
Library 14,828 → 14,658. Post-conditions, all measured on the live DB: 0 duplicate groups, 0 orphan
child rows (plus 5 PRE-EXISTING stale spot_checks rows from the 08-13 incident found and removed),
FTS in sync, integrity_check ok. Payment totals honestly adjusted (Reviewer E 25.0 → 17.8 min — their
double-reviewed twins were the bulk of the 117).

**The 37 defective verified clips went back to the paid reviewers**, not to the owner: 27 two-voice
+ 12 wrong-dialect cleared via clear_human_decision's exact semantics (backup
`pre-requeue-20260817-160349`), re-queued with the ⚠ badge showing and dialect routing enforcing who
may judge them. The owner types nothing. The spot-check pool dropped 22 → 21 keys because one
answer key was itself a two-voice clip — a BAD key, correctly retired.

**Standing protections born today:** `docs/OWNER_CANON.md` (+ 8 machine pins, python-policies leg) —
approved-FINAL decisions changeable only by the owner's literal `change canon:`; the
`dataset-duplicates` sweep gate at baseline **0** — any duplicate import from now on is a RED sweep.

**Cleanliness, measured after everything:** 0 missing audio files, 0 empty/placeholder drafts, 0
verified clips with empty transcripts, 0 verified two-voice clips, 0 orphans, integrity ok, all
live gates green. Honest remainder: simultaneous cross-talk is per-clip reviewer judgment (badge +
the type-every-word/REJECT rule), and the QC pool needs 3 owner adjudications to reach 24.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

---

## 2026-08-17 — the external audit closed, provenance for processed audio, and the training pack learns to describe itself

**Three landings, two merged PRs, and one honest failure worth recording.**

### PR #61 — the app-reliability audit's remaining list (629e9f1)

Nine commits. The substantive one: the long-audio import called itself streaming while its decode
callback was `Send + 'static`, so the only thing it could do was push every window into a Vec and
process the file afterwards. Measured on the library's longest source (KBHP-EP12.wav, 5,315 s /
162 MB):

| | before | after |
|---|---|---|
| peak PCM held | 170.1 MB | ≤ 11.5 MB |
| full-file hashes | 60 | 1 |
| retained chunk f32 (champion config) | 340 MB | 0 |

Windows now arrive over a `sync_channel(1)`; the timeout is per-window and genuinely stops the
decoder; `decode_to_pcm_with_timeout` gained an abort flag its worker checks per packet. Proven
faithful on real audio: 60 windows, byte-identical offsets and lengths to the direct decoder,
`is_last` set exactly once.

Also: 200 % zoom (`h-full` + `justify-center` inside a scroller pushed content above `scrollTop: 0`
where nothing can reach it — six boxes, measured at 640×400 in the running app); ErrorBoundary made
a real `<svelte:boundary>` (it listened on window `error`, so one failure painted all ten instances
red); a refused Settings save no longer sticks; total deadline on cloud calls and a write deadline
on couch responses; trust-boundary checks on the executed script path and the off-drive backup dir;
60-day log retention (23 MB, unbounded); the freshness gate now sees stale installers.

### PR #62 — processed audio declares itself (6dc958c)

The pre-import cleaner (`kurdish-audio-cleaner`, separate repo) separates voice from music with
MelBand-RoFormer, **cuts every non-speech stretch out**, re-concatenates with 150 ms pauses, and
normalises to −20 LUFS. Its output is an ordinary WAV — indistinguishable from a raw field recording
by inspection, and until now indistinguishable in this database too. 550 h of it was staged to
import.

Migration **v54** adds `source_audio_provenance`, keyed by SOURCE PATH (the processing is a property
of the recording, and keying it this way leaves `speech_segments`' column layout — and
`SEGMENT_SELECT_COLUMNS`' index-based `map_row` — untouched). `source_provenance::detect` reads the
cleaner's own manifest; the import records it before decoding anything; `export_dataset` and the
published `dataset_card.md` print one notice per affected recording.

Detection requires an explicit `audio_is_processed: true` and NEVER infers from a folder name: a
false positive brands originals as processed, a false negative is the exact lie this prevents. An
absent record means **unclaimed**, never "verified original".

Verified end to end on the real corpus: the cleaned tree declares its separator model, its removals
and that its timeline does not map back; `KBHP-EP12.wav` beside it stays unclaimed. The tree's
manifest predated the provenance block and was backfilled from the cleaner's own constants.

### Phase 1 + 2 of docs/PLAN_TRUE_10.md

The 2026-08-17 flywheel audit was reality-checked line by line against the code and the live DB.
**Mostly true** — training is external and unwired, the pack was unsplit and minimal, and the labeled
set really is **426 clips / 64.7 min with 94.7 % from one recording** (measured, not quoted). **Two
claims refuted:** Inbox decisions are NOT omitted (`record_human_decision` sets `verified=1`), and
accept-saves-a-different-transcript was fixed on 2026-08-17 by the accept-what-you-SEE snapshot.

*Phase 1* — the 550 h cleaned corpus imports in diversity-ordered batches (smallest books first;
each book is one narrator). Batch 1 = 5 narrators / 2.69 h.

*Phase 2* — the pack now carries `split` (leakage-safe, from the same `assign_splits` the HF export
uses, fixed seed, every clip of a recording in one split), plus `segment_id`, `source_recording`,
`decision`, `decision_revision`, `grade`, and `audio_processed`. The snapshot id IS the manifest
content hash, sealed into `dataset_runs` with INSERT OR IGNORE — `dataset_runs` existed with zero
rows and no writer; this is its first.

**Counted, never silent:** `emittedWithoutHumanDecision` reports rows verified with no live human
decision — batch-verified, or a verdict the reviewer UNDID, since `clear_human_decision` clears the
decision but leaves `verified=1`. The rubric grades those SILVER so nothing is mislabeled; the
quantity was simply invisible. Review semantics deliberately unchanged — whether undo should re-open
a clip is the owner's call.

**Deliberately NOT changed:** the review queue. Round-robin across recordings was considered for
label diversity and rejected after reading the code — oldest-first FIFO is a documented decision (a
27 h import once buried 537 clips of the original corpus behind 6,823 newer ones). With FIFO, import
SELECTION is the diversity lever.

### The honest failure

The first headless import refused all 23 files: **closing the app kills the champion**, because
supervision lives in the app process (`engine_runtime.rs` holds the `wsl.exe` child). The hard stop
behaved exactly as designed — 0 clips written, nothing half-transcribed. Two more self-inflicted
failures followed on the batch runner: the re-enabled watchdog relaunched the app and took the
instance lock, and Python's CRLF left a carriage return inside every directory name (`Os code 123`)
while a `grep`-based success check hid the reason. All three are now written into the runner's own
header so the next run cannot repeat them.

**State:** library 14,894 clips; 34 cleaned recordings imported and declared; 1,218 Rust lib tests,
72/72 python policies at commit time, 272 frontend tests, clippy + fmt clean. Remaining red gates are
environmental and owner-gated: `spot-check-pool` needs 3 owner adjudications, and the watchdog is
disabled only for the open import window.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

---

## 2026-08-18 — Phase 3 closes the challenger loop, and adversarial review breaks Phase 2

**Phases 1-3 of docs/PLAN_TRUE_10.md. The most useful thing that happened was a defect found in
work I had already reported as done.**

### The split was leaking, and my own test said it was fine

`assign_splits` groups by the path's BASENAME, so "same recording" meant "same filename". Two real
consequences on this library:

* `A1-0032_PODCAST-001.mp4` is a re-encode of the Lamofull material (check_dataset_duplicates.py
  documents it). Lamofull is **94.7 %** of labeled duration, so it fills `train` alone and FORCES
  every other group into validation/test — the re-encode was measured landing in `validation` while
  the same session trained the model.
* The audiobook corpus names every chapter `01.wav`, `02.wav` … inside its own book folder, so
  chapter 4 of three different books collided into one group.

Grouping now keys on `audio_content_hash` (v51). **The regression test took two attempts and the
first is the lesson**: it PASSED against the broken code, because four equal-sized recordings let the
greedy fill put the twins together by luck. Rebuilt with the library's real shape (one dominant
recording), the old grouping fails exactly as predicted:
`content same-content is spread across splits {"validation", "test"}`.

Also removed a claim I had SEALED into the pack provenance: `"speaker+recording disjoint"`. It is not
speaker-disjoint — the only automatic labeler emits per-recording `SPEAKER_00…` indices, which
assign_splits scopes per recording, so the speaker union links nothing. Sealing a guarantee the data
cannot support is the exact failure that record exists to prevent.

And `relink_audio` moved `speech_segments.audio_path` while leaving `source_audio_provenance` at the
old key, so relinking silently turned a DECLARED processed recording back into "unclaimed".

### The duplicate gate learned to listen

Text matching alone flagged legitimate repeats: `bangewazek_bo_behesht` announces the series title at
the top of every episode, and a Mehwi ghazal collection repeats verses. With 134 books to import the
gate would have gone permanently red — and a gate that is always red is one nobody reads. RULE C: a
text match is a CANDIDATE, the clip audio confirms it. Baseline stays 0; audio that cannot be READ is
reported unconfirmed and KEEPS FAILING. Live: 6 groups cleared by audio, 0 duplicates.

### Phase 3 — the chain closes

`sealed snapshot -> verified pack -> run record -> trainer -> scorecards -> slices -> PROMOTE/REJECT`

* `promotion_gate.py` — 10 arms, all failing closed: cross-basis or unpaired comparison is INVALID;
  no improvement, sub-threshold improvement, or MAPSSWE p > 0.05 is REJECT; **any protected slice
  regressing is REJECT**.
* `build_eval_slices.py` — generates those slices (dialect from `dialect.rs`, speaker by RECORDING
  since diarizer labels are indices not identities, noisy/clean by measured SNR).
* `train_challenger.py` — refuses a pack whose manifest does not hash to the snapshot id, rows with
  no `train` split, missing clips, or an unsealed snapshot. With no trainer configured it exits **3**
  with status `prepared` — never a record implying training that did not happen.

**Measured end to end on the live library:** a challenger with a 32 % relative overall CER win
(0.2000 -> 0.1350) that regresses Hawleri (0.2000 -> 0.2600) is REJECTED, exit 1.

### Phase 1 — batch 1

5 books / **47 files / 0 failed**. Cleaned corpus in the library: **1,247 clips / 177.6 min / 70
recordings**, undeclared: **0**. Library total 15,905.

**The honest limit:** importing is not labeling. Gate B still reads **426 clips / 1.08 h labeled,
top-1 94.7 %, 5 recordings** against a target of 25 h / ≤30 % / ≥25. Only human review moves it;
15,453 clips / 37.9 h now sit in the queue for that.

### CI, twice

PR #63 went red on Linux and macOS. First diagnosis (numpy: the new audio tests import it, CI's
policy suite runs on a bare setup-python with no pip install) was REAL but INCOMPLETE — a full
CI-simulation run with numpy hidden found the ledger gate red as well. Both fixed here. The audio
assertions now skip without numpy, which loses coverage of an optional confirmation and not of the
rule: without numpy every text-matched group stays unconfirmed and the gate keeps failing on it.

**State:** 1,219 Rust lib tests, 76 python policy scripts (75 green in a simulated-CI run + this
ledger entry), clippy + fmt clean. Owner-gated and unchanged: `spot-check-pool` 21/24 needs 3
adjudications.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-18 (overnight) — the pack export was quadratic, and the playback error was never the audio

**Two defects, both found by measuring instead of assuming, both landed with a gate proven to fail
first. Gates C and D moved off SKIP-ENV onto real data. Gate D's own definition is NOT met, and that
is stated plainly below rather than rounded up.**

### The canary export was going to take all night (PR #64)

The gate-D canary was launched at 02:08 and had produced **87 rows in 56 minutes (~36 s/row)**. It
was still running an hour later. The cause was not slow audio decoding:

`decode_finetuned_clip_16k` walks a source from byte zero to the clip's end — right for one clip,
**quadratic for a pack**. This library keeps **416 of its 447 verified clips inside one podcast
FLAC**, so exporting it re-decoded that FLAC 416 times, each walk longer than the last.

The export now runs in three passes: choose the rows (no audio touched, so dedup settles before
anything is decoded) → decode **grouped by source recording**, one streaming walk each, handing each
clip over as its window passes → write the manifest in the original order. Peak memory stays bounded
to the clips overlapping the current 30 s window.

| | old (per-clip walk) | new (per-source walk) |
|---|---|---|
| rows produced | 132 in **78 min** | **414 in 50 s** |
| per row | ~35 s | ~0.12 s |
| decode walks for 12 clips of one file (test) | 12 | 1 |

**The byte-identical claim was verified against the old code's real output, not just asserted.** The
slow run was left alive to the end for exactly this: its 132 rows and 135 clips were compared against
the new exporter's.

```
slow rows (old per-clip code): 132
MANIFEST PREFIX: byte-identical over all 132 rows
clips byte-identical: 135 | differing: 0 | missing from fast pack: 0
```

So the snapshot id does not move because of this change.

The regression gate counts **decode walks** through a thread-local counter, because the bug is
invisible to output equality and a wall-clock assertion on this box is a flake generator. Against the
old walk:

```
assertion `left == right` failed: 12 clips out of ONE recording must cost ONE decode walk,
not 12 — this is the quadratic export that made a pack take hours
  left: 12
 right: 1
```

The same run's slice assertions passed under BOTH implementations — that is the evidence for
byte-identity at unit scale. The fixture covers overlapping clips, a clip straddling the decoder's
30 s window boundary, and a clip whose span runs past the end of the file.

Also in that PR: `export_pack`, a headless exporter (gate D forbids clicking through the desktop app
to seal a snapshot; it deliberately does not take the instance lock, so producing a snapshot never
costs reviewers their links).

*Correction to that PR's own commit message:* it claims a failed WAV **write** is "now fatal rather
than skipping a source". Checked against `origin/main` — the original already used `?` on
`write_wav_16k_mono`, so a write failure was ALWAYS fatal. The new code preserves that; it does not
change it. What IS new is the decode-failure path: a dead source now skips its rows once and logs
why, instead of each of its clips failing individually.

### The playback error the owner hit was not a codec (PR #65)

Reported: **"Playback was blocked or the file could not be found"** while reviewing.

The obvious suspects were the containers. The review queue holds 361 `.mov` (`pcm_s24le` in
QuickTime) and 51 `.mp4` (FLAC-in-MP4) — both plausible WebView codec gaps. **I probed them with a
real `<audio>` element against the actual library files instead of trusting codec lore, and the
theory was wrong:**

```json
{"src": "sample.mov", "verdict": "PLAYS", "duration": 21.625}
{"src": "sample.mp4", "verdict": "PLAYS", "duration": 4.1}
```

The real cause is in `AudioPlayer.svelte`. Per the WHATWG spec a pending `play()` is **rejected with
AbortError** as soon as anything supersedes it, and `resolveAudioUrl` supersedes it on every source
switch: it pauses the element and reloads it. Advancing to the next clip while the current one was
still starting rejected the play, and that rejection was reported as a playback failure.

**This was not a cosmetic toast.** `audioError` is bound to the parent, where it disables Accept/Save
so a verdict cannot be recorded on audio nobody could hear (2026-08-17 — a correct guard). A spurious
AbortError therefore **locked the reviewer out of accepting a clip that plays perfectly**.

*Correction, made before this entry landed:* I first wrote that those 412 `.mov`/`.mp4` clips were
"one file each". The rows were measured; the FILES were not. Measured:

```
.mov   rows=361  distinct files=108  clips/file=3.34
.mp4   rows=51   distinct files=32   clips/file=1.59
.wav   rows=15034 distinct files=102 clips/file=147.39
```

412 clips over **140** files, ~3 each — so a source change every few clips rather than every clip.
The defect and the fix are unchanged; only the frequency claim was overstated, and this project does
not get to state a number it did not measure.

Fixed with a generation counter — a rejection from a superseded attempt is discarded, only the newest
attempt may set `playing` — plus an explicit AbortError discard. Nothing real is hidden: undecodable
audio rejects `NotSupportedError`, blocked autoplay `NotAllowedError`, and the second test pins that
an undecodable clip is STILL reported. Failing first:

```
× advancing to the next clip does not raise a playback error or block the decision
AssertionError: advancing mid-start must not report a playback failure:
  expected [ Array(1) ] to deeply equal []
```

### The same bug had a worse sibling, found by verifying at the consumer

Checking the "locked out of Accept" claim against `ReviewMode.svelte` rather than asserting it turned
up a second defect. `audioError` was cleared in exactly two places: when `audioPath` **changes**, and
if the reviewer noticed the Retry link. Consecutive review clips from one recording SHARE an
audioPath, and the dominant recording holds **403 of the 414 exportable clips** — so one failure kept
Accept, Save and Mark-bad refused for the rest of that recording. While the banner is up it REPLACES
the transport, so there is no play button left to press either.

A `play()` that starts is proof the audio is audible, so it now clears the error.

Pinning it needed a real parent component. `@testing-library`'s `rerender` reassigns every prop, which
re-runs the audioPath effect and reloads the element — the one thing that already cleared the error,
so the bug was invisible through that harness. The component's actual call sequence, traced rather
than assumed:

```
load() >> play() >> [reject] >> banner? true >> [advance to clip-2]
  >> pause() >> load() >> pending plays: 0
```

`tests/fixtures/AudioPlayerHost.svelte` holds audioPath fixed while clipKey advances, which is the
shape `ReviewMode` actually has. With it the gate fails without the fix:

```
× the next clip of the SAME recording is not still blocked by the previous failure
AssertionError: playback started on the next clip, so the audio is audible and the
decision must unblock: expected null not to be null
```

### Bounding the blast radius: the PAID reviewers were never affected

Checked rather than assumed, because the 8 live links matter more than the desktop UI. The couch
phone page is a different implementation, and it is the better one:

* `play()` rejections are swallowed (`.catch(() => {})`) — correct there, because a superseded play is
  not a failure, which is precisely the lesson the desktop side just learned;
* genuine load failures are caught on the element's `error` event, which names the reason and offers a
  **skip that writes NOTHING to the corpus** — no decision, no verdict, no attribution.

So the lockout was confined to the owner's own desktop review. The desktop was the outlier, not the
couch page. Also swept `ReviewMode.svelte` for the same bug family — every `saving` / `retranscribing`
/ `aligning` flag has a matching `finally`, so none of them can stick on a failure.

### Gates C and D, measured

A real pack was exported and its snapshot sealed, then the chain was run against it.

```
verified candidates            : 447
excluded (holdout leak)        : 18
excluded (not training-ready)  : 15
skipped (empty/dup/undecodable): 0
emitted                        : 414
of those, no human decision    : 0
snapshot id                    : 23db46a0b66c1c064c870a3c9afaf6d393aa5686f61f506b56e4fd86203e3ebe
newly sealed                   : true

CHALLENGER: snapshot 23db46a0... verified against the pack
  train 403 rows | validation 7 rows | test 4 rows
CHALLENGER: PREPARED (not trained)          exit 3

SNAPSHOT IMMUTABILITY: OK (1 sealed snapshot(s), every id a content hash of what it sealed)   exit 0
CHALLENGER LOOP: OK (1 run(s), 0 trained, 0 verdict(s), all internally consistent)            exit 0
```

**Gate D is NOT met, and the passing script does not mean it is.** Gate D's definition in
`docs/LOOP_TO_10.md` §2 is *"a canary completes snapshot → train → eval → verdict with zero manual
steps."* What ran is `snapshot ✓ → train ✗ → eval ✗ → verdict ✗`. `check_challenger_loop.py` passes
because its job is to catch a run record that claims more than it did, and this one claims exactly
what happened: `status: "prepared"`, `trainer_command: null`. **The loop is blocked because no
trainer exists** — `train_challenger.py --trainer` expects an external fine-tune command, and writing
a real LoRA trainer for the 7B champion is a compute and model-handling decision that belongs to the
owner, not to an overnight run.

The 403/7/4 split is not a bug either: content-grouping puts the whole dominant FLAC in one group, so
80/10/10 cannot be honoured when 97 % of the clips are one recording. That is the same skew gate B
measures, showing up in a second place.

### Measured, NOT fixed: every export re-hashes the whole source once per clip

Chasing the quadratic export turned up a second cost in a different function, so it was measured
rather than assumed. `decode_to_pcm` (the path the CSV/HuggingFace/WAV exports use, one call per
segment) computes its LRU key by blake3-hashing the ENTIRE source file before it can look the entry
up — so a cache HIT still reads and hashes the whole file, then clones the decoded PCM.

Release build, on the dominant recording:

```
MEASURED file=326.8 MB decoded=120.0 MB | cold decode 44.37s | cached hit avg 0.162s (5 runs)
        | 414 clips would pay 66.9s of cache-hit overhead
```

So a full dataset export burns about **67 seconds doing nothing but re-hashing a file it has already
decoded**, and that grows with both corpus size and file size. (The same probe in a debug build reads
0.222 s/hit — quoted here only to note that the debug number is NOT the real one.)

**Deliberately not fixed tonight, and this is the reason.** The obvious fix — memoise
`path -> content hash` on `(path, len, mtime)` — trades a *correctness* property for speed: content
addressing is what makes the cache immune to a file being edited in place. The exposure is small
(same length AND same 100 ns mtime) but it is a real weakening of a deliberate design, on a machine
whose whole point is a verbatim corpus. That is an owner call, not an overnight one. The export is
slow, not wrong; the quadratic one was blocking a gate, this one is not.

### Corpus, measured tonight

```
verified rows      : 447        distinct sources : 8
labeled (gate B)   : 429 clips / 1.086 h        dataset_runs : 1 (was 0)
```

Gate B targets 25 h / top-1 ≤ 30 % / ≥ 25 recordings. Unchanged by tonight's work, and only human
review moves it — which is exactly why the Accept-lockout bug above mattered.

### Gate B is reachable on review throughput alone — and my "fix" for it was unnecessary

I suspected the couch queue's strict FIFO (`ORDER BY created_at ASC, id ASC` in
`pending_segment_ids_for`) was hurting gate B: with ~147 clips per `.wav`, reviewers should be walking
long single-narrator runs, which is the worst possible shape for a "top-1 recording <= 30 %" target.
Before changing anything, I replayed the REAL pending queue in the exact order the couch page serves
it, starting from today's 429 labels:

```
FIFO (what the queue serves today):
  after N more labels |  hours | top-1 % | recordings | gate B?
                1000 |    3.5 |    28.8 |         91 | no
                5000 |   13.4 |    10.6 |        155 | no
               10000 |   25.7 |     5.5 |        167 | PASS
               15458 |   39.0 |     3.6 |        245 | PASS

Round-robin by recording (NOT implemented — comparison only):
               10000 |   25.5 |     4.0 |        245 | PASS
```

**The hypothesis was wrong and no change was made.** FIFO already reaches 91 distinct recordings
inside the first 1,000 clips, because the importer interleaves files as it processes them — the long
single-narrator runs I assumed simply are not there. Round-robin buys 1.5 points of top-1 share at
the moment gate B passes, which is not worth touching what 8 paid reviewers see.

**The number that matters:** gate B needs roughly **10,000 more human LABELS** — at which point it
reads 25.7 h, top-1 5.5 %, 167 recordings, and PASSES all three arms.

Labels, not clips reviewed. Measured conversion on the work done so far:

```
edit 241 + accept 188 = 429 labels        reject 18        -> 429 of 447 decided = 96 %
```

so ~10,400 clips have to be REVIEWED to yield those ~10,000 labels. Either way: no code change, no
import, and no design decision stands in the way. Gate B is review throughput and nothing else —
which is exactly why the Accept-lockout fixed above was worth the night.

### Owner-gated — one item cleared itself

Every cheap gate was re-run tonight and the **spot-check pool is no longer blocking**:

```
SPOT-CHECK POOL: healthy - the QC that proves reviewers are listening can fire
  human decisions        : 429
  active reviewers       : 8
  spot-check answer keys : 24   (need >= 24)
REVIEWER QUEUES: OK (8 live link(s), every one has clips to review; 15458 servable pending)
DATASET DUPLICATES: OK (no cross-file duplicate content)
REVIEW SERVING PROVENANCE: all invariants hold
SUPERVISION GATE: OK (watchdog enabled, 8 reviewer link(s) answering on 8737, 564.4 GB free)
```

So the standing "3 spot-check adjudications (21/24)" is STALE and is retired here. Still owner-gated:
the 12 wrong-dialect re-reviews, and now **whether to build a real challenger trainer** — the only
thing standing between the wired loop and gate D.

### The one red gate was RIGHT, and my first explanation of it was wrong

A `verify_10.py --quick` sweep read `22 PASS, 1 FAIL (test-e2e+a11y), 18 skipped (env/not-built)`.

I first guessed the FAIL was my own fault for switching branches mid-sweep, re-ran `npm run test:e2e`,
got **95 passed**, and was ready to call it explained. Both halves of that were wrong, and reading the
gate's own log instead of guessing is what caught it:

```
Error: http://localhost:1420 is already used, make sure that nothing is running on the
port/url or set reuseExistingServer:true in config.webServer.
```

A **stale vite dev server, running since 20:44 the previous evening**, held port 1420.
`playwright.config.ts` sets `reuseExistingServer: !CI && !CORTEX_GATE`, and `verify_10.py` exports
`CORTEX_GATE` — deliberately, so that a stale-but-valid dev server makes the leg RED instead of green
about code that is not under test. The gate was working exactly as designed.

**And my re-run was the vacuous pass that config exists to prevent.** Run interactively, reuse is ON,
so those 95 tests attached to yesterday's stale server. The number was real; what it measured was not
what I claimed.

Stopped the stale server and re-ran the gate the way the sweep runs it:

```
CORTEX_GATE=1 npm run test:e2e   ->  REAL EXIT=0,  95 passed (22.7s)
```

That one is honest — Playwright started and owned its own server. Recorded in full because the near
miss is the lesson: I was one step from writing off a working gate using evidence the gate had already
been hardened against.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-18 (morning) — the rebuild landed, and Phase 4 turned out to be mostly built already

**Loop iteration after all four overnight PRs merged. The headline is a plan correction, not a fix:
the work this document called "NOT STARTED" has existed since July.**

### The rebuild, verified at the serving path

```
EXE FRESHNESS GATE: OK (exe at HEAD 04647f6...)
[gpu0] Pipeline ready.  serving on 127.0.0.1:8799 (lang=ckb_Arab)
[gpu1] Pipeline ready.  serving on 127.0.0.1:8799 (lang=ckb_Arab)
test pipeline::tests::wsl_7b_preflight_passes_when_server_up ... ok      (exit 0, read from a file)
verify_10 --quick: 41 kept gates, 23 PASS, 0 FAIL, 18 skipped (env/not-built)
```

**PR #67 verified live, not just in a unit test.** The app was relaunched with
`CORTEX_7B_SERVER_SCRIPT` deliberately EMPTY — the exact condition that made supervision fail every
~8 minutes for two hours — and it resolved the script itself, from the repo layout beside the exe.
Champion up in **101 seconds**, both GPUs, `ckb_Arab`.

### A preflight failure that was NOT a defect

`champion-7b-preflight` first returned `E_ASR_7B_UNAVAILABLE` even though port 8799 had been probed
open minutes earlier. Chased rather than re-run until green. Two hypotheses died on contact with
evidence:

* *"the test suite stomped live settings"* — **wrong**: `settings.json` untouched since 2026-08-17
  17:04, `champion_supervision_enabled` still true.
* *"the champion crashed"* — **wrong**: no traceback anywhere in the server log.

The watchdog log had it: `session expected but app not running - relaunching [clean exit (orderly
exit 2026-08-18T08:23:54.390Z)]`. The APP exited cleanly; supervision then killed the child it owned
(the standalone kill in the `!enabled && was_enabled` branch), and the watchdog relaunched 3.5 minutes
later. The preflight ran inside that window and measured an app that was not there. Re-run against the
live server: **PASS**.

**What I still do not know, and will not invent: why the app exited cleanly at 08:23:54.** No crash,
no panic — an orderly exit means something asked it to close. Recorded as unexplained. Also a number
worth keeping: the watchdog took **3.5 minutes** to notice, and every import in that window would have
failed the champion preflight.

### Phase 4 was already built — verified in code and on the live DB

This document said "Phase 4 — registry + rollback: NOT STARTED". False, and a run that believed it
would have rebuilt 53 KB of working code.

`src-tauri/src/registry.rs` has existed since 2026-07-22 with `register_candidate`,
`promote_to_champion`, `gate_and_promote`, `decide_promotion`, `record_eval_result`,
`hash_checkpoint`, `import_checkpoint`, `sync_champion_pointer`. Promotion is already ONE transaction
demoting the incumbent to `rolled_back` (registry.rs:101-128); migration v23 pins the status
vocabulary and a partial unique index for one champion per family; `sync_champion_pointer` already
runs at startup (lib.rs:519-522).

**The real remainder is three things, and only these:**

```
model_versions   0 row(s)
adapters         0 row(s)
champion.json    {"champions": {}}
engine_runtime.rs:132  model_dir = env("CORTEX_7B_MODEL_DIR")
                         .unwrap_or("/home/ai/cortex_champion_model")
verify_10.py     grep promotion-drill -> zero hits
```

a. The registry is **empty and the serving path ignores it** — the champion serving right now is not a
   registry row, so promoting would change `champion.json` without changing what serves.
b. No rollback **drill**: `rolled_back` is a status the code writes; nothing proves a rollback restores
   byte-identical serving.
c. No `promotion-drill` gate in `verify_10.py`.

Because the registry is empty, wiring `start_child` to read it is a NO-OP today — which makes it the
safe first step. It still changes what CAN serve, and champion supremacy is canon precisely because a
weaker engine once silently drafted 494/494 clips, so it goes to the owner with its design first
rather than landing unasked.

### Operational

GitHub **GraphQL** hit its rate limit (0 remaining; REST still had 4,935) because the overnight PR
monitors used `gh pr view` / `gh pr checks`, which are GraphQL. Polling loops must use `gh api`.

The exe bakes `04647f6` and the docs PR merged after it, so `check_exe_freshness` reads
`EXE IS NOT HEAD` from `main`. The binary is correct — docs do not enter it — but that is stated
rather than left looking like a stale build.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-18 (midday) — the watchdog was Ready and doing nothing, and §5 is now fully owner-gated

### The defect every existing gate called healthy

`CortexWatchdog` reported `Ready`, and both `test_watchdog_enabled.py` and `check_supervision_live.py`
only ever read that State. Meanwhile the LIVE scheduled task had drifted from the script that defines
it:

```
StartWhenAvailable = False        (cortex-watchdog.ps1 registers it True)
ExecutionTimeLimit = PT72H        (the script registers unlimited)
trigger            = TimeTrigger  (the script registers AtLogOn + repetition)
```

Windows DROPS a repetition occurrence that comes due while the machine is asleep unless
`StartWhenAvailable` is set — it does not run it late. So the one thing that restarts the app was
silently inert across every sleep.

**Measured from `watchdog.log`, which is what turned a hunch into a defect:**

```
clean-exit relaunches : 19
median downtime       : ~8 min
worst                 : 9 h 18 m   (exit 2026-08-13 19:23 UTC, detected 2026-08-14 07:42 local)
```

That is not merely failed imports. **The couch review server lives inside the app**, so each of those
windows is every reviewer link DEAD — and gate B moves on human labels and nothing else. A 9-hour
reviewer outage is the most expensive thing found in two days of this loop.

Proven against real machine state, both directions:

```
before:  SUPERVISION GATE: FAIL   (exit 1) — StartWhenAvailable is FALSE ...
after :  SUPERVISION GATE: OK (watchdog enabled, 8 reviewer link(s) answering on 8737)   (exit 0)
```

Three pins added to the pure core so CI covers the decision on Linux: the drift fails, an UNREADABLE
flag (`None` — no PowerShell, not Windows) is NOT a failure so the gate degrades rather than
false-alarms, and a Disabled watchdog still outranks the drift so the worse problem is the one
reported. With the arm disabled all three fail (`AssertionError: []`).

The live task was repaired with `Set-ScheduledTask`. **`-Register` needs elevation** (`Access is
denied`), so the gate message names the re-register command rather than pretending an agent can do it.

### Chased and cleared: the 446 / 447 verified count

The startup log reads `Restored session: 15905 segments, 446 verified` while the DB reads 447.
Chased because this repo has a documented history of count bugs (6x fixed). It is NOT one:

```
distinct (typeof, value) of `verified`:  integer 0 -> 15458 ,  integer 1 -> 447
app query SUM(CASE WHEN verified ...) : 447
my query  COUNT(*) WHERE verified=1   : 447          <- the two predicates AGREE
446 is stable across every restart since 2026-08-17 22:58 — nothing decreased
```

The line comes from `serde_json::from_str::<SessionState>(&json)` in `session/mod.rs:225` — a
persisted session file replayed verbatim, not a DB read. So it is a startup breadcrumb that can lag,
and `session/mod.rs:62-64` already says compute_stats owns the figure the UI shows. **No corpus
problem, no data loss, no fix made** — recorded so the next run does not re-chase it.

### §5 is now fully owner-gated — the honest stopping condition

Per §8, every remaining item in the work queue needs the owner, not another agent iteration:

| §5 item | State |
|---|---|
| 1. wire gates C/D/E | C and D done; **E needs Phase 4** |
| 2. Phase 4 registry | registry EXISTS (see the 2026-08-18 morning entry); wiring the serving path and registering the live champion are **owner decisions** — they touch the model lock and champion supremacy |
| 3. canary run (gate D) | **blocked: no trainer exists**; building one is a compute + model-handling call |
| 4. Phase 1 throughput | the doctrine's own answer is STOP importing — more audio cannot move gate B |
| 5. speaker-disjoint holdout | needs newly REVIEWED recordings, i.e. human review |

What moved the needle in this loop was never the queue: five real defects (quadratic export, the
reviewer Accept-lockout, the sticky audio error, the champion's env-var dependency, and this watchdog
drift) all came from measuring the live machine rather than from the plan.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-18 (afternoon) — durability verified end to end, and no defect found

A sweep that found NOTHING is recorded here on purpose: the next run should not re-verify this from
scratch, and "we assume the backups are fine" is exactly the belief this project does not get to hold.

The corpus's last line of defence, measured rather than assumed — a plain file copy once lost the WAL
tail on this repo (74/22 instead of 77/23), so both copies were opened and queried, not just listed:

```
                       primary (C:)                    off-drive (F:\cortex-backups)
cadence                every 10 min, exact              written the same minute
integrity_check        ok                               ok
segments/verified/labeled  15905 / 447 / 429            15905 / 447 / 429
rotation               20 dirs, 1.4 GB (+ pinned/)      11 dirs, 969 MB
```

Both match the LIVE database exactly, so the WAL tail is captured (`VACUUM INTO`, not a file copy) and
the off-drive copy is genuinely restorable rather than merely present. `backup_second_dir` =
`F:\cortex-backups`, and F: has 2.4 TB free, so the second copy is not about to stop silently.

**No defect found in this sweep.** Areas now measured and healthy: the export path, both review paths
(desktop and couch), champion supervision and its script resolution, the watchdog task's live
settings, corpus counts, and durability. The five defects this loop DID find all came from measuring
the live machine, and that well is dry for now.

### The loop stopped here, per §8

Every remaining item in §5 needs the owner rather than another agent iteration — Phase 4's serving-path
wiring and registering the live champion (both touch the model lock and champion supremacy), the
challenger trainer that gate D is blocked on, and gate B's ~10,000 human labels. Continuing to iterate
would be motion without progress, which §8 says to stop rather than dress up.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-18 (afternoon) — integrating the Codex flywheel range

Cherry-picked `0fbb85f` + `4b496d1` from `cortex-speech-codex-flywheel` onto `origin/main` as
`integrate/codex-flywheel` (PR #71). **36 files, +10,715 / −1,150.** Not merged.

### The merge is mechanically clean, and the handoff's premise was wrong

The handoff said the primary repo "remains unchanged at 282250d, with no Claude process or integration
commits detected". `282250d` IS a Claude commit (the watchdog ledger), and primary was at `040d8ae`
with #64–#69 merged. More usefully: the Codex base `4b0d597` is PR #66, which lands AFTER #64, #65 and
#67 — so the range already CONTAINS the pack-export rewrite, the review fixes and the champion-script
fallback rather than conflicting with them.

```
codex range files: 37      primary-since-base files: 4
OVERLAP: (empty)
git diff 4b0d597..4b496d1 -- '*/migrations/*'  ->  0 lines
```

Zero conflicts, zero migration collision. `cargo fmt`, `svelte-check` (448 files), `eslint` clean;
275 frontend tests pass.

### A false red I raised and withdrew

I first reported **5 Rust failures in champion-supremacy territory**. That was my own artifact:
`CARGO_TARGET_DIR` pointed at a scratch dir under `AppData/Local/Temp`, and `resolve_wsl_7b_client`
finds the bundled client by walking exe-relative hops (`../../../../scripts/cortex_7b_client.py`)
that reach the repo from `target/debug/deps` and reach nothing from a temp dir. Re-run in the repo's
own target dir: **1250 passed, 0 failed**. Champion supremacy is not broken by the range. Withdrawn on
the PR rather than left standing.

### Three real defects found and fixed

1. **UI freeze** — `start_champion_engine` had become a sync `#[tauri::command]` running
   `restart_current_champion` inline: tree-kill the held child and spawn a wsl.exe loading ~30 GB, all
   on the UI thread, while its own doc comment still said "returns immediately". Now `async` +
   `run_blocking`, matching the sibling command.
2. **Hygiene** — a WSL-mounted per-user profile path hardcoded in a tracked file of a public repo.
   (The gate then caught my explanatory comment for containing the forbidden pattern. Correct of it.)
3. **An aliased-temp-dir test failure** on macOS AND Windows CI that passed locally.
   `test_atomic_replace_failure_leaves_no_partial_output` compared `_atomic_publish`'s RESOLVED
   `destination` against the UNRESOLVED `out`; wherever the temp dir is an alias the inner
   AssertionError masks the injected OSError. Reproduced locally with a Windows 8.3 short path:

   ```
   alias: C:\...\Temp\CO968B~1
   OLD  destination == out          -> False   <- the CI failure
   NEW  destination == resolved_out -> True
   ```

   **I first called this an ordering bug** (`temporary = None` after a call that can throw). Wrong —
   the `finally` unlinks the temp file correctly and no partial output was ever left behind. Recorded
   because the wrong diagnosis was published on the PR before the right one.

### Three stale pins left RED on purpose

| pin | pins on | replaced by |
|---|---|---|
| `test_agentic_pipeline_policy` | literal `"omniasr-wsl-7b"` | dynamic deployment identity |
| `test_rust_runtime_panic_policy` | `probe_wsl_7b_server` | exact-identity health check |
| `test_champion_supremacy_policy` | `CHAMPION_MODEL_ID` in the filter | per-row `model_version_id` |

One architectural change, three greppable pins — the documented failure mode here (a changed symbol
breaks a pin, and the pin must be fixed in the same commit). Editing a pin to match new code is how a
guarantee gets silently dropped, so they stay red.

**The owner ruling they wait on:** the new filter keeps hypotheses matching the segment's own
persisted `model_version_id` and returns NONE when provenance is absent. Stale 300M/1B/MMS/Scribe
votes are still discarded, and votes from a *different champion deployment* now are too — stronger
than a fixed string. But a row drafted by a weaker engine BEFORE WSL7B was selected now surfaces that
engine's hypothesis, where the old code showed none. Honest provenance, but it relaxes "in champion
mode you only ever see champion output" — the rule that exists because 494/494 clips were once
silently drafted by a weaker engine.

### Not done, not claimed

No release-profile clippy run by me, no e2e, no fresh EXE/MSI, no exact-HEAD sweep. The live champion,
database and registry were not touched. The WebView2 verification of the desktop review fix remains
UNFINISHED: the harness runs, reaches Review & Correct, and correctly refuses to pass without a
working positive control — but the component's own `play()` path never fired, so it proves nothing yet.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-18 (late afternoon) — the pin chain, and the one that was not a pin

PR #71 completed: **82/82 python policies, Rust 1250 passed / 0 failed, frontend 275 passed**, fmt /
typecheck / lint clean. Six stale assertions across two files, every one from a SINGLE architectural
change (fixed champion string -> content-addressed identity).

### Each guarantee traced to its new home BEFORE its assertion was touched

| stale assertion | where the guarantee actually moved |
|---|---|
| `probe_wsl_7b_server` reaping | BOTH surviving loops: `pipeline.rs` still reaps via `kill_and_reap_wsl_child` (>=3 sites — cancel, timeout, failure), and `engine_runtime::terminate_and_reap` closes the Job Object then BREAKS every not-yet-exited arm into an unconditional `kill` + `wait` |
| `Ok((raw, _))` blank guard | INTACT — `parse_wsl_segment_result` merely returns a struct now |
| `"omniasr-wsl-7b"` literal | `registry::champion_identity` binding, PLUS a new assertion that a mismatched `result.model_version_id` is refused |
| `external_asr_script_path()` | `resolve_wsl_7b_client(...)` — the configured path is one INPUT to resolution; a bundled client also counts |
| 2 renamed regression tests | their current names, each confirmed to cover the same scenario (`pipeline_tests.rs:451`, `:656` with segment id `"preflight-refused"`) |

**Not one was relaxed.** The reap gate now guards two loops where it guarded one; the provenance gate
gained an assertion it did not have.

### The find that justified refusing to batch-update

`test_wsl_refinement_loop_refuses_blank_draft` looked like another renamed-symbol pin. It is the
**blank-transcript-never-overwrites-good** guard — the data-loss bug already fixed twice in this repo.
A batch update of "obviously stale" pins had a one-in-six chance of blessing its removal. It survived
the range; that was established by READING THE CODE, not by trusting the pin.

Both critical pins were then proven non-vacuous by deleting the guarantee and watching them fail:

```
terminate_and_reap must end in an unconditional kill + wait; without it a launcher that
never reports exit is leaked

the WSL-7B refinement loop does not guard a blank draft before the atomic champion commit
— an empty 7B result would overwrite a good transcript with ""
```

### Seven defects found in the incoming range

UI freeze (`start_champion_engine` running a ~30 GB server start inline on the UI thread), a hardcoded
per-user profile path in a tracked file of a public repo, an aliased-temp-dir test failure (reproduced
locally with a Windows 8.3 short path), a stale bundle pin, **the champion-supremacy relaxation**, and
two of my own (a swallowed registry-read error, and a pin my 4-arg signature broke).

### Two false alarms I raised and withdrew

* **"5 Rust failures in champion-supremacy territory"** — my `CARGO_TARGET_DIR` pointed at a scratch
  dir, where `resolve_wsl_7b_client`'s exe-relative hops reach nothing. In the repo's target dir:
  1250/0.
* **"an ordering bug in `_atomic_publish`"** — the `finally` unlinks correctly; the real cause was a
  resolved-vs-unresolved path comparison in its TEST.

A third was avoided by checking timestamps: a "Windows Release Gate failed" event fired 26 seconds
AFTER the new run started — a stale signal for the previous head, not a failure of this work.

### Standing assessment, unchanged

This is infrastructure for a flywheel that cannot turn yet: no trainer exists, the registry holds 0
rows, and the last snapshot's test split was 4 clips. Gate B needs **~10,000 human labels** and nothing
technical is in the way. The live champion, database and registry were untouched throughout.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-18 (evening) — the review that found what my review missed

Codex reviewed PR #71 and returned **Request changes**. I verified every finding against the code and
the live database before agreeing with any of it. Six of six check out. One is release-blocking, and
it is worse than reported.

### P0 — the challenger could never have trained on this corpus

`train_challenger.py` admitted only `decision` in `{None, "accept"}`. `eval.rs` exports the review
decision verbatim, and `edit` is what a reviewer produces when they read the champion draft and type
the correct text. Measured on the live DB, not remembered:

```
edit    241        accept  188        reject   18 (already excluded by grade)      <null>  15458
```

**241 of 429 usable labels — 56% — were classified "non-training".** And pack problems are fail-closed
(`if pack_problems: return 2`), so this never merely dropped those rows: **a single edited row refused
the entire run.** Gate D was unreachable for a reason nobody had measured.

Fixed by mirroring the Rust authority, `quality::is_human_rejected` — only a human REJECTION is
non-training. Fail-before, with the old validator restored:

```
AssertionError: human corrections are training data, not contamination:
["finetune_manifest.jsonl:1 carries non-training decision 'edit'",
 "finetune_manifest.jsonl:2 carries non-training decision 'human_edit'"]
```

Two tests, both directions, so widening the set cannot become a hole. **82/82 python policies pass.**
The line came from `b0c7f7a` — inside the range I integrated, reviewed, and reported seven defects on
without catching this one. Every fixture in the cycle policy used `"accept"`.

### Gate B: I have been quoting the wrong bar

I repeatedly told the owner "~10,000 labels and nothing technical is in the way". The count is roughly
right and the framing was wrong. `docs/LOOP_TO_10.md` gate B is **≥ 25 h labeled, top-1 recording
≤ 30 %, ≥ 25 labeled recordings** — diversity is mandatory, not a bonus. Measured now:

```
labeled clips      : 429
labeled duration   : 1.09 h  (65.1 min)      needs >= 25 h
labeled recordings : 5                       needs >= 25
top-1 share        : 94.5%                   needs <= 30%
mean clip length   : 9.1 s
```

At 9.1 s, 25 h is ~9,890 clips — so the number was fine, but **10,000 more Lamofull clips would leave
top-1 near 99 % and recordings at 5, and gate B would still fail.** Review order is the diversity
lever, and the ledger now says so.

### The WebView2 proof, recorded where it survives the session

Codex correctly noted the PR-head ledger called this unfinished and no artifact backed it. It has now
run, with a working positive control that proves the harness can SEE a genuine failure:

```
clip label: "بەش 126/429" -> "بەش 110/429"
positive control saw the playback toast: true
positive control OK — a real failure IS reported and IS visible to this harness
stalling play() so it stays pending; driving 8 rapid advances...
playback toast during the advance race: no
RESULT: PASS
```

Four earlier runs were vacuous and I reported none of them as a pass: the on-disk key is
`autoplay_segments` (snake_case), and the camelCase name is the serde-renamed FRONTEND view, so
autoplay stayed off and no `play()` was ever pending.

### Where I would not go as far as the review

* **No promotion initiation path** — confirmed, zero non-test callers of `start_promotion`. But
  `champion_promotion_runtime.rs` opens by saying so deliberately: recovery only, because "until
  promotion evidence is backend-minted and owner-authorized, 'no initiation surface' is the only
  honest trust boundary". A documented decision, not an oversight. Gate E is still blocked either way.
* **Two IPC writes in `ReviewMode`** — confirmed as fact. The ordering is deliberate and the partial
  state is the SAFE one: `recordHumanDecision` runs first and validates, so a failure in
  `updateSegment` leaves the clip UNVERIFIED and still in the queue, where re-review self-heals it.
  Worth closing; not a data-loss hazard today.

### Standing correction

Green CI on PR #71 means `verify_10.py --static`. It has never run gates C/D/E live, and I let "all
four checks green" carry more weight than it earns. The live champion, database and registry were
untouched throughout.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-18 (evening) — gate D turns for the first time, and stops where the corpus stops

The flywheel's train step had **no compatible trainer at all**. `train_challenger.py` hands an
external trainer `--manifest --out --base --contract` and refuses to say "trained" without hashed
consumption evidence; the real trainer (`train_omni_7b.py`, fairseq2 + PEFT LoRA, DDP under WSL)
speaks `--train_jsonl`/`--output_dir` and emits none of it. `scripts/omni7b_trainer_adapter.py` is
the bridge. It attests only what it can prove: rows consumed parsed from the TRAINER'S OWN stdout,
a recipe hash over the trainer file plus hyperparameters, an environment hash over the measured
toolchain, and a seed applied by a launcher shim (the trainer has no `--seed`, and a number that
steers nothing is decoration).

### The measured result

**A challenger trained on 403 of the owner's corrected labels beats the champion.** Same 348 frozen
FLEURS ckb clips, same manifest bytes (`ed7130755aaf14e1`), same normalization basis:

```
                     CER                          WER
champion    7.91%  [6.83, 9.15]        33.97%  [32.23, 35.78]
challenger  7.56%  [6.32, 9.01]        28.22%  [26.35, 30.18]

MAPSSWE word: z = +7.81   p = 5.887e-15   N=348  -> challenger better  (SIGNIFICANT)
MAPSSWE char: z = +0.88   p = 3.769e-01   N=348  -> challenger better  (NOT significant)
```

**The WER gain is significant; the CER gain is not.** Both are reported because reporting only the
significant one would be a lie by selection. Honest limits: one epoch, 403 clips, ~94 % of the
labeled duration from a single recording, evaluated out of domain on FLEURS. This proves the
machinery and the direction — not a promotable model.

The training set was 403 rows of which **235 were `edit` rows** — the human corrections the P0
validator was rejecting outright. Those corrections are what produced the gain. Before this
morning's fix the run could not have happened: the real pack went from **241 preflight problems to
0** once `edit` was admitted, the 241 − 6 difference being exactly the 235 edits.

### Gate D, step by step

| step | state | evidence |
|---|---|---|
| snapshot | **PASS** | sealed `23db46a0…`, pack verified 0 problems |
| train | **PASS** | `CHALLENGER: TRAINED + VERIFIED`; `check_challenger_loop.audit_run` -> **NONE, fully bound** |
| eval | **PASS** | both scorecards above, paired MAPSSWE |
| verdict | **BLOCKED** | see below — not a tooling gap |

Challenger identity, served through the real production path on GPU 1:

```
model_id  omniasr-7b-challenger-eb0105fdb6a57fdcc5b568ff
adapter   79,768,688 B  6f938f78…   (incumbent: same size, c348ade8… — same LoRA shape, new weights)
[gpu1] LoRA applied. Pipeline ready. serving on 127.0.0.1:8798
```

### Why the verdict is corpus-blocked, measured not inferred

```
EVAL SLICES: REFUSED - 348 manifest row(s) did not resolve to exactly one library segment
(first: [0, 1, 2, 3, 4]); zero protected slices were produced
```

`promotion_gate.py` mandates manifest-bound protected slices; `build_eval_slices.py` builds them only
from eval rows resolvable to live library segments. FLEURS is external and deliberately never
imported — and importing it is **not** a legitimate workaround, because the pack selects training
rows from the library, so it would risk exactly the train/test leak the split policy prevents. The
library-resident alternative is **11 held-out clips across 4 recordings (7/2/1/1)** against a
>= 5-per-group floor.

**Gate D's verdict is therefore gated on gate B.** Labels are not merely the largest workload; they
are the prerequisite that makes the promotion machinery exercisable at all.

Also missing and now written: nothing generated the scorecard provenance sidecar the gate requires —
only the gate that consumes it. `scripts/emit_scorecard_provenance.py` derives the challenger's
fields through the gate's OWN `load_challenger_run`, so the sidecar cannot disagree with what the
gate re-verifies.

### The champion cannot currently survive a restart

Found while restoring the GPUs, and more serious than the thing it interrupted. The on-disk
`cortex_7b_server.py` refuses to start without a schema-2 champion pointer:

```
FATAL: refusing to serve an unverified champion deployment:
no champion pointer configured / unsupported champion pointer schema
```

The live `champion.json` is `{"champions": {}, "schema": 1}`, and the RUNNING server was started
from an older code path (`CORTEX_7B_MODEL_DIR=/home/ai/cortex_champion_model`, no pointer). So the
champion serves fine while it lives, but **a reboot, a watchdog relaunch, or any restart would not
bring it back**. This is pre-existing and independent of this work; it is Codex's finding #2
confirmed empirically. All five hashes `bootstrap_legacy_champion.py` pins verify against the files
on disk, so the incumbent IS reconstructible — installing that pointer is an owner-gated deployment
and was not done.

### Operating discipline

The canary ran beside a live champion with **zero reviewer downtime**. GPU 1 was freed by stopping
only its worker: the parent deliberately does not respawn (`SERVING DEGRADED on 1 of 2 replica(s)`),
so no app restart and no couch interruption. Verified mid-operation, not merely by port — the
champion transcribed real clips at 7.49 % CER while the challenger trained on the other card. WSL
reports GPU memory through `vmwp`; `nvidia-smi` per-GPU figures do not track WSL allocations at all
(holding 14 GiB moved it by 0 MiB), so the split was confirmed with a deliberate allocation probe.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-18 (night) — the champion restore, and the outage it took to find the real cause

The canary had left the champion on one GPU (its second worker was stopped to free GPU 1). Restoring
it surfaced a latent fault far more serious than the missing card: **the champion could not restart
at all.**

```
FATAL: refusing to serve an unverified champion deployment:
       no champion pointer configured / champion pointer omniasr-7b must be an object
```

The running server had been started by an older exe through the retired `CORTEX_7B_MODEL_DIR` path
(read directly from `/proc/<pid>/environ`), while the working-tree `cortex_7b_server.py` now requires
a schema-2 champion pointer — and the live `champion.json` was `{"champions": {}, "schema": 1}`. So
the champion served fine while it lived, but a reboot, a watchdog relaunch, or any restart would not
have brought it back. Pre-existing, and independent of the canary.

### What actually fixed it — registration, not a pointer file

Writing a correct `champion.json` by hand was **not** enough: the app regenerates that file from its
registry on startup (`registry::sync_champion_pointer` selects `model_versions WHERE status =
'champion'`), and the registry held **0 rows**, so it promptly overwrote the hand-written pointer
with an empty one. This is Codex's finding #2 confirmed exactly: the legacy deployment had to be
manifest-generated, verified **and registered**.

The full sequence that worked:

1. `bootstrap_legacy_champion.py` run **inside WSL** — over the 9P share `os.replace` fails with
   `WinError 50 (The request is not supported)`, so the atomic publish cannot happen from Windows.
   All five pinned hashes verified against the files on disk.
   -> `omniasr-7b-legacy-c348ade8a816`, deployment `ae33143ec8b25f45`
2. Pointer resolution proven through the server's OWN `load_champion_pointer` before any restart —
   no 31 GB model load needed to de-risk it.
3. The incumbent registered in `model_versions` (status `champion`).
4. Frontend + `cargo build --release` from PR head (7m 42s), then restart.

Result — both cards, with a verified deployment identity the champion never had before:

```
Devices: GPUs [0, 1] — pre-forking one worker process per GPU
[gpu0] LoRA applied. Pipeline ready. serving on 127.0.0.1:8799
[gpu1] LoRA applied. Pipeline ready. serving on 127.0.0.1:8799
       model=omniasr-7b-legacy-c348ade8a816  deployment=ae33143ec8b25f45
16,424 MB phys_0 | 16,410 MB phys_1 | ports 8737 + 8799 LISTENING | watchdog Ready
```

Verified by real transcription, not by a listening socket.

### The outage, stated plainly

Restarting the app before registering the incumbent took the champion **down for roughly ten
minutes** (18:53 to 19:04). No reviewer was active (0 decisions in the preceding 3 h), and the couch
server returned with the app, but it was a real outage and it was mine: I had proven the pointer
loaded and did not anticipate that the app would rewrite it from an empty registry. The correct
order — register first, restart second — is now recorded above.

### A check that proved nothing

An earlier claim that the old exe "contains CORTEX_7B_CHAMPION_POINTER -> 0 occurrences" used
`strings`, which returns 0 for **every** string in this environment, including ones certainly present
(`cortex_7b_server.py`, `CORTEX_7B_PORT`). Re-done with `grep -a`, the rebuilt exe carries
`CORTEX_7B_CHAMPION_POINTER` (1) and no longer carries `CORTEX_7B_MODEL_DIR` (0). The original
conclusion held only because it rested on `/proc/<pid>/environ`, which was direct evidence; the
`strings` line was decoration that could have been wrong in either direction.

A second near-miss: `tail -f` on the append-only champion log replayed the FATAL from the FIRST
failed restart, which read as a fresh failure of the fix. Timestamps (log mtime 18:53:49, well before
the current start) showed it was stale — the same "check the timestamp before chasing it" lesson as
the CI event on 2026-08-18.

Live state changed by this work, deliberately and with owner approval: `model_versions` gained the
incumbent row, `champion.json` is now a valid schema-2 pointer (previous contents backed up to
`champion.json.bak-before-restore-2026-08-18`), `/home/ai/cortex_champion_model/deployment_manifest.json`
was created, and the app exe was rebuilt from PR head.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-19 — the accept that lied, and the backup that was a database and nothing else

Two defects found by taking the certification brief's Phase 1 and Phase 4 literally. Both were
invisible to every existing gate, and both were found by reading the LIVE system rather than the code.

### An accept that claimed the champion wrote a human's words

`check_review_serving_provenance` reds on segment `82681df2-5ec9-4ef3-8ef0-e3ec968159c9`. Its
append-only history:

```
Reviewer G skip -> Reviewer A edit -> Reviewer G edit -> Reviewer E edit -> Reviewer A edit -> Reviewer D edit -> Reviewer H ACCEPT
```

Five humans corrected the clip; the sixth pressed Accept on what they saw. `accept` asserts something
checkable — an ASR engine produced this exact text and a human approved it unchanged — and the
accepted text carries a name and punctuation present in NONE of the segment's four hypotheses.

Classification now happens in the backend at the one choke point every path converges on, from the
stored bytes rather than the renderer's word: matches a hypothesis -> `accept`; matches none ->
`edit` (human-authored); **no hypotheses on file at all -> unchanged.** That last case was a bug in
the first draft — absence of recorded provenance is not evidence of human authorship, and
reclassifying there launders in the opposite direction. It broke an existing pack-provenance test,
and the test was right.

### The off-drive backup held the database alone

`take_snapshot_at_from` read `EXTRA_STATE` (settings.json, champion.json) from the DESTINATION. For a
local snapshot destination == primary, so it worked and every test passed. Off-drive they are
different disks. Measured on the live system:

```
F:\cortex-backups\snapshots\snapshot_1787091336: ['cortex-speech.db']          <- 11 of 11 trees
%APPDATA%\cortex-speech\snapshots\snapshot_1787091336: ['champion.json', 'cortex-speech.db', 'settings.json']
```

Worse than missing metadata: a restore from that tree returns **no champion pointer**, and the 7B
server refuses to start without one — the disaster backup would resurrect a library that cannot
transcribe. Fail-before, with only the fixed line reverted:

```
panicked: settings.json is missing from the off-drive snapshot — a restore from it could not
recover the champion pointer or the owner's settings
```

Added alongside: a canonical `SNAPSHOT_MANIFEST.json` (size + SHA-256 per file, written into staging
before the promoting rename, so a promoted snapshot always has one), and `scripts/restore_drill.py`,
which restores into a DISPOSABLE profile and checks quick_check, foreign keys, migration version, row
counts and champion identity. It carries its own negative control:

```
PRIMARY  : schema 54, {'speech_segments': 15905, 'review_events': 646, 'spot_checks': 31,
           'model_versions': 1}, champion omniasr-7b-legacy-c348ade8a816 — 1 problem (no manifest;
           these trees predate the manifest code)
OFFSITE  : correctly REFUSED (4 problems, incl. both missing state files) -- --expect-fail
```

### Two of my own checks that proved nothing

* `PRAGMA user_version` reported 0 and read as "migrations did not travel". The app tracks migrations
  in `schema_migrations`; the snapshot was fine and the drill was wrong. Corrected to read what the
  app reads — version 54.
* `PRAGMA synchronous` read 2 (FULL) from a fresh Python connection. The pragma is PER-CONNECTION, so
  that measured my own default, not the app's. `db.rs` sets NORMAL; the audit's durability finding
  stands and is NOT yet fixed.

Rust 1260 passed / 0 failed, clippy 0 diagnostics, 85/85 python policies. No live data was modified:
the violating row's repair is deliberately deferred until the offsite fix is in the running binary
and a validated restore exists, per the brief's own ordering.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-19 (cont.) — three gate legs that were testing the environment, and a certified binary

Sweep moved **36 PASS / 5 FAIL -> 38 PASS / 3 FAIL** on an idle machine.

`bench-budget` now PASSES. Its earlier failure was measured while a challenger was training and two
scorecards were running on both GPUs — my own concurrency, exactly the trap recorded in
[[sweeps-need-an-idle-machine]]. It was reported as "likely my artifact" at the time rather than as a
regression, and that held.

### The three legs

* **snapshot-immutability** — failed on the pre-flywheel `challenger_canary_01` (prepared-only, never
  trained, no pack binding). Excluding it was the easy and wrong fix. The gate now honours an
  EXPLICIT, reasoned `evidence_status` with a `superseded_reason`, PRINTS it every run, and still
  fails both a record that merely lacks the hash and an invalidation with no reason — otherwise the
  marker becomes a way to silence any inconvenient run.
* **ignored-real-model** — `champion_selected_refuses_to_downgrade_to_the_finetuned_model` depended on
  the machine having no 7B client script, so it failed the moment the real champion was running. Fixed
  that, and it then failed with "Segment not found in database": the pipeline resolves a SEGMENT
  before selecting an engine, so **this test had never exercised the no-downgrade rule at all**. It
  now seeds the row and points the client at an absolute POSIX path that does not exist.
* **champion-7b-preflight** — "no such table: model_versions". The preflight compares the champion's
  EXACT identity against the registry, but the fixture DB was never migrated and held no champion, so
  the leg proved nothing about identity. It now migrates and registers the live champion.

### Durability, measured

```
MEASURED: 0.6 ms per durable human decision (n=40)
```

The decision commit alone escalates to `synchronous=FULL`. SQLite refuses the pragma inside a
transaction ("Safety level may not be changed inside a transaction" — found by running it), so it
happens before the write opens and drops back after. Every other write here is reproducible; a human
decision is not.

### The binary is now identity-certified

```
commit f7c34d1f75dfb26f8d377910021c58b32b647f04   tree 22d44d5e74bf505065769d012a206e9cfe363a32
exe    498b5b8334bf2be99f047652acfb246567fcbf63e71a341bc6784c832dbb4432  (baked commit VERIFIED)
sbom   1171 components  2c0e8527b590723f          signature UNSIGNED (owner-descoped 2026-07-10)
```

The MSI was NOT built: it fails with `os error 32` because the running app holds
`models/onnxruntime.dll`, and distribution is owner-descoped. Taking the champion down for a descoped
artifact was not worth it; the reason is recorded in `artifacts/release_provenance.json` rather than
omitted.

### Checks of mine that proved nothing (four this session)

`strings` (returns 0 for every string here), `PRAGMA user_version` (the app uses `schema_migrations`),
`PRAGMA synchronous` from a fresh connection (per-connection, so it measured my own default), and a
clippy run without `-D warnings` **from cache** that printed nothing and was reported as "0
diagnostics" — that one reached CI as a real dead-code failure. A check that cannot fail is not a
check; each of these was caught and corrected rather than reported.

Also self-inflicted and caught: I removed a `drop(db)` from an unrelated test by matching an
identical three-line block, and a `| tail` masked a FAILED MSI build as exit 0.

**GO_LINKS: NO** (Phase 3 playback evidence untouched; Phase 8 unstarted).
**GO_MODEL_PROMOTION: NO** (Gate B: 1.09 h of 25, 5 recordings of 25, top-1 94.5 %).

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-19 (late) — Phase 3: proving a clip was HEARD, not merely loaded

The decision surfaces gated on `audioError` — the ABSENCE of a failure, which is not the presence of
listening. A clip that loaded fine and was never played was indistinguishable from one a reviewer
listened to twice, and nothing in the row said which. For a verbatim corpus that is the difference
between a label and a guess.

Landed this session:

* **migration 55 / `playback_receipts`** — keyed by (segment, revision), bound to the audio
  fingerprint, storing cumulative media time, coverage and the policy version that judged it.
* **`has_sufficient_playback_evidence` / `require_playback_evidence`** — fail-closed with
  `E_NO_PLAYBACK_EVIDENCE`. Coverage bar 0.90: not 1.0 (a reviewer who has heard the sentence stops
  before the trailing silence, and demanding the last frame trains them to leave it running rather
  than listen), not 0.5 (half a Sorani sentence is where a plausible-but-wrong verdict comes from).
  `reject` is covered too — marking a clip bad is a judgement about AUDIO, and a wrongly rejected clip
  is silently dropped from the corpus.
* **`AudioPlayer.heardMs`** — only forward movement of a not-paused element counts; a jump beyond one
  tick is a seek, not listening; a source change resets it, so a previous clip's listen never travels.
* **IPC + ReviewMode wiring** — the receipt is posted BEFORE the verdict, and its revision and
  fingerprint are resolved SERVER-SIDE. A client that could name them could mint a receipt for a clip
  it never heard; then the guard would be comparing the client's claim with the client's claim.

18 tests across Rust and the frontend pin the rules that matter: a previous clip's listen cannot
unlock the next, different audio bytes do not count, a correction needs its own listen, an
opened-but-never-played clip is not evidence, a zero-duration clip fails closed instead of dividing
its way to 100 %, and a scrub is not a listen.

**Not yet enforcing.** The guard is proven but is not called from the decision path, because the phone
surface (`couch.html`) does not post receipts yet: switching it on first would refuse every decision
from all eight live reviewers. The running binary is unchanged.

Rust 1275 / 0, clippy `-D warnings` 0, frontend 280 tests, typecheck 448 files / 0 errors.

**GO_LINKS: NO** — couch.html receipts, guard wiring, real-device verification (WebView2 + Funnel),
Phase 8 link drill.
**GO_MODEL_PROMOTION: NO** — Gate B: 1.09 h of 25, 5 recordings of 25. ~9,800 listening decisions,
which is reviewer time and cannot be engineered.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-19 (final) — enforcement in observation, and a security finding I nearly fabricated

Phase 3 is wired end to end and now runs in OBSERVE-ONLY mode on the phone path: the guard is asked
the real question and its answer is logged, but no decision is refused.

The remaining unknown was never the rule — it is whether `timeupdate` fires often enough on a real
mobile browser through the Funnel for coverage to reach the bar. Switching to hard refusal on an
assumption about that would, if wrong, reject every decision from all eight reviewers at once.
Observation makes the LIVE SYSTEM answer it: a day of real reviewing shows whether receipts land with
sensible coverage, and only then is enforcement a one-line change backed by evidence.

Watch for `PLAYBACK_EVIDENCE_OBSERVE` in the log. Zero of them across a day of reviewing means mobile
accounting works and enforcement is safe to switch on.

### Phase 8, the part that needs no reviewer session

```
public shell            : HTTP 200, 77,816 bytes
secrets in public shell : NONE
queue    without cookie : HTTP 401 -> REFUSED
audio    without cookie : HTTP 401 -> REFUSED
decision without cookie : HTTP 401 -> REFUSED
```

The rest of Phase 8 (cookie flags, ranged-audio authorisation, idempotent replay, revocation, reissue)
needs a claim session, and a 9th reviewer cannot be created: canon caps the roster at 8 and a 9th
raises the spot-check floor to 27 keys against the 24 that exist.

### The fifth check of mine that proved nothing

The first run of those probes reported **"audio SERVED without cookie (!!)"**. False. The couch server
is HTTPS-only, plain HTTP timed out, and the probe treated a connection failure as "not refused" —
it would have been a fabricated security finding. Re-run over TLS, everything refuses correctly.

Five this session: `strings` (0 for every string), `PRAGMA user_version` (wrong migration table),
`PRAGMA synchronous` (per-connection), a CACHED clippy run without `-D warnings` (reached CI as a real
dead-code failure), and this one. **A check whose failure mode is indistinguishable from its pass mode
is not a check** — every one of these looked conclusive and was empty.

### Standing verdicts

**GO_LINKS: NO** — enforcement observing, not refusing; Phase 8 needs a claim session.
**GO_MODEL_PROMOTION: NO** — Gate B: 1.09 h of 25, 5 recordings of 25. ~9,800 listening decisions,
which is reviewer time and cannot be engineered.

Phases 1, 2, 4, 7 complete. 5 at 3-of-5 (rest terminates at Gate B). 3, 6, 8 blocked on a phone, a
corpus and a reviewer slot respectively — none of them code.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-19 (later) — the enforcement green light was vacuous, and the coverage denominator was the client's

Asked to wire enforcement "after the log shows no `PLAYBACK_EVIDENCE_OBSERVE`". The log did show none.
It was still not permission, and the reason is worth keeping.

### Why zero warnings meant nothing

```
running exe bakes                    : c6c1bee   (NOT ddeb2c3, which added the guard)
PLAYBACK_EVIDENCE_OBSERVE in the exe : 0 occurrences (grep -a)
record_playback_receipt in the exe   : present
playback_receipts rows               : 0
last phone decision                  : 2026-08-18 21:19:20   (18 events, 21:11-21:19)
running exe built                    : 2026-08-19 05:50      (8.5 h AFTER those decisions)
```

The observe guard was never deployed, so it could not warn; the receipt-minting build went live after
the last phone review, so nothing had minted a receipt. Zero and zero, and the honest reading is
"this code has never executed". Flipping to hard refusal on it would have shipped a rejection path to
all eight reviewers on no evidence at all.

`check_playback_enforcement_readiness.py` now asks whether the warning COULD have fired before
treating its absence as evidence: marker present in the binary, real decisions in the window, more
than one reviewer represented (playback ticks come from a device, not a policy), every decision
covered at the bar. Negative controls drive it against this exact shape and require NOT READY; a
positive control requires READY so it is not merely a refuser. Live verdict: **NOT READY, 2 checks
failed** — which is the true state, not a failure of the work.

### Half the guard was comparing the client with itself

The receipt resolves revision and fingerprint server-side precisely so a client cannot mint evidence
for a clip it never loaded — and then took the coverage DENOMINATOR from the client anyway. A page
reporting `heardMs: 100, clipDurationMs: 100` for a 1.5 s sentence scored 1.00 and cleared the 0.90
bar on a tenth of a second of audio.

```
before fix : receipt recorded clip_duration_ms = 100  (client's claim)
after fix  : receipt recorded clip_duration_ms = 1500 (server's row), coverage 0.067 -> REFUSED
live rows carrying a duration : 15,905 of 15,905, so the client's number is now dead in practice
```

### Phase 8 does not need a reviewer slot — the earlier entry was wrong

Recorded above: "the rest of Phase 8 needs a claim session, and a 9th reviewer cannot be created".
Five of those six items are already pinned against the REAL server on disposable in-memory profiles,
which is exactly the dedicated audit profile the brief asked for, at zero cost to the roster. Each run
individually and confirmed as `1 passed`, never a filtered-out zero:

```
cookie flags (Secure/HttpOnly/SameSite=Strict, pairing secret != session)  pairing_mints_distinct_secure_bounded_sessions
token gate on every route, two reviewers kept apart                        live_server_gates_every_route_...
a valid token in the query string is worthless                             a_valid_token_in_the_query_string_is_worthless
idempotent replay, never recorded twice                                    a_legacy_interrupted_decision_..., a_mid_session_server_restart_...
revocation, and revocation surviving a restart                             revoking_one_reviewer_..., ..._must_outlive_a_restart, durable_stop_marker_...
sleep / restart                                                            a_lease_that_lapsed_while_the_phone_slept_..., a_spot_check_served_before_a_restart_...
```

Ranged-audio authorisation needed no new test: the auth gate returns 401 before the `Range` header is
read and before any route dispatch, so a Range-specific bypass would require moving the audio route
above the gate — which the existing 401 test already catches. Checking beat writing.

### Gate B's refusal was blaming the corpus

`EVAL SLICES: REFUSED - 348 manifest row(s) did not resolve to exactly one library segment` reads as
corruption in `speech_segments`. Measured:

```
manifest rows        : 348
rows with 0 matches  : 348
rows with >1 match   : 0
manifest             : runs/eval/fleurs_ckb_iq_frozen.eval.tsv  (FLEURS — external by canon)
```

Every row, not some. FLEURS clips are not library clips and never will be, so library-derived slices
(speaker, dialect, SNR, source) cannot be built from that manifest at all. That is a question about
which eval set gets sliced — a promotion-policy decision, owner-gated — not a corruption to hunt. The
message now says so. The gate still REFUSES; nothing was weakened.

### Standing verdicts

**GO_LINKS: NO** — enforcement still observing. The precondition is now measurable rather than
assumed, and the instrument that measures it exists and currently reads NOT READY.
**GO_MODEL_PROMOTION: NO** — unchanged. Gate B: 1.09 h of 25, 5 recordings of 25.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-19 (pilot) — the two-reviewer pilot answered the device question, and found a workflow instead

Bar lowered 0.90 -> 0.85 on the owner's call. Full sweep at exact HEAD: **RED**, 40 of 41 kept gates
PASS, the one failure `challenger-loop` (one trained challenger, no linked verdict — the FLEURS eval
manifest cannot produce library-derived slices). 9 legs owner-descoped, 3 owner-gated pending, so
`CORTEX 10/10: ALL GATES GREEN` remains unreachable by the aggregator's own definition.

### The device question is answered, and the answer is clean

```
reviewer   receipts   mean coverage   min coverage
Reviewer A       22           0.981          0.932     every single one above the bar
Reviewer H       18           0.721          0.000
```

Reviewer A's second device produced 22 receipts and not one false refusal. That was the whole point of the
pilot: `timeupdate` fires on a browser, not on a policy, and one phone proved nothing about another.
Two phones now agree.

### What enforcement would actually have refused

5 of 40. One is the case the guard exists for (0.352 — a 9.8 s clip decided after 3.5 s). The other
four are a single pattern:

```
12:05:56  reject  clip 11797ms  heard 0ms
12:05:56  reject  clip  8145ms  heard 0ms
12:05:57  reject  clip  8575ms  heard 0ms
12:05:57  reject  clip  7450ms  heard 0ms
```

Four rejects in two seconds with zero playback. Not a device fault and not dishonesty — a rapid
rejection sweep, which is a real way to work. It is also indistinguishable, to the guard, from
deleting corpus without listening.

That is now an owner decision the pilot surfaced and no amount of code can settle: **does the guard
gate a REJECT?** An accept or an edit injects text and must require listening. A reject removes a
clip permanently, from a corpus whose size is already the binding blocker (1.10 h of 25). Refusing
unheard rejects protects the corpus and breaks the sweep workflow; exempting them keeps the workflow
and lets a careless reviewer silently delete clips nobody heard. Currently everything except `skip`
is gated.

### A gate of mine that manufactured the evidence it existed to refuse

`check_playback_enforcement_readiness.py` reported **0 decisions while a reviewer was mid-session**.
SQLite `datetime('now')` writes UTC and the tracing log stamps Zulu; the default window came from the
exe mtime in LOCAL time, so on this UTC+3 box it discarded the last three hours — and with them the
first genuine below-bar receipt. Seventh check of mine this session that proved nothing, and the only
one that actively misled rather than merely failing to inform. Pinned by
`test_the_default_window_is_utc_like_the_rows_it_filters`.

### Standing verdicts

**GO_PILOT: YES** (running). **GO_LINKS: NO** — 40 of 20 decisions and 2 of 2 reviewers now pass;
coverage is the last gate leg and it hangs on the reject question above, not on a defect.
**GO_MODEL_PROMOTION: NO** — unchanged. Gate B: 1.10 h of 25, 6 recordings of 25.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-19 (enforcement) — the guard now refuses, rejects included

Owner call: bar to 0.85, rejects gated, enforcement live. Deployed at HEAD `128a067`; the running exe
carries `PLAYBACK_EVIDENCE_REFUSED` and no longer carries the observe marker, verified against a
negative control that returned 0. Eight reviewer links answering, champion pointer intact, exe fresh.

A verdict on a clip played below the bar is refused with **428**. `skip` stays exempt — it writes no
verdict, and gating it would leave a reviewer who cannot hear a clip with no legal move at all.

### Two bugs the existing tests caught, both the same shape

The server's own failure was being reported as the reviewer's fault:

  * a locked database makes the evidence check unanswerable, and the first draft returned 428. Someone
    who listened properly would replay the clip and be refused again, forever. Unanswerable is now 500.
  * a receipt that could not be WRITTEN left nothing to judge, so a busy database refused honest work.
    Now 500 — and it claims the lease first, or a transient error hands the clip to the next
    reviewer's batch while the reviewer still has it open with their correction typed.

Neither was visible from the enforcement code alone; both surfaced because 34 tests went red and one
of them asserted `500, "a locked database must surface as a server error, not a verdict"`.

### The Funnel link, and why the app never had it

A reviewer lost audio mid-session and five skips silently failed to reach the server. Server healthy
(shell 0.04 s, zero ERROR lines), every audio file present, all eight queues live, public Funnel
answering in 0.05 s from outside. The app hands out a LAN URL and a tailnet URL; both die when the
phone leaves the network, and iOS suspends the VPN whenever the app is backgrounded. The one link that
still worked was the one never handed out. Now read from `tailscale serve status --json` — three
conditions or no row, because a guessed hostname resolves and then refuses.

### Gates that caught me this round

  * repo hygiene: the owner's real device hostname went into a tracked test fixture. Redacted.
  * the readiness pin: its marker string no longer existed in the source — exactly the "passes on
    silence" trap it was written for. The gate is reframed for enforcement rather than retired.
  * clippy MSRV: `is_none_or` is 1.82, the project floor is 1.81.
  * a tab had eaten `C:\Program Files\Tailscale\tailscale.exe` into `Tailscale<TAB>ailscale.exe`.

Rust 1282 passed, clippy -D warnings clean, 86 python policies, typecheck 0 errors.

### Standing verdicts

**GO_PILOT: YES.** **GO_LINKS: NO** — enforcement is live but has not yet refused a real reviewer;
the first `PLAYBACK_EVIDENCE_REFUSED` in the log is the live proof, and it has not happened yet.
**GO_MODEL_PROMOTION: NO** — Gate B unchanged: 1.10 h of 25, 6 recordings of 25.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-19 (hunt) — the brutal bug hunt: 14 agents, 8 confirmed-certain, and a deploy that would have bricked the desktop

Six specialised finders swept the codebase in parallel (read-only, while a reviewer worked live),
32 raw findings deduped to 21, the top 8 adversarially verified — a second set of agents instructed
to REFUTE each claim. All 8 survived as CONFIRMED at "certain". 1.56M tokens, 319 tool calls, 14 min.

### The one that mattered most

The staged desktop-enforcement build — already compiled, waiting only for the reviewer to finish so
it could deploy — would have refused essentially every honest desktop verdict. Clips are windows into
shared source files (403 of 414 exportable clips live in ONE recording), and both desktop surfaces
report the WHOLE file's length as the coverage denominator: an honest full listen of a 10 s clip
scored ~0.004 against the 0.85 bar. The hold-for-Reviewer A pause is the only reason it wasn't live.

### Confirmed and fixed (each failing-first where a test could be written)

1. **One front door for receipts** — db::record_playback_receipt resolves revision, fingerprint AND
   denominator from the row, overriding every caller's claims. The desktop's `path:` vs `id:`
   fingerprint deadlock died in the same stroke.
2. **Listening is personal** — the guard matched segment+revision+fingerprint only, so reviewer A's
   listen evidenced reviewer B's blind verdict. Guard, gate and requeue tool all reviewer-scoped now.
3. **heardMs never reset between same-file clips** — resetHeardTime() had ZERO external callers;
   clip B passed on clip A's minutes. Reset now fires on every clipKey change in the shared player.
4. **Undo punished listening** — the decision bumps the revision past its own receipt, so re-deciding
   a clip just heard and undone was refused 428. The undo txn carries the reviewer's best receipt
   forward; fingerprint equality is what makes that honest. Measured 428 before, 200 after; another
   reviewer still gets 428.
5. **funnel_host under the COUCH mutex with no timeout** — a wedged tailscale.exe would have frozen
   every status call and is_running(), the restore fence. Bounded 3 s, cached per process.
6. **requeue --apply rerun wiped good re-reviews** — it judged every event in the window, not the
   segment's LATEST decision. Plus the 'T'-separator fence bypass ('T' sorts after digits, so an ISO
   timestamp sailed past the lexicographic fence into an empty window).
7. **exe-freshness blind to build config** — commits touching only package-lock/vite/svelte/tsconfig
   read as "non-source" and certified a stale exe as HEAD.
8. **Desktop refusals in raw English** — E_NO_PLAYBACK_EVIDENCE shown verbatim; now review.mustListen
   in both languages (Sorani awaiting the owner's native read, with the phone's copy).

### Found, verified or plausible, NOT yet fixed — owner should know

* **Side-door verdict writers**: batch_verify, write_segment_verdict, restore_segment_snapshot and
  update_segment_fields can set verified/verdict without the listening guard. Some are legitimate
  (undo/restore, owner batch tools with their own audit trail); whether batch_verify should demand
  evidence is a policy call, not a code call.
* Outbox flush before `me` is known can 409-destroy another reviewer's queued decision (phone).
* api_decision mints the receipt at the CURRENT revision before the rowVersion CAS — a stale outbox
  replay can mint evidence at a revision the phone never saw.
* Spot-check pairs can outlive a batch-unverify, silently mis-scoring a real re-review.
* The readiness gate's ±5 s window can pair one receipt with two same-segment decisions.

### The instrument lied twice today before the hunt

Local-time window vs UTC rows (hid 3 h of live work), then current-revision matching vs
revision-bumping decisions (called nine correctly-guarded decisions unevidenced). Both times the gate
UNDER-reported the system. The hunt's verifiers were told to refute, not confirm — same lesson.

Totals after fixes: Rust 1288 passed, clippy -D warnings 0, vitest 280, 86 policy scripts, typecheck
0 errors. Gate: READY (32/32 evidenced, 2 reviewers). Deployed exe still 128a067 — rebuild pending.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-19 (voice focus) — one host's voice, found by cross-file presence, confirmed 15/15 by ear, now the whole queue

Owner goal for this phase: collect the KBHP host's voice, clean and single-speaker, for voice cloning;
guests later. Reviewer hours are the scarce resource, so the QUEUE narrows to his clips. The library
does not change: nothing relabelled, nothing deleted, and removing one data-dir file restores the
full queue on the next fetch.

### Finding him

The per-file SPEAKER_xx labels name nobody — the same person is SPEAKER_05 in one episode and
SPEAKER_04 in the next (measured across the 32 files). `host_voice_probe` embedded every single-speaker
clip with the production CAM++ service and clustered across ALL files at once:

```
clips embedded   8,753 (single-speaker, >= 2 s, across 25 of 32 files)
clusters            39
  cluster  files  audio  clips
        1   24/25  2.25h   905   <- one voice in nearly every episode
       28    1/25  1.06h   415   <- one file only: a guest
       29    1/25  1.01h   408   <- one file only: a guest
```

Ranked by DISTINCT FILES PRESENT, not volume: a long-winded guest out-talks the host in their own
episode, but cannot appear in 24 of them.

### The owner's ear, blind

15 clips, two-thirds candidate and one-third deliberately not, shuffled, audio embedded in a page so
nothing had to be found or decoded. Owner: "all Kawa except 1, 2, 11, 13, 14." Scored against the
key: **15 of 15 agree**. The activator is built to REFUSE past one disagreement — it refused once
earlier when the owner ran the example command with my placeholder numbers (7 disagreements, wrote
nothing), which is the safety working.

### Live, proven at the serving path for all eight reviewers

```
Reviewer H / Reviewer A / Reviewer C   served 29 = 25 Kawa + 4 spot-checks   pendingTotal 905
Reviewer B / Reviewer G / Reviewer D / Reviewer F / Reviewer E   served 0, pendingTotal 0  ("No clips in your dialect yet")
```

Kawa is Hawleri, the five are Sorani-restricted by the owner's roster, and the owner's decision is to
leave them: Hawleri reviewers only this phase. The spot-checks (1 in 8 by canon) still fire inside
the focus — the QC does not care which voice is being collected.

### Two things caught on the way

* The background `tauri build` wait-loop declared "done" on a STALE exe (it checked for cargo while
  vite was still running; no focus code in the binary). Caught by the negative-control grep before
  deploy; rebuilt in the foreground.
* `check_reviewer_queues_live` reported 15,318 servable while every real queue held 905 — it
  mirrored the pending query but not the focus, and would have read OK against an empty queue. Now
  reads the same file the server reads; honest, it immediately surfaced the five-empty-queues fact.

Caveats carried: 7 of 32 episodes contributed no candidate clips (no single-speaker scoring yet);
clusters 10 and 17 (12 and 11 files) may be the host on other mic days — a second blind round could
roughly double his hours. Not now: 905 clips / 2.25 h is a working set, and the reviewers should be
on it rather than waiting.

**GO_LINKS: YES** (Hawleri roster). **GO_MODEL_PROMOTION: NO** — unchanged.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-20 (round 2) — the suspects were him: Kawa's set grows 905 -> 1,352 clips

Round 2 sampled the two clusters that looked like the host on other mic days (10: 185 clips/12
files, 17: 262 clips/11 files) — 6 clips each, plus 3 controls drawn from the already-confirmed
905, shuffled. Owner verdict: all 15 are Kawa. Scored per cluster: controls 3/3 (round valid),
cluster 10 6/6 MERGE, cluster 17 6/6 MERGE.

```
focus now : 1,352 clips | 3.35 h | across all 25 scored episodes | all pending
serving   : the three Hawleri reviewers, pendingTotal 1352, 25 Kawa + 4 spot-checks per batch
            Sorani-restricted reviewers still 0, by owner decision
```

The merge machinery refuses two ways by design (pinned): a cluster confirmed on only some of its
clips is rejected whole, and a missed control voids the round. Neither fired — but they existed
before the verdict did, which is the order that makes a clean merge mean something.

Remaining Kawa upside, not taken now: 7 of 32 episodes still have no speaker-change scores, so
none of their clips could enter any cluster. Scoring them and re-probing would grow the set again.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>

## 2026-08-20 (round 2 hunt) — 24 confirmed findings fixed, and Kawa's masters were 48 kHz all along

Second adversarial sweep (54 agents: 6 scoped finders, 2 independent refuters per finding): 24
raw findings, 24 confirmed, 0 disputed. All-confirmed earned extra suspicion, so every finding was
re-verified by hand against the source before any fix — all 24 held. Every fix landed with a test
that fails on the pre-fix code (spot-verified by swapping HEAD files back in).

The ones that mattered most:

* Policy fail-closed had TWO remaining holes: a roster KEY that mismatches the live name by
  case/whitespace bound nobody (the 2026-08-16 incident back through the keyhole — lookup now
  matches the session layer's name rules, colliding keys are a broken file), and the Python
  mirrors parsed NaN/Infinity that serde 503s (parse_constant rejector, repo precedent).
* My own morning fix was incomplete: the receipt front door re-resolved revision AFTER the fence,
  re-opening the manufactured-evidence window. The mint is now a single atomic
  check-and-insert (`record_playback_receipt_if_at_revision`) — verified revision or no row.
* The reorder had broken skips (fenced into an endless boomerang + false "work lost") and served
  spot checks (one bulk stamp away from 409ing every check in flight). Both are now fence-exempt;
  a stale-serve skip lands WITHOUT evidence.
* Pay integrity: the review_events audit row now commits INSIDE the decision transaction, the
  event snapshots its clip's duration (migration v56), and reviewed_audio_ms LEFT JOINs — so a
  kill window or a deleted clip can no longer erase paid work. reject_speaker_change_clips
  collapsed to the same one-commit finalize.
* Shared-phone identity: attribution-409s HOLD work for its author (both flush and live paths),
  drafts/refused banners are reviewer-scoped, a background refill no longer yanks the reviewer to
  clip 0 mid-listen (and re-showing the same clip no longer zeroes heardMs).
* Settings: auto-saves clamp unsaved consent GRANTS (Cancel genuinely cancels, measured leak
  closed), rollbacks target the last BACKEND-confirmed state, the inbox autonomy dial updates the
  global store. Gate mirrors: live_reviewers honors the revocation marker + db_path binding;
  enforcement readiness matches receipts the way the guard does (any age at the decided revision).

```
cargo test        1305/1305 green (after: 2 migration tests taught that v56 exists)
clippy            clean, release + dev
vitest            283 pass (55 files) — 3 new consent tests fail-before-verified
python policies   87 scripts pass; queue-gate core 14 tests
```

**Kawa audio verdict (owner question): GOOD — and better than we were using.** The 1,352-clip
focus (3.35 h): SNR median 40 dB (2 clips under 20), zero clipping, RMS median −24.7 dBFS,
192 of 201 min pass SNR≥20 + RMS≥−30. Stored per-clip QC verified exact against re-measured audio
(8/8, Δ 0.0 dB). The find: the pipeline's 16 kHz WAVs are downmixed copies — the true masters are
48 kHz stereo 320 kbps MP3s (all 32 episodes, timeline-identical: lag 0.0 ms, corr 0.999), and
every focus clip carries source offsets. Cloning exports must cut from the masters (8–12 kHz
sibilance the 16 k copies cannot carry) and loudness-normalize per clip (episode medians spread
−29.1…−21.7 dBFS). No re-recording, no reviewer workflow change.

Accepted, not fixed: same-name link reissue to a different human is indistinguishable by design
(the name IS the identity unit); global speaker identity across the corpus remains the strategic
open item.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>

## 2026-08-20 (champion) — the 7.5/10 review was right, and the champion pipeline is now proven at the serving path

The external engineering review retracted its near-10 and rated 0f7d9ae ~7.5/10 with nine champion
-pipeline blockers. All 19 cited code sites verified by hand: substantively correct (two
overstatements noted: worker death was loud-but-permanent, not silent; all-GPU default matches this
rig's intent — the silent-CPU fallback was the real hazard). Fixed in four commits
(e9e869d, 6815605, e21ee38, 930a6a1), each with the pins updated in the same commit:

1. Crash-atomic imports: resume discriminates interrupted stages (placeholder/empty drafts) from
   completed files and discards+redoes them; adopting the 2026-08-14 incident state is now
   impossible.
2. Halt-on-first-failure at EVERY level: directory imports stop at the first failing file (resume
   journal makes it cheap); an exhausted-empty champion result rolls the file back and halts —
   escalate-and-continue is gone.
3. One commit owner: batch_transcribe no longer re-writes the champion's atomic commit
   (TranscriptionDraft.committed_by_pipeline, exactly one true site, pinned).
4. Direct transport: Rust speaks the server protocol itself. Gone per clip: a Python interpreter
   spawn, a ~108 MB live DB+WAL copy into WSL, TWO full decodes of a 55-80 min episode. Server
   decodes only the clip window (ffmpeg -ss/-t). MEASURED same clip (9.5s, 42.6 min into EP26):
   8.64s -> 3.42s warm server-side; in-app champion draft ~1.4s in the E2E.
5. Cancellation: batch threads its token into the 7B call; the gate wait is cancellable (100ms
   polls); the socket read polls cancel every 500ms.
6. Fleet: dead workers respawn on their own device (bounded 3, backoff, unit-tested 4/4); GPU
   discovery failure is a HARD STOP (CPU only by name); auto-discovery takes only cards with
   >=20 GB free (measured: WSL-side nvidia-smi sees replica VRAM on this rig).
7. Factory refinement default Local -> None: a fresh install's champion no longer depends on an
   unrelated LLM endpoint (privacy pin updated: None is stricter).
8. Gates test production: verify_10's champion probe speaks the protocol and matches the pinned
   deploymentSha256 (verified green against the live server); the 7B test leg's skips are loud and
   fatal under CORTEX_REQUIRE_7B=1; ONE port accessor ends the health-vs-transcribe port drift.
9. Reproducible recovery: scripts/wsl7b_requirements.lock (158 pins frozen from the LIVE venv,
   py3.12.13/Ubuntu 26.04); WSL_DR_RUNBOOK corrected off /root/cortex_env; start_7b_server.ps1
   passes the champion pointer the server actually requires (it was setting an ignored variable).

PROVEN AT THE SERVING PATH (deployed exe e21ee38, real audio, real 7B server):
```
E2E WSL7B : import -> VAD -> champion draft (direct transport) -> re-transcribe -> couch TLS
            -> skip writes nothing -> 428 on evidence-free verdict (asserted) -> decision
            persisted verified+attributed -> REAL-DATA RUN OK
identity  : health probe sha ae33143e... == champion.json pin (content check, not port-open)
suite     : cargo 1309/1309 | clippy clean | vitest 283 | python policies 87/87
```

Found live during the E2E (blocker #6's shape, reproduced): a second app instance's champion
supervision churns 6-minute warm-ups against a port it can never own; production single-instance
self-heals (measured: pkill -> supervision respawn) but slowly. Fast child-exit detection on the
Rust side and multi-instance port arbitration are the honest remaining lifecycle gaps, now tracked.

Still open and owner-facing: the 20-decision enforcement canary (needs real reviewing on this
build), spot-check pool 22/24, and the strategic speaker-identity/train-test-split design.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>

## 2026-08-20 (voice focus) — the desktop was still playing guests, and the canary needs a Hawleri speaker

The owner, reviewing on desktop with the Kawa_Hawleri focus active, heard guests. The focus was
real, live, and narrowing every PHONE queue — and the desktop review page had never heard of it.
`pending_segment_ids_focused` was couch-only; the desktop reads through `get_segments_page`, which
served the whole library. Fourth incident of the same class: the check lived on one serving path
while another path served the lie.

Fix (65deefa): `get_segments_page_focused` joins the allow-list in SQL (`json_each`) so rows,
COUNT, and keyset pages agree; the focus SET is hashed into the cursor scope, so a cursor dies
when `voice_focus.json` changes (its total was computed under the retired list); the command
mirrors couch's fail-closed wording on a present-but-broken file; ReviewMode asks `focused: true`;
curate/library stays unfocused on purpose — the queue narrows, the library does not.

```
proof     : db test desktop_review_page_narrows_to_the_voice_focus_but_the_library_does_not
            (guest never enters the queue; total counts the queue; focused cursor refused against
            an edited set AND against the full library)
pins      : test_voice_focus_policy +1 — one anchor per serving layer (couch, db, command,
            ReviewMode) + curate must never turn the focus on
suite     : cargo 1310/1310 | clippy clean | vitest 283 | svelte-check 0 | 88/88 policy scripts
deployed  : exe at HEAD 65deefa, supervision OK (8 links answering), stale installers deleted
```

Same session, the canary and the roster met: readiness is 22 decisions / coverage-clean but
single-reviewer — and the live queue gate shows why the second reviewer hasn't appeared. The
Kawa_Hawleri focus leaves every sorani-only reviewer (Reviewer B, Reviewer G, Reviewer D, Reviewer F, Reviewer E) with a live
link and ZERO servable clips; Reviewer B's "empty queue" screenshot was the system obeying the roster,
not a bug. The second canary reviewer must be Reviewer A or Reviewer C. Owner decision surfaced: five paid
links are idle for as long as the focus holds.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>

## 2026-08-20 (couch sessions) — six of eight reviewers were locked out by the server's amnesia

Asked why one reviewer's link "doesn't work", and the answer was not the one I gave first. My initial
read — dialect roster x voice focus = 0 clips, therefore a people problem — was HALF the story and
the wrong half. Measured properly, six of eight paid reviewers had stopped at staggered dates
(Reviewer E/Reviewer G 08-13, Reviewer D/Reviewer F 08-15, Reviewer C 08-16, Reviewer B 08-17) and the cause was a defect.

The pairing tokens were durable. The COOKIE the page authenticates with was not: `CouchState` was
rebuilt with `..CouchState::default()` on every start, so the server forgot every session it had
issued while the browser went on presenting one valid for its full 24 h `Max-Age`. The app restarts
4-9 times a DAY on this machine (48 in the nine days measured) — every restart a mass logout. And the
reviewer could not recover: the page strips `#t=` from the address bar after claiming, so a bookmark
or the installed PWA (`start_url` "/") carries no token to re-claim with. Settled 401 renders as the
terminal "link expired", no Retry. Nothing expired; the server had amnesia. Nothing logged it either,
so "their link is dead" and "they stopped working" were indistinguishable from the owner's side.

```
LIVE, same machine, same cookie, at the serving path:
  before (unfixed 65deefa) : /api/queue 200 -> restart -> 401   session FORGOTTEN
  after  (fixed   0224dad) : /api/queue 200 -> restart -> 200   session SURVIVED
  live log : "Couch Review restored 1 live session(s) — saved shortcuts keep working"
fail-before: reverting the restore reds the new test; source restored byte-identical (0813a620be3a7681)
suite     : cargo 1311/1311 | clippy clean | vitest 283 | 88/88 policy scripts
deployed  : exe at HEAD 0224dad, supervision OK (8 links answering), watchdog re-enabled
```

Fix: sessions are DPAPI-protected at rest beside the pairing tokens, restored on resume, filtered by
roster name AND by TTL (restoring resumes, never extends); `session_issued` is wall-clock because an
`Instant` cannot cross the restart it is saved for; `persist_session_state()` runs on claim and on the
sliding page-load renewal; the pairing/session separation is untouched. Refused requests now LOG.

Owner-facing: the reviewers' OLD cookies were never saved and cannot be resurrected — each device
needs its original `#t=` link once more, and after that restarts stop mattering. Two gaps this
exposed, now known: `check_supervision_live.py` proves only that the PORT answers and that the file
lists N reviewers — it never presents a credential, so it read OK through nine days of total
lockout; and the 20-decision enforcement canary has been measuring a system most reviewers could
not log into.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>

## 2026-08-21 (review) — the desktop fix was half a fix, and the Inbox was 89% guests

An xhigh-recall review of the branch found 11 defects. The first one undid the previous entry's
claim: `ReviewInbox` serves the ESCALATION queue — it plays clips, mints playback receipts and
records accept/edit/reject — and applied no focus at all. Narrowing the review page had left the
owner's actual complaint reachable one tab over.

```
escalated & awaiting a human : 11331
  inside the voice focus     :  1293
  OUTSIDE it (guests etc.)   : 10038   <- 89% of what the Inbox served
```

Ten of eleven fixed, one deliberately skipped. The ones worth naming:

  * the Inbox obeys the focus, and `voice_focus::resolve` is now THE entry point — one three-arm
    policy, one refusal wording. Re-deriving it per call site is exactly how the desktop missed it,
    and how the Inbox then missed it again;
  * a narrowed queue can no longer claim the library is finished. `allReviewed` excluded only the
    SEARCH subset, so draining 1,318 focused clips announced all 15,262 as reviewed. The banner and
    empty state now say which subset, in both locales, with no speaker name in tracked strings;
  * `durable_pairing_codes()`: the empty-map guard existed at one save site and not at the newer
    per-claim one, where it would have written a linkless session file over a good one;
  * `supervise_workers` re-checks the stopping event AFTER its backoff — a SIGTERM landing mid-wait
    used to fork a replica nothing would ever signal, orphaning ~19 GB of VRAM and the listen port;
  * the ffmpeg seek starts early and trims exactly. MEASURED on a 320 kbps MP3 master at 42 min: a
    bare seek left the first 10 ms as wrong as the signal was loud (0.0 dB) and 10-50 ms at -5.4 dB
    — the word ONSET — while everything past 50 ms was bit-identical. With the pad the onset
    measures -381 dB against the exact whole-file slice;
  * the stale-installer check compares against the newest SOURCE instead of a 15-minute tolerance
    that was wider than a full build (CLAUDE.md: never weaken a gate).

SKIPPED, with the reasoning recorded on `resolve()`: caching the focus file. A 2 s TTL was built and
reverted — it kept serving clips through a file that had just become broken, which the existing
fail-closed test caught at once. Fail-closed means "on the next fetch", and this policy exists
because pointing paid reviewers at the wrong audio costs money per minute.

And the monitoring gap that hid the nine-day lockout is closed. `check_reviewer_links_live.py`
DPAPI-decrypts each reviewer's real pairing token, claims a session with it, and fetches their real
queue — the thing `check_supervision_live.py` never did, which is why it read OK throughout.

```
REVIEWER LINKS (live, deployed build, real credentials)
  3 reviewers IN with 29 clips served / 1293 pending
  5 reviewers IN but 0 clips  (the Hawleri focus against a sorani-only roster)
  0 links refused
suite    : cargo 1315/1315 | clippy clean | vitest 283 | svelte-check 0 | 88/88 policy scripts
deployed : exe at HEAD 245a472, supervision OK, freshness OK
```

Owner-facing, unchanged by any of this: each device still needs its original link ONCE more (cookies
that were never saved cannot be resurrected), and five paid links stay idle while the focus holds.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>

## 2026-08-21 (paid-review hardening) — focused champion provenance is green; production readiness is not

The owner requested a proof-backed, reliability-only audit before the first paid review batch, with
the fine-tuned OmniASR-7B champion as the sole normal drafting path and no GPU disturbance. This
audit did not start any ASR model, model server, or GPU workload.

Measured state:

```
active Hawleri focus       : 1293 playable pending; 1293/1293 exact champion provenance
global untouched backlog  : 8731 rows without champion hypothesis (focus must remain enforced)
live review serving       : watchdog disabled; port 8737 down; 8/8 saved links unreachable
reviewer allocation       : Reviewer B/Reviewer G/Reviewer D/Reviewer F/Reviewer E have 0 eligible clips under this focus
hidden-check pool         : 0,2,0,2,0,0,0,0 — all eight reviewers below floor
playback canary           : 0/20 post-deployment decisions
frontend                  : 283 tests; typecheck/lint/build pass; format check red on 6 old files
Rust full library suite   : 1340 pass, 8 ignored, 1 fail (compensation-law contradiction only)
Python policy suite       : 86/88 scripts; ledger staleness fixed by this entry, watchdog still red
```

Reliability fixes in the dirty, uncommitted worktree: decision-time row-version/focus/dialect
authorization; idempotent legacy retries; immutable/retryable spot-check scoring; globally ordered
session persistence so a stale snapshot cannot resurrect a revoked reviewer; case-normalized dialect
rosters; reviewed-audio export restricted to current human accept/edit with exact revision-bound
metadata; public reviewer redaction; and production bundle rights/source-byte rechecks with BLAKE3
source and recipient-readable licence/credit manifests. Production bundle generation now uses a
fresh sibling stage, one frozen core-row snapshot, selected-ID DPO output, stored canonical-PCM
verification, and publish-on-success rename; stale or partial files cannot be sealed. Independent
concurrency review found no production deadlock, lock inversion, or unguarded save path. Targeted
export suites passed (19 audio, 39 bundle, 65 dataset with 1 ignored, 10 validation, 18 learning).
The final rights gate also replaced punctuation-stripping licence normalization with an exact,
case-insensitive alias allowlist. Sorani/Chinese compound suffixes and punctuation suffixes now fail
closed instead of collapsing to `CC0-1.0`; 39/39 bundle tests and an independent validator passed.

Stop-ship contradiction: protected `docs/OWNER_CANON.md` defines compensation differently from both
the owner's latest 100% edit / 10% unchanged accept / 10% valid reject request and the current dirty
code (accept/edit 100%, reject 0%). The mismatch is the full Rust suite's sole failure. No pay logic
was changed; owner authorization must be the literal phrase `change canon: review compensation`.
The eventual ledger must use policy versions, canonical work IDs, idempotency keys, integer micro-IQD,
reversals, and settlement state; rejected audio remains outside every dataset and corrected-duration
total even if a valid reject earns the proposed 10% review fee.

Dataset boundary remains strategic: hardened production bundles are ASR-oriented. Generic audio
export still lacks equivalent commit-time consent/source-byte sealing, and the app has no explicit
first-class sibling `asr/<export_id>` and `tts/<export_id>` artifacts. TTS needs preserved-rate masters,
speaker/session/consent provenance, and deterministic speaker-disjoint splits.

Verdict: **NO-GO for paid production**, not 10/10. Engineering reliability is materially higher, but
compensation authorization, supervision, credentialed links, deliberate roster allocation, hidden
checks, a two-reviewer 20-decision canary, export snapshot closure, and backup/restore plus load drills
must pass before release-candidate status.

## 2026-08-21 (GPU-safe recovery) — code green; serving remains deliberately fail-closed

The initial global champion-provenance result above was wrong. The gate hard-coded the historical
`omniasr-wsl-7b` label even though the authoritative registry identifies the same pinned deployment
as `omniasr-7b-legacy-c348ade8a816` (deployment SHA-256
`ae33143ec8b25f45e393f4aa484c3a3d165850f0dc15e95254dd6e4cb4c05cbf`). The corrected gate resolves
exactly one `omniasr-7b` champion, admits the old alias only for that immutable id/hash tuple, and
uses a bound model-plus-transcript `EXISTS` check. Twelve resolver/fallback regressions pass. During
the live import it rejects only the single segment currently between placeholder insertion and its
champion hypothesis; the earlier 8,731-row claim must not be used.

The running `batch_importer.exe` owns the same exclusive `cortex.lock` as the GUI and was left
untouched. At 18:08 +03:00 it had completed 5,136 of 6,922 Lamo WAVs; 5,135 had champion hypotheses
and one was the expected in-flight placeholder. The existing OmniASR-7B server remained the sole
model listener on `127.0.0.1:8799`. No 300M/1B inference, ElevenLabs Scribe call, extra model server,
or GPU restart was performed. Live settings remain `WSL7B`, cloud STT off, with champion process
supervision temporarily disabled so review-only startup cannot duplicate the importer's workers; a
timestamped settings backup exists beside the live settings file.

The watchdog now probes the OS lock rather than file existence: real Windows sharing violations
defer cleanly, stale lockfiles continue to launch, and all other probe faults block nonzero. Its
throwaway-process drill passes 13 branches, and the live dry run reports `defer` on the importer.
The watchdog task remains disabled intentionally because automatic launch after import would expose
five empty reviewer queues and exhausted hidden-check pools. The exact hardened release binary was
rebuilt from HEAD (`07debb908967d285a87abb7f47c1752e281e671cc8a1bea1fdf35f4243255e03`), and the freshness gate
passes.

Verification after the fixes: 1,341 Rust library tests passed (0 failed, 8 ignored); 283 frontend
tests passed across 55 files; Svelte/TypeScript typecheck and ESLint passed; formatting/diff checks
passed; champion hard-stop, agentic export, playback, provenance, queue, spot-pool, and watchdog
policy regressions passed. The Python policy sweep's only remaining red test is the deliberately
disabled live watchdog gate.

The unauthorized partial compensation edit (accept/edit 100%, reject 0%) was removed, restoring the
currently protected distinct-reviewed-clip rule and eliminating the Rust contradiction. The desired
edit 100% / unchanged accept 10% / valid reject 10% schedule is still not implemented; it requires
the literal owner authorization `change canon: review compensation` and a versioned immutable ledger.

Serving-path result remains **NO-GO**: all eight links are down while the importer owns the database;
Reviewer B/Reviewer G/Reviewer D/Reviewer F/Reviewer E have zero eligible Hawleri work; hidden-check capacity is
0,2,0,2,0,0,0,0; and the exact new binary has 0/20 required playback-evidenced decisions. Those are
real campaign/owner/reviewer prerequisites, not defects that may be fabricated away.

## 2026-08-21 18:23 +03:00 — final certification checkpoint

The result remains **NO-GO for real paid review**. This is a proof-backed operational stop, not a
code-quality guess.

Fresh read-only evidence:

```
importer             : PID 57180 still active; 5359/6922 source rows at sample; GUI absent
listeners            : OmniASR-7B only on 127.0.0.1:8799; review port 8737 absent
browser/a11y E2E     : 95/95 passed against a fresh mocked-Tauri Vite instance
freshness logic      : 22/22 passed
release attestation  : FAIL — 12 compiled Rust source inputs are uncommitted
format/diff hygiene  : cargo fmt --check PASS; git diff --check PASS
```

The corrected freshness gate now rejects dirty tracked, staged, renamed, or untracked build inputs.
The executable SHA-256 `07debb908967d285a87abb7f47c1752e281e671cc8a1bea1fdf35f4243255e03`
is therefore a tested local candidate, not an immutable reproducible release. Version the intended
source and rebuild before production certification.

Operational blockers remain: the importer's exclusive lock, disabled watchdog, dead reviewer
serving path, five empty Hawleri reviewer queues, hidden-check counts `0,2,0,2,0,0,0,0`, and a
`0/20` exact-build two-reviewer playback canary. Three genuine Hawleri answer keys are only the
minimum needed to cross the structural hidden-check floor for the three compatible reviewers; under
the current 25-item/every-eighth-hidden cadence, a balanced full 1,293-clip Hawleri shift needs about
69 genuine Hawleri keys. Keys must never be fabricated. A Sorani campaign is possible only after an
explicit owner campaign/focus/roster decision.

No ASR inference, smaller model, ElevenLabs Scribe call, model restart, GPU workload, campaign
mutation, reviewer credential claim, or production service start was performed during this final
checkpoint.

The concurrent live provenance probe passed human-field and accepted-text provenance, then failed
closed on exactly one new untouched in-flight row with no champion hypothesis
(`86d131f6-8a80-470e-9089-9464c869bcdf`). This is consistent with the importer's known two-step write
window and is precisely why the gate must be rerun only after the importer exits and then pass clean.

## 2026-08-21 18:47 +03:00 — handover accepted; hidden-check exhaustion now fails closed

Accepted `cortex-speech-app/docs/HANDOVER_TO_CODEX.md` from commit `e68c9f9` as the coordination and
ownership surface. The highest-value unclaimed reliability gap was the listening-check pool silently
ending while paid review continued. The fixed `>= 200` planning target was replaced with an
enforceable capacity contract derived from the actual Rust serving constants and live campaign.

Commit `3694999` (`fix(review): fail closed when listening proof expires`) now provides both layers:

- `check_spot_check_pool.py` mirrors the live focus, roster, dialect routing, on-disk audio, prior
  per-reviewer scores, and Rust `QUEUE_BATCH=25` / `SPOT_CHECK_EVERY=8` constants. It computes the
  refill-rounded worst-case key requirement for each reviewer because no enforced work quota
  prevents one eligible reviewer from draining the campaign.
- A credentialed durable serving session now refuses a batch with retryable HTTP 503 when fewer
  genuine checks exist than the batch requires, and releases every work lease from that unserved
  batch immediately. Adding a genuine key makes the next request work without restart.
- Ephemeral internal/test sessions retain best-effort behavior; synthetic keys remain forbidden.
- `verify_10.py` carries the full-campaign capacity gate permanently.

Fresh live read-only result:

```
human decisions : 623
active focus     : yes
cadence          : 25 work clips; ceil(batch/8) checks per refill
Reviewer H       : 2 available / 207 required for 1293 accessible work clips
Reviewer C       : 2 / 207 for 1293
Reviewer A       : 0 / 207 for 1293
other five       : 0 / 3 with zero accessible Hawleri work
```

The earlier estimate of 69 keys assumed a balanced three-reviewer split that the application does
not enforce. It is not a production safety bound. The 207 figure is the requirement against the
current unchanged backlog; genuine owner adjudications inside the focus both add keys and reduce
pending work, so the gate recomputes the target on every run.

Verification: spot-check Python core 7/7; runtime pause/unpause/lease regression passed; Couch
88/89; full Rust 1,341 passed, 1 failed, 8 ignored; Python policies 88/89. The two reds are preserved
and unrelated to `3694999`: live watchdog intentionally disabled during import, and compensation
commit `e68c9f9` changed reject pay to zero while an existing canonical Couch test still requires a
reject to count as reviewed audio. Do not edit the money rule/test/canon by implication; owner
authorization remains `change canon: review compensation`.

## 2026-08-23 — schema-61 release-gate convergence

Hardened the Reviewer A-first/Reviewer B-second Lamo campaign without contacting reviewers, starting services,
or loading a GPU. Production review ownership remains fail-closed: generic machine writers cannot
author or overwrite human decisions, and historical test data now enters only through explicit
test-only legacy fixtures. Fixed a real stale machine-upsert defect that could erase an existing
`speaker_change_score` when an unrelated update carried no new measurement.

Fresh automated evidence from the working release candidate: the complete Rust library suite passed
1,508 tests with 0 failures (8 explicitly isolated hardware/benchmark tests); all 209 database tests,
69 command/recovery tests, and 19 jury tests passed; frontend typecheck reported 0 errors and warnings;
292 Vitest tests passed; ESLint, Prettier, and the Vite production build passed. All 101 Python policy
scripts passed. The wider all-target run then exposed two directory-import failures: schema-v60
imports still entered the retired machine-jury path and failed on its first forbidden verdict write.
The shared jury core now makes every current-schema caller a no-write human-review handoff, so file,
directory, audiobook, and direct-command paths cannot drift; the 10-test import E2E suite and a new
schema-boundary regression pass. The remaining integration/property/soak targets also pass after
their historical fixtures were made explicit instead of weakening schema-v60 production guards.
`cargo fmt --check`, all-target clippy with warnings denied, and `git diff --check` are green. Source baseline:
`4b9965486836cd33f3b9eb1e97a439065472c19e`.

## 2026-08-24 — schema-63 private-production hardening in isolated worktree

Implemented and verified the flexible-pool resolution authority in the isolated
`codex/review-production-v63` worktree without touching the live database or GPUs. Any two distinct
eligible reviewers with an exact matching keep/reject outcome resolve a clip; disagreement admits one
blinded third judgment; any matching pair among three resolves; three distinct outcomes require an
append-only owner adjudication; skip is non-judgmental; undo invalidates dependent authority and
requeues. Resolved clips leave the queue and no fourth review is served.

Added fail-closed exact owner-rights stamping, immutable per-voice certificates, deterministic staged
ASR/TTS export, read-only certification, five-minute watchdog certification, and an isolated daily
restore drill. On the live-sized clone, queue p95 measured 542.413 ms for Reviewer A and 537.993 ms for
Reviewer B against the 750 ms gate; two-reviewer decision commit p95 measured 4.889 ms against the 500 ms
gate. A real offsite snapshot restored and verified in 3.355 seconds against the five-minute RTO.

Release-boundary hardening now stages an immutable, hash-bound app/admin/operations bundle outside
the live tree, migrates and certifies a live-sized clone first, gates exposure behind a maintenance
marker, and journals every handover phase. The watchdog refuses malformed or tampered active-release
pointers. Recovery restores the pinned v62 snapshot only when no v63 judgment exists; after the first
v63 judgment it preserves that database and permits only a verified schema-63-compatible binary
rollback. A staged rehearsal passed at schema 63 with all 20,323 audio clips present and exact rights
coverage. Canonical read-only probes found 20,321 available clips for Reviewer A and 17,374 for Reviewer B,
materialized a valid 223,916-byte RIFF/WAVE clip, and verified live submission idempotency authority.

Final isolated verification: Rust library 1,527 passed / 0 failed / 8 intentionally ignored
hardware/isolated-benchmark tests; binary tests passed; strict Clippy and Rust formatting passed;
Tauri integration passed; the synthetic soak passed; frontend 292 passed with lint, formatting,
typecheck (0 errors/warnings), and production build green; all 103 Python policy scripts passed,
including the 13-branch watchdog drill, 24 recovery-snapshot tests, 19 restore tests, and 8 immutable
release-controller tests. `git diff --check` is green. No live production mutation or GPU use occurred
at this checkpoint; commit-bound release build and controlled handover remain next.

The first controlled handover attempt then failed closed before snapshot or migration. Windows
PowerShell 5 represented the controller's one-element JSON executable list as a nested `Object[]`, so
the exact-path stop filter matched nothing and the still-running v62 app retained `cortex.lock`.
Recovery also could not overwrite the administrator-owned legacy `CortexWatchdog` task. The live app,
schema 62, 877 review events, zero pool decisions, port, and both links remained unchanged; the legacy
watchdog was re-enabled and the aborted journal/recovery task were cleared only after those facts were
re-proved. The replacement uses a flat exact-path transport with a real renamed-`ping.exe` termination
regression, waits after force-stop, and registers a separate user-owned
`CortexPrivateProductionWatchdog` bound to the immutable release while leaving the protected legacy
task available only for pre-migration fallback. Its logon trigger is explicitly scoped to the current
interactive Windows principal; the prior unscoped “any user” trigger required administrator rights.

The second handover reached schema 63 and candidate launch, then the watchdog correctly refused to
bless the active pointer because its hash verifier depended on module-autoloaded `Get-FileHash`, which
was unavailable in that real subprocess. Automatic recovery restored the pinned schema-62 snapshot,
preserved the failed v63 database in `recovery-quarantine`, relaunched the fallback, and re-proved both
links with 877 events and zero pool decisions unchanged. The watchdog now computes SHA-256 directly
through `System.Security.Cryptography.SHA256`; a full valid immutable pointer passed in a throwaway
profile, while the malformed-pointer refusal regression remains green.

## 2026-08-24 — final non-human private-production boundary audit

Starting from tested release `0a22485` and source-hardening commit `040e544`, audited every remaining
non-human Milestone 1–3 boundary while the known-good reviewer release stayed live and GPUs remained
untouched. Future production imports now require an existing explicit `CORTEX_APP_DATA_DIR` that is
canonical-path separate from the live profile; missing, aliased, nested, ancestor, and live paths fail
closed. `pool_admin` now classifies every command before database access: writers require the shared
instance lock, ordinary observations use an OS/SQLite `READ_ONLY` stable transaction, and five-minute
certification uses a WAL-consistent disposable in-memory snapshot because the bundled SQLite FTS5
integrity validator performs an internal write and otherwise reports false corruption. Certification
schema 2 separately reports human consensus, owner adjudication, and unresolved conflict totals.

The rotating snapshot loop no longer sleeps ten minutes after a completed capture, which accumulated
backup-duration drift beyond the ten-minute RPO. It advances from fixed monotonic deadlines on a
nine-minute cadence, leaving a one-minute capture/jitter margin while retaining ten snapshots locally
and off-drive.

The first staged candidate was refused before deployment when its clone certification reported the
FTS5/query-only incompatibility. The live database and reviewer server were not touched. Reproduction
on a persistent live-sized clone proved source audio 20,323/20,323, exact rights 20,323/20,323, and
healthy SQLite only when certification used the writable detached copy; focused regressions now pin
that exact access split. Full post-correction evidence: Rust library 1,534 passed / 0 failed / 8
hardware-or-model-isolated ignored; pool-admin binary 6/6; frontend 292/292; Playwright 97/97; typecheck
0 errors and 0 warnings; ESLint, production Vite build, Rust formatting, all-target/all-feature Clippy
with warnings denied, and `git diff --check` green. The complete 103-file Python run passed every
technical policy and deliberately red-marked only this ledger as four commits stale; this entry closes
that administrative gate before the final exact rerun and controlled handover.

## 2026-08-24 — final release and completion-audit correction

Commit `8ef5d3c1b29e41ea926bcf8ab22e3b8b2e68334d` was built as immutable release
`8ef5d3c1b29e-8a999c88e220-2ad63448136e`, passed the live-sized schema-63 clone, and completed a
protected same-schema handover. Independent live proof after exposure: exactly one active release
process; database integrity healthy with zero foreign-key violations; 20,323/20,323 audio clips and
20,323/20,323 exact rights; preserved Reviewer A and Reviewer B local/Funnel credentials; valid WAV and
idempotency probes; queue p95 153.54 ms and 150.97 ms; fresh local and `F:` snapshots; and watchdog
result zero on its five-minute clock. No GPU or ASR inference ran.

The final requirement audit then reproduced a real proof defect: the export crash tests shared one
process-global `AtomicBool`, so parallel Rust scheduling could make a normal export consume another
test's fault or make the crash test miss it. The first unforced run failed 3/19 for exactly that
cross-test contamination. Commit `18423dee480ac5bcff577d02c5d22b02415afc68` scopes the one-shot
fault to the calling test thread and removes a second process-global environment mutation from the
7B semaphore test. The original 19-test selection then passed 20/20; the six export tests passed five
consecutive 16-thread repetitions; the full Rust library passed 1,535/1,535 with eight intentional
hardware/model ignores; pool-admin 6/6 and batch-importer 3/3 passed; strict all-target/all-feature
Clippy, formatting, and diff integrity passed. This audit-only correction preserves production
semantics, so the known-good reviewer release was deliberately not interrupted or replaced.

The final operational audit then found one mode drift in the proof harness, not in the reviewer
runtime: the standalone live-link checker supported generic flexible-pool authentication, but the
master verification command still demanded the retired legacy pilot-policy file. Commit
`cfa403162582b799c50bcb070c70d4a47f2f8d12` adds one fail-closed
`--require-private-production` contract shared by the live-link checker, release controller, and
master verifier. It requires the exact schema-63 pool registry, complete immutable membership,
distinct durable reviewer identities, exact database binding, fixed port, and no simultaneous legacy
pilot policy; pre-pool databases still fall back to the exact legacy contract. Twenty focused policy
tests and nine release-controller tests passed. Fresh read-only checks authenticated Reviewer B and Reviewer A
through both local HTTPS and the public Funnel without minting sessions, taking leases, or changing
review data. This proof-only correction also requires no live release interruption.

A deeper master-verifier run then reproduced two more legacy-only false blockers. The final review
certificate demanded the retired 8,278-ID controlled-pilot focus, and compensation readiness demanded
the absent pilot policy. The playback canary read only legacy `review_events`, so no future schema-63
pool judgment could ever satisfy its 20-decision evidence bar. Commit
`bd9235fb572d2e849f0ad7a7d869764b7a5f254f` selects all three authorities by the active review mode.
Flexible production now hash-verifies the immutable active release and its own `pool_admin`, requires
a fresh full-integrity report with internally consistent pool/champion/voice/resolution/audio/rights/
disk/snapshot authority, proves legacy pay remains immutable but operationally deferred with zero
post-pool event/ledger leakage, and audits effective pool decisions against their exact served
revision, decoded-PCM hash, source span, duration, guard version, and playback receipt. Pre-pool mode
retains every strict legacy canary and compensation requirement.

Focused proof: review-mode certification 24/24, compensation 34/34, playback 35/35, and all 103
Python policy files passed, including the full watchdog drill. The real flexible-mode certificate is
green (`reviewReady=true`, `finalDatasetReady=false`); deferred compensation is green with zero
post-pool legacy events, ledger entries, operation collisions, or foreign-key violations. The
listening evidence gate is now truthfully reachable but remains externally incomplete at 0/20
post-release decisions across two reviewer browsers. Reviewers satisfy that through ordinary game
play; engineering does not fabricate or backdate it. No live database, reviewer server, or GPU was
changed, so the healthy immutable release was deliberately not restarted.

## 2026-08-24 — schema-65 excluded-duplicate legal-lineage hardening

An isolated schema-64 audit reproduced a fail-closed rights defect before deployment: the duplicate
exclusion trigger guarded `review_revision`, while the schema-53 metadata trigger increments that
revision after every segment update. A legitimate rights revocation or provenance correction on an
excluded duplicate was therefore misclassified as human-review evidence and aborted. Schema 65 now
guards only the actual review-authority columns; excluded rows still cannot become canonical review
work, while rights revocation and metadata lineage remain writable and revision-bound. Regression
tests prove both sides and prove an interrupted v65 migration is atomic and recoverable.

Commit `93bbfd310919bdd7cedae34fde7fc892fb61e069` also advances snapshot, restore, certification,
watchdog, and immutable-release boundaries to schema 65. Recovery now proves the pre-exposure
schema-64 snapshot can be restored and its managed last-known-good release reactivated, while any
post-exposure schema-65 judgment blocks destructive database rollback. Full isolated evidence before
the release build: Rust library 1,541 passed / 0 failed / 8 intentional hardware or benchmark ignores;
all binary tests, strict Clippy and formatting, snapshot 26/26, restore 21/21, release recovery 11/11,
integrity 12/12, compensation 34/34, final-certification 24/24, and watchdog 13/13 passed. The live
schema-64 reviewer service and GPUs remained untouched; exact-commit build, clone preflight, and
protected handover remain pending.

The protected v64→v65 handover and a subsequent same-schema proof-tool release both passed live-sized
clone preflight before exposure. Active release
`3aca5885867b-3e1014f35b6f-af72e4ccbf6a-363def17e69f` is built from exact commit
`3aca5885867bc77a87fe669cbee48556a84a04a5`; app/admin/operations SHA-256 values are
`3e1014f35b6f542312e25ebc3e095be8c754fa00d7edc78c9e1a3ef777f41605`,
`b3cc0dfd35163f1512d399da6f8cbc119e92c88d5fa4ef20192a53bde3361a7b`, and
`af72e4ccbf6a7ae9eb5b988197073096d4ec6d439c32fa019b158604837c6378`. Independent live proof found
one exact listener, schema 65 quick/full integrity `ok`, zero foreign keys, 16,990/16,990 canonical
audio, exact rights, zero dedup risk, and `reviewReady=true`. The mode-aware queue proof reports Reviewer B
14,041 and Reviewer A 16,988 eligible clips after dialect policy; both local and Funnel links authenticate
read-only. Fresh verified local and `F:` schema-65 snapshots satisfy the ten-minute RPO, and snapshot
`F:/cortex-backups/snapshots/snapshot_1787574862` restored alone in 3.925 seconds. The watchdog is
enabled with last result zero. Human resolution remains incomplete; no GPU or ASR inference ran.

## 2026-08-24 — schema-65 master-verifier reality check

An unabridged master sweep against the live private-production boundary exposed proof drift that
smaller focused suites had not shown. The legacy duplicate gate included all 3,333 already excluded
audit rows and falsely called the certified dedup manifest a new import; the schema-contract gate was
hardcoded to schema 60; runtime gates searched only the mutable developer target although production
intentionally runs a hash-pinned immutable release; and two Rust integration fixtures still assumed
pre-schema-65 migration/model state. The training snapshot and challenger gates were also confirmed
to be genuinely incomplete—not verifier defects—because the existing sealed fine-tune pack has no
trained challenger binding or measured verdict.

Commit `0983d54eef9acde19c2fe6265017dd83e28ccec5` fixes the proof boundary without changing reviewer
runtime semantics. Duplicate auditing first validates source/canonical/exclusion/risk counts and then
scans only the canonical overlay; missing or inconsistent authority remains red. Schema verification
replays migrations 57 and 60-65 in order and binds 162 exact objects, including the v65 replacement
trigger. Runtime discovery validates pointer containment, executable SHA-256, and baked git SHA before
exporting the active immutable executable to probes. The quality fixture now rolls schema 65 back six
steps to its intended schema-59 legacy boundary, and model-status testing no longer assumes optional
300M/1B artifacts must be absent.

Measured proof: the live schema contract is green at schema 65; the canonical 16,990-clip duplicate
scan reports zero cross-file duplicate content; all 105 Python policy scripts pass; Clippy, formatting,
syntax, and diff integrity pass; the counted Rust aggregate ran 1,649 tests across 43 binaries with a
1,541/0/8 library result; and the exact active binary passed a positive-control egress probe with
2,389 offline workload loops and zero non-loopback backend TCP endpoints. Final live readback kept
`reviewReady=true`, both Reviewer B/Reviewer A links authenticating, supervision green, complete audio/rights,
zero integrity/dedup risk, and fresh local/offsite snapshots. Human progress remains two Reviewer A
judgments, zero resolutions, and 0/20 playback canary decisions. The active release was deliberately
not restarted, and no live database write, GPU, model server, ASR inference, or synthetic judgment was
used.

## 2026-08-24 — bounded master fuzz proof completed

Commit `bc1b5f7b5c186f83f7a15b2b7eaa74627ec8dd52` closes the last interrupted engineering
proof from the schema-65 master sweep. The Windows gate now uses `wsl --exec`, a per-checkout ext4
Cargo cache, one all-target ASAN build, and direct execution of the exact built harnesses. Runtime
corpus/artifacts remain derived Linux-cache state; committed corpus seeds are copied read-only. The
gate rejects an empty target list, build failure, timeout, unsafe target name, nonzero exit, missing
`DONE`, and even an exit-zero `#0 DONE` result.

Measured final run: cache 149,597 iterations; diff 106,129; normalizer 41,326; validation 5,398,283;
total 5,695,335 with zero crashes. All 106 Python policy scripts, including five fail-closed fuzz
policy assertions, passed on the final code. Live read-only certification remained `reviewReady=true`
at schema 65 with all 162 contract objects exact, quick/full integrity `ok`, zero foreign keys,
16,990/16,990 canonical audio, exact rights 20,323/20,323, zero dedup risk, fresh verified local and
`F:` snapshots, Reviewer B/Reviewer A authenticating, and supervision green with 424.8 GB free. Human progress stayed
two Reviewer A judgments, zero resolutions, and 0/20 post-release playback decisions. No reviewer restart,
live database write, GPU, model server, ASR inference, or synthetic judgment occurred.

## 2026-08-25 — deep adversarial audit, and the remediation of 53 of its 55 findings

A 43-agent adversarial audit ran over the whole Rust/Python/Svelte surface at `1282578`
(13 subsystem hunters, one skeptic per finding instructed to REFUTE by reading the code, then a
completeness critic over the subsystems nobody owned; 855 tool uses). **51 defects survived
verification — 6 high, 15 medium, 30 low — and 3 claims were refuted and recorded as refuted.**
The report is `audit-2026-08-25/DEEP_BRUTAL_AUDIT.md`; what was done about it is
`audit-2026-08-25/REMEDIATION.md`. Auditor's grade, explicitly a judgment and not harness output:
**8.4/10**.

Three gates were RED on the machine at the time, from real runs, not review opinion: the
**CortexWatchdog scheduled task was registered but DISABLED** (healthy until 08-24 05:12, then
disabled and never re-enabled — with the app restarting 4-9x/day this is the dead-phone-link
incident re-armed); `PROGRESS_LEDGER.md` was **7 commits stale**; and `spot-check-pool` reported
**0 of 1,107 required answer keys for each of 4 reviewers**. The watchdog is re-enabled and its gate
now prints `WATCHDOG GATE: OK (CortexWatchdog state=Ready)`. This entry closes the second.

The third is NOT fixed, deliberately: the gate's own contract says fixing it is an owner action,
and fabricating answer keys is forbidden. The live database says 53 candidate keys exist
(`verified = 1`, `reviewed_by IS NULL`) and **0 of them are inside the active focus** — the only
honest remedies are owner adjudication at the desktop inside the focus, or narrowing the campaign.

Fourteen fix commits landed (`86719e3`..`d759515`, plus `e69198c`), each carrying a regression gate.
The six high findings: the champion's hard-stop event was emitted by the backend and silently
dropped by the only frontend listener, so a halt wedged the UI at "transcribing" and never named its
cause; `batch_importer` — the owner's primary import lane — built an empty `AudioFingerprint` and
never rehydrated it, leaving the headless path with zero cross-run duplicate protection while
printing a false "Resuming" line; `check_dataset_duplicates.py` divided sample counts by a hard-coded
16 kHz and promised a resample step that did not exist anywhere in the file, so a 48k/16k pair of the
same audio cleared as a legitimate repeat; pool and blinded-second-pass decisions passed the full
playback-evidence gate and appended nothing to the ledger; and `run_gold_eval_with_transcriber`
tallied per-clip failures into a `tracing::warn!` and published a headline CER over the survivors.

**Correction on live evidence:** `review_pool_decisions` and `independent_review_decisions` are both
**0 rows**. The unpaid-work defect is real and the wiring genuinely absent, but no reviewer has been
shorted — the exposure is prospective. The readiness gate now counts and loudly reports uncredited
playback-evidenced decisions per reviewer with duration, and **mints nothing**: whether a
non-canonical second pass is payable under `review-iqd-v1-2026-08-21` is an owner decision requiring
`change canon:`, not an agent's.

Three defects were found while fixing, none of them in the audit. `review_pool.rs` carried two
clippy errors and had **never been linted** — it is absent at `21c639d`, the last fully-swept commit,
and arrived in `e2b256c`, one of the seven unledgered commits (verified with `git cat-file`), so the
unledgered-commit problem was never mere bookkeeping. `test_rust_runtime_panic_policy.py` listed
`remove_lock_file(&lock_path, "failed Unix instance lock acquisition")` as **required** — the very
call by which a refused second instance deletes the live holder's lockfile; it is now forbidden. And
two agent fixes overreached and were caught by existing tests: excluding placeholders (not just
rejects) from the quality counters would have hidden clips the champion has not drafted, and a
reviewed-baseline guard on the generic upserts made a conflicting upsert a silent no-op against the
pinned v60 contract. Both narrowed/removed; `merge_dataset_json`'s own pre-existing guard was briefly
removed with them and restored verbatim.

One methodology error of mine, recorded because the honesty law applies to the auditor too: five
`pipeline` tests were first reported as pre-existing failures. They were not. Running with
`CARGO_TARGET_DIR=scratch-target` (to dodge the running app's DLL lock) put
`scripts/cortex_7b_client.py` out of reach of `resolve_wsl_7b_client`, which walks up from
`current_exe()`; the baseline comparison used the same flag, so the artifact reproduced and looked
like proof. Against the normal target dir all five pass. **There are no pre-existing test failures.**

Phase 4 went further than expected and hit a structural wall worth recording. Both scorecards are
real measured runs over a byte-identical frozen manifest (`ed713075…`): champion
`omniasr-7b-legacy-c348ade8a816` at **micro CER 7.913%** and challenger
`omniasr-7b-challenger-eb0105fdb6a5` at **7.556%**, N=348 each. The champion's provenance sidecar —
the missing piece that made a verdict impossible — was emitted with the repo's own
`emit_scorecard_provenance.py`, which re-hashes every component from disk and fails closed against
the manifest's pinned hashes; the adapter hashes to `c348ade8a816…`, matching `champion.json`'s
`deploymentSha256` pin. But `promotion_gate.py` requires `--slices` and `build_eval_slices.py`
refuses: `none of the 348 manifest row(s) name a clip in this library`. Protected slices derive from
library metadata (speaker, dialect, SNR); the frozen FLEURS eval lives in `gold_segments`, which
carries only id/path/reference/holdout/hash. **The challenger loop is structurally unclosable on the
corpus canon designates as the frozen eval set** — that, not an unrun cycle, is why `runs/` holds
zero verdicts. Closing it needs enriched gold metadata or an owner-authorized in-library holdout eval
set. Weakening the slice requirement was not considered.

Evidence, from real runs on this machine: `cargo test --lib` **1,542 passed, 0 failed, 8 ignored**
(429.86s), and the db suite 209/209 after the final lint cleanup; `cargo clippy --all-targets --
-D warnings` **clean** (it had 4 errors, two of them the never-linted `review_pool.rs` pair);
`cargo fmt --check` clean; `npm run typecheck` **0 errors across 449 files**; `npm test` **297 passed
across 56 files**; `npm run test:python-policies` **105 of 105 policy test scripts passed** (the
ledger-staleness gate goes green with this entry). The policy suite grew 101 -> 105 scripts, and each new file
was checked for a working `__main__` block — without one it is counted as passed while asserting
nothing. Live gates: watchdog OK, and `check_review_serving_provenance.py` passes on the live
database **under the tightened invariants** (skip-bearing rows are no longer exempt), which also
proves no violation was hiding among them.

Not ship-green, and this entry does not claim otherwise. `spot-check-pool` and `challenger-loop`
remain RED for the reasons above; `iaa-kappa-ceiling`, `cordi-dialect-fairness` and
`refinery-lift-in-product` remain owner-gated. Only a full `verify_10.py` run at the release commit
can print `GREEN - PERSONAL-USE SHIP-READY`.

## 2026-08-27 — backend module ceiling closed and generated IPC migration resumed

The schema-65 integration line completed the incremental decomposition of every mutable oversized
Rust production facade without changing registered Tauri command names, dual-writing human truth,
or touching the live database or release pointer. Commits `ea20b50`, `fd2fc48`, `b0170c8`,
`e12edcc`, `c91cd12`, and `e8a8557` split evaluation publication, review-pool deduplication,
processing, commands, Couch, and database authority into cohesive sibling modules. Historical
migrations 1–65 remain byte-identical. The fail-closed architecture scanner now covers 151 shipped
Rust modules with zero failures; the only exception is exact-hash-bound immutable migration
history. The clean backend code commit `e8a85577fee7a7d7b72ee8b18f8d4c2356074484` passed the
complete locked Rust graph once without retry: 1,849 passed, 0 failed, and 43 explicit ignores,
including importer 14/14, soak 1/1, and Tauri integration 1/1. Commit `332e12c` bound that proof in
the integration finding matrix and Obsidian project record.

The next exact contract slice moved the complete desktop-history domain (`undo`, `redo`,
`can_undo`, `can_redo`) from the handwritten bridge to generated Tauri Specta bindings. Undo and
redo now expose renderer-safe `CommandErrorV1` refusals; database-busy remains explicitly
retryable, while hostile SQL, token, and private-path text cannot cross IPC. Capability queries use
the same typed-result envelope, and compile-time tests prevent all four commands from returning to
the legacy inventory. The deterministic policy now measures 116 invoked commands: 17 generated and
99 explicitly contained handwritten calls. Focused proof passed 1/1 Rust public-error tests, 34/34
history-related Rust tests, 26/26 frontend command/adapter tests, and the complete 437/437 frontend
suite; generated-binding drift, TypeScript/Svelte diagnostics, lint, formatting, strict all-target/all-feature Clippy, backend
layering, main-thread, panic, and secret-hygiene gates are green.

This is an integration checkpoint, not certification. Coverage remains below the declared floors;
99 handwritten IPC calls and the oversized Review/Settings workspaces remain; the 50,000-segment,
fault, performance, accessibility, signed-Windows, owner-session, pilot, and comparator evidence is
absent. The only honest verdict remains **INTEGRATED SOURCE IMPROVED — NOT CERTIFIED — NOT 10/10**.

## 2026-08-27 — generated library/metadata contracts and conflict-safe autosave

Commits `6c5cb53`, `1e47eab`, `d8f0454`, and `5551fc1` continued the generated IPC migration through
transcript utilities, renderer-safe diagnostics, bounded library reads, and versioned segment
metadata. The last slice retires the generic renderer field writer: speaker/alignment edits now carry
the exact server value observed and execute as an atomic per-field compare-and-set. Stale same-field
writes fail with a scrubbed typed conflict, mixed conflicts make no partial mutation, and exact
response-loss replay is idempotent. Failed autosave now rejects and remains in an ordered retry queue,
so a native close cannot acknowledge an unsaved edit; retained comparison baselines are bounded to
current or pending work instead of accumulating large alignment JSON across visited clips.

Exact proof at clean `5551fc1bdebfdfc2dd9b2c521791172ff33159e3`: the locked Rust all-target
graph passed with 1,739 library tests, zero failures and eight explicit library ignores plus green
binary/integration/soak/shell/benchmark targets; frontend passed 456/456 across 74 files; Playwright
passed 101/101; all 128 reachable Python policy scripts passed; strict Clippy with warnings denied,
rustfmt, generated bindings, Svelte/TypeScript, ESLint, Prettier, source layering, secret hygiene and
survivor/port checks were green. One development frontend run exceeded the frozen Workstation ceiling
by one line and is not counted; the corrected file is 2,489 lines. The latency probe now honestly names
this operation `metadata_save` and leaves durable playback-authorized decision latency unmeasured.

The inventory at that commit is 116 invoked commands: 31 generated and 85 handwritten. Coverage,
workspace decomposition, real decision latency, timeout calibration, certifying verifier runs,
50,000-segment concurrency/recovery/performance, signed Windows/VM/manual accessibility, owner field
sessions, comparator study and pilot evidence remain red. Production data, processes, ports,
credentials, migrations and the active immutable release pointer were untouched. The verdict remains
**INTEGRATED SOURCE IMPROVED — NOT CERTIFIED — NOT 10/10**.

## 2026-08-27 — evidence-safe deletion, atomic speaker changes, and exact history inverses

Commits `9de2fc2dcbef982aa49f64ebc2a285e7270232b8`, `4bd4b4d7`, and
`fb16b2e8b969c3129eebc74eb67ce1f9aaa24d96` close three desktop mutation hazards without changing
historical migrations or touching production state. Segment deletion now captures exact server rows,
refuses duplicate/missing identifiers and protected review authority before mutation, and restores a
complete server snapshot on Undo. Speaker rename, merge, and batch assignment are revision-aware,
all-or-nothing, replay-safe operations with exact server-owned Undo and Redo. Async command workers
retain restore admission until database mutation and crash-session persistence both finish, so a
cancelled renderer future cannot commit a change and silently skip session recovery state.

The history boundary no longer sends backend English descriptions or separately polls racy boolean
capabilities. Its generated V1 contract returns a stable action enum plus one coherent post-mutation
status. Renderer Undo/Redo is single-flight and locally translated in English and Sorani. Delete and
batch-transcription history now use writer-reserved savepoints, exact endpoint compare-and-set, and
whole-command rollback; deterministic batch Redo stores both server-observed endpoints. Injected
late-row failures prove that a partial multi-row inverse is never published, while stale rows refuse
without consuming the retryable history entry.

Exact proof on `fb16b2e8b969c3129eebc74eb67ce1f9aaa24d96`: strict all-target Clippy passed
with warnings denied; `cargo test --all-targets` passed 1,755 library tests with zero failures and
eight explicit library ignores, then every binary, integration, property, reliability, shell,
87.92-second soak, Tauri-integration and benchmark target completed successfully. Frontend proof was
466/466 unit tests across 78 files and 101/101 Playwright tests; Svelte/TypeScript reported zero
errors and warnings; ESLint, Prettier, rustfmt, generated IPC, secret hygiene, migration immutability,
and all 128 reachable Python policy scripts passed. The production bundle measured 120.66 KB gzip
initial JavaScript and 11.42 KB gzip initial CSS, below the 125 KB and 15 KB budgets. No final gate
needed a test retry; an earlier mistyped policy-runner path executed no gate and is not represented as
product evidence.

This is the strongest integrated source checkpoint so far, not a product attestation. The verifier
still truthfully reports ten missing evidence classes: timeout calibration, verifier fault campaigns,
coverage/mutation thresholds, architecture contract, known-defect ledger, schema clone/restore,
concurrency/performance/memory, owner workflow/recovery, deployment/reboot runs, and owner field
sessions. Signed Windows artifacts, clean-VM update/rollback, manual accessibility, pilot, and paired
comparator proof also remain absent. Production databases, releases, ports, credentials, models and
reviewer work were not touched. The verdict remains **INTEGRATED SOURCE IMPROVED — NOT CERTIFIED —
NOT 10/10**.

## 2026-08-27 — verifier-owned architecture and known-defect evidence

Commits `6a261edd1cc724fd212cb2ac0f81c059bc4c4783` and
`0f3e81373b9f68ddb2819eaf62101d9342c6a4cb` replace two self-authored certification placeholders
with class-specific, fail-closed evidence. `architecture-contract-evidence` measures the Rust module
ceiling, generated-versus-handwritten IPC inventory, Svelte composition/workspace/component ceilings,
and direct desktop-runtime imports. `known-defect-ledger-evidence` validates a strict tracked defect
inventory and binds every audit/tracking authority to canonical bytes from the exact Git commit. Its
evidence cannot verify unless `clean-source-tree` also passes. The Windows LF/CRLF materialization
false negative found by the first clean-commit run is pinned by a regression test; Git blob bytes,
not checkout-translated bytes, are now the authority.

The history persistence extraction moved 309 lines from `db/segments.rs` into the cohesive
`db/history.rs` sibling without changing command behavior. The segment store is back to 1,822
production lines, and the 152-module Rust architecture scan has zero failures. Focused exact proof:
20/20 history tests, 12/12 segment-write store tests, strict all-target Clippy, frontend typecheck,
lint and formatting, 30/30 verifier supervisor/fault-contract tests, and the repaired segment-store
policy passed. The complete 128-script policy campaign found only the extraction-sensitive path
assertion; after the policy was pointed at the new authority, that test and the policy reachability
meta-gate passed independently.

On clean `0f3e81373b9f68ddb2819eaf62101d9342c6a4cb`, the exact known-defect artifact passed with
zero supported-flow P0/P1/P2 blockers and a SHA-256-bound ledger. The architecture artifact honestly
failed: Rust passed, but 80 handwritten IPC contracts, one dynamic bridge, and ten oversized Svelte
files remain. This is proof of a narrower known-defect inventory and an exact architecture worklist,
not release certification. Production data, releases, ports, credentials, models, migrations and
reviewer work were untouched. The verdict remains **INTEGRATED SOURCE IMPROVED — NOT CERTIFIED —
NOT 10/10**.

## 2026-08-27 — generated diagnostics, local-state and analytics IPC contracts

Commits `addbd24a5e2fa57ebdd09b839890a72595e5ce5d`,
`a140b2343a32d7cb361d71d1ddc10a67132ba3e8`, and
`3347c71702719e52a1d3d875ac85404b8060f54f` move twelve renderer-reachable commands from the
handwritten compatibility inventory to checked-in Tauri Specta bindings. The first tranche covers
fingerprint diagnostics, operation cancellation, configured providers, bounded API-key persistence,
and session save/restore. The second covers dataset statistics, dataset quality, training-grade
breakdown, conformal certification, and label-quality lift. Generated command failures now use
`CommandErrorV1`; analytics failures scrub database/internal details, and certificate probabilities
are rejected unless finite and within their exact public domains.

The measured IPC inventory improved from 34 generated / 80 handwritten to 46 generated / 68
handwritten. The scanner reports zero generated commands with a noncanonical error type. The one
remaining dynamic bridge is confined to the legacy adapter. The architecture gate still fails
honestly because 68 handwritten contracts, that bridge, and ten oversized Svelte files remain; the
Rust architecture subgate is green.

During the complete policy campaign, the `save_session` rate-limit source policy rejected rustfmt's
multiline spelling even though the runtime command already contained the correct limiter. The policy
now recognizes the call expression across formatting whitespace and strips line comments so a
comment-only spoof cannot pass. Its positive and negative scanner regression and the policy
reachability meta-gate pass.

Post-change proof: focused analytics and API/session Rust tests passed, strict all-target Clippy and
rustfmt passed, the frontend passed 476/476 tests across 83 files with zero Svelte/TypeScript,
ESLint, or Prettier findings, generated-binding drift and secret-hygiene gates passed, and the
verifier supervisor passed 30/30 fault-contract tests. A fresh complete campaign then passed all 128
reachable Python policy scripts, including syntax compilation and the real watchdog timing drill.
No production database, release pointer, port, credential, model, migration, reviewer decision, or
payment state was touched. The verdict remains **INTEGRATED SOURCE IMPROVED — NOT CERTIFIED — NOT
10/10**.

## 2026-08-27 — opaque renderer media grants and deterministic redirect refusal proof

Commits `90dcfbca84a1e6aa6449338ed21fb7a097a26c3f` and
`93676500a7ec57d847d46b949c2b4f37672e511f` close a renderer privacy and capability flaw: the media
command previously returned an absolute cache path even though its contract described an opaque
grant. The public generated `MediaGrant` now contains only a canonical RFC 4122 version-4 grant ID
and expiry. The renderer accepts only the private `cortex-media:` origin (or its exact Windows
loopback-origin form), while backend registry state remains the sole path authority. The generic
Tauri asset filesystem protocol, `convertFileSrc`, renderer path conversion, and asset CSP origins
are removed.

The private media protocol is live-grant-only, GET/HEAD-only, query-free, canonical-ID-only, and
returns generic empty error bodies with `no-store` and `nosniff`. Reads hold an immutable cache-file
lease, execute through bounded blocking work, allow at most eight concurrent protocol workers, and
cap each response at 1,024,000 bytes. Single byte ranges are supported; multiple ranges and malformed
or stale grants fail closed. All three media commands are checked-in Specta contracts returning
`CommandErrorV1`; renderer-visible failures are bounded and scrub paths, SQL, tokens, and backend
details. The IPC architecture inventory is now 49 generated, 65 handwritten, one closed dynamic
legacy bridge, and zero generated commands with noncanonical errors.

The same hunt found a nondeterministic security-test fixture rather than a production redirect bug:
the local redirect server could close with an unread POST body, causing Windows to surface a TCP
reset before the client could prove redirect refusal. It now consumes the declared request body
before returning 307. The isolated redirect-refusal test passed 30 consecutive runs and passed again
inside the full Rust campaign.

Post-freeze proof: 1,766 nonignored Rust library tests passed with zero failures and eight explicit
ignores; every binary, integration, property, reliability, shell, 61.19-second soak, Tauri, and user
data target reached a green result. The first aggregate command then exited 1 because its libtest-only
`--test-threads=1` argument was forwarded to Criterion; this invocation is retained as failed proof,
not reclassified. A corrected exact run of the `audio`, `diff`, and `normalizer` benchmark targets
passed. The frontend passed 489/489 tests across 85 files and 101/101 Playwright tests; Svelte/
TypeScript, ESLint, Prettier, strict all-target/all-feature Clippy, rustfmt, generated bindings,
security policy, and all 128 Python policy scripts passed. Verifier supervisor proof was 30/30;
`npm audit --audit-level=high` reported zero vulnerabilities; `cargo deny check` passed; the production
bundle measured 120.95 KB gzip initial JavaScript and 11.42 KB gzip initial CSS.

This is not a 10/10 attestation. The measured architecture gate remains red with 65 handwritten IPC
calls, one legacy dynamic bridge, and ten oversized Svelte files. Coverage/mutation thresholds,
timeout calibration, full verifier fault campaigns as certifying manifests, live-sized schema-65
clone/restore, 50,000-segment concurrency and performance, exact owner workflow/recovery, immutable
deployment/reboot runs, real champion/hardware exercises, signed Windows/VM/manual accessibility,
field sessions, comparator study, pilot, and model evidence remain absent. Production databases,
release pointers, processes, ports, credentials, models, migrations, reviewer decisions, and payment
state were untouched. The honest verdict remains **INTEGRATED SOURCE IMPROVED — NOT CERTIFIED — NOT
10/10**.

## 2026-08-27 — generated model/engine IPC and renderer-safe registry mutations

Commits `a3b743abf0d0070c2ad73a1967a2dd7f6fb8a97a` and
`5f1bec63e0374207790f668396d2a1cd39c0743d` move eight model-management commands from the
handwritten compatibility bridge to checked-in Tauri Specta contracts. Read/start coverage is
`models_status`, `models_download_all`, `get_champion_engine_status`, `start_champion_engine`, and
`list_model_versions`; registry-write coverage is `import_model_checkpoint`,
`import_model_deployment`, and `bootstrap_legacy_champion`. Renderer DTOs are versioned and
camel-cased, model paths remain backend-only, development preview returns the same result shape as
production, and the frontend preserves structured `CommandErrorV1` values instead of flattening
them to strings.

The champion-status command no longer turns a registry database failure into a false “no champion”
state or forwards raw engine/WSL diagnostics. Registry mutations validate inputs before worker
admission and reduce hashing, manifest, database, pointer, path, SQL, and restore-fence failures to
bounded public codes. The restore fence and async-worker ownership remain intact; no simultaneous
old/new write path was introduced. Focused Rust proof passed 2/2 typed public-contract tests and
33/33 registry tests. Frontend proof passed 492/492 tests across 86 files with zero Svelte/TypeScript,
ESLint, or Prettier findings. Generated-binding regeneration, secret hygiene, model-provenance,
owner-canon (12/12 pins), and UI-thread policy gates passed. The measured IPC inventory is now 114
invoked commands: 57 generated, 57 explicitly contained handwritten, one closed dynamic bridge, and
zero generated commands with a noncanonical error contract.

Strict Clippy for the second commit did not produce a source verdict: Windows refused a Tauri build
artifact with `os error 32` while the separately owned pool agent's concurrent Cargo process held it
open. The failure is retained as an uncompleted proof, not rewritten as a pass. The scoped diff check
was clean, and the Rust contract/registry test binaries compiled and passed. Pool/Couch/reviewer-link/
compensation files were neither edited nor staged by this tranche; they remain owned by the concurrent
pool workstream. Production databases, releases, credentials, ports, models, migrations, reviewer
truth, and payment state were untouched.

This closes eight concrete IPC/error-leak imperfections but does not satisfy the architecture class:
57 handwritten calls, the closed legacy bridge, and ten oversized Svelte files remain. The prior
coverage/mutation, calibrated certifying-manifest, schema-clone, 50,000-segment fault/performance,
owner workflow/deployment/reboot/field, signed Windows/VM/manual-accessibility, comparator, pilot, and
model-evidence gaps also remain. The honest verdict remains **INTEGRATED SOURCE IMPROVED — NOT
CERTIFIED — NOT 10/10**.

## 2026-08-27 — generated durable Job Center contract

Commit `5132b6d8a2065618c99facd9cc8e63d5ff9e1747` moves `get_jobs` out of the handwritten IPC
bridge. The native boundary now returns `JobV1` with an exact generated five-state union and
renderer-safe `CommandErrorV1` failures; database and path detail can no longer cross into the UI.
The development browser preview also returns the production list shape instead of rejecting the Job
Center as an unknown mock command. No job lifecycle transition, durable row, or database schema was
changed.

Proof ran after the independently owned pool fix landed as base commit `4446cbb2`: the focused Rust
wire/error test passed 1/1, the focused frontend adapter tests passed 2/2, the complete frontend
suite passed 494/494 across 87 files, Svelte/TypeScript reported zero errors and warnings, and ESLint,
Prettier, strict library/all-feature Clippy with warnings denied, rustfmt, generated-binding drift,
secret hygiene, scoped diff, and IPC policy gates passed. The measured IPC inventory is now 58
generated / 56 handwritten / one closed dynamic bridge / zero noncanonical generated errors.

The proof used a separate Cargo target because the pool workstream held the default Windows target;
Cargo removed all 9,729 disposable build artifacts (11.8 GiB) afterward. Pool/Couch/reviewer and
compensation source was not edited or staged in this commit. Production data, migrations, release
pointers, ports, credentials, models, reviewer truth, and payment state were untouched. Architecture
and the previously declared release/evidence classes remain incomplete, so this is **NOT CERTIFIED —
NOT 10/10**.

## 2026-08-27 — integrated source freeze after pool and typed-IPC convergence

Commit `f00282cac13a1e965f24e4a772453d6729a94899` repairs three proof-surface defects exposed by a
complete campaign: champion-release policy now requires the generated `ModelVersionSummaryV1` and
`CommandErrorV1` boundary; restore-admission policy verifies guard assignment plus propagated failure
instead of demanding one literal `begin_mutation()?` spelling; and the opaque-media rejection fixture
no longer hardcodes a fake private Windows user-profile URL. The first 129-script campaign remains a
failed diagnostic with four named failures: those two stale scanners, that repository-hygiene finding,
and generated-binding compilation colliding with the concurrently running Rust target.

Fresh proof was then run without that contention. On clean
`f00282cac13a1e965f24e4a772453d6729a94899`, one uninterrupted sequential campaign passed all 129
reachable Python policy scripts, including generated-binding regeneration, the repaired scanners,
pool pay-fence scope, repository hygiene, verifier supervisor 30/30, the real watchdog timing drill,
and Python byte compilation. The current frontend passed 494/494 tests across 87 files with zero
Svelte/TypeScript, ESLint, or Prettier findings. Playwright passed 101/101 with zero retries on clean
`009220f1c41fd17995e974d47dc34095bf6f6bc2`.

The same integrated Rust source at `009220f1c41fd17995e974d47dc34095bf6f6bc2` completed one
uninterrupted `cargo test --all-targets`: 1,771 library tests passed, zero failed, eight library tests
were explicitly ignored, and every binary, integration, property, reliability, 88.38-second soak,
Tauri, user-data, and Criterion audio/diff/normalizer target passed. Another 35 target-specific tests
were explicitly ignored because they require live models, network, or real media. Strict
library/all-feature Clippy with warnings denied had already passed on the same production Rust source.
The later `f00282c` commit changes only Python policies and one TypeScript test fixture, not production
Rust or frontend code.

This is a strong integrated source freeze, not certifying evidence. Final runs may not contain ignored
or skipped checks; the policy campaign also reports that its optional jiwer numeric-equivalence check
was skipped because jiwer is absent, and Couch Sorani policy still identifies 19 strings awaiting
owner linguistic review. Architecture remains 58 generated / 56 handwritten IPC calls, one closed
dynamic bridge, and ten oversized Svelte files. Coverage/mutation thresholds, calibrated certifying
manifests, live-sized schema-65 clone/restore, 50,000-segment concurrency/performance/memory, exact
owner workflow/deployment/reboot/field evidence, signed Windows VM/manual-accessibility evidence,
comparator/pilot evidence, and model evidence remain incomplete. No production data, active release,
credentials, reviewer truth, or payment state was changed. Honest verdict: **INTEGRATED SOURCE GREEN
FOR THE NAMED GATES — NOT CERTIFIED — NOT 10/10**.

## 2026-08-27 — owner-workstation-only execution plan locked

Owner direction narrows the active release target to the current Windows 11 workstation and one
desktop user. `docs/OWNER_WORKSTATION_10_10_EXECUTION_PLAN_2026-08-27.md` is now the controlling
execution order from starting commit `865c5c01e5c7a4c1b1f53fa0c1dc11cb3ee101bb`. Public installer
signing/updater work, servicing-release VM matrices, comparator studies, external pilots/reviewers,
and broad ASR-superiority evidence do not block this owner verdict. Local human truth, exact champion
identity, crash recovery, schema-65 clone compatibility, offline privacy, internal concurrency,
coverage/mutation, performance/memory, exact-binary proof, cold reboot, and thirty owner sessions
remain mandatory.

The execution order is proof integrity first: pin the desktop Python verifier environment with
`jiwer==4.0.0`, route all 43 explicit Rust opt-in tests without letting an owner-critical check hide
behind an ignore, and refresh coverage at the exact clean SHA. Then close the 56 handwritten IPC
commands, one dynamic bridge, ten oversized Svelte owners, coverage/mutation thresholds, hostile
local campaigns, real champion workflow, and final no-retry verifier/burn-in evidence. Rust's current
mechanical architecture gate is green; the measured renderer inventory remains 58 generated, 56
handwritten, one dynamic, and zero noncanonical generated errors.

Pool/Couch/reviewer implementation remains owned by the separate pool workstream. This plan verifies
its shared-database and API seams but does not create overlapping implementation edits. No production
database, release pointer, credential, process, port, reviewer truth, or payment state was changed.
Honest status: **OWNER SCOPE AND PLAN LOCKED — IMPLEMENTATION PENDING — NOT 10/10**.

---

## 2026-08-29 — Robustness loop on `public/clean-release` (PR #72 branch)

Doctrine: `docs/ROBUSTNESS_LOOP.md`. One-day continuous, in-session, owner-directed. Product work is
confined to this worktree; ops tooling lives on `deploy/link-health`. Codex's trees are never touched,
and any target whose fix would land in a file codex is refactoring is SKIPPED (that rule sent the
`ipc` domain — the second-largest by uncovered branches — back untaken, because codex holds
`commands.rs` and nearly every `commands/*.rs`).

**Coverage work (CI-confirmed, critical-domain branch coverage):**

| domain | before | after |
|---|---|---|
| review | 41.67% | 61.73% |
| payment | 55.66% | 62.26% |
| restore | 56.85% | 58.41% |

- `71f6e448` guarded two tests requiring the optional OmniASR 300M fixture CI never downloads.
- `bfd7f280` `f50671af` `15509821` review-campaign forgery refusals, adjudication lifecycle,
  second-pass activation refusals; `d1d7c81a` all sixteen playback-error mapper arms.
- `f1d7417b` `a334408f` pool activation + voice certificate + decision refusals, and the pay-once
  rule (a re-typed `"ALPHA  "` cannot become a second payable opinion).
- `6deb6f84` pay arithmetic asserted as literals (1s edit = 5_000_000 micro-IQD) and the anti-split
  work-id namespace — the rule stopping one clip becoming several paid work ids.
- `629947b0` `3d4d74c4` restore effect-graph refusals: forged effect on a skip event, a decision
  stripped of its pay effect, a deleted v60 frontier, revision/identity/text/provenance corruptions.

**Defect found and fixed — restore refused a legitimate phone Undo.**
`validate_review_effect_semantics` compared the UNDO's operation id against `review_events.operation_id`,
which carries the DECISION's — different values by construction, in TWO separate queries. Any backup
containing an undone phone decision was refused. Reproduced entirely through production APIs
(`record_phone_human_decision_…` + `undo_human_decision`); live path is `couch/decisions.rs` `api_undo`.
Failed closed, so nothing was ever corrupted. Fixed by removing both impossible clauses; the binding
remains complete via `entry_key = 'undo:' || <undo op>` plus full field-by-field negation checks.
No policy pin depended on the clause (checked first). Regression tests cover both directions: a
genuine undo restores, and four broken inverses (deleted, mis-keyed, non-negating, paying out) are
still refused.

**Process defects caught in this loop's own work, all recorded in commit messages:** an assertion
that could never fire (`CHECK(singleton_key = 1)` makes a second row impossible), a `.to_uppercase()`
no-op on a letterless UUID, two guessed trigger names that made a test bounce off untouched guards,
a scoreboard that printed GREEN at `lines=79.12/85`, and three coverage measurements lost to a
linker OOM that was diagnosed twice by inference before the error was read.

Honest status: **the tests and the restore fix stand on their own; PR #72's thresholds remain far
out of reach by this method — roughly 1,780 more covered branches across five domains.**

## 2026-08-29 — The published tree names no reviewer, and a gate now says so

`origin` is a PUBLIC repository. The reviewers are private individuals who agreed to transcribe
audio, not to appear in a public git history. 577 name occurrences were scrubbed across 41 files
once. Sixteen came back the same day this entry was written, in test fixtures that used real
reviewer names as sample data, and `git blame` attributes every one of them to commits made hours
earlier in this session. Nothing objected, because nothing was checking: the rule existed only in a
human's memory.

`scripts/test_deidentified_tree.py` now checks it. The forbidden names are stored as SALTED SHA-256
digests, never as literals — a gate that spelled the names out would be the leak it exists to
prevent, and this way the gate file itself stays publishable. It tokenizes every tracked text file,
lowercases, hashes, and compares. Two exclusions are deliberate and documented in the file: the
repository author's own name, which git records as commit authorship, and the VOICE names of the
corpus speakers, which are dataset identity the owner holds distribution rights to.

The gate immediately found five occurrences a case-sensitive `git grep -w` sweep had missed —
a lowercase reviewer name embedded in work-id fixtures, and three test FUNCTION names. Fixtures were renamed
to neutral identifiers (Alpha, Bravo, Delta), preserving the byte lengths the work-id length-prefix
assertions depend on (the old and new fixture names are both five characters, so `reviewer-work-v1:5:` stays
true). No policy script pinned any of the renamed test functions; that was checked before renaming,
because a renamed symbol silently breaks a greppable pin.

Measured: 1,812/1,812 library tests, 0 failed, 8 ignored. The gate carries an anti-vacuity check that
proves a real reviewer name hashes into the forbidden set while a neutral fixture name does not, so
a future edit cannot leave it asserting nothing. No live database write, no deploy, no reviewer
restart.

## 2026-08-30 — Reliability iterations 1–2: restore refusal coverage

The pinned robustness scoreboard ranked
`restore_service::effects::validate_review_effect_semantics` first. Work stayed on
`public/clean-release` in the isolated `cortex-scrub` worktree; no live database, reviewer service,
scheduled task, release pointer, model, GPU setting, or production artifact was touched.

- Iteration 1 proved that every noncanonical schema-v60 frontier field and both frontiers claiming
  nonexistent event/ledger history are refused for the intended reason. Commits `f9566433` and
  `75ebb3df` were pushed. Exact pinned coverage passed 1,814 library tests with 0 failures and moved
  restore branches from 58.59% to 58.99% (622 to 616 uncovered); overall branches moved from 59.24%
  to 59.30%.
- Iteration 2 took the next uncovered desktop-operation boundary. One table-driven test proves that
  an unlinked desktop effect is refused if it is relabelled as Couch work, acquires a reviewer, or
  loses its operation id, payload hash, requested action, or timestamp. Focused result:
  `test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 1807 filtered out`. Strict
  all-target/all-feature Clippy with `-D warnings` and rustfmt both exited 0. Commit
  `e32b5a3a3aa66b4fbe50f15cac9f3a5c539846cb` was pushed and independently matched on
  `origin/public/clean-release`.

Honest status before the iteration-2 remeasurement: P0 remains unknown because no current health
heartbeat exists; P1 remains red and materially below its locked bars; P2 remains green.

## 2026-08-30 — Reliability iteration 3: exact legacy correction-memory boundary

The current pinned scoreboard still ranked
`restore_service::effects::validate_review_effect_semantics` first. The next exercised refusal gap
was the schema-v60 correction-memory boundary: only rows that existed before migration may carry
`legacy_seed = 1`; every post-v60 row starts at zero and requires immutable effect-bound capture
lineage.

One two-sided regression now constructs the exact grandfathered row and proves it remains
restorable, then applies `legacy_seed = 2` with restored-file guards disabled and proves the
validator returns the specific invalid-legacy-boundary refusal. Focused result:
`test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 1807 filtered out`. Strict
all-target/all-feature Clippy with `-D warnings` and rustfmt both exited 0. Commit
`0cd9c52b734775f047069a4c0d3281d4dfed3a44` was pushed and independently matched on
`origin/public/clean-release`.

Honest status before the iteration-3 remeasurement: the product chunk is green and pushed, but no
coverage improvement is claimed until the pinned all-target run finishes. P0 remains unknown; P1
remains red; P2 remains green. Production and scheduled tasks were untouched.

## 2026-08-30 — Reliability iteration 4: correction-memory contribution admission

The same top-ranked restore validator next exposed its correction-memory contribution admission
cluster. One table-driven regression inserts eight guarded restored-file corruptions: missing effect,
missing memory, an out-of-range capture delta, a zero contribution, simultaneous confirm/override,
capture on an accept decision, confirm without `fired_at`, and blank `fired_at`. Every case first
proves its corrupt row was inserted, then requires either the exact missing-effect refusal or the
exact action/evidence-identity refusal.

Focused result: `test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 1807 filtered out`.
Strict all-target/all-feature Clippy with `-D warnings` and rustfmt both exited 0. Commit
`69d9b9b7b07a2be9f49839c5e8fdcf1caf9e8786` was pushed and independently matched on
`origin/public/clean-release`.

Honest status before the iteration-4 remeasurement: no coverage delta is claimed yet. P0 remains
unknown; P1 remains red; P2 remains green. Production and scheduled tasks were untouched.
