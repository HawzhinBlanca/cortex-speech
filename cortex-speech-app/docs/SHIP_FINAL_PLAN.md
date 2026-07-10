# SHIP_FINAL_PLAN — What is TRULY left for full-charter 10/10 (2026-07-10)

Product of a 10-agent, evidence-cited audit of every definition-of-done artifact (AGENT_CHARTER,
TRUE_RATING_2026-07-09, REAL_READINESS_PLAN/FINAL_READINESS_10, HARDENING_PLAN_10, the verify-10
gate code, PROGRESS_LEDGER, CI/release workflows, ROAD_TO_10) plus two adversarial sweeps for
deferred/`#[ignore]`/unmeasured items. Every item carries its source evidence. DONE items dropped.

**Headline:** 36 of 58 remaining items are agent-automatable on the owner's GPU machine today.
The critical path that is NOT automatable is one thing: the **Gold Marathon** (≥500 human Sorani
review decisions), which alone unblocks 7 downstream measurements. Nothing here permits declaring
10/10 early — the declaration belongs to the P7 re-audit (#58) after the numbers exist.

## A · Automatable now — the 7 workstreams (execute in order)

### WS1 — Land the measurement record (items 1–4, 25, 36) — IN PROGRESS
As soon as the in-flight same-set FLEURS-ckb (N=922) runs land (fine-tuned MMS-1B + stock CTC-300M,
champion already measured 7.03% CER [6.53, 7.55]):
- Pin the three-engine scorecard: `make measure-10` → docs/MEASUREMENTS.md (git-SHA + manifest-SHA
  stamped); commit frozen FLEURS-922 / CV22 manifests + `.sha256` (M4a); write the formal C1
  engine-decision record (M1.3).
- SeamlessM4T-v2 baseline on the same frozen sets + MAPSSWE p<0.05 gate (charter: stock Whisper is
  NOT a valid baseline).
- AsoSoft-600 leg (verify eval-use licensing during acquisition).
- Doc-honesty sync: EVAL.md still says "default 7B engine remains unmeasured" (stale vs 7.03%
  N=922); purge stale "not yet wired" caveats; refresh ledger headline.
- Re-confirm exe-freshness GREEN at branch tip.

### WS2 — Make `verify-10` mean 10/10 (items 5–6, 21–22)
- Complete + merge the `feat/proof-metadata-10-10-gate` branch: fix its 8 broken SpeechSegment
  initializers, renumber its migration v34→v36 (v35 is taken by the FTS repair), land
  confidence_source / cloud_call / decoder_config_hash / normalizer_version + get_segments_page.
- Extend `scripts/verify_10.py` + Makefile from the narrow M0/M1 gate into the full-charter
  aggregator: CHANGELOG byte-equality, LICENSE/NOTICE content, repo-URL, ASR-on-gold
  zero-caller-text assertion, holdout content-hash gate, per-clip license/consent export gate,
  a11y coverage, docs-match-code, scorecard/eval legs.
- Add OpenSSF Scorecard workflow (+ solo-dev cap note) and a SLSA attestation step in release.yml.

### WS3 — Prove privacy + rigor at runtime (items 7–8, 13, 24)
- Socket-interception egress harness (WSL): assert ZERO outbound sockets across the full
  T0→T1→T2→debate jury and updater paths — replaces the static source-grep.
- Wire the 5 existing fuzz targets into nightly CI + cargo-mutants (irt/conformal/ood/diff/
  normalizer); run first long local campaigns.
- MAPSSWE vs NIST SCTK reference cross-check; replace the self-referential jiwer fixture;
  stratified WER (SNR/duration/speaker).
- Run every `#[ignore]`'d real-model/7B harness on this rig (~30 gates never exercised) and record.

### WS4 — Nightly gates that hold the line (items 4, 9–10, 29)
- `make eval-ckb` nightly network-sandboxed CER-regression gate + dual-metric (CER+WER) gold
  baseline wired PR-blocking.
- RTF measured on this named dual-GPU rig, published per release + criterion bench-regression gate.
- Refinery-lift proof: nightly fixed-seed injected-error benchmark (≥30% CER reduction at ≤15%
  escalation; F1 > pinned Cleanlab baseline) — synthetic, needs no human decisions.
- Release-blocking gender/age disparity gate on existing corpus metadata.

### WS5 — Accuracy levers toward measured-best (items 11–12)
- KenLM shallow fusion + beam decode + lexicon/hotword biasing on the MMS-CTC head (cited ~36%
  rel. WER lever), verified against the frozen FLEURS-922 yardstick — the credible path past
  Scribe v1's 32.1% same-set WER.
- Confidence-filtered pseudo-labeling of the unlabeled archive (verify archive presence on this
  machine first; provisioning may need the owner).

### WS6 — Operational hardening (items 14–20, 23, 26–28)
ServerSupervisor (app-owned 7B server spawn/health/restart + UI state); champion promotion
plumbing (promotion pointer read by server, adapter SHA-256 verified at start, gate_and_promote
IPC + Promote button); the full reliability drill matrix (kill-exe/kill-WSL mid-import, corrupt
DB, disk-full, force-kill-at-40% resume, WSL DR restore, multi-hour soak, live review-while-import
latency, clean-profile migration) — each scripted with a ledger entry; DPAPI key-at-rest + egress/
consent audit log; LOOP-0 firing-blindness + T1 confidence-semantics latent fixes; fill empty
model `*_ARCHIVE_SHA256` pins + missing optional-model URLs + resumable downloads; consent-
revocation-list + per-clip consent/attribution export gates + transcript-PII scrub; fix the
aux-bins rlib build quirk + full ship-check green at tip; Cluster-G UX minors; split-grouping by
content hash; autonomy safety valve (rate cap, spot-check sampling, kill-switch) + ECE-gated dial.

### WS7 — Codebase health (items 30–35)
Reconcile the separate CORTEX workspace checkout FIRST (branch `goal-9.5-upgrades`, commits
d778b27/f187d79 — App.svelte decomposition already exists there; do not redo it), then: true
end-to-end pagination; overlap-decode-and-stitch; concurrent ASR engine pool; ENOSPC preflight;
global unhandledrejection trap + listbox/option a11y semantics; IPC round-trip tests for the 94
commands (1 exists) + coverage floors; README/PIPELINE/About docs; 3 dependabot CI bumps.

## B · Owner-gated — cannot happen without the human

| # | What | What the owner must provide |
|---|---|---|
| 37 | **Gold Marathon** — ≥500 in-app review decisions (start: one 10-decision session) | The decisions. THE bottleneck; unblocks 38–43 |
| 38–43 | C3 review-speed, C4 precision, C5 LOOP-0 go-live, app-gold freeze → first conversational Sorani benchmark, C7 retrain cycle, conformal recalibration | Nothing beyond #37 — agents finish each afterward |
| 44 | Inter-annotator agreement (kappa ceiling) | Recruit + compensate ≥2 independent Sorani annotators |
| 45 | Headline CER normalization basis | A decision (both bases computed automatically) |
| 46–48 | Diarization spot-check, export spot-check, expert walkthrough + 100% approval | ~1–2 h native-speaker listening/judgment |
| 49 | Branch protection, signed tag (gitsign/Sigstore), Scorecard required-check, tailwind-4 decision | Repo-admin clicks + signing identity |
| 50 | Git-history PII scrub (script ready, dry-run default) | Owner runs it; agents never force-push shared history |
| 51 | Self-hosted runner so the 3 skip-with-warning CI legs enforce | Register machine + set env secrets |
| 52 | Distribution (owner already scoped OUT for personal use): Authenticode cert + secrets + v* tag, macOS signing, stores, updater hosting, model-hosting license | Purchases + accounts + decisions |
| 53–54 | CORDI corpus access; Scribe same-set rival row | Signed agreement; ~$3 cloud-egress consent |
| 55–56 | Cross-platform stance; media-cache/streaming rewrite go-ahead | Decisions (execution then automatable) |
| 57 | Maturity/adoption axis | Releases + calendar time |
| 58 | **P7 re-audit — the only place "10/10" may ever be written** | Exists only after the above; interim run would re-confirm ~7/10 |

## C · Standing rules for executing this plan
The one law (CLAUDE.md) applies to every line above: no number without a real harness run pasted
into PROGRESS_LEDGER.md; nothing "done" until user-observable or measured on real audio; branch +
Conventional Commits; never weaken a gate to pass it; no private profile paths in tracked files.
