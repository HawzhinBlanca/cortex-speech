# Cortex Speech — Progress Ledger

## 1. Overall 10/10 Gate Status

* **Stop Condition (`verify-10` checker)**: **GREEN** (Exits 0 locally via `python scripts/verify_10.py`)
* **Scorecard Progress**:

| Dimension | Initial Score | Current Score | Exit Criteria Met? |
|---|:---:|:---:|---|
| **Proven accuracy** | 2 | 2 | No (ASR-on-gold runner pending in M3) |
| **Language breadth** | 1 | 1 | No |
| **Real-time/latency** | 0 | 0 | No |
| **Data-curation refinery** | 10 | 10 | Yes (Refinery logic fully built) |
| **Engineering rigor** | 8 | 8 | No |
| **UX/polish** | 5 | 5 | No |
| **Distribution/adoption** | 0 | 1 | No (Git init complete, remote pending) |
| **Trust/proof** | 2 | 2 | No |
| **Ethics & Data Governance** | 0 | 2 | No (Ledger + Policy written; full audit pending) |
| **Sustainability & Bus-factor** | 1 | 1 | No |

---

## 2. Milestone Status Table (Wave 0)

| ID | Title | Wave | Status | Depends On Met? | Evidence / Done-When Verification |
|---|---|---|---|---|---|
| **M0** | Ethics & Data Governance Foundation | Wave 0 | **DONE** | Yes | `DATA_GOVERNANCE.md` exists; `docs/provenance_ledger.json` passes schema validation. |
| **M1** | Local Repo Init & Manifest Sync | Wave 0 | **DONE** | Yes | `git init` complete. Version bumped to `2.1.0` and license aligned to `Apache-2.0` across package.json/Cargo.toml/tauri.conf.json. LICENSE/NOTICE present. |
| **M2** | Sorani-aware Metrics | Wave 0 | TODO | No | Dep: M1. (Awaiting SoraniNormalizer routing) |
| **M2b** | Wire language='ckb' hint | Wave 0 | TODO | No | Dep: M1. |
| **M3** | ASR-on-gold runner | Wave 0 | TODO | No | Dep: M2, M2b. |
| **M3b** | Inter-annotator agreement | Wave 0 | TODO | No | Dep: M0, M3. |
| **M4a** | Acquire + pin datasets | Wave 0 | TODO | No | Dep: M0, M2. |
| **M4b** | Publish commit-pinned scorecard | Wave 0 | TODO | No | Dep: M0, M3, M3b, M4a. |
| **M5** | Holdout integrity & FLAC | Wave 0 | TODO | No | Dep: M1, M3. |

---

## 3. Current Focus

* **Active Milestone**: M2 — Sorani-aware Metrics
* **Branch**: `m02-sorani-metrics`
* **Next Done-When Bullet**: Route `wer::normalize_for_metrics` through `SoraniNormalizer` and Unicode NFC normalization.

---

## 4. Measured Numbers Table

| Date | Metric | Value | Model/Dataset SHA | Command / Source of Truth |
|---|---|---|---|---|
| 2026-06-17 | E2E Soak Test Latency | 112.72s | Local Synthetic / 300M | `cargo test --test soak` |

---

## 5. Blockers Awaiting Human

* None currently. Local development is unblocked.

---

## 6. Backlog

* macOS notarization (stretch target in Wave 1/M7).

---

## 7. Decision Log

* **2026-06-17**: Initialized progress ledger. Set canonical version to `2.1.0` and licensed under `Apache-2.0` to resolve codebase branding inconsistencies.
