# Safe preparation for owner-directed re-review

This is preparation tooling and incremental redo-queue hardening, not a completed general reopen
workflow. It does not authorize production activation, change payment, or put any clip into gold.

## Read-only inventory

Run from `cortex-speech-app`, using the project's locked Python interpreter:

```text
python scripts/prepare_review_reopen.py --db <existing-library.db> --reviewer <name> --reviewer <name> --output <new-private-inventory.json>
python scripts/prepare_review_reopen.py --db <existing-library.db> --reviewer <name> --reviewer <name> --verify-against <saved-private-inventory.json>
```

The database is opened with SQLite URI `mode=ro` and `query_only`, inside one explicit read
transaction. Output uses exclusive creation and cannot overwrite a prior inventory. A missing
database, missing schema, ambiguous pool, or registry/member mismatch fails rather than inventing
an empty result. Neither invocation has an apply mode. Store inventories privately, not in Git.

The inventory preserves four separate facts:

- A recorded `requested_action=accept` is a Looks Good submission, even if its semantic action is edit.
- A semantic accept does not prove that button was used; unchanged Save & next can also be accept.
- Historical activity does not prove the currently effective opinion or approved text. Clip revision
  and current text hashes are retained, and unknown current-button provenance is explicit.
- Retired duplicates and out-of-pool history remain visible, but are never added to the selected
  retained set, automatically mapped to another clip, or summed as new payable work.

Per-reviewer counts may overlap. Use the distinct segment-ID union, not their sum, for a common
pool. Keep edits-later and unknown-button groups distinct. An old Looks Good submission may already
have a later correction; never restore old text just because its historical event selected the clip.

The inventory hash binds pool identity, selection, history, current revision/text, schema and other
reviewers' effective evidence/adjudications on the affected retained clips. Verification refuses
tampering and drift. Verification is advisory freshness evidence, not an atomic apply boundary;
the eventual writer must recheck inside its own transaction.

## Queue hardening included here

`review_redo::pending_segment_ids` reuses ordinary pool eligibility for returned pool opinions.
That includes family exposure, legacy opinions, resolution, audio and dialect checks. It no longer
mistakes two disagreeing opinions for a resolved clip. A new matching pair removes the clip from
the returned reviewer's queue, matching the decision writer. Earlier reversals do not automatically
enter a later timestamp-defined redo round. A canonical skip is not completed redo work.

For this interim timestamp policy, `started_at_ms` must be fixed **before** the reversals belonging
to that round. Generating a newer start after performing the reversals intentionally excludes them;
do not reset the timestamp to repair a queue without checking the exact intended scope.

Tests are `test_prepare_review_reopen.py` (automatically discovered by the existing Python policy
runner), `redo_reversal_reuses_consensus_authority_and_is_scoped_to_its_round`, and
`redo_skip_is_not_a_completed_canonical_correction`, plus the existing end-to-end Couch redo test.

## Still required before a general reopen rollout

1. An owner-reviewed, exact manifest and durable round identity; preview/apply must target the same
   frozen evidence, rather than recomputing a changing reviewer-wide filter.
2. Atomic withdrawal of disputed authority, including canonical opinions, from final resolution,
   all export routes, learning/few-shot memory and gold eligibility until re-verification succeeds.
   A queue restriction alone does not do this.
3. A shared reopen set that any eligible reviewer can take, including a previous reviewer. One
   person still contributes at most one current opinion; owner reopening must not fabricate a
   second independent opinion or revive a retired recording family.
4. Round/revision-bound stale-submit and playback protection, durable retries and restart tests,
   explicit skip/defer behavior, and an exact policy for supersession and compensation adjustments.
5. Snapshot/restore proof, production-size clone verification, review-link checks and an approved
   rollout window. Keep a single production writer. Do not run the current bulk send-back apply as
   a shortcut around these requirements.

Repeated ordinary phone undo during the same timestamp interval is not yet distinguishable from
owner send-back intent. Timestamp filtering alone is not a durable reopen-round design.
