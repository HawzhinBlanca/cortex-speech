# Cortex Speech Constitution

The non-negotiable principles every spec, plan, and implementation in this repo is checked against.
`AGENT_CHARTER.md` governs the deeper *why / when-to-stop*; `CLAUDE.md` governs *how to work here*;
this constitution is what `/speckit-plan` and `/speckit-analyze` gate against. When they conflict, the
**honesty principle (I) wins**.

## Core Principles

### I. Honesty — Never Fabricated (NON-NEGOTIABLE)
The project's entire credibility rests on real, never fabricated, results.
- Never invent, estimate, round, or "remember" any metric (WER/CER/F1/kappa/RTF/p-value/CI). Every number
  comes from a real run of the real harness, with the exact command + dataset/model SHA in the ledger.
- Nothing is "done" until it is **USER-OBSERVABLE or MEASURED on real audio**. "Tests pass" / "clippy
  clean" are necessary, not sufficient. Lead with the honest reality, then the progress.
- A bad result is shippable; a flattering fake one never is. When a metric is undefined (no scoreable
  data), render it as *undefined*, never as `0%`.
- If something cannot be verified here, say so plainly and hand it to the user's machine. Never imply
  verification that did not happen.

### II. Offline-First, Consent-Gated Cloud (NON-NEGOTIABLE)
- The production transcription path is **fully local**: OmniASR-7B Champion + Silero VAD. There is no
  cloud ASR runtime or fallback. The only approved cloud model is the fixed, advisory
  `gemini-2.5-pro`, and it requires the relevant explicit opt-in
  (`cloud_llm_opt_in` / `jury_cloud_opt_in`).
- Never send audio or a transcript to a provider without acknowledged consent. Never make cloud
  load-bearing in the default path. `settings.effective_llm_mode()` downgrades cloud → none when not
  opted in; `pipeline.rs` enforces it in both `llm_refinement_permitted()` and `build_refiner()`.
- Treat voice as biometric (GDPR Art. 9): enforce consent + license + attribution before any
  publish / train / redistribute step.

### III. Privacy & Secrets
- Never persist or echo API keys. Never hardcode a private profile path in any tracked file — the repo is
  PUBLIC (github.com/HawzhinBlanca/cortex-speech). `scripts/test_windows_repo_hygiene.py` enforces this
  across the whole git surface; use env vars / repo-relative / `__file__`-derived paths.

### IV. Real Verification — Gates Are the Bar (NON-NEGOTIABLE)
- A fix without a regression gate is incomplete. Add the test that would have caught the bug.
- Every change runs the relevant gates and pastes the real output:
  Rust `cargo fmt` + `clippy --all-targets -D warnings` + `cargo test`; frontend `npm run typecheck` +
  `lint` + `test` (vitest); `npm run test:python-policies`. Don't weaken, skip, or delete a gate to pass.
- For anything user-observable, verify on the **real exe** (rebuild + the smoke drive on real Kurdish
  audio), not just unit tests.
- **Adversarially verify your own fixes** — a second, skeptical pass over a just-applied fix repeatedly
  catches an incomplete fix or a missed sibling. This is mandatory for medium+ severity changes.

### V. Correctness Invariants
- Every per-segment audio operation slices the clip via `crate::chunking::slice_pcm_by_alignment` — a VAD
  chunk shares the whole-source `audio_path`; its range lives in `alignment_json`. Never operate on the
  whole file under one segment's transcript.
- Sorani text is **NFC + RTL**: NFC-canonicalize on every write path; give every Kurdish display container
  `dir="rtl"`. No silent data loss on any edit/undo/import path.

## Architecture & Constraints

- Stack: **Tauri v2 + Svelte 5 (runes: `$state`/`$derived`/`$effect`/`$props`/`$bindable`) + Rust**,
  SQLite (WAL) + FTS5, ~105 IPC commands, EN/CKB (RTL) localized UI.
- Pipeline: import → VAD chunk → ASR (OmniASR-7B Champion only) → optional consent-gated
  refine → review/annotate → validate → verify → export (JSON/JSONL/CSV/Parquet/HF/WAV).
- Scope is **personal daily use** by the owner (local Sorani transcription + dataset curation), not public
  product distribution. "Highest grade" = reliability + accuracy for daily use, NOT code-signing / store
  scorecards — don't chase distribution-cert items.
- Commits: one logical change per commit, **Conventional Commits**, on a **branch** (never straight to
  `main`), ending with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

## Development Workflow (Spec-Driven)

Use the spec-kit flow for any non-trivial feature, and check each artifact against this constitution:
`/speckit-constitution` → `/speckit-specify` → `/speckit-clarify` (de-risk ambiguity) →
`/speckit-plan` → `/speckit-checklist` + `/speckit-analyze` (consistency + constitution compliance) →
`/speckit-implement` → verify (gates + real-exe smoke).
- A spec/plan that would fabricate a metric, make cloud load-bearing by default, bypass a consent gate,
  leak a private path, or skip a quality gate is **non-compliant** — `/speckit-analyze` must flag it and
  the plan must change before `/speckit-implement`.
- Don't scope-creep: log out-of-scope ideas as separate tasks instead of implementing them.
- A feature is not "done" until its real positive test passes AND it is user-observable / measured on
  real audio (Principle I + IV).

## Governance

This constitution supersedes convenience and habit. It is the compliance bar for every `/speckit-plan`
and `/speckit-analyze` run. Amending it requires updating this file (bump the version below) and keeping
it consistent with `AGENT_CHARTER.md` and `CLAUDE.md`. Any added complexity must be justified against the
simplest design that upholds these principles. The honesty principle (I) is absolute and cannot be traded
for speed, completeness, or a nicer number.

**Version**: 1.0.0 | **Ratified**: 2026-06-28 | **Last Amended**: 2026-06-28
