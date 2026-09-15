# Export batches (schema 73)

Owner 2026-09-15: "add the batch record inside the database too … make sure we dont export earlier
exports, and make sure we record this batch so we dont export again in future".

## What a batch is

`pool_admin export --approved-subset --voice-name <V> --output <new dir> --batch <id> --db <db>`
publishes the approved subset of one voice **minus every clip an earlier batch already delivered with
the same authority**, then records the batch inside the database:

- `review_pool_export_batches` — one row per batch: pool, voice, kind (`approved-subset` | `legacy`),
  manifest and certificate digests, output dir, counts, **the trust policy that decided it** (owner +
  trusted names, JSON + SHA-256), app git sha, creation time.
- `review_pool_export_batch_members` — one row per clip: pool audio identity, the resolution evidence
  digest, the SHA-256 of the exact exported text, and a disposition:
  `exported` | `skipped-previously-exported` | `re-exported-changed-authority`.

Both tables are append-only (triggers refuse UPDATE/DELETE); migration 73 refuses to roll back while a
batch exists; the release controller treats batch history as a restore floor (a rollback that would
drop a batch is refused, like quarantine history); snapshot row counts include both tables in all three
evidence copies (Rust verifier, Python pre-handover writer, restore drill).

## Skip rule (why it is not "by id")

A clip is skipped only when an earlier batch delivered **this exact authority**: the same
`resolution_evidence_sha256`, or — for a legacy artifact recorded without evidence — the same exported
text. A clip whose authority changed since (re-decided, or delivered earlier with different text) is
delivered again and marked `reExportedChangedAuthority: true` in `asr/metadata.jsonl` /
`tts/metadata.jsonl`, `re-exported-changed-authority` in the member row, and counted in the manifest as
`reExportedChangedAuthoritySegments`. Audit 2026-09-15 found 64 clips of the 2026-09-08 TTS test carried
a different text than the canon yields; an id-only skip would have hidden the corrected text forever.

A retry of the same `--batch <id>` reproduces the same artifact (its own earlier record is not "an
earlier batch"); a different artifact under a known id is refused.

## Artifact fields added in 73

`manifest.json` / `certificate.json`: `batchId`, `trustPolicy`, `previouslyExportedSegments`,
`reExportedChangedAuthoritySegments`, and a `transcriptAuthority` sentence that names the trusted rule.
Per clip: `resolutionAuthority` = `owner` | `trusted` | `consensus` | `single`. `exclusions.jsonl` now
also lists training-quarantined clips (`training_quarantine_hold`) and skipped clips
(`previously_exported_by_earlier_batch`).

## Operator commands

- `pool_admin export-batches --db <db>` — every batch, and `deliveredButWithdrawn`: clips delivered
  earlier whose authority no longer stands (unresolved, rejected, held, re-decided). Read this before
  training on old batches.
- `pool_admin record-export-batch … --confirm-legacy-record` — backfill of artifacts made before 73
  (the 2026-09-08 TTS test and the 2026-09-15 approved-v1 batch). Offline (write command).
- `export --batch` refuses to run unless `review_trust.json` sits beside the database: the policy that
  decides the batch must be the live one (audit 2026-09-15 P2-3: a clone dir without it silently trusts
  nobody).

## Procedure for a new batch

1. Clone the live DB (`sqlite3 backup`) into a fresh dir **with the live policy files copied beside it**
   (`review_trust.json`, `review_reopen_routing.json`, `review_routing.json`, `settings.json`).
2. `pool_admin export --approved-subset --voice-name Lamo --output <new dir> --batch approved-v2-<date> --db <clone>`
   per voice. The output dir must not exist beforehand.
3. The batch is recorded in the **clone**. Record it in the live DB with `record-export-batch` using the
   clone's manifest/certificate digests and member list (offline, ~20 s), or run step 2 directly against
   the live DB during a maintenance window. Never train from a batch the live DB does not know.
