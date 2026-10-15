# v12 as a champion CANDIDATE + GPU-1-only device policy (plan, 2026-10-08)

- **Status:** owner approved building the mechanisms (2026-10-15, see the Approved-by lines). Implementation of Part A and
  Part B steps 1-2 is on this branch (see its git log); Part B steps 3-6 and every owner ruling below remain open.
- **Branch:** `claude/v12-candidate` (worktree `D:\cortex-worktrees\v12-candidate`, from `origin/main` `868c4983`).
- **Owner's direction (2026-10-08):** v12 enters **as a candidate through the normal promotion process**, not as a direct swap ("option a"). Separately, make the 7B service use GPU 1 only.
- **Rules followed:** `docs/OWNER_CANON.md`, `AGENT_CHARTER.md`, `cortex-speech-app/CLAUDE.md`. Nothing here weakens a canon pin or a gate. Every canon edit below is listed for the **owner to write** as `change canon: <item>`.

## v12 facts
- **The checkpoint:** one fused fairseq2 checkpoint (OmniASR-7B, `wav2vec2_llama`/`7b`, vocab 10288):
  `/home/ai/kurdish_models/omni_7b_v3_continuation/runs/champion_v12_lora64_750_fused/kurdish_omniasr_7b_v12_c750.pt`.
  It is 15,610,646,560 B, with its sha256 in that folder's `SHA256SUMS`.
- **The tokenizer** is byte-identical to the champion's.
- **No adapter, no flywheel run:** it is a rank-64 LoRA trained offline on a fused v11 and then fused.
- **Training data** includes the **FLEURS train split**. The test split was excluded with a fuzzy leakage gate.
- **External measurement:** FLEURS test 23.29 WER / 5.50 CER, beam 5, through the omniasr-v11-audit server. Under the canon's honesty rule this **cannot be cited here** until a Cortex harness reproduces it.

## Part A: GPU 1 only (small; can go first)
**Problem.** `scripts/cortex_7b_server.py:592-640` `parse_device_indices`:
- auto mode takes **every** GPU with at least 20,000 MiB free, so it seizes GPU 0;
- an explicit list is honoured with no checks;
- `CUDA_DEVICE_ORDER` is never set, so nvidia-smi and CUDA indices can disagree.

**Plan:**
1. In `parse_device_indices`, the allowed set becomes `{1}` and the default `[1]`.
   - An explicit list containing 0 → refuse to start, with a clear message.
   - Keep the free-VRAM floor, so a busy GPU 1 means **refuse, never seize** (canon :19-20).
2. Set `CUDA_DEVICE_ORDER=PCI_BUS_ID` in `main()` before workers fork.
3. Update the docs that still say "both GPUs":
   - `start_7b_server.ps1:15-16`
   - `.claude/skills/cortex-operator/MEASUREMENTS.md:97, 139, 265`
4. Add tests in `test_cortex_7b_protocol.py` (mocking `free_vram_mib`) for: the default `[1]`, 0 refused, a busy GPU 1 refused, and the env var set.
5. **Not in scope unless the owner says so:** the trainer default `CORTEX_OMNI7B_GPUS=2` (`omni7b_trainer_adapter.py:231, 298-304`) and `run_challenger_cycle.py --challenger-gpu 1`.

**Consequences:**
- one replica, roughly half the current throughput;
- a challenger can't share the card with the champion;
- the omniasr :8000 server (14.6 GB on GPU 1) must be stopped while Cortex's 7B service runs.

**Canon:** "GPU 1 only, never GPU 0" is **not currently a canon item.** For a gate to pin it, the owner must write `change canon: add GPU 1 only`, and then a pin goes in `test_owner_canon_pins.py`.

**Approved-by (Part A):** owner, 2026-10-08: "yes do all 3, cortex option a"; and 2026-10-15: "no, we wanna
make v12 ready for use everywhere, not just wsl and this app, inside hawedit and cortex too", choosing "Approve all"
to "Approve building all of it (PRs with tests, nothing promoted until your canon lines)?" — which covers the
GPU-1-only fix. Approved as **code + tests only**: GPU 1 only is not a canon item until the owner writes
`change canon: add GPU 1 only`; a pin in `test_owner_canon_pins.py` can be added then.

## Part B: v12 as a candidate (blocked on owner decisions)
On `868c4983`, five things block a fused, non-flywheel v12 from reaching canary and promotion:

1. **Registration is flywheel-only** (`registry.rs:208-210, 720-722`), and there is no external or offline provenance kind. Inventing a flywheel record would fabricate provenance; `bootstrap_legacy_champion.py:10-11` forbids that.
2. **Deployments must be base + adapter.**
   - `deployment.rs:99-114` requires the adapter fields.
   - `cortex_7b_server.py:374-382, 723-725` always loads through PEFT.
   - The health check in `engine_runtime.rs:717-731` and `cortex_7b_client.py:271, 280` requires adapter hashes.
3. **The promotion gate cannot judge v12.**
   - `promotion_gate.py:137-152` requires the champion's base sha and snapshot.
   - `build_eval_slices.py:259-266` refuses FLEURS (not library clips).
   - The library-held-out set is only 11 clips across 4 recordings (`run_challenger_cycle.py:23-27`); the gate needs at least 30 paired clips.
4. **There is no way to start a promotion.** `start_promotion` has no production caller, by design (`champion_promotion_runtime.rs:3-5`, `test_promotion_trust_boundary.py:43-58`).
5. **Promotion pauses the live review pool for good** (`review_pool.rs:439-443`). The pool registry is a single row guarded by triggers (`migrations/mod.rs:3931-3961`), so a new pool needs a migration. Also, `check_review_serving_provenance.py` turns RED for every untouched legacy-drafted clip until it is re-drafted.

**Smallest canon-compliant route** (each step is a PR, with tests, through CI):
1. **Fused deployment shape:** optional adapter fields, or a schema-2 manifest, in `deployment.rs`, the server verifier and worker (skip PEFT), `engine_runtime.rs` health, and `cortex_7b_client.py`.
2. **An honest `external_offline` provenance kind**, modelled on the legacy bootstrap:
   - `training_provenance: "external_unverified"`, the training-manifest sha, the parent (v11) sha and an evidence sha;
   - id `omniasr-7b-external-<sha[:24]>`;
   - added in lockstep across both verifiers, `registry.rs` registration and gating, health and the client.
3. **A real held-out evaluation set inside the library:**
   - at least 30 clips, with at least 5 per slice, proven disjoint from the v11 and v12 training manifests;
   - the champion and v12 scored one after another on GPU 1;
   - `promotion_gate.py` binds v12 by its deployment hashes instead of a run record.
4. **Canary:** `build_promotion_canary.py` builds the suite and its sha is recorded. The canary is identity-only; quality rests on step 3.
5. **An owner-only promotion entry point** (none exists today).
6. **In the same change as the promotion:**
   - update `LOCKED_CHAMPION` in `check_champion_supervision.py:30, 88` and its pin in `test_champion_supervision_policy.py:52`;
   - re-draft the untouched clips;
   - add a new-pool migration or tool;
   - schedule it **after the current review pool finishes**.

## Owner decisions needed (agents may not make these)
1. **FLEURS.** Canon line 34 says "never train on it" without naming a split, and v12 trained on the train split. Choose one:
   - **a)** write `change canon: FLEURS — train split may be used for training; the test split stays the frozen eval set`;
   - **b)** v12 is not eligible, and a v12-without-FLEURS-train must be trained first (about 12–15 h on the omniasr side).
2. **GPU rule:** `change canon: add GPU 1 only` (and say whether it also covers the trainer), or approve Part A as code-only.
3. **Approve the new mechanisms:**
   - the `external_offline` provenance kind;
   - fused (no-adapter) deployments;
   - replacing the gate's same-base / same-snapshot requirement with deployment-hash binding;
   - an owner-only promotion entry point.

   These are not canon items, but each changes a quality gate (`AGENT_CHARTER.md:89`).
4. **Timing:** the "2026-09-24: current pool finishes on legacy" decision is in the owner's vault but **not in this repo**. Should it be written into canon so it binds the promotion timing?
5. **`cortex-speech-app/CLAUDE.md:30-46` ("Never change the production AI model")** needs an owner-worded exception for this promotion. Canon :11 still holds, because v12 is a Sorani OmniASR-7B.

**Approved-by (Part B):** owner, 2026-10-08: "yes do all 3, cortex option a" (v12 enters as a candidate through
the normal promotion process); and 2026-10-15: "no, we wanna make v12 ready for use everywhere, not just wsl and
this app, inside hawedit and cortex too", choosing "Approve all" to "Approve building all of it (PRs with tests,
nothing promoted until your canon lines)?": fused-model deployments, an honest `external` provenance kind, a
promotion gate bound to deployment hashes, and an owner-only promotion command. **Mechanisms only:** the FLEURS-train
and CORDI share-alike rulings are still pending, so nothing registers or promotes v12.
