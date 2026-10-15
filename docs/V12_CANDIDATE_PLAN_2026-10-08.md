# v12 as a champion CANDIDATE + GPU-1-only device policy (plan, 2026-10-08)

- **Status:** owner approved building the mechanisms (2026-10-15, see the Approved-by lines). Part A and Part B steps
  1-5 are implemented and run on this branch (see its git log and "Runbook steps 6-9, run 2026-10-15" below).
  **The deployment-bound gate verdict for v12 is `REJECT`** on the library held-out set. Not done: step 6 and every
  owner ruling below. Nothing is registered or promoted.
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

## Implemented on this branch (2026-10-15)
- **Part A:** `parse_device_indices` allows only GPU 1 (default `[1]`), refuses any list naming GPU 0 or
  another GPU, applies the free-VRAM floor to explicit and default GPU 1 alike (busy or unreported GPU 1
  refuses, never seizes), and pins `CUDA_DEVICE_ORDER=PCI_BUS_ID` before workers fork. The trainer default
  is unchanged.
- **Part B step 1 (fused shape):** chosen form is a **schema-2 manifest**: `base_checkpoint` is the fused
  `.pt`, the adapter keys are forbidden, and identity is the manifest's exact size + SHA-256 of that `.pt`
  and the tokenizer (no sidecar file has authority). Schema-1 adapter manifests are unchanged. The worker
  skips PEFT entirely for schema 2. Health identity is the four components, or exactly `{base, tokenizer}`.
- **Part B step 2 (`external_offline`):** provenance
  `{kind, training_provenance: "external_unverified", training_manifest_sha256, parent_checkpoint_sha256,
  evidence_sha256}`; the model id must equal `omniasr-7b-external-` + the first 24 hex of
  `sha256(canonical {training_manifest_sha256, parent_checkpoint_sha256, evidence_sha256})`. Both verifiers,
  health, the direct transport and the client accept the kind; `registry.rs` registers it as a
  **candidate only**. `refuse_external_offline_promotion` blocks it in `gate_and_promote` and in the
  promotion saga's registry verification; the legacy bootstrap and `promotion_gate.py` (now an explicit
  `kind == "flywheel"` check) refuse it too. `scripts/write_external_deployment.py` writes the manifest
  from the real files (it hashes them; no hash is ever an argument); nothing here registers or promotes v12.
- **Part B step 3a (held-out set):** `scripts/build_heldout_eval_set.py --train-manifest ... --out-dir ...`
  takes human-verdict library clips (`human_decision` accept/edit; the reference is `verdict_transcript`)
  and drops any clip that a given training manifest contains. Text leakage uses omniasr-v11-audit's `canon`
  with spaces removed and `difflib` ratio ≥ 0.85; retrieval is a quick_ratio upper bound, so it never
  misses a pair. Audio leakage is the sha256 of every clip and of its source recording, checked against every
  training audio file; a missing training file refuses the build. The tool then prunes slices below 5 until
  none remain and writes the eval TSV, the gate-bound `eval_slices.tsv` and a proof JSON.
  **Run 2026-10-15** against a read-only copy of the live library and the v12 manifest plus the 7 v11-lineage
  manifests in `audit_common.py` `TRAIN_MANIFESTS` (337,526 training rows, 237,847 audio files, none
  missing). Of 2,770 human-decided clips, 142 had sources libsndfile cannot read (`.mov`/`.mp4`), 0 leaked
  by text, 0 by audio, and 2,148 sat in groups below the slice floor. That left **480 clips in 9 slices**:
  - 3 sources, with 409, 64 and 7 clips;
  - dialect: sorani 409, hawleri 71;
  - audio: clean 480.

  Eval manifest sha256 `449cd034d02ca2b22ec865011631e7ab0858f15154fa304741855cfb81cede96`. The set is local
  and gitignored (`runs/heldout_eval_v12/`), because it is voice data. One recording supplies 409 of the
  480 clips, so the overall CER is dominated by that recording; each recording keeps its own slice.
- **Part B step 3b (gate binding):** `promotion_gate.py --champion-deployment/--challenger-deployment`
  binds both models by their deployment manifests:
  - the manifest file sha256;
  - the canonical `manifestSha256`;
  - every component, re-hashed at its stated size.

  It uses the same `_measure` checks as flywheel runs; flywheel reports are byte-identical. A pass is
  `JUDGED_PASS` (exit 3, `auto_promotable: false`), never `PROMOTE`.
- **Part B step 5 (owner-only entry point):** `owner_promote` (`src-tauri/src/bin/owner_promote.rs`,
  `owner_promotion.rs`). Design: the repo has no signing key or owner token, and the one owner-only
  channel it already has is the canon. A promotion therefore needs:
  - **(1)** an owner decision record committed byte-identically on the protected `origin/main`;
  - **(2)** the `change canon:` line it quotes, naming the candidate model id **and** full deployment sha256,
    present in `docs/OWNER_CANON.md` on `origin/main` (agents may never edit that file);
  - **(3)** a phrase naming that deployment, typed on an interactive terminal (piped input is refused).

  Then:
  - It also needs a `JUDGED_PASS` deployment-bound verdict for exactly those identities, on the set that the
    held-out proof describes; the registry champion as the incumbent; the registered candidate's exact sha;
    and the canary suite file. It takes the instance lock, so Cortex must be stopped.
  - It **admits** the durable saga job, with the record's sha256 pinned in the contract, and exits. The next
    Cortex start runs the saga through startup recovery: DB swap, pointer, restart, exact-identity health,
    canary, and rollback on any failure.
  - Recovery lifts the `external_offline` refusal only for the exact candidate named by a stored record
    that still hashes to the pin. `gate_and_promote`, the bootstrap and every other path still refuse
    unconditionally.

  **Limit:** this is a procedural barrier, not a cryptographic one. An agent with the owner's GitHub
  rights and a TTY could forge all three. Requiring signed commits on the canon line is the upgrade.

## Runbook steps 6-9, run 2026-10-15 (agent; results exactly as the tools printed them)
- **Step 6, manifest (not registered).** The plan's approval says "nothing registers or promotes v12", so v12 is
  **not** registered, not even as a candidate. In WSL:
  `python3 cortex-speech-app/scripts/write_external_deployment.py --checkpoint .../champion_v12_lora64_750_fused/kurdish_omniasr_7b_v12_c750.pt
  --tokenizer /home/ai/cortex_v12_candidate/omniASR_tokenizer_written_v2.model --parent-checkpoint
  .../champion_v11_supreme_fused/kurdish_omniasr_7b_v11_champion.pt --training-manifest
  <omniasr-v11-audit>/data/v12_train.jsonl --evidence
  <omniasr-v11-audit>/release/kurdish-omniasr-7b-v12/SHA256SUMS --out
  /home/ai/cortex_v12_candidate/deployment_manifest.json` printed:
  - `modelVersionId` `omniasr-7b-external-2e659f9fcda9f7fdca7a9922`;
  - `deploymentSha256` `dae2307fbcd5f0ccb31523030b20212826da6bead0e7a48cea51e6f1df0cf977`, `manifestSha256`
    `89675a7ad4c0a354d60b608a94a90a483ce20d2ba37b6a2fab4ffdc1b4c7f52a`;
  - checkpoint `f7b3a5e2…5ff5fb2` (15,610,646,560 B), tokenizer `8aa11a10…08240b1e`, parent (fused v11)
    `93d8b1da7cbc98878aec75e1ef4c65f0c1f2ba67dd014e2e08f574176d7abfc0`, training manifest
    `464a31d758519a9e6b74bae2c8ccf1a70af62b50929e1c8ce8839d2f5171c1fc`, evidence (release `SHA256SUMS`, which pins
    `MODEL_CARD.md` and every eval file; `sha256sum -c` passed) `254b6b27beca3f9571c160b404c255fee7e533b0813f4184de0157afaac7fdd0`.
  - `cortex_7b_server.py --verify-deployment ... --expected-sha256 dae2307f… --expected-model-id ...` printed
    `"status": "verified"`, `"provenanceKind": "external_offline"`.
- **Smoke run (first real fused load), GPU 1, omniasr :8000 paused with a live `MAINTENANCE` PID.**
  `CORTEX_7B_CHAMPION_POINTER=<scratch pointer> CORTEX_7B_PORT=8898 CORTEX_7B_DEVICES=1 python cortex_7b_server.py`.
  The first attempt verified every byte, then died: fairseq2 looked for `/mnt/f/Kurdish%20Sorani%20Dataset/...`
  (fixed in `fix(7b-server)`, test-first). After the fix it served; `cortex_7b_client.py --health
  --expected-pointer` returned `__HEALTH__` with `modelVersionId omniasr-7b-external-2e659f9fcda9f7fdca7a9922`,
  `provenanceKind external_offline`, `worker gpu1`, and 3 clips transcribed through the client protocol with the
  identity validated on every reply.
- **Step 7, scoring** (`scorecard_7b.py`, `CORTEX_7B_WORKERS=1`, one model loaded at a time; 0 SKIP each). The
  champion is the **registry** champion `omniasr-7b-legacy-c348ade8a816` (deployment `ae33143e…`), served from its
  byte-exact copy `/home/ai/cortex_champion_model_backup_legacy/` because `/home/ai/cortex_champion_model/` now holds
  the unregistered v6 (see the Warning below). Printed:
  - v12: `micro CER = 12.31%   95% CI [11.21%, 13.67%]   N=480`, `micro WER = 34.06%   95% CI [32.52%, 35.61%]`;
  - champion: `micro CER = 5.43%   95% CI [4.75%, 6.17%]   N=480`, `micro WER = 15.09%   95% CI [13.38%, 16.86%]`.
  Sidecars: `emit_scorecard_provenance.py --scorecard ... --role ... --deployment ... --eval-manifest
  runs/heldout_eval_v12/heldout_eval.tsv` (schema 2, new in `feat(promotion-gate)`).
- **Step 8, gate.** `promotion_gate.py gate/champion/omni7b_results.tsv gate/v12/omni7b_results.tsv --eval-manifest
  runs/heldout_eval_v12/heldout_eval.tsv --slices runs/heldout_eval_v12/eval_slices.tsv --champion-deployment
  /home/ai/cortex_champion_model_backup_legacy/deployment_manifest.json --challenger-deployment
  /home/ai/cortex_v12_candidate/deployment_manifest.json --out gate/v12_vs_champion_verdict.json` printed
  **`REJECT`** (exit 1): "CER did not improve (0.0543 -> 0.1231)", "WER did not improve (0.1509 -> 0.3406)", and
  every one of the 9 protected slices REGRESSED in CER and WER. Verdict sha256
  `680b260b2d9fe526a3d13ee2c8be6da0c098c924ab282bd811e89d615adcba15`; all files in gitignored
  `runs/heldout_eval_v12/gate/`.
- **Step 9, canary.** `build_promotion_canary.py --suite-id v12-promotion-canary-v1` wrote 8 cases to
  `%APPDATA%\cortex-speech\promotion_canaries\v12-promotion-canary-v1.json`, `suiteSha256 =
  2ec01c06fb0cfa637670f26f88c0dd9034af79200e3619d2452ef0795d5a52bf`.

> [!WARNING] Measured, not resolved here
> - **The registry champion's manifest path no longer holds its bytes.** The registry and `champion.json` pin
>   `omniasr-7b-legacy-c348ade8a816` at `/home/ai/cortex_champion_model/deployment_manifest.json` (`ae33143e…`); that
>   file is now v6 (`0eecab8e…`, model id `omniasr-7b-champion-v6-ef757e58ee57`), so a pointer-bound start refuses.
>   The legacy bytes survive in `/home/ai/cortex_champion_model_backup_legacy/`. v6 is not registered and was not scored.
> - **nvidia-smi does not see the 7B allocations on this rig right now:** GPU 1 read 1139 MiB used with v12 loaded
>   and 1105 MiB with the champion loaded (while omniasr's torch reports 14.55 GB). The server's "busy GPU 1 refuses"
>   floor therefore cannot see a co-resident 7B; pausing omniasr by hand remains necessary.

## How v12 would be promoted once the owner's canon lines land (runbook)
Only the owner can do steps 1-5. An agent may prepare steps 6-9 but may not run 10. Steps 6-9 were run on
2026-10-15 (above): the verdict is **REJECT**, so step 10 cannot proceed for this v12 deployment
(`owner_promote` requires `JUDGED_PASS`).
1. **FLEURS-train ruling:** write `change canon: FLEURS — train split may be used for training; the test
   split stays the frozen eval set`, or rule v12 ineligible (decision 1 below).
2. **CORDI share-alike ruling:** v11-lineage `next_phase_train` contains CORDI rows. Rule whether training on
   a share-alike corpus is acceptable for a promoted model (`AGENT_CHARTER.md` forbids share-alike corpora in
   a training target).
3. **GPU-1 canon line:** `change canon: add GPU 1 only`, or confirm that code-only is enough (decision 2).
4. **Model-change exception:** owner-worded exception to `cortex-speech-app/CLAUDE.md` "Never change the
   production AI model" for this promotion (decision 5).
5. **Pool timing:** confirm that the current review pool has finished on the legacy champion, or schedule
   the promotion after it (decision 4). Promotion pauses the live pool for good.
6. Write v12's schema-2 `external_offline` deployment manifest (`write_external_deployment.py`), verify it,
   and register it as a candidate (registration only after the owner's rulings).
7. Score the champion and then v12 on GPU 1 over `runs/heldout_eval_v12/heldout_eval.tsv` (`scorecard_7b.py`),
   then write schema-2 deployment sidecars (`emit_scorecard_provenance.py --deployment`).
8. Run `promotion_gate.py ... --champion-deployment ... --challenger-deployment ...` where the checkpoint bytes
   are (WSL). It must print `JUDGED_PASS`.
9. Build the canary suite into `<data_dir>/promotion_canaries/` (`build_promotion_canary.py`).
10. Owner: write the canon line naming `omniasr-7b-external-<24hex>` and its deployment sha256. Commit a
    record under `docs/owner_decisions/` through a PR, then run `git fetch origin`. Stop Cortex and run
    `owner_promote --repo . --authorization docs/owner_decisions/<file>.json --verdict <verdict.json>
    --eval-proof runs/heldout_eval_v12/heldout_eval.proof.json --canary-suite-id <id> --data-dir <data_dir>`,
    typing the phrase it prints. Start Cortex. In the same change, step 6 of Part B: update `LOCKED_CHAMPION`
    and its pin, re-draft the untouched clips, and set up a new pool.

    The record has exactly these fields (unknown fields are refused):
    ```json
    {"schema": 1, "decision": "promote_champion", "family": "omniasr-7b",
     "incumbent": {"modelVersionId": "<champion id>", "deploymentSha256": "<registry sha>"},
     "candidate": {"modelVersionId": "omniasr-7b-external-<24hex>", "deploymentSha256": "<registry sha>"},
     "canonLine": "change canon: <text naming the candidate id and its full deployment sha256>",
     "ownerWords": "<the owner's words>", "date": "YYYY-MM-DD"}
    ```

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

## Gate result caveat (2026-10-15): the held-out set favours the incumbent drafter
**Reading-only analysis** of the 480 clips, using a read-only copy of the live library DB (`speech_segments`) and the gate's per-clip distances (`runs/heldout_eval_v12/gate/{champion,v12}/omni7b_results.tsv`).
- **Who drafted the references:** 473 of 480 were drafted by `omniasr-wsl-7b`, and 7 by `finetuned-mms-ckb`.
- **Accepted drafts:** `human_decision`: 210 `accept`, 270 `edit`. In all 210 accepts, `verdict_transcript` equals the raw draft, so **44% of the references are the drafting model's own output.**

| Subset | n | Champion CER (exact) | v12 CER (exact) |
|---|---|---|---|
| accept (reference = draft) | 210 | 0.34% (177) | 8.20% (8) |
| edit (human-corrected draft) | 270 | 8.95% (0) | 15.14% (0) |

**Consequences**
- The REJECT verdict mixes model quality with reference provenance. "Accept" clips measure agreement with the incumbent's own drafts.
- "Edit" clips keep the draft's wording and colloquial spelling wherever the reviewer left it; examples are Arabic ة/ه, نەوعه and اها.
- The champion's training provenance is `unverifiable` (legacy_bootstrap). Overlap between its training data and these clips cannot be excluded, and the leakage check covered only the v12/v11 manifests.
- Even on the edited clips, v12 is worse under Cortex's conventions (15.1 vs 9.0 CER). This is consistent with v12's known weakness on conversational speech: CORDI conversational CER 26.7, against 2–5 on read speech (omniasr-v11-audit `RESEARCH_PLAN.md`).

**Fix the evaluation design before relying on this gate for any external model:**
- (a) Exclude `accept` clips, or score them separately.
- (b) Add a reference set transcribed from scratch, not post-edited from a model draft.
- (c) Report convention-neutral metrics, such as Kurdish character normalization and spacing-neutral CER. The scorecard currently keeps only distances, so it would also need to store hypotheses.

**Recommendation:** do not promote v12 into Cortex. The in-domain route is to fine-tune on Cortex's human-edited library clips, holding out by recording.
