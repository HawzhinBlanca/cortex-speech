# The Verbatim Dataset Pipeline — final blueprint (2026-08-12)

**Provenance.** Produced by an 11-agent research workflow: 5 researchers with live web access over
2024–2026 literature and shipped-corpus practice, 1 read-only inventory of this codebase, 3
independent pipeline designs (precision-biased, throughput-biased, robustness-biased), 2 adversarial
judges. Both judges independently selected the precision design ("Champion-Draft, Double-Confirm",
8.8/10) and independently identified the same residual weakness (draft-anchoring), whose fix is
incorporated below along with the judges' grafts from the other designs. Full agent outputs: session
workflow `wf_49bed89e-d5a`.

**The owner's question this answers:** should reviewers see champion-only drafts, or a machine-arbitrated
"best of champion + Gemini"? **Answer: champion-only drafts, Gemini caged as judge/evidence — and the
research is unambiguous about why:**

- LLMs used as ASR text correctors hallucinate at measured 3–12% rates and "over-correct" correct text
  (arXiv 2505.24347, 2405.15216) — the literature's mitigation is exactly what this repo already ruled:
  staged detect→flag→verify, never free generation into the draft.
- Machine captions systematically **delete** spoken content; models trained on human-verified subsets
  significantly beat machine-caption subsets (YODAS, arXiv 2406.00899). Deletion is the verbatim
  dataset's mortal enemy.
- Transcription policy (verbatim vs cleaned) is a controlled variable: mixing them untagged makes up to
  60% of reported WER style-mismatch noise (arXiv 2607.18934). Verbatim-only, policy-tagged, is the
  scarce and valuable resource (CrisperWhisper, arXiv 2408.16589).
- Where the SOTA teams use humans at all, the pattern is **consensus-then-verify**: multiple engines'
  *agreement* routes human attention; machines never author the label (Typhoon ASR 2601.13044,
  Interspeech 2025 SpeechLLM fusion).
- "Gold" operationally = **two independent human passes + third-pass adjudication**, with
  inter-reviewer CER published as the human floor (Novotney & Callison-Burch line; Common Voice's
  two-concurring-votes gate).

## The pipeline

**One law above all: machines route and flag; humans write; every trusted component is measured before
it is trusted.**

### Stage 0 — Conventions + reviewer calibration *(new, prerequisite)*
A versioned Sorani verbatim style guide (CORAAL-style): fillers, repetitions, false starts,
code-switching/loanwords AS SPOKEN, numbers as spoken, heh/ye variants, ZWNJ. Every reviewer passes a
~20-clip calibration batch against owner-adjudicated keys before their decisions mint anything.
*Gate: calibration CER threshold; recorded per reviewer.*

### Stage 1 — Ingest + identity + coverage intake *(exists; + required tags)*
Existing import with content-hash fingerprint dedupe and the frozen-eval contamination guard (FLEURS/
CV22/CORDI hashes may never enter training). **Graft (both judges): dialect / speaker / domain tags
become REQUIRED intake fields** — coverage steering and disparity numbers need real data, not a
hoped-for column. *Gate: import report fails on missing tags; contamination check on every import.*

### Stage 2 — Segmentation *(exists)*
Silero VAD per-utterance clips (the universal training unit), denoiser, CAM++ speaker labels,
speaker-change scores, honest backend labels. *Gate: alignment/backend provenance recorded per clip.*

### Stage 3 — Champion verbatim draft *(exists; the ONLY draft)*
OmniASR-7B champion transcribes every clip; hard-stop on any failure; raw goes to `raw_transcript`
only. **Graft: a per-batch engine-attribution report** proving every clip was champion-drafted — a
standing detector for the silent-engine-fallback incident class. *Gate: attribution report +
review-serving-provenance on the live DB.*

### Stage 4 — Secondary hypotheses *(exists; evidence, never fused)*
CTC-300M, CTC-1B, MMS-1B per clip into `segment_hypotheses`. Never served, never exported.

### Stage 5 — Auto-triage + routing *(build: signals exist, score does not)*
Per-clip routing score from: champion-vs-secondary CER after comparison-only folding; repeated-n-gram/
hallucination detector; chars-per-second outliers (**IQR bounds measured on THIS corpus, not
borrowed** — judge graft); aligner score. Precedents: People's Speech CER<50% keep gate, YODAS
align-score drop, Emilia IQR filters. **Routing only — no rewrite path exists.** High-agreement clips
→ fast lane; disagreement/outliers → priority lane. *Gate: score distribution reported per batch.*

### Stage 6 — Jury, Gemini caged *(exists; + measure Gemini first)*
T0 (IRT consensus, fail-closed) → T1 (tool judge) → T2 (**Gemini 2.5 Pro as AUDIO judge on escalated
clips only** — flags and scores, never writes served text). Whole-file context pass (the unused
`source_transcripts` chain) for long recordings — answering the owner's rate-limit/context point: one
whole-file call per recording, stored as evidence. **Measure-first:** Gemini-as-ASR CER on the frozen
gold set before its opinion outranks anything; its paraphrase-guard bound derived from the 492
already-measured rewrite clips (the empirical distribution of exactly this failure is on disk).

### Stage 7 — Human pass 1 *(exists; + disagreement UI)*
Phone page serves the champion-verbatim draft. Audio-first. **Build: per-word disagreement highlight +
engine-provenance badge** so the reviewer's ear focuses exactly where engines diverge (counters
rubber-stamping/automation bias). Hidden spot checks stay at 10–15% density, randomly interleaved.

### Stage 8 — Human pass 2: blind double-confirm *(build; the GOLD bar)*
Every GOLD-tier clip gets a second, different reviewer, blind to pass 1, same champion seed;
dialect-stratified. **Judges' fix for draft-anchoring: a ~5% draft-FREE audit slice** — clips
transcribed from audio alone, no seeded text — directly measuring how much the champion draft anchors
both passes (systematic champion deletions could otherwise survive two confirmations).

### Stage 9 — Adjudication *(build)*
Pass1-vs-pass2 discrepancies go to a third pass (owner or top spot-check-ranked reviewer).
Inter-reviewer CER is published as **the dataset's human floor** — the honest ceiling for any model
trained on it.

### Stage 10 — Grade + export *(exists; tiers realigned)*
GOLD = two blind matching passes OR adjudicated final. SILVER = one human decision (+ machine
agreement evidence). Verbatim policy tag on every row. Rejects/placeholders/holdout excluded at the
shared root (already law). *Gates: gold-provenance + verbatim-training-text policies (live).*

### Stage 11 — Measurement loop *(exists partly; enforce)*
Every export reports three numbers, never one: frozen FLEURS CER (read speech), internal spontaneous
holdout CER, dialect disparity. Kappa gate **enforced** at ≥0.75 before scaling (currently pending
2-reviewer overlap). Reviewer quarantine on >30% spot-check failure, with immediate re-queue of their
undecided leases (judge graft).

## Measure before trusting — the standing list
1. Gemini-as-ASR CER on frozen gold (before any draft-adjacent role).
2. Draft-anchoring bias via the draft-free slice (before calling double-confirm "independent").
3. Corpus-native IQR bounds for every auto-filter (before any filter drops a clip).
4. Inter-reviewer CER floor (before publishing any model comparison against the dataset).

## Build deltas from today (in order)
1. Conventions doc + calibration batch (stage 0) — unblocks everything human.
2. Gemini-as-ASR gold benchmark + whole-file evidence pass (stage 6) — cloud, no reviewer impact.
3. Routing score + queue ordering (stage 5).
4. Couch disagreement highlight + provenance badge (stage 7).
5. Double-pass serving + adjudication flow + kappa enforcement (stages 8–9) — the GOLD bar.
6. Dialect intake requirement + coverage steering (stages 1, 11).
