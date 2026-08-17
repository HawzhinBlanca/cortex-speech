# Gold Dataset Production Audit — 2026-08-12

**Audit snapshot:** 2026-08-12 12:09:43 +03:00  
**Repository commit:** `e364bbb` (`fix(couch): 348 reviewer clips served stale machine text from the human-only field`)  
**Scope:** current repository, live local database, reviewer recovery flow, annotation and export code, release documentation, current primary standards, and current Kurdish speech-data sources.

## Executive verdict

**Do not send the current corpus to external reviewers as a production batch, publish it, or use it as a claimed gold fine-tuning dataset yet.** The app has strong personal-use engineering and several valuable safeguards, but the data contract is not production-gold.

The hard facts at this snapshot are:

| Gold release condition | Current evidence | Verdict |
|---|---:|---|
| Rights complete for every distributable utterance | 0 / 494 | **Stop-ship** |
| Strictly adjudicated gold transcripts | 0 / 494 | **Stop-ship** |
| Independently double-annotated items | 0 / 494 | **Stop-ship** |
| Accepted items unaffected by the stale reviewer-serving path | 9 / 45 exactly match the current raw champion; 36 / 45 do not | **Re-review required** |
| Non-empty leakage-safe train/validation/test partitions | Current connectivity collapses the three recordings into one component | **Stop-ship** |
| Frozen, reproducible review snapshot | 349 / 494 still pending and the app is actively changing | **Not frozen** |
| Dialect parity within the current 10-point budget | 14.79-point WER spread | **Fails intended broad-dialect claim** |

The existing production-bundle rights gate is doing the correct thing: it should block the current data. Do not weaken or bypass it. The green `33/33` status documents strong scoped app machinery; it is not proof of gold data quality. That same status explicitly leaves independent agreement, dialect fairness, and the 500-item Gold Marathon owner-gated, while distribution is owner-descoped.

**A real 10/10 is a release contract, not a score.** It means every row is traceable, licensed for the declared use, independently verified to the declared quality level, split without leakage, reproducible from an immutable snapshot, and delivered with enough evidence that a third party can audit those claims.

## What is already strong

- The app retains the original model transcript, refined text, alignment, source audio identity, and human review state instead of overwriting everything into one opaque field.
- Audio fingerprints and source-content hashes are present on all 494 current segments.
- Current alignments are parseable, finite, ordered, and bounded in an independent structural scan of 10,077 word items. All current rows report `ctc_forced` alignment.
- Rejected rows and obvious placeholders are excluded from the training-grade path.
- The production bundle has a rights-completeness blocker and produces `SHA256SUMS`.
- Public reviewer links remove the credential from the URL, exchange it for an HttpOnly/SameSite cookie, return `401` for an unauthenticated queue request, and do not leak queue data through the public shell.
- Commit `e364bbb` repaired 348 untouched rows that had stale machine text in the human-only field. The new reviewer-serving provenance check passes: populated human text now requires human evidence, and untouched rows serve the current raw champion.
- The repository contains a useful dialect scorecard and does not hide its present disparity.

These are meaningful foundations. The remaining problems are mostly about the semantics and evidence required to call data **gold**, not about cosmetic polish.

## Reality check: the current corpus

### Corpus and review state

| Measure | Snapshot |
|---|---:|
| Segments | 494 |
| Audio duration | 1.246 hours |
| Source recordings | 3 |
| Duration range | 3.020–12.927 seconds |
| Current reviewed/verified rows | 145 |
| Pending rows | 349 |
| Current decisions | 45 accept, 94 edit, 6 reject, 349 none |
| Review events | 189 events covering 189 unique segments |
| Current decision rows with accept/edit/reject | 145 |
| Event/current-row drift | 44 rows |
| Reviewers represented | 4 |
| Reviewer event distribution | 121, 43, 18, 7 |
| Spot-check events | 5 clips, one reviewer |
| Dual-review overlap | 0 clips |
| Explicit `is_gold` rows | 0 |

The app's current quality function nevertheless classifies a single accepted/edited/verified positive as `TRAINING_GRADE_GOLD`. Under that rule, 139 positive rows amounting to about 0.352 hours are called gold. Under a strict production definition—independent annotation plus resolution or adjudication—the count is **zero**.

### Metadata completeness

Present on all current rows: speaker label, alignment JSON, model version, normalizer version, audio fingerprint, source recording content hash, VAD method, and diarization state.

Missing on all 494 current rows:

- declared license;
- consent or other rights basis;
- permitted-use statement;
- authoritative source field;
- persisted split;
- decoder-configuration hash;
- speaker-change score;
- signal-anomaly score;
- posterior/confidence value suitable for cross-row comparison.

The current `audio_content_hash` identifies the source recording, not the individual exported clip; there are only three unique values. A production row needs both the immutable source-recording hash and the exact emitted clip hash.

### Transcript provenance

For all 494 rows, the LLM/refiner output differs materially enough from the raw champion that it cannot honestly be called a deterministic normalizer:

- median character edit distance from raw: **10.9%**;
- 90th percentile: **30.3%**;
- maximum: **60.0%**;
- punctuation-bearing rows: 9 raw versus 321 refined under the independent count.

That is a model-generated transcript candidate, not normalization. It must never silently become reviewer truth or export truth. A normalizer may apply documented, deterministic orthographic transformations; a generative repair model must have its own provenance, confidence, and review state.

### Reviewer-serving incident and affected decisions

Before commit `e364bbb`, 348 untouched rows carried machine text in `annotated_transcription`, a field whose meaning is supposed to be human-authored. The reviewer served that field ahead of the current raw champion. The repair correctly cleared those untouched values without changing the 145 human decisions.

The already accepted set remains contaminated by uncertainty about what the reviewer actually saw:

- 45 current accepts;
- 9 verdict transcripts exactly equal the current raw champion;
- 3 more equal the current refined value;
- 33 match neither current candidate;
- therefore 36 / 45 accepted texts differ from the current champion.

All 45 accepts must be re-queued after the review protocol is fixed. The 94 edits are valuable human evidence, but they still need an independent blind pass before strict-gold promotion.

## Stop-ship findings and exact fixes

### P0 — Rights and intended use are unknown

**Evidence.** Every current row lacks `license`, `consent_basis`, `permitted_use`, and authoritative `source`. A local Hugging Face export can still inherit the settings default `hf_license = "mit"`; software code licensing does not license speech recordings or transcripts. The production bundle correctly blocks because the declarations are absent.

**Why it matters.** A technically excellent label does not make an utterance lawful or ethically usable. Fine-tuning, internal evaluation, redistribution, and publication are distinct uses and may require different permissions. A dataset card cannot manufacture rights that the source did not grant.

**Required fix.** Build a recording-level rights registry, link each utterance to one immutable rights record, and require:

- authoritative source URL or acquisition record;
- source/dataset version and access date;
- content hash;
- copyright or database-rights holder where known;
- exact license/terms version;
- consent or other documented basis where relevant;
- allowed uses: internal training, commercial training, evaluation, redistribution, derivatives;
- attribution/notice obligations;
- geographic/retention restrictions if any;
- revocation/takedown state and a tested deletion path.

**Exit gate.** 100% of candidate release rows resolve to a reviewed rights record that explicitly permits every declared release use. Unknown means excluded. The package license must be derived from compatible row/source rights, never a free-text default.

Current source claims need correction. The project governance file describes AsoSoft, CORDI, and Common Voice too broadly. The primary CORDI page declares CC BY-SA 4.0. Mozilla's current Common Voice 26.0 Central Kurdish page declares CC0 but also explicitly forbids re-hosting or re-sharing that download. Rights must be source- and version-specific, not inferred from a dataset family name.

### P0 — One human action is incorrectly called gold

**Evidence.** `quality.rs` treats `verified`, `is_gold`, accept/edit, or other human-positive state as enough for `TRAINING_GRADE_GOLD`. There is no dual-annotation overlap, no adjudication record, and no current strict-gold row.

**Why it matters.** A single reviewer may be skilled, but their label is not independently validated. Systematic spelling preferences, missed particles, dialect normalization, hallucinated punctuation, or inaudible-region handling can become invisible model targets. Calling this gold also prevents downstream users from separating single-review training data from adjudicated evaluation truth.

**Required fix.** Replace one status with a state machine:

`machine_candidate → single_reviewed → double_reviewed_agree | disputed → adjudicated → release_gold`

Gold promotion must require immutable evidence of two independent assignments or explicit adjudication. Keep single-reviewed material as a useful **silver/verified** tier; do not discard it and do not mislabel it.

**Exit gate.** Every release-gold row has the required annotation event IDs, guideline version, two distinct qualified reviewers, agreement result, and adjudication ID when needed. Evaluation/test gold should be 100% adjudicated. If “highest-grade training gold” is the product promise, apply the same standard to the training gold tier.

### P0 — The 45 accepts need a clean pass

**Evidence.** They were decided during the stale serving period; 36 do not equal the current champion. The event table does not snapshot the exact text displayed to the reviewer, so the original review context cannot be reconstructed reliably.

**Required fix.** Demote the 45 accepts to `needs_revalidation`, freeze the current champion/model provenance, and send them through a new blinded reviewer assignment. Preserve old decisions as historical evidence—never delete or rewrite them.

**Exit gate.** No release-gold row depends only on a decision made before the fixed serving provenance gate. A regression fixture proves the API response, UI text, and stored submission refer to the same immutable candidate ID.

### P0 — Generative refinement is conflated with normalization

**Evidence.** Refined text has up to 60% character distance from raw and is the fallback truth for unreviewed plain exports. The serving incident shows the concrete risk of this ambiguity.

**Required fix.** Store these separately:

- `asr_hypothesis_raw`;
- `model_refinement_candidate` with provider/model/prompt hash/run ID;
- `normalized_deterministic` with profile/version;
- `human_transcript_verbatim`;
- `adjudicated_transcript`.

Effective training text must be an explicit materialized choice with a reason and source event, not a precedence chain over nullable columns. Never export unreviewed model refinement as human truth.

**Exit gate.** A provenance query can answer, for every emitted character, which immutable human/model artifact supplied the released transcript. Re-running the deterministic normalizer produces byte-identical output.

### P0 — The present split strategy collapses the corpus

**Evidence.** Eight diarization speaker labels appear across the three source recordings; seven labels appear in all three. The split code joins a recording basename to the raw speaker label and creates connected components. This makes all three recordings one component, so an 80/10/10 greedy assignment places the only component in train and leaves validation/test empty.

**Why it matters.** Generic diarizer labels such as `SPEAKER_00` are recording-local, not global identities. Treating them as global creates false cross-recording links. Conversely, basename-based recording identity can miss the same content under another filename or merge unrelated files with the same basename.

**Required fix.** Use `source_recording_sha256` as the recording identity. Namespace local diarization IDs by that hash. Only use a cross-recording `global_speaker_id` after verified identity resolution. Add perceptual/near-duplicate audio grouping. Partition the connected groups, then assert all requested splits are non-empty and approximately meet size/duration targets. With only three recordings, collect more independent recordings before claiming a trustworthy three-way benchmark.

**Exit gate.** Zero source-hash, clip-hash, near-duplicate-series, or verified-speaker overlap across splits; non-empty train/validation/test; split report includes counts, hours, speakers, sources, dialects, domains, and leakage-check results.

### P0 — Annotation lineage is not transactionally reproducible

**Evidence.** The review-event schema stores segment, reviewer, action, source, and time. It does not snapshot submitted text, displayed candidate ID/hash, audio hash, guideline version, reject reason, assignment, model provenance, or adjudication. The event is written best-effort after the mutable decision. There are 189 current events but only 145 current accept/edit/reject rows, a 44-row state/event difference.

**Required fix.** Make annotation events append-only and authoritative. In one database transaction, write an immutable event containing:

- event/revision ID;
- assignment and round;
- pseudonymized reviewer ID and qualification version;
- exact clip SHA-256 and source/offset identity;
- exact candidate ID and displayed-text hash;
- submitted verbatim transcript and its hash;
- action and structured reason codes;
- guideline/schema versions;
- playback evidence and active-review duration;
- client/server timestamps;
- supersedes/voids relation;
- app commit and reviewer-build version.

Materialized current state should be rebuildable from events. Gold promotion and its evidence must commit atomically.

**Exit gate.** Rebuilding a blank database from the event log produces byte-identical current decisions and release rows. No current gold row can exist without its complete evidence chain.

### P0 — The reviewer protocol is too thin for gold work

**Evidence.** The remote UI says essentially “listen and correct.” It does not show a versioned annotation law, require playback before acceptance, capture reject/skip reasons, enforce blind independence, or clearly distinguish verbatim transcription from normalization. Buttons are immediately usable. The detailed local guideline is not embedded and contains a risky instruction allowing unintelligible material to be “edited out,” which can create an audio/text mismatch.

**Required fix.** The reviewer task must:

1. show a compact, versioned verbatim rule and examples;
2. require meaningful playback before accept/edit/reject;
3. require structured reject/skip reasons;
4. prohibit deleting spoken content; use a standardized uncertainty token or reject according to policy;
5. hide other annotators and, for blind pass B, hide machine text entirely or randomize controlled assistance;
6. autosave safely but expire/clear local reviewer data;
7. surface clip boundaries and allow replay at reduced speed;
8. periodically insert adjudicated qualification/control items;
9. capture whether the reviewer heard overlap, noise, code-switching, truncation, PII, or speaker change.

**Exit gate.** A reviewer cannot submit a gold-eligible decision without passing the qualification set, opening the current guide version, playing the audio, and providing all required fields. Control-item performance and review-time anomalies are monitored without treating speed alone as guilt.

### P0 — The current snapshot is unfinished and moving

**Evidence.** 349 / 494 rows are pending, the application is active, and commits changed during this audit. The status document was generated at an older commit than the current reviewer-serving fix.

**Required fix.** Create a release candidate snapshot: database backup, commit SHA, schema version, model/prompt hashes, normalizer version, rights-registry version, review-guideline version, and manifest checksums. Review and adjudicate the frozen candidates, then emit a new immutable version when anything changes.

**Exit gate.** Re-running the release builder from the frozen inputs produces identical hashes; the release report names no mutable `latest` dependencies.

## High-priority quality gaps

### P1 — Coverage is too narrow and skewed

There are only 1.246 hours from three recordings. The two largest inferred speakers hold 76.7% of duration (45.0% and 31.7%); the largest recording holds 87.6% of all audio. Speaker labels are not verified global identities. There is no row-level dialect, domain, channel, demographic, or environment metadata.

The current model scorecard also shows a 14.79-point WER spread across six varieties: Mehabad 27.63, Silemani 29.81, Kalar 31.24, Sine 36.31, Hewler 39.19, and Serdest 42.42. The intended budget is 10 points, and the strongest/weakest confidence intervals do not overlap. This does not make the corpus useless; it means the product cannot honestly claim strong broad-variety performance yet.

**Fix.** Define the intended-use population first. Collect by a coverage matrix—dialect/region, domain, speaker, channel, acoustic condition, speech style, code-switching—and publish missing cells. For evaluation, choose sample sizes from desired confidence-interval precision rather than a magic row count. A practical internal target is no dialect/domain WER confidence-interval half-width above 2.5 points and no material gap above the declared budget, unless explicitly documented as an excluded use.

### P1 — Audio and boundary quality are under-measured

All rows are marked denoised false and diarized true, but persisted perceptual and structural quality fields are largely absent. There is no stored SNR/SI-SDR proxy, clipping, DC offset, speech ratio, overlap ratio, UTMOS/SIGMOS-like score, speaker-change score, truncation score, or robust anomaly score.

**Fix.** Measure rather than infer quality from one alignment score. Add deterministic decode/format checks, VAD speech ratio, leading/trailing silence, clipping, loudness, bandwidth, overlap, speaker change, and boundary truncation. Use perceptual estimators only as triage signals, never as proof of semantic correctness. Review risk strata and compare error rates by quality bucket.

### P1 — Alignment validation is too permissive

Current data passed an independent structural scan, which is good. However, the ingestion validator only verifies that alignment is parseable JSON under a size limit; it does not enforce a typed schema, finite numbers, monotonicity, word-text correspondence, or bounds against clip duration.

**Fix.** Validate a versioned alignment schema at every merge/import/export boundary. Reject NaN/Infinity, negative or decreasing timestamps, out-of-bounds spans, empty words, impossible confidence values, and mismatched normalized token sequences. Persist the aligner model/config hash.

### P1 — Agreement measurement does not measure transcript gold

The current IAA script reduces annotations to `accept/edit/reject` and computes unweighted Cohen's kappa, with no confidence interval. The five spot checks come from one reviewer and do not form an independent truth set. Action kappa measures triage agreement, not transcription accuracy, and can be distorted by category prevalence.

**Fix.** For Sorani transcription, make pairwise and adjudicator-referenced **CER** the primary text metric, with bootstrap confidence intervals; report WER as a secondary, tokenization-sensitive measure. Separately measure categorical tag/reason agreement with an appropriate chance-corrected statistic and confidence interval. Report disagreement by dialect, reviewer pair, acoustic quality, and assistance condition. Never describe kappa as a ceiling on model accuracy.

**Proposed internal promotion target.** On a representative audited sample, the upper 95% confidence bound for residual adjudicated CER is at most 1.0%, and the lower 95% confidence bound for required categorical agreement is at least 0.80. These are proposed product thresholds, not universal scientific laws; calibrate them to intended use and annotation difficulty.

### P1 — Public reviewer hardening is incomplete

The token exchange design is materially better than a token that stays in the query string. Remaining production issues include a cookie without `Secure`, no observed Content-Security-Policy or defense headers, one-year immutable private caching for sensitive audio, and transcript drafts/outbox in `localStorage` without a visible expiry/clear-data control. A current-run language switch changed the document language to English but left the existing expired-link alert in Kurdish; direct `?lang=en` recovery rendered correctly.

**Fix.** Require HTTPS and `Secure`; add CSP, `X-Content-Type-Options`, frame-ancestors, restrictive Referrer-Policy and Permissions-Policy; shorten/no-store sensitive audio caching; expire and clear local drafts on submission/session end; test every dynamic string after language changes. Threat-model shared and lost phones, not only network attackers.

### P1 — Packaging is not yet a production data product

Plain JSON/JSONL/CSV/Parquet export can include unreviewed non-rejected rows; a `trainingReady` flag does not stop accidental misuse. The production bundle contains all remaining rows rather than an explicit gold-only manifest. Dataset version `2.0` is hardcoded instead of derived from an immutable release. There is no Croissant metadata file.

**Fix.** Separate exports by guarantee:

- `candidates/` — restricted, non-training research material;
- `silver/` — single-reviewed material;
- `gold/` — independently verified/adjudicated material;
- `evaluation/` — sealed benchmark with stronger contamination controls.

Default training manifests must contain only eligible rows. Any mixed/raw export must be visibly named and require an explicit unsafe flag. Use semantic dataset versions and publish lineage, checksums, metrics, coverage, known limitations, and change history.

## The strict gold annotation protocol

1. **Freeze the task law.** Define verbatim orthography, punctuation, numerals, filled pauses, repetitions, code-switching, named entities, uncertainty, overlap, truncation, and non-speech. Version it and include positive/negative examples in Kurdish.
2. **Create an adjudicated qualification set.** Build it from difficult, representative clips. Reviewers must pass by CER and required tag accuracy, not just a multiple-choice quiz.
3. **Independent pass A.** Reviewer receives a stable clip/candidate ID. Machine assistance, if used, is logged as an experimental condition.
4. **Independent blind pass B.** A different qualified reviewer receives the same audio without pass A's result. For the highest-confidence estimate of human transcription, hide machine text.
5. **Compute disagreement.** Normalize only for scoring through a documented evaluation normalizer; preserve the verbatim originals. Use CER primary, WER secondary, and separate categorical agreement for reasons/tags.
6. **Adjudicate disputes.** A senior adjudicator listens to the audio and creates a resolved transcript with a reason. Do not mechanically vote between text strings.
7. **Audit agreements too.** Sample clips where A and B agree, stratified by dialect, speaker, source, duration, confidence, and acoustic risk; correlated errors can agree.
8. **Promote atomically.** Gold status, chosen transcript, evidence IDs, rights record, quality record, and split identity are committed together.
9. **Freeze and reproduce.** Generate a new immutable release and run downstream loading/training smoke tests against the packaged files.

For budget-constrained work, use silver for most training and reserve the strictest protocol for evaluation plus a representative high-value training subset. But if the release is marketed as “highest-grade gold,” do not redefine one pass as gold to increase the count.

## Recommended row contract

Use JSONL or Parquet for the rich canonical manifest and generate minimal framework-specific views. A production row should resemble:

```json
{
  "schema_version": "1.0.0",
  "dataset_version": "2026.08.1",
  "utterance_id": "ckb_<stable-id>",
  "audio": {
    "path": "audio/ckb_<stable-id>.flac",
    "sha256": "<exact-emitted-clip-sha256>",
    "source_recording_sha256": "<immutable-source-sha256>",
    "offset_ms": 120340,
    "duration_ms": 6840,
    "sample_rate_hz": 16000,
    "channels": 1,
    "codec": "flac"
  },
  "text": {
    "verbatim": "<adjudicated Sorani transcript>",
    "normalized": "<deterministic training view>",
    "normalization_profile": "ckb-train-v1.0.0"
  },
  "language": {
    "bcp47": "ckb-Arab",
    "dialect": "<controlled value or unknown>",
    "script": "Arab"
  },
  "speaker": {
    "local_id": "<source-hash>:SPEAKER_00",
    "global_id": null
  },
  "content_tags": {
    "overlap": false,
    "code_switch": false,
    "pii": false,
    "truncated": false,
    "reject_reason": null
  },
  "quality": {
    "speech_ratio": 0.91,
    "clipping_ratio": 0.0,
    "speaker_change_score": 0.01,
    "boundary_risk": 0.02,
    "quality_profile": "audio-qc-v1.0.0"
  },
  "annotation": {
    "status": "adjudicated_gold",
    "guideline_version": "ckb-verbatim-v1.0.0",
    "annotator_count": 2,
    "event_ids": ["ann_a", "ann_b"],
    "adjudication_id": "adj_123",
    "pairwise_cer": 0.0
  },
  "provenance": {
    "segmentation_run_id": "seg_run_123",
    "asr_run_id": "asr_run_456",
    "refinement_run_id": null,
    "alignment_run_id": "align_run_789",
    "decoder_config_sha256": "<hash>",
    "app_commit": "e364bbb"
  },
  "rights": {
    "record_id": "rights_123",
    "license_spdx": "<reviewed value>",
    "consent_basis": "<documented value>",
    "permitted_uses": ["internal_finetuning"],
    "source": "<authoritative record>",
    "revoked_at": null
  },
  "split": "train"
}
```

Keep the append-only annotation/adjudication event log in a restricted package with pseudonymous reviewer IDs. Do not publish reviewer names or operational secrets in public rows.

## Recommended release package

```text
dataset-2026.08.1/
├── README.md                 # dataset card, intended use, limits, metrics
├── DATA_USE.md               # exact rights, obligations, prohibited uses
├── ANNOTATION_GUIDE.md       # exact frozen reviewer law
├── CHANGELOG.md
├── croissant.json            # schema, files, provenance, usage conditions
├── SHA256SUMS
├── manifests/
│   ├── train.jsonl           # minimal audio_filepath/text/duration view
│   ├── validation.jsonl
│   ├── test.jsonl
│   └── metadata.parquet      # rich canonical metadata
├── reports/
│   ├── quality.json
│   ├── agreement.json
│   ├── coverage.json
│   ├── fairness.json
│   ├── leakage.json
│   └── rights.json
└── audio/
```

This follows current framework expectations—NeMo's JSON-line `audio_filepath`, `text`, and `duration` manifest—while keeping richer metadata available. MLCommons Croissant 1.1 adds machine-readable file hashes, semantic versioning, W3C PROV-O provenance, and ODRL-compatible usage conditions.

## Required automated release gates

| Gate | Pass condition |
|---|---|
| Rights | 100% declared and compatible with intended use; no unknown/revoked row |
| Gold evidence | Every gold row has complete independent-review/adjudication lineage |
| Serving provenance | Displayed candidate, submitted text, event, and chosen transcript share immutable IDs/hashes |
| Transcript integrity | Nonblank, no placeholders, valid script/profile; deterministic normalization reproducible |
| Audio integrity | Every file decodes, hash matches, duration/format agree, no missing payload |
| Alignment | Typed schema, finite/monotonic/in-bounds, aligner/config hash present |
| Boundary/audio quality | No unresolved clipping, truncation, multi-speaker, overlap, or anomaly policy violation |
| Agreement | CER and tag agreement meet declared thresholds with confidence intervals |
| Coverage | Intended-use cells meet minimum counts/hours and precision targets; missing cells disclosed |
| Fairness | Dialect/domain gaps meet declared budget or intended use is narrowed |
| Leakage | Zero source, exact/near-duplicate, or verified-speaker overlap across partitions |
| Split viability | Train, validation, and test are all non-empty and useful |
| Privacy | PII/biometric-use review complete; public metadata pseudonymized |
| Reproducibility | Frozen inputs regenerate byte-identical manifests and checksums |
| Usability | NeMo/Hugging Face load tests and one small fine-tuning/evaluation smoke run pass |
| Documentation | Card, rights, guide, changelog, quality, fairness, leakage, and Croissant metadata agree |

## Production sequence from here

### Phase 0 — Stop shipment and preserve evidence

1. Do not share the current corpus as gold or production training data.
2. Snapshot the database and commit; preserve the pre-fix decisions/events.
3. Demote the 45 accepts to revalidation and the 94 edits to single-reviewed silver.
4. Remove model refinement from every implicit truth fallback.
5. Complete the recording-level rights registry.
6. Replace basename/raw-diarizer split identity and add non-empty/leakage assertions.

### Phase 1 — Make review scientifically defensible

1. Implement append-only transactional annotation revisions and adjudication.
2. Publish and embed the versioned Kurdish annotation guide.
3. Add playback gating, structured reject/skip reasons, and blind assignment rounds.
4. Build a representative adjudicated qualification/control set.
5. Replace action-only kappa with text CER plus tag agreement and confidence intervals.
6. Re-review all 45 accepts and independently review the 94 edits.

### Phase 2 — Build the data product

1. Add clip hashes, quality features, controlled dialect/domain tags, rights references, and typed alignments.
2. Finish pending annotation only after the fixed protocol is live.
3. Collect more independent recordings and rebalance the intended-use matrix.
4. Emit gold-only framework manifests plus rich Parquet/JSONL and Croissant metadata.
5. Run the complete release gates against one frozen version.
6. Load the package from a clean environment and run a small canary fine-tune/evaluation before reviewer or customer delivery.

## Reviewer-flow audit

| Step | Flow | General health | Evidence and limitation |
|---:|---|---|---|
| 1 | Open reviewer URL without a valid credential | **Privacy pass; recovery partial** | Current-run page leaked no queue/audio/text and the queue API rejected unauthenticated access. Screenshot captured. |
| 2 | Switch the expired-link recovery page to English | **Fail** | The document language changed, but the already-rendered alert remained Kurdish. Static keyset tests do not exercise this state transition. Screenshot captured. |
| 3 | Open the recovery page directly with `?lang=en` | **Pass** | Current-run English message and actions render correctly. Screenshot captured. |
| 4 | Authenticated queue and editing | **Not directly exercised** | Active review credentials were deliberately not inspected or reused. Code, live DB, the post-repair provenance gate, and the unauthenticated privacy test provide supporting evidence, not a full authenticated E2E result. |
| 5 | Decision → gold promotion → release | **Fail** | Live DB and code show single-review gold semantics, incomplete rights, no independent overlap, affected accepts, and a collapsing split. |

## Evidence-based reference points

- [MLCommons Croissant 1.1](https://docs.mlcommons.org/croissant/docs/croissant-spec-1.1.html) — machine-readable dataset structure, per-file SHA-256, semantic versioning, PROV-O lineage, and usage conditions.
- [Croissant Responsible AI extension](https://docs.mlcommons.org/croissant/docs/croissant-rai-spec.html) — structured RAI metadata for data collection, annotation, intended use, and limitations.
- [W3C PROV-O](https://www.w3.org/TR/prov-o/) — interoperable entities, activities, and agents for provenance.
- [NVIDIA NeMo ASR dataset manifests](https://docs.nvidia.com/nemo/speech/nightly/asr/datasets.html) — one JSON object per audio sample with audio path, text, and duration.
- [Hugging Face audio dataset loading](https://huggingface.co/docs/datasets/audio_load) and [dataset cards](https://huggingface.co/docs/hub/datasets-cards) — complex audio metadata and discoverable dataset documentation.
- [NVIDIA NeMo Curator quality assessment](https://docs.nvidia.com/nemo/curator/v26.04/curate-audio/process-data/quality-assessment), [quality filtering](https://docs.nvidia.com/nemo/curator/curate-audio/process-data/quality-filtering), and [speaker separation](https://docs.nvidia.com/nemo/curator/v26.04/curate-audio/process-data/quality-filtering/speaker-separation) — current audio-quality, duration, speech, and speaker-risk processing patterns.
- [NIST SCTK](https://github.com/usnistgov/SCTK) — established speech-recognition scoring tools.
- [2025 NAACL multilingual ASR evaluation paper](https://aclanthology.org/2025.findings-naacl.277/) — evidence for CER as a less tokenization-dependent multilingual ASR metric.
- [2026 peer-reviewed speech annotation study](https://www.mdpi.com/2076-3417/16/10/4850) — one recent empirical example combining semantic segmentation, noise/overlap/dialect labels, and staged QA; useful supporting evidence, not a universal prescription.
- [EU AI Act, Regulation (EU) 2024/1689](https://eur-lex.europa.eu/eli/reg/2024/1689/oj?locale=en) — used here only as a data-governance benchmark for provenance, relevance, representativeness, gaps, and bias examination, not as a legal classification of this app.
- [NIST AI Risk Management Framework resources](https://airc.nist.gov/) — governance, measurement, and risk-management reference.
- [CORDI primary dataset page](https://huggingface.co/datasets/SinaAhmadi/CORDI) — source-specific CC BY-SA 4.0 declaration.
- [Mozilla Common Voice 26.0 Central Kurdish](https://mozilladatacollective.com/datasets/cmqino7ym00wqnq072h0gab54) — current version-specific dataset facts and the explicit no-rehosting/no-resharing condition.
- [GDPR Article 4 definitions](https://eur-lex.europa.eu/eli/reg/2016/679/art_4/par_1/oj) — voice is personal data when identifiable; it is “biometric data” in the special technical-processing sense when used for unique identification, so governance language should avoid saying every speech recording is automatically Article 9 biometric processing.

## Bottom line

The app is close to being a strong **annotation workbench**. It is not yet a producer of defensible production-gold data. The most important change is conceptual: stop using `accept/edit/verified` as a synonym for gold. Build an evidence-backed promotion pipeline in which rights, exact candidate lineage, independent review, adjudication, leakage-safe splits, and immutable release metadata are all required before a row can cross the gold boundary.

Until those gates pass, the honest release label is: **private research candidate corpus; not approved for production fine-tuning or redistribution**.
