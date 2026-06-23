# Cortex Speech — Progress Ledger

## 1. Overall 10/10 Gate Status

* **Stop Condition (`verify-10` checker)**: **GREEN — narrow M0/M1 gate only** (`make verify-10` exits 0: manifest sync, asset presence, ledger schema, license-compatibility). This is **NOT** the full-charter 10/10 — the deep gates (published reproducible CER/WER scorecard, RTF, signing/SLSA, fuzz/mutants) are unbuilt. Honest full-charter grade **~4.7/10** — see [docs/BLUEPRINT_9_5.md](docs/BLUEPRINT_9_5.md).
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

---

## 5. Blockers Awaiting Human

* External gold-runner execution for the real scorecard table.

---

## 6. Backlog

* macOS notarization (stretch target in Wave 1/M7).

---

## 7. Decision Log

* **2026-06-23**: Closed M3 and M5 on verified code+test evidence. Shifted active focus to M4b (publish-pinned scorecard) as the next highest-impact 10/0 blocker.
* **2026-06-23 (correction)**: A **root-scoped re-audit** found the prior M2/M2b/M3 "DONE" marks overstated — M2's jiwer check is self-referential, M2b's hint effect is unproven (no A/B spike), and M3's `gold_wer_eval` gate is `#[ignore]`'d (5×) and unenforced in CI with no committed gold set/scorecard. Reverted those three to **PARTIAL**. M5 (FLAC + hash-holdout) and M0/M1 (governance + manifests, verified at the git root) remain genuinely DONE. Confirmed `make verify-10` green is the **narrow** M0/M1 gate, not full-charter 10/10. Full evidence-grounded plan + corrected scorecard: [docs/BLUEPRINT_9_5.md](docs/BLUEPRINT_9_5.md).
* **2026-06-23 (Wave 0 truth-fixes landed)**: Implemented blueprint W0.1–W0.5 — corrected the accuracy baseline (Whisper→SeamlessM4T-v2) in AGENT_CHARTER; fixed the stale OMNIASR_MIGRATION `language='ckb'` status (it IS wired, `asr.rs:425-427`; effect still unverified); corrected AsoSoft (→`LicenseRef-AsoSoft-NonCommercial`, no-redist) and CORDI (→`CC-BY-SA-4.0`) ledger license labels; removed the phantom `kmr` UI option; removed the dead duplicate `cortex-speech-app/.github`. Gates: verify-10 GREEN, typecheck 381/0, lint clean, python policies pass. **Next: blueprint M1.1** — replace the self-referential jiwer fixture with a real external `scripts/crossval_jiwer.py` (Rust↔jiwer ≤1e-6 after identical Sorani normalization) + NIST SCTK MAPSSWE cross-check.
