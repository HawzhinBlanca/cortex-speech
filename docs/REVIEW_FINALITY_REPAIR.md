# Review finality and recovery

This repair builds on `062e42ab`. It does not delete historical opinions, reverse earnings,
transfer transcripts between audio cuts, or activate a new review round.

## Protected review workflow

- The configured owner's first review is final; configured trusted reviewers also need one
  opinion. Ordinary reviewers still require independent agreement.
- Queue selection and stale-assignment media checks share the same fresh-opinion predicate.
  Renewal checks current audio authority before acknowledging continued work. A refusal stops
  cached phone playback and leaves typed text available for recovery.
- Bulk reopen previews and commits refuse owner-final clips. This is deliberately conservative:
  disputed ordinary work must not silently invalidate the owner's final authority.
- Reopening a retained duplicate-family root does not discard the owner's exposure on a retired
  twin. It does not transfer the twin's transcript: extended cuts can contain different words.
- Owner conflicts with room for an ordinary owner opinion reach the owner under the prior fix.
  Three-opinion conflicts still require the existing explicit administrative adjudication path;
  this repair does not bypass the immutable three-opinion constraint or fabricate a paid fourth vote.

## Export an approved subset

The existing `pool_admin export` remains a complete-voice certification operation by default.
Use `--approved-subset` explicitly to publish only currently resolved clips:

```text
pool_admin export --db <offline-library.db> --voice-name Lamo --output <new-output-directory> --approved-subset
```

Use an offline, backed-up database with its matching trust and routing profiles. The existing
exclusive-instance guard applies. Test with a private clone before any production operation.

The subset uses the same rights, exact audio identity, reject, transcript and TTS-admission gates.
It writes `scope: approved-subset`, `completeVoice: false`, and `pendingSegments` into its
manifest and artifact certificate. It does **not** insert a complete-voice database certificate
or prevent later review. Existing destinations are never blindly overwritten; exact retries are
verified. A changed selection requires a new output directory.

“Review final” is not a promise of gold TTS: speaker changes, overlapping speech, clipping,
boundary errors, consent and train/evaluation split isolation still need their own evidence.
The live audio audit must be assessed before treating an export as training-ready.

## Phone recovery

Acknowledged pool Undo receipts remain reachable after reload for the same reviewer. This is
last-action Undo, not a complete history browser. Malformed receipts fail closed.
This visibility repair is for durable pool receipts; canonical-review Undo
discoverability after a page reload remains a separate UI gap.

If draft storage fails, Skip/Undo do not discard the correction. A reviewer/revision-scoped
in-memory fallback remains available during queue refresh and in the local recovery panel;
leaving the page warns that this copy is not durable. Persist or copy it before closing the tab.
Another reviewer cannot see that fallback. API timeouts include response-body consumption,
and uncertain saves retain their existing operation identity rather than minting a retry payment.

## Acoustic reconciliation, not automatic deletion

The live duplicate audit now includes short phrases as candidates. The immutable historical
manifest builder retains its versioned nomination contract. Audio proof, not matching text,
decides whether a recording may be retired.

The private snapshot rehearsal found eight exported TTS-eligible clips in inconclusive acoustic
groups. Existing signal/speaker gates are not exhaustive duplicate clearance. That rehearsal
artifact is explicitly `DO_NOT_TRAIN`; operator assessment and an explicit export-quarantine
policy remain necessary before treating those clips as training-ready.

Inconclusive groups remain unresolved. Do not raise the baseline, blindly remove clips, infer a
second independent vote from one person's repeated work, or copy an old transcript onto a longer
cut. Preserve original recordings, decision receipts, family lineage and compensation history.

## Regression gates

- `npm run test:reviewer-reliability`: isolated DOM tests; no production HTTP or credentials.
- Rust `review_pool` tests: owner protection, family exposure, subset publication/retry, and a
  single owner's actual export/DPO/LM path.
- Rust `couch::` tests: serving/decision/renewal boundaries, idempotency and reviewer isolation.
- `scripts/test_private_production_release.py`: clone trust-profile byte parity independent of
  the implementation's profile list.
- `scripts/test_dataset_duplicates.py`: short-candidate nomination without weakening audio proof.

Do not equate a green availability probe with zero repeat risk or a fully cleared dataset.
