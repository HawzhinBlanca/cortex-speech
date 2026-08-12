# Cortex Speech: Evidence-Based Product and Technical Roadmap

**Research date:** 2026-08-06  
**Repository examined:** `e8eb646` (`codex/newbranch`)  
**Scope:** what will materially improve the application now, why it matters, how to build it, and what evidence should decide whether it ships.

## Executive conclusion

Cortex Speech does not mainly need more features or a fashionable new speech model. It already has an unusually deep local-first Sorani workflow: import, VAD/chunking, several recognizers, diarization, forced alignment, human review, provenance, rights controls, validation, and multi-format export. The latest hardening work also closed the previously reported consent-withdrawal, couch-review lease, spot-check, and revoked-audio bundle defects.

The largest remaining risk is **decision quality**: the app cannot yet prove that the next model, queue, or automation setting improves real user audio. Its headline evaluation contains 922 rows but only 348 distinct clips, the champion and one baseline were scored under different normalizers, the LoRA training overlap cannot currently be checked, and no conversational Sorani benchmark exists. Those are documented honestly in [MEASUREMENTS.md](MEASUREMENTS.md), but they prevent a strong external accuracy claim.

The best roadmap is therefore:

1. Build one contamination-resistant evaluation spine.
2. Replace the overloaded scalar “confidence” with separate, explainable risk signals.
3. Route review by uncertainty **and** diversity, then measure error reduction per human minute.
4. Make recording identity and duplicate protection durable across restarts.
5. Turn the many capabilities into one obvious user journey and measure where time is lost.
6. Only then benchmark newer models behind the same promotion gate.

This is not the glamorous answer, but it is the highest-confidence path to a more accurate and more useful app.

## What is already strong and should be preserved

- **Local-first is the correct default.** Audio remains local unless a specific cloud feature is enabled. API keys are protected with Windows DPAPI, rights state travels through export, and a live cloud-consent withdrawal now reaches an import already in progress.
- **The review experience is not primitive.** It already has keyboard-first actions, exact word playback, disputed-word highlighting, inline word editing, forced alignment, undo/redo, draft preservation, session restore, a sticky action bar in Review Inbox, and local reviewer-throughput statistics.
- **Dataset governance is substantially stronger than most small ASR tools.** Production export can block on rights, validation, provenance, and revoked/holdout content; model manifests and dataset cards are emitted.
- **The Windows release path has real supply-chain controls.** The workflow requires Authenticode signing, emits checksums and a CycloneDX SBOM, and creates GitHub build attestations before publication.
- **The current model choice is defensible.** The app already runs a Sorani-adapted OmniASR LLM 7B v2 champion. Meta’s current model family still explicitly covers more than 1,600 languages and its v2 line is the relevant modern family for Cortex—not a legacy model that should be replaced on age alone. [Meta OmniASR repository](https://github.com/facebookresearch/omnilingual-asr)

These are product assets. A rewrite would put them at risk without addressing the main evidence gap.

## Ranked roadmap

| Rank | Improvement | Expected value | Evidence confidence | Approximate scope |
|---:|---|---|---|---|
| 1 | Clean evaluation and promotion gate | Makes every accuracy decision trustworthy | Very high | Medium |
| 2 | Separate uncertainty, acoustic quality, OOD, and agreement | Safer automation and better review explanations | High | Medium |
| 3 | Uncertainty × diversity review selection | More model improvement per review minute | High, transfer must be measured on Sorani | Medium |
| 4 | Durable recording identity and duplicate control | Prevents silent corpus/eval contamination | Very high | Medium |
| 5 | One guided workflow plus local UX instrumentation | Makes the existing capabilities understandable and faster | High | Medium |
| 6 | Bounded data paths and frontend decomposition | Makes 100k+ clip libraries predictable and maintainable | Very high | Medium–large |
| 7 | OmniASR v2 CTC bakeoff; research-only alternatives | Possible faster/better fallback without destabilizing the champion | Medium | Small–medium |
| 8 | Finish dependency/release/metadata polish | Makes public distribution and reuse more credible | High | Small–medium |

## 1. Establish one clean evaluation spine

### Observed evidence

The current FLEURS scorecard is useful but not a clean final benchmark:

- 922 manifest rows represented only 348 distinct overwritten clips, so point estimates and confidence intervals are duplication-weighted.
- The 7B champion and stock 300M result were scored with different normalization functions; the repository now blocks new cross-basis MAPSSWE comparisons, but the historical comparison cannot be repaired without a rerun.
- The LoRA training manifest is unavailable, so train/test overlap is unverified.
- FLEURS measures read speech. The app has no pinned conversational, multi-speaker, noisy Sorani benchmark.

This means “7.03% CER” is a real historical measurement, but not yet a sufficient production acceptance criterion.

NIST’s 2025 ARIA evaluation work recommends connecting model tests, red-team tests, and field testing through an explicit measurement structure instead of treating one benchmark as the system verdict. [NIST ARIA 0.1 report](https://www.nist.gov/publications/assessing-risks-and-impacts-ai-aria-pilot-evaluation-report)

### How to improve it

Create a versioned evaluation manifest with at least:

```text
row_id, recording_id, utterance_id, speaker_id, source_dataset,
canonical_pcm_sha256, source_split, dialect, domain, device,
snr_bucket, reference_text, reference_version, normalizer_id,
rights_state, created_at
```

Then maintain three non-overlapping suites:

1. **Public read-speech suite:** rebuild all unique FLEURS `ckb_iq` test recordings with collision-free filenames.
2. **In-domain suite:** human-adjudicated clips from the actual recordings Cortex is meant to process. Split by recording and speaker, never by segment alone.
3. **Stress suite:** noisy audio, clipping, long-form seams, multi-speaker audio, devices, and underrepresented speakers/dialects. This is a failure-discovery suite, not a flattering headline.

Every engine run must use the same frozen rows, decoder configuration, and normalizer. Report both micro error and speaker/recording-level macro error. Keep per-clip outputs so paired bootstrap intervals and matched-pairs tests can be reproduced. Group resampling by recording when several segments come from the same source.

Before calling any set “gold,” independently review a meaningful subset, adjudicate disagreements, and report the agreement statistic with its sample count. The app already has reviewer attribution and spot checks; use that infrastructure to measure the reference itself.

### Promotion gate

A model is promoted only when all of these are true:

- zero known canonical-audio overlap with training or calibration data;
- identical manifest and normalization basis for incumbent and candidate;
- paired accuracy interval meets a predeclared superiority or non-inferiority rule;
- no material regression in any protected domain/speaker/noise slice;
- latency, VRAM/RAM, failure rate, and offline behavior stay inside declared budgets;
- the exact model, adapter, tokenizer, decoder, and manifest hashes are recorded.

If the old LoRA training manifest remains unavailable, preserve that caveat permanently and do not market the result as independently verified Sorani SOTA.

## 2. Replace one “confidence” with an explainable review-risk contract

### Observed evidence

The app currently uses several different notions under confidence-like fields: a heuristic engine value, IRT agreement, CTC/acoustic information when available, conformal routing, and human verification. The latest hardening correctly prevents poor-audio and single-voter hard vetoes from being auto-accepted, but queue ordering and parts of the UI still collapse evidence into a scalar.

That is unsafe because agreement is not correctness. Two related CTC models can agree on the same error, and the default sherpa OmniASR CTC path does not expose a trustworthy token posterior. Recent ASR research found that under severe noise, 10–20% of wrong Whisper tokens still had confidence above 0.7; selective calibration reduced expected calibration error in that experiment, but did not fix recognition itself. [ASRU 2025 noisy-ASR calibration paper](https://arxiv.org/abs/2509.07195)

Calibration and conformal prediction must also be evaluated separately. A 2025 TMLR study found that conventional temperature scaling can make adaptive conformal sets larger; “better calibrated probability” does not automatically mean a more efficient conformal decision. [TMLR paper](https://openreview.net/forum?id=6DDaTwTvdE)

### How to improve it

Persist and expose separate fields:

```text
agreement_score       # how much independent hypotheses agree
acoustic_risk         # SNR, clipping, alignment failure, speech coverage
error_probability     # calibrated on human gold; NULL when unavailable
ood_score             # distance from validated speaker/domain conditions
review_priority       # routing score, not presented as correctness probability
review_reason_codes   # stable machine-readable explanations
```

Rules:

- A heuristic `0.90` must remain explicitly heuristic and cannot become `error_probability=0.10`.
- Missing evidence stays `NULL`; it is not converted to a neutral-looking number.
- Model-family correlation is part of the voter metadata. Agreement from two closely related OmniASR CTC checkpoints should count as weaker evidence than agreement from architecturally independent systems.
- Calibration is fitted only on human-adjudicated data and reported per noise/domain bucket where sample size permits. Small buckets fail closed to review.
- Measure ECE, Brier score or NLL for probability quality; separately measure conformal coverage, review fraction, and selective CER for routing quality.

In the UI, lead with reasons such as “models disagree on 3 words,” “low SNR,” “speaker/domain not represented in calibration,” or “only one independent recognizer.” A percentage can remain in diagnostics, but it should not be the primary explanation.

### Success gate

On a held-out human-gold set, plot error rate versus retained/auto-accepted coverage. Choose a threshold only when the upper confidence bound on accepted error is below the product’s declared target in every sufficiently represented slice. Track human overrides and adjudications; NIST’s AI RMF playbook specifically recommends measuring overrides, reported errors, and adjudication activity. [NIST AI RMF Measure playbook](https://airc.nist.gov/airmf-resources/playbook/measure/)

## 3. Make the review queue uncertainty × diversity × impact

### Observed evidence

`get_active_learning_queue` currently loads all segments, keeps unverified rows, and ranks only by closeness to the conformal threshold. That finds ambiguous samples but can repeatedly select the same recording, speaker, noise condition, or error pattern. It also turns into an unbounded memory path as the corpus grows.

Recent direct ASR evidence supports combining epistemic uncertainty with core-set selection: a 2025 ACL study reported a 27% relative WER improvement while using 45% less data than its baselines on African-accented English. That result is not a Sorani guarantee, but it is strong evidence for the selection mechanism. [ACL 2025 active-learning ASR study](https://aclanthology.org/2025.acl-srw.1/)

### How to improve it

Build the queue in two stages:

1. Query a bounded candidate pool in SQL using unverified state, risk, rights, and current review filters.
2. Select a batch that balances:
   - uncertainty or expected error;
   - diversity across recording, speaker, source, duration, SNR, and dialect/domain;
   - impact, such as duration, disagreement span, or a recurrent error family;
   - fairness constraints so one abundant speaker/domain cannot consume the batch.

Start with metadata diversity—no new model is required. Later, add a versioned frozen audio/text embedding only if metadata selection plateaus. Use a greedy max-min or cluster-stratified selector, cap samples per source recording, and store the selection round and score components for auditability.

Do not optimize the queue for “hardest clips reviewed.” Optimize for **error reduction per human minute**:

```text
label_efficiency = held_out_error_reduction / human_review_minutes
```

Run alternating or randomized review batches comparing the current uncertainty-only queue with the hybrid queue. Also compare reviewer seconds per clip and coverage across speakers/domains. Promote the new ranking only if it improves the learning curve without hiding difficult subgroups.

## 4. Make recording identity durable across restarts

### Observed evidence

`src-tauri/src/fingerprint.rs` explicitly documents that its spectral fingerprint map is in memory and starts empty on every launch. The UI correctly labels the count as session-only. Full-file hashes already exist for source-transcript cache, corrections, and gold data, but there is no single persisted recording identity shared by import, segmentation, evaluation, learning, and export.

The current 64-bit energy fingerprint is useful as a cheap hint, but it is too coarse to be a durable uniqueness constraint. A false collision must never make the app discard a legitimate voice recording.

### How to improve it

Introduce a first-class `recordings` entity:

```text
recordings(
  id, original_file_hash, canonical_pcm_hash,
  hash_algorithm, canonicalization_version,
  duration_ms, sample_rate, channels,
  rights/provenance fields, created_at
)
segments(recording_id, start_ms, end_ms, ...)
import_runs(recording_id, pipeline_config_hash, model_version_id, ...)
```

- Hash the original bytes for exact file identity.
- Hash a versioned canonical PCM representation for transcoded copies.
- Re-importing known content should be idempotent or offer an explicit reprocess action attached to the same recording; it should not create a second independent corpus identity.
- Keep the current spectral fingerprint only as an advisory near-duplicate signal. Present a cluster for human confirmation; do not auto-delete or block on it.
- Backfill existing rows in a resumable job. Missing files stay visibly unresolved rather than receiving invented hashes.
- Use `recording_id`/canonical hash as the grouping key for train/eval splits and bootstrap resampling.

This single change closes a product defect and strengthens every accuracy claim.

## 5. Turn the feature set into one obvious workflow

### Observed evidence

The app has the parts of a professional annotation studio, but it still presents a lot of system concepts. There is no clear first-run wizard in the current Svelte code. `App.svelte` remains about 3,399 lines, and the primary Review Mode action row is not sticky even though Review Inbox already implements a sticky verb bar. Local throughput exists, but the app does not yet measure the complete path from first import to trustworthy export.

A 1,280 x 720 visual pass on 2026-08-06 confirmed the product impact. The workspace entry state exposes more than ten similarly prominent actions across its top controls, so the otherwise useful central “start review” prompt must compete with import, export, validation, model, inbox, and settings actions. In Review Mode, the waveform and word-level evidence are strong, but the accept/save action row starts below the initial viewport. The Insights view is the clearest pattern to preserve: export readiness, blockers, reviewed progress, duration, and speaker coverage are understandable at a glance. Screenshot evidence supports simplifying the entry hierarchy, making Review Mode actions sticky, and turning each readiness blocker into a direct corrective action. It does not, by itself, establish keyboard order, 200% zoom behavior, contrast compliance, or screen-reader quality; those still require dedicated accessibility tests.

### How to improve it

Make the home screen answer three questions:

1. **What should I do now?** Import audio, continue the highest-value review batch, or fix export blockers.
2. **How long will it take?** Reuse the existing per-reviewer median to estimate remaining review time, with an “estimate based on N intervals” disclosure.
3. **Why is export blocked?** Show the smallest actionable set of reasons, with direct links to the affected clips/settings.

Add a first-run readiness check:

- library/data location and backup destination;
- local fallback model installed and verified;
- 7B/WSL optional acceleration status;
- expected model disk/VRAM requirements;
- cloud features off by default, with provider-specific consent;
- a short sample import that proves playback, transcription, save, and export.

For review:

- make Review Mode actions sticky and ensure focused controls are never obscured;
- show reason-first risk chips, not only colors or a percentage;
- keep `Accept`, `Edit/Save next`, `Bad audio`, replay, and undo in a stable position;
- add an end-of-session summary: reviewed, edited, rejected, median seconds, recurrent disagreements, and estimated effect on the next training/eval batch.

Store product instrumentation locally by default: time to first successful import, time per decision, replay count, edit distance, undo rate, abandoned drafts, export-blocker frequency, model/fallback failures, and queue strategy. Export an anonymized diagnostic report only through explicit consent.

WCAG 2.2 adds requirements for visible/unobscured focus and minimum target size. Treat AA conformance, RTL keyboard traversal, 200% zoom, and screen-reader names as release tests—not optional polish. [W3C WCAG 2.2](https://www.w3.org/TR/WCAG22/)

### Success gate

Run the same representative tasks before and after the redesign. Ship only if median time to first successful export and median review seconds fall, while incorrect accepts, abandoned drafts, undo rate, and accessibility violations do not rise. The existing automated axe/Playwright coverage should be supplemented with short observed sessions using real Sorani work.

## 6. Make large libraries bounded and the code easier to change

### Observed evidence

Pagination exists for the main segment store, but legacy `get_segments` remains public and multiple analytics/active-learning commands still call `db.get_segments(None)`. This can copy the entire corpus through SQLite, Rust, IPC, and JavaScript. The active-learning command is one confirmed example. `App.svelte` also still owns too many unrelated workflows, which raises regression risk even though tests are extensive.

### How to improve it

- Replace full-row analytics with SQL aggregates and narrow projections.
- Require a cursor/limit for list IPC commands. Keep an explicitly named bounded export/maintenance iterator for jobs that truly need every row.
- Move queue selection, statistics, and export to cancellable background jobs with progress and resumable checkpoints where practical.
- Add database indexes based on measured query plans, especially for review state, rights state, recording, speaker, source, and risk bucket.
- Split frontend orchestration by domain: import workspace, review workspace, validation/export workspace, model/runtime status, and settings. Preserve Svelte/Tauri; this is decomposition, not a rewrite.
- Add synthetic 100k and 1M segment benchmarks for startup, first page, search, queue creation, statistics, and cancellation. Set budgets from acceptable interactive behavior, then make them CI/nightly regression gates.

The target is not a smaller file for its own sake. It is the ability to change one workflow without loading or invalidating the others.

## 7. Use newer speech technology as gated candidates, not roadmap drivers

### High-value candidate: OmniASR CTC v2 fallback

The bundled sherpa archive is still the `2025-11-12` OmniASR 300M CTC int8 package. Meta later released improved v2 CTC checkpoints, and sherpa-onnx added v2 support/export. [sherpa-onnx changelog](https://github.com/k2-fsa/sherpa-onnx/blob/master/CHANGELOG.md) The documented sherpa path remains greedy CTC decoding, so this is a model bakeoff—not a reason to promise beam search or easy KenLM integration. [sherpa OmniASR model docs](https://k2-fsa.github.io/sherpa/onnx/omnilingual-asr/models.html)

Action:

1. Export/pin OmniASR CTC 300M v2 and 1B v2 int8.
2. Register them as experimental candidates, never overwrite the current fallback in place.
3. Run the clean paired suites plus RTF, peak memory, startup, and failure tests.
4. Promote the smallest candidate that clears the accuracy/non-inferiority and device budgets.

The current 7B champion already uses LLM 7B v2 plus a Kurdish LoRA. Keep it until a clean comparison says otherwise. Meta’s unlimited-length v2 models are interesting for long audio, but official fine-tuning recipes are not supported for that variant; it is not a drop-in replacement for the adapted champion. [Meta inference/model documentation](https://github.com/facebookresearch/omnilingual-asr/blob/main/src/omnilingual_asr/models/inference/README.md)

### Research only: models with no demonstrated `ckb` support

| Technology | Current evidence | Cortex decision |
|---|---|---|
| Qwen3-ASR 0.6B/1.7B | Supports 30 named languages plus Chinese dialects; the official list includes Arabic and Persian but not Kurdish. Its forced aligner covers 11 languages, also not Kurdish. [Official model card](https://huggingface.co/Qwen/Qwen3-ASR-0.6B-hf) | Do not integrate as default or jury voter. A one-off clean `ckb` benchmark may justify future work. |
| Voxtral Transcribe 2 | Diarization, context biasing, timestamps, and 13 named languages; Kurdish is not among them. [Mistral release](https://mistral.ai/news/voxtral-transcribe-2/) | Do not add a cloud dependency based on multilingual averages. |
| VibeVoice-ASR / BitNet | 2026 system unifies long-form ASR, timestamps, and diarization; July 2026 BitNet report claims a ~1.6 GB CPU model and real-time inference, but neither report demonstrates `ckb`. [VibeVoice-ASR](https://arxiv.org/abs/2601.18184), [BitNet report](https://arxiv.org/abs/2607.21075) | Watchlist. Benchmark only when weights/license/runtime and Sorani behavior are verified. |
| FLEURS-Kobani | New Northern Kurdish (`kmr`) corpus, not Central Kurdish (`ckb`). [Paper](https://arxiv.org/abs/2603.29892) | Useful as a dialect/OOD stress set with explicit tags; never mix silently into `ckb` gold. |
| KUTED | Central Kurdish text paired with 170 hours of **English** audio for speech translation. [Paper](https://arxiv.org/abs/2604.00613) | Useful for orthography/text research, not as Sorani acoustic ASR training data. |

The rule is simple: a release date and a multilingual headline are not evidence for Sorani.

## 8. Finish distribution, dependency, and dataset interoperability

### Dependency integrity

The current `npm ls --all` exits with `ELSPROBLEMS`: `svelte-check` → `fdir@6.5.0` requires `picomatch ^3 || ^4`, while the installed/locked tree resolves that edge to `picomatch@2.3.2`. Builds may still pass, and the custom lockfile SBOM avoids relying on the invalid installed tree, but the dependency graph is not clean.

Fix the lock resolution, then gate CI on:

```text
npm ci
npm ls --all
npm audit --omit=dev
npm sbom --sbom-format cyclonedx
```

Keep the custom SBOM as an independent cross-check until the native command is stable.

### Updates and platforms

Continue publishing Windows only until the supported runtime, installer signing, model installation, backup/restore, and end-to-end workflow are proven on each additional platform. “Builds on macOS/Linux” is not the same as “the product works there.”

After the owner has a durable signing/key-management process, add the Tauri v2 updater with a staged channel, explicit release notes, manual install confirmation, and rollback instructions. Tauri requires signed updater artifacts and HTTPS in production; the updater signing key is an additional long-lived secret, not a substitute for Windows Authenticode. [Tauri v2 updater documentation](https://v2.tauri.app/plugin/updater/)

### Dataset interoperability

Add `croissant.json` to production-ready bundles alongside the existing dataset card and manifests. MLCommons Croissant 1.0 describes resources, record structure, hashes, licenses, and ML semantics, and is already integrated by Hugging Face, Kaggle, Google Dataset Search, OpenML, and TFDS. [MLCommons Croissant](https://docs.mlcommons.org/croissant/)

Generate it from the same filtered export plan as every other artifact so revoked or held-out rows cannot reappear through metadata. Validate the JSON-LD in the bundle gate.

## What not to build now

- Do not replace OmniASR with Qwen3, Voxtral, or VibeVoice based on aggregate multilingual marketing.
- Do not add an LLM “judge” as gold truth or let two related models count as independent proof.
- Do not auto-accept from high agreement alone.
- Do not mix Northern Kurdish data into Central Kurdish evaluation without explicit dialect identity.
- Do not train on KUTED audio as though it were Sorani speech; it is English audio.
- Do not make cloud ASR load-bearing in the default path.
- Do not add a custom CTC beam/KenLM decoder until the clean benchmark shows the fallback decoder—not the acoustic model or data—is the limiting factor.
- Do not rewrite Tauri/Svelte/Rust. Bound the data paths and split responsibilities incrementally.
- Do not add more dashboards before the app measures whether users transcribe, review, and export faster.
- Do not turn on automatic updates until both installer and updater key custody are operationally proven.

## Twelve-week execution order

### Weeks 1–2: truth and identity

- Rebuild the unique FLEURS manifest and same-normalizer paired harness.
- Define the in-domain/stress manifest schema and overlap checks.
- Add first-class recording identity design and migration tests.
- Repair the npm dependency tree and make `npm ls --all` a gate.

**Exit:** one command produces a reproducible incumbent scorecard whose rows, normalizer, model, and hashes are all explicit.

### Weeks 3–4: trustworthy routing

- Add separate risk fields and reason codes.
- Make missing/heuristic confidence fail visibly closed.
- Add reliability, selective-risk, and conformal coverage reports by condition.

**Exit:** the app can explain why every clip was auto-accepted or sent to review, and the explanation is backed by stored evidence.

### Weeks 5–6: label-efficient review

- Replace the full-table active-learning path with a bounded SQL candidate pool.
- Add metadata diversity and per-recording/speaker caps.
- Run a baseline-versus-hybrid review experiment.

**Exit:** report held-out error reduction per review minute, not just clips completed.

### Weeks 7–8: workflow simplification

- Add first-run readiness and a “what next?” home state.
- Make Review Mode actions sticky and surface risk reasons.
- Add local funnel/session metrics and accessibility regression tests.

**Exit:** representative users reach a trustworthy export faster without more incorrect accepts.

### Weeks 9–10: model bakeoff and scale

- Benchmark OmniASR CTC v2 300M/1B candidates.
- Add 100k/1M library query and cancellation benchmarks.
- Retire or bound remaining whole-corpus IPC paths.

**Exit:** fallback choice and scale claims are measurements, not assumptions.

### Weeks 11–12: distribution and reuse

- Add Croissant metadata to the filtered bundle.
- Exercise signed install/update/rollback in a staging channel if signing custody is ready.
- Run a clean-machine Windows workflow from import through production export and restore.

**Exit:** a public artifact is signed, attestable, recoverable, and its dataset output is independently understandable.

## Final decision standard

The app is materially better only when one of these moves in the right direction on real data:

- paired CER/WER with clean, unique, same-basis evaluation;
- worst-slice error and calibration;
- error reduction per human review minute;
- median time to first successful export and median seconds per review;
- incorrect accept, undo, abandoned-draft, and export-blocker rates;
- startup/search/queue/export latency at realistic corpus size;
- crash, recovery, model-fallback, and update/rollback success rates.

Everything else is machinery. Useful machinery is worth building, but Cortex should only claim an outcome after the corresponding measurement exists.
