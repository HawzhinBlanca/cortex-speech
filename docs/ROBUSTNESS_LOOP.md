# ROBUSTNESS LOOP — drive this system to bulletproof, one measured iteration at a time

The owner's directive (2026-08-29, verbatim intent): *"write a loop or goal that will get us to
robust state working — continuously make it bulletproof."*

This document is the doctrine for that loop.

**Mode (owner, 2026-08-29): a ONE-DAY CONTINUOUS loop, run in-session — not a scheduled task.**
The owner declined a cron. Iterations run back to back for a day, each one small enough to be
attributed, and the loop reports at the end of the day rather than firing overnight. The doctrine is
nevertheless written for a reader with no memory, because context is lost long before a day is: any
session picking this up mid-flight should be able to continue from this file plus the scoreboard and
`PROGRESS_LEDGER.md` alone. **Follow this document exactly.**

The loop exists because robustness is not a project that finishes — it is a property that decays.
Reviewers stop being able to log in, a monitor starts lying, a gate goes vacuous, a threshold slips.
The loop's job is to find the decay before a human does, and to leave every iteration's evidence
behind so the next session can trust it.

---

## 0. Read first, every run (in this order)

1. This file, top to bottom.
2. `docs/OWNER_CANON.md` — approved-and-FINAL decisions. Canon items may not be altered without the
   owner writing `change canon: <item>` himself. If it is in the canon, the discussion is over.
3. `cortex-speech-app/CLAUDE.md` — the honesty law and the auditor discipline. **It overrides this
   file**, including anything below that appears to conflict with it.
4. The scoreboard, which is the only thing that decides what you work on:
   ```
   cd cortex-speech-app && .policy-python\Scripts\python.exe scripts\robustness_scoreboard.py
   ```

## 1. The goal, stated so it can be falsified

**Bulletproof** means all four of these are simultaneously true and *measured*, not asserted:

| # | Condition | How it is proven |
|---|---|---|
| G1 | A reviewer can always work, and any break is visible within 30 minutes | `review-health.json` heartbeat green: links + queues + continuity + vault |
| G2 | Every critical domain meets its coverage bar | `robustness_scoreboard.py` P1 GREEN (all five domains + overall) |
| G3 | `make verify-10` exits 0 — `CORTEX 10/10: ALL GATES GREEN` | the repo's own definition of done |
| G4 | Nothing is unpushed and no live ops script has drifted from its tracked copy | scoreboard P2 GREEN |

The loop ends when all four hold **on the same day**, and the final run says so with the four
verbatim outputs pasted into `PROGRESS_LEDGER.md`. Not before, and never on a claim.

## 2. The one law (restated because it is the whole product)

Never invent, estimate, round, or "remember" a metric. Every number comes from a real run with the
verbatim command and output recorded. Nothing is "done" until it is user-observable or measured.
Never weaken, skip, or delete a gate to make something pass — **if a threshold is unmet, the answer
is tests, never a smaller threshold**. A fix without a regression gate is incomplete. Report the bad
result as a bad result; an honest halt is always acceptable, a flattering "finished" never is.

Two failures from this loop's own construction, kept as warnings:

- The first scoreboard printed **GREEN at `lines=79.12/85`** — inverted comparison. A scoreboard that
  says done when it isn't is worse than no scoreboard. Any status logic gets a case proving it goes
  RED on real failing input before it is trusted.
- The first freshness check compared the report against the **wrong worktree's** commit log, so a
  measurement taken two commits ago read as current and named an already-covered function. Coverage
  is now tied to a commit by identity (`coverage-latest.meta.json`), not by timestamp.

## 3. Run preconditions (check before touching anything)

1. **Reviewers first.** If the scoreboard's P0 is RED, that is the iteration. Reviewers losing paid
   time outranks every other front. Do not proceed to coverage work with P0 red.
2. **Do not touch codex's trees — owner instruction, restated 2026-08-29.** The main tree
   (`cortex-speech`, `codex/10-10-integration`) and `cortex-speech-codex-v63` belong to another
   agent and are usually dirty with hundreds of files. Read them; never commit, stash, clean, or
   `cargo` into them. All loop work happens in **`cortex-scrub`** (branch `public/clean-release`)
   for product code and **`cortex-deploy`** (branch `deploy/link-health`) for ops tooling. If a
   target's only fix would land in a file codex is actively refactoring, say so and take the next
   target instead — a merge conflict in their tree is a cost this loop must not impose.
3. **Never press Stop in Couch Review, and never change the roster.** Stop is a REVOKE: it kills
   every reviewer's link. See `docs/` history and the vault below.
4. **Owner activity.** If the owner is using the app, do not rebuild into
   `src-tauri/target/release`, do not run `batch_importer`, do not touch the live DB. Code, tests
   and commits in the isolated worktree are always fine.
5. **Disk.** If C: has less than 60 GB free, reclaim `target/` directories in loop-owned worktrees
   only — never data, never the HF cache's contents blindly.

## 4. The iteration (this is the loop)

Exactly one target per iteration. Small, proven, pushed.

**1 — MEASURE.** Run the scoreboard. If coverage reads STALE or UNVERIFIABLE, re-measure first.

> **Never start a measurement while an iteration is in flight.** A measurement takes ~35 minutes and
> is pinned to the commit it started from; the moment this iteration commits, that number describes
> a tree that no longer exists and the scoreboard correctly discards it. Measured 2026-08-29: a run
> launched in parallel with iteration 1 was invalidated by iteration 1's own commit — 35 minutes for
> nothing. Measure at the END of an iteration (step 8), let it finish, then pick.
```
powershell -ExecutionPolicy Bypass -File cortex-speech-app\scripts\robustness_measure.ps1
```
This refuses a dirty worktree on purpose: a number measured from uncommitted code belongs to no
commit and cannot be compared to anything.

**2 — PICK.** Take the scoreboard's top NEXT TARGET row. It names a file, a line span and a
function. Do not substitute your own judgment for the ranking; the ranking is uncovered-branch
count in a domain that is below its bar, which is the honest work list.

**3 — READ THE CODE FIRST.** Open the function and understand every refusal arm before writing a
line of test. Most uncovered branches in this codebase are refusal arms, and **in this product the
refusal arms are the feature**: they are what stops double-paying a reviewer, serving a forged
certificate, or accepting a decision against stale audio.

**4 — WRITE TESTS THAT WOULD CATCH A REAL DEFECT.** Not coverage theatre. Each test asserts the
*specific* error, so it cannot pass because a different clause fired first. Where a refusal must
also leave no trace, assert the table is empty afterwards — in the payment domain a stray row is
pay evidence. Follow the file's existing test idiom rather than inventing one.

**5 — PROVE, WITHOUT A PIPE.** Capture the exit code from the gate itself:
```
cargo test --lib <module> > out.txt 2>&1; echo "exit=$?"; grep "test result" out.txt
```
`cargo test | tail` reports **tail's** exit code, which is always 0. That has already caused a push
during a red suite in this repo. Never gate a commit on a piped exit status.

**6 — FORMAT, THEN COMMIT, THEN PUSH.** `cargo fmt` first (the tree must be clean for the next
measurement to be attributable). One logical change per commit, Conventional Commits, the
`Co-Authored-By: Claude` trailer. The commit message states what was proven and what the measured
result was. Push to the worktree's own remote.

**7 — RECORD.** Append to `PROGRESS_LEDGER.md`: the target taken, the tests added, the verbatim
test result line, the commit SHA. One short block. This is what the next amnesiac session reads.

**8 — RE-MEASURE AND STOP.** Re-run the measure step and the scoreboard so the next iteration starts
from truth. Then stop. One target per iteration is the whole discipline — a session that takes five
targets cannot attribute its own results.

## 5. What is already true (do not redo it)

As of 2026-08-29, verified live and pushed:

- **Reviewer links are locked.** Six reviewers hold verified links. Pairing tokens survive app
  restarts; only Stop revokes them, and `reviewer_link_vault.py restore` undoes even that. The
  30-minute probe checks links, queues, token continuity, and vaults the credentials.
- **Two monitoring gates exist that did not before**: `check_reviewer_link_continuity.py` (catches a
  reminted token — the failure that hid three dead links for days) and the vault.
- **The watchdog is the only healer.** It probes the port, hash-verifies relaunches, defers to the
  importer's lock and caps kills. A second reviver was built and reverted the same night; a policy
  pin (`test_reviewer_link_ops_policy.py`) now reds if one is re-added. Probe detects, watchdog heals.
- **Coverage work landed** in review (branches 41.67 → 61.73) and payment (certificate + activation
  + decision refusal arms). PR #72 is content-green and threshold-red.

## 6. Guardrails that have already cost something here

- A green write-path check proves nothing about what a reviewer receives. **Verify at the serving
  path**, replicating the consumer's exact field precedence on the live DB.
- Before "fixing" a monitor, **read its code**. The watchdog was accused of blindness from four
  grepped log lines and was innocent; the "fix" built on that accusation had to be reverted.
- A pin can require the bug. Before changing a symbol, grep `scripts/test_*.py` for it and fix the
  pin in the same commit.
- A new `scripts/test_*.py` without a `__main__` block is **counted as passed while running zero
  assertions**. Every new gate must be proven to bite by injecting the regression it forbids.
- Tests that write a file then immediately read it flake on this machine. Confirm stability with
  repeated standalone runs before blaming the change.

## 7. When the loop should stop and ask

Surface, never guess: anything needing a native Sorani ear; any change to the models (canon:
champion + fine-tuned MMS are fixed); any roster or Stop/Start action; deleting anything; any
public-repo action (closing PRs, deleting branches); and any case where meeting a threshold would
require lowering it. Say plainly what is blocked and why, and keep working the rest.
