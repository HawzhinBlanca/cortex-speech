# CORTEX SPEECH — TRUE RATING & COMPETITIVE AUDIT (2026-07-09)

> The owner asked: *"deep audit my code in very detail, truly rate the app against the top 3 in
> every way, honest report, and how to make it truly top-3 in history."* This document is that
> answer. Every number here traces to a command run today or a cited source; where something is
> unmeasured it says so. Method: all local gates re-executed at HEAD today + a 36-agent audit
> (8 read-only dimension auditors on the live tree, each finding adversarially verified by
> independent refuters; REFUTED findings dropped) + 4 web-research tracks with cited URLs
> (3.45M subagent tokens, 680 tool calls). Tree audited: `origin/main 1e52a41` + PR #36.

---

## 1 · What was verified green TODAY (not claimed — run)

| Gate | Result today (2026-07-09) |
|---|---|
| `python scripts/verify_10.py` (charter/license/provenance) | **CORTEX 10/10: ALL GATES GREEN** |
| Python policy suites (`run_python_policies.py`, 24+ suites) | all passed, exit 0 |
| vitest | **132/132** (28 files) |
| `svelte-check` + `tsc` | 0 errors / 0 warnings, 393 files |
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean, exit 0 |
| `cargo test` (FULL — lib + all integration bins) | **822 lib + integration all pass**, exit 0 |
| Release exe | **rebuilt today** (8m27s, frontend rebuilt first); freshness gate **GREEN at HEAD** |
| GitHub CI | main was RED (2 causes, below) → **both fixed in PR #36** |

**Defects found and fixed today by this audit** (all live in PR #36 / the rebuilt exe):

1. **Nightly Real Audio red (07-09)** — the soak job compiled `tauri-build` without the
   gitignored base models (`models/silero_vad_v4.onnx` is a validated `bundle.resources` entry).
   Added the `npm run fetch-models` provisioning step ci.yml already had.
2. **Release Gate red on main (07-08)** — `test_concurrent_db_access_no_deadlock` false-fired
   "deadlocked within 5s" on a slow shared runner (0.18s locally; single-mutex test cannot
   classically deadlock). All three deadlock-detector deadlines widened to 60s — a real deadlock
   never resolves, so detection strength is unchanged and the flake is gone.
3. **The daily exe was stale** — built 07-04 (`b3111ed`), missing all ~110 July 5–8 fixes
   **including the A1 aligner data-destruction fix**. Rebuilt today; freshness GREEN.
4. Local-only: the integration tests were briefly broken at the stale checkout (E0061 on the new
   3-arg `health_check`); origin had already fixed it identically — confirmed, reconciled by
   fast-forwarding the local checkout ~110 commits.

---

## 2 · The dimension scorecard (36-agent audit, adversarially verified)

Grading bar: 10 = nothing left that matters for a single power user; 7 = solid daily-driver with
real gaps; 4 = works but painful/risky.

| Dimension | Grade | One-line verdict |
|---|---|---|
| Security · privacy · consent | **8.5** | Defense-in-depth consent gating verified at both settings and pipeline layers; better than any comparable app surveyed. 1 regression (T2 Gemini response bypasses the JSON OOM bound). |
| Claims-vs-evidence (honesty) | **8.0** | 12/12 sampled "fixed" claims verify in the tree; retired numbers struck through, not deleted. 2 majors: EVAL.md still asserts a retracted 7B conclusion; an N=1 number computed pre-CER-fix is unflagged. |
| Reliability · disaster recovery | **7.5** | Quarantine/snapshot/restore chain is real and tested; 4 confirmed majors (no pre-migration snapshot; ~100-min recovery horizon; restore not serialized against writers; quarantine lifecycle dead-ends). |
| Dataset integrity · export | **7.0** | All 8 claimed guards live (rubric-gated pack, fail-closed holdout, speaker-disjoint splits). 2 majors: offset-less alignment rows ship the WHOLE recording as a clip; gold promotion accepts partially-reviewed files. |
| ASR engine layer | **6.5** | Fail-hard 7B path + serialization + provenance chokepoints all hold. 3 majors: `use_finetuned_asr`+WSL7B mis-attributes MMS text as the 7B's; gold-eval runs persist under a caller-typed model_id; the fine-tuned juror is ability-weighted BELOW stock CTC-1B. |
| Review UX | **6.5** | The core loop is genuinely fast (one-key accept, bounded autoplay, tap-a-word). **1 BLOCKER**: global shortcuts still fire on the HIDDEN curate segment during review (Ctrl+Enter double-fires; Ctrl+T machine-overwrites). 2 majors: inbox edit-text leaks onto a different segment; single-letter shortcuts dead under a Kurdish keyboard layout. |
| Gates · tests · CI | **6.5** | Dense, honest local gate system (10-gate ship-check, 24+ policy suites). 2 majors: release.yml still has the models-before-cargo ordering bug the CI-repair series fixed elsewhere; the gate-on-the-gate never asserts provisioning ORDER (the exact class that broke Nightly). |
| Intelligence layer | **4.5** | Machinery is careful and honest, but measured against "does it reduce human work today": suspect-first collapses to recency (T1/T2 escalations NULL the persisted confidence — a regression), auto-accept is mathematically unreachable at shipped constants at this data volume, and the C4 precision metric is survivor-biased. |
| **Unweighted mean** | **6.9** | |

### The honest topline: **≈ 7 / 10** — the strongest verified state this project has ever been in,
and still not a 10, for reasons that are now precisely enumerable (below).

Grade lineage (same bar throughout): 6.5 (2026-07-02 audit) → 6.5 (2026-07-06 deep-check: strong
substrate, 12 blockers incl. live data destruction) → **~7.0 today** (all 61 automatable 07-06
findings fixed + gated; CI and exe-freshness restored today; held back by 1 new UX blocker,
~14 confirmed majors, an intelligence layer that doesn't yet pay rent, and the unmeasured default
engine).

### Broken regression claims found (self-reporting corrections)
1. Suspect-first "ranks by real jury confidence" — **regressed**: T1/T2 escalation paths NULL the
   persisted IRT confidence (back to recency ordering).
2. Correction-memory confidence "no longer frozen" — the Beta-posterior exists but the shipped
   default still behaves as frozen on this DB (0 decisions).
3. Provider JSON OOM bound — **T2 Gemini response path bypasses it**.
4. "Search/filter-scoped review queue" + "platform-aware key labels" — partially regressed
   (filter inheritance contradicts the in-code contract; Mac glyphs remain in 2 spots).
5. Ledger's last freshness claim ("GREEN, all 26 fixes live") was true 07-04 but silently went
   stale — the gate was RED at today's HEAD until the rebuild. (The ledger's headline block also
   still says "~4.7/10", contradicting its own newer entries.)

### New confirmed backlog (fix in this order)
- **B (blocker):** review-mode keyboard isolation — global shortcuts reach the hidden curate
  segment (one keystroke = actions on an invisible clip). Fix before the next review session.
- **M1:** inbox `isEditing/editText` survives rail navigation → edit lands on the wrong segment.
- **M2:** `e.key`-matched shortcuts dead under Arabic-script keyboard layouts (use `e.code`).
- **M3:** engine truth: single authoritative dispatch (kills the `use_finetuned_asr`+WSL7B
  mis-attribution) + eval runs record the engine that actually ran.
- **M4:** `get_initial_ability` branch for `finetuned-mms-ckb` (measured-best voter must outweigh
  stock kin CTC pair) + stop NULLing confidence on T1/T2 escalation.
- **M5:** offset-less alignment rows refused by pack/export slicers (whole-recording clip bug);
  gold promotion refuses partially-reviewed files.
- **M6:** T2 response through `json_bounded`; release.yml step ordering; workflow-policy asserts
  provisioning order; pre-migration snapshot; restore serialization vs writers.
- **Doc honesty:** EVAL.md — retract the stale "on-par-to-slightly-better" 7B claim from the main
  body; flag the N=1 29.33% as pre-CER-fix; fix the ledger headline block.

---

## 3 · Competitive rating — honestly, against the actual top 3

The research track's core conclusion: **no product on the mid-2026 market covers Cortex's three
axes** — (a) offline fine-tuned low-resource ASR, (b) fast human review with word timestamps and
confidence triage, (c) training-grade dataset export with provenance. The market splits into
transcription GUIs (no dataset export) and annotation platforms (no built-in low-resource ASR).
The three strongest overall comparators, rated per axis (10 = best conceivable):

| Axis | **Cortex** | Label Studio (+ML backend) | Prodigy | NVIDIA NeMo SDP/SDE |
|---|---|---|---|---|
| Offline Sorani ASR (fine-tuned, integrated) | **9** — LoRA 7B champion + MMS-1B + CTC fallbacks, all local | 4 — bring-your-own model via ML-backend plumbing | 4 — whisper plugin (no ckb) | 3 — external inference |
| Review UX for speech (word-level, keyboard, triage) | **7** — word-tap playback, jury triage, autoplay; minus the blocker above | 7 — mature templates, hotkeys, agreement metrics (Enterprise) | 8 — the gold standard of keyboard-first annotation ergonomics | 4 — analysis-oriented (SDE), not correction-first |
| Training-grade dataset export (grades, splits, provenance) | **9** — rubric grades, speaker-disjoint splits, holdout fail-closed, corpus ledger, SHA-pinned packs | 6 — many formats incl. NeMo ASR_MANIFEST; no quality rubric/leakage guards | 5 — JSONL out; roll your own splits | **9** — SDP is the industrial benchmark for manifest processing/filtering |
| Learning loop (decisions → retrain → promote, gated) | **8 on plumbing / 2 on evidence** — full apparatus exists, zero cycles run | 3 — active learning via ML backend | 6 — active learning is Prodigy's signature | 5 — pipelines, no human loop |
| Privacy (biometric voice, offline-first) | **9.5** — offline by default, consent fail-closed at two layers, tested | 8 (self-hosted) | 9 — fully local by design | 8 (self-hosted) |
| Reliability/ops for a solo user | 7.5 | 7 | 8 | 6 |
| Maturity/polish/community | 4 — one user, weeks old | 9 — huge community, years of hardening | 8 | 7 |
| **Proven accuracy on ITS language** | **3 — the honest weak spot: default engine unmeasured** | n/a (BYO model) | n/a | n/a |

**And the ASR rival (different category — cloud STT):** ElevenLabs Scribe. Verified today:
Scribe v1 is still the number on ElevenLabs' own Central Kurdish page — **32.1% WER on
FLEURS-ckb** (Gemini Flash 2: 43.5%; Whisper large-v3: 99.1% — no ckb support). No published
Scribe v2 ckb number exists. Scribe is cloud-only (on-prem is a waitlisted Early Access),
**retains customer audio by default** unless Zero-Retention is enabled, at $0.22/hr. Against
Cortex's core promise — *offline, consent-gated, learns your voice domain* — Scribe is not even
playing the same game; the only axis it wins today is that **its Sorani number is published and
Cortex's isn't.**

### Where Cortex truly stands today
- **In its own category (offline low-resource speech curation studio): it is effectively the
  only entrant** — the combination does not exist elsewhere. That is genuinely rare and worth
  stating plainly.
- **Against the best of each axis:** it already out-features Label Studio on integrated ASR and
  export integrity, loses to Prodigy on annotation ergonomics polish, matches NeMo SDP on
  manifest rigor while adding what SDP lacks (a human loop), and beats Scribe on privacy/offline
  while lacking Scribe's one published Sorani number.
- **The single deepest weakness is not code.** It is that the flagship claim ("the best Sorani
  ASR + dataset tool") is **unmeasured**: the 7B champion has no trustworthy CER (EVAL.md:267
  says so, honestly), the review marathon has never started (live DB today: **87 segments,
  18 min audio, 0 human decisions, 0 gold rows, 0 eval runs**), and zero retrain cycles have run.
  The instrument is built; the concert hasn't begun.

---

## 4 · The published landscape Cortex must beat (cited, verified today)

| Benchmark (READ speech) | Best published | Source |
|---|---|---|
| FLEURS ckb | Scribe v1 **32.1% WER**; base omniASR-7B-LLM **6.0% CER** (ckb_Arab, Meta's official per-language CSV) | elevenlabs.io/speech-to-text/central-kurdish; github.com/facebookresearch/omnilingual-asr |
| AsoSoft test | **11.8% WER** — XLS-R-2B + 3-gram KenLM, 100h corpus (arXiv 2406.02561, 2024) | arxiv.org/abs/2406.02561 |
| Common Voice ckb | **7.8% CER / 36.8% WER** (XLS-R-300M, 2022); self-reported 6.13% WER (whisper-small-ckb, custom split) | HF model cards |
| CV22 ckb data | 135.98 validated hours, 1,938 speakers | github.com/common-voice/cv-dataset |
| **CONVERSATIONAL Sorani** | **No established benchmark.** CORDI corpus exists (311 episodes, 186k utterances, LREC-COLING 2024); read→conversational degradation is catastrophic in controlled studies (1.7%→30.3% WER) | aclanthology.org/2024.lrec-main.877 |

Two implications. First — **the base model Cortex's champion is LoRA-tuned from is documented at
6.0% CER on read ckb**, so the champion plausibly sits at or beyond published Sorani SOTA
*already*; only a measurement stands between Cortex and a defensible "best available Sorani ASR"
row. Second — **conversational Sorani has no benchmark at all**: the first credible one would be
a historic artifact for the language, and Cortex's verified-clip pipeline is precisely the
machine that builds it.

---

## 5 · How to make it truly top-3-in-history (the measured path)

"Top 3 in history" has an honest reading: **for Sorani — a language of ~8M speakers with no
usable Whisper, no offline tool, and one cloud rival at 32% WER — become (1) the measured-best
ASR, (2) the first conversational benchmark, and (3) the first dataset flywheel that provably
learns from its one user.** All three are achievable with what is already on this machine. In
order, with expected gains cited, not vibed:

**Step 0 (hours): protect the labels + finish CI.** Merge PR #36; fix the review-keyboard
blocker + the two review-integrity majors (M1, M2) before the next review sitting; land the
EVAL.md honesty corrections. *(Everything downstream depends on trustworthy labels.)*

**Step 1 (one GPU afternoon, zero owner review-hours): the P2.2 three-engine benchmark = C1.**
7B champion vs fine-tuned MMS-1B vs stock 300M on frozen FLEURS-ckb-test + CV22-ckb-test,
identical normalization, paired bootstrap CIs (the runbook + harness already exist:
`make measure-10`). Optionally score Scribe (consent-gated, ~$3 of API) on the same sets for the
direct-rival row. Deliverable: the first defensible **"Cortex vs the world"** table. Given the
base model's 6.0% CER, the champion likely *beats the only commercial rival outright* on read
speech — but the number must be run, not assumed.

**Step 2 (days of code): make the intelligence pay rent.** M3/M4 fixes (ability weight for the
fine-tuned juror, stop NULLing escalation confidence, recalibrate the conformal constants at
real volume). This is what converts "jury escalates ~everything" into genuine auto-accept — the
single biggest review-throughput lever in the codebase.

**Step 3 (the only large owner cost): the Gold Marathon (M3 in FINAL_READINESS_10).** ≥500
human decisions with instrumentation ON (it already is): every decision simultaneously mints
review-speed data (C3), jury ground truth (C4), LOOP-0 shadow validation (C5), app-gold (the
conversational number), and training pairs (C7). Freeze app-gold at N≥300 → **the first
conversational Sorani CER/WER ever published with provenance.**

**Step 4 (one weekend of GPU): the moat.** Execute ONE full retrain→gate→promote cycle
(pre-registered comparison, either outcome closes C7). Then the two cheapest cited accuracy
levers: **KenLM shallow fusion** on the MMS-CTC head (~36% relative WER reduction across 18
Indic languages; Luganda 42.9→20.7 — arXiv 2311.15077, 2512.10968) and **pseudo-labeling** the
owner's unlabeled archive with confidence filtering (Scottish-Gaelic solo-practitioner recipe:
35.2→23.1 WER — arXiv 2506.04915). Both are exactly matched to Cortex's assets (a CTC head, a
300k-clip unlabeled archive, a 4090).

**Step 5: P7 re-audit.** Re-score C1–C9 on pasted evidence; declare the grade the evidence
supports — and nothing more. That is the only place "10/10" or "top 3" may ever be written.

---

## 6 · Provenance appendix

- Gates: run 2026-07-09 on `1e52a41` + PR #36 (this machine, Windows 11, rustc 1.96.1).
  Commands as listed in §1; outputs in the session transcript.
- Audit: workflow `wf_39ac5a5d-2f6` — 36 agents (8 auditors + verifiers + 4 researchers),
  3,449,887 subagent tokens, 680 tool calls, 0 agent errors. Findings above are only those that
  survived adversarial verification (CONFIRMED/PLAUSIBLE); REFUTED were dropped.
- Live-DB numbers: read-only SQLite query of `%APPDATA%/cortex-speech/cortex-speech.db` today.
- Accuracy numbers: docs/EVAL.md (MMS 21.00% CER [19.93–22.04] N=900; stock 29.40% [26.29–32.54]
  N=400; 7B: no trustworthy set — stated there and here).
- Competitive claims: URLs inline in §3–§4, fetched today by the research agents.
- This report makes **no** accuracy claim for the 7B champion. That number does not exist yet.
