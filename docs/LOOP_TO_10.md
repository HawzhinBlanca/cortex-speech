# LOOP TO 10 — the doctrine that finishes this, or reports honestly why it cannot

The owner's directive (2026-08-18, verbatim intent): *"finish all remaining tasks at 10/10 using all
its power and SKILLs and anything needed to make the system (7/10 and 3/10) to real 10/10 10/10."*

The two numbers are the 2026-08-17 flywheel audit's honest grades: **data-curation ≈7/10** and
**correction→better-live-model ≈3/10**. This document is the doctrine for driving BOTH to 10/10.

Every run is a fresh session with no memory. Everything a run needs is here, in
`docs/PLAN_TRUE_10.md`, and in `PROGRESS_LEDGER.md`. **Follow this document exactly.**

---

## 0. Read first, every run (in this order)

1. This file, top to bottom.
2. `docs/OWNER_CANON.md` — approved-and-FINAL decisions. **Canon items may not be altered without
   the owner writing `change canon: <item>` himself.** If it is in the canon, the discussion is over.
3. `cortex-speech-app/CLAUDE.md` — the honesty law. It overrides everything, including this file.
4. `docs/PLAN_TRUE_10.md` — the phase plan and each phase's measurable exit gate.
5. `PROGRESS_LEDGER.md`, last ~150 lines — what was done, what failed, what is next.
6. `docs/GODMODE_LOOP.md` — per-iteration protocol and guardrails.

## 1. The one law

Never invent, estimate, round, or "remember" any metric. Every number comes from a real run, with
the command and its output in `PROGRESS_LEDGER.md`. **Nothing is "done" until it is user-observable
or measured on real audio.** A bad result is reported as a bad result. Owner-gated items are
**surfaced, never faked**. Never weaken, skip, or delete a gate to make something pass. A fix
without a regression gate is incomplete.

Three failure modes this project has actually suffered, all of which look like progress:

- **A test that passes against the broken code.** Every regression test must be run against the bug
  it describes and shown to FAIL before the fix is called done. This happened on 2026-08-17: the
  first split-leak test passed on the defect, which would have shipped a false green.
- **A gate that is always red.** It gets ignored, and then a real failure passes unnoticed. Fix the
  gate's blind spot; never raise a baseline to silence it.
- **An exit code read through a pipe.** `cmd | tail` reports `tail`'s status. Read the printed
  verdict, or capture to a file and check `$?` directly.

## 2. What 10/10 means here (the finish line, measurable)

`make verify-10` exits 0 printing `CORTEX 10/10: ALL GATES GREEN`, **and** the three flywheel gates
from Phase 5 exist and pass. Until then, keep going.

| # | Gate | Green when |
|---|------|-----------|
| A | every kept sweep gate | `verify_10.py` reports 0 failed |
| B | labeled corpus | ≥ 25 h labeled, top-1 recording ≤ 30 %, ≥ 25 labeled recordings |
| C | snapshot immutability | two exports of an unchanged library hash identically; sealing twice is a no-op |
| D | challenger loop | a canary completes snapshot → train → eval → verdict with zero manual steps |
| E | promotion drill | promote → serve a real clip → roll back → prove byte-identical serving of the prior champion |

**A REJECT verdict from the promotion gate is a PASS of gate D.** The loop proves the machinery, not
that a particular model won.

## 3. Phase status (update this table every run; it is the loop's memory)

| Phase | State | Exit gate |
|---|---|---|
| 0 — land in flight | **OWNER-BLOCKED** | 3 spot-check adjudications (21/24) + 12 wrong-dialect re-reviews |
| 1 — break the data skew | **STARTED** | gate B above. Importing is not labeling — only reviewers move it |
| 2 — snapshots + pack provenance | **DONE** 2026-08-18 | gate C — **MEASURED GREEN**, 1 sealed snapshot |
| 3 — challenger loop | **WIRED; BLOCKED on a trainer** | gate D — snapshot ✓ train ✗ eval ✗ verdict ✗ |
| 4 — registry + rollback | **NOT STARTED** | gate E |
| 5 — redefine done | **NOT STARTED** | gates C/D/E wired into `verify_10.py` |

## 4. Per-run protocol

1. **Lock.** If `.loop-to-10.lock` at repo root is < 6 h old, another run owns the machine — exit.
   Otherwise write it. Remove it by **ABSOLUTE path** at the end; `cwd` drifts into subdirectories
   and a relative `rm` leaves a stale lock that stalls every later run.
2. **Reality check before work.** Run the cheap gates and read the real numbers:
   `python scripts/check_spot_check_pool.py`, `check_dataset_duplicates.py`,
   `check_reviewer_queues_live.py`, and the labeled-corpus query in §6. Never plan against remembered
   numbers.
3. **Pick the highest-value UNBLOCKED item** from §5. Do not start owner-blocked work.
4. **Build it with its gate.** The regression test comes with the fix, and is proven to fail without it.
5. **Verify at the consumer**, never the write path: read the exact row/field the reviewer, export, or
   trainer actually reads. Three incidents in this repo passed their write-path checks and lied at
   the point of consumption.
6. **Adversarially verify anything load-bearing.** Spawn skeptics whose job is to REFUTE the claim,
   with the code in front of them. This is not ceremony: on 2026-08-17 it caught a leaking train/test
   split in work that had already been reported as done and tested.
7. **Land it.** `cargo fmt` → commit → build (in that order; the exe bakes HEAD's SHA and building
   before the commit re-stales it). One logical change per commit, Conventional Commits, on a branch,
   PR to `main`. Never check `main` out.
8. **Record it.** Append to `PROGRESS_LEDGER.md`: what was measured, the verbatim numbers, what
   failed, what is owner-gated. Update §3's table.

## 5. The work queue (highest value first; skip what is blocked)

1. **Phase 5 first, not last.** Wire gates C, D and E into `verify_10.py` as `snapshot-immutability`,
   `challenger-loop` and `promotion-drill`. Doing this early means every later run is measured by the
   finish line instead of by opinion. C and D can be wired now; E lands with Phase 4.
2. **Phase 4 — the registry.** `adapters` becomes the single source of truth (adapter path, snapshot
   id, eval report hash, status). Promotion is ONE transaction: registry flip → adapter reload →
   health check → automatic rollback to the prior adapter if the health check fails. The champion
   server must read its adapter path FROM the registry, not from `CORTEX_7B_MODEL_DIR`. Drill it like
   the backups were drilled, and make the drill gate E.
3. **The canary run (gate D) — RAN 2026-08-18, and stopped at `train`.** The first two links are
   done on real data: `export_pack` sealed snapshot `23db46a0…` (414 rows) and `train_challenger.py`
   verified the pack against it, exit 3 / `status: "prepared"`. `export_pack` does NOT need the app
   closed (it takes no instance lock), so §7's first bullet no longer applies to this step.
   **What blocks the rest: no trainer exists.** `--trainer` expects an external fine-tune command,
   and building a real LoRA trainer for the 7B champion is an OWNER decision (compute, and it touches
   the model lock) — surface it, never improvise one. Until then gate D cannot be met, and
   `check_challenger_loop.py` passing is NOT gate D: that script only certifies that the run records
   on disk claim no more than they did.
4. **Phase 1 throughput.** Keep the review queue stocked: import the next diversity-ordered batch
   (smallest books first — each book is one narrator, and the queue is oldest-first FIFO, so import
   order IS the diversity lever). Then STOP importing: gate B measures labels, not clips, and more
   audio cannot move it.
5. **Own speaker-disjoint holdout.** The frozen FLEURS set is not enough for gate E's slice checks.
   Grow `gold_segments` from newly reviewed recordings — speaker- and source-disjoint from training.
6. **Whatever the last run's ledger entry names as next.**

## 6. Measuring gate B (paste the real output, never a remembered number)

```sql
SELECT COUNT(*), SUM(duration_ms)/3600000.0 FROM speech_segments
WHERE human_decision IN ('accept','edit');
```
Top-1 share and distinct labeled recordings come from the same table grouped by `audio_path`.
As of 2026-08-18: **426 clips / 1.08 h, top-1 94.7 %, 5 recordings** — far from gate B, and only
human review moves it.

## 7. Machine realities that have each cost a run

- **The app owns the champion.** `engine_runtime.rs` holds the `wsl.exe` child, so closing the app
  KILLS the 7B server and every headless import then refuses every file. Start the champion
  separately first, and hold it — a `nohup`'d WSL process is torn down when the launching `wsl.exe`
  exits.
- **Disable the watchdog for any app-down window**, and re-enable it after. It relaunches the app
  mid-run, and the app takes the instance lock the importer needs.
- **Reviewers may be live.** Check port 8737 and `review_events` before closing the app; ask the
  owner rather than dropping paid reviewers mid-clip.
- **Never run two cargo commands against one target dir** — the second fails on a locked linker
  output. Use `CARGO_TARGET_DIR=<scratch>` to type-check while the app runs.
- **Python prints CRLF on Windows.** Strip `\r` from any list piped into bash, or every path built
  from it is invalid (`Os code 123`).
- **Read exit codes without a pipe.** See §1.

## 8. Stopping conditions (an honest stop always beats a flattering finish)

Stop the run and write the ledger entry when any of these is true:

- every unblocked item in §5 is done and the remainder is owner-gated;
- a gate is red for a reason the run cannot fix without weakening it;
- an owner-gated decision is reached (canon, consent, a native Sorani ear, a model swap);
- the machine is not idle enough to measure on (a concurrent session once made a benchmark CONFIRM a
  fake 1.41× regression).

**Never** report a phase as complete because its code was written. A phase completes when its exit
gate in §2 passes, measured, with the output in the ledger.
