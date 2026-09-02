# HANDOFF — continue Cortex Speech (2026-07-11)

> **PARTIALLY SUPERSEDED (2026-07-11):** the crash below is FIXED + committed (`f01ab66`). The live
> issues are now: (1) the `clear_db.py` data-loss default (fixed here — snapshot + opt-in), (2)
> segment pagination truncating at 300, (3) app-owned 7B ServerSupervisor, (4) the reliability drill
> matrix. See PROGRESS_LEDGER.md (newest first) for the current state.

> **You are picking up mid-task from another chat session.** Before doing anything, learn the
> context so you don't redo or undo work:
> 1. Read **PROGRESS_LEDGER.md** (newest entries first) — the honest, evidence-cited history.
> 2. Read **docs/SHIP_FINAL_PLAN.md** — the full remaining-work map (workstreams + owner-gated).
> 3. Read **cortex-speech-app/CLAUDE.md** — the one law (never fabricate a metric; nothing "done"
>    until user-observable or measured on real audio) and the owner's ship definition.
> 4. If you can access it, the prior chat transcript is at
>    `%USERPROFILE%\.claude\projects\<cortex-session-dir>\<session-id>.jsonl`
>    (this session did the WS2 merge, the verify-10 aggregator, the desktop icon, and the crash diagnosis below).
> 5. Recall memory: **[[ship-means-personal-use]]** — "ship" = the owner's personal daily use,
>    truly reliable and bug-free; distribution is descoped, but NO reliability/honesty/correctness
>    gate is waived.

Branch: **autonomous-day-20260710** (pushed through `51df16e`). Owner uses the app **tonight** on
this Windows machine (WSL2 + dual RTX 3090 Ti). The 7B champion server must be warm on
`127.0.0.1:8799` in WSL; if down, run `cortex-speech-app/scripts/start_7b_server.ps1`.

---

## ⚠️ DO THIS FIRST — an uncommitted, unverified crash fix is on disk

**Symptom (owner-reported):** the app "crashes" (freezes, fully unresponsive) the moment you click
**Open audio** or **Import**.

**Root cause — CONFIRMED this session** (not a guess): `open_audio_file` and `import_directory` in
`cortex-speech-app/src-tauri/src/commands.rs` were **synchronous** `#[tauri::command]`s calling
`blocking_pick_file()` / `blocking_pick_folder()`. Sync Tauri commands run on the **main thread**, so
the blocking native picker froze the whole UI while open. Proof: with the dialog open, a second
command (`get_settings`) hung for the full 5s timeout → main thread blocked. The e2e harness never
caught this because `e2e_real_app.cjs` calls `import_audio_file` **directly**, bypassing the dialog.
There is **no Windows WER crash event** (checked) — consistent with a freeze, not a native crash.

**Fix already written to `commands.rs` (BUT NOT YET BUILT OR VERIFIED — build was interrupted):**
both commands were converted to `async fn` + non-blocking `.pick_file(move |p| { tx.send(p) })` /
`.pick_folder(...)` with a `tokio::sync::oneshot` channel; `import_directory` now fetches
`app.state::<AppState>()` **after** the await (no `State` borrow held across `.await`). Verified
before editing: `use tauri::Manager;` is present (commands.rs:31), tokio has `features=["sync"]`,
and `pick_file`/`pick_folder` exist in tauri-plugin-dialog 2.7.1.

**Your immediate steps:**
1. `cd <repo-root>` and build the exe the RIGHT way:
   `cd cortex-speech-app && npm run build && cargo build --release --manifest-path src-tauri/Cargo.toml`
   (frontend first — a bare `cargo build` ships a stale UI). If it does not compile, fix the two
   edited functions (most likely: a lifetime/`Send` issue around the oneshot or `as_path()`).
2. Prove the fix with the repro harness (in the prior session's scratchpad; re-create if gone):
   launch the exe with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9355`,
   connect via CDP, `invoke('open_audio_file')` (don't await — the dialog blocks for selection),
   then `invoke('get_settings')` with a 5s race. **PASS = get_settings RETURNS quickly** (main
   thread NOT blocked). Repeat for `import_directory`. Before the fix this hung; after, it must not.
3. `cargo test --lib` (expect 835+ passing) + `npm run typecheck` + `cargo clippy --all-targets -D warnings`.
4. Kill any stale instance + remove `%APPDATA%\cortex-speech\cortex.lock`, then run
   `scripts/cortex_doctor.ps1` → must print READY, and do a real-audio smoke via `e2e_real_app.cjs`
   with a real file (owner's corpus: `F:\Kurdish Sorani Dataset\cortex_audiobook_curated\wavs\*.wav`).
5. **Add a regression gate** (the law: a fix without a gate is incomplete): a test asserting these
   two commands are `async` and do NOT call `blocking_pick_*` — e.g. extend
   `cortex-speech-app/scripts/test_rust_runtime_panic_policy.py` (grep commands.rs for
   `pub async fn open_audio_file` / `pub async fn import_directory` and assert no `blocking_pick`
   in either body). Ideally also a real interactive proof driven by computer-use if available.
6. Commit (Conventional Commits, `Co-Authored-By: Claude` trailer), rebuild at HEAD so the exe is
   fresh, re-run cortex_doctor, push. Update PROGRESS_LEDGER.md with the verbatim repro output.

**Watch for a sibling bug:** any OTHER sync `#[tauri::command]` that opens a dialog or does a long
blocking call will have the same freeze. Grep `src-tauri/src/commands.rs` for `blocking_pick`,
`blocking_` , and long synchronous work in non-async commands; audit each.

---

## Then resume the ship-reliability workstreams (order per SHIP_FINAL_PLAN, personal-use priority)

**WS6a — finish (task #16, in progress):** the 7B server is now committed
(`cortex-speech-app/scripts/cortex_7b_server.py`), and `start_7b_server.ps1` / `cortex_doctor.ps1` /
`launch_cortex.ps1` exist. **Still TODO:** the app-owned **ServerSupervisor** — spawn/health/restart
the 7B server from inside the app + surface engine state in the UI, so the owner never hand-starts it.

**WS6b — reliability drill matrix (task #17):** scripted, repeatable drills, each against a THROWAWAY
profile/DB copy (never the real library), each with a ledger entry: kill-exe mid-import → resume
offered + no data loss; kill-WSL mid-7B → hard fail + rollback, no blank transcript; corrupt-DB →
quarantine + snapshot restore; disk-full → graceful; force-kill-at-40% → resume finishes remainder;
2-hour soak → no memory/handle growth; review-while-import latency <200ms; clean-profile migration
v1..v36. (Design spec was produced this session — check scratchpad / ask; otherwise re-derive from
`snapshot.rs`, `crash.rs`, the resume path in `App.svelte`/`pipeline.rs`.)

**WS3a — run the 37 `#[ignore]` real-model gates (task #18):** they PASS today via the verify-10
aggregator's `ignored-real-model` leg (121s, cloud-key test excluded) — but exercise them
individually and record each. Inventory + exact commands were produced this session.

**WS3b — runtime egress harness (task #19):** replace the static `test_cloud_privacy_policy.py`
source-grep with a socket-interception harness asserting ZERO outbound sockets in default config
across import→T0→T1→T2→debate. This is the `egress-runtime` gate that verify-10 currently reports
NOT-BUILT.

**WS4 — nightly gates (the other NOT-BUILT verify-10 legs):** ~~`refinery-lift`~~ (BUILT
2026-07-25: `tests/refinery_lift.rs`, measured 59.7% CER reduction at 5.5% escalation vs the
≥30%/≤15% thresholds) and ~~`fairness-gender-age`~~ (since built). Also wire `fuzz-smoke`
(5 targets) into nightly — still open (windows-msvc cannot link ASAN; Linux-CI leg).

**WS5 — accuracy levers (optional, lowest priority):** KenLM shallow fusion + pseudo-labeling. The
historical duplication-weighted run reported 7.03% CER on FLEURS-ckb, but it is not current champion
attestation or a defensible SOTA claim; accuracy work remains outside product certification.

**WS7 — codebase health:** reconcile the separate CORTEX workspace checkout (branch
`goal-9.5-upgrades`) FIRST; then pagination/overlap-stitch/engine-pool/ENOSPC; a11y trap; IPC
round-trip tests for the ~102 commands; docs; dependabot bumps.

### verify-10 is the scoreboard
`python scripts/verify_10.py` (or `make verify-10`) runs the personal-use full-charter aggregator:
23 tiered gates, honest verdict (RED / INCOMPLETE / GREEN-PERSONAL-USE), with owner-descoped and
owner-gated legs always printed.

**Gate-status authority is documented in [STATUS.md](STATUS.md).** The current status itself lives
inside the immutable, hash-validated proof bundle; this tracked page never embeds a commit verdict. This
paragraph used to restate the last run's tally here, and it went stale the moment any leg flipped
(it claimed `egress-runtime` and `refinery-lift` were NOT-BUILT for weeks after both shipped).
Run `python scripts/verify_10.py`; its proof-local `STATUS.md` is computed by the same code path that
sets the exit code and is covered by the manifest artifact inventory.

`--static` preserves the CI contract (ci.yml / release.yml call it that way) — never change that.

---

## Owner-gated (cannot be done without the human — surface, don't attempt)
- **#37 Gold Marathon** — ≥500 in-app review decisions (start: one 10-decision session in Review
  Inbox). THE bottleneck; unblocks the first conversational-Sorani accuracy number + 6 other gates.
- Branch protection / signed tag (repo-admin), CER normalization-basis decision, expert
  native-speaker walkthrough, IAA (≥2 annotators), AsoSoft-600 eval-license check.

## Standing rules (CLAUDE.md — non-negotiable)
No fabricated metrics — every number from a real harness run pasted into the ledger. Nothing "done"
until user-observable or measured on real audio. Branch + Conventional Commits, never straight to
main. Never weaken a gate to pass it. No private Windows profile paths in tracked files. Rebuild the
exe after any commit so `check_exe_freshness.py` stays green (the exe embeds its git SHA).
