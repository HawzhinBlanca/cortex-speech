# The Accuracy & Usefulness Loop

**Standing doctrine for the autonomous ~10-minute loop.** One fire = one honest, gated increment toward
the highest *real* Central Kurdish (Sorani) ASR accuracy and the highest everyday usefulness of Cortex
Speech. This supersedes the generic "continue and finish the app" prompt with a sharper mission and a
brutally honest scope. Read it top to bottom every fire.

Companion docs: the honesty bar [CLAUDE.md](../CLAUDE.md) · the current tech reality
[ASR_TECH_SCAN_2026-07-23.md](ASR_TECH_SCAN_2026-07-23.md) · the measured record
[MEASUREMENTS.md](MEASUREMENTS.md) · the loop mechanics [MONTH_LOOP.md](MONTH_LOOP.md).

---

## 0. The one law — honesty (non-negotiable)

Never invent, estimate, round, or "remember" any metric (CER/WER/RTF/κ/α/ECE/p-value/CI). Every number
comes from a **real run of the real harness**, with the exact command + dataset/model SHA pasted into the
ledger. **Nothing is "done" until it is user-observable or measured on real audio.** "Tests pass" is
necessary, not sufficient. If a result is bad, report the bad number — the honest number always ships; a
flattering fake never does. See memory `no-unearned-completion-claims`.

## 1. Brutal reality check — read this before you believe you can raise accuracy

**A 10-minute autonomous loop cannot move the measured CER. It cannot, ever, by itself.** The two things
that actually lower Sorani error are both outside this loop's hands:

1. **The owner's GPU rig** (2×3090 Ti) — every retrain, quantization, and *measurement* runs there. This
   loop has no GPU, no Rust toolchain guarantee, no live model weights, and must never touch the live DB
   or the running `.exe`.
2. **The owner's ears** — the Gold Marathon (500 human review decisions; currently **3/500**) and the
   inter-annotator gold set. No model can be honestly graded on Sorani without them.

A loop that pretends otherwise becomes a **fabrication engine** — it "reports progress" that never
happened. That is the one failure mode this doctrine exists to prevent.

**So the loop's honest job is to maximize the return on the owner's scarce rig-time and review-time.**
Its success metric is **not** "CER dropped" (it may never claim that). It is:

> *When the owner next sits at the rig, the single highest-leverage accuracy experiment runs in one
> command, and its result is trustworthy — because the loop already built the harness, the gate, the
> normalizer, the data importer, and the exact work order.*

Everything below serves that. Where the loop CAN directly help users today is **usefulness** (reliability,
correctness, honesty, privacy, UX) — so that half of the mission is real, shippable, and compounding.

### The current honest state (verify, don't trust this snapshot)
- Champion (GPU-served fine-tuned OmniASR-7B): **7.03% CER on FLEURS-ckb** read speech — at/above
  verifiable Sorani SOTA as of 2026-07-23, but **not reproduced under a documented normalizer** and
  **not measured on conversational Sorani or the owner's own audio**.
- Offline default (bundled OmniASR-CTC-300M int8): **11.34% CER**.
- Owner's own verified data: **~3 segments**; marathon **3/500**; verified audio **< 5 h**.
- The label ceiling (gold-set noise), not the model, currently caps every number you could report.

## 2. Per-fire protocol (identical discipline to the month loop)

1. **Acquire the lock** `.month-loop.lock` by **absolute repo-root path** (`C:/Users/Wareen/Desktop/
   cortex-speech/.month-loop.lock`). If held, stop. See memory `month-loop-lock-absolute-path`.
2. **Reality check:** `tasklist` shows `cortex-speech-app.exe` NOT running; `git status` clean; record
   HEAD; lock was free. If the exe is running: do not kill it, do not touch the live DB, do not rebuild
   into `src-tauri/target/release`.
3. **Pick exactly ONE item** — the highest-leverage *ready* task per §3.
4. **Hand-verify against source** — the finding/claim must be confirmed by reading the actual code/docs.
   Never trust an agent verdict or this doc's snapshot; the code is the evidence.
5. **Fail-before** — write the test/policy first and observe it FAIL on the unfixed state (neutralize-
   then-restore if needed). A fix without a regression gate is incomplete.
6. **Root-cause fix** — shortest correct diff, at the shared choke point (ponytail). Deletion over
   addition. One logical change.
7. **Full gate** — for Rust: isolated `CARGO_TARGET_DIR=/c/Users/Wareen/AppData/Local/Temp/
   cortex-monthloop-target`, then `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` ·
   `cargo test --lib` · `python scripts/run_python_policies.py`. For frontend: `npm run typecheck` ·
   `npm test` · `npm run lint` · policies. Never weaken/skip/delete a gate. Paste real output.
8. **Commit** — Conventional Commit, one logical change, end with
   `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Push branch, FF `main`.
9. **Ledger** — append a `PROGRESS_LEDGER.md` entry (what/why/fail-before/gate/honest caveats). Commit,
   push, FF `main`.
10. **Release the lock** — `rm -f` by absolute path. **No `ScheduleWakeup`** — cron drives cadence.

On Sundays, also produce the weekly owner report.

## 3. Task selection — priority order

Pick the first ready item; if blocked (needs owner/rig or unverifiable in-sandbox), record the work order
(§4) and drop to the next.

1. **Honesty repair (always first).** Any place a number, gate, or provenance claim could be fabricated
   or inflated. The AsoSoft-normalizer re-score (backlog #1) lives here — it protects the 7.03% headline.
2. **Accuracy machinery — build/harden the owner's next run.** Work the ranked backlog in
   [ASR_TECH_SCAN_2026-07-23.md §5](ASR_TECH_SCAN_2026-07-23.md): normalizer → pseudo-labeling harness →
   KenLM fusion → 1B benchmark harness → int8 quant script → active-learning queue → IAA/ECE → data
   importers → homophone FST → PR-gate/ledger reconcile → two-stage retrain recipe. Each ships as an
   offline, gated, testable artifact; the GPU/marathon RUN stays an owner work order. **You never claim
   the CER moved — only that the machinery to move it is now built and trustworthy.**
3. **Usefulness — user-observable improvements.** Reliability, correctness, privacy, and UX of daily
   transcription/curation/export: the adversarial defect hunt (the proven engine), plus review-flow,
   playback, search, and export quality. This is the half of the mission the loop CAN ship directly.
4. **Research refresh (cadence, not every fire).** Roughly weekly, or when the backlog is drained,
   re-run the 5-facet scan (SOTA models / bases / low-resource techniques / offline tooling / data+eval),
   write a new dated `ASR_TECH_SCAN_<date>.md`, and convert genuinely-new findings into backlog items.
   **Honestly flag when nothing new is found** — "no change since last scan" is a valid, valuable result.

## 4. Owner-gated work orders (keep this current)

Maintain a section the owner can execute with zero re-derivation — the exact commands for the rig/marathon,
updated as the machinery lands. A rig session should be pure execution, not plumbing. Format each as:
*goal · exact command(s) · expected artifact · which gate blesses it · what a good vs bad result looks
like.* The highest-value orders today (build the harness first, then the order becomes runnable):

- **Re-score the champion under the AsoSoft normalizer** on a contamination-checked FLEURS-ckb split →
  confirms or corrects the 7.03% headline.
- **Benchmark OmniASR-CTC-1B int8 vs 300M** on the ckb gold set → decide the offline default by measured
  delta.
- **One pseudo-labeling round** (teacher = champion) on the 1.74M corpus, confidence-filtered, QLoRA
  retrain, **gated on a held-out human slice** → ship only if the gate says it didn't regress.
- **Advance the Gold Marathon** on the active-learning-ranked queue (uncertainty × diversity).

## 5. Stop conditions & guardrails

- Never claim accuracy improved without a real harness run pasted into the ledger.
- Never ship a pseudo-label round (or any retrain) that a real held-out eval did not bless — self-training
  bakes in the teacher's blind spots; that is the designed-against failure.
- Never adopt a banned/unverified model (Qwen-family for ckb; cloud ckb = Gemini 2.5 Pro + Scribe only).
- Never make cloud load-bearing in the default path; never persist/echo API keys; never hardcode private
  Windows paths. Treat voice as biometric (consent + license + attribution before publish/train).
- Never let "machinery built" masquerade as "accuracy raised." They are different claims; keep them apart
  in every commit, ledger entry, and report.

## 6. Cadence

Fired by cron every ~10 minutes. Each fire is independent and idempotent under the lock: if there is
nothing honestly ready to ship, do the smallest real hardening/usefulness increment and stop — never
manufacture work, never manufacture a number.
