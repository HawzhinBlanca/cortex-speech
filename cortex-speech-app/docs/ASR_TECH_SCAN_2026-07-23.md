# Central Kurdish ASR — Tech Scan, 2026-07-23

> [!WARNING]
> **Historical research snapshot, not a production runbook.** Later owner canon removed Scribe and
> pinned all production drafting to the OmniASR-7B WSL champion. References below to approved cloud
> ASR, stock-model fallbacks, or model swaps are superseded; retain them only as dated research
> provenance. See [`../../docs/OWNER_CANON.md`](../../docs/OWNER_CANON.md).

A brutally-honest, evidence-first scan of the *actual* mid-2026 state of Central Kurdish (Sorani, `ckb`)
ASR, run to ground the Accuracy & Usefulness Loop ([ACCURACY_USEFULNESS_LOOP.md](ACCURACY_USEFULNESS_LOOP.md)).
Five parallel researchers did real web searches + source fetches on 2026-07-23. **Every number here is
someone else's claim with its source; nothing is a Cortex measurement.** Refresh this doc on the loop's
research cadence and date the new file.

> **One-line headline:** As of 2026-07-23, the app's champion (fine-tuned OmniASR-7B, **7.03% micro-CER
> on FLEURS-ckb read speech**) appears to be **at or above the verifiable Sorani SOTA** — no 2026 release
> credibly beats it on FLEURS-ckb. The real levers are not a new model; they are **data, an LM, a
> literature-standard normalizer, and the review marathon** — almost all owner-gated to *run*.

---

## 1. Is there a better model? (facets 1–2)

**No new base model adds Kurdish beyond what you already run.**

- The newest `ckb`-capable open base anywhere is **Meta Omnilingual ASR (OmniASR)** — your own family
  (paper arXiv 2511.09690, Nov 2025; GitHub v0.2.0 released **2025-12-22**; 1,672 languages incl. `ckb`;
  CTC 300M/1B/3B/7B + LLM variants). Official sherpa-onnx CTC int8 exports exist for **300M and 1B**
  (`csukuangfj/sherpa-onnx-omnilingual-asr-1600-languages-{300M,1B}-ctc-int8-2025-11-12`). LLM-decoder
  variants have **no ONNX export**.
- Everything *newer in time* is Kurdish-negative or banned: **Qwen3-ASR** (0.6B/1.7B, Jan 2026) — 52
  langs, no `ckb`, and **BANNED** for us; **Voxtral Transcribe 2** (Feb 2026) — no `ckb` evidence;
  **IBM Granite Speech 4.1** — no `ckb`; **NVIDIA Canary/Parakeet** — 25 EU langs only; **Whisper** —
  no v4, and large-v3 is **~99–140% CER on `ckb`** (unusable); **SeamlessM4T** — still v2, `ckb` weak
  (15.7% CER, see §4).
- The only genuinely-2026 Kurdish ASR paper is **FLEURS-Kobani** (arXiv 2603.29892, 31 Mar 2026) — but
  it's **Kurmanji (KMR), not Sorani**. Value = the reusable **two-stage Common-Voice→FLEURS fine-tune
  recipe** (Whisper-v3, WER 28.11 / CER 9.84 on their KMR set), not a Sorani model.
- Cloud (jury/reference only, per policy): **ElevenLabs Scribe v2** and **Gemini 2.5 Pro** are the only
  approved `ckb` cloud engines. Scribe's page **self-contradicts** on `ckb` (intro "3.1% WER" vs its own
  table "32.1% WER v1") — treat the 3.1% as **unverified**. Gemini `ckb` support is unofficial/weak.

**So the model question is settled for now: keep the OmniASR family. The two offline model levers are a
config-swap and a self-trained checkpoint, not a new architecture.**

## 2. Offline accuracy levers that fit sherpa-onnx / Windows (facet 4, repo-grounded)

The app's **offline-fallback** path runs `sherpa-onnx = "1.13.2"` (Rust) + OmniASR-CTC-300M
`model.int8.onnx` (the DEFAULT engine is the WSL-served 7B champion, `settings.rs` asr_model_size=WSL7B;
CTC-300M is only the user-chosen fallback), **already GPU** (DirectML on Windows, CUDA elsewhere, CPU
fallback — `asr.rs:27-60`), **already int8**. So "add GPU/int8" is done. The real, narrower findings:

- **Hard ceiling — no CTC hotwords.** sherpa-onnx supports hotwords/context-biasing for **transducer
  only, not CTC**, and no 2026 release changed that. For a Kurdish names/places app on a CTC model this
  is *the* domain-term constraint. The only CTC-legal substitute is sherpa's **FST homophone-replacer**
  (post-decode text swap) — but the changelog wires it into the **Qwen3ASR** impl; **verify it's exposed
  on the Omnilingual offline path in 1.13.4 before building anything on it.**
- **Biggest zero-tooling lever: swap to OmniASR-CTC-1B int8** (already exported) and **measure** the
  `ckb` delta vs 300M. Pure config change; the 2×3090 Ti makes the higher RTF irrelevant. Gain is
  **unmeasured — do not assume it, measure it.**
- **int8 quantization script gap** (real): the fine-tuned `ckb` checkpoint is re-exported to int8
  out-of-band. Standard fix ≈ 15 lines: `onnxruntime.quantization.quantize_dynamic(weight_type=QInt8)`
  **excluding the CTC output head** (quantizing it hurts WER), plus one WER assert vs fp32. int8 dynamic
  ≈ fp32 WER in the literature.
- **Confidence stays heuristic.** sherpa 1.13.4 still emits empty `ys_log_probs` for CTC → no real
  per-token posteriors. The app's `Some(0.90)` is an **honest label** (`ConfidenceSource::Heuristic`),
  not a bug — but it means calibration/autonomy can't trust it until posteriors exist.
- **Not recommended:** CUDA-EP switch (not GPU-bound; DirectML is the right zero-dep default), int4/AWQ
  (LLM-oriented, accuracy risk, no need), faster-whisper (second runtime, weak `ckb`, no win).

## 3. Low-resource techniques for a data-starved pipeline (facet 3)

You have ~3 verified in-app segments and <5 h verified audio; the marathon is **3/500**. Techniques that
squeeze accuracy from little data matter most. All numbers are comparable-language evidence, **not Sorani
guarantees** — and all degrade on conversational speech.

1. **Iterative pseudo-labeling / noisy-student self-training** (teacher = champion, on the 1.74M
   unverified clips): the mechanism behind Whisper-v3's 10–20% low-resource gain; studies report 4–36%
   *relative* WER improvement. **Zero new human labels.** Risk: it **bakes in the teacher's conversational
   blind spots** ("confident wrong") — every round MUST be gated on a held-out human slice, or it's
   fabrication. (arXiv 2501.14788, 2408.05554.)
2. **Confidence-filtered pseudo-labels** (avg token log-prob / entropy / ensemble agreement) prevent
   error amplification — but at the *very bottom* (tens of minutes) unfiltered can win (quantity
   dominates). Threshold is empirical; leave the knob. (uDistil-Whisper 2407.01257.)
3. **KenLM n-gram shallow fusion** — ~~cheapest win~~ **VERIFIED NOT FEASIBLE on our path (iter 144).**
   sherpa-onnx 1.13.2's `OfflineLMConfig` is `{ model, scale }` where `model` is a path to a NEURAL RNN-LM
   **ONNX** file (offline_asr.rs:200-204) — a KenLM is an ARPA/binary n-gram, not ONNX, so it **mechanically
   cannot be wired into sherpa's offline LM at all**; and sherpa's offline LM + hotwords are transducer /
   beam-search features that don't apply to the OmniASR **CTC** greedy path. A real KenLM fusion would need
   a CUSTOM CTC prefix-beam-search + KenLM decoder in the app's own `ort` path (constrained_decode.rs) —
   a large, opt-in-only build, NOT a cheap win. The only offline **CTC-applicable** decode knob that exists
   is `OfflineRecognizerConfig.blank_penalty` (default 0.0, app unset) — biasing against blank cuts
   deletions — but its value must be **tuned on the gold set** (owner-gated; setting it blind is an
   unmeasured change → honesty violation). (NGPU-LM 2505.22857 is NeMo-side, not sherpa.)
4. **LoRA / QLoRA** — matches full fine-tune at ~5% of params (LoRA-Whisper: 18–23% rel. WER); the win is
   **cheap, repeatable self-training rounds** on 2×3090 Ti, not a standalone CER drop. **Don't chase the
   exotic-variant zoo** (AdaLoRA/DoRA/VeRA) — noise at this scale.
5. **Active-learning queue ranking** (uncertainty × diversity): makes each of the 497 remaining marathon
   decisions worth **~2–6×** a random one; rides free on #1's confidence scores. (arXiv 2406.02566.)
6. **TTS-augmentation** (14.3% abs WER on comparable langs) — **chicken-and-egg**: needs a good Sorani
   TTS that doesn't yet obviously exist. Honorable mention only.

## 4. Data + evaluation — the real bottleneck (facet 5)

- **Legally-clean, provenance-verified `ckb` acoustic data you can actually train on:**
  - **Common Voice ckb 22.0** (2025-06-25, **CC0**, ~120 h collective / smaller validated; read speech).
  - **SoraniTTS** (Mendeley jmtn248cc9 v5, 2025-09-16, **~19 h, single male, studio, CC BY 4.0**) — use
    for augmentation, not sole set.
  - Everything else you hold (the 1.74M corpus, `PawanKrd/asr-ckb-v2` 216k gated clips) is
    **eval-only / unverified provenance / undocumented license** — never in a gold or train-legal path.
  - **Trap:** Kuvost/KUTED (1,003 h "Central Kurdish", arXiv 2604.00613) is **English audio + Kurdish
    text** (speech *translation*) — NOT `ckb` acoustic data. Its value is the orthography pipeline, below.
- **Normalization is honesty-critical.** Leading `ckb` papers use the **AsoSoft / ScriptNormalization**
  pipeline (`github.com/sinaahmadi/ScriptNormalization`): unify **Kaf U+0643→U+06A9**, **Yeh
  U+064A→U+06CC**, canonicalize **ZWNJ U+200C**, unify digits. In KUTED, normalization touched **~10% of
  Kurdish tokens** — i.e. the normalizer can *manufacture* a low CER. **SN-WER** (arXiv 2606.02548, Jun
  2026) formalizes this for non-Latin scripts. **The champion's 7.03% must be reproduced under a
  documented, defensible normalizer + a contamination-checked FLEURS split before it is quoted
  externally.**
- **FLEURS-ckb sanity check** (different denominators — do not compare cell-for-cell):

  | System | FLEURS-ckb | Source |
  |---|---|---|
  | **App champion (internal claim)** | **7.03% CER** | internal (unreproduced externally) |
  | SeamlessM4T-v2 Large | 15.7% CER | Fleurs-SLU, arXiv 2501.06117v3 (176-sample subset) |
  | Whisper-v3 Large | 140.7% CER (broken for ckb) | same |
  | ElevenLabs Scribe v1 | 32.1% WER (self-contradicted) | elevenlabs.io page |

  7.03% CER beating Seamless (15.7%) by ~2.2× is **plausible for a ckb-fine-tuned model** — *if* (1) no
  FLEURS train/test contamination, (2) the normalizer is documented/defensible, (3) it's genuinely CER.
- **Label ceiling.** With ~3 verified segments, the **gold set's own label noise caps every accuracy
  number** you can report. Double-annotate a FLEURS-ckb subset, report **Krippendorff's α** (or Cohen's
  κ), adjudicate disagreements; add **ECE / reliability** on the champion's confidence before trusting it.

---

## 5. Ranked backlog (what the loop builds; RUN = owner-gated)

Ordered by leverage. "In-loop" = the loop can build/harden it now, offline, no rig. "RUN owner-gated" =
the actual GPU/marathon execution needs the owner. **None of these lets the loop claim a CER changed.**

| # | Item | In-loop work | RUN gate | Source |
|---|------|--------------|----------|--------|
| 1 | ~~Sorani normalizer = AsoSoft/ScriptNormalization~~ **AUDITED 2026-07-23 → already implemented** in `normalizer.rs` (Kaf U+0643→ک, Yeh U+064A→ی, Alef-Maksura→ی, ZWNJ incl. heh+ZWNJ→ە, zero-width strips, tashkeel, Persian/Arabic digits, NFC-idempotent). In-loop part DONE. | — (done) | re-score 7.03% under it on a contamination-checked split (still owner-gated) | §4, SN-WER |
| 2 | **Pseudo-labeling / noisy-student harness** on 1.74M clips (batch infer → confidence-filter → QLoRA pack → per-round held-out regression gate) | build all scripts + the gate | GPU run each round | §3.1–2 |
| 3 | ~~KenLM n-gram fusion~~ **REFUTED (iter 144)** — sherpa offline LM is neural-ONNX + transducer-only; a KenLM can't be wired to CTC at all. Residual: tune `blank_penalty` (owner-gated) or write a custom CTC+KenLM beam decoder (large, opt-in). See §3.3. | — (refuted) | tune blank_penalty (owner) | §3.3 |
| 4 | **OmniASR-CTC-1B int8 benchmark harness** (config swap + one-command 300M-vs-1B ckb scorecard + runbook) | build harness + runbook | GPU run + measure | §2 |
| 5 | **int8 quantization script** (`quantize_dynamic`, exclude CTC head, WER assert) + py policy | write script + policy | quantize new ckpt | §2 |
| 6 | **Active-learning queue ranking** (uncertainty × diversity) in the review-queue workflow | implement re-sort | owner reviews | §3.5 |
| 7 | **Gold-set IAA (Krippendorff α) + ECE/reliability** harness | build α + ECE scripts | owner double-annotates | §4 |
| 8 | **Ingest Common Voice ckb 22.0 (CC0) + SoraniTTS (CC BY 4.0)**: importer + manifest + ATTRIBUTION | build importers | owner downloads (large) | §4 |
| 9 | **Homophone-replacer FST + Kurdish name/place lexicon** (only CTC-legal biasing) — **verify Omnilingual-path support in 1.13.4 first** | verify → build lexicon + wire | — | §2 |
| 10 | **gold-CER/WER PR-gate** (from `#[ignore]`/`num_segs>0` → real gate vs committed baseline); reconcile stale ledger numbers table; bump sherpa 1.13.2→1.13.4 | implement | — | machinery |
| 11 | **Two-stage CV→FLEURS fine-tune recipe** (FLEURS-Kobani) prepared as the champion's next retrain script | build script | GPU retrain | §1, facet 1 |

**Honest gaps to keep visible:** no reproduced-externally 7.03%; no measured 1B-ckb delta; no Sorani TTS;
**sherpa CTC n-gram fusion VERIFIED impossible (iter 144 — offline LM is neural-ONNX + transducer-only)**;
homophone-path support still unverified; 1.74M + PawanKrd provenance unverified; conversational Sorani
entirely unmeasured. Refresh this scan on cadence and re-date.
