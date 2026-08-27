# Cortex Private-Production 10/10 Completion Audit — 2026-08-24

## Verdict

**Reviewer line: PASS / GO. Dataset completion: IN PROGRESS.**

The engineering requirements that can be completed without human judgments or GPU availability are
implemented and measured. The active immutable reviewer release is
`3aca5885867b-3e1014f35b6f-af72e4ccbf6a-363def17e69f`, built from
`3aca5885867bc77a87fe669cbee48556a84a04a5`. Schema 65 binds the immutable 20,323-row
source pool to 16,990 canonical review clips, excludes 3,333 duplicate aliases across 2,903
families, and reports zero unconfirmed duplicate risk. Schema 65 also preserves rights-revocation
and metadata lineage on excluded duplicate audit rows without allowing those rows to gain review
authority. Audit correction
`18423dee480ac5bcff577d02c5d22b02415afc68` improves parallel test isolation without changing the
reviewer API, database schema, pool, model route, or runtime consensus behavior; the live release was
not interrupted for this proof-only correction. Operational proof correction
`cfa403162582b799c50bcb070c70d4a47f2f8d12` makes the link gate and release controller explicitly
mode-aware; it also changes no reviewer-runtime semantics and required no live handover.
Master-proof correction `bd9235fb572d2e849f0ad7a7d869764b7a5f254f` extends that selection to
review certification, deferred compensation, and playback evidence while preserving every strict
legacy-mode rule. Queue-proof correction `3aca5885867bc77a87fe669cbee48556a84a04a5` makes the
standalone runway checker select the same immutable flexible-pool authority as the server instead of
reapplying the superseded 6,922-ID Lamo JSON focus.
Master-verifier correction `0983d54eef9acde19c2fe6265017dd83e28ccec5` closes four proof drifts
found by an unabridged sweep: the duplicate audit now verifies the v64 manifest/exclusion counts and
audits only the canonical overlay; the schema contract replays migrations 57 and 60-65, including
the v65 trigger replacement; runtime probes hash-verify and select the immutable active release
instead of requiring a mutable developer build; and two stale Rust fixtures now follow the
champion-only model fallback and schema-65 rollback boundary. No production runtime code changed, so
the healthy reviewer release was not restarted.
Fuzz-proof correction `bc1b5f7b5c186f83f7a15b2b7eaa74627ec8dd52` makes the final deep gate
bounded and repeatable on Windows: WSL stores only derived ASAN build/runtime state in a
per-checkout Linux cache, builds all four harnesses once, executes the exact built binaries without
cargo-fuzz's redundant second Cargo build, preserves its ASAN/artifact/corpus semantics, and refuses
exit-zero runs without a positive libFuzzer `DONE` iteration count. The measured final run completed
5,695,335 iterations across cache, diff, normalizer, and validation with zero crashes.

> **Superseded evidence correction:** an initial schema-64 handover refused exposure because the
> Python snapshot manifest omitted the two schema-64 dedup authority counts. No reviewer data was
> lost and the candidate was not exposed. Commit `fc22c10ef3e3cb680fc85e2eaa12fc5737633150`
> binds those tables in both snapshot and restore evidence; the protected redeployment and isolated
> offsite restore passed. See the permanent incident record in Obsidian.

This is a measured private-production result, not a claim of universal flawlessness, public-store
readiness, or a completed dataset.

## Resolution authority

- Two **distinct** non-skip reviewers may resolve a clip; there is no assigned first/second pair.
- A keep outcome is the exact NFC-normalized, outer-trimmed transcript; reject is a separate outcome.
- Two matching outcomes resolve. Two different outcomes admit one blinded third judgment. Any matching
  pair among three resolves; three distinct outcomes enter the owner-only conflict queue.
- Skip records “seen” but contributes no judgment. Undo is append-only, invalidates dependent
  resolution, and safely requeues. A resolved or three-way-conflict clip receives no fourth review.
- Dialect, focus, audio-availability, playback, row-version, identity, and idempotency filters remain
  enforced before a judgment can become authority.

Primary evidence: `src-tauri/src/review_pool.rs` tests
`every_accept_edit_reject_skip_pair_has_the_exact_consensus_semantics`,
`reviewer_identity_is_distinct_and_case_trim_normalized`,
`disagreement_gets_one_blinded_third_review_then_resolves_by_matching_pair`,
`three_distinct_outcomes_require_owner_and_owner_ruling_is_evidence_bound`, and
`consensus_is_nfc_and_outer_trim_exact_but_keeps_punctuation_distinct`.

## Requirement matrix

| Plan requirement | Result | Authoritative evidence |
|---|---|---|
| Isolated schema-63/64/65 engineering and clone-first migration | PASS | Dedicated `codex/review-production-v63` worktree; external build roots; live-sized clone preflights passed before protected handovers. |
| Preserve links, sessions, decisions, operation IDs, outbox, and undo | PASS | Real Reviewer A/Reviewer B localhost and Funnel authentication, valid WAV and idempotency probes after the v63 handover; 877 historical events preserved and no synthetic pool decisions created. |
| Stop resolved circulation; allow exactly one third review | PASS | Review-pool queue and full 16-pair action matrix tests; concurrent HTTP reviewer tests; live queues show unresolved work without synthetic resolution. |
| Queue p95 ≤750 ms; commit p95 ≤500 ms | PASS | Live-sized release: Reviewer A 153.54 ms, Reviewer B 150.97 ms; two-reviewer decision commit 4.889 ms. |
| Mode-aware verification | PASS (implementation) | Links, queues, final review authority, compensation, and playback select the live mode. Flexible mode proves the hash-bound release/admin, exact reviewer-specific pool queue, champion/voice/report consistency, deferred-pay namespace isolation, and effective pool playback evidence; legacy mode retains its exact hidden canary and ledger rules. Fresh local/Funnel authentication and live certification pass. The genuine post-release playback sample remains an external evidence gate. |
| Safe deployment and compatible rollback | PASS | Immutable hash-bound release, pre-migration snapshot, maintenance marker, clone preflight, post-exposure auth/queue/audio/idempotency/supervision gates, and schema-aware rollback controller tests. |
| Exact owner rights, fail closed on conflicts/revocation | PASS | Live certification reports exact rights for every active owner recording; idempotent/scoped/conflict/revocation Rust tests pass. |
| Per-voice certificates and independent finalization | PASS (implementation) | Certificate binds pool/focus/champion/deployment/rights/audio/resolution/reviewers/export; each voice is independently selected. Runtime certificates correctly wait for completed human resolution. |
| Pool-native ASR/TTS export | PASS (implementation) | Reject exclusion; ASR 16 kHz mono PCM16; TTS 24 kHz PCM16; whole-master byte preservation; exact-sample bounded extraction; deterministic manifest; sync and atomic publication; tamper/crash recovery tests. |
| Read-only five-minute certification | PASS | Report schema 3 separates consensus, owner adjudication, conflicts, and duplicate authority; watchdog publishes without taking leases or mutating the source DB. |
| Local and `F:` RPO ≤10 min; daily isolated RTO ≤5 min | PASS | Fixed nine-minute monotonic capture deadlines; fresh verified live local/offsite schema-65 snapshots; scheduled restore result zero; post-release offsite restore measured 3.925 s. |
| Future imports isolated and champion-only | PASS | Batch importer requires an explicit existing staging data root, rejects live/ancestor/descendant/alias paths, and accepts only exact local OmniASR-7B champion evidence. |
| Migration/future-schema/partial-failure tests | PASS | Real v62→v63, v63→v64, and v64→v65 paths, restart/reapply, reversible down paths, atomic failure, incomplete-history refusal, and future-schema refusal. |
| Retry/restart/network/concurrency durability | PASS | 1,000 lost-response retries across 20 DB reopen cycles; 25 forced process crashes; concurrent reviewer hammer and mid-session restart tests; zero duplicated authority. |
| Clean engineering verification | PASS | Rust library 1,541 passed, 0 failed, 8 intentional hardware/model/isolated-benchmark ignores; counted aggregate 1,649 tests across 43 binaries; pool-admin 6/6; importer 3/3; frontend 292/292 and browser E2E 97/97; all 106 Python policy scripts; four ASAN fuzz targets / 5,695,335 iterations / zero crashes; schema-65 snapshot 26/26, restore 21/21, release 11/11; strict Clippy/format/lint/typecheck/build green. |

## Live checkpoint

Independent post-deploy certification reported exact commit `3aca5885867bc77a87fe669cbee48556a84a04a5`,
schema 65, healthy quick/full integrity, zero foreign keys, complete canonical audio and rights,
`reviewReady=true`, 16,990 unresolved canonical clips, zero owner conflicts, and zero owner
adjudications. With the live dialect policy applied, Reviewer A had 16,988 eligible clips and Reviewer B 14,041;
both sampled valid WAV data and proved submission idempotency. Both links authenticated read-only over
local HTTPS and Funnel, the 14-assertion supervision gate passed, and the watchdog's last result was
zero. Fresh schema-65 snapshots verified on local and `F:` storage; the offsite snapshot restored in
3.925 seconds. No GPU, WSL model process, or ASR transcript was changed.

The post-`0983d54` read-only checkpoint again reported schema 65, quick/full integrity `ok`, zero
foreign keys, 16,990/16,990 canonical audio, exact rights on 20,323/20,323 source rows, zero
unconfirmed duplicate risk, and fresh verified local/offsite snapshots. The canonical duplicate
gate independently rescanned all 16,990 eligible clips and found zero cross-file duplicate content.
Reviewer B and Reviewer A both authenticated through Funnel, supervision passed, and the active immutable
binary completed 2,389 positive-control-backed offline workload loops with zero non-loopback backend
TCP endpoints. Human state remained two genuine Reviewer A judgments, zero resolved clips, and 0/20
post-release playback decisions; these numbers were not fabricated or backdated.

The post-`bc1b5f7` read-only checkpoint remained review-ready: schema 65 and all 162 protected
contract objects matched; quick/full integrity returned `ok`; foreign-key violations, missing audio,
rights gaps, revocations, and unconfirmed duplicate risk were zero. Reviewer B and Reviewer A authenticated,
supervision passed with 424.8 GB free, and verified local/offsite snapshots were 253/251 seconds old.
Human state was still two Reviewer A judgments, zero resolutions, and 0/20 current-release playback
decisions. The reviewer process was not restarted and no live write, GPU, model server, or ASR
inference was used.

## External completion gates

These are real remaining work, not engineering defects to fabricate away:

1. Resolve all 16,990 canonical clips through matching independent judgments, a matching pair among three, or
   explicit owner adjudication; current state is two genuine judgments and zero resolved clips.
2. Reach zero three-way conflicts, missing audio, rights gaps, integrity errors, and stale snapshots.
3. Kawa requires another Hawleri-capable reviewer besides Reviewer A; Reviewer A cannot self-confirm.
4. After each voice resolves, run its real certificate/export and record the resulting digests.
5. Accumulate at least 20 ordinary post-release non-skip decisions across two reviewer browsers; the
   mode-aware playback gate must prove every one against its immutable pool decision and canonical
   listening receipt. Current release window: 0/20. No synthetic or backdated evidence is allowed.
6. When GPUs are free and sufficient human gold exists, execute one champion-versus-challenger cycle
   using only the fine-tuned OmniASR-7B family. Promotion requires a measured win; an honest rejection
   is a valid completed cycle.

Until those gates finish, **reviewers may work safely, but the final dataset must remain not ready**.
