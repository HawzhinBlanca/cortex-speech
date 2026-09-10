# Training-only acoustic quarantine

Schema 72 adds an explicit safety hold independent of reviewer decisions, pay and review routing.
An inconclusive duplicate/audio assessment is **not** proof that a clip is a duplicate or a bad review.
Owner-final transcripts remain final; quality restrictions still apply before training use.

## Authority and scope

- Holds bind a batch UUID, exact segment ID, canonical source PCM hash, source interval, reason,
  assessment-file SHA-256, and canonical plan SHA-256. Application is atomic and exact-retry safe.
- The original ID and overlapping intervals of the same source PCM are excluded. Adjacent
  non-overlapping intervals are not excluded. Different-hash acoustic similarity is not inferred.
- A distinct manual-clearance record can release one hold. It cannot erase history, release other
  holds, or rewrite the original reviewer opinion. A later new hold requires a new batch UUID.
- Shared dataset/HF/production/audio export selection, DPO/LM, few-shot retrieval and correction-memory
  loading exclude held evidence. Memory tokens with any held contributor are conservatively excluded
  as a whole rather than assigning invented confidence. A held clip can still be reviewed and paid.
- Pool full-voice export refuses certification while any member is held. Approved-subset export
  omits held clips and lists IDs/counts plus the quarantine digest in its manifest; it never certifies
  the complete voice. Already generated artifacts are not retroactively repaired or approved.
- Missing quarantine authority fails closed for training. Upgrade old databases through the normal
  pinned migration flow first. Old binaries must refuse schema 72.
- Restore admission preserves every exact hold and clearance. Schema downgrade refuses to discard
  even cleared history. A raw external replacement of the whole data directory is not an approved restore.

## Offline operator workflow

Only one operator may change the library. Keep Cortex and its watchdog stopped through the existing
maintenance/release procedure. Write commands hold the instance lock and pin a certified backup before
mutation; successful writes use FULL-sync transactions. Never run these against a live reviewer writer.

1. Preserve a private assessment artifact identifying the uncertain clips and its SHA-256.
2. `pool_admin plan-quarantine --db <library.db> --segment-list <ids.json> --reason <reason> --evidence-sha256 <sha>`
   emits a read-only plan. Inspect all exact identities against the assessment; save the JSON privately.
3. `pool_admin apply-quarantine --db <library.db> --manifest <plan.json> --confirm-training-only`
   revalidates identities and applies the complete batch. A stale member rolls back the whole operation.
4. `pool_admin quarantine-status --db <library.db>` reports active blocked IDs, including overlapping aliases.
5. After genuine manual audio assessment, use
   `pool_admin clear-quarantine --db <library.db> --batch-id <uuid> --segment-id <id> --reason <assessment> --evidence-sha256 <sha> --confirm-training-only --confirm-manual-clearance`.
   Keep that new assessment file with the audit. The tool records evidence; it cannot verify human listening.

Deployment, actual hold activation, and acoustic clearance are separate milestones. A test pass or an
approved-subset export is not a claim of a perfect dataset.

The protected release controller accepts `--quarantine-plan <plan.json>` on `stage` and `deploy`.
It applies the same hash-pinned plan first on its disposable clone, then under maintenance before
the new release is exposed. The controller independently hashes every non-hold table before and
after the operation, refusing any change to review, payment, clearance or other library history.
