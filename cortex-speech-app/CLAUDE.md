# CLAUDE.md — Cortex Speech (Cowork project instructions)

These are the standing instructions for any Claude/Cowork session in this repo. Read this
first, every session. It governs **how to work here**; `AGENT_CHARTER.md` governs the
deeper *why/when-to-stop*, and `docs/REAL_READINESS_PLAN.md` defines the honest bar.

## What this project is

Cortex Speech is an **offline-first desktop app** (Tauri v2 + Svelte 5 + Rust) for
**Central Kurdish (Sorani)** speech transcription, transcript curation, and dataset export.

- Local ASR: **Meta OmniASR CTC** via **sherpa-onnx** + **Silero VAD** (no cloud needed).
- Optional, **consent-gated** cloud: ElevenLabs Scribe (STT) and OpenRouter (LLM refine, reaches Gemini-class models).
- Storage: SQLite + FTS5 search. ~102 Tauri IPC commands. EN/CKB (RTL) localized UI.
- Workflow: import -> VAD chunk -> ASR -> (optional refine) -> review/annotate -> validate -> verify -> export (JSON/JSONL/CSV/Parquet/HuggingFace/WAV).

## The one law: honesty (non-negotiable)

This project's entire credibility rests on real, never fabricated, results.

- **Never** invent, estimate, round, or "remember" any metric (WER/CER/F1/kappa/RTF/p-value/CI). Every number comes from a **real run of the real harness**, with the exact command + dataset/model SHA pasted into the ledger.
- **Nothing is "done" until it is USER-OBSERVABLE or MEASURED on real audio.** "Tests pass" / "clippy clean" are necessary, not sufficient. Lead with the honest reality, then the progress.
- Don't claim a feature works until its real positive test passes. If a result is bad, report the bad result — the honest number is always shippable; a flattering fake one never is.
- If you cannot verify something here, say so plainly and hand it to the user's machine. Do not imply verification you did not do.

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

- Default is **fully offline**. Cloud LLM and cloud STT are **off by default** and require explicit opt-in (`cloud_llm_opt_in`, `cloud_stt_opt_in`, `jury_cloud_opt_in`). `settings.effective_llm_mode()` downgrades cloud -> none when not opted in; `pipeline.rs` enforces it in both `llm_refinement_permitted()` and `build_refiner()`. **Never** send audio/transcript to a provider without acknowledged consent, and never make cloud load-bearing in the default path.
- Treat **voice as biometric** (GDPR Art. 9): enforce consent + license + attribution before any publish/train/redistribute step.
- **Never** persist or echo API keys. **Never** hardcode private Windows profile paths in any tracked file — `scripts/test_windows_repo_hygiene.py` blocks it (use env vars / repo-relative paths).

## Verify your work (gates)

Run the relevant gates and paste the real output. A fix without a regression gate is incomplete.

- Sandbox-runnable: `npm run test:python-policies` (honesty/privacy/CI/dataset policies), `npm test`, `npm run typecheck`, `npm run lint`.
- User's machine: `cargo fmt/clippy/test --manifest-path src-tauri/Cargo.toml`, `npm run test:e2e`, `npm run tauri build`.
- Full clean gate: `docs/RELEASE.md`. CI: `.github/workflows/{ci,nightly-real-audio,release}.yml`. Open 10/10 items: `docs/HARDENING_PLAN_10.md`.

## Working agreement

- One logical change per commit, **Conventional Commits**, on a **branch** (never straight to `main`); end commit messages with the `Co-Authored-By: Claude` trailer per the charter.
- Save final deliverables into the repo (the selected folder) and share them with `present_files`.
- Don't weaken, skip, or delete a quality gate to make something pass. Don't scope-creep — log out-of-scope ideas instead of implementing them.

## Key map

- Backend: `src-tauri/src/` — `commands.rs` (IPC), `pipeline.rs`, `asr.rs`, `audio.rs`, `db.rs`, `normalizer.rs`, `eval.rs`, `models.rs`, `settings.rs`, `jury/`, `export*.rs`.
- Frontend: `src/App.svelte` (shell) + `src/lib/*.svelte` (`AudioPlayer`, `ReviewMode`, `ReviewInbox`, `ValidationPanel`, `DiffView`, `StatsDashboard`, ...).
- Docs: `AGENT_CHARTER.md`, `ROAD_TO_10.md`, `docs/REAL_READINESS_PLAN.md`, `docs/HARDENING_PLAN_10.md`, `docs/COWORK_PIPELINE_PROMPT.md`.
- Scripts: `scripts/*.py` (dataset build/review + policy gates), `e2e_real_app.cjs` (real-app driver).

## Definition of done (10/10)

Per `AGENT_CHARTER.md`: on a clean checkout, a single `make verify-10` exits 0 and prints
`CORTEX 10/10: ALL GATES GREEN`. Until that command exists and passes, you are not done —
partial completion means keep going, and nothing is called "10/10" on tests alone.
