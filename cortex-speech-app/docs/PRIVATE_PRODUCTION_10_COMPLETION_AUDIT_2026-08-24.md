# Cortex Private-Production 10/10 Completion Audit — 2026-08-24

## Verdict

**Reviewer line: PASS / GO. Dataset completion: IN PROGRESS.**

The engineering requirements that can be completed without human judgments or GPU availability are
implemented and measured. The active immutable reviewer release is
`8ef5d3c1b29e-8a999c88e220-2ad63448136e`, built from
`8ef5d3c1b29e41ea926bcf8ab22e3b8b2e68334d`. Audit correction
`18423dee480ac5bcff577d02c5d22b02415afc68` improves parallel test isolation without changing the
reviewer API, database schema, pool, model route, or runtime consensus behavior; the live release was
not interrupted for this proof-only correction. Operational proof correction
`cfa403162582b799c50bcb070c70d4a47f2f8d12` makes the link gate and release controller explicitly
mode-aware; it also changes no reviewer-runtime semantics and required no live handover.

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
| Isolated v63 engineering and clone-first migration | PASS | Dedicated `codex/review-production-v63` worktree; fresh external build root; live-sized clone preflight passed before both protected handovers. |
| Preserve links, sessions, decisions, operation IDs, outbox, and undo | PASS | Real Rubar/Alle localhost and Funnel authentication, valid WAV and idempotency probes after the v63 handover; 877 historical events preserved and no synthetic pool decisions created. |
| Stop resolved circulation; allow exactly one third review | PASS | Review-pool queue and full 16-pair action matrix tests; concurrent HTTP reviewer tests; live queues show unresolved work without synthetic resolution. |
| Queue p95 ≤750 ms; commit p95 ≤500 ms | PASS | Live-sized release: Rubar 153.54 ms, Alle 150.97 ms; two-reviewer decision commit 4.889 ms. |
| Mode-aware verification | PASS | `--require-private-production` proves the exact flexible-pool registry, complete membership, distinct durable reviewers, exact database binding, fixed port, and absence of a simultaneous legacy pilot; pre-pool databases retain the exact legacy fallback. Fresh local and Funnel checks authenticated Alle and Rubar read-only. |
| Safe deployment and compatible rollback | PASS | Immutable hash-bound release, pre-migration snapshot, maintenance marker, clone preflight, post-exposure auth/queue/audio/idempotency/supervision gates, and schema-aware rollback controller tests. |
| Exact owner rights, fail closed on conflicts/revocation | PASS | 20,323/20,323 live exact rights; idempotent/scoped/conflict/revocation Rust tests. |
| Per-voice certificates and independent finalization | PASS (implementation) | Certificate binds pool/focus/champion/deployment/rights/audio/resolution/reviewers/export; each voice is independently selected. Runtime certificates correctly wait for completed human resolution. |
| Pool-native ASR/TTS export | PASS (implementation) | Reject exclusion; ASR 16 kHz mono PCM16; TTS 24 kHz PCM16; whole-master byte preservation; exact-sample bounded extraction; deterministic manifest; sync and atomic publication; tamper/crash recovery tests. |
| Read-only five-minute certification | PASS | Report schema 2 separates consensus, owner adjudication, and conflicts; watchdog publishes without taking leases or mutating the source DB. |
| Local and `F:` RPO ≤10 min; daily isolated RTO ≤5 min | PASS | Fixed nine-minute monotonic capture deadlines; fresh live local/offsite snapshots; scheduled restore result zero; measured isolated restore 3.758 s. |
| Future imports isolated and champion-only | PASS | Batch importer requires an explicit existing staging data root, rejects live/ancestor/descendant/alias paths, and accepts only exact local OmniASR-7B champion evidence. |
| Migration/future-schema/partial-failure tests | PASS | Real v62→v63 clone, restart/reapply, reversible down paths, atomic v63 failure, incomplete history refusal, and future-schema refusal. |
| Retry/restart/network/concurrency durability | PASS | 1,000 lost-response retries across 20 DB reopen cycles; 25 forced process crashes; concurrent reviewer hammer and mid-session restart tests; zero duplicated authority. |
| Clean engineering verification | PASS | Rust 1,535 passed, 0 failed, 8 intentional hardware/model ignores; pool-admin 6/6; importer 3/3; frontend 292/292; browser E2E 97/97; Python policies 103/103; strict Clippy/format/lint/typecheck/build green. |

## Live checkpoint

The generated 08:49 +03:00 certification reported schema 63, healthy database, full audio and rights
coverage, `reviewReady=true`, `finalDatasetReady=false`, 20,323 unresolved clips, zero owner conflicts,
and zero owner adjudications. The five-minute watchdog remained result zero while the complete Rust
suite ran. No live database, link, session, reviewer lease, GPU, WSL model process, or ASR transcript
was changed by the audit.

## External completion gates

These are real remaining work, not engineering defects to fabricate away:

1. Resolve all 20,323 clips through matching independent judgments, a matching pair among three, or
   explicit owner adjudication; current state is two genuine judgments and zero resolved clips.
2. Reach zero three-way conflicts, missing audio, rights gaps, integrity errors, and stale snapshots.
3. Kawa requires another Hawleri-capable reviewer besides Rubar; Rubar cannot self-confirm.
4. After each voice resolves, run its real certificate/export and record the resulting digests.
5. When GPUs are free and sufficient human gold exists, execute one champion-versus-challenger cycle
   using only the fine-tuned OmniASR-7B family. Promotion requires a measured win; an honest rejection
   is a valid completed cycle.

Until those gates finish, **reviewers may work safely, but the final dataset must remain not ready**.
