# Reliability findings — 2026-08-15

Every item below was verified at the source by hand, not taken from a report. Anything I could not
confirm is marked as such. Fixes that need a rebuild are deferred to after reviewer hours.

## Resolved today

### C: drive reached 0 bytes free — the snapshot safety net was off for hours
The owner's separate dataset project (`D:\Hawzhin Kurdish Datasets 2026\From Web`,
`ingest.py aranemini/central-kurdish-audiobook-raw --workers 32`) downloads into the default
HuggingFace cache, which lived on **C:**. It reached 627 GB and filled the system drive at a
measured **121.7 GB/hour**.

Consequence: the 10-minute DB snapshot thread failed every cycle —
`periodic DB snapshot failed: Database error: not an error` — so the only protection for
irreplaceable review labour was silently off while the app still reported healthy.

*Not* corrupted: `PRAGMA integrity_check` = ok, `foreign_key_check` = clean, 14,828 segments,
17,290 hypotheses, 494 verified. The database survived.

**Fixed at the root:** cache moved to `F:\huggingface` (627.4 GB, 0 files left behind, all 18 repos
and the FLEURS eval set intact), `HF_HOME` set persistently, download resumed. Measured afterwards:
F: burns 120 GB/hr while **C: stays flat at −3.9 GB/hr**. C: went 0 → 796 GB free.

### The app was dead and nothing was supervising it
The app reached `RunEvent::Exit` at 11:14:40 (`logs/last-exit.txt` = `orderly exit`) — a clean
shutdown, not a crash. It stayed dead because **CortexWatchdog was left `Disabled`** by the rebuild
procedure hours earlier. Five reviewer links were dead the whole time.

No gate in the 30-plus gate sweep would ever have caught this: every one of them inspects the source
tree, none asks whether the running system can serve anyone.

**Fixed:** watchdog re-enabled; new gate `scripts/check_supervision_live.py` fails on (a) watchdog
disabled or unregistered, (b) live reviewer links not answering on 8737, (c) data drive below 20 GB.
It **failed on the live machine before it passed** — a real fail-before, not a fixture. Nine unit
assertions cover the decision core (`scripts/test_supervision_live.py`), auto-discovered by
`run_python_policies.py`.

### The fuzz harness reported two false crashes
`extended_fuzz.sh` wrote logs to `/tmp`, which Git Bash maps onto C:. When the disk filled, the
redirect failed and every target after it was reported as `CRASH` on the strength of a non-zero exit
code. A harness that cries wolf is as dangerous as one that sleeps.

**Fixed:** logs now live inside WSL, and a crash is only reported when libFuzzer leaves an artifact
or prints a sanitiser banner. Everything else is labelled `INFRA FAILURE — NOT a finding`.

Re-run result, all four targets clean: cache 26 min, diff 20 min, normalizer 20 min, validation
21 min. Zero crashes, zero artifacts. Caveat: the final ~14 minutes overlapped the cache move, so
those targets ran on a busier machine than ideal.

## Open — needs a rebuild, scheduled for after reviewer hours

### CRITICAL: one unauthenticated request from the public internet aborts the whole app
`tiny_http-0.12.0/src/util/equal_reader.rs:66-70`:

```rust
fn drop(&mut self) {
    let mut remaining_to_read = self.size;
    while remaining_to_read > 0 {
        let mut buf = vec![0; remaining_to_read];
```

`self.size` is the client's declared `Content-Length`, parsed into a `usize` with no cap
(`request.rs:149-158`) whenever it exceeds 1024 bytes (`request.rs:215`). A request carrying
`Content-Length: 18446744073709551615` and no body makes this allocate `usize::MAX` bytes on drop →
allocation failure → `handle_alloc_error` → **process abort**. Our 256 KB body cap bounds only what
*our* code reads; the drain runs afterwards regardless.

Reachable from the internet: `tailscale serve status` confirms Funnel is on, and the `*.ts.net`
hostname is published in Certificate Transparency logs the moment Funnel is enabled. Loss = the app,
every reviewer's in-flight verdict, and the desktop session.

**Not fixable from application code.** `body_length()` can see the declared size, but the allocation
happens in the crate's own `Drop`; there is no public `into_reader`, and `into_writer` still drops
the reader. The fix is a patched/vendored `tiny_http` that rejects an oversized `Content-Length` at
parse time. To be proven fail-before against a disposable profile, never against the live instance.

### Two policy tests have never executed a single assertion
Confirmed by running both and diffing defined-vs-called:

* `scripts/test_premium_dataset_policy.py` — `test_alignment_only_review_risk_is_tolerated_but_audio_risk_is_not`
  (6 defined, 5 called)
* `scripts/test_rust_runtime_panic_policy.py` — `test_file_dialog_commands_do_not_block_the_main_thread`
  (80 defined, 79 called)

Each guards a real past incident and each has been reporting PASS while running nothing. This is the
known bug class. The fix is not only to add the two calls but to make the class impossible — replace
the hand-written call lists with the `globals()` discovery loop the other policy files already use.

### The recorded GREEN does not cover the newest gates
`docs/STATUS.md` records GREEN at 33 gates; the sweep now defines more, including the three built for
the three worst incidents (`spot-check-pool`, `review-serving-provenance`, `supervision-live`). The
sweep must be re-run on this HEAD before GREEN means anything about them.

## Reported by audit, verified by me, not yet fixed

* **A half-written decision re-queues the clip and serves the stale draft** (`couch.rs`, `api_decision`)
  — `record_human_decision_by` commits the verdict, then `insert_segment` commits the annotation and
  `verified`. If the second fails, the correction sits in `verdict_transcript` with `verified = 0`,
  `pending_segment_ids` re-queues the clip, and the next reviewer sees the original draft with no
  sign a correction exists.

  **The obvious fix is wrong and was reverted the same day.** Making `review_text` delegate to
  `quality::effective_transcript` (verdict ▸ annotated ▸ raw) looks like deleting a buggy duplicate
  of the verbatim law. It is not: on a SPOT-CHECK clip `verdict_transcript` holds the **answer key**,
  and the listening QC works by serving an already-answered clip with its raw, known-wrong draft.
  That change would have shown every reviewer the answer and auto-passed every check while the QC
  still reported healthy — strictly worse than the bug it fixed. Caught by
  `every_decision_lands_in_the_append_only_audit_trail`, which saw the spot check reclassify from
  "edit" to "accept". A regression test (`the_phone_never_serves_a_spot_check_its_own_answer_key`)
  and a comment on `review_text` now pin the reason, so the same "cleanup" is not attempted again.

  The real fix is to make the two commits ONE transaction, so the half-written state cannot exist.
  That is a deliberate change to the decision path and is not something to rush.
* **`set_api_key` can wipe the other two keys** (`api_keys.rs:105`) — any read failure other than
  not-found substitutes a blank template, which is then written back.
* **Spot-check pairs are never retired** and the grading predicate is weaker than the minting one
  (`couch.rs:1674-1698`), so an un-verified-but-still-human-decided clip can swallow a reviewer's
  real verdict repeatedly, answering `200 {"ok": true}` each time.
* **No `catch_unwind` around couch request handling** (`couch.rs:800-818`) — a panic silently removes
  one serving thread for good; with two or more reviewers the watchdog's probe is answered by a
  survivor, so the degradation is invisible.

## BLOCKER for the concurrent agent's branch (found 2026-08-16 during pre-merge review)

Their 103-file change is otherwise strong — 1,188 tests pass, `cargo deny`/`npm audit`/typecheck clean,
migration v53 is sound, the model pins are untouched, their vendored `tiny_http` fork closes the
Content-Length abort, and they fixed the half-written-decision bug properly with an atomic
`finalize_phone_human_decision_at_revision`. Two tests fail, and one is a privacy regression.

### 1. The snapshot-AND consent gate for cloud STT was deleted
`pipeline::tests::granting_consent_mid_run_does_not_retroactively_enable_a_run_started_without_it`
fails with *"a run begun without consent must stay offline"*.

Cause: `scribe_api_key_if_enabled` was removed from `pipeline.rs`, and it carried the only
`snapshot AND live consent` check for STT:

```rust
if !self.settings.cloud_stt_opt_in || !self.consent.cloud_stt() { return None; }
```

Grep confirms **no remaining STT path ANDs the snapshot with live consent**, and `cloud_stt()` is now
`#[cfg(test)]` because its last production caller went with it. `require_cloud_stt_consent`
(`commands.rs`) still guards the direct Scribe IPC commands, but it reads LIVE settings only.

Consequence, exactly as the `LiveConsent` doc comment warns (it documents this as the 2026-08-06
audit finding): revocation mid-run still fails closed, but **granting** mid-run now retroactively
applies to a run the user started under a no-cloud understanding. That contradicts
`DATA_GOVERNANCE.md`'s "zero silent collection" clause, and voice is biometric data.

**This needs a decision, not a quick fix, and the test must NOT simply be deleted.** Either the
pipeline still has an STT upload path — in which case restore the snapshot AND there — or the
pipeline genuinely no longer uploads to Scribe, in which case the test's subject has moved to the IPC
commands and the assertion should move with it, with the reasoning written down. A live-only check is
defensible for a user-initiated IPC call (there is no "run" to snapshot); it is not defensible for a
long-running import. Deciding which is which is the work.

Also stale: `pipeline.rs:1230` still refers to `scribe_api_key_if_enabled` in a doc comment.

### 2. The 7B error message no longer names the offline alternative
`pipeline::tests::wsl7b_without_script_is_unresolved_not_silently_downgraded` fails on
*"error must name the offline-model choice the owner can deliberately pick"*.

The BEHAVIOUR is still correct — it refuses to downgrade (`E_ASR_7B_UNAVAILABLE … Refusing to
silently downgrade to a smaller model you did not select`). The rewritten message just no longer
names the offline model the owner may deliberately select. Smaller fix, no safety impact — but it is
the message that tells the owner the 300M/1B are a legitimate deliberate choice, which is one reason
those models should not be deleted.

### 3. Deployment blocker (not a code defect)
The couch server becomes TLS-only on every interface while Tailscale Funnel proxies **plain HTTP** to
`127.0.0.1:8737`. Nothing in their diff touches the Tailscale config, so a rebuild alone takes all
eight reviewer links dark. Cutover runbook staged at `scratchpad/cutover_tls.sh`: cold DB backup
before v53 runs, rebuild, repoint Funnel to `https+insecure://127.0.0.1:8737`, verify every link
streams audio, auto-rollback on failure.

## Verified clean — stated so it is not re-audited

* The watchdog's 3-strike kill cap is sound and self-resetting; no expiry hole.
* `settings.rs` state persistence is exemplary: atomic replace, persist-before-commit.
* The HuggingFace export stages into a fresh tree and has an explicit zero-clip guard.
* Reviewer identity is derived only from the cookie token, never from the request body.
* Every couch SQL statement is parameterised, including the dynamic `IN` list.
* Live data-integrity invariants all pass on the real DB: no machine text in the human field, every
  untouched row serves the champion's own transcript, every accept freezes text an engine produced.
