# M1 · Engine Decision Runbook

**Milestone**: Measure three ASR engines on frozen gold sets (FLEURS ckb_IQ + Common Voice 22 ckb) and apply the decision protocol to determine the default engine. **Zero owner time** (GPU-only, ~4-6 hours wall-clock).

## M1.1 · Freeze FLEURS ckb_IQ test split

**What**: Download google/fleurs ckb_IQ test split, create a frozen manifest (SHA-pinned), commit to repo.

```bash
cd cortex-speech-app
python scripts/build_fleurs_ckb_manifest.py --output scripts/fleurs_ckb_iq_frozen.json
# Outputs:
#   - scripts/fleurs_ckb_iq_frozen.json (manifest: audio index, reference, gender, age)
#   - scripts/fleurs_ckb_iq_frozen.txt (SHA-256 of the manifest)
git add scripts/fleurs_ckb_iq_frozen.* && git commit -m "feat(m1.1): freeze FLEURS ckb_IQ test split"
```

**Expected output**: N=~200-500 clips (ckb_IQ test split size varies; exact N recorded in manifest).

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

### 7B (warm server):
```bash
wsl python scripts/scorecard_7b.py scripts/fleurs_ckb_iq_frozen.json 3000 \
  > /tmp/7b_fleurs_results.tsv
# Computes: CER (%), WER (%), micro aggregation, Bisani & Ney bootstrap CI
# Outputs to ledger:
# - 7B / FLEURS / CER: [X.XX%, CI: [a, b]]
```

### Fine-tuned MMS-1B:
```bash
python scripts/measure_finetuned_cer.py scripts/fleurs_ckb_iq_frozen.json 3000 \
  > /tmp/finetuned_fleurs_results.tsv
# Identical normalization + bootstrap as 7B
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

## Commands to run (summary for quick copy-paste)

```bash
# Step 1: Freeze manifests
python cortex-speech-app/scripts/build_fleurs_ckb_manifest.py --output cortex-speech-app/scripts/fleurs_ckb_iq_frozen.json

# Step 2: Run benchmarks (GPU-heavy, ~2h each)
wsl python cortex-speech-app/scripts/scorecard_7b.py cortex-speech-app/scripts/fleurs_ckb_iq_frozen.json 3000
python cortex-speech-app/scripts/measure_finetuned_cer.py cortex-speech-app/scripts/fleurs_ckb_iq_frozen.json 3000
cd cortex-speech-app && cargo test --manifest-path src-tauri/Cargo.toml --test real_audio -- --ignored omniasr_on_fleurs --nocapture

# Step 3: Record results in EVAL.md with artifacts (command SHA, dataset SHA, N, metric, CI)

# Step 4: Apply decision protocol, flip default in settings.rs, test

# Step 5: Commit "feat(m1): engine decision protocol and measured default"
```

**Owner time required**: ~0 (GPU runs in background, results reviewed once complete).
**GPU time**: ~6 hours (3 engines × 2 datasets, parallel runs reduce to ~3-4 hours).
**Dependency**: FLEURS download library (datasets or audioset); CV22 already on disk.

## Notes

- If FLEURS download fails (network, library), fall back to CV22-only + FLEURS N=1 spot-check (see EVAL.md current state).
- RTF measured on a *personal machine* — not reference-rig, but a data point.
- The decision is the ONLY thing that changes; no code beyond the decision-protocol logic and the default flip.
- Once M1 green, M2 instruments the app (no GPU), then M3 is the owner's gold marathon.
