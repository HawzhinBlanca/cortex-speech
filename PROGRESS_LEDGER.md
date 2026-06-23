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
* **2026-06-23 (Wave 1+ milestones landed)**: **M1.1** — WER/CER cross-validated against jiwer 4.0.0 on identical Rust-normalized input (12/12 match; empty-ref convention divergence noted), self-referential fixture replaced. **M2.3** — runtime ONNX SHA-256 verification wired into the ASR load path (tampered model rejected; real 300M model passes its pin). **M3.3** — `RefineryPanel` surfaces eval-runs + escalation trend (DOM-tested). **M3.4/M3.5** — ReviewInbox real audio playback + persisted autonomy dial. **M3.6** — axe-core WCAG 2.2 AA gate built, marked `fixme` (surfaced real a11y debt: aria-required-children, color-contrast ×3-5, scrollable-region-focusable, select-name). Gates: cargo test full ✓, clippy ✓, fmt ✓, verify-10 GREEN, typecheck 382/0, vitest 105, python policies ✓, lint ✓. Follow-on: M1.1 SCTK/KLPT/stats-reference; M1.2/M1.3 (ckb A/B + CC0 gold scorecard) need real-data runs.
* **2026-06-23 (M3.6 COMPLETE)**: Fixed all four WCAG 2.2 AA violation classes (select-name, scrollable-region-focusable, aria-required-children, color-contrast — footer cortex-600 3.49:1 → cortex-500 5.28:1, computed) and flipped the axe gate from `.fixme` to **ENFORCED**: `npx playwright test e2e/axe.spec.ts` = 3 passed, **0 violations** across App root (en + ckb/RTL) + settings dialog. typecheck 383/0/0, vitest 105, lint clean.
* **2026-06-23 (M3.1 + M2.5 COMPLETE)**: **M3.1** — end-to-end measured raw-vs-jury label-quality lift: `compute_label_quality_lift` (micro CER raw vs post-jury verdict against human-confirmed `annotated_transcript` references, CER lift + seeded paired bootstrap 95% CI), `load_lift_triples` (human_decision'd segments), `get_label_quality_lift` IPC command, and the RefineryPanel lift card wired (3 Rust tests + DOM test). **M2.5** — root workflow actions already 100% SHA-pinned (0 tag refs); added `.github/dependabot.yml` (actions+cargo+npm). All gates green (cargo test, clippy, fmt, verify-10, typecheck 383/0/0, vitest 105, lint, python policies).
* **2026-06-23 — IN-SANDBOX CEILING REACHED**: Completed every milestone achievable+verifiable in this environment (Wave 0, M1.1, M2.3, M2.5, M3.1, M3.3, M3.4, M3.5, M3.6). Remaining milestones are externally blocked: **M1.2** ckb A/B (run ASR on ≥10 ckb clips), **M1.3/M1.4** published scorecard + IAA (human Sorani annotators + CC0 dataset), **M1.5** SeamlessM4T baseline (download+run), **M2.1/M2.2** network-sandbox + runtime egress (Linux/CI), **M2.4** fuzz CI (nightly Linux), **M2.6** cargo-mutants (long run), **M4.1/M4.2** RTF + bench gate (named reference machine + audio set), **M5.x** signing/updater/SLSA/winget (Azure cert + live CI run-URLs). Honest full-charter grade: **~6/10** (up from ~4.7), capped on the unmeasured headline scorecard + undistributed signed release.
* **2026-06-23 (M1.2 A/B — measured finding)**: Ran the ckb language-hint A/B on a real 10s Kurdish clip with the live OmniASR-CTC 300M model. **Finding: the `language="ckb"` hint is a confirmed NO-OP** — identical output (`"jóg dú mali"`) with vs without it — and the generalist model emitted **Latin-script, non-Kurdish** text. Implication: the base model needs effective language conditioning or fine-tuning (charter M13) before a real ckb scorecard is meaningful; a from-scratch OmniASR-CTC scorecard would currently measure non-Kurdish output. Harness committed (`ckb_language_hint_ab`, env-gated); audio not committed (unknown provenance).
