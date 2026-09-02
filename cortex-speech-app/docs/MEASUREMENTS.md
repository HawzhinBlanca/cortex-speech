# MEASUREMENTS — pinned accuracy records (never fabricated)

> [!CAUTION]
> **Historical measurement archive, not a current model attestation.** The 7.03% result below was
> scored over 922 manifest rows containing only 348 distinct clips; it is duplication-weighted and
> its confidence interval is not valid for 922 independent clips. Preserve it as provenance, but do
> not use it as a current headline, SOTA claim, release metric, or integrated-line verdict. A clean
> N=348 regeneration plus a hash-bound model attestation is required before any primary claim.

Every number here is parsed verbatim from a real harness run, stamped with the git SHA, the manifest
SHA-256 + row count, the model pins, and the exact command. New records are appended by
`make measure-10` (scripts/run_measurements.py) or, when runs are orchestrated manually, by pasting
the harness's own stdout — never by hand-computing or estimating. If a run fails, the failure is
recorded, never a placeholder number.

---

## 2026-07-10 — Same-set three-engine scorecard, FLEURS ckb_IQ **test** (N=922)

Same-set engine comparison on a **boundary-aligned** gold set: identical set (N=922 rows = 348
distinct clips; see duplication correction below).

> **Normalization correction (2026-08-05, audit H1).** This section previously claimed "identical
> NFC+lower+whitespace (space-KEPT) normalization for all engines". **That was wrong**, and the code
> proves it. The 7B and MMS-1B rows were scored by Python harnesses whose `norm()` is NFC + lowercase
> + whitespace-collapse only. The stock-300M row was scored by the **Rust** harness
> (`cargo test --test real_audio ckb_scorecard_on_gold`), whose `wer::normalize_for_metrics` ALSO
> applies `normalize_hamza`, `remove_diacritics` and `normalize_numbers` before comparing.
>
> Direction matters and is stated plainly: extra folding makes matching **more forgiving**, so
> stock-300M's 11.34% is *flattered* relative to the strict basis. That fact does not rehabilitate
> the duplication-weighted comparison or authorize it as a current headline.
>
> What IS damaged: the MAPSSWE p-value pairing champion-7B against stock-300M (§ C1 below) is
> computed across two different normalizations, so it measures the normalizers as well as the
> engines and **is not interpretable as stated**. It is retained for the record and flagged, not
> silently deleted — the run really happened. A sound champion-vs-stock significance test needs both
> engines re-scored on ONE basis; that is a re-run on the rig, owner-gated.
>
> The 7B-vs-SeamlessM4T-v2 comparison is unaffected: both are Python scorecards on the strict basis.
>
> Prevention, not just correction: every scorecard now stamps a `norm_basis` column into its TSV and
> `norm_basis` into its JSON, and `scripts/mapsswe_compare.py` **refuses** to pair two TSVs whose
> bases differ. Pinned by `scripts/test_eval_basis_policy.py`.

### Frozen eval set
- google/fleurs `ckb_iq` **test** split, all 922 rows with non-empty audio+reference (0 skipped),
  16 kHz mono PCM_16 WAV, decoded via `Audio(decode=False)` + soundfile.
- Machine manifest SHA-256 (absolute WSL paths, as scored):
  `bb5737581094e7ce4e717eb3b7726c693cfd3799eb8a03a4b0032eee1af58ac5  fleurs_ckb_iq_frozen.tsv`
- Committed portable copy (clip-relative paths), **deduplicated to 348 distinct clips** (see
  correction below): [`docs/eval/fleurs_ckb_iq_frozen.rel.tsv`](eval/fleurs_ckb_iq_frozen.rel.tsv) —
  `4063da0309b11046069bb40f865a75f56053199b28fd37580c4312049c4dd3ce` (`.sha256` sidecar committed).
  Rebuild clips with `scripts/build_fleurs_ckb_manifest.py` (FLEURS is CC-BY-4.0; see ATTRIBUTION.md).

> **Duplication correction (2026-07-23).** FLEURS `id` is the *sentence* id, shared across multiple
> recordings; the builder named every clip `<id>.wav`, so same-id recordings clobbered each other on
> disk and produced exact-duplicate manifest rows. The scored manifest above (`bb5737…`, N=922) thus
> held **348 distinct clips duplicated to 922 rows** (574 dupes). `scorecard_7b.py` /
> `scorecard_stats.py` count every row, so the pinned N=922 over-counts distinct clips ~2.6×, each
> engine's micro-CER is weighted toward the sentences with more recordings, and the bootstrap CI (over
> 922 rows) is narrower than 348 distinct clips warrant. The point estimates below were really run and
> are reported as-is; their **N and CI are duplication-affected**, not fabricated. The committed
> portable manifest is now deduped to its 348 distinct rows, and the builder disambiguates same-id
> clips (`<id>.<n>.wav`, guarded by `test_frozen_eval_manifest_integrity.py`). A clean re-score on a
> uniquely-rebuilt ~922-distinct set is owner-gated (needs the FLEURS download + the rig).

### Results (verbatim historical harness output)

| Engine (identical 922 clips) | micro CER | 95% CI | micro WER | 95% CI |
|---|:--:|:--:|:--:|:--:|
| **OmniASR-7B champion (base + Kurdish LoRA)** | **7.03%** | [6.53%, 7.55%] | 32.93% | [31.89%, 33.98%] |
| Fine-tuned MMS-CTC-1B (HF fp32, CPU) | 9.32% | — (point estimate only) | — | — |
| Stock OmniASR-CTC-300M (sherpa-onnx int8) | 11.34% | [10.83%, 11.93%] | 50.01% | — |

1. **OmniASR-7B champion** — git SHA `a44b1b7` at run time (scorecard code byte-identical through
   `6bbe551`; later commits touched only docs + db migrations), warm `cortex_7b_server.py` on
   127.0.0.1:8799 (WSL2, RTX 3090 Ti, bf16).
   Command: `wsl python3 scripts/scorecard_7b.py <manifest> 2000`
   Verbatim: `OmniASR-7B (warm server) micro CER = 7.03%   95% CI [6.53%, 7.55%]   N=922` ·
   `micro WER = 32.93%   95% CI [31.89%, 33.98%]   N=922` ·
   `throughput = 0.24 clips/s (4.125 s/clip; 3803.3s total)`
   Model pins: base `omniASR-LLM-7B-v2.pt` (30 GB, fairseq2 asset cache) SHA-256
   `1b29a4045ddfbe9125e6c9d465d5bc29063eea256ace37c129742edc07aed17a`;
   LoRA `adapter_model.safetensors` SHA-256
   `c348ade8a8160319e7e6f070addb3c7b066b70716390e8f4ae548c7db7af3750`;
   tokenizer `omniASR_tokenizer_written_v2.model` SHA-256
   `8aa11a1092142ef472537476ef6e76541123e2f0d789b79f3ebd119008240b1e`.
2. **Fine-tuned MMS-CTC-1B** — git SHA `316a549` at run time (same code-identity note), HF
   `Wav2Vec2ForCTC` fp32 on CPU (`CUDA_VISIBLE_DEVICES=""`; GPUs held by the warm 7B server).
   Command: `CORTEX_FINETUNED_MODEL=<MMS_CTC_1B_Champion dir> python scripts/measure_finetuned_cer.py <manifest>`
   Verbatim: `MMS-CTC-1B (fine-tuned) micro CER = 9.32%   (N=922)`
   Caveat: this harness prints a point estimate only (no CI/WER); the ONNX-based
   `scorecard_finetuned.py` CI leg on this set is tracked in SHIP_FINAL_PLAN WS1.
3. **Stock OmniASR-CTC-300M** — git SHA `316a549` at run time (same code-identity note), int8 ONNX
   from `scripts/fetch_models.py` (SHA-256-pinned there).
   Command: `CORTEX_GOLD_MANIFEST=<manifest> CORTEX_GOLD_RESULTS=<out.tsv> cargo test --test real_audio ckb_scorecard_on_gold -- --ignored --nocapture`, then `python scripts/scorecard_stats.py <out.tsv> 2000`
   Verbatim: `[scorecard] N=922 micro_CER=0.1134 micro_WER=0.5001 (ckb, OmniASR-CTC-300M)` ·
   stats: `micro CER = 11.34%   95% CI [10.83%, 11.93%]` · output-script split: `arab N=922 (100%)`.

### C1 engine decision (M1.3) — recorded

**The OmniASR-7B champion remains the default engine, now on measured evidence:** best CER on the
same-set benchmark (−4.3 pts vs stock, 38% relative; −2.3 pts vs fine-tuned 1B, 25% relative),
100%-Arabic-script output, coherent proper-noun handling (verified in real-app e2e). The −4.3 pt
stock margin is measured **across two normalization bases** (see the correction above) and is
therefore a lower bound on the true gap, not a paired result; the decision does not rest on its
significance test.
Same-set external context: ElevenLabs Scribe v1 publishes 32.1% WER on FLEURS-ckb; the champion's
32.93% [31.89, 33.98] is statistically on par — with the caveat that this normalization counts digit
verbalization/format as errors, which penalizes the champion's style. Trade-off accepted by policy:
the champion needs the warm WSL server (~4.1 s/clip here) and the app ASKS (retry/offline) rather
than silently downgrading when it is down (verified 2026-07-10). Fallback order for the offline
path: fine-tuned MMS-1B (9.32%) over stock CTC-300M (11.34%) — matching the shipped juror-ability
ordering (7B > finetuned > 1b > 300m).

### SeamlessM4T-v2 external baseline + MAPSSWE significance (added same day)

4. **SeamlessM4T-v2** (the charter-required external baseline; stock Whisper is explicitly invalid
   for ckb) — git SHA `ca16a38`, `facebook/seamless-m4t-v2-large` via transformers
   `SeamlessM4Tv2ForSpeechToText`, `tgt_lang="ckb"`, fp32 CPU, same frozen manifest + normalization.
   Command: `python scripts/scorecard_seamless.py <manifest> <out.tsv> 2000`
   Verbatim: `SeamlessM4T-v2 micro CER = 12.71%   95% CI [12.02%, 13.44%]   N=922` ·
   `micro WER = 42.38%   95% CI [41.17%, 43.59%]   N=922` · `(10.88 s/clip; 10030s total)`

**MAPSSWE matched-pairs significance** (`python scripts/mapsswe_compare.py <A.tsv> <B.tsv>`,
verbatim output; per-clip TSVs pair 1:1 in manifest order, N=922):

```
MAPSSWE word: champion7b 32.93% vs seamlessv2 42.38%  mean diff/seg = -1.793  z = -16.10  p = 2.415e-58   -> champion7b better  (SIGNIFICANT p<0.05)
MAPSSWE char: champion7b  7.03% vs seamlessv2 12.71%  mean diff/seg = -6.999  z = -24.41  p = 1.301e-131  -> champion7b better  (SIGNIFICANT p<0.05)
MAPSSWE word: champion7b 32.93% vs stock300m  50.01%  mean diff/seg = -3.241  z = -28.59  p = 1.025e-179  -> champion7b better  (SIGNIFICANT p<0.05)
MAPSSWE char: champion7b  7.03% vs stock300m  11.34%  mean diff/seg = -5.312  z = -26.26  p = 5.849e-152  -> champion7b better  (SIGNIFICANT p<0.05)
```

**Charter comparison gate (line 13/48) — MET on this set:** MAPSSWE p<0.05 ✓ AND champion ci_high <
baseline ci_low on both metrics (CER 7.55% < 12.02% ✓; WER 33.98% < 41.17% ✓) vs SeamlessM4T-v2.

### Honest caveats
- **Training-set overlap with the 7B LoRA is UNVERIFIABLE on both sets** (audit H5, restored
  2026-08-05). `FINAL_READINESS_10.md` §M1.1 requires this caveat to be *"permanent … stated, never
  hidden"*, because the LoRA's training manifest lives on an offline drive and cannot be checked
  against these clips. This section previously called the FLEURS set **"known-disjoint"**, which
  asserted precisely what the plan says cannot be established. Using the official FLEURS **test**
  split makes overlap *unlikely* — that is the standard split contract, and it is the reason the set
  was chosen — but "unlikely by construction" is not "verified", and only the latter licenses a SOTA
  claim. Until that manifest is available, every number here carries this caveat.
- The CV22-ckb champion number (5.04% CER, 2026-07-09) predates this record and carries the same
  caveat. Neither it nor the duplication-weighted FLEURS result is a current headline; public source
  availability does not substitute for deduplication, disjointness evidence, and attestation.
- FLEURS is read speech; no conversational Sorani number exists yet anywhere (that requires the
  app-gold set from the owner's review marathon — SHIP_FINAL_PLAN §B #37/#41).
- Digit/punctuation normalization is the strict space-kept basis shared by all three engines here;
  a `CORTEX_CER_STRIP=1` "fair" basis exists in `scorecard_7b.py` but is deliberately NOT used for
  the historical run (owner methodology decision pending — SHIP_FINAL_PLAN §B #45).
