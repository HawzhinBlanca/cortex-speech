# Cortex Speech owner-workstation 10/10 execution plan

**Authority date:** 2026-08-27  
**Starting commit:** `865c5c01e5c7a4c1b1f53fa0c1dc11cb3ee101bb`  
**Branch:** `codex/10-10-integration`  
**Target:** the owner's existing Windows 11 workstation and one desktop user

This is the active execution plan for the only release that matters now: a dependable local Cortex
Speech workstation on the owner's current PC. It narrows the older machine-only and Windows-product
plans without weakening data-integrity, recovery, privacy, model-identity, or proof requirements.

## Exact verdict contract

`CORTEX PRODUCT 10/10 — OWNER WORKSTATION` may be generated only when one exact clean Git commit and
one exact executable satisfy all of the following:

1. Zero unresolved P0, P1, or supported owner-flow P2 defects.
2. No lost, duplicated, misattributed, stale, or silently corrupted human decision.
3. The exact champion serves the real local workflow or the operation hard-stops before mutation.
4. Every owner-product gate passes with no retry, skip, stale evidence, lease takeover, manual status
   edit, or unexplained ignored test.
5. A completed immutable manifest binds the source, executable, database schema/fingerprint,
   environment, gate registry, logs, and all proof artifacts by SHA-256.
6. Thirty owner daily-use sessions finish without a P0, P1, or core-loop P2 incident.

This verdict is about product reliability on this workstation. It is not a public Windows-product,
multi-user, external-reviewer, or ASR-superiority claim.

## Scope lock

### Required

- Local library, import, exact champion transcription, playback, review, undo/redecision, restart,
  backup, restore, validation, and export.
- Owner review truth consists of durable accept, correction, reject, and structured technical-
  unusable flag actions. “Next — no decision” is navigation only: it writes no review event, changes
  no revision, earns no credit, and is not an Undo target. A durable `skip` action is deliberately
  outside the single-owner product contract.
- Internal concurrency among the UI, database, import, ASR/WSL, backup, export, jobs, and recovery.
- Fully offline default operation and renderer-safe errors.
- Real schema-65 production-clone compatibility without touching the live database during
  development or destructive drills.
- The active pool boundary must not corrupt or block the owner workstation, but pool/Couch/reviewer
  implementation remains owned by the separate pool agent.

### Not required for this verdict

- Signed MSI/NSIS artifacts, updater, public release tags, or clean-VM distribution proof.
- Windows servicing-release matrices, public uninstall/update/rollback certification, or support for
  another PC.
- Comparator studies, five-user pilots, external reviewers, or a top-three-ever claim.
- Gold Marathon, IAA, CORDI, or broad ASR superiority. Those remain model-evidence work.
- New features, speculative abstractions, cloud ASR, or another model family.

## Starting evidence: honest, not a verdict

| Area | Current evidence | Status |
|---|---|---|
| Git | Clean `codex/10-10-integration` at the starting commit | green baseline |
| Rust architecture | Current mechanical module/dependency ceiling passes | green |
| Rust functional suite | Exact integrated production source passed broadly with zero failures | green but not certifying |
| Rust ignored inventory | 43 explicit opt-in tests remain across real media/model/network/bench paths | red until routed |
| Frontend functional suite | 494 unit tests and 101 zero-retry E2E tests passed on the integrated line | green but not certifying |
| IPC | 58 generated, 56 handwritten, one dynamic bridge across 114 invoked commands | red |
| Frontend architecture | Ten Svelte owners exceed their locked line ceilings | red |
| Frontend coverage | Last measured global coverage is well below 85/80 thresholds | red; remeasure at current SHA |
| Python metric proof | `jiwer` 4.0.0 exists in the WSL lock, but one desktop policy treats absence as optional | red |
| Owner evidence classes | Clone/restore, hostile campaigns, workflow, deployment/reboot, field sessions, attestation | pending |

Passing source tests proves that known assertions are green. It does not prove the product verdict.

## Milestone 0 — freeze the owner contract and current baseline

1. Teach the verifier that `owner-product` excludes public-distribution and external-human evidence
   while retaining every local truth, recovery, privacy, performance, and field-use requirement.
2. Inventory every owner command, process, database, filesystem, port, model, and runtime dependency.
3. Run one non-certifying baseline at the starting commit and preserve all failures and artifact
   hashes. No retry result may become the baseline.
4. Record the pool agent's exact integration commit and verify only its shared-database/API seams.

**Exit:** the gate registry has no irrelevant Windows-product dependency and no owner-critical hole;
the baseline manifest is immutable and the worktree is clean.

## Milestone 1 — make proof fail closed

1. Add one pinned desktop Python proof environment. Pin `jiwer==4.0.0` and every verifier dependency;
   a missing dependency is a hard gate failure, never a printed skip.
2. Classify all 43 Rust opt-in tests into:
   - owner-critical and explicitly executed by an owner gate;
   - deterministic hermetic tests that must join the default suite;
   - model-evidence or cloud-only tests that are absent from the owner profile by declared scope;
   - obsolete/duplicate tests to remove only after equivalent stronger proof is cited.
3. Require an explicit gate for each applicable opt-in test. `cargo test` reporting ignored tests is
   diagnostic; it is not release proof.
4. Implement validators for timeout calibration, three verifier fault campaigns, known defects,
   architecture, coverage/mutation, schema clone/restore, hostile performance, owner workflow,
   deployment/reboot, owner sessions, and the final attestation.
5. Make evidence writes, disk-full, stale status, missing `run_end`, retry, abandoned lease, and
   incomplete artifact publication fail closed.

**Exit:** an intentionally missing dependency, ignored owner test, stale artifact, retry, or broken
proof write makes `owner-product` red for the correct reason.

## Milestone 2 — close remaining backend and IPC authority debt

Work by authority domain and preserve one mutation path:

1. Characterize the remaining owner-critical raw database/command behavior before extraction.
2. Finish Tauri-free stores/services for owner review truth, jobs/import, export, recovery, rights,
   queries, and model/runtime status where legacy authority remains.
3. Migrate all 56 handwritten invoked commands to exact generated Specta bindings. Remove the one
   dynamic bridge only after every caller is generated and registered.
4. Keep `CommandErrorV1` renderer-safe and bounded; expose no secret, SQL, WSL internals, absolute
   private path, or raw native error.
5. Preserve operation UUID replay, base revision conflicts, exact undo, playback receipts, draft
   transactionality, champion hard-stop, restore admission, and append-only migrations.
   Exact Undo applies to every durable owner review action, including generic and technical flags;
   navigation-only “Next — no decision” has no inverse because it has no durable effect.
6. Do not edit pool/Couch/reviewer compensation implementation. If a shared seam fails, stop and
   coordinate against the pool agent's exact commit instead of creating simultaneous fixes.

**Exit:** zero handwritten/dynamic renderer IPC, zero command/handler SQL, zero raw connection
escape outside the narrow migration/recovery/test exceptions, strict Clippy/rustfmt green, and all
affected characterization/fault tests green.

## Milestone 3 — make the frontend one dependable workstation

1. Reduce `Workstation.svelte` to navigation/composition and extract testable controllers for
   startup, health, jobs, recovery, and workspace lifecycle.
2. Merge `ReviewMode` and `ReviewInbox` behind one review-session controller and the existing
   clip/attempt-bound audio state machine. Do not dual-write or rewrite human truth.
3. Split Settings, Insights, audio, validation, agent-report, waveform, and refinery owners along
   cohesive state/interaction boundaries until every locked line ceiling passes.
4. Preserve the failed-save contract: clip, draft, playback state, and focus stay put until the
   backend returns `CommittedReviewV1`; stale truth is shown side-by-side and never auto-merged.
5. Cover startup failures, lazy-load failures, event teardown, source changes, lost responses,
   conflicts, undo/redecision, keyboard guards, bidi, supported viewports, zoom, and reduced motion.
6. Keep initial JS at or below 125 KB gzip and CSS at or below 15 KB gzip.

**Exit:** zero direct component-level Tauri imports/invokes, no oversized Svelte owner, 100% typed
i18n parity, full functional/E2E gates green, and no build-budget regression.

## Milestone 4 — prove assertion quality, not test volume

1. Raise global backend coverage to at least 85% lines/regions and 80% functions/branches.
2. Raise review, playback, restore, IPC, and any shared compensation seam to at least 95%
   lines/regions and 90% branches.
3. Kill at least 90% of backend critical-domain mutants.
4. Raise global frontend coverage to at least 85% statements/lines and 80% branches/functions.
5. Raise critical review/audio/draft/error reducers to at least 95% lines/statements and 90%
   branches, and kill at least 80% of frontend reducer mutants.
6. Forbid coverage exclusions, lowered thresholds, unreachable-code games, or assertion-free tests.

**Exit:** hash-bound coverage and mutation reports satisfy every threshold at the exact source SHA.

## Milestone 5 — attack the local system

Use disposable databases and isolated restored clones only:

1. Run a 50,000-segment, 30-minute review/import/backup/export hammer with zero lost writes, lock
   failures, stale clobbers, or invalid restore admission.
2. Kill parent, child, WSL worker, native inference worker, and application processes at every
   publication boundary. Prove restart is idempotent and no partial transcript truth appears.
3. Inject disk-full, unwritable evidence, corrupt database, lost response, stale revision, occupied
   port, wrong model, hung child/grandchild, inherited pipe, and kill-during-manifest failures.
4. Run the 100,000-segment UI workload and 1,000-decision heap soak. Enforce the locked latency,
   responsiveness, resident-page/clip, and under-20-MB retained-heap budgets.
5. Intercept runtime sockets across startup, browsing, transcription, review, backup, and export;
   default mode must produce zero outbound connection with a working positive control.
6. Run every campaign three times with fixed recorded seeds where deterministic and preserve all
   failed attempts as diagnostics.

**Exit:** three clean hostile campaigns meet zero-loss, zero-lock, zero-egress, latency, and memory
budgets and leave no process, port, lease, lock, or partial status pointer behind.

## Milestone 6 — prove the real owner binary

1. Freeze one clean release candidate and generate source-tree, executable, gate-registry, bindings,
   schema, settings, and dependency hashes.
2. Create local and offsite snapshots; restore both into isolated locations and byte/fingerprint
   check their authoritative content.
3. Run fresh install/migration/reopen and a live-sized schema-65 production-clone campaign.
4. On the isolated clone, execute the real flow:
   `WAV import -> exact champion -> listen -> correct -> commit -> undo -> recommit -> validate -> export -> restart -> byte-check export`.
5. Repeat the flow with wrong-model, engine crash, process kill, disk-full, corrupt clone, lost
   response, kill-during-write, and kill-during-export faults. Every unsafe case must hard-stop or
   recover honestly.
6. Only after clone proof, activate the immutable local release pointer. Database rollback is never
   performed after newer-schema writes; recovery uses a compatible binary or admitted restore.

**Exit:** one exact executable passes the full real workflow and recovery campaign without touching
or weakening prior live truth.

## Milestone 7 — certify, then burn in with the sole user

1. Run the complete owner verifier at the exact release SHA before deployment: no retry or skip.
2. Deploy that exact executable locally and rerun: no retry or skip.
3. After an owner-approved cold reboot, rerun a third time: no retry or skip.
4. Complete thirty owner daily-use sessions. Any P0/P1 or supported core-loop P2 reopens the relevant
   milestone, invalidates the candidate, and requires a new exact-SHA proof sequence.
5. Publish `ProductAttestationV1` and the verdict only from the completed immutable proof manifest.

**Exit:** and only then, `CORTEX PRODUCT 10/10 — OWNER WORKSTATION`.

## Execution discipline

- One authority cluster per commit; no broad rewrite and no dual writing.
- Before each edit: characterize the current behavior and identify its serving-path consumer.
- After each edit: focused tests, policy tests, strict formatting/lint, then the affected broad gate.
- After each milestone: full clean-tree proof, finding-matrix update, progress ledger, and Obsidian
  evidence entry with exact SHA and artifact hashes.
- No failed run is erased. No green claim is copied from another commit.
- The live database and release pointer remain untouched until Milestone 6 authorizes activation.

## Immediate next slice

The first implementation slice is proof integrity, not a feature:

1. Pin the desktop Python verifier environment and make missing `jiwer` fail closed.
2. Produce the 43-test opt-in classification and wire every owner-critical test into explicit gates.
3. Refresh current frontend/backend coverage baselines at the exact clean SHA.
4. Commit only after the proof-policy suite demonstrates both the pass path and deliberate refusal
   paths.
