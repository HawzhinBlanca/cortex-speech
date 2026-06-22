# Cortex Speech Studio — The Definitive Architecture & Research Design

**Document type:** Lead-architect design doc (end-to-end)
**Subject:** A professional, top-3-class Sorani (Central Kurdish, `ckb`) ASR dataset-curation studio whose model learns from user corrections and ingests an externally fine-tuned OmniASR-LLM-7B.
**Date:** 2026-06-20 · **Author model cutoff:** Jan 2026 (see §9 — this is the single most important caveat for every version/date/benchmark below)
**Status of ground truth:** Every *code* claim below was re-derived by reading the working tree on branch `m02-sorani-metrics`. Code anchors are given as **stable symbol names plus the line they sit on in the tree as read today**; line numbers drift, so trust the symbol name first. Every *external* fact (tool versions, third-party issue states, published benchmarks, arXiv results) is explicitly tagged for verification status — see §9 and the inline `[unverified at cutoff]` markers.

> **Verification note up front.** I read the actual modules before writing, and I corrected the incoming system map where it was wrong:
> 1. **Schema head is migration `v19`, not v18** (`migrations/mod.rs`, the `Migration { version: 19, ... }` for the verified-filtered/`created_at`-ordered composite index). The only higher integer in the file is a **test-only** `version: 99_999` broken-migration fixture inside `#[test] failed_migration_is_all_or_nothing`. **The next free version is therefore `v20`,** and the entire proposed migration cascade below is numbered from v20 up.
> 2. The on-device CTC engine is the sherpa-onnx OmniASR int8 bundle whose archive URL is dated **2025-11-12** (verified in `models.rs`: `OMNIASR_CTC_300M_ARCHIVE_URL` / `OMNIASR_CTC_1B_ARCHIVE_URL`), not a "v2".
> 3. The string `omniASR_LLM_7B_v2` is what the **local** ensemble script actually passes — `ASRInferencePipeline(model_card="omniASR_LLM_7B_v2")` in `scripts/sorani_ensemble_asr.py` (the line reading `pipe7b = ASRInferencePipeline(model_card="omniASR_LLM_7B_v2")`). There is **no Meta release I can verify under either `omniASR_LLM_7B_v2` *or* `omniASR_LLM_7B`** — the canonical published base-card name is itself unconfirmed at my cutoff and **must be resolved at integration time** (see §3.2). The doc treats `…_v2` as an unresolved local override throughout, never as a usable card.

> **Implementation status (shipped on `worktree-cortex-10x`).** A large slice of this design is built and tested (502 lib tests green, clippy 0, charter gate green). The *shipped* migration numbering differs slightly from the v22/v23 sketch in §3.3/§7.3 — exactly the drift this doc warned about (trust the table/symbol names, not the integers).
>
> **Schema (P0/P1):** **v20** `correction_memory` (§2.3) · **v21** `corrections` ledger (§2.7) · **v22** `model_version_id` stamp on hypotheses + verdicts · **v23** model registry — `model_versions` **and** `adapters` in one FK-coupled migration (§3.3). Proven on disk by an `open→initialize→restart→integrity_check` smoke test.
>
> **Correction→learning flywheel — capture + LOOP-0 firing are LIVE:**
> - Every human edit atomically writes the `corrections` audit ledger (durable blake3 content hash + agent verdict + `model_version_id`) **and** the LOOP-0 `correction_memory` (hit-count upsert on independent re-confirmation; gold segments excluded to prevent eval leakage).
> - `corrections.rs`: `extract_substitution_memories` (self-contained word aligner over *normalized* words, so orthographic variants are never spurious corrections) + `apply_memories`, the gated firing rule (exact normalized slot match ∧ phonetic similarity ∧ confidence ∧ hit_count). A real over-trigger bug was caught and fixed: the phonetic key stores the normalized wrong WORD (the matcher g2p's internally), not pre-g2p'd phonemes.
> - Firing is wired into all interactive transcribe paths (WSL / cached / non-WSL) behind the opt-in `loop0_firing_enabled` (default OFF — it rewrites ASR output). `raw_transcript` provenance is preserved; firing affects only the returned/displayed text.
>
> - `firing_error_delta` (`corrections.rs`) measures, on a gold set, whether firing lowers (helps) or raises (over-triggers) the word-error count — the safety signal that gates enabling firing / tuning `phon_tau`/`tau_conf`/`min_hits`.
>
> **LOOP-1 (generative error correction) — first pieces shipped:** `get_few_shot_examples` now **relevance-ranks** `agent_examples` by lexical (Jaccard, normalized) overlap to the segment instead of recency (§2.4). `llm_refiner.rs::build_ger_user_prompt` + `refine_with_context` build a HyPoradise-style prompt over the deduped N-best + relevant few-shot, conservatively instructed (prefer candidate agreement, don't invent). Wired into the WSL refine path behind the opt-in `ger_refinement_enabled` (default OFF).
>
> **Registry + gate + ingestion (P1/P2):** `registry.rs` — the import gate (non-empty checkpoint SHA required for finetuned sources; never import-straight-to-champion), atomic champion promotion, `record_eval_result` / `champion_gold_cer`, `decide_promotion` + `gate_and_promote` (require WER `beats_baseline` **AND** CER non-regression, reconciling §1.2), `import_checkpoint` (server-side SHA), and `build_fairseq2_asset_card` (resolved-base + WSL-path preconditions). Exposed via Tauri commands `list_model_versions` / `get_champion_model` / `import_model_checkpoint` (promotion deliberately not exposed — it must run the gate). DB-enforced invariants: one champion per family (partial unique index), CHECK `source`/`status` vocab, adapter cascade.
>
> **LOOP-2 (n-gram LM) — data-prep shipped:** `jury/learning.rs::export_lm_corpus` exports the human-confirmed-correct, char-normalized, holdout-excluded Sorani text — the exact corpus to run `kenlm lmplz` on externally (the gold holdout-hash loading is now a shared `holdout_content_hashes` helper used by both the DPO and LM exports). Shallow-fusing the resulting LM into the N-best is the remaining step.
>
> **Not yet built (follow-up):** running `lmplz` on the exported corpus + shallow-fusing the LM (external tool); batch-import-path firing (needs a where-does-fired-text-live decision so it doesn't clobber `raw_transcript`); GER on the non-WSL path (hypotheses are populated after refine there); fairseq2 base-card resolution + `~/.config` write + LoRA-merge (need WSL); FE panels for the registry / firing & GER toggles (FE lane); threshold calibration + over-trigger gating (need the gold set, then run `firing_error_delta`); LOOP 3 (periodic LoRA), which trains externally.

---

## Table of contents

1. [Vision & north star](#1-vision--north-star)
2. [The Correction→Learning Flywheel (the centerpiece)](#2-the-correctionlearning-flywheel)
3. [Fine-tuning integration & model registry](#3-fine-tuning-integration--model-registry)
4. [Agentic + ensemble + jury system](#4-agentic--ensemble--jury-system)
5. [The tool/tech stack (versions hedged)](#5-the-tooltech-stack)
6. [Kurdish/Sorani resource map](#6-kurdishsorani-resource-map)
7. [E2E module reorganization + phased roadmap](#7-e2e-module-reorganization--phased-roadmap)
8. [Robustness & quality (pro-app patterns)](#8-robustness--quality-pro-app-patterns)
9. [Honest constraints — verified vs aspirational](#9-honest-constraints)

---

## 1. Vision & North Star

### 1.1 The one-sentence vision

> **Cortex is DaVinci Resolve for Sorani speech data:** a local-first studio where a single curator turns raw Kurdish audio into a publishable, provenance-stamped ASR dataset, and where *every correction the curator makes measurably improves the model* — under hard gates that make accuracy regressions impossible to ship.

The DaVinci Resolve analogy is load-bearing and concrete, not marketing. Resolve's reliability does **not** come from project *files*; it comes from three disciplines Cortex already half-implements:

| Resolve discipline | Cortex equivalent (verified present) | Gap to close |
|---|---|---|
| All state in a **Project Library database** | SQLite owns segments/hypotheses/verdicts/runs (`db.rs`; schema currently at migration **v19**) | No first-class `project` container |
| **Source / Optimized+Proxy / Render-Cache** tiering; source never touched | `source_audio_identity()` blake3 hashing (`pipeline.rs`, `pub(crate) fn source_audio_identity`); `cache.rs`, `fingerprint.rs`, `PcmCache` | Not unified into one purgeable, content-addressed cache tier |
| **Non-destructive** edit + versioned timeline | Command-pattern undo/redo (`history/mod.rs`); transactional migrations | No dataset-level snapshots |
| **Render queue** that survives crashes | `job_history` table | It is a *log*, not a durable single-writer *queue* |

### 1.2 What "top-3-class" means here, operationally

"Top-3-class" is a falsifiable bar. Cortex is top-3-class when **all** of these hold simultaneously:

1. **Quality (the product north star is CER).** On a *frozen, in-domain* held-out gold set, jury-curated **CER** beats raw single-engine CER by a large margin. The charter's north star is **≥30% CER reduction at ≤15% escalation** (`AGENT_CHARTER.md` lines reading "measured raw-ASR-vs-jury CER reduction >=30% at <=15% human escalation" and the metrics-table row "Refinery label-quality lift … >=30% CER reduction … OR escalation_fraction>0.15").
2. **The gate the code enforces today is on WER, not CER — this must be reconciled, see §3.4.** `scorecard.rs` computes both `micro_wer` and `micro_cer`, but `beats_baseline` is set by the line `beats_baseline: significant && system_micro_wer < baseline_micro_wer` — i.e. **micro-WER**, gated by MAPSSWE significance. So today the *promotion* gate is WER and the *product north star* is CER. The required fix (§3.4) is to gate on **both**: no WER regression *and* the CER-reduction target, so the promised gate matches the enforced gate.
3. **Robustness:** crash-resumable batch jobs, atomic writes (`atomic_file.rs`), `PRAGMA integrity_check` on open with quarantine-on-corruption (verified in `db.rs::open_with_retry`, the branch that logs "quarantining database" and calls `recover_database_at`), and a promotion gate that **blocks** any model/prompt/threshold change that regresses held-out WER/CER within CI.
4. **Learning:** a closed loop where a single human correction (a) fixes the *same* word next time with **no retraining**, and (b) accumulates into adapters promoted only after passing the gate.
5. **Provenance:** every label traces to the exact model version + adapter that produced it, and every export carries a machine-readable manifest with content hashes and the scorecard.
6. **Honesty:** the app never publishes a number it cannot reproduce, and always keeps an honest stock-engine scorecard shippable (§9). The charter is explicit: "the honest stock-CTC number is always shippable, a flattering fake number never is."

### 1.3 Why Sorani specifically is hard (and why curation is the moat)

Central Kurdish in Arabic script is dominated by two error classes that *no acoustic model alone fixes*: **rare proper nouns** (names, places — unbounded vocabulary) and **orthographic-presentation noise** (ZWNJ `U+200C`, Yeh `ي↔ی`/`U+06D0`, Kaf `ك↔ک`, contextual word-final Heh, tatweel, Arabic vs ASCII digits, hamza, NFC). Cortex already owns a clean-room **MIT** Sorani normalizer (`normalizer.rs::normalize`, with NFC up front, Yeh standardization, and ZWNJ→space) that handles these. That asset is what makes its WER/CER honest and its corrections meaningful. The moat is the *curated correction stream*, not the base model.

---

## 2. The Correction→Learning Flywheel

This is the centerpiece. The design principle, drawn from the 2025 "learn from corrections" literature (see arXiv tags below, each marked read / unread):

> **Most of the value is captured WITHOUT retraining.** Cortex today implements only the slowest, most expensive layer (batch DPO export). The right architecture is **four cooperating loops over the same data Cortex already stores**, each on a different timescale, each gated on the held-out gold set.

### 2.1 What Cortex captures today (verified)

When a curator edits a verdict, `db.rs::record_human_decision` (the `pub fn record_human_decision(&self, segment_id, decision, corrected_transcript)` — note it **requires** a corrected transcript for `"edit"`) writes the human verdict/transcript to `speech_segments` and — **if the segment is not gold** — inserts an `agent_examples` row `(wrong_transcript, human_fix)`. `jury/learning.rs::build_dpo_dataset` then turns those into DPO preference pairs, **excluding any segment whose audio content-hash matches a holdout gold clip** (the `if holdout_hashes.contains(&identity.content_hash)` guard, seeded from `SELECT audio_path FROM gold_segments WHERE is_holdout = 1`). That holdout-by-hash discipline is correct and must be preserved across *all four* loops.

**What is missing:** nothing *consumes* the export, nothing *re-infers* after learning, nothing *re-prioritizes* the queue, and a single correction does **not** change the next decode. The loop is open.

### 2.2 The three timescales (the answer to "what happens when I fix a word")

```
 USER FIXES A WORD  ("ساڵی ٢٠١٤" not "ساڵی ٢٠١٥")
        │
        ▼
┌───────────────────────────────────────────────────────────────────────────┐
│ IMMEDIATE  (no retraining, deterministic, < 1 s — right NEXT time)         │
│   LOOP 0  Error-memory + correction lexicon + RAG post-correction          │
│   → persist a slot key + phonetic key + human token into correction_memory;│
│     next decode of the same confusion prefers the remembered token.        │
│     Engine-agnostic, fully auditable, no model touch. (algorithm in §2.3)  │
└───────────────────────────────────────────────────────────────────────────┘
        │  (correction also appended to the corrections ledger, §2.7)
        ▼
┌───────────────────────────────────────────────────────────────────────────┐
│ SHORT-TERM (hours; no model fine-tune)                                      │
│   LOOP 1  GER / N-best LLM post-correction primed on agent_examples         │
│   LOOP 2  KenLM rebuild (lmplz) + CTC decoder-side word-boost (design)      │
│   → corrected transcripts become an n-gram LM that shallow-fuses/rescores   │
│     the ensemble N-best; corrected words bias the CTC beam.                 │
└───────────────────────────────────────────────────────────────────────────┘
        │  (pairs accumulate; adapter delta staged)
        ▼
┌───────────────────────────────────────────────────────────────────────────┐
│ PERIODIC (when N new corrections accumulate; PEFT/full fine-tune)          │
│   LOOP 3  Incremental LoRA/DPO + replay buffer → eval-gate → promote/rollback│
│   → adapter trained EXTERNALLY (4090/WSL), run through run_gold_eval_*,     │
│     promoted ONLY if it passes the gate (§3.4). Rollback-able.             │
└───────────────────────────────────────────────────────────────────────────┘
```

### 2.3 LOOP 0 — Error memory (ship first; the cleanest "no-retrain" answer)

**The single most important non-obvious constraint** (per k2-fsa sherpa-onnx docs and GitHub issues read at cutoff — *re-verify against the live changelog*, §9): **sherpa-onnx hotwords/contextual biasing work for transducer models with `modified_beam_search`, not for CTC.** Cortex's on-device default engine is OmniASR **CTC**. So the built-in hotwords path **does not exist on Cortex's default engine.** Do not claim "Cortex has hotword biasing." The engine-agnostic substitute is an **error-memory + retrieval correction**, which fits Cortex's ensemble perfectly because the ROVER confusion network is *already built* (`scripts/sorani_ensemble_asr.py::rover_fuse` — anchor = longest hyp, per-slot `Counter` vote, verified in that file).

**Concrete, buildable algorithm (this is the part the previous draft hand-waved):**

1. **Slot-key construction.** When a human edits slot *s* in the ROVER confusion network, build the trigger key from the *normalized* neighbor context, not raw text:
   - Run the left and right neighbor words through `normalizer.rs::normalize` (NFC → Yeh-fold → Kaf-fold → ZWNJ→space) so `(left_word, right_word)` is canonical.
   - `slot_key = (norm(left_word), norm(right_word))` — an **exact-match windowed tuple** with window = 1 each side. (A wider window is a tunable; ship with ±1, which is what the `agent_examples`/ROVER context already affords.)
2. **Phonetic key.** Compute `phonetic_key = g2p(norm(wrong_token))` using `normalizer/g2p.rs::g2p` (the `pub fn g2p(text: &str) -> String`). Similarity between an incoming candidate and a remembered entry uses `diff/phonetic.rs::normalized_phonetic_word_distance` (the `pub fn normalized_phonetic_word_distance(w1, w2) -> f64`), with an **explicit threshold `phon_tau` on the normalized phonetic distance** (start at `phon_tau = 0.2`; this is a tunable, not a benchmark).
3. **Firing rule (deterministic, gated).** On a future decode, after ROVER fusion, an entry **fires for slot *s* iff ALL hold:**
   - exact `slot_key` match (normalized ±1 neighbors), **AND**
   - `normalized_phonetic_word_distance(g2p(candidate), phonetic_key) ≤ phon_tau`, **AND**
   - entry `confidence > tau_conf` (start `tau_conf = 0.6`), **AND**
   - `hit_count ≥ 1` elsewhere (the same fix has been independently confirmed on at least one other segment) — this is the anti-one-off guard.
4. **Integration with `rover_fuse` votes.** The memory does **not** silently override the anchor. It **adds one weighted vote** for `human_token` into the per-slot `Counter` (weight = `confidence`), so a strong ensemble consensus can still out-vote a weak memory. Only when the memory vote wins the `Counter` does the slot change. This keeps the existing ROVER semantics intact and auditable (record `loop_applied='loop0'` on the corrections ledger when it fires).
5. **Confidence / decay update.** On a confirmed re-fire that the human accepts, `confidence ← min(1.0, confidence + 0.1)` and `hit_count += 1`; on a human *reject* of a memory-driven change, `confidence ← confidence * 0.5` (fast decay), and below `0.2` the entry is retired.
6. **Eval gate — the proxy to minimize is over-triggering.** Define **over-trigger rate = (# slots the memory changed where the original ROVER token was already correct on the gold set) / (# slots the memory fired on)**. The LOOP-0 eval gate (on `is_holdout=1`) requires CER non-regression *and* over-trigger-rate below a committed budget; tune `phon_tau`, `tau_conf` against AsoSoft/Common-Voice-`ckb` held-out CER. This is what keeps "fix it once → right forever" from poisoning correct words.

**New schema — migration `v20` `correction_memory`:**

```sql
-- v20: error-memory for LOOP 0
CREATE TABLE correction_memory (
  id               TEXT PRIMARY KEY,
  wrong_token      TEXT NOT NULL,
  human_token      TEXT NOT NULL,
  slot_key         TEXT NOT NULL,   -- normalize()'d (left_word ⏐ right_word) tuple, the trigger key
  phonetic_key     TEXT NOT NULL,   -- g2p(normalize(wrong_token))  (g2p.rs)
  source_segment   TEXT NOT NULL REFERENCES speech_segments(id),
  model_version_id TEXT,            -- provenance (see §3)
  confidence       REAL NOT NULL DEFAULT 1.0,
  hit_count        INTEGER NOT NULL DEFAULT 0,
  last_fired_at    TEXT,
  created_at       TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_corrmem_slot ON correction_memory(slot_key);
CREATE INDEX idx_corrmem_phon ON correction_memory(phonetic_key);
```

### 2.4 LOOP 1 — Generative Error Correction over the N-best (no retrain, highest accuracy leverage)

Cortex already produces a multi-engine N-best per segment (`segment_hypotheses`: `model_id`+`transcript`+`confidence`) and a ROVER consensus. Feed `{OmniASR-7B, Whisper-ckb, XLSR-ckb}` hypotheses **plus phonetic readings of each candidate** to the existing LLM refiner (`llm_refiner.rs` — local Ollama / cloud Gemini) using a HyPoradise-style instruction, few-shot-primed on `agent_examples` and RAG-augmented with `correction_memory` hits.

**Correct the prior draft here:** `get_few_shot_examples` (`jury/mod.rs::get_few_shot_examples`) is **not a stub** — it executes `SELECT … FROM agent_examples ORDER BY created_at DESC LIMIT ?1` and returns real rows. The precise gap is that it **ignores its `segment_id` argument** (the parameter is declared `_segment_id`), so retrieval is **globally recency-ordered, not relevance-ranked** to the current segment. **Fix:** add segment-relevance retrieval — rank `agent_examples` by phonetic/lexical similarity to the current segment (reuse `diff/phonetic.rs` + `normalizer.rs`) so few-shot priming is on-topic instead of merely recent. (The function's own doc-comment already flags "Phase 6 will upgrade to embedding-based retrieval" — this is that upgrade.)

- **Conservative mode (autonomy-dial low):** N-best **rescoring** — the LLM may only *choose among* real hypotheses, never invent tokens. Low hallucination risk.
- **Aggressive mode (autonomy-dial high):** free GER, grounded by phonetic readings and a confidence floor from the ensemble agreement score.

**Citations with verification status (do not let any algorithmic decision rest on an unread ID):**
- HyPoradise (NeurIPS 2023) — *concept read, ID not independently re-verified*.
- Whispering-LLaMA cross-modal GER (EMNLP 2023, arXiv:2310.06434) — *unread-not-verified*; cited only as a future option, no decision depends on it.
- Phonetic-grounded GER for low-resource scripts (Interspeech 2025, arXiv:2505.17410) — *unread-not-verified*; presented as a **design choice** (phonetic readings as anchors), not as a cited result.
- RAGEC / DeRAGEC retrieval correction (arXiv:2409.06062, arXiv:2506.07510) — *unread-not-verified*; the retrieval design stands on its own engineering merits, not on these numbers.

**Risk:** free GER can hallucinate fluent-but-wrong Sorani names — exactly Cortex's hardest case; default to conservative mode.

### 2.5 LOOP 2 — KenLM rebuild + CTC-side biasing (hours per batch, cheap)

Accumulate `is_holdout=0` corrected transcripts → **normalize them with `normalizer.rs` BEFORE training** (so the LM matches the scorer — a real failure mode if mismatched; the charter calls a normalization mismatch "a fabrication bug") → `kenlm lmplz -o 3..4 --prune …` → shallow-fuse/rescore the ensemble N-best. Because the default engine is CTC, the contextual-biasing path that works is **decoder-side**.

**Design choice, not a cited result:** compile corrected words into a small WFST/lexicon **word-boost over the CTC logits**. A WCTC-style biasing recipe (arXiv:2506.01263, "WCTC-Biasing") is **unread-not-verified** at my cutoff — I present the word-boost-over-CTC-logits approach as an engineering design, and **no decision here depends on that paper's numbers**. Prior Sorani work used XLS-R + an n-gram LM, so KenLM is an established, low-risk path for `ckb`. Gate the corpus on `is_holdout=0` exactly like `build_dpo_dataset` already does.

### 2.6 LOOP 3 — Incremental LoRA/DPO with replay buffer (periodic; touches weights)

Trigger when *N* new `is_holdout=0` corrections accumulate (count `agent_examples`). Train an adapter **externally** (RTX 4090 / WSL) via HF **TRL/PEFT**. Two non-negotiables:

1. **Replay buffer is mandatory.** LoRA alone can still forget (continual-learning literature, arXiv:2407.00756 — *unread-not-verified*; the replay-buffer mitigation is standard practice regardless). Mix a frozen sample of prior verified segments + general Sorani (FLEURS/Common-Voice `ckb`) at a fixed ratio.
2. **Promote only if it passes the gate (§3.4).** Run `run_gold_eval_with_transcriber` (`eval.rs`, runs the *live* engine) → `scorecard.rs` → promote only if the reconciled WER-and-CER gate holds with no per-slice regression.

Emit **KTO** (binary — matches Cortex's accept/reject jury better than paired DPO) and token-level preference alongside the existing DPO export. Run **confident-learning / Cleanlab-style** label-noise filtering over `agent_examples` first so a wrong human edit cannot poison every loop.

**Which engine gets which fine-tune (this is the user's real question — see §3.2 for the 7B bridge):** LoRA/QLoRA/DoRA are first-class for the **Whisper-ckb** engine. For the **7B**, the *ingestion artifact* may itself be a LoRA adapter (the realistic single-4090 path) — §3.2 documents the merge-to-base bridge that turns it into an ingestible checkpoint.

### 2.7 The data/event model for the flywheel

A corrections ledger (migration `v21`) makes the training set reconstructable and every label attributable:

```sql
-- v21: corrections provenance ledger
CREATE TABLE corrections (
  id                  TEXT PRIMARY KEY,
  segment_id          TEXT NOT NULL REFERENCES speech_segments(id),
  audio_content_hash  TEXT NOT NULL,         -- single source of truth for holdout exclusion
  raw_hypothesis      TEXT NOT NULL,
  ensemble_hyps_json  TEXT,                  -- each engine's transcript
  agreement_score     REAL,                  -- cross-architecture agreement (LOOP-1 signal)
  jury_verdict        TEXT,
  human_fix           TEXT NOT NULL,
  model_version_id    TEXT,                  -- WHICH model produced the corrected label (§3)
  adapter_id          TEXT,                  -- WHICH adapter was active
  reviewer_id         TEXT,
  loop_applied        TEXT,                  -- which loop changed the output (audit; e.g. 'loop0')
  decided_at          TEXT NOT NULL DEFAULT (datetime('now'))
);
```

**Sequencing for a DaVinci-grade result:** LOOP 0 → 1 → 2 → 3. The first three deliver visible "it learned from my fix" behavior with zero training infrastructure and full auditability; LOOP 3 compounds it into the weights.

---

## 3. Fine-tuning Integration & Model Registry

### 3.1 The discipline: training is a backend, Cortex is the safe control surface

The user fine-tunes the 7B **externally**. A 7B *full* fine-tune does **not** fit one RTX 4090 — the public `omnilingual-asr` recipe assumes a multi-GPU cluster *[scale figure unverified at cutoff; treat "32–96 GPUs" as an order-of-magnitude claim to confirm against the published recipe]*; only inference fits the 4090 *[≈17 GiB BF16 is an estimate, unverified in-tree]*. Cortex's job is **ingest → eval-gate → A/B → promote → rollback → hot-swap** — never training the 7B locally. This is Resolve's "render farm vs. project" separation.

### 3.2 The clean ingestion path (fairseq2-native) — **including the LoRA-of-7B case the user most likely has**

Because the user's model is **fairseq2**, the cleanest drop-in needs **zero change** to the Python inference code: a fairseq2 **asset card**. Cortex writes a user-level YAML to `~/.config/fairseq2/assets/<name>.yaml`:

```yaml
name: omniASR_ckb_v3@user
base: <RESOLVED_BASE_CARD>      # MUST be confirmed at integration time — see precondition below
checkpoint: "/mnt/c/.../models/<sha256>/consolidated.pt"   # WSL path, since the 7B runs in WSL
```

**Hard precondition (new requirement).** The code currently passes `omniASR_LLM_7B_v2`, and I cannot verify *any* published Meta base card — neither `…_v2` nor the unsuffixed `omniASR_LLM_7B`. **The import path MUST resolve the real published base-card name at integration time and fail loudly if it is unresolvable**, rather than silently writing a `base:` override that will not resolve against any fairseq2 asset registry. Do not hardcode `…_v2`; do not assume the unsuffixed name either — query the installed `omnilingual-asr` asset registry and abort ingestion if the base is unknown.

**Two ingestion artifact shapes — handle both:**

- **(A) Full fine-tune → `consolidated.pt`.** The straightforward case: SHA-pin the checkpoint, register it, point the asset card at it.
- **(B) LoRA/QLoRA adapter of the 7B → merge-to-base bridge (the realistic single-4090 path).** Since a full 7B fine-tune is infeasible on the user's hardware (§3.1), the artifact they actually produce is most plausibly a **QLoRA adapter**. fairseq2's own LoRA save/load was, at my cutoff, reported incomplete (issue **#310** — *third-party state, unverified post-cutoff; treat as "re-check before relying on it"*). The **ingestion bridge is: merge the adapter into the base weights → emit a `consolidated.pt`**, then ingest via path (A). Registry implications: record `base_version_id` = the base 7B's `model_versions.id`, `adapter_sha256` = the adapter's hash, **and** the resulting merged-checkpoint `checkpoint_sha256`; the `adapters` row (below) captures the lineage so the merge is reproducible. This closes the gap where the user's most likely artifact (a QLoRA of the 7B) otherwise had no ingestion story.

**Caveat (verified):** the `checkpoint:` path must be the **WSL-visible** `/mnt/c/...` path, since the 7B runs in WSL; a Windows path silently fails.

### 3.3 The registry (upgrade `model_manifest`)

Today `model_manifest` is `(filename, size_bytes, sha256, source_url, version, installed_at)` — a SHA-pinned *seed*, but with **no version lineage, no status, no adapter chain**. Two new tables:

```sql
-- v22: model_versions registry
CREATE TABLE model_versions (
  id                TEXT PRIMARY KEY,
  family            TEXT NOT NULL,        -- omniasr-7b | omniasr-ctc-1b | whisper-ckb | xlsr-ckb
  model_card_name   TEXT,                 -- the RESOLVED fairseq2 asset-card name (for swap)
  checkpoint_sha256 TEXT NOT NULL,        -- MUST be non-empty for trusted promotion (see §3.4)
  checkpoint_path   TEXT NOT NULL,
  base_version_id   TEXT REFERENCES model_versions(id),   -- provenance chain (LoRA merge → base)
  source            TEXT NOT NULL,        -- meta-stock | user-finetuned | cortex-finetuned
  license           TEXT NOT NULL,
  eval_run_id       TEXT REFERENCES eval_runs(id),
  gold_wer REAL, gold_cer REAL, gold_ci_low REAL, gold_ci_high REAL,
  mapsswe_p_vs_active REAL,
  scorecard_json    TEXT,
  status            TEXT NOT NULL DEFAULT 'candidate',  -- candidate|challenger|champion|rolled_back|rejected
  promoted_at       TEXT,
  created_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
-- v23: adapter lineage
CREATE TABLE adapters (
  id                       TEXT PRIMARY KEY,
  parent_model_version_id  TEXT NOT NULL REFERENCES model_versions(id),
  base_checkpoint_sha      TEXT NOT NULL,
  adapter_sha256           TEXT NOT NULL,
  merged_checkpoint_sha    TEXT,                 -- the consolidated.pt produced by merge-to-base (§3.2 B)
  training_corrections_query_hash TEXT,          -- which corrections trained it (reproducible)
  recipe                   TEXT,
  created_at               TEXT NOT NULL DEFAULT (datetime('now'))
);
```

Exactly one row per family is `champion`. The active model is selected by a single settings row that the Python pipeline reads — promotion flips it; rollback flips it back.

### 3.4 The promotion gate (block/promote decision) — **with the checkpoint-pin and metric fixes**

```
 INGEST consolidated.pt
   │  sha256 verify (models.rs::verify_extracted_against_pin)
   │  ── REGISTRY IMPORT PRECONDITION (NEW): REQUIRE a non-empty checkpoint_sha256.
   │      Reject empty-pin ingestion on the trusted-promotion path. (see box below)
   │  record license + provenance → status='challenger'
   ▼
 EVAL on gold WHERE is_holdout=1   (run_gold_eval_with_transcriber — LIVE engine)
   ├─ jiwer-style WER/CER + bootstrap CI         (scorecard.rs)
   ├─ MAPSSWE significance vs champion           (significance.rs::mapsswe)
   └─ per-slice regression block                 (per-speaker / per-condition)
   ▼
 PROMOTE iff  (no WER regression: system_micro_wer < baseline_micro_wer AND MAPSSWE p<0.05  [the existing
              scorecard.rs `beats_baseline` rule])
         AND  (CER target met: meets the charter's CER-reduction / non-regression budget  [NEW — reconciles
              the product north star with the enforced gate])
         AND  no protected-slice regression
   │    else status='rejected' (scorecard attached)
   ▼
 ATOMIC SWAP: flip active-model settings row; drain+reload asr_pool / WSL process
   ▼
 ROLLBACK: flip back; status='rolled_back'  (every consolidated.pt + card retained, content-addressed)
```

> **Soften the runtime-tamper guarantee — this was overstated.** `models.rs::verify_extracted_against_pin` rejects a mismatch **only if a non-empty pin is recorded**: its first line is `if pinned.is_empty() { return Ok(()); }`, and the test `verify_extracted_against_pin_semantics` asserts `verify_extracted_against_pin("f.onnx", "abc123", "")` is `Ok`. Several `MODELS[]` entries historically ship with `sha256: ""` (e.g. `OMNIASR_CTC_300M_ARCHIVE_SHA256 = ""`). So an incoming user 7B `consolidated.pt` **with no recorded pin is NOT refused — it sails through.** The empty-pin-is-OK behavior is correct for **first-seed bootstrap** (archive hashes that cannot yet be computed on disk), but it is **wrong for trusted promotion.** **Requirement:** the registry import path MUST compute and **require a non-empty `checkpoint_sha256`**, and reject empty-pin ingestion for any `source` in {`user-finetuned`, `cortex-finetuned`}. The guarantee is therefore: *"a tampered or mismatched checkpoint is rejected at install **iff** a non-empty pin is recorded — and the import path makes a non-empty pin mandatory for promotion."*

**Metric reconciliation (the §1.2 issue, resolved here):** the code's `beats_baseline` gates on **micro-WER**; the charter's north star is **CER**. The gate above **requires BOTH** — the existing WER `beats_baseline` rule *and* the CER-reduction budget — so the gate the doc promises matches the gate the code enforces, plus the product metric the charter demands.

**A/B shadow (optional):** transcribe a sample of recent real segments with both champion and challenger; surface disagreements in `ReviewInbox` for human spot-check, and use old-vs-new 7B disagreement to localize exactly where the fine-tune changed behavior.

### 3.5 Risks specific to ingestion

- **Windows/WSL path:** the `checkpoint:` must be WSL-visible (`/mnt/c/...`); a Windows path silently fails.
- **`ckb` corpus inclusion is UNCONFIRMED, not "excluded."** Per the published language-coverage list at my cutoff, **`ckb_Arab` appears as a supported decode target, but whether it was in the 7B's training corpus is unconfirmed** — and I cannot have read any post-cutoff language list. Do **not** present corpus-exclusion as settled fact. The honest statement: *the user's Sorani 7B gains are unproven until run through Cortex's gold harness*, and the stock-CTC scorecard stays always-shippable regardless.
- **Disk growth:** size figures for `consolidated.pt` (~30 GiB) and BF16 inference (~17 GiB) are **estimates, unverified in-tree**; regardless of the exact number, add a retention policy (keep champion + last-good + N candidates; GC rejected ones after archiving their scorecard).
- **CTranslate2 caveat:** CT2/faster-whisper applies to **Whisper only**, not fairseq2 `consolidated.pt`. Do not promise a CT2 fast-path for the 7B; its fast path is the existing sherpa/fairseq2 route.

---

## 4. Agentic + Ensemble + Jury System

### 4.1 The diverse 3-architecture ensemble (the disagreement engine)

Three architecturally-distinct engines that **fail differently** (verified in `scripts/sorani_ensemble_asr.py`):

| Engine | Architecture | Role | Verified caveat |
|---|---|---|---|
| OmniASR-LLM-7B | encoder + LLM decoder | consensus primary / teacher | fairseq2 WSL; the script passes `model_card="omniASR_LLM_7B_v2"` — **`…_v2` is an unresolved local override** that must be reconciled to the real base card at integration time (§3.2). Not usable as-listed until resolved. |
| `roseman/whisper-medium-ckb` | seq2seq | diversity voter | *upstream last-updated date unverified at cutoff* — refresh candidate |
| `Akashpb13/Central_kurdish_xlsr` | wav2vec2 CTC | diversity voter | CV8-era, no n-gram LM bundled → KenLM is free headroom |

`agreement()` computes mean pairwise character-agreement; `rover_fuse()` builds a ROVER confusion network and majority-votes per slot. The on-device fast path is the separate sherpa-onnx OmniASR **CTC** int8 engine (archive dated 2025-11-12).

### 4.2 ROVER/IRT consensus + cross-session ability persistence

The ensemble feeds `segment_hypotheses`, which `quality/irt.rs::fit_irt_consensus` fuses with a **1PL IRT** model (EM, `iterations = 50`, per-model abilities `theta_j`, per-segment difficulty `b_i`). **Verified gap:** abilities are **session-local** — `update_abilities = segment_slots_map.len() >= 10` (so abilities only update when ≥10 segments are present) and they are **refit from scratch each session**; human corrections do not feed back into ability estimates.

**Concrete mechanism to persist abilities (this was named but not specified before):**
- **Storage:** add a `model_abilities` table keyed by `model_version_id` (one row per engine version): `(model_version_id TEXT, theta REAL, n_obs INTEGER, updated_at TEXT)` — or, equivalently, a `theta` + `n_obs` column pair on `model_versions`. Keying by `model_version_id` (not by family) is what makes the prior valid across fine-tunes.
- **Warm-start prior:** when a new session's EM begins, initialize each engine's `theta_j` from the persisted value instead of zero, and **mix the persisted estimate with the fresh estimate by precision-weighting on `n_obs`**: `theta_warm = (n_prior·theta_prior + n_session·theta_session) / (n_prior + n_session)`. New evidence dominates once the session accumulates observations; a cold engine inherits its long-run ability.
- **Human-correction feedback (currently absent):** when a human verdict resolves a slot, treat the resolved token as ground truth for that slot and update each engine's `theta` toward "did this engine match the human?" — i.e. corrections become labeled observations that nudge `theta`, persisted back to `model_abilities`. Without this, abilities only ever reflect inter-engine agreement, never the human's ground truth.

`quality/conformal.rs::calibrate_and_certify(verified, 0.05, 0.90)` then sets a risk-controlled auto-accept threshold (fallback 0.35 when <10 verified), replacing a hard-coded 0.9/0.95 with a *calibrated* certificate.

### 4.3 The tiered jury (verified routing)

```
 T0 GATE (jury/mod.rs::run_t0_gate_with_autonomy)
   disagreement_score = 1.0 - irt_confidence          [verified: the `let disagreement_score = 1.0 - irt_confidence;` line]
   nonconformity = ((1 - irt_confidence) + 0.1·(-ctc)).max(0)
   AutoAccept  iff nonconformity ≤ threshold AND NOT poor_quality (SNR<5 or clip>0.1)
   else EscalateToT1
        │
        ▼
 T1 TEXT JUDGE (jury/t1_judge.rs)  lexicon coverage + char-trigram perplexity + conf
        │  score ≥ jury_t1_threshold (0.75) → Commit, else EscalateToT2
        ▼
 T2 AGENTIC REFINER (agentic.rs + llm_refiner.rs)
   reference-aware selection: 0.52·window + 0.18·global + 0.15·quality + 0.15·prior
   commit iff score≥0.72 ∧ margin≥0.08 ∧ window_overlap≥0.45
        │
        ▼
 DEBATE (jury/debate.rs)  diverse judges + swap-agreement (position-bias guard)
```

### 4.4 Active-learning review prioritization (the disagreement-driven upgrade)

**Verified weakness:** `get_escalation_queue` orders by `ORDER BY COALESCE(agent_confidence, 0.5) ASC` (the literal clause in `jury/mod.rs::get_escalation_queue`) — a single, uncalibrated proxy. The fix is a **composite query-by-committee utility** (Settles' Active Learning survey is the canonical reference) stored as a `priority_score`:

```
priority = (w1·ensemble_disagreement       -- 1 - agreement_confidence (the TRUE signal)
          + w2·irt_uncertainty             -- 1 - irt_confidence (already computed)
          + w3·conformal_nonconformity)    -- (1-conf)+0.1·(-ctc)  [conformal.rs]
          · representativeness             -- 1 - ood_score  (anti-outlier; quality/ood.rs)
          / (duration_ms / 1000)           -- value per SECOND of human listening (cost-aware)
```

This is "maximum model improvement per human-minute." Tune `w1..w3` empirically against AsoSoft/Common-Voice-`ckb` held-out CER; do **not** present the weights as cited benchmarks. The ROVER columns also give **word-level** disagreement, so the UI can highlight the exact conflicted word.

**Pseudo-labeling the easy tail:** segments where all three engines agree above the *conformal-certified* threshold become silver data, auto-committed at autonomy ≥ ActConfirm, **never shown to the human**. (NST / Alternative-Pseudo-Labeling, arXiv:2404.07341 and arXiv:2308.06547 — both *unread-not-verified*; the design stands on the conformal certificate, not on those papers.) **Risk:** auto-accepting high-agreement segments can reinforce a *shared* blind spot of all three engines; periodically human-audit a random sample and keep the conformal bound conservative.

### 4.5 Autonomy controls + guardrails + the typed agent contract

`AutonLevel` (verified enum in `settings.rs`): **Observe → Propose → ActConfirm → ActAuto**. Four named guardrails on the auto-accept path: (1) rate cap, (2) mandatory random spot-check (force N% of auto-accepts into the inbox), (3) kill switch (instant demote to Propose), (4) signed append-only audit trail (hash-chained; also the crash-recovery substrate, §8). Tie an **auto-demotion** trigger to the conformal certificate: if measured coverage drops below target on recent spot-checks, fall back to Propose.

**Make the agents' contract machine-checked — with a defined schema and IPC protocol (this was asserted but not specified before):**

The shared blackboard is three JSON record types, validated on every read:

```
Hypothesis  { engine_id: string, transcript: string, confidence: number,
              per_slot_nbest: [ { slot: int, candidates: [ {token, score} ] } ] }
Evidence    { source_transcripts: Hypothesis[], agreement_score: number,
              irt_confidence: number, conformal_nonconformity: number,
              correction_memory_hits: [ {slot_key, human_token, confidence} ] }
Verdict     { segment_id: string, decision: "accept"|"edit"|"reject"|"escalate",
              transcript: string, confidence: number, rationale: string,
              tier: "T0"|"T1"|"T2"|"debate" }
```

Enforce it with Gemini `responseSchema` and **llama.cpp GBNF grammars** for local GGUF judges (JSONSchemaBench, arXiv:2501.10868 — *unread-not-verified*; constrained-decoding is a design choice here, not a benchmarked claim). **Constrained decoding guarantees schema validity, not transcript correctness** — the gold-WER/CER gate remains the only real correctness guard.

**EngineSpec sidecar contract (the substrate that makes "7B is just another engine" buildable):**
- **Request (JSON):** `{ audio_path: string | pcm_handle: string, lang: string, model_card: string }` — `audio_path` for file-backed segments, `pcm_handle` (a content-addressed cache key) for in-memory PCM.
- **Response (JSON):** `{ transcript: string, confidence: number, per_slot_nbest: [...] }` — same `per_slot_nbest` shape as `Hypothesis`, so the sidecar output drops straight into ROVER.
- **IPC framing:** **newline-delimited JSON over the sidecar's stdin/stdout** (one request object per line, one response object per line), so the Rust host can stream without a length-prefix parser; large PCM travels by `pcm_handle` into the shared content-addressed cache, never inline.
- **Crash / error semantics:** a **non-zero sidecar exit (or malformed/absent response line) FAILS the job** — the host writes the job to `job_history` as `failed`, increments `retry_count`, and (per §8.2) the durable queue re-queues it up to a retry cap before parking it for human attention. A crashing 7B thus fails *its job*, not the app, and the failure is attributable.

The user's fine-tuned 7B should ideally enter as a **4th committee member**, not a swap — diversity makes disagreement informative and gives a built-in old-vs-new A/B channel.

---

## 5. The Tool/Tech Stack

Concrete picks per layer for **Windows + WSL2 + RTX 4090 (24 GB)**, local-first.

> **Versions are UNVERIFIED at my cutoff.** There is **no dependency manifest in the tree that pins any of these** — `src-tauri/Cargo.toml` pins only `tauri = 2`, and there is no `requirements.txt`/`pyproject` pinning the Python tools. Every version/date below is therefore marked `[unverified at cutoff; pin and re-verify at integration time]`. The **only** package facts I can verify from the tree are the `omnilingual_asr.models.inference.pipeline.ASRInferencePipeline` import path (in the ensemble script) and the sherpa-onnx CTC archive URLs dated **2025-11-12** (in `models.rs`).

| Layer | Recommended pick | Why this one | Caveat |
|---|---|---|---|
| **7B inference** | `omnilingual-asr` on fairseq2; Meta OmniASR-LLM-7B | the package behind "OmniASR-7B"; import path verified in-tree | `[PyPI version/date, model size, license — unverified at cutoff]`; no verifiable LoRA recipe; base-card name unresolved (§3.2) |
| **On-device ASR runtime** | **sherpa-onnx** (k2-fsa) — embedded in Rust; OmniASR CTC int8 (archive **2025-11-12**, verified) | no Python/network; binds into Rust | **CTC ⇒ no built-in hotwords** (transducer-only) `[re-check live changelog]` |
| **Whisper-family inference** | **faster-whisper** (CTranslate2, INT8/FP16) | faster local; built-in VAD | CT2 ≠ fairseq2 7B; `[version unverified at cutoff]` |
| **PEFT / training** | **Unsloth** QLoRA; **HF PEFT + Trainer** as the conservative fallback; **Axolotl** for config-driven | DoRA/QLoRA first-class | Unsloth ASR adapter save/reload maturity and `torchtune` EOL status are `[third-party state, unverified post-cutoff]` |
| **Preference opt** | HF **TRL** (DPO/KTO/ORPO trainers) | KTO's binary signal matches the jury | `[version unverified]` |
| **Serving (multi)** | **NOT vLLM** for a single desktop user | overkill; audio path immature | — |
| **Eval** | **jiwer**-style WER/CER over `normalizer.rs` output + existing ROVER harness | hallucination-aware empty-ref behavior | `[jiwer version unverified]`; keep the Sorani normalizer |
| **Registry/tracking** | **MLflow** (self-hosted) as a **pattern**, backed by Cortex's SQLite | local-first; offline-safe | `[version unverified]`; W&B/Weave is hosted-first → conflicts with offline posture |
| **Data versioning** | **DVC** (or private HF Hub repo) for checkpoints/corpora | Git-tracked, local | a reported lakeFS/DVC corporate change is `[unverified post-cutoff]` — does not affect the design |
| **Data prep** | **Lhotse** Cut/CutSet manifests | bridges SQLite corrections → reproducible splits | — |
| **Annotation** | in-app `ReviewInbox` (defer **Argilla** until multi-annotator) | single user covered | — |
| **Audio** | **ffmpeg + soundfile + torchaudio** (already used) | correct standard trio | no change |
| **Crash reporting** | `std::panic::set_hook` (local dump) + opt-in **sentry-tauri** | biggest current robustness gap | `[version unverified]`; scrub transcripts (`secret_redaction.rs`) |
| **Provenance manifest** | **MLCommons Croissant** JSON-LD | machine-actionable lineage; HF/Kaggle-consumable | `[version/date unverified]` |
| **Constrained decoding** | **llama.cpp GBNF** (local) / **XGrammar** (if vLLM) | schema-valid verdicts | — |

---

## 6. Kurdish/Sorani Resource Map

License is the headline — share-alike and non-commercial contamination are real, shippable risks. **The license strings below are quoted from the repo's `DATA_GOVERNANCE.md`, not asserted independently.**

### 6.1 ASR

| Asset | License | How Cortex uses it | Caveat |
|---|---|---|---|
| Meta Omnilingual ASR (CTC + LLM-ASR) | Apache-2.0 *[license/version unverified at cutoff]* | on-device CTC 300M/1B; 7B teacher/fine-tune target | any published ckb_Arab 7B CER figure (e.g. "6.0") is `[unverified at cutoff]` and is the *teacher* number, **not** on-device CTC; **`ckb` training-corpus inclusion is unconfirmed** (§3.5) |
| `roseman/whisper-medium-ckb` | Apache-2.0 *[unverified]* | ensemble diversity voter | older stack — refresh candidate |
| `Akashpb13/Central_kurdish_xlsr` | Apache-2.0 *[unverified]* | ensemble CTC voter | CV8-era, no bundled LM → KenLM is free headroom |
| `razhan/whisper-small-ckb` | Apache-2.0 *[unverified]* | ensemble only | self-reported WER **likely contaminated; never cite as eval anchor** |
| `Qulabarzi21/whisper-small-ckb-fleurs-pro` | Apache-2.0 *[unverified]* | ensemble only | **FLEURS-trained → do not eval on FLEURS** |

> **Publish Cortex's OWN on-device CTC-300M `ckb` CER on a pinned test set. Do not inherit any 7B CER number.**

### 6.2 NLP / normalization — the license fork

| Asset | License | Decision |
|---|---|---|
| **AsoSoft Library / `asosoft` PyPI** | **MIT** *[version/date unverified at cutoff]* | ✅ `normalizer.rs` is the clean-room MIT port; cross-check parity against the PyPI package so in-app and published numbers match |
| **KLPT** (Kurdish Language Processing Toolkit) | **CC-BY-SA-4.0** | ❌ **share-alike copyleft — NEVER link/derive.** `normalizer.rs` documents this avoidance; do not regress |

### 6.3 Datasets — **reconciled against the actual `DATA_GOVERNANCE.md`**

The previous draft wrongly called the AsoSoft asset "NON-COMMERCIAL" and claimed the governance file "omits the NC restriction." **Both are false.** The repo's `DATA_GOVERNANCE.md` labels **AsoSoft-600 as `CC-BY-SA-4.0`** (sourced via `PawanKrd/asr-ckb-v2`) and **already carries a "SHARE-ALIKE CONTAMINATING" warning**. A *separate* entry — **CORDI** — is the `CC-BY-NC-SA-4.0` (NON-COMMERCIAL & SHARE-ALIKE) one. These are **different obligations** and must not be conflated.

| Dataset | License (as in `DATA_GOVERNANCE.md`) | Obligation | Use |
|---|---|---|---|
| Mozilla Common Voice `ckb` | **CC0-1.0** | none (public domain) | safest — fine-tuning + LM text; **not** the headline test set |
| FLEURS `ckb_iq` | **CC-BY-4.0** | attribution | secondary public comparable; pin the revision SHA |
| **AsoSoft-600** | **CC-BY-SA-4.0** | **share-alike copyleft** (derivatives must also be CC-BY-SA-4.0) | benchmark + LM text; **isolate AsoSoft-derived exports under CC-BY-SA-4.0** — the file's existing contamination warning is correct, keep it |
| **CORDI** (dialect corpus) | **CC-BY-NC-SA-4.0** | **NON-COMMERCIAL *and* share-alike** | research/fairness only; **blocked from any commercial/redistributable training export** |
| PawanKrd `asr-ckb-v2` / `tts-ckb` | gated | clear license/consent | resolve before any train/eval target |

**The two distinct risks to gate separately:** (a) **share-alike copyleft** (AsoSoft-600 CC-BY-SA: derivatives inherit CC-BY-SA), and (b) **non-commercial** (CORDI CC-BY-NC-SA: cannot ship in a paid artifact at all). The share-alike-contamination gate must enforce *both* obligation classes, per the asset's actual label — not a blanket "NC."

### 6.4 TTS & LM

- **F5-TTS:** code is permissive, but pretrained checkpoints are typically **CC-BY-NC** — NC **propagates** into derived Sorani TTS weights (a commercial blocker). A peer-reviewed F5-TTS→Sorani adaptation reportedly exists *[publication date/venue unverified at cutoff]* — confirm before relying on it.
- **KenLM:** no canonical public `ckb` LM ships ready-made — **build one** from AsoSoft Text + CC-0 Common Voice transcripts. (Note: an LM built from AsoSoft text inherits AsoSoft's **CC-BY-SA** share-alike, not NC — rebuild from CC-0 sources if a permissive LM is needed.)

---

## 7. E2E Module Reorganization + Phased Roadmap

### 7.1 Clean architecture: 8 modules over a typed event bus

The current layout is pipeline-centric (`pipeline.rs` calls ASR/jury/runs inline). Refactor into eight modules communicating through the **`PipelineEvent`** bus that *already exists* in `pipeline.rs`, each owning its tables and exposing a narrow command surface:

```
┌──────────────┐  ┌───────────────────┐  ┌────────────────┐  ┌──────────────────────┐
│ 1 INGESTION  │  │ 2 INFERENCE/      │  │ 3 CONSENSUS/   │  │ 4 REVIEW /           │
│ audio,       │─▶│   ENSEMBLE        │─▶│   JURY         │─▶│   ACTIVE-LEARNING    │
│ chunking,    │  │ asr.rs + WSL-7B + │  │ irt.rs,        │  │ priority_score queue │
│ diarization, │  │ ensemble script,  │  │ conformal.rs,  │  │ + correction capture │
│ denoiser,    │  │ versioned hyps    │  │ jury/* +debate │  │ (LOOP 0 memory)      │
│ fingerprint  │  └───────────────────┘  └────────────────┘  └──────────┬───────────┘
└──────────────┘                                                        │ corrections
                                                                        ▼
┌──────────────┐  ┌───────────────────┐  ┌────────────────┐  ┌──────────────────────┐
│ 8 OBSERVABIL.│  │ 7 EXPORT          │  │ 6 REGISTRY     │  │ 5 TRAINING / ADAPTER │
│ telemetry,   │◀─│ export_bundle +   │◀─│ model_versions │◀─│ corrections→DPO/LoRA │
│ observer,    │  │ Croissant +       │  │ + adapters +   │  │ +replay → checkpoint │
│ health, perf │  │ scorecard         │  │ hot-swap       │  │ (LOOP 1/2/3)         │
└──────────────┘  └───────────────────┘  └────────────────┘  └──────────────────────┘
```

### 7.2 End-to-end data-flow

```
 raw audio ─▶ decode/VAD ─▶ chunk ─▶ [denoise?] ─▶ segment
     │                                              │
     ▼                                              ▼
 SOURCE (never touched, blake3 id) ──────────► segment_hypotheses
   source_audio_identity() (pipeline.rs)        ├─ omniasr-ctc-300m/1b (sherpa-onnx)
                                                ├─ omniasr-wsl-7b (fairseq2/WSL)
                                                └─ whisper-ckb + xlsr-ckb (ensemble)
                                                        │
                                      ROVER fuse + agreement  ─┐
                                                        ▼      │
                                      IRT consensus ─▶ conformal cert
                                                        ▼      │
                                      T0 gate ─▶ T1 ─▶ T2 ─▶ Debate
                                                        ▼      │
                   ┌──── AutoAccept (silver) ──────────┤      │ disagreement
                   │                                    ▼      ▼
                   │                            ReviewInbox (priority_score)
                   │                                    │ human fix
                   ▼                                    ▼
              verdict ───────────────────► corrections ledger ──► agent_examples
                   │                                    │
                   ▼                          LOOP 0 (memory, instant)
              eval on gold (is_holdout=1)     LOOP 1 (GER) · LOOP 2 (KenLM/CTC bias)
                   │  scorecard (CI+MAPSSWE)   LOOP 3 (LoRA+replay → eval-gate)
                   ▼                                    │
              export bundle + Croissant       registry: candidate→challenger→
              (content-hashed, reproducible)            champion / rolled_back
```

### 7.3 Phased migration roadmap (each phase ends in a machine-checkable CI gate)

| Phase | What | Reuses (verified real) | Gate | Risk |
|---|---|---|---|---|
| **P0 Provenance** | Migrations **v20–v21**: stamp `model_version_id` on every hypothesis + verdict; `correction_memory` (v20) + `corrections` ledger (v21); back-fill history to `unknown@pre-registry` | `migrations/mod.rs` (transactional, ascending-guard `#[test] migration_versions_are_strictly_ascending_and_unique`), `source_audio_identity` | CI test: no new hypothesis/verdict row lacks `model_version_id` | **Blocker for everything** — attribution impossible without it |
| **P1 Registry + Import** | **v22/v23** `model_versions`/`adapters`; "Import fine-tuned 7B" (mandatory non-empty SHA + license + base-card resolution + asset-card writer; LoRA→merge-to-base bridge); read-only registry panel | `models.rs::verify_extracted_against_pin`, `test_model_provenance_policy.py` | importing a checkpoint with an **empty/unresolvable pin or unresolvable base card is rejected** | resolve real fairseq2 `base:` card; WSL path |
| **P2 Eval-gate wiring** | Wire `scorecard.rs` as the promotion gate (WER `beats_baseline` **AND** CER target) over registry versions on `is_holdout=1` | `eval.rs::run_gold_eval_with_transcriber`, `significance.rs::mapsswe` | a challenger that does not pass **both** metrics **cannot** promote in CI | tiny ckb gold set → wide CIs; honesty rules |
| **P3 Ensemble disagreement** | Persist per-engine agreement into `segment_hypotheses`; redefine escalation key as `priority_score`; persist IRT abilities (§4.2) | `sorani_ensemble_asr.py`, `quality/*`, `get_escalation_queue` | escalation-order test on a high-disagreement fixture | older ckb models drift → flag for refresh |
| **P4 Close the loop** | LOOP 0 first (memory, full §2.3 algorithm), then LOOP 1/2, then LOOP 3 (corrections→adapter→auto-eval→auto-challenger; human clicks Promote) | `jury/learning.rs`, gold holdout, scorecard, MAPSSWE | injected-error benchmark: CER reduction ≥30% at ≤15% escalation **and** LOOP-0 over-trigger under budget before auto-challenger | `ckb` corpus inclusion unconfirmed → keep honest stock scorecard |
| **P5 Module boundaries + observability** | Move cross-module calls behind the event bus + append-only events log; EngineSpec sidecars (§4.5); registry/flywheel dashboards | `PipelineEvent` enum, `agent_stage_events`, `telemetry/*` | module-isolation tests + a11y/egress gates stay green | refactor churn — stage **last** |

**Overarching invariant (charter-enforced):** never let an unverified fine-tune or a fabricated number through. The anti-hallucination + offline-egress + holdout-by-hash gates must gate *every* phase. P0–P3 are low-risk extensions; P4 is the genuinely new (and, for `ckb`, empirically unproven) closure; P5 is a refactor.

---

## 8. Robustness & Quality (Pro-App Patterns)

Cortex already has most of the Resolve-class substrate. The framing is **close the last mile**, not rebuild.

### 8.1 What is already built (verified by reading the code)

- **Atomic file writes** with fsync + Windows-safe rename + cleanup-on-error (`atomic_file.rs`, used by `models.rs::replace_file`).
- **Crash recovery:** session autosave (`session/mod.rs`) with corrupt-file quarantine; DB open runs `PRAGMA integrity_check` and **quarantines a corrupt DB** (`db.rs::open_with_retry`, the branch logging "quarantining database" → `recover_database_at`; test `open_with_retry_quarantines_db_when_integrity_check_fails_after_open`).
- **Undo/redo:** Command-pattern history with poisoned-lock recovery (`history/mod.rs`).
- **Transactional migrations:** all-or-nothing up/down, with the strictly-ascending **`#[test] migration_versions_are_strictly_ascending_and_unique`** in `migrations/mod.rs` (there is no `_assert_migration_order` function — the guard is that test) and `failed_migration_is_all_or_nothing`.
- **Telemetry:** RAII span ring buffer (`telemetry/mod.rs`).
- **Deterministic eval:** seeded bootstrap CI + MAPSSWE; `beats_baseline` true only when **lower (micro-WER) and significant** (`scorecard.rs`: `beats_baseline: significant && system_micro_wer < baseline_micro_wer`; `significance.rs`).
- **DB pragmas (verified in `Database::open`):** `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `cache_size=-64000`, `busy_timeout=10000`.
- **Provenance:** SHA-256-pinned `model_manifest` + `test_model_provenance_policy.py`; export bundles with blocking validation (`export_bundle.rs`); DPO export with holdout-hash exclusion (`jury/learning.rs`).

### 8.2 The gaps and the fixes

| Gap (verified absent) | Fix | Leverage |
|---|---|---|
| **No `std::panic::set_hook`** → a sync-pipeline panic = silent hard crash | install a panic hook that dumps the `telemetry` ring buffer + active job + schema version to `$APPDATA/crashes/{ts}.json` **before** the process dies; next-launch "Recover/Report" prompt; opt-in `sentry-tauri` scrubbing transcripts via `secret_redaction.rs` | **highest** — biggest current robustness gap |
| `job_history` is a **log, not a queue** | promote to a durable single-writer queue: `queued/running/failed/done` + `payload_json` + `retry_count`; on startup reset orphaned `running`→`queued`; **this is the queue the EngineSpec sidecar failure path (§4.5) re-queues into**; progress via Tauri `ipc::Channel` | crash-resumable ActAuto batches |
| No `wal_autocheckpoint` | add `PRAGMA wal_autocheckpoint=1000`. **Enforce single writer at the dispatcher** — WAL still has exactly one writer | avoids "database is locked" |
| No project container / snapshots | new `projects` + `dataset_snapshots`; auto-snapshot before any **bulk** write (batch re-transcribe, normalizer change, checkpoint re-labeling) | non-destructive, versioned |
| Export not standards-based | emit **Croissant** JSON-LD beside the bundle, pinning every input by content hash, embedding the scorecard WER/CER + CI | byte-reproducible, HF-droppable |
| Engines in-process | formalize engines as out-of-process **sidecars** behind the **EngineSpec JSON contract (§4.5)**; a crashing 7B fails the job (not the app) → `failed` + `retry_count++` | crash isolation + clean ingestion |

### 8.3 The eval-gate as a CI merge gate

The gate that makes regressions un-shippable: **no model/prompt/threshold change, no incoming fine-tuned checkpoint, and no correction-trained adapter merges unless held-out WER stays within the bootstrap CI (the existing `beats_baseline` rule), the CER-reduction target is met, and the conformal coverage target holds.** Add a trajectory metric: % of auto-accepted segments that later survive human spot-check. This is the safety net that lets the Autonomy Dial rise on **evidence**, not vibes.

---

## 9. Honest Constraints

### 9.1 Date sensitivity — what I could NOT verify

Today is 2026-06-20 but my training cutoff precedes much of 2026. **I cannot verify any release, version, benchmark, or model dated specifically in 2026, and several late-2025 facts are equally ungrounded in the tree.** Concretely, **every one of the following is `[unverified at cutoff; pin and re-verify at integration time]`** and must not be presented as settled in any downstream summary: the `omnilingual-asr` PyPI version/date; `jiwer` version; `asosoft` PyPI version/date; `faster-whisper`/CTranslate2 version and `large-v3-turbo`; MLflow version; Croissant version/date; DaVinci Resolve version/date; `sentry-tauri` version; the reported lakeFS/DVC corporate change; fairseq2 LoRA issue #310 state; Unsloth ASR adapter issue #2726; `torchtune` EOL status; the OmniASR ckb_Arab 7B "CER 6.0 @ 59.6h" figure; and the "~17 GiB BF16" / "~30 GiB consolidated.pt" sizes.

**What the tree actually proves:** the `omnilingual_asr.models.inference.pipeline.ASRInferencePipeline` import path, the local card string `omniASR_LLM_7B_v2`, and the sherpa-onnx CTC archive URLs dated **2025-11-12**. Nothing else about external versions is grounded — there is no dependency manifest pinning the Python tools.

**arXiv citations** are tagged inline as *read* or *unread-not-verified*. Several 2602.x–2606.x IDs were **not read** and are not cited as load-bearing. Crucially, **no algorithmic decision in this doc depends on an unread citation** — the WCTC/CTC word-boost (§2.5), phonetic-grounded GER (§2.4), and constrained decoding (§4.5) are presented as engineering **design choices**, not as cited results. The 2025 production-flywheel papers (Agent-in-the-Loop arXiv:2510.06674; MAPE arXiv:2510.27051 — *unread-not-verified*) are **LLM customer-support** systems: the loop *pattern* transfers; their numeric gains are domain-specific and must **not** be presented as ASR results.

### 9.2 Aspirational vs verified

| Verified-present in code | Aspirational (designed here, not yet built) |
|---|---|
| DPO export + holdout-hash exclusion | LOOP 0/1/2/3 closure; correction_memory; KenLM/CTC biasing |
| `run_gold_eval_with_transcriber`, scorecard (CI+MAPSSWE) | promotion gate wired over a registry; **WER-and-CER** reconciled gate; per-slice regression block |
| `model_manifest` SHA-pinning; provenance policy test | `model_versions`/`adapters` lineage; **mandatory non-empty pin on import**; base-card resolution; LoRA→merge-to-base bridge; hot-swap |
| 3-engine ensemble + ROVER + IRT + conformal + tiered jury | `priority_score` (composite QBC); cross-session IRT ability persistence |
| atomic writes, integrity-check quarantine, migrations, telemetry | panic hook/crash dump; durable job queue; EngineSpec sidecars; Croissant; snapshots |

The 7B's `ckb` accuracy gains are **aspirational until measured** — `ckb`'s training-corpus inclusion is *unconfirmed* (not proven-excluded), so the user's fine-tune must pass Cortex's own gold harness before any claim is made.

### 9.3 Security & privacy (local-first, share-alike gating)

- **Local-first / offline-egress:** scripts already set `HF_HUB_OFFLINE=1`; the charter enforces a runtime offline-egress gate (zero outbound sockets across the full T0→T1→T2→debate jury and the auto-updater). Keep crash reporting **local-only by default** (opt-in Sentry, transcript-scrubbed via `secret_redaction.rs`). MLflow self-hosted preserves the offline guarantee — do **not** default to hosted W&B.
- **Share-alike / NC data gating (per the actual `DATA_GOVERNANCE.md`):** **KLPT is CC-BY-SA — never link/derive.** **AsoSoft-600 is CC-BY-SA-4.0 — share-alike copyleft: any AsoSoft-derived export must also be CC-BY-SA-4.0, isolated and gated** (the file already carries this contamination warning). **CORDI is CC-BY-NC-SA-4.0 — non-commercial *and* share-alike: blocked from any commercial/redistributable training export.** **F5-TTS checkpoints are typically CC-BY-NC — NC propagates to derived TTS weights.** The contamination gate must enforce the *specific* obligation class per asset (copyleft vs non-commercial), not a blanket label.
- **Feedback-loop hygiene:** train only on **human-confirmed** corrections, run Cleanlab-style label-noise filtering first, keep the holdout-by-content-hash exclusion across all four loops, and gate every adapter on held-out WER/CER + per-dialect fairness before promotion — so the flywheel cannot silently degrade the model it is meant to improve.

---

### Appendix — verified file map (absolute paths, as found in the tree)

Repo root: `<repo-root>` (a local git worktree). Note the **two distinct `scripts/` roots** — they are not the same directory:

- `…\scripts\sorani_ensemble_asr.py` — **(repo-root scripts/)** 3-engine ensemble; `agreement()`, `rover_fuse()`; the live call `ASRInferencePipeline(model_card="omniASR_LLM_7B_v2")` and import `omnilingual_asr.models.inference.pipeline`
- `…\cortex-speech-app\scripts\test_model_provenance_policy.py` — **(app scripts/)** model-provenance policy gate
- `…\cortex-speech-app\src-tauri\src\jury\learning.rs` — DPO export + holdout-hash exclusion (`build_dpo_dataset`; `holdout_hashes.contains(&identity.content_hash)`)
- `…\cortex-speech-app\src-tauri\src\jury\mod.rs` — T0 gate (`let disagreement_score = 1.0 - irt_confidence;`); `get_few_shot_examples` (queries `agent_examples`, **ignores `_segment_id`**, recency-ordered); `get_escalation_queue` (`ORDER BY COALESCE(agent_confidence, 0.5) ASC`)
- `…\cortex-speech-app\src-tauri\src\quality\irt.rs` — 1PL IRT EM (`iterations = 50`; `update_abilities = segment_slots_map.len() >= 10`; session-local abilities)
- `…\cortex-speech-app\src-tauri\src\quality\conformal.rs` — `calibrate_and_certify(verified, 0.05, 0.90)`, fallback 0.35
- `…\cortex-speech-app\src-tauri\src\eval.rs` — `run_gold_eval_with_transcriber` (live engine)
- `…\cortex-speech-app\src-tauri\src\scorecard.rs` / `significance.rs` — bootstrap CI + MAPSSWE; `beats_baseline: significant && system_micro_wer < baseline_micro_wer` (**micro-WER gate**)
- `…\cortex-speech-app\src-tauri\src\models.rs` — SHA-256 pinned manifest; `verify_extracted_against_pin` (**empty pin → `Ok`**); OmniASR CTC int8 archive URLs dated **2025-11-12**
- `…\cortex-speech-app\src-tauri\src\migrations\mod.rs` — **schema head at v19**; ascending-guard `#[test] migration_versions_are_strictly_ascending_and_unique`; test-only `version: 99_999` fixture; `model_manifest`/`job_history`/`agent_examples` schemas
- `…\cortex-speech-app\src-tauri\src\db.rs` — `record_human_decision`; WAL/`busy_timeout` pragmas in `Database::open`; integrity-check quarantine in `open_with_retry`
- `…\cortex-speech-app\src-tauri\src\pipeline.rs` — `source_audio_identity` (`pub(crate) fn source_audio_identity`), `should_use_wsl_primary_asr`, `run_wsl_segment_transcript`, `PipelineEvent` bus
- `…\cortex-speech-app\src-tauri\src\normalizer.rs` / `normalizer\g2p.rs` / `diff\phonetic.rs` — `normalize()` (NFC + Yeh + ZWNJ→space); `g2p()`; `normalized_phonetic_word_distance()` — the building blocks of the LOOP-0 slot/phonetic keys
- `…\DATA_GOVERNANCE.md` — **(repo root)** AsoSoft-600 = **CC-BY-SA-4.0** (share-alike warning present); CORDI = **CC-BY-NC-SA-4.0**; CV `ckb` = CC0-1.0; FLEURS `ckb_iq` = CC-BY-4.0
- `…\AGENT_CHARTER.md` — **(repo root)** CER as primary north star ("≥30% CER reduction at ≤15% escalation"); anti-hallucination + offline-egress mandates
- `…\cortex-speech-app\src-tauri\src\atomic_file.rs`, `session\mod.rs`, `history\mod.rs`, `telemetry\mod.rs` — robustness substrate
