# TTS qualification safety boundary

This change contains unsafe gold claims. It does **not** implement or certify the
missing perceptual listening workflow. Transcription consensus, a turn-change
score, successful WAV decoding and a folder named “gold” are insufficient.

## Export schema 3

- `asr/` keeps the existing fully resolved human transcript and exact audio contract.
- `tts/metadata.jsonl` and `tts/audio_24k/` are empty until an authenticated,
  audio/text-bound quality authority is implemented. There is no opt-in bypass.
- `tts_candidates/` contains only retained masters passing the existing calibrated
  turn-change screen. These are **unqualified**, not training-ready TTS.
- Each candidate binds its exact output/master SHA256, source span, verbatim text
  SHA256 and resolution-evidence SHA256. It states `goldTtsEligible=false`,
  `qualificationStatus=pending`, and names the missing checks.
- `exclusions.jsonl` explains every retained clip's exclusion from gold TTS.
  Passing the turn screen produces `tts_quality_qualification_missing`, not an
  invented human rejection. Low/unmeasured scores keep their existing reasons.
- Result, manifest and certificate distinguish zero `ttsRetainedSegments`, all
  retained clips as `ttsExcludedSegments`, and `ttsCandidateSegments`.
- Schema-3 certificate validation refuses positive gold counts/status, missing
  candidate counts, and counts outside the retained population, even if the JSON
  is rehashed. Old schema-2 evidence remains readable, but does not qualify TTS.
- `pool_admin certify` separates `asrDatasetReady` from `ttsGoldReady`;
  `finalDatasetReady` cannot turn green while gold qualification is unsupported.

Schema 3 changes the **export artifact**, not database schema or paid review
semantics. Older readers may refuse new certificates; do not downgrade an
operator to one that cannot read an already-published artifact. Existing
certificates/artifacts are immutable: no in-place relabeling or rewriting. Future
TTS qualification must be a separately versioned, evidence-bound generation
derived from the frozen reviewed source, not a mutation of an ASR certificate.

The reviewer queue retains its calibrated screen/order. The legacy function name
`tts_admission` remains for that queue preference; its documented meaning is a
turn-change screen, not a clean-speaker certificate. No jobs, judgments, payroll,
thresholds, models or original audio are changed by this boundary.

## Audio audit

```text
python cortex-speech-app/scripts/audit_tts_clip_quality.py <prepared-wavs-folder> --out <new-report.csv>
```

The audit now:

- emits `hold` or `pending_qualification`, never `keep`/gold;
- preserves missing SNR/speaker evidence and rejects nonfinite thresholds;
- flags constant/silent audio and keeps exact clipping counts/fractions;
- reports every WAV, including corrupt, truncated and unsupported files;
- does not downmix stereo into a potentially misleading mono signal;
- rejects duplicate metadata identities instead of choosing the last score;
- requires metadata to reference the requested folder and existing files;
- refuses to overwrite previous reports.

Exit 0 means **measurement completed**, not “safe for training.” Exit 2 indicates
unreadable audio or invalid metadata. All rows explicitly state gold is false.
The existing SNR-style measurement is an amplitude-range proxy, not calibrated
noise, overlap, speaker-identity or perceptual quality evidence.

## Metadata separation

```text
python cortex-speech-app/scripts/partition_tts_metadata.py --folder <prepared-wavs-folder> --output <new-audit-directory>
```

Every input row is conserved once. All members of a duplicate path group go to
`quarantine_metadata.csv`; no winner is selected from duration or similarity.
Missing/out-of-scope paths also go to quarantine. Unambiguous rows go to
`unambiguous_metadata.csv` with current audio hashes, but are still NOT gold.
The sealed manifest records original metadata hash, source root, counts and
output hashes. Originals are untouched; these outputs do not edit pool membership.

## Remaining qualification work

Before any gold admission can be enabled, implement and prove:

1. A real human quality decision for target speaker, no secondary speech,
   acceptable audio, and complete verbatim text/word boundaries. Bind it to the
   exact clip hash/span and resolved text/revision. Missing, stale, reversed or
   conflicting evidence must refuse admission.
2. Agreement on who performs this additional listening and its compensation.
   Do not silently convert transcript reviews into unpaid extra work or reprice
   completed decisions.
3. Locally calibrated overlap screening on separate held-out human-labeled
   difficult examples. Do not reuse a turn-change calibration as overlap proof.
4. Source-episode reconciliation for ambiguous metadata; episode/session and
   duplicate-family grouped splits before training. Exact-PCM dedup alone does
   not resolve shifted/re-encoded speech duplicates.
5. Any re-cut/denoised source must receive a new identity and requalification.
   Existing immutable reviewed clips and paid judgments stay unchanged.

Tests: `test_audit_tts_clip_quality.py` and `test_partition_tts_metadata.py` are
discovered by the mandatory Python-policy gate in Verify-10/CI. Rust export
tests cover real candidate/empty-gold artifacts, exact audio preservation,
deterministic retries, crash recovery, source drift and forged certificates.
This is safety containment, not a finished gold-data certification feature.
