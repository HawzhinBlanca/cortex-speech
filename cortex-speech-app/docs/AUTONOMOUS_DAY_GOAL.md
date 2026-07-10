# Autonomous Day Goal — Cortex Speech

**Mission:** Autonomously make Cortex Speech *measurably, honestly* better for a full day
without owner input. Work in a continuous loop. Optimize for **real, verified quality** —
not activity, not claims.

Read this together with `CLAUDE.md` (how to work here), `AGENT_CHARTER.md` (why / when to
stop), and `docs/REAL_READINESS_PLAN.md` (the honest bar). This doc is the standing driver
for an unsupervised `/loop`.

## The one law (non-negotiable, from CLAUDE.md)

- **Never** fabricate, estimate, round, or "remember" any metric (WER/CER/F1/kappa/RTF/CI).
  Every number comes from a **real run of the real harness**, with the exact command +
  dataset/model SHA pasted into `PROGRESS_LEDGER.md`.
- **Nothing is "done" until it is USER-OBSERVABLE or MEASURED on real audio.** "Tests pass"
  / "clippy clean" are necessary, not sufficient. If a result is bad, record the bad result —
  the honest number is always shippable; a flattering fake one never is.
- Keep the **7B champion server warm** (WSL `127.0.0.1:8799`) and drive the **real exe** on
  **real Kurdish audio** (`node e2e_real_app.cjs`) whenever you touch anything on the
  ASR/pipeline path.

## Operating loop (repeat until the stop condition)

1. **ORIENT** — read the newest `PROGRESS_LEDGER.md` entry, the
   `docs/TRUE_RATING_2026-07-09.md` backlog, `docs/HARDENING_PLAN_10.md`, and current gate
   status. Pick the **single** highest-value next task, priority order:
   `correctness bug > security/DR > measured ASR/UX regression > perf > polish`.
   One logical change at a time.
2. **IMPLEMENT** — the smallest correct change that fully fixes the **root cause**. Grep
   every caller and fix it once where all callers route through. Prefer
   deletion/simplification. No speculative abstractions; no new dependency a few lines can
   replace.
3. **VERIFY** — run the relevant gates and **paste the real output** (a fix without a
   regression gate is incomplete):
   - `cargo fmt --check`
   - `cargo clippy --all-targets -D warnings` (`--manifest-path src-tauri/Cargo.toml`)
   - `cargo test --manifest-path src-tauri/Cargo.toml`
   - `npm test` · `npm run typecheck` · `npm run lint` · `npm run test:python-policies`
   - For any ASR/pipeline change: rebuild **frontend BEFORE cargo**, confirm
     `python scripts/check_exe_freshness.py` is **GREEN**, then
     `CORTEX_AUDIO=<real ckb wav> node e2e_real_app.cjs` (fails on blank/placeholder — the
     no-fabrication guard).
4. **ADVERSARIAL PASS** — after it's green, actively try to **break** your own change: empty
   / huge / corrupt audio, concurrent ops, WSL-down, DB-locked, RTL/CKB input. Turn every
   real break into a test + fix before moving on.
5. **RECORD** — append a dated `PROGRESS_LEDGER.md` entry: what changed, why, exact commands
   + pasted output, honest caveats. Commit on a **branch** (never `main`), Conventional
   Commits, ending with the `Co-Authored-By: Claude` trailer per the charter.
6. **LOOP.**

## Standing work tracks (rotate; always leave the tree green + buildable)

- **Correctness** — fix confirmed bugs from the TRUE_RATING backlog and any you find.
- **Stress / soak / fuzz** — hammer `import → VAD → ASR → refine → export` with adversarial
  and large inputs; run long soak loops; fuzz the normalizer, aligner, parsers, and export
  paths.
- **Harden** — input validation at trust boundaries, error handling that prevents data loss,
  DR backup/restore integrity, security (no key leaks, bounded IO, consent gates intact).
- **ASR quality** — only via **measured** CER/WER on real gold sets (FLEURS-ckb, CV22-ckb)
  through `scripts/scorecard_7b.py` / `scripts/scorecard_finetuned.py`. Always report CI + N;
  never a bare number.
- **UX / a11y** — fix real user-observable issues; verify by driving the actual UI.
- **CI / gates** — keep every workflow green; strengthen gates, never weaken them.

## Guardrails (do not violate)

- Do **not** weaken, skip, `#[ignore]`, or delete a quality gate to make something pass.
- Do **not** scope-creep — log out-of-scope ideas in the ledger instead of building them.
- **Never** hardcode a private Windows/WSL profile path in a **tracked** file
  (`scripts/test_windows_repo_hygiene.py` enforces it — use env vars / repo-relative paths).
- Default **offline**; cloud LLM/STT stay off-by-default and consent-gated.
- **Ponytail discipline** — the shortest change that works, once you fully understand the flow.

## When blocked

If a task needs owner-only action (hardware, secrets, a human decision, GPU-hours you can't
spend), write a clear **BLOCKED** note in the ledger with exactly what you need, then move to
the next task. **Never stall the loop.**

## Stop condition

Keep going until `make verify-10` is **full-charter GREEN**
(`CORTEX 10/10: ALL GATES GREEN`) with real-audio evidence, or you genuinely run out of
verifiable improvements. At each session end, write a short ledger summary: what got
measurably better, the numbers, what remains, and the single most valuable next task.

## Kick-off

Confirm the 7B server is warm, capture an **honest baseline** of the full gate suite
(paste it into the ledger), then begin the loop.
