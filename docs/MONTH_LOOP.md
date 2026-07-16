# MONTH LOOP — autonomous nightly engineering, 2026-07-16 → 2026-08-15

The owner's directive (2026-07-16, verbatim intent): *"a month of continuous work to improve the
app again and again, without user — a full loop using the best latest techniques and intelligence."*

This document is the doctrine for that loop. A **scheduled task** (`cortex-month-loop`, nightly at
02:00 local) spawns a fresh Claude session each night with no memory of any prior run. Everything a
run needs to know is here, in `PROGRESS_LEDGER.md`, and in the sources of truth below. **Follow
this document exactly.**

---

## 0. Read first, every run (in this order)

1. This file — top to bottom.
2. `PROGRESS_LEDGER.md` — last ~120 lines. What was done, what failed, what's next.
3. `docs/GODMODE_LOOP.md` — the process law (per-iteration protocol, tool doctrine, guardrails).
4. `cortex-speech-app/CLAUDE.md` — the honesty law. It overrides everything, including this file.
5. `.claude/skills/cortex-operator/SKILL.md` — runbook: exact commands, paths, gotchas, testids.

## 1. The one law (restated because it is the whole product)

Never invent, estimate, round, or "remember" any metric. Every number comes from a real run with
the verbatim command + output pasted into `PROGRESS_LEDGER.md`. Nothing is "done" until it is
user-observable or measured on real audio. A bad result is reported as a bad result. Owner-gated
items (anything needing a native Sorani ear, a consent opt-in, an elevated dialog, or the owner's
judgment) are **surfaced, never faked**. Never weaken, skip, or delete a gate to make something
pass. A fix without a regression gate is incomplete.

## 2. Run preconditions (check before touching anything)

1. **Lock.** If `.month-loop.lock` exists at repo root and its timestamp is < 6 h old, another run
   (or an interactive session) is active — **exit immediately with a one-line ledger note**.
   Otherwise write the current ISO timestamp into `.month-loop.lock`. Delete it at the end of the
   run, including on failure paths. The file is gitignored — never commit it.
2. **Owner activity.** If `cortex-speech-app.exe` is running, the owner may be using the app:
   do NOT kill it, do NOT run `batch_importer`, do NOT rebuild into `src-tauri/target/release`
   (file locks), do NOT touch the live DB. Code + tests + commits are still fine.
3. **Repo state.** `git status` must be clean on `codex/newbranch`. If dirty, someone's work is in
   flight: do not stash, do not clobber — note it in the ledger and work only in new files, or exit.
4. **Orphaned toolchain processes.** Kill stray `cargo`/`rustc`/`cortex_speech_app_lib*` test
   processes before building (they hold the package-cache lock and wedge builds silently).
5. **Tests never touch production.** All `cargo test` runs use the isolated target dir
   (`CARGO_TARGET_DIR` under the session temp dir) and DISPOSABLE data profiles — never
   `%APPDATA%\cortex-speech`. Any step that must touch the live DB starts with a timestamped
   backup of `.db` + `-wal` + `-shm`.

## 3. Ownership note (supersedes older docs)

The former Codex-owned file restriction (`commands.rs`, `db.rs`, `pipeline.rs`) was **lifted by the
owner on 2026-07-14** ("codex no longer working on it"). Those files are editable, but any change
to them carries mandatory adversarial Workflow verification before commit (they are the IPC/DB/
pipeline core — the blast radius is the whole app). Older docs (`cortex-operator/SKILL.md` §2,
GODMODE) still say "don't edit" — this section supersedes them.

## 4. The month plan — weekly themes

Pick the theme by today's date. Within a theme, always select the **smallest unblocked increment**;
one logical change per commit. If the current week's list is exhausted, pull forward from the next
week. Checked boxes are maintained by the runs themselves (edit this file in the same commit).

### Week 1 · Jul 16–22 — Responsiveness & process reliability
- [ ] Measure first: instrument/identify which sync IPC commands block the UI thread with heavy
      work (ASR, hashing, export, file IO). Produce a ranked list with real timings in the ledger.
- [ ] Migrate the worst offenders to `async` + `spawn_blocking`, a few commands per run,
      behavior-preserving, each with a test. (This is GODMODE execution-order item 1.)
- [ ] Adopt `cargo-nextest` for the Rust suite (per-test isolation + timeouts + flaky detection);
      keep `cargo test` green too — nextest is additive, not a replacement gate.
- [ ] Kill/restart durability drill: scripted N× kill-during-write cycles against a disposable
      profile; assert zero lost edits / zero duplicates. Ship the drill as a repeatable script.
- [ ] 7B engine supervision skeleton: warm-probe → start → restart-with-backoff → tree-kill on
      shutdown, wrapping the existing `start_7b_server.ps1` path. No terminal window required.

### Week 2 · Jul 23–29 — Storage durability
- [ ] Audit the real write path: is there a single serialized writer? Document what exists;
      serialize if not (bounded readers, one writer queue).
- [ ] SQLite online-backup API: an IPC command + scheduled snapshot to a second directory, plus a
      restore path with a writer fence. Drill both.
- [ ] STRICT tables migration, staged with a migration test on a copy of a real DB.
- [ ] Fault drills as scripts: disk-full, DB-corruption (bit-flip a copy), missing-media,
      mid-export kill. Each drill's honest result goes in the ledger — including failures.
- [ ] Credentials off plaintext: DPAPI-protect stored API keys if any are plaintext today.

### Week 3 · Jul 30–Aug 5 — Measured intelligence
- [ ] Replace the 0.90 heuristic confidence with real CTC-logit-derived uncertainty; calibrate on
      the frozen gold set; report measured ECE/Brier (good or bad — the number is the deliverable).
- [ ] Chunk-overlap stitching: wire the pure overlap-merge into the pipeline behind an A/B flag;
      prove on real long audio that stitched ≥ unstitched (measured CER on gold), ship only on
      measured non-regression.
- [ ] Batch Gemini watcher — **flag-only, consent-gated, cost-capped**: when cloud opt-in is on,
      pre-screen the pending review queue (audio+text to `google/gemini-2.5-pro` via OpenRouter,
      never Qwen) and mark clips "watcher-flagged" with a reason. It may never auto-accept or
      auto-edit. Cap per-run spend; count and log every call.
- [ ] Rename OOD → `signal_anomaly` (one mechanical, gated sweep). **Scope re-verified 2026-07-16
      (iter 30), corrections to this note:** (a) the **user-facing** rename is ALREADY DONE — every
      `validation.ood.*` i18n *value* in `src/lib/i18n/en.ts` already reads "Signal Anomaly"; what
      remains is purely INTERNAL identifiers (DB column `ood_score`, `SpeechSegment.ood_score` +
      serde-derived `oodScore`, the `quality/ood.rs` `OodDetector`/`compute_ood_score` module, the
      `validation.ood.*` i18n *keys*, and their frontend refs). So this change has **zero UX/functional
      benefit** — it's internal-consistency only. (b) The "migration test that already exists" does
      NOT exist — none found in `migrations/` or planning notes; it must be BUILT as part of the sweep
      (a `RENAME COLUMN ood_score → signal_anomaly_score` migration that preserves existing rows).
      (c) Real scope is ~88 precise-identifier occurrences across ~20 files (not 143/34); atomic —
      DB column + Rust field + serde + frontend must land together (a partial sweep won't compile).
      **Deliberately deferred to its Week-3 slot** (iter 30): large + cross-boundary + risky for zero
      user benefit is a poor trade to rush 2 weeks early. The type systems + 924 tests catch most
      breakage; still worth its own focused, adversarially-verified pass.

### Week 4 · Aug 6–15 — Architecture, UX quiet-down, re-audit
- [ ] Decompose the 3–4k-line files (`commands.rs` first) into slices — one slice per run,
      command names and behavior preserved, adversarially verified.
- [ ] e2e expansion: a full import → transcribe → review → export pass via `e2e_real_app.cjs`
      against a disposable profile, run nightly when the app isn't in use; failures are P1.
- [ ] Quieter UX per GODMODE item 6 (workflow naming, one Export menu, Job Center) — only with
      rendered-frame proof via computer-use; no visual rewrite, use the existing design system.
- [ ] Final re-audit: `python scripts/verify_10.py` full run; reconcile every leg; month report.

### Standing items (any week, when their trigger fires)
- **Retrain trigger:** when the DB shows ≥500 human decisions, build/run the full
  train → eval → promote-or-refuse harness up to the promotion decision, which is owner-gated.
  Current count and the check command go in every Sunday report.
- **Flaky anything:** a test that flakes even once gets root-caused the same run or quarantined
  with a ledger entry — never ignored.
- **Found-in-passing bugs:** reproduce → root-cause → fix → regression gate, same run if small;
  otherwise ledger it as the next run's first pick.

## 5. Per-run protocol (one iteration ≈ one night)

1. **Select** — one line in the ledger: date, theme, chosen increment, why it's the smallest.
2. **Understand** — trace every file and the runtime flow before editing (Explore agent / Workflow
   parallel readers for breadth). Never edit on a guess.
3. **Implement** — minimum correct change; reuse > stdlib > native > fewest lines; root cause, not
   symptom.
4. **Gate** — run and paste verbatim:
   `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` · `cargo test --lib`
   (isolated CARGO_TARGET_DIR) · `npm test` / `npm run typecheck` / `npm run lint` (if frontend
   touched) · `python scripts/run_python_policies.py`. Add a regression gate for the new logic.
5. **Adversarially verify** — for anything non-trivial (byte math, durability, privacy, hot paths,
   `commands.rs`/`db.rs`/`pipeline.rs`): a Workflow with independent skeptics trying to refute the
   change; fix every CONFIRMED finding before commit. This has caught shipping blockers here —
   do not skip.
6. **Commit** — Conventional Commits on `codex/newbranch`, proof in the body,
   `Co-Authored-By: Claude` trailer. Push. Fast-forward `main` only when all gates are green.
   Never use PowerShell `@'...'@` here-strings through the Bash tool for commit messages —
   use `git commit -F <file>`.
7. **Ledger** — append the honest entry (what ran, verbatim outputs, what's NOT verified).
8. **Rebuild the shipped exe** only if shipped behavior changed AND the app is not running;
   otherwise ledger "rebuild pending — owner's app was open".
9. **Release the lock.** If time remains and the increment was small, loop once more from step 1.

## 6. Weekly owner report (every Sunday run, and on 2026-08-15)

Append to `docs/MONTH_REPORTS.md` and the ledger: increments shipped (commits), measured numbers
(with commands), drills run and their honest outcomes, human-decision count vs the 500 retrain
trigger, owner-action queue (firewall click, iPhone Tailscale test, native review, opt-ins),
and next week's top three. No grades that a gate didn't produce.

## 7. Stop conditions

- **Month end (2026-08-15):** final report, then recommend disabling the scheduled task.
- **All remaining work owner-gated:** write the exact hand-off and make runs cheap no-ops that
  say so in one ledger line — never manufacture busywork.
- **Two consecutive failed runs on the same root cause:** stop retrying blind; write a diagnosis
  entry and mark the item blocked so the next run picks something else.

## 8. What NOT to do (inherited, non-negotiable)

No chat assistant, no cloud-first anything, no microservices/k8s/vector-DB/event-bus, no model
zoo, no multi-user, no ungated auto-training, no design-system churn, no LLM-as-truth-authority.
Cloud stays opt-in and never load-bearing offline. Voice is biometric — nothing leaves the machine
without acknowledged consent. Never persist/echo API keys; never hardcode private Windows paths in
tracked files. One offline desktop app, made boringly reliable.
