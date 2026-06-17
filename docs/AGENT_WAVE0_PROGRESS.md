# agent/wave0 — Progress & Merge Handoff

Branch `agent/wave0` (off `f89dc4a`) delivers the agent-safe slice of the 10/10
roadmap, fully isolated from the in-flight `m02-sorani-metrics` working tree. Every
change was verified locally; nothing here was shipped unverified.

## What landed

| Commit | Area | Summary |
|--------|------|---------|
| `da4fa69` | Governance / Trust | `verify_10.py` made a **real gate**: JSON-Schema-validates the provenance ledger and **fails the build** on a share-alike / NonCommercial / no-redistribution corpus being marked for redistribution. Wired into CI (`governance-gate` job). Added `SECURITY.md`, `.github/CODEOWNERS`, `docs/provenance_ledger.schema.json`, and `datasetUsage` to the ledger. Fixed the `OMNIASR_MIGRATION.md` "Implemented header vs investigation body" contradiction. |
| `7613520` | Ship tooling | `Makefile` (`verify-10`, `gate`, `test-rust`, `lint`, `ship-check`) + `scripts/verify-10.ps1` Windows entrypoint. |
| `b4f7ec7` | **M3** Accuracy | `eval::run_gold_eval_with_transcriber` + `ProcessingPipeline::run_gold_eval_asr` / `transcribe_audio_file_raw` + `commands::run_gold_eval_asr`. Runs the **real sherpa OmniASR** over gold audio (zero caller text) — an honest CER is now *producible from audio*. |
| `b335124` | **M2** Accuracy | `significance.rs`: seeded segment-level **bootstrap CI** + **MAPSSWE** significance test + jiwer-equivalence cross-check. |
| `b925fd2` | **M6** Rigor | Crate-root `deny(clippy::unwrap_used, clippy::expect_used)` outside tests — a new prod `.unwrap()` now fails CI. 13 sites resolved (12 justified static-regex allows, 1 converted to `let-else`). |
| `2053ee3` | **M6** Rigor | proptest property tests for the statistics layer (the verifiable substitute for libfuzzer). |
| `4a5adb5` | Fix | The property test caught a real 1-ULP float bug (degenerate single-segment bootstrap CI); fixed at the root. |
| `760cabd` | **M6** Supply-chain | All 8 GitHub Actions pinned to commit SHAs across the 3 workflows (version kept as a comment for Dependabot/Renovate). |
| `d990b3f` | **M3** Accuracy | `#[ignore]`-gated real-audio test: drives `run_gold_eval_with_transcriber` through the **real** OmniASR engine → non-blank transcript + valid CER (runs in the nightly job; compile-verified here). |

## Verification (the three ship gates)

- **Governance gate:** `python scripts/verify_10.py` → `CORTEX 10/10: ALL GATES GREEN`.
  Proven to *bite*: flipping `asosoft_600`/`cordi_sorani` to `redistribute` makes it exit 1.
- **Lint:** `cargo clippy --all-targets -- -D warnings` passes (the CI gate).
- **Tests:** `cargo test --lib` → **375 passed, 0 failed** (incl. 6 eval, 13 significance).

## How to merge

```bash
git diff f89dc4a agent/wave0          # review (16 files, +925/-91)
# commit your in-flight m02-sorani-metrics work first, then:
git merge agent/wave0
git worktree remove --force ../cortex-agent-wave0   # cleanup; its models/ + dist/ are gitignored scratch
```

**Merge note:** M3 adds code to `eval.rs`, `pipeline.rs`, `commands.rs`, `lib.rs` —
files with uncommitted edits on `m02-sorani-metrics`. All additions are *additive*
(new functions / methods / registrations), so git should auto-merge most hunks; expect
to eyeball a few. Everything else (`significance.rs`, `scripts/`, governance, Makefile,
docs) is in files not touched on `m02` → clean.

## Intentionally deferred (not shipped — would be unverifiable or risky here)

- **libfuzzer fuzz targets** — need nightly + `cargo-fuzz` (not installable offline), so
  they can't be compile-verified here. The proptest property tests are the verifiable
  substitute; the libfuzzer harnesses are the only piece of "M6 remainder" still open.
  *(SHA-pinning — the other half of M6 remainder — is now done: commit `760cabd`.)*
- **M2b** (`language="ckb"` hint) and **M5** (FLAC + holdout-by-hash) overlap files you
  are actively editing (`asr.rs`, `export_audio/mod.rs`, `jury/learning.rs`).
- **M8b Autonomy Dial** — wiring it requires changing `run_t0_gate`'s signature, which
  ripples into `commands.rs` (in-flight). Best done after merge.

## What unlocks the North Star next (human/data-gated)

The code path now exists to **run M3 on a real ckb gold set and publish the first CER**
(roadmap M4b) — that, plus inter-annotator agreement (M3b) and a signed release (M7),
are the human/data/credential-gated steps that actually move "Proven accuracy" and
"Distribution".
