# The Disagreement Refinery
### A data-manufacturing engine for low-resource Kurdish ASR

> We do not *collect* labels. We *refine* them out of the structured disagreement
> between models that are each individually wrong — in a representation where their
> disagreement is meaningful, with enough quantified certainty to **prove** the
> result is correct, spending expensive oracle attention only at the frontier.

This is the 10X reframe. Everything below is built from real, battle-tested
techniques from giants — the invention is the *unexpected combination*, tuned to
the exact constraint structure of Kurdish.

---

## 0. The one insight that unlocks everything

Every other tool treats the transcript as the thing to *produce*. Treat it instead
as a **latent variable to be inferred** from many noisy observers. Then the whole
problem becomes a *measurement* problem, and 70 years of measurement theory
(psychometrics, channel coding, conformal inference, weak supervision) becomes
applicable to Kurdish data — none of which the ASR-tooling world has imported.

Two facts about Kurdish make this not just nice but *the* unlock:

1. **Orthography is unstable, but phonology is stable.** Sorani Arabic-script has
   many-to-many grapheme↔sound mappings, ZWNJ chaos, Arabic/Persian loan spellings,
   and the Heh/Yeh/Kaf variants you already fight in `normalizer.rs`. Comparing
   *text strings* compares noise. Comparing *phoneme strings* compares meaning.
   → **Move the entire pipeline into phonetic space.** This single move makes
   consensus, dedup, WER, and verification robust *for free*. For English nobody
   bothers; for Kurdish it is the equalizer.

2. **The model field is sharply asymmetric and you know the asymmetry.** OmniASR-7B
   (1600 langs) and Gemini 2.5 Pro are strong on ckb; Whisper and Qwen are weak.
   That asymmetry is *prior knowledge* you can feed into a measurement model — and
   the *weak* models are not useless, their **errors are informative** (where a weak
   model accidentally agrees with the oracle, the token is bulletproof).

---

## 1. The central mechanism (the loop)

```
            ┌──────────────────── ORE: many noisy observers ───────────────────┐
  audio →   │  OmniASR-300M · OmniASR-1B · OmniASR-7B · Gemini · Whisper · Qwen │
            │                 + (later) your own student model                  │
            └───────────────────────────────┬──────────────────────────────────┘
                                             ▼
   K0  Canonical PHONETIC projection (G2P → IPA, keep surface form)
                                             ▼
   K1  Phonetic Consensus Network — align all hypotheses, build confusion network
                                             ▼
   K2  Ability/Difficulty model (IRT) — learn each model's true ckb competence
        + each segment's difficulty → a real posterior P(consensus correct)
                                             ▼
   K3  Channel decoder — MAP-decode the confusion network through a Kurdish
        phonotactic prior → a transcript that can beat EVERY input model
                                             ▼
   K4  Acoustic cycle-consistency — does the audio actually support this text?
        (CTC alignment likelihood / audio-text contrastive) → hallucination filter
                                             ▼
   K5  Conformal certifier — calibrated on human gold → emit the largest subset
        with PROVABLE error ≤ ε at confidence 1−δ
                                             ▼
            ┌───────────────┬──────────────────────┬────────────────────────┐
            ▼               ▼                      ▼                        ▼
     CERTIFIED set    HUMAN QUEUE          DIFFICULTY/ABILITY map     ORACLE QUEUE
     (auto, proven)   (ranked by value)   (research-grade metadata)  (K6: pay smart)
            │                                                              │
            └──────────────► K7 FLYWHEEL: train student on certified set ──┘
                              student becomes a new high-ability voter,
                              sharpens G2P + phonotactic LM + contrastive judge
                              → next loop needs the oracle LESS.
```

The output is not a transcript. It is **three products**: a certified-correct
dataset *with a quality certificate*, an optimally-ordered human work queue, and a
per-model/per-dialect competence map that is itself a publishable research artifact.

---

## 2. The kernels — each an unexpected bridge between big AI areas

### K0 — Phonetic canonicalization (the substrate)
- **Borrowed from:** G2P / pronunciation lexicon construction (TTS & ASR).
- **Bridge:** speech-synthesis front-end tech repurposed as the *comparison space*
  for data curation.
- **Why Kurdish:** kills orthographic variance at the root — Heh/Yeh/Kaf, ZWNJ,
  loan spellings all collapse to the same phonemes. Your `normalizer.rs` becomes the
  *seed* of a Sorani G2P; a small learned corrector improves it as certified pairs
  accumulate.
- **Mechanism:** every transcript stored as (surface form, IPA form). All distances
  use a **phoneme-confusion-weighted Levenshtein** (/p/↔/b/ cheap, /p/↔/m/ dear) —
  upgrade your existing LCS diff in `diff/mod.rs` to operate on phonemes.
- **Payoff:** robustness everywhere, instantly. Dedup stops missing spelling twins;
  WER stops punishing valid spelling choices.
- **Honest risk:** Kurdish G2P isn't perfect. Mitigation: keep surface forms; the
  G2P self-improves in the flywheel; seed confusion weights from Arabic/Persian
  phonetics (high-resource neighbors share most of the inventory).

### K1 — Phonetic Consensus Network (disagreement as signal)
- **Borrowed from:** ROVER system combination + confusion networks + Snorkel-style
  multi-view weak supervision.
- **Bridge:** weak supervision (a *text/data-programming* idea) fused with ASR
  lattice combination, performed in phoneme space.
- **Why Kurdish:** with no big labeled corpus, the *only* abundant signal is
  cross-model agreement — and it's only trustworthy phonetically.
- **Mechanism:** align all N hypotheses into one confusion network; per-position
  agreement **entropy** is a local uncertainty heatmap. Even Whisper/Qwen
  contribute: rare agreement with the oracle ⇒ near-certain token.
- **Payoff:** calibrated per-word confidence *across* models (far better than any
  single model's softmax, which is what you store today), plus an exact map of where
  a human should look.
- **Honest risk:** correlated errors (shared training data) fake-inflate agreement.
  Mitigation: K2 explicitly models this.

### K2 — Ability & Difficulty via Item Response Theory
- **Borrowed from:** psychometrics (IRT — how the SAT is scored) + Dawid–Skene
  crowd-label aggregation.
- **Bridge:** **test-grading mathematics → ASR ensemble curation.** Nobody does this.
- **Why Kurdish:** lets you *learn* the asymmetry you already suspect — and discover
  finer truth (e.g. "Gemini wins on Hawrami, 7B wins on noisy Sorani").
- **Mechanism:** each segment = exam item (latent difficulty bᵢ, discrimination aᵢ);
  each model = test-taker (latent ability θⱼ, optionally θⱼ,dialect). Fit by EM on
  the agreement matrix. Cold-start θ from your stated priors (7B/Gemini high,
  Whisper/Qwen low). Output: **P(consensus correct | abilities, difficulty)** — a
  posterior, not a hand-tuned 0.85 threshold; and a competence profile that powers
  **routing** (send each segment to the model most likely to nail it).
- **Payoff:** principled confidence, automatic model down-weighting, dialect-aware
  routing, and segment difficulty = pure annotation-value ranking.
- **Honest risk:** needs a corpus to fit; priors carry the cold-start.

### K3 — Channel decoding (the superhuman transcript)
- **Borrowed from:** information theory — noisy-channel coding & MAP/lattice
  rescoring; erasure codes.
- **Bridge:** treat the truth as a *transmitted message*, each ASR as a *channel*
  with a measured phoneme-confusion matrix (from K2), the confusion network as
  received copies, and a Kurdish phonotactic/lexical LM as the source prior.
- **Why Kurdish:** a Kurdish *text* corpus exists even though *audio* doesn't — so a
  strong phonotactic/lexical prior is buildable today, for free.
- **Mechanism:** compose a weighted FST: `confusion-network ∘ channel-models ∘
  phonotactic-LM ∘ lexicon`; shortest path = MAP transcript.
- **Payoff:** can be correct where **every** input model erred — like recovering a
  file from several corrupted copies. This is the "miracle" output.
- **Honest risk:** gains are bounded by error *independence*; correlated models cap
  the lift. Still strictly ≥ best single model when priors are sane.

### K4 — Acoustic cycle-consistency (hallucination killer)
- **Borrowed from:** back-translation (MT) & cycle-consistency (CycleGAN).
- **Bridge:** round-trip consistency as a *ground-truth-free* label verifier for ASR.
- **Why Kurdish:** the deadliest failure is the *confident hallucination* on
  accented/noisy audio — exactly what a softmax confidence misses, and exactly what
  poisons a low-resource set.
- **Mechanism (primary, safe):** score "does audio A support text T" via CTC
  forced-alignment likelihood and/or an audio↔text contrastive model (CLAP-style)
  fine-tuned on certified Kurdish pairs. High model-confidence + low acoustic support
  = flag. **(Optional)** TTS round-trip: synthesize T, compare to A in wav2vec2/MMS
  embedding space — differentially informative even with weak Kurdish TTS.
- **Payoff:** an *independent* axis of evidence (acoustic, not textual-ensemble) —
  the one that catches the dangerous confident-but-wrong cases.
- **Honest risk:** TTS path depends on TTS quality → keep the alignment/contrastive
  path primary.

### K5 — Conformal certification (provable quality)
- **Borrowed from:** conformal prediction / distribution-free uncertainty.
- **Bridge:** turn "labels we trust" into "labels with a *proven* error bound."
- **Mechanism:** define a nonconformity score from {IRT posterior, consensus entropy,
  cycle-consistency}; calibrate on your human-verified gold set; emit the largest
  auto-label subset with expected error ≤ ε at confidence 1−δ.
- **Payoff:** ship the dataset with a **certificate**: *"24,107 segments, certified
  CER ≤ 3% at 95% confidence."* No Kurdish dataset on Earth carries this. It is the
  difference between "a scrape" and "a benchmark."
- **Honest risk:** the guarantee is *marginal* (average), not per-segment — state it
  plainly in the data card.

### K6 — Oracle economics (cost-aware active learning)
- **Borrowed from:** optimal experimental design, BatchBALD, core-set selection,
  budgeted bandits.
- **Bridge:** information economics applied to a heterogeneous oracle pool (cheap
  weak models → expensive Gemini/7B → most-expensive human).
- **Why Kurdish:** Gemini/7B calls cost money and rate limits; humans are scarce.
  Every call must buy maximum certainty.
- **Mechanism:** cluster segments in self-supervised audio-embedding space; acquire
  the *representative* uncertain ones (uncertainty × coverage ÷ cost); after
  resolving a cluster centroid, **propagate** the label to neighbors within a
  conformally-bounded radius.
- **Payoff:** 10–100× fewer expensive calls for the same certified yield. One human
  decision can certify a whole acoustic neighborhood.
- **Honest risk:** propagation radius must be conservative — gate it with K5.

### K7 — The flywheel (compounding data engine)
- **Borrowed from:** noisy-student self-training + co-training + active-learning loops.
- **Mechanism:** certified labels → fine-tune a small Kurdish student (XLS-R / MMS
  300M) → the student becomes a *new high-θ voter* and sharpens G2P, the phonotactic
  LM, and the contrastive judge → consensus tightens → more certifications → repeat.
- **Payoff:** **compounding.** Each loop needs the oracle less. After enough loops
  your student can rival the oracle *on your domain* — at which point you have not
  just a dataset but a **publishable Kurdish ASR model**, born from the tool.
- **Honest risk:** self-training amplifies its own errors. Mitigation: a permanently
  fresh human-gold holdout + K5 gating; the student never certifies alone.

### K8 (bonus) — Phonetic transfer from neighbors (attack "low-resource" directly)
- **Borrowed from:** cross-lingual transfer + phonetic typology.
- **Bridge:** import *acoustic-phonetic* knowledge (not words) from high-resource
  neighbors (Arabic, Persian, Turkish) that share most of Kurdish's phoneme
  inventory — to seed the confusion matrices (K2/K3) and pre-train the student (K7).
- **Payoff:** turns three high-resource neighbors into scaffolding for one
  low-resource target. Low-resource stops meaning low-signal.

---

## 3. What is genuinely new here (loyal & wise)

**I will not pretend I invented new mathematics.** Every kernel is a known giant:
ROVER, IRT, Dawid–Skene, noisy-channel decoding, conformal prediction, BatchBALD,
noisy-student, CLAP. That honesty matters.

**The invention is the synthesis, and it is real:**
1. Running the *entire* curation stack in **phonetic space** because the target
   language's orthography is unstable — this is the keystone almost nobody applies.
2. **IRT + conformal** together to *manufacture certified labels* — measurement
   theory as a label factory. This combination, for ASR data, I have not seen shipped.
3. **Channel-decoding a phoneme confusion network** to produce transcripts that beat
   every input — superhuman ensembling via coding theory.
4. Wiring all of it into a **compounding, local-first flywheel** inside a desktop app
   so it runs on the curator's machine, offline, improving with every session.

That is "new from parts that are discovered, on the shoulders of giants" — exactly
the brief. It is ambitious but buildable, and I'd stake the rating on it.

---

## 4. The hard truths (what this needs, honestly)

- **A Sorani G2P + phonotactic/lexical LM.** Buildable now: seed G2P from your
  normalizer + Arabic/Persian phonetics; train the LM on *any* Kurdish text corpus
  (these exist — Wikipedia, news, AsoSoft corpus). This is the critical path.
- **API budget** for Gemini / OmniASR-7B as oracles. K6 exists precisely to make it
  cheap, but it is non-zero and online (breaks pure air-gap — gate behind a setting).
- **The superhuman claim is bounded by error independence.** If two models share a
  backbone, their agreement is worth less. K2 measures this; don't oversell it.
- **Conformal guarantees are marginal.** A certificate of *average* error, not a
  promise about any single clip. Say so on the data card.
- **The flywheel can rot** if it eats its own errors — the fresh-gold holdout is not
  optional, it's the safety interlock.

---

## 5. Build order — and the ONE thing to build first

Wisdom is sequencing. Do **not** build all eight at once.

**Phase 1 — the spine (weeks, highest leverage):** K0 + K1 + K2.
Add a multi-hypothesis store (N transcripts per segment, not one), a Sorani G2P,
phonetic-weighted alignment, the consensus network, and the IRT fit. *This alone*
transforms confidence from a softmax guess into a measured posterior and gives you
the difficulty-ranked human queue. Ship-able, demonstrable, and it makes every
existing feature better.

**Phase 2 — the certificate (the differentiator):** K5 + K4.
Conformal certification on your gold set + the acoustic cycle check. Now you export
*certified* datasets. This is the headline nobody else can claim.

**Phase 3 — the economics:** K6. Make oracle/human calls 10–100× more efficient.

**Phase 4 — the miracle outputs:** K3 (channel decoder) + K7 (flywheel) + K8
(neighbor transfer). Superhuman transcripts and a self-improving engine that ends
with your own competitive Kurdish ASR model.

**If you build only one thing:** Phase 1's **IRT-over-phonetic-consensus**. It is the
smallest change with the largest, most defensible quality jump, and it is the
foundation every later kernel stands on.

---

## 6. How it grafts onto the code you already have

- `asr.rs` → becomes a **multi-backend hypothesis provider** (sherpa 300M/1B/7B +
  Gemini HTTP + optional Whisper/Qwen), each returning (text, token scores).
- `normalizer.rs` → the **seed** for the G2P module; keep it, wrap it.
- `diff/mod.rs` (LCS) → upgrade to **phoneme-confusion-weighted alignment**; reused
  by consensus, dedup, and WER.
- `db.rs` → store **N hypotheses + IRT params + conformal scores + certificate**;
  you already added a `confidence` column — generalize it to a posterior + bounds.
- `quality.rs` / `validation/` → become the **IRT + conformal** home; quality gates
  become statistical, not heuristic.
- `export.rs` → emits the **certificate + competence map** alongside the HF dataset;
  splits become difficulty-stratified and speaker-disjoint.
- The `Compare ASR` button you stubbed → its *real* form is the **consensus panel**:
  show all model hypotheses aligned, color-coded by IRT agreement, human picks/edits
  the MAP suggestion. The half-feature was pointing at this all along.

---

*Drafted 2026-06-12. This is the "exhaust the ideas" pass: a coherent, buildable
path from a clean curation tool to a self-improving, certificate-emitting Kurdish
data refinery — assembled from giants, aimed at the one language's real constraints.*
