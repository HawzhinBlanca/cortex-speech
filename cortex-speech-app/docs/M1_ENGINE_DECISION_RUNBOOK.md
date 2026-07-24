# M1 · Engine Decision Runbook

**Milestone**: Measure three ASR engines on frozen gold sets (FLEURS ckb_IQ + Common Voice 22 ckb) and apply the decision protocol to determine the default engine. **Zero owner time** (GPU-only, ~4-6 hours wall-clock).

## M1.1 · Freeze the eval manifests (TSV — the format the scorecards parse)

> **Manifest format (P2.1 correction):** every scorecard (`scorecard_7b.py`, `scorecard_finetuned.py`,
> `measure_finetuned_cer.py`) reads a **TSV** manifest — one `<wav_path>\t<reference>` row per clip
> (optional `\t<gender>\t<age>`). Emit `.tsv`, **not** `.json`. Each builder also writes a `.sha256`
> sidecar so the eval set is pinned.

**CV22 ckb (on disk — no download; builder ready):**

```bash
cd cortex-speech-app
# One-time: unpack the tar-packed audio shard for the split, then build the frozen manifest.
python scripts/build_cv22_ckb_manifest.py \
    --cv22-dir "$CORTEX_CV22_DIR" --split test --extract --wsl-paths \
    --output scripts/cv22_ckb_test_frozen.tsv
# Outputs scripts/cv22_ckb_test_frozen.tsv (+ .sha256). ~5.3k clips in the ckb test split.
# --wsl-paths rewrites C:\ -> /mnt/c/ so scorecard_7b.py (runs in WSL) can read the paths.
git add scripts/cv22_ckb_test_frozen.tsv* && git commit -m "feat(m1.1): freeze CV22 ckb test manifest"
```

**FLEURS ckb_IQ (one-time ~1–2 GB download):**

```bash
pip install datasets soundfile   # one-time
python scripts/build_fleurs_ckb_manifest.py --output-dir scripts/fleurs_ckb_iq --wsl-paths
# Writes scripts/fleurs_ckb_iq/clips/*.wav + scripts/fleurs_ckb_iq/fleurs_ckb_iq_frozen.tsv (+ .sha256).
git add scripts/fleurs_ckb_iq/fleurs_ckb_iq_frozen.tsv* && git commit -m "feat(m1.1): freeze FLEURS ckb_IQ test manifest"
```

The FLEURS-ckb WER row is the one directly comparable to ElevenLabs Scribe's published **32.1% WER**
(theirs is WER on FLEURS-ckb) — but the split is only ~350 sentences, so **report the CI, never a
tight point estimate**, and note the contamination caveat (M1.2). CV22 remains the larger, boundary-
aligned fallback if the download fails.

**Expected output**: CV22 ckb test ≈ 5.3k clips; FLEURS ckb_IQ ≈ 350 sentences (wide CI — report the
interval, never headline a tight point estimate).

## M1.2 · Contamination statement

**What**: Attempt to verify whether FLEURS/CV22 appear in the 7B LoRA training mix. If the manifest is inaccessible (on the F: drive), record ABSENT and annotate FLEURS numbers with caveat.

```bash
# If F: drive is accessible:
grep "fleurs\|ckb" /mnt/f/Kurdish\ Sorani\ Dataset/Kurdish_Sorani_ASR_Combined_v12/*/metadata.jsonl | wc -l
# If not found, record in EVAL.md:
# **FLEURS contamination**: training-set overlap with 7B LoRA unverifiable (v12 manifest on offline F: drive).
```

## M1.3 · Three-engine benchmark (THE MAIN MEASUREMENT)

**What**: Run 7B, fine-tuned MMS-1B, and stock OmniASR-CTC-300M on both FLEURS and CV22 ckb test sets. Measure CER, WER, RTF with identical normalization and bootstrap 95% CI.

> **Fastest path**: use the verified copy-paste block at the bottom of this file — `run_measurements.py`
> runs the 7B + fine-tuned scorecards in one honesty-stamped command AND leaves the per-clip TSVs the
> significance test needs. The per-engine commands below are the manual equivalent.

### 7B (warm server):
```bash
# scorecard_7b writes its PER-CLIP TSV to omni7b_results.tsv NEXT TO the manifest (stdout is the headline).
wsl python3 cortex-speech-app/scripts/scorecard_7b.py scripts/fleurs_ckb_iq/fleurs_ckb_iq_frozen.tsv 3000
# Computes: CER (%), WER (%), micro aggregation, seeded utterance-bootstrap 95% CI.
```

### Fine-tuned MMS-1B:
```bash
# Use scorecard_finetuned.py (not measure_finetuned_cer.py) — it writes the per-clip finetuned_results.tsv
# the paired significance test pairs against omni7b_results.tsv. Needs CORTEX_FINETUNED_MODEL + _ONNX.
python scripts/scorecard_finetuned.py scripts/fleurs_ckb_iq/fleurs_ckb_iq_frozen.tsv 3000
# Identical normalization + bootstrap as 7B.
```

### Paired significance (the p-value M1.4 needs):
```bash
python scripts/mapsswe_compare.py scripts/fleurs_ckb_iq/omni7b_results.tsv \
    scripts/fleurs_ckb_iq/finetuned_results.tsv 7B finetuned
# Matched-pairs z-test on CER + WER, paired by clip_index (safe when engines skip different clips).
```

### Stock OmniASR-CTC-300M:
```bash
cargo test --manifest-path src-tauri/Cargo.toml --test real_audio -- \
  --ignored omniasr_on_committed_fleurs_ckb_fixture --nocapture
# Existing gate; also run on FLEURS full set for comparison
```

Repeat for CV22 ckb test split.

**Expected output**: 3×2 (3 engines × 2 datasets) CER/WER/RTF numbers with CIs, pasted into EVAL.md with commands/SHAs.

## M1.4 · Engine decision protocol + flip default

**What**: Apply the decision rule to the measured numbers and change the default engine (if needed).

**Decision rule**:
```
default_engine = {
  "7B" if CER(7B) < CER(fine-tuned) AND p-value(paired-test) < 0.05,
  else "fine-tuned" if CER(fine-tuned) < CER(stock) AND p-value < 0.05,
  else "stock" if no clear winner,
  else defer to app-gold (M3 — once gold data exists)
}
```

**Implementation**:
```rust
// src-tauri/src/settings.rs
pub fn default_asr_engine(fleurs_results: &M1Results) -> AsrModelSize {
    // Compare paired bootstrap CIs; apply protocol
    // Record decision + evidence in settings default
}
```

**Flip the default**:
```rust
pub fn new() -> Self {
    Self {
        asr_model_size: AsrModelSize::FineTuned, // or Stock, or WSL7B based on protocol
        ...
    }
}
```

**Gate**: USER-OBSERVABLE — import a fresh clip, check the engine badge. Decision + numbers in ledger.

---

## Commands to run (summary — verified copy-paste, 2026-07-24)

> The FLEURS builder now EXISTS (`build_fleurs_ckb_manifest.py`, M1.1). Every number below comes from a
> real run of the real harness; `run_measurements.py` stamps the git SHA + manifest SHA-256 + row count +
> exact command line, and the paired significance step (`mapsswe_compare.py`) is the honest way to decide
> "which engine wins" — do NOT eyeball two overlapping CIs.

```bash
# ── ONE-TIME PREREQUISITES ──────────────────────────────────────────────────────────────────────
# a) Warm OmniASR-7B server in WSL (for the 7B scorecard) — leave it running in its own terminal:
#      wsl python3 cortex-speech-app/scripts/cortex_7b_server.py
# b) Fine-tuned ONNX exported + env pointing at it (for the fine-tuned scorecard):
#      export CORTEX_FINETUNED_MODEL="<your fine-tuned HF export dir>"
#      export CORTEX_FINETUNED_ONNX="<that dir>/model.int8.onnx"
#      # (first time only: scripts/export_finetuned_onnx.py then scripts/quantize_finetuned_onnx.py)

# ── STEP 1 · FREEZE A GOLD MANIFEST (writes a .sha256 sidecar so the eval set is pinned) ──────────
# FLEURS ckb_IQ — the ~350-sentence set the champion's 7.03% CER is measured on (wide CI, report it):
python cortex-speech-app/scripts/build_fleurs_ckb_manifest.py \
    --output-dir cortex-speech-app/scripts/fleurs_ckb_iq --wsl-paths
MANIFEST=cortex-speech-app/scripts/fleurs_ckb_iq/fleurs_ckb_iq_frozen.tsv
# OR the larger CV22 ckb test split (~5.3k clips, already on disk — no download):
#   python cortex-speech-app/scripts/build_cv22_ckb_manifest.py --cv22-dir "$CORTEX_CV22_DIR" \
#       --split test --extract --wsl-paths --output cortex-speech-app/scripts/cv22_ckb_test_frozen.tsv

# ── STEP 2 · CER / WER / 95% CI PER ENGINE (one command; honesty-stamped output) ──────────────────
python cortex-speech-app/scripts/run_measurements.py "$MANIFEST" --engines 7b,finetuned --bootstrap 3000
# Also leaves the PER-CLIP TSVs next to the manifest — omni7b_results.tsv + finetuned_results.tsv —
# which the significance test below consumes. (7B must run where it can reach the warm WSL socket.)

# ── STEP 3 · PAIRED SIGNIFICANCE — the p-value the decision rule needs ─────────────────────────────
MDIR=$(dirname "$MANIFEST")
python cortex-speech-app/scripts/mapsswe_compare.py \
    "$MDIR/omni7b_results.tsv" "$MDIR/finetuned_results.tsv" 7B finetuned
# MAPSSWE matched-pairs z-test on CER + WER, paired BY clip_index — correct even if the two engines
# skipped different clips (a plain row-zip would silently mispair). Prints mean diff, z, p, verdict.

# ── STEP 4 · DECIDE + (only if it flips) change the default ────────────────────────────────────────
# Apply the M1.4 rule to the measured CER + the mapsswe p. If the default changes, set
# AsrModelSize in settings.rs::new(), then verify USER-OBSERVABLE (import a clip, check the engine badge).
# Paste the numbers + the exact commands + SHAs into the ledger. Never a metric without its real run.
```

**Owner time required**: ~0 (GPU runs in background, results reviewed once complete).
**GPU time**: ~6 hours (3 engines × 2 datasets, parallel runs reduce to ~3-4 hours).
**Dependency**: FLEURS download library (datasets or audioset); CV22 already on disk.

## Notes

- If FLEURS download fails (network, library), fall back to CV22-only + FLEURS N=1 spot-check (see EVAL.md current state).
- RTF measured on a *personal machine* — not reference-rig, but a data point.
- The decision is the ONLY thing that changes; no code beyond the decision-protocol logic and the default flip.
- Once M1 green, M2 instruments the app (no GPU), then M3 is the owner's gold marathon.
