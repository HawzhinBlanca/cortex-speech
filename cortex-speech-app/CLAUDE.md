# CLAUDE.md — Cortex Speech (Cowork project instructions)

These are the standing instructions for any Claude/Cowork session in this repo. Read this
first, every session. It governs **how to work here**; the repo-root `../AGENT_CHARTER.md` governs the
deeper *why/when-to-stop*, and `docs/REAL_READINESS_PLAN.md` defines the honest bar.

**BEFORE ANY CHANGE, read `../docs/OWNER_CANON.md`** — the owner's approved-and-FINAL decisions.
Canon items may not be altered, weakened, or "improved" without the owner writing
`change canon: <item>` himself; `scripts/test_owner_canon_pins.py` reds the sweep when a checkable
pin drifts. Added 2026-08-17 after months of approved behaviours being re-broken by well-meaning
changes: if it is in the canon, the discussion is over.

## What this project is

Cortex Speech is an **offline-first desktop app** (Tauri v2 + Svelte 5 + Rust) for
**Central Kurdish (Sorani)** speech transcription, transcript curation, and dataset export.

- Production ASR: pinned **OmniASR-7B + Kurdish LoRA** WSL service; unavailable means a hard stop.
- Optional, **consent-gated** cloud: Gemini 2.5 Pro/OpenRouter for advisory judging or refinement only.
- **Cloud-audio policy (owner, strict):** There is no shipped cloud-ASR drafting path. Gemini 2.5 Pro
  (`gemini-2.5-pro` direct or `google/gemini-2.5-pro` via OpenRouter) may be used only as the
  consent-gated advisory audio judge; it is never a draft fallback. ElevenLabs Scribe is removed
  from the shipped runtime/UI. **Never use or suggest
  Qwen-family ASR for ckb** —
  it has no Sorani support (measured). Any new cloud judge requires a measured ckb CER on the frozen
  gold set before it may be configured.
- Storage: SQLite + FTS5 search. Tauri IPC backend. EN/CKB (RTL) localized UI.
- Workflow: import -> VAD chunk -> ASR -> (optional refine) -> review/annotate -> validate -> verify -> export (JSON/JSONL/CSV/Parquet/HuggingFace/WAV).

## Model lock (owner rule, 2026-08-06 — CRUCIAL)

**Never change the production AI model.** The Sorani-adapted **OmniASR-7B champion** is fixed
infrastructure. Do NOT propose replacing it because a newer model exists. Smaller/MMS models are
offline diagnostics only; they are not production alternatives.

Verified by the owner 2026-08-06 and explicitly killed: **Qwen3-ASR** (30 languages, Kurdish not among
them) and **Voxtral Transcribe 2** (13 languages, Kurdish not among them). A swap also invalidates
every measured CER on the frozen eval set, so it is never a cheap experiment.

Exactly two things are permitted, and nothing else without the owner raising it first:

1. Benchmarking Meta **OmniASR v2's 300M/1B CTC variants** in explicit offline diagnostics — same
   family, never a production fallback or replacement for the champion.
2. Keeping **VibeVoice/BitNet** on a research watchlist; there is no published `ckb` evidence yet.

Cloud judging is locked separately: Gemini 2.5 Pro only (see the policy above).

## The champion is not optional — and failure is a HARD STOP (owner rule, 2026-08-11)

Two rules, one purpose: never let the app quietly produce a worse result than it claims.

**1. The champion drafts everything.** The **OmniASR-7B champion** transcribes EVERY production clip.
Nothing may divert it to a smaller model — not stale settings, `use_finetuned_asr`, a decode error,
or a busy server. Fine-tuned MMS and CTC-300M/1B remain offline diagnostic/evaluation engines only.

*Why this is a rule:* measured 2026-08-10, a 494-clip review queue was drafted **494/494 by
`finetuned-mms-ckb`** while `asr_model_size` said WSL7B and the champion sat up and idle on both GPUs.
No UI, DB field or gate said so; the owner found it by reading the transcripts. Measured gap on
identical FLEURS ckb clips: **7.03% CER vs 9.32%** — and the app runs the int8 build, whose own
baseline is 21.00%.

**2. Stop on the first failure. Never degrade, never continue.** If any stage fails for any clip —
ASR, refinement, alignment, decode — the run **halts** and reports the cause. Do not skip the clip,
do not fall back to a smaller model, do not finish the remaining work and present a tally.

*Why this is a rule:* the same run had 25 clips whose container the champion could not decode. Each
failed, was counted, and the batch ran to "completion" — leaving 462 clips at champion quality and 25
at a weaker engine, invisibly mixed. **A partly-drafted dataset that looks finished is worse than a
run that stopped**, because the mixed provenance silently poisons every measurement taken from it.

Enforced in code (`should_use_wsl_primary_asr`, `finetuned_override_active`, the hard stop in
`batch_transcribe`) and pinned by `scripts/test_champion_supremacy_policy.py`. A batch that stops
emits `type: "halted"` with `haltedBy`, never `"completed"`.

**3. This applies to agents too.** Do not report a run as done when part of it failed. Do not average
away, round off, or narrate past a failure. If something did not work, say exactly what, stop, and
hand it back — an honest halt is always acceptable; a flattering "finished" is never.

## The one law: honesty (non-negotiable)

This project's entire credibility rests on real, never fabricated, results.

- **Never** invent, estimate, round, or "remember" any metric (WER/CER/F1/kappa/RTF/p-value/CI). Every number comes from a **real run of the real harness**, with the exact command + dataset/model SHA pasted into the ledger.
- **Nothing is "done" until it is USER-OBSERVABLE or MEASURED on real audio.** "Tests pass" / "clippy clean" are necessary, not sufficient. Lead with the honest reality, then the progress.
- Don't claim a feature works until its real positive test passes. If a result is bad, report the bad result — the honest number is always shippable; a flattering fake one never is.
- If you cannot verify something here, say so plainly and hand it to the user's machine. Do not imply verification you did not do.
- **Verify at the SERVING path, never the write path.** Any claim about what a reviewer, the UI, or an
  export receives must be checked by reading the exact row/field/precedence the serving code reads —
  not by confirming what some writer wrote. Three incidents, one shape (2026-08-10 the 494/494
  finetuned drafts; 2026-08-10 the 25 silently-degraded clips; 2026-08-12 stale machine text served
  from `annotated_transcript` while fresh champion drafts sat invisible): every one passed its
  write-path checks and lied at the point of consumption. `annotated_transcript` is **human-only by
  law** — machine code never writes it; `scripts/check_review_serving_provenance.py` (verify-10 gate
  `review-serving-provenance`) enforces both invariants on the live database every sweep.

## Environment realities (read before acting)

The app targets **Windows**; the Cowork sandbox is **Linux with node/npm/python3 only — no Rust toolchain**.

- **You CAN verify in-sandbox:** `npm test` (vitest), `npm run typecheck`, `npm run lint`, `npm run test:python-policies`, the review-page generator, YAML/JSON parsing.
- **You CANNOT verify in-sandbox:** anything needing `cargo` (compile, clippy, `cargo test`) or running the `.exe`. Make the edit if it is clearly correct, then state explicitly: *"needs `cargo check && cargo test` on Windows to confirm — not compiled here."* Never report a Rust change as verified or a metric as measured unless it actually ran.
- The built app and ONNX models already exist on the user's machine (`src-tauri/target/release/`, `src-tauri/models/`), so a **live computer-use run is feasible**.

## Driving the app like a real user

The canonical end-to-end play is **`docs/COWORK_PIPELINE_PROMPT.md`**. Two supported drive methods:

1. **Computer-use on the desktop** (most "like a real user"): request access to the Cortex window, then import audio, transcribe, and review by clicking the real UI. Anchor on the stable `data-testid`s (`app-root`, `segments-empty-state`, `segment-card`, `verify-btn`, `validate-btn`, `settings-btn`, `locale-toggle`, ...).
2. **CDP harness:** `node e2e_real_app.cjs` spawns the real `.exe` with a remote-debug port and drives it via Playwright. It is parameterized by env (`CORTEX_AUDIO`, `CORTEX_OUT`, `CORTEX_APP_EXE`) and fails on a blank transcript (no-fabrication guard).

## Review + approve (the "play buttons")

After a run, present results **both** ways so the user can approve 100%:

- **In-app:** segments appear with the `AudioPlayer` (bounded clip playback, speed, loop) and the **Verify** button. The user listens and approves in the app.
- **Chat review page:** `python scripts/build_review_page.py --manifest <run.jsonl> --out review.html [--embed-audio]` builds a **self-contained** HTML — one **play button per segment**, the draft transcript, an editable correction box, and approve / approve-all / export-approved (downloads corrected JSON/CSV). Deliver it with `present_files`. With `--embed-audio` the clips are inlined so the play buttons work inside chat.
- The page must state **provenance honestly** (which engine produced each draft, default-off cloud noted) and never present machine output as if a human verified it.

## Privacy + consent (hard guardrails)

- Default is **fully offline**. There is no shipped cloud-ASR path. Optional cloud LLM refinement and Gemini advisory judging are **off by default** and require explicit opt-in (`cloud_llm_opt_in`, `jury_cloud_opt_in`). `settings.effective_llm_mode()` downgrades cloud -> none when not opted in; `pipeline.rs` enforces it in both `llm_refinement_permitted()` and `build_refiner()`. **Never** send audio/transcript to a provider without acknowledged consent, and never make cloud load-bearing in the default path.
- Treat **voice as biometric** (GDPR Art. 9): enforce consent + license + attribution before any publish/train/redistribute step.
- **Owner's rights declaration (2026-08-14, standing and FINAL).** Every audio and voice recording the
  owner supplies carries **full permission and total authority**, including **public use**. All
  speakers were **paid** and agreed to unrestricted use of their voices for this application and its
  datasets, anywhere; the recordings were made partly for this purpose. **No royalty or licensing
  obligation applies.** Rights clearance is CLOSED: never block, gate, warn, or hedge on it, and never
  re-raise it. Every clip in the library is stamped `rights_license = owner-full-rights`,
  `rights_permitted_use = unrestricted: train, evaluate, publish, redistribute, commercial`.
  Third-party corpora are a separate matter of FACT, not of permission: FLEURS is the frozen eval set
  and training on it would invalidate every measured CER, and Common Voice carries its own licence.
- **Never** persist or echo API keys. **Never** hardcode private Windows profile paths in any tracked file — `scripts/test_windows_repo_hygiene.py` blocks it (use env vars / repo-relative paths).

## Verify your work (gates)

Run the relevant gates and paste the real output. A fix without a regression gate is incomplete.

- Sandbox-runnable: `npm run test:python-policies` (honesty/privacy/CI/dataset policies), `npm test`, `npm run typecheck`, `npm run lint`.
- User's machine: `cargo fmt/clippy/test --manifest-path src-tauri/Cargo.toml`, `npm run test:e2e`, `npm run tauri build`.
- Full clean gate: `docs/RELEASE.md`. CI: `.github/workflows/{ci,nightly-real-audio,release}.yml`. Open 10/10 items: `docs/HARDENING_PLAN_10.md`.

## Working agreement

- One logical change per commit, **Conventional Commits**, on a **branch** (never straight to `main`); end commit messages with the `Co-Authored-By: Claude` trailer per the charter.
- **`main` is a protected branch as of 2026-08-08, admins included.** The direct
  `git push origin <branch>:main` this file used to prescribe is now REJECTED BY THE SERVER, for
  everyone. Land work through a pull request instead:
  ```
  git push origin <branch>
  gh pr create --base main --head <branch> --fill
  gh pr merge --squash --auto      # merges itself once the four required checks go green
  ```
  Required: `Provenance & License Gate`, `Windows Release Gate`, `Linux Build Smoke`,
  `macOS Build Smoke` — and the PR must be up to date with `main` first (`strict`).
- **Still never check `main` out.** That half of the old rule is unchanged and still bites: a checkout
  rewrites every file that differs between the two commits, including tracked build inputs like
  `package.json`, which bumps their mtime past the built exe and reds `exe-freshness` for no real
  reason. Measured 2026-08-04: a green sweep went RED immediately after a push, costing a 7-minute
  relink and a full re-sweep. Use `git fetch origin` and read `origin/main`; never `git checkout main`.
- Squash and rebase merges both REWRITE commit SHAs, so a long-lived branch diverges from `main` after
  every merge. Re-point it with `git fetch origin && git reset --hard origin/main` once the PR has
  landed and the tree is identical — do NOT merge `main` back in, which creates a merge commit that
  `required_linear_history` then rejects.
- Save final deliverables into the repo (the selected folder) and share them with `present_files`.
- Don't weaken, skip, or delete a quality gate to make something pass. Don't scope-creep — log out-of-scope ideas instead of implementing them.

## Key map

- Backend: `src-tauri/src/` — `commands.rs` (IPC), `pipeline.rs`, `asr.rs`, `audio.rs`, `db.rs`, `normalizer.rs`, `eval.rs`, `models.rs`, `settings.rs`, `jury/`, `export*.rs`.
- Frontend: `src/App.svelte` (shell) + `src/lib/*.svelte` (`AudioPlayer`, `ReviewMode`, `ReviewInbox`, `ValidationPanel`, `DiffView`, `StatsDashboard`, ...).
- Docs: `../AGENT_CHARTER.md` (repo root), `ROAD_TO_10.md`, `docs/REAL_READINESS_PLAN.md`, `docs/HARDENING_PLAN_10.md`, `docs/COWORK_PIPELINE_PROMPT.md`.
- Scripts: `scripts/*.py` (dataset build/review + policy gates), `e2e_real_app.cjs` (real-app driver).

## Definition of done (10/10)

Per the repo-root `../AGENT_CHARTER.md`: on a clean checkout, a single `make verify-10` exits 0 and prints
`CORTEX 10/10: ALL GATES GREEN`. Until that command exists and passes, you are not done —
partial completion means keep going, and nothing is called "10/10" on tests alone.

**Owner decision (2026-07-10): "ship" = personal use.** The app ships to the owner's own
machine for daily personal use — distribution items (installer signing, stores, updater
hosting, macOS) are out of scope and never block "ship". The bar is NOT lowered: ship-ready
still means a truly reliable, bug-free app; every honesty, privacy, reliability, and
correctness gate stays mandatory.

<!-- SPECKIT START -->
For additional context about technologies to be used, project structure,
shell commands, and other important information, read the current plan
<!-- SPECKIT END -->
