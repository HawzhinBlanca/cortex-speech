# Cortex Speech machine-only 10/10 closure plan

**Authority date:** 2026-08-27  
**Starting commit:** `d4df9251ebcef446258f07a68606762492af3729`  
**Branch:** `codex/10-10-integration`

This plan covers every remaining product-certification defect that can be implemented or measured
without recruiting reviewers, study participants, or pilot users. It does not weaken the
certification contract, change owner canon, deploy over schema-65 production, or turn source-level
evidence into a product verdict.

## Locked safety rules

1. The live database, active release pointer, reviewer credentials, production process and ports are
   read-only until one exact owner-product candidate has complete clone proof.
2. Migrations 1–65 remain byte-identical. New schema work is append-only.
3. Human truth is never dual-written. Every refactor retains one authoritative mutation path.
4. Coverage can improve only through executable semantic tests. Excluding production files,
   deleting reachable branches, lowering thresholds or marking tests ignored is not closure.
5. Module-size closure requires cohesive boundaries with characterization tests. Moving code into a
   differently named large file is not closure.
6. Every milestone starts clean, changes one authority cluster, passes focused proof, then passes
   repository-wide formatting, strict linting and affected policy gates before commit.
7. A retry, stale artifact, diagnostic override, recovered verifier lease or manually edited status
   cannot certify a release.

## Exact starting baseline

| Gate | Starting evidence | Required |
|---|---:|---:|
| Rust lines | 71,751 / 90,561 = 79.23% | 85% |
| Rust regions | 127,650 / 161,284 = 79.15% | 85% |
| Rust functions | 6,212 / 9,197 = 67.54% | 80% |
| Rust branches | 7,206 / 12,495 = 57.67% | 80% |
| Frontend statements | 6,335 / 11,642 = 54.41% | 85% |
| Frontend branches | 2,675 / 5,430 = 49.26% | 80% |
| Frontend functions | 1,516 / 2,778 = 54.57% | 80% |
| Frontend lines | 4,601 / 8,096 = 56.83% | 85% |
| Generated renderer IPC | 13 / 116 invoked | 116 / 116 |
| Oversized Rust modules | 6 | 0 |
| Oversized frontend owners | `Workstation`, `ReviewInbox`, `ReviewMode`, `SettingsPanel` | 0 |
| Certifying owner proof | absent | 3 clean runs at one SHA |

The starting Rust coverage artifact SHA-256 is
`040efce54e880903f7e724f340ab38191e9bfbc25766c7518b41fff5d5f6968d`. The starting frontend
summary SHA-256 is
`c8c0d37962ebf10ac2f2c67febe39e0f9a87b3cec721ea2aaf1ea88a019e84`.

## Milestone 1 — eliminate backend architecture violations

Apply a strangler refactor from the smallest independently testable boundary to the largest:

1. `eval.rs` — extract sealed export-generation publication and recovery.
2. `review_pool.rs` — separate compensation, invitation/session authority and pool decisions.
3. `commands.rs` — move remaining restore, media and orchestration services behind Tauri-free APIs.
4. `pipeline.rs` — separate import planning, champion drafting and publication orchestration.
5. `couch.rs` — separate lifecycle/session, routing, audio authorization and decision services.
6. `db.rs` — move remaining segment, review/payment/playback, import/jobs, provenance and query
   behavior into stores while retaining `Database` as the compatibility facade.

Exit criteria for each extraction:

- Original and new module are each below the applicable line ceiling.
- No new circular dependency, Tauri import in a store/service, raw connection escape or dual write.
- Focused characterization, fault and restart tests pass.
- `cargo fmt`, strict all-target/all-feature Clippy and the architecture gate pass for that slice.

## Milestone 2 — complete generated typed IPC

Migrate by authority domain, never by alphabetical bulk edit:

1. Review, playback and technical-unusable operations.
2. Compensation, pool and Couch administration.
3. Library, import and jobs.
4. Export, recovery and diagnostics.
5. Model, evaluation and advanced tools.

For each domain, Rust commands expose Specta-compatible request/response DTOs and
`CommandErrorV1`; checked-in bindings are regenerated; frontend services call generated functions;
legacy string adaptation is removed only after the entire domain is typed. CI must reject tracked
binding drift and any new raw invoke.

Exit criteria: 116/116 invoked commands generated, zero reachable raw `invoke`, zero secret/private
path fields in public DTOs, all E2E mocks derived from the same inventory.

## Milestone 3 — close backend coverage with semantic campaigns

Order follows consequence, not easiest percentage:

1. Review: conflict, replay, undo, draft, source-change and transaction-fault branches.
2. Payment: no-credit refusal, compensation replay, revocation and immutable-ledger branches.
3. Playback: attempt cancellation, interval union, source change, expiry and receipt replay.
4. Restore: every commit point, marker state, safety-pin and rollback failure.
5. IPC: validation, rate-limit, worker failure and typed error mapping.

Every critical domain must reach 95% lines/regions and 90% branches independently. Then close the
global 85% lines/regions and 80% functions/branches floor. Mutation tests must kill at least 90% of
critical backend mutants so line execution cannot masquerade as assertion quality.

## Milestone 4 — decompose and cover the frontend

1. Extract workstation navigation, job center, recovery, health and workspace lifecycle controllers.
2. Merge duplicated review state into one session controller and one audio attempt state machine.
3. Split Settings into the locked section architecture with typed field groups.
4. Cover startup, lazy failures, event lifecycle, every review conflict/retry branch and every
   supported responsive state.

No coverage exclusions are added. Presentational components remain below 350 lines;
controllers/workspaces remain below 500. Global frontend coverage reaches 85% statements/lines and
80% branches/functions. Unit, typecheck, lint, formatting, build-budget and zero-retry E2E gates all
remain green.

## Milestone 5 — hostile durability, concurrency and performance proof

All runs use generated disposable databases or restored production clones:

- 50,000-segment concurrent review/import/backup hammer for 30 minutes.
- Parent/child kill at every review, import, restore and export commit boundary.
- Disk-full, unwritable evidence root, corrupt database, stale response and occupied-port campaigns.
- 100,000-segment frontend performance run and 1,000-decision retained-heap soak.
- Native inference load/kill/crash campaign with zero partial transcript publication.
- Offline egress probes across startup, browse, transcription, review and export.

Exit criteria are the locked latency, memory, zero-loss, zero-lock-failure and zero-default-egress
budgets in the integration finding matrix. Every campaign stores exact seeds and artifact hashes.

## Milestone 6 — owner-product proof

At one clean release SHA:

1. Build the immutable release and record source/executable hashes.
2. Start the exact champion or hard-stop before mutation.
3. Execute WAV import → champion transcript → listen → correct → commit → undo → recommit → export
   → restart → byte-check on a clone.
4. Restore local and offsite snapshots in isolation.
5. Run the complete verifier three times with zero retry, skip, takeover or stale evidence: before
   deployment, after deployment and after cold reboot.

Only a completed immutable manifest plus `ProductAttestationV1` may publish owner-product status.

## Milestone 7 — machine-only Windows product work

Implement the opt-in signed updater contract, committed public key, HTTPS endpoint configuration,
signature rejection and interrupted-update recovery. Build signed MSI/NSIS artifacts when a real
certificate is available; otherwise keep signing evidence explicitly pending. Run clean Windows 11
VM install/update/rollback/uninstall drills on the two supported servicing releases, including
offline WebView installation and user-data preservation.

## Deliberately deferred human evidence

Native Sorani copy approval, manual NVDA/keyboard/zoom/high-contrast evidence, comparator usability,
30 owner sessions and the external pilot remain separate release gates. Machine-only completion does
not invent or waive them and therefore cannot by itself justify `windows-product` 10/10 or a
top-three-ever claim.

## Progress accounting

The finding matrix remains the executable ledger. After every milestone, record the exact commit,
tests, artifacts, coverage deltas, remaining failures and proof limitations. A milestone is closed
only when its named gate changes from red to green without making another gate weaker.
