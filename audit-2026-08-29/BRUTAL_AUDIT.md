# Cortex Speech — Deep, Brutally Honest Audit

**Audit date:** 2026-08-29 (Asia/Baghdad)
**Audited branch:** `codex/10-10-integration`
**Audited HEAD:** `31a5018d4a4baf320a1b1b7e0ff5b12a2590841c`
**Scope:** current local working tree, architecture, Rust backend, Svelte/TypeScript frontend, tests, proof verifier, release controls, and relevant project canon
**Mutation policy:** read-only audit. No production database, model service, release pointer, or user data was changed.

## Executive verdict

**This working tree is not releasable. It is not close enough to call a release candidate.**

The project is not fake, shallow, or carelessly built. It contains serious engineering: fail-closed behavior, broad automated testing, typed IPC generation, explicit proof requirements, security checks, and an unusually honest attempt to separate claims from evidence. The source-reference privacy path that earlier notes questioned is now correctly wired and its focused tests pass.

But the current state is a large, uncommitted integration laboratory:

- the production frontend build fails;
- the full Rust library suite has 35 failures;
- at least two Python policy checks independently fail;
- five release-evidence classes are still explicitly unsupported by the verifier;
- the flexible review pool is disabled in production while tests compile and exercise a different path;
- its fallback undo design has a credible lost-response/restart data-integrity failure mode;
- the audited state spans 377 changed paths, including 165 untracked files, so the result is not reproducible from HEAD.

The blunt version: **the project currently has more release machinery than release evidence.** Its controls are valuable, but a control that correctly says “not proven” is not a substitute for proving the product.

## Judgment scores

These scores are audit judgments, not measured product metrics.

| Dimension | Score | Why |
|---|---:|---|
| Engineering ambition and system design | 7/10 | Strong safety posture, explicit canon, rich verification framework, and meaningful modularization. |
| Current correctness evidence | 4/10 | Large passing surface, but the full backend suite and policy suite are red, and key operational behaviors remain unexecuted. |
| Maintainability | 5/10 | Frontend decomposition improved and architecture gates pass, but several central files and the verifier remain very large, while documentation has drifted. |
| Operational/recovery proof | 2/10 | Required owner-field, reboot, restore, concurrency, and mutation evidence is absent or deliberately rejected. |
| Release readiness of this exact tree | **2/10** | Dirty and unreproducible state, failed production build, failed Rust suite, failed policies, unsupported proof classes. |

## Evidence snapshot

### Repository state

- 894 tracked files.
- 212 unstaged paths.
- 5 staged paths.
- 165 untracked paths.
- 377 distinct changed paths in total.
- The unstaged diff alone spans 212 files, approximately 48,197 insertions and 16,455 deletions; the staged diff adds approximately 3,521 insertions.
- Changes touch Rust, Svelte/TypeScript, Python verification, proof/release machinery, and new core modules.
- `git diff --check` passes; only line-ending warnings were emitted.

This is too much unpublished state to audit as a reproducible release unit. HEAD does not represent the product that was tested.

### Verification results

| Check | Result | Audit interpretation |
|---|---|---|
| `npm run typecheck` | PASS | 0 errors and 0 warnings. |
| `npm run lint` | PASS | Frontend lint gate is clean. |
| `npm run format:check` | PASS | Frontend format gate is clean. |
| `cargo fmt --all --check` | PASS | Rust format gate is clean. |
| `npm test` | PASS | 128 files and 1,008 tests pass, with an important false-signal caveat described below. |
| `npm run build` | **FAIL** | Initial JavaScript is 128.05 KB gzip against a 125.00 KB budget; over by 3.05 KB. |
| Full Rust library suite | **FAIL** | 1,938 passed, 35 failed, 8 ignored; exit 101 after 996.96 seconds. |
| Focused source-reference tests | PASS | 32 passed, 0 failed. |
| Rust architecture policy | PASS | 165 measurements, 0 failures; production module ceiling respected. |
| IPC contract policy | PASS | 120 invoked commands, 140 registered, one deliberately closed low-level bridge. |
| Python policy checks | **FAIL, incomplete count** | At least two failures were independently reproduced. The complete aggregate was not claimed after the runner/UI repeatedly failed. |
| `npm audit --omit=dev` | PASS | 0 reported vulnerabilities. |
| `cargo deny check` | PASS | Dependency policy passed. |
| Current backend coverage | NOT PROVEN | Not rerun successfully for this working tree. Prior vault measurements are stale relative to these changes. |
| Backend mutation outcomes | NOT PROVEN | An inventory exists, but a complete mutation campaign has not been executed. |
| Owner field/reboot/power-loss campaigns | NOT PROVEN | Required raw evidence is absent or explicitly unsupported. |

## Ranked findings

### P1 — The production build is red

`npm run build` completes Vite compilation and then fails the enforced bundle budget:

- initial JavaScript: 128.05 KB gzip;
- allowed: 125.00 KB gzip;
- excess: 3.05 KB.

The limit is enforced by `scripts/check_bundle_budget.mjs` (not merely advisory), so this exact source cannot produce a gate-passing production frontend artifact. Any release claim made before this is fixed or the budget is deliberately re-baselined would be false.

### P1 — Thirty-five backend behaviors do not complete their tests

The full Rust library run ends with:

```text
test result: FAILED. 1938 passed; 35 failed; 8 ignored
```

The failures cluster in couch review, review-pool, and review-pool-export behavior. Inspection shows a common immediate cause: test fixtures assert rollback/reapplication through schema 67, while the source schema is now 69. Examples occur around:

- `cortex-speech-app/src-tauri/src/couch.rs:3146`, `:4113`, and `:6434`;
- `cortex-speech-app/src-tauri/src/review_pool.rs:2031`, `:2051`, and `:2387`;
- `cortex-speech-app/src-tauri/src/review_pool_export.rs:925`.

This does **not** prove there are 35 independent runtime bugs. It most likely proves systematic fixture drift. It still matters: the fixtures fail before the intended behaviors are exercised, so those behaviors are currently unproven and the backend gate is unambiguously red.

### P1 — The audited product cannot be reproduced from its commit

The tested product includes 377 changed paths not represented by the stated HEAD, including 165 untracked files and newly introduced core modules. A verifier manifest tied only to `31a5018d...` would not identify the product audited here.

Until the work is divided into coherent commits and all gates are rerun from an exact clean SHA, there is no trustworthy mapping between source, evidence, and release candidate.

### P1 — Five proof categories are structurally incapable of turning green

The verifier still unconditionally rejects unsupported owner evidence in five areas:

- schema clone and restore (`scripts/verify_10.py:6557-6560`);
- concurrency and performance (`:6786-6789`);
- owner workflow and recovery (`:6990-6993`);
- owner field sessions (`:7133-7136`);
- deployment and reboot (`:7436-7439`).

This is honest fail-closed behavior, not a verifier defect. It is a hard release blocker. Adding projection files or prose attestations cannot legitimately bypass these rejections; raw producers and consumers must be wired and executed.

Coverage/mutation verification has the same evidence problem when raw replay is missing (`scripts/verify_10.py:5922-5926`).

### P1 — Pool undo can target an older decision after a lost response and restart

This is the most serious code-derived integrity risk found.

The pool undo endpoint is bodyless. In `couch/decisions.rs:1566-1591`, it uses an in-memory undo stack and, after restart or stack loss, falls back to `review_pool::latest_decision`. That query reads the latest *effective* decision (`review_pool.rs:1918-1933`). The effective view excludes decisions already reversed (`migrations/mod.rs:4157-4162`).

A credible sequence is:

1. the user undoes the newest decision;
2. the server commits the reversal, but the response is lost;
3. the process restarts, losing the in-memory token;
4. the client retries the bodyless undo;
5. the already reversed newest decision is absent from the effective view;
6. fallback selects and reverses the previous decision.

`reverse_decision` is idempotent only when supplied the same decision identity and operation identity. The bodyless retry supplies neither. Existing tests cover normal undo, not this response-loss/restart sequence.

This risk is **latent rather than currently exposed**, because production pool decisions are disabled. It becomes P1 immediately if the pool is enabled without a durable, request-addressed undo protocol.

### P1 — Tests and production compile different review-pool behavior

In `couch/decisions.rs` around `:736-746`:

- test builds route pool decisions into `api_pool_decision`;
- non-test builds return HTTP 503 with `PAY_POLICY_REQUIRED`.

The lifecycle code also documents that external pool work is unpaid/disabled. Failing closed until compensation policy is settled is the correct ethical choice. The verification consequence is uncomfortable but simple: a large body of pool tests exercises functionality the shipped build refuses to expose.

Therefore, statements such as “the flexible pool is working in production” are not supported. The actual production behavior is “feature unavailable by policy.” This split should be explicit in product status and release evidence, not buried behind conditional compilation.

### P2 — The Python policy layer is already red before a complete aggregate run

At least two policy failures were reproduced:

1. `test_activate_review_pilot.py` seeds schema 67 while activation correctly derives the current required schema as 69, then fails because migrations 68 and 69 are missing.
2. `test_backend_layering_policy.py` rejects production command-layer references to `rusqlite::`; `commands/segments_write.rs:977-980` pattern-matches `rusqlite::Error::SqliteFailure` and SQLite error codes.

The first is more fixture drift. The second is genuine layering leakage, even though it is error classification rather than an inline SQL query. SQLite-specific busy/error interpretation belongs below the command layer.

Because the complete Python aggregate was interrupted by repeated runner/UI failures, this audit intentionally does not invent a total policy failure count. “At least two independently reproduced failures” is the strongest honest statement.

### P2 — One green frontend test emits a real browser-operation error and barely asserts the behavior

The 1,008-test frontend suite is green, but jsdom emits:

```text
Not implemented: navigation (except hash changes)
```

The path comes from `window.location.reload()` in `StatsDashboard.svelte` around line 301. The related test in `tests/lib/StatsDashboardDesktop.test.ts:385-404` says it verifies the reload boundary, but does not assert that reload occurred; it waits for stats calls instead.

This is not a proven production crash—real browsers implement reload. It is a test-signal defect: stderr reports a failed operation while the suite remains green, and the named boundary is not actually asserted.

### P2 — Coverage and mutation strength are not current evidence

The last clean Rust measurement recorded in the vault was approximately:

- 79.23% lines;
- 79.15% regions;
- 67.54% functions;
- 57.67% branches.

Those values were below the project's stated 85/85/80/80 targets and predate substantial working-tree changes. They must not be presented as current coverage. The backend mutation system has an inventory of roughly 4,348 mutants, but no completed outcome campaign.

The brutal interpretation: counting tests is flattering; measuring the behaviors they fail to kill is harder and has not yet been done.

### P2 — Architecture documentation materially trails the code

`cortex-speech-app/ARCHITECTURE.md` still describes an older system: roughly 20–24 commands, synchronous pipeline assumptions, older schema/module details, truncated long-file behavior, and failed ASR chunks being skipped. Current source has approximately 140 registered commands, generated typed IPC, blocking-work isolation, schema 69, and fail-closed champion behavior.

Stale architecture documentation is dangerous in this project because the release model depends on reviewers knowing which path is authoritative. The code is healthier than this document suggests in some places and substantially different in others.

### P2 — The known-defect ledger is not an adequate view of current risk

`docs/KNOWN_DEFECTS.v1.json` lists only three open items: two P3 architecture items and one P2 pay-policy item. It does not represent the current failed production build, failed backend suite, stale-schema test infrastructure, unsupported evidence categories, or the pool undo retry/restart risk.

A defect ledger does not need to duplicate every failed CI check, but a release-governance project needs a visible bridge from red gates to owned remediation. That bridge is currently incomplete.

### P3 — Central files remain expensive to reason about

The architecture gate passes its production limits, which is a real improvement. Nevertheless, several central surfaces remain large:

- `src-tauri/src/lib.rs`: about 3,094 physical lines;
- `src-tauri/src/commands.rs`: about 5,813 physical lines, much of it test/support content;
- `src-tauri/src/db.rs`: about 2,065 physical lines;
- `src-tauri/src/pipeline.rs`: about 1,912 lines;
- `scripts/verify_10.py`: about 12,789 lines / 571 KB;
- frontend command/event bridges: roughly 1,734 and 1,212 lines.

This is not automatically bad code. It does increase the cost of proving changes, especially when a single verifier combines policy, evidence parsing, business rules, and reporting.

## Important positive findings

The audit would be dishonest if it ignored what is working.

1. **The source-reference snapshot path is correctly wired now.** The generator receives and uploads the immutable snapshot path, while the original source is used only for provenance/redaction checks. Snapshot and original are verified, and the temporary snapshot is cleaned before persistence. The focused suite passes 32/32. Earlier vault warnings on this point are stale.
2. **The frontend static gates are clean.** Type checking, linting, and formatting all pass.
3. **The broad frontend suite is genuinely large.** 1,008 passing tests are meaningful, even with the reload-test caveat.
4. **The Rust architecture policy passes.** Modularization work has produced measurable improvement rather than cosmetic file movement.
5. **The IPC contract is explicitly audited.** Generated contracts cover most invoked commands, handwritten exceptions are enumerated, and the low-level escape bridge is closed.
6. **Dependency hygiene is currently good.** Both the production npm audit and `cargo deny` pass.
7. **Fail-closed choices are visible.** Unsettled compensation, unsupported proof, and missing raw evidence are rejected rather than silently waved through.
8. **The status document avoids inventing a release verdict.** This is the right behavior while proof remains incomplete.
9. **No obvious production TODO/FIXME/HACK/unimplemented markers or real committed API secrets were found in the targeted scans.** A secret-like value found was a deliberate dummy test value.

## What must happen next, in order

### 1. Make the source auditable

- Partition the 377-path working tree into coherent commits.
- Separate generated artifacts, proof outputs, fixtures, source changes, and documentation.
- Ensure untracked core modules are committed or deliberately removed.
- Establish one exact candidate SHA and stop changing it during certification.

**Exit criterion:** a clean working tree whose SHA exactly identifies every tested source file.

### 2. Restore baseline gates

- Make migration fixtures derive their expected range from the migration catalog/current schema instead of hard-coding 67.
- Rerun the full Rust suite and require zero failures.
- Fix the 3.05 KB bundle-budget regression, or deliberately re-baseline the budget with a documented decision and bundle analysis.
- Remove `rusqlite` knowledge from command-layer code.
- Repair the navigation/reload test so it asserts the actual browser boundary and does not emit ignored stderr.
- Run the complete Python policy suite and record the exact result.

**Exit criterion:** format, lint, typecheck, production build, all Rust targets, frontend tests, and all policy gates pass from the exact candidate SHA.

### 3. Resolve pool product truth before enabling it

- Settle and document the pay/compensation policy.
- Decide whether the pool is part of the owner product now or explicitly out of scope.
- Replace bodyless “undo latest effective decision” with a durable request containing at least the target decision ID and an idempotency/reversal operation ID.
- Persist the undo receipt so response-loss retries survive restart.
- Add a regression test for commit → lost response → restart → retry.
- Test the same code path that production compiles.

**Exit criterion:** production and tests share the behavior, pay policy is settled, and the lost-response/restart sequence cannot reverse an older decision.

### 4. Produce raw evidence instead of more attestations

- Wire and run schema clone/restore evidence.
- Run concurrency/performance evidence.
- Run owner workflow/recovery and field sessions.
- Run deployment/reboot and an appropriately controlled power-loss recovery exercise.
- Execute the backend mutation campaign, not just inventory generation.
- Rerun current frontend and backend coverage against the exact candidate.

**Exit criterion:** the verifier consumes raw artifacts with provenance and no unsupported-evidence rejection remains.

### 5. Certify only the exact clean candidate

- Generate manifests and attestations after all evidence exists.
- Verify artifact hashes, source SHA, schema, model identity, and release pointer agree.
- Run the final owner-path smoke/field sessions without editing the candidate afterward.

**Exit criterion:** a clean, reproducible candidate passes the complete verifier and the actual production path, not a test-only approximation.

## Claims this audit supports

I am confident that:

- this exact working tree is not releasable;
- the production build is currently red;
- the backend test gate is currently red with 35 failures;
- schema-67 fixture drift explains the immediate failure point for the observed review/pool cluster;
- at least two Python policies currently fail;
- five required proof classes remain explicitly unsupported;
- the flexible pool is disabled in non-test builds;
- the current pool undo fallback has a credible lost-response/restart hazard;
- the source-reference snapshot path is now correctly connected and focused tests pass;
- the current source cannot be reproduced from HEAD because of the dirty/untracked state.

## Claims this audit does not support

I will not claim that:

- all 35 Rust failures are production defects;
- the full Python suite has only two failures;
- old coverage numbers describe the current tree;
- the product survives power loss, reboot, restore, or owner field use without the missing raw campaigns;
- the pool bug is currently user-reachable while production pool decisions remain disabled;
- passing unit tests alone prove release readiness;
- a verifier can compensate for a source state that is not committed and reproducible.

## Final assessment

Cortex Speech has the bones of a serious owner-grade system, but the current repository state is overextended. The design has accumulated a sophisticated vocabulary of canon, proofs, policies, manifests, campaigns, and attestations while the basic candidate still fails to build, fails its full backend suite, and cannot be reconstructed from its commit.

That is fixable. The project does not need another layer of certification language right now. It needs consolidation: one clean candidate, zero baseline failures, one production-equivalent behavior path, durable undo semantics, and raw operational evidence. Until then, the only defensible release verdict is **NO-GO**.
