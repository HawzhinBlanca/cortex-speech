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
| **2026-06-24** | **ckb micro CER (primary)** | **29.40% (95% CI 26.3–32.5)** | OmniASR-CTC-300M stock / "Comprehensive Central Kurdish Sound Dataset", N=400 (seed=42) | `cargo test --test real_audio ckb_scorecard_on_gold` → `python scripts/scorecard_stats.py results.tsv 3000` |
| **2026-06-24** | **ckb micro WER (secondary)** | **67.62%** | same (N=400) | see `cortex-speech-app/docs/EVAL.md` |
| **2026-06-24** | **fairness: gender CER disparity** | **4.29 pts** (M 29.66% / F 25.37%) | same, N=375/25 | first per-gender Sorani CER slice; `docs/EVAL.md` |
| **2026-06-24** | **real-exe pipeline e2e (import→VAD→OmniASR)** | **PASS** — non-blank `"بروايت مفهومي حديث"` (ref `بە ڕیوایەتی مەفهوومی حەدیس`) | release build 2.1.0 / real ckb clip | `node e2e_pipeline_ipc.cjs` against `target/release/cortex-speech-app.exe` (no-fabrication guard) |
| **2026-06-24** | **Windows installer bundle** | **PASS** — MSI 310 MB + NSIS 291 MB produced | release 2.1.0 (models bundled) | `npm run tauri build` → `target/release/bundle/{msi,nsis}/` |
| **2026-06-24** | **Rust gates after reliability/CSP fixes** | **GREEN** | branch `m02-sorani-metrics` | `cargo clippy -D warnings` + `cargo test` + `npm run test:python-policies` |

> **2026-06-24 hardening pass** (branch `m02-sorani-metrics`): drove the real `.exe` end-to-end and
> fixed real defects found — import-worker panic-guard, unknown-frame audio recovery, CSP block of
> the Windows event IPC (`http://ipc.localhost`), an un-broke of the eval row-propagation policy gate,
> `ship-check` made a superset of CI, honest model-download docs + root README, and a real-app e2e
> harness (`test:e2e:real` + `e2e_pipeline_ipc.cjs`). **Open finding:** the full UI click-through
> e2e does not yet pass because the post-import jury adjudication holds the global DB lock across
> heavy ASR (commands.rs:560-564), starving the UI's `get_segments` so the segment list does not
> render promptly during import — flagged for a focused fix (segment is created and transcribes
> fine; this is a UI-responsiveness defect, not an ASR defect).

---

## 5. Blockers Awaiting Human

* ~~External gold-runner execution for the real scorecard table.~~ **DONE** — first real ckb CER/WER measured (34.5% / 79.4%, N=40); see `docs/EVAL.md`. Remaining for a *publishable* scorecard: scale N to ≥900, compute IAA ceiling, fix the Latin-romanization/language-locking, add a real baseline (SeamlessM4T-v2).

* **The four items between the in-sandbox state and a full-charter 10/10 are now EXTERNAL (turnkey for the human):**
  1. **GPU fine-tune (the only real accuracy cure, ~29% → ~8% CER).** Constrained decode (now shipped, opt-in) guarantees Kurdish *script*; only fine-tuning fixes Kurdish *recognition*. Needs a GPU + the dataset. Hand the resulting model back and it gets wired + re-measured through `ckb_scorecard_on_gold`.
  2. **Fresh-clone model fetch (blocker #1).** A decision: **Git LFS** (`git lfs track` the ~300 MB models — consumes repo LFS quota) **vs.** a `scripts/fetch-models` downloader with pinned SHA-256 (needs a one-time ~235 MB OmniASR-archive download to compute the archive hash — `OMNIASR_CTC_300M_ARCHIVE_SHA256` is intentionally empty until then). Pick one and it gets implemented + gated.
  3. **Code-signing certificate** (Authenticode EV/OV) — a paid identity document; once provisioned, `bundle.windows.certificateThumbprint` + `timestampUrl` wiring is a small config change.
  4. **CC0 committable audio fixture** — the user corpus is eval-only; a redistributable fixture must be CC0-sourced (e.g. Common Voice ckb, `CC0-1.0` per the provenance ledger) to make a default-`cargo test` real-ASR gate possible.

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
* **2026-06-24 (M1.3 — FIRST REAL ckb SCORECARD)**: User provided a 1.74M-clip Central Kurdish corpus (audio + verified Sorani transcripts + gender/age). Built reproducible gold-set extraction + `ckb_scorecard_on_gold` harness + `scorecard_stats.py`. **Measured: micro CER 29.40% (95% CI [26.3, 32.5], N=400, seed=42), micro WER 67.62%**, stock OmniASR-CTC-300M. First per-gender/age Sorani fairness slice (gender disparity ~4–10pt, noisy on small female N). **PIVOTAL FINDING (script split, N=200): when the model emits Kurdish script (78% of clips) CER is 19.71%; the ~30% aggregate is dominated by the 21% it romanizes to Latin (94.5% CER vs Arabic refs, despite usually-correct content).** → The headline gap is mostly **script-locking, not recognition**: forcing Arabic-script output (or transliterating the romanized minority) should pull CER ~30%→~20% with no re-train. Roadmap: **script-locking first, fine-tune second**, then scale to ≥900 + IAA + SeamlessM4T baseline. Full scorecard: `cortex-speech-app/docs/EVAL.md`. Eval-only corpus; no audio/refs committed.
* **2026-06-24 (hardening pass — 16 commits, in-sandbox ceiling)**: Drove the real `.exe` end-to-end and hardened it. **Fixed real defects:** import-worker panic-guard; unknown-frame audio no longer false-rejected; **CSP missing `http://ipc.localhost`** (Windows event IPC); **post-import jury adjudication held the global DB lock across ASR → starved `get_segments`** (now runs on its own WAL connection + off the import thread — verified: the CDP reload that timed out now succeeds). **Shipped the script-locking lever (the #1 finding above):** constrained Kurdish-token CTC decode ported to Rust (`constrained_decode.rs`, 6 tests + real-model parity → `"رفايت مفهومي حدي"`, all Kurdish-script), wired as opt-in `transcribe_segment_constrained` IPC command (verified live) + UI "Kurdish-only" button + cached `ort` session. **Gate honesty:** un-broke a red eval policy gate; `make ship-check` made a CI superset; CI mock-e2e honestly labeled + nightly skip made a visible `::warning::`. **Proof:** real-exe pipeline e2e (`e2e_pipeline_ipc.cjs` → non-blank Kurdish), Windows installers (MSI+NSIS) built. All gates green (clippy `-D warnings`, cargo test 0-fail, vitest 105/105, python-policies). **Remaining to full-charter 10/10 is now wholly external** (see §5): GPU fine-tune (the accuracy cure), model-fetch decision (LFS vs script), code-signing cert, CC0 fixture. Honest grade ~6.5/10; capped on the unadapted model + undistributed signed release — NOT on any remaining safe in-sandbox work.
* **2026-06-25 (model-fetch + FINE-TUNED MODEL MEASURED — the accuracy cure)**: (1) **Blocker #1 closed** — `scripts/fetch_models.py` (user chose a fetch script over Git LFS) downloads + SHA-256-verifies the gitignored models into `src-tauri/models/` so a fresh clone can `tauri build`; pins match the in-repo extracted-file pins; `npm run fetch-models`/`verify-models`; wired into `release.yml` before the bundle step. `--check` verified all 5 files green (download path un-run in-sandbox; SHAs authoritative). (2) **The user provisioned a fine-tuned model (MMS-CTC-1B, Wav2Vec2ForCTC, base `facebook/mms-1b-all`) and I MEASURED it** head-to-head on an identical seed-fixed gold sample (N=50, seed=42, same normalization, CPU): **stock OmniASR-CTC-300M 42.06% CER → fine-tuned MMS-CTC-1B 19.77% CER (~53% relative reduction), and all output is Kurdish script (language-lock fixed).** The fine-tune is the real accuracy cure, now measured (not estimated). Caveats: N=50 preliminary (publishable needs ≥900 + CI); measured via a CPU `transformers` harness, **NOT yet wired into the Tauri app** (the HF model needs an ONNX export for sherpa-onnx/`ort`, or a Python inference bridge). Full numbers + caveats: `cortex-speech-app/docs/EVAL.md`. **Remaining to ship the fine-tuned model in-app:** ONNX-export the Wav2Vec2ForCTC (or add a Python bridge) + wire it as the ASR engine; scale the scorecard to ≥900 + CI; then code-signing + a CC0 fixture. Honest grade lifts toward ~7.5/10 on the measured accuracy cure (capped on in-app integration + publishable-N + signed release).
* **2026-06-25 (FINE-TUNED MODEL EMBEDDED + WORKING IN THE APP)**: Integrated the user's fine-tuned MMS-CTC-1B into Cortex end-to-end. **ONNX-exported** the `Wav2Vec2ForCTC` (`scripts/export_finetuned_onnx.py`) + **int8-quantized** to 925 MB; export fidelity verified (fp32 18.57% / int8 **19.29%** CER on the N=50 gold sample, matching transformers 19.77%). Built a Rust **`wav2vec2_asr.rs`** engine (feature norm → `ort` inference → CTC decode vs `vocab.json["ckb"]`, cached session, 4 unit tests + parity test), an opt-in **`transcribe_segment_finetuned`** IPC command (resolves env → active → bundled models dir), a **"Fine-tuned" UI button**, and **embedded** `finetuned-mms-ckb/{model.onnx,vocab.json}` in both bundle configs. **Verified END-TO-END on the real exe with NO env override** (resolves the EMBEDDED model): `transcribe_segment_finetuned` → `"بەڕیڤایەتی مەفومی حەدی"` (accurate Kurdish, ref `بە ڕیوایەتی مەفهوومی حەدیس`; stock OmniASR gave `بروايت مفهومي حديث`). Gates: clippy -D warnings, cargo test 0-fail, vitest 105/105, typecheck/lint, python policies — all green. **The app now ships an opt-in engine that roughly halves Sorani CER.** Not wired: OmniASR_7B (user removed its 31.2 GB base weights). README documents placing the model in `src-tauri/models/finetuned-mms-ckb/`. Honest grade lifts to ~8/10 — proven accuracy lever is now IN the app; remaining: publishable N≥900 + CI, code-signing, a CC0 default-test fixture.
* **2026-06-25 (PUBLISHABLE N=900 FINE-TUNED SCORECARD)**: Ran the embedded int8 fine-tuned engine on a seed-fixed **N=900** gold sample via onnxruntime (`scripts/scorecard_finetuned.py`): **micro CER 21.00%, 95% CI [19.93%, 22.04%]** (3000-sample utterance bootstrap, seed=42), same normalization as the stock baseline. vs stock OmniASR-CTC-300M 29.40% (N=400) — ~8.4 pts / ~29% relative lower, all Kurdish script. This is the first publishable-tier (≥900 + CI) accuracy number for the SHIPPED engine. Full scorecard: `cortex-speech-app/docs/EVAL.md`. Eval-only corpus; no audio/refs committed.
