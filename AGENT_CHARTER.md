# Cortex Speech — Autonomous Agent Charter

> Generated 2026-06-17. Reload this file at the start of every working session. The roadmap in [docs/ROADMAP_TO_10.md](docs/ROADMAP_TO_10.md) is the source of truth for *what* to build; this charter governs *how* you work and *when* you stop.

## Mission
Turn Cortex Speech into a genuine 10/10 best-in-class tool: a fully-offline, git-versioned, signed, auto-updating Central Kurdish (Sorani) speech-transcription and dataset-curation desktop app whose every public claim is independently reproducible by a stranger AND legally + ethically redistributable. Accuracy, ethics, and verifiability must all be GATED in CI, never merely asserted. The project's entire credibility rests on REAL, never fabricated, accuracy numbers — honesty is the non-negotiable foundation. The full charter is saved at the repository root (`AGENT_CHARTER.md`) and is reloaded at the start of every iteration.

## Definition of Done (the objective 10/10 gate)

> **Owner amendment (2026-07-10) — what "ship" means:** Ship = the OWNER'S PERSONAL USE on his
> own machine: a truly reliable, bug-free daily tool. Distribution legs (signed installer,
> winget/Homebrew/Flathub, auto-updater hosting, HF publishing, macOS) are DESCOPED by owner
> decision and do not block ship. Nothing else is waived — every honesty, privacy, reliability,
> accuracy, and correctness gate remains mandatory and unweakened.

- DONE means: on a clean checkout of main, a single command `make verify-10` exits 0 and prints `CORTEX 10/10: ALL GATES GREEN`. Until that command exists and passes, the agent is NOT done; partial completion (e.g. 9/10) means keep going.
- Git + integrity: repo under git on protected main with a signed tag; tauri.conf.json/package.json/src-tauri/Cargo.toml versions byte-equal the canonical CHANGELOG version (2.1.0, currently 2.0.0 in manifests); declared SPDX byte-equals LICENSE+NOTICE (PolyForm-Noncommercial-1.0.0, relicensed 2026-07-14); Cargo.toml repository URL is the real remote (currently placeholder github.com/cortex/kurdish-speech).
- Sorani-aware metrics: wer::normalize_for_metrics routes through SoraniNormalizer + Unicode NFC identically for ref and hyp (today wer.rs:20 only lowercases/collapses whitespace); golden tests prove Kaf/Yeh/ZWNJ-equivalent strings score 0 WER; compute_wer/compute_cer match jiwer on a fixed fixture; micro AND macro WER emitted; bootstrap CI + MAPSSWE match a reference implementation.
- ASR-on-gold runner: a backend command iterates gold_segments, loads audio_path, runs the live sherpa-onnx OmniASR CTC engine with language='ckb' wired FIRST, produces hypotheses with zero caller-supplied text (today run_gold_eval at eval.rs:150 takes caller hypotheses and runs NO ASR), and persists per-segment results recomputable-from-DB across migrations.
- Published scorecard: commit-pinned CER (primary) + WER on FLEURS ckb_iq + AsoSoft-600, AsoSoft-normalized, 95% blockwise-bootstrap CIs, MAPSSWE p<0.05 vs a real **SeamlessM4T-v2** baseline (ci_high < baseline_ci_low) — stock Whisper-large-v3 is NOT a valid Sorani baseline (~98 CER on FLEURS ckb), so significance is tested against a Sorani-capable system — each number citing the measured inter-annotator-agreement (kappa) label-noise ceiling.
- Offline egress proven at RUNTIME: a socket-interception harness asserts zero outbound sockets in default config across the full T0->T1->T2->debate jury on an escalation-requiring fixture AND across the auto-updater; cloud_call=false on every segment row; a separate test asserts no audio/transcript leaves without acknowledged Gemini consent (today scripts/test_cloud_privacy_policy.py is a static source-grep, not runtime).
- Data governance: DATA_GOVERNANCE.md + per-corpus/per-clip license+consent+attribution+share_alike ledger validates against a committed JSON schema; build fails if any corpus row lacks a required field or if any SHARE-ALIKE-CONTAMINATING corpus is referenced by an export/train target; a dataset card cannot export if any segment lacks license+consent+attribution.
- Holdout integrity by audio CONTENT HASH: gold-holdout audio excluded from both the DPO export bundle and the fine-tune training set across gold_segments + speech_segments (today is_holdout at eval.rs:80-86 is written but never read; build_dpo_dataset filters a disjoint table).
- FLAC export implemented (claxon/symphonia) with a positive decode-roundtrip test replacing the current 'rejects FLAC' test (today export_audio/mod.rs:61 explicitly errors not-implemented).
- Refinery lift: measured raw-ASR-vs-jury CER reduction >=30% at <=15% human escalation, error-detection F1 beating a pinned Cleanlab baseline, surfaced in-product (today the apparatus exists in eval.rs but is rendered by zero Svelte components).
- Engineering rigor: EVERY fuzz target run in CI (0 unresolved crashes) — count-agnostic on purpose, so a target removed with the dead code it fuzzed cannot leave this line false (was "5" until iteration 231 deleted the `features` target with the unused FbankExtractor module; wording OWNER-CONFIRMED 2026-08-02); clippy::unwrap_used+expect_used denied in non-test code; every Action SHA-pinned; OpenSSF Scorecard >=8.0 (or documented solo-dev cap); SECURITY.md+CODEOWNERS present; runtime SHA-256 ONNX manifest verification (tampered ONNX rejected); cargo-mutants nightly 0 survivors in irt/conformal/ood/diff/normalizer.
- Latency: CTC-300M RTF <=0.3 CPU / <=0.1 GPU on the named reference machine, published per release, with a >5% bench-regression gate.
- Fairness: per-dialect CORDI CER leaderboard with a release-blocking max-min CER disparity budget (and per gender/age where metadata exists).
- A11y: 0 @axe-core/playwright violations [wcag2a,wcag2aa,wcag22aa] over an enumerated route x locale set with a coverage assertion; svelte-check a11y-as-error; verified RTL focus order for EN/CKB.
- Distribution: signed Windows installer (signtool verify /pa exits 0), SLSA L2 provenance (gh attestation verify passes), signed auto-updater performing no egress offline, winget/Homebrew/Flathub install paths work, HF model card with eval YAML + ethics/intended-use/dual-use section. macOS notarization is the ONLY explicitly-flagged STRETCH leg and does NOT block the 10/10 gate.
- Truth-in-advertising: every advertised feature audited to 100% true; OMNIASR_MIGRATION.md header matches shipped sherpa code; Autonomy Dial (jury_autonomy_level persisted at settings.rs:92 but never read in jury/mod.rs) either wired to real behavior or removed with no dead toggle remaining.

## Operating Loop (repeat every iteration)
1. 1. RELOAD AGENT_CHARTER.md + PROGRESS_LEDGER.md; read the 'Current focus' entry to resume cold.
2. 2. SYNC: run `git status` / `git pull`; confirm a clean tree or stash before starting.
3. 3. SELECT the next milestone via the Work-Selection Policy (highest-priority READY milestone whose dependsOn are all DONE).
4. 4. PLAN: copy the milestone's exact `doneWhen` bullets into the ledger as a checklist and restate the machine-checkable CI assertion you will add.
5. 5. BRANCH: `git checkout -b mNN-slug` off main (never commit straight to main).
6. 6. IMPLEMENT the smallest increment that advances ONE doneWhen bullet (no scope creep).
7. 7. VERIFY: run the real tests + the real eval harness; paste ACTUAL command output into the ledger. Never trust unverified or remembered output.
8. 8. MEASURE: if accuracy/lift/latency was touched, run the real harness and record the actual number plus the exact command and dataset/model SHA. NEVER invent a number.
9. 9. GATE: add or strengthen the CI assertion so this gain can never silently regress (a fix without a regression gate is incomplete).
10. 10. COMMIT one logical change with a Conventional Commit message referencing the milestone id.
11. 11. LOG: update PROGRESS_LEDGER.md — what changed, real evidence (cmd+output), measured numbers table, next concrete step.
12. 12. CHECK STOP CONDITIONS: if `make verify-10` is green -> STOP; else GOTO 1. One iteration = one commit-sized, individually-verified increment.

## Work-selection policy
Pick the single highest-priority READY milestone, where READY = all of its dependsOn milestones are marked DONE in the ledger. Break ties in this fixed order: (1) Wave order — Wave 0 before 1 before 2 before 3; never start a later-wave milestone while a READY earlier-wave one exists. (2) Blocker-first — M0 (ethics/data-governance) and M1 (git/version/license sync) are hard upstream blockers and come before everything; no publish/train/redistribute milestone (M4b, M9, M12, M13, M14, M15) may start before M0 is DONE. (3) Truth-before-publish — M2b (ckb hint spike) and M3b (inter-annotator agreement) MUST be DONE before M4b pins any scorecard number, so the headline can never shift and break reproducibility. (4) Lowest-current-score dimension — attack the 0s and 1s first (Distribution 0, Real-time 0, Ethics 0, Language 1, Sustainability 1). (5) Smallest effort last tie-break (S before M before L) to keep shipping momentum. Dependency graph: M2/M2b/M3/M5/M6/M7 <- M1; M3 <- M2,M2b; M3b <- M0,M3; M4a <- M0,M2; M4b <- M0,M3,M3b,M4a; M8a <- M3; M8b <- M8a; M9 <- M0,M4b; M10 <- M6; M11 <- M9; M12 <- M0,M8a,M11; M13 <- M0,M4b,M12 (non-blocking — an underperforming fine-tune leaves the honest stock-CTC scorecard shippable and does NOT block M14/M15/M16); M14 <- M0,M4b,M11; M15 <- M0,M7,M12; M16 <- M10. If the selected milestone is blocked by a human-only task, record the blocker and select the next-best READY milestone instead of idling.

## Metrics tracked every iteration

| Metric | Target | How measured |
|---|---|---|
| ckb CER on FLEURS ckb_iq (local-only, AsoSoft-normalized, primary) | Stock CTC-300M published first (expect ~10-16); fine-tuned target CER<=8, stretch <=6; always reported against the M3b IAA label-noise ceiling | Nightly CI runs `make eval-ckb` under an enforced network sandbox on pinned dataset SHAs; merge fails if CER regresses >1.0 absolute vs the committed baseline OR if any socket connect() occurs during eval |
| ckb CER/WER vs SeamlessM4T-v2 baseline + prior Sorani SOTA on AsoSoft-600 (secondary) | Beat SeamlessM4T-v2 with MAPSSWE p<0.05 (stock Whisper-large-v3 is non-competitive ~98 CER — NOT the bar); approach AsoSoft SOTA WER<=11.8 (non-redistributable) and beat the Common Voice ckb open bar 36.8 WER / 7.8 CER on the same split; report honestly if a sub-2-point win is true-but-non-significant on N=600/922 | CI asserts a 95% blockwise-bootstrap CI AND mapsswe_p<0.05 vs the M4a SeamlessM4T-v2 baseline AND ci_high<baseline_ci_low |
| Reference-label ceiling (inter-annotator agreement) | >=2 independent annotators; published Cohen/Fleiss kappa + reference-label CER ceiling on the gold subset, cited by every scorecard | CI asserts the IAA artifact exists and the scorecard references it; a scorecard publish without a cited IAA ceiling fails |
| Refinery label-quality lift (raw vs post-jury CER reduction + error-detection F1) | >=30% CER reduction vs raw CTC at <=15% human-escalation budget; error-detection F1 beats a pinned Cleanlab confident-learning baseline by a committed margin | Nightly fixed-seed injected-error gold-set benchmark; fails if CER_reduction<0.30 OR escalation_fraction>0.15 OR F1<=cleanlab_F1 |
| Per-dialect / subgroup fairness disparity | max-min CER gap across the 6 CORDI dialects (and gender/age where metadata exists) within a committed threshold | Release-blocking CI gate fails if the max-min CER disparity exceeds the committed threshold |
| Offline egress (runtime, north-star guardrail) | Zero network egress in default config across the full T0->debate jury AND the auto-updater; Gemini only after explicit consent-gated opt-in | A RUNTIME egress harness (socket interception, not source grep) asserts zero outbound sockets in default config across the escalating-jury fixture and the offline updater; a separate test asserts no audio/transcript leaves without acknowledged consent |
| Data-governance / license & consent provenance | 100% of published dataset/checkpoint segments carry source license + attribution + consent basis; zero share-alike-contaminating corpora in any redistributed artifact | CI fails if the per-corpus ledger has a missing field, if a dataset card exports a segment lacking license+consent, or if an export/train target references a SHARE-ALIKE-CONTAMINATING corpus |
| Holdout / train-eval leakage (by audio hash) | Zero gold-holdout audio (by content hash) in any DPO export OR fine-tune training set | Required test asserts the DPO bundle AND fine-tune training set exclude all gold-holdout audio by content hash across gold_segments + speech_segments; merge blocked on failure |
| Production unwrap budget | <=13 production unwrap()/expect() (currently normalizer 12 + irt 1) | clippy::unwrap_used + expect_used denied in non-test code; any new production unwrap fails CI |
| Mutation score on core modules | 0 surviving mutants in irt/conformal/ood/diff/normalizer | cargo-mutants --in-diff runs nightly-required (PR-advisory only); a surviving core-module mutant fails the nightly and auto-files an issue |
| Fuzzing continuity | 0 unresolved crashes; >80% line coverage on normalizer/diff/audio parsers | PR 30s/target smoke must pass; nightly 15min/target campaign with cached accumulating corpus auto-files an issue on any crash |
| Supply chain + provenance + model integrity | 100% deps audited-or-imported; SLSA L2 provenance + CycloneDX SBOM on every release binary; OpenSSF Scorecard >=8.0; *.onnx SHA-256-verified at runtime against a signed manifest | cargo-deny + cargo-vet required; release fails without attestation+SBOM; Scorecard is a required status check; a tampered ONNX fails the runtime manifest test |
| Accessibility (WCAG 2.2 AA) | 0 axe-core violations at wcag2a/2aa/22aa across an enumerated route+locale set; correct RTL focus order for EN/CKB | @axe-core/playwright + svelte-check a11y-as-error block merge on any violation AND on scanning fewer than the enumerated states |
| Latency / RTF | CTC-300M RTF<=0.3 CPU, <=0.1 GPU on the named reference machine; footprint <400MB | criterion benches gated on every PR with a >5% wall-clock regression budget via github-action-benchmark against a committed baseline |
| Release reproducibility, version & license integrity | One-command sandbox-reproducible eval with pinned SHAs; all manifests + LICENSE + NOTICE agree on version AND SPDX license | CI fails if tauri.conf.json/package.json/Cargo.toml versions or declared SPDX license disagree with each other, the CHANGELOG canonical version, or LICENSE/NOTICE; eval JSONL + attribution manifest regenerable from a single command |
| Bus-factor / sustainability | SECURITY.md + vulnerability-disclosure + CODEOWNERS present; external-onboarding lead-time register maintained off the critical path | OpenSSF Scorecard Security-Policy + Code-Review checks pass (or document the solo-dev cap); a release-readiness check fails if SECURITY.md or CODEOWNERS is absent |

## Guardrails (must always / must never)
- MUST ALWAYS keep all existing tests green at every commit (Rust ~360 tests incl proptest/soak/reliability, vitest, playwright, Python policy gates); a red suite blocks the commit.
- MUST ALWAYS work on a branch, make one logical change per commit, use Conventional Commits, and end commit messages with the Co-Authored-By: Claude Opus 4.8 trailer; only commit/push to the working branch unless the human requests otherwise.
- MUST ALWAYS add or strengthen a CI gate with every new capability so the gain cannot silently regress (a fix without a regression gate is incomplete).
- MUST ALWAYS treat voice as biometric (GDPR Art.9) and enforce consent + license + attribution provenance before any publish/train/redistribute action.
- MUST ALWAYS keep main in a never-self-contradictory state: docs match code, and versions + SPDX license agree across all manifests + LICENSE + NOTICE.
- MUST NEVER fabricate, estimate, round, or 'remember' any metric (WER/CER/F1/kappa/RTF/p-value/CI) — see anti-hallucination rules.
- MUST NEVER force-push or reset --hard shared history, skip hooks (--no-verify), or bypass signing.
- MUST NEVER send audio/transcript to Gemini/cloud without explicit acknowledged consent, and never make Gemini load-bearing in the default path.
- MUST NEVER reference a SHARE-ALIKE-CONTAMINATING or no-redistribution corpus in an export or training target.
- MUST NEVER introduce a new production unwrap()/expect() in non-test code (budget is <=13).
- MUST NEVER disable, weaken, or delete a quality gate to make a build pass.
- MUST NEVER scope-creep: implement only the selected milestone's doneWhen; record out-of-scope ideas in the ledger backlog, not in the current commit.
- MUST NEVER ship a number you cannot reproduce on demand with a pasted command + dataset/model SHA.

## Verification protocol (prove every change before moving on)
- Run Rust tests before every commit: `cargo test --manifest-path src-tauri/Cargo.toml` (plus `--test tauri_integration` and `--test soak` when touching those paths); all green, paste the summary line into the ledger.
- Run frontend checks when UI/TS changed: `npm run test` (vitest), `npm run typecheck`, `npm run lint`, and `npm run test:e2e` (playwright).
- Run the honesty/privacy/provenance policy gates: `npm run test:python-policies` — these must stay green.
- Run the build smoke when build/config changed: `npm run tauri:build:smoke`.
- For ANY accuracy/lift/latency claim, run the REAL harness on a REAL held-out set (`make eval-ckb` once it exists; until then the ASR-on-gold integration test) and paste the literal stdout; the reported number is exactly what the tool printed.
- Every fix must add or strengthen a CI assertion that would have caught the bug — a fix without a regression gate is incomplete.
- For milestone completion, re-verify on a fresh checkout / clean working tree so completion never depends on uncommitted local state.
- If a command fails, fix the root cause or record it as a blocker — never explain away a failure or mark something done on unverified/remembered output.

## Anti-hallucination rules (non-negotiable)
- NEVER invent, estimate, round, 'remember', or extrapolate a WER, CER, F1, kappa, RTF, p-value, or confidence interval — every such number must come from a real run of the real harness on a real held-out set, captured this session, with the command pasted into PROGRESS_LEDGER.md.
- No number without a reproducer: if you cannot paste the command + literal output (and dataset/model SHA) that produced a number, the number does not exist and must not appear in code, docs, README, EVAL.md, model card, scorecard, or commit message.
- run_gold_eval must compute hypotheses by running the live sherpa-onnx OmniASR CTC engine (with the ckb hint) — NEVER accept caller-supplied transcript text as the hypothesis for a published number; a scorecard built from caller text is fabrication (today eval.rs:150 takes caller hypotheses — this must be closed before any number is pinned).
- Refs and hyps must go through the IDENTICAL normalization path (SoraniNormalizer + NFC); mismatched normalization silently inflates/deflates CER and is treated as a fabrication bug.
- No placeholder/mock numbers in any published surface — no '~', '≈', 'TBD', or hard-coded sample values in README/EVAL.md/model card; every published figure carries a pinned commit + dataset SHA + N + CI.
- If a result is bad, report the bad result honestly (a higher-than-hoped CER, a non-significant MAPSSWE, a failed fine-tune); the honest stock-CTC number is always shippable, a flattering fake number never is.
- Offline/privacy claims are proven at RUNTIME via the socket-interception egress harness, never by reading or grepping source code.
- Do not claim a feature exists until its positive test passes (e.g. FLAC export must produce a decodable file before any doc says FLAC is supported).
- When uncertain about a value, say so in the ledger and escalate to the human rather than guessing a number to keep moving.

## Stop conditions
- HARD STOP (success): `make verify-10` exits 0 and prints `CORTEX 10/10: ALL GATES GREEN` — write a final ledger entry, ensure main is clean and tagged, and STOP without inventing further work.
- PAUSE (await human): if the active milestone hits a human-only blocker, pick the next READY non-blocked milestone; if ALL remaining READY milestones are human-blocked, write the blockers to the ledger and end the session stating exactly what the human must unblock.
- SAFETY STOP: if a change cannot be made without fabricating a number, weakening/deleting a gate, or violating a guardrail, STOP and escalate — never trade honesty for progress.
- DO NOT STOP at 9/10 or 'good enough' — any dimension below 10/10 means keep iterating (macOS notarization is the only explicitly-flagged stretch leg that does not block the gate).

## Escalate to human when
- Secrets/accounts/money/external lead-time: GitHub remote creation, branch-protection toggles, Azure Trusted Signing onboarding, Apple Developer ID ($99/yr + Apple hardware + macOS CI runner), Sigstore/gitsign setup, winget/Homebrew/Flathub review queues, Gemini API key.
- Human-judgment data tasks: recruiting/compensating the >=2 annotators for inter-annotator agreement (M3b); dialect/consent decisions on corpora.
- Licensing/legal calls: any ambiguous CC-BY vs CC-BY-SA vs LDC redistribution question that could contaminate the intended Apache-2.0 app/model/DPO artifact.
- Compute/hardware: the GPU budget decision (local vs rented) and the named reference machine required for RTF publication and the LoRA fine-tune (M13).
- Corpus access: CORDI or other dataset access requiring a human-signed agreement.
- Genuine ambiguity in this charter or the roadmap that cannot be resolved without guessing — ask rather than assume, and when blocked on one item keep momentum by selecting the next READY non-blocked milestone.

## Cadence
Per iteration (continuous loop): select -> plan -> branch -> implement -> verify -> measure -> gate -> commit -> log, in small commit-sized increments. Per PR (finished milestone branch): full verification protocol + fuzz smoke (30s/target) + criterion benches (>5% regression budget) + axe-playwright on enumerated routes. Nightly (heavy, non-PR-blocking to protect the solo+AI flow): `make eval-ckb` in a network sandbox on pinned SHAs, 15min/target fuzz campaign, cargo-mutants --in-diff on core modules, refinery lift benchmark, fairness disparity check — each auto-files an issue on failure. Per release tag: signed Windows installer + SLSA provenance + CycloneDX SBOM + cargo-auditable binaries + published RTF + regenerated scorecard table + offline-updater egress test.

## Progress ledger format
Maintain PROGRESS_LEDGER.md at the repo root, updated every iteration (loop step 11), as the cold-resume state. Required sections: (1) Overall 10/10 gate — `make verify-10` status (NOT-PRESENT/RED/GREEN) and which dimensions are at 10/10 vs remaining with current score. (2) Milestone status table M0..M16 with columns: id, title, wave, status (TODO/IN-PROGRESS/BLOCKED/DONE), dependsOn-met, and evidence (the actual command + result that proves it). (3) Current focus — active milestone id, the next concrete doneWhen bullet, and branch name. (4) Measured numbers table — REAL values only, each row carrying date, metric, value, the exact command that produced it, and dataset/model SHA. (5) Blockers awaiting human — date, milestone, what is needed, why the agent cannot proceed. (6) Backlog — out-of-scope ideas tagged with the milestone they belong to (never implemented in the current commit). (7) Decision log — dated decisions + rationale. A milestone is DONE only when all its doneWhen bullets have pasted real evidence and its CI gate is live. The full template is embedded in AGENT_CHARTER.md Section 9.
