# Routed queue verification and recovery

## Incident

The compact frontend PR #115 passed its four required checks and merged. Its
private-production rollout was nevertheless refused before candidate activation.
The release verifier compared two different queue definitions: the admin
benchmark counted the unrestricted pool, while the audio probe correctly applied
the live shared-reopen routing policy. The same mismatch prevented the original
recovery path from clearing maintenance on the previous release.

This was a real availability interruption, not evidence of an audio-file failure.
The previous release was restored without restoring the database. A read-only
comparison against the pre-handover snapshot found no missing or changed prior
review decisions, review events, or payment-ledger rows. Existing public reviewer
links authenticated after recovery. The compact frontend was not activated by
that failed attempt.

## Corrections

- `pool_admin probe` and `benchmark` use one canonical reviewer queue resolver,
  including dialect, listen-list, difficulty, legacy-redo and shared-reopen rules.
  The benchmark now times the same resolution path it counts. Both reports carry
  the explicit `routed-reviewer-v1` queue authority marker.
- Clone preflight copies every relevant reviewer-routing profile and proves the
  reviewer queues, WAV sample and submission-idempotency authority **before** the
  live maintenance phase. Structural database/audio certification alone was not
  enough to establish serving-path compatibility.
- Recovery of a validated previous release has a narrow compatibility rule for
  historical, unmarked benchmarks: a smaller, nonempty shared-reopen routed queue
  may be accepted when the canonical probe still passes audio and idempotency
  checks. The latency gate remains mandatory. New candidate deployments never
  receive this exception; their benchmark and probe counts must match exactly.
- Modern authority-marked benchmark mismatches remain failures, even in recovery.
  Empty/malformed/increased probe counts, absent routing, and failed audio or
  idempotency evidence do not qualify for the legacy compatibility rule.

## Regression evidence

The new admin fixtures reproduce the old unrestricted/routed mismatch, preserve
the held and final reviewers' access, exclude unrelated reviewers, reject malformed
legacy-redo policy, and preserve valid empty dialect-restricted queues. The first
fixture revision lacked a retained pool opinion; it was corrected to record a
real synthetic fixture opinion before reopening. The revised full admin suite
passes 41 tests.

The release-controller suite passes 41 tests, including strict deployment refusal
versus narrowly allowed legacy recovery, copied profile fidelity, unchanged source
databases, all supported clone migration boundaries, and routed count mismatch
refusal during preflight. Private logs retain the initial failure and subsequent
passing runs; these are not field acceptance or a full dataset-quality certificate.

The subsequent deployment must still build the exact merged revision, pass clone
preflight, preserve a certified snapshot, prove the live serving page and existing
reviewer links, and receive separate physical-phone acceptance.

The full local policy sweep also exposed an independent Windows test-fixture race:
killing a virtual-environment launcher did not establish that the actual Python
process owning the synthetic mutex had exited. The fixture now launches the base
interpreter directly and asserts its reported PID matches the process handle it
kills and waits on. Twenty consecutive crash/reacquire checks passed. Production
mutex semantics, timeouts and exclusion rules are unchanged.
