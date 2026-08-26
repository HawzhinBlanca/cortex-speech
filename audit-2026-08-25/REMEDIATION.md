# Remediation of the 2026-08-25 deep audit

**Audited HEAD:** `1282578` · **14 fix commits + 1 lint cleanup**
**Branch:** the work was made on `integrate/codex-flywheel` and integrated into
`codex/10-10-integration`, which rebased it — the SHAs below are the ones reachable HERE.
The originals (`86719e3`..`e69198c`) survive on `integrate/codex-flywheel` and are the same
content; each was matched to its rebased twin by commit subject and verified reachable.
**Companion:** [DEEP_BRUTAL_AUDIT.md](DEEP_BRUTAL_AUDIT.md) — the findings, preserved verbatim.

Every fix carries a regression gate, per the repo's own law that a fix without one is incomplete.
Nothing below is a claim a real run didn't produce.

---

## Verified state after remediation (real harness output)

| Gate | Before | After |
|---|---|---|
| `cargo test --lib` | 1,542 pass | **1,542 pass, 0 fail, 8 ignored** (429.86s) |
| `cargo clippy --all-targets -- -D warnings` | **4 errors** | **clean** |
| `cargo fmt --check` | clean | clean |
| `npm run typecheck` | 0 errors | **0 errors / 449 files** |
| `npm test` (vitest) | 292 pass | **297 pass / 56 files** |
| `npm run test:python-policies` | 2 of 101 FAIL | **105 of 105 pass** |
| `test_watchdog_enabled.py` (live) | **FAIL — disabled** | **OK (state=Ready)** |
| `check_review_serving_provenance.py` (live DB) | pass | **pass under TIGHTENED invariants** |
| `check_spot_check_pool.py` (live) | FAIL | **FAIL — owner action, unchanged by design** |

The policy suite grew 101 → 105 scripts: four new gates, each with a working `__main__` block
(verified — a policy test without one is counted as passed while asserting nothing).

---

## Phase 0 — the live REDs

1. **Watchdog re-enabled.** `schtasks /change /tn CortexWatchdog /enable`; the gate now prints
   `WATCHDOG GATE: OK (CortexWatchdog state=Ready)`. The rebuild procedure that disabled it must
   re-enable it as its final step rather than leaving it for the sweep to notice.
2. **Ledger entry written** — on `integrate/codex-flywheel` (`f89c521`). It is NOT on this
   branch: `codex/10-10-integration` carries its own ledger entry from the session that
   integrated this work, and the staleness gate passes here on that one.
3. **Hidden-check capacity — NOT fixed, and deliberately so.** The gate's own contract says it:
   *"Fixing it is an owner action, not a code change."* Fabricating answer keys is forbidden. What
   the live database says, so the action is concrete rather than a shrug:
   - 53 candidate keys exist (`verified = 1`, `reviewed_by IS NULL`), **0 of them inside the active
     focus** — they are ZarPodcast/Lamo clips outside the 6,922-id campaign.
   - Requirement stands at **1,107 keys per reviewer** for 6,920 accessible work clips at the 1-in-8
     cadence, for each of 4 active reviewers.
   - Only two honest remedies: adjudicate clips **at the desktop, inside the focus** (which leaves
     `reviewed_by` NULL and mints keys), or narrow the campaign/roster before opening links.

---

## What was fixed — 53 of 55 findings, 14 commits

| Commit | Findings closed |
|---|---|
| `86719e3` | **H2** champion hard stop reported, not swallowed |
| `d2e3b0e` | **M10** + 3 lows — decisions written back by id; blank text no longer masks the draft |
| `3f851a6` | **M7 + M8** + 2 lows — wrong-model probe FAILs; meta-gate sees unittest policy files |
| `4729ba9` | **H4** duplicate detection across sample rates |
| `47e501e` | **M14 + M15** + 1 low — decision-bearing exemptions; registry, not the startup mirror |
| `edf8bbb` | **H5 (visibility)** uncredited second-pass work surfaced |
| `d5b6fde` | **M3** + 2 lows — Halwest splits at the source recording |
| `e682c67` | **H6** + 1 critic lead — gold eval hard-stops; external hypotheses labelled |
| `00f1f21` | **M1 + M2 + M11(couch)** + 2 lows — hidden-check durability, case-insensitive identity |
| `eec4d01` | **M11(db)** + 7 lows — one placeholder authority; blank guard at the shared boundary |
| `040e04b` | **M4 + M5** + 1 low + 2 critic leads — honest bundle counts, thresholds unified |
| `51058eb` | **M6** + 3 lows — instant spawn failures charged now; WSL7B integrity hole closed |
| `e7f75e7` | **H3 + M12** + 1 low — headless importer gets cross-run dedup |
| `d759515` | **M13** + 6 lows — span-divergent doubled generations refused; legacy IPC deleted |

### The two not fixed as written, and why

**H5 — pool/blinded second-pass work mints no pay.** The gate now counts and loudly reports
playback-evidenced decisions carrying no ledger credit, per reviewer with duration. It **mints
nothing**. Adding a payable surface is a canon change requiring the owner's literal
`change canon:`; whether a non-canonical second pass is payable is his call, not an agent's.

> **Correction to the original report, on live evidence:** `review_pool_decisions` and
> `independent_review_decisions` are both **0 rows**. The defect is real and the wiring is
> genuinely absent, but **no reviewer has been shorted**. The exposure is prospective. The
> original report implied a live loss; that was wrong, and the gate is now armed to catch the
> moment it stops being theoretical.

**Phase 4 — the challenger loop.** Got further than the audit expected, and found a deeper wall.

---

## Phase 4 — how far it actually goes, and the real blocker

Both scorecards are real measured runs over a byte-identical frozen manifest
(`eval_manifest_sha256 ed713075…`):

- champion `omniasr-7b-legacy-c348ade8a816` — **micro CER 7.913%**, N=348
- challenger `omniasr-7b-challenger-eb0105fdb6a5` — **micro CER 7.556%**, N=348

The challenger's provenance sidecar already existed. The champion's did not — which is why no
verdict was ever producible. It now does: emitted with the repo's own
`emit_scorecard_provenance.py`, which re-hashes **every** component from disk and fails closed
against the manifest's pinned hashes. The adapter hashes to `c348ade8a816…`, matching the
champion's own model id and `champion.json`'s `deploymentSha256` pin exactly.

**The wall:** `promotion_gate.py` **requires** `--slices`, and `build_eval_slices.py` refuses:

```
EVAL SLICES: REFUSED - none of the 348 manifest row(s) name a clip in this library
```

Protected slices derive from **library** metadata (speaker, dialect, SNR, source). The frozen
FLEURS eval lives in `gold_segments`, which carries only `id, audio_path, reference, is_holdout,
created_at, audio_content_hash` — no speaker, no SNR. So the loop is **structurally unclosable on
the corpus canon designates as the frozen eval set**. That, not "nobody ran a cycle", is why
`runs/` has zero verdicts and `challenger-loop` has been RED all along.

Two honest ways out, both owner decisions: enrich the gold set with the metadata slices need, or
authorize an in-library holdout eval set for promotion comparisons. **Weakening the slice
requirement is not on the list.** A PROMOTE verdict would also not promote anything — canon fixes
the champion; registration is a separate, owner-gated act.

---

## Defects found *while fixing*, not in the original audit

1. **`review_pool.rs` had 2 clippy errors and had never been linted.** It arrived in `e2b256c` —
   one of the 7 unledgered commits — and is absent at `21c639d`, the last fully-swept commit
   (verified with `git cat-file`). So the unledgered-commit problem was never mere bookkeeping:
   those commits also skipped the lint gate. Both fixed.
2. **A policy pin was requiring the bug.** `test_rust_runtime_panic_policy.py` listed
   `remove_lock_file(&lock_path, "failed Unix instance lock acquisition")` as **required** — the
   very call by which a refused second instance deletes the *live holder's* lockfile. Moved to
   `forbidden` in the same commit as the fix.
3. **Two agent overreaches, caught by existing tests and corrected:**
   - `quality.rs` excluded *placeholders* as well as rejects from the quality counters. Only the
     permanent reject was the documented defect; a placeholder nobody rejected is clearable work
     and a dataset still holding it is genuinely incomplete. Suppressing it is the same dishonesty
     pointed the other way. Narrowed to rejects; the two tests that pin the intent pass.
   - `db.rs` added a reviewed-baseline guard to the generic upserts that made a conflicting upsert
     a **silent no-op**, contradicting the pinned v60 contract (machine fields update, review truth
     preserved) and the file's explicit-error design. Removed.

## A methodology error of mine, recorded because the repo's law demands it

I first reported **5 `pipeline` tests as pre-existing failures**. That was **wrong**. I ran the
suite with `CARGO_TARGET_DIR=scratch-target` to dodge the running app's DLL lock, and
`resolve_wsl_7b_client` locates the bundled champion script by walking up from `current_exe()` —
the moved target dir put `scripts/cortex_7b_client.py` out of reach. My "baseline" comparison used
the same flag, so it reproduced the artifact and looked like proof. Against the normal target dir
**all 5 pass**. There are no pre-existing test failures. The lesson is the repo's own:
a harness that changes the environment can manufacture the failure it then reports.

---

## What remains, honestly

**Still RED, owner action only:**
- `spot-check-pool` — 1,107 keys/reviewer needed; only desktop adjudication inside the focus mints
  them. Never fabricate keys.
- `challenger-loop` — blocked on the slice/gold-metadata decision above.

**Still owner-gated (unchanged by this work):** `iaa-kappa-ceiling` (≥2 independent Sorani
annotators), `cordi-dialect-fairness`, `refinery-lift-in-product` (Gold Marathon, ≥500 real review
decisions), plus the 9 descoped distribution legs.

**Open owner decision:** whether pool / blinded-second-pass review work is payable
(`review-iqd-v1-2026-08-21`). Currently unpaid, now visible, zero rows affected so far.

**The honest bar remains what the harness prints.** `verify_10.py` in full mode, at the release
commit, on this machine, is the only thing that can say `GREEN — PERSONAL-USE SHIP-READY`. This
document is not that, and does not claim to be.
