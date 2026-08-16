# Cortex Speech: the remaining work for a true 10/10

**Fresh audit:** 2026-08-09  
**Audited code:** `78bf875` on `codex/newbranch`  
**Standard:** senior-engineering perfection, not a marketing score

## Verdict

Cortex is already a strong, unusually well-tested personal-use speech product. It is not a literal
10/10 today. The honest current position is:

- **Standard engineering:** strong. Frontend tests are 236/236; `npm audit --omit=dev` reports zero
  vulnerabilities; `npm ls --all` is clean; Rust advisory/license/source checks pass; the generated full
  sweep records 31 of 32 kept gates passing.
- **Current ship proof:** red. `ignored-real-model` fails because the default OmniASR-7B WSL service is
  not listening on `127.0.0.1:8799`.
- **Literal charter:** incomplete. The verifier still lists eight owner-descoped legs and five
  owner-gated legs. One of those five, branch protection, is actually complete and live, so the verifier
  itself is stale; four genuine owner-gated evidence legs remain.
- **Reliability:** one serious native-stack risk remains. The ledger records intermittent foreign
  exceptions in the ONNX/sherpa path terminating app test binaries with `0xC0000409`. Instrumentation is
  now broad, but the cause is not fixed.
- **Product/ML proof:** the machinery is excellent, but human-gold, independent-annotator, dialect,
  calibration, and public distribution evidence remain incomplete.

## P0 — required before calling the current build flawless

### 1. Contain and eliminate the intermittent native ASR crash

The project has observed the app's own native test binaries abort when a C++ exception escapes the
sherpa/ONNX FFI boundary. This is not a test-only cosmetic failure: the same in-process native stack is
used by the product. The latest instrumentation can name the next failing binary/test, but
instrumentation is not remediation.

**Best fix:** move native model inference behind a supervised worker-process boundary. The desktop app
should own a small protocol, health checks, per-request deadlines, cancellation, crash detection,
bounded restart/backoff, and a circuit breaker. A native abort should fail one transcription and
restart the worker, never terminate the library/review UI. Keep the current breadcrumbs to locate and
fix the underlying native call too.

**Done when:** a fault-injection test kills/aborts the inference worker during load and inference; the
desktop survives, the job reaches an explicit recoverable failure state, no partial transcript is
committed, and subsequent work succeeds after bounded recovery. Then run a long stress campaign with
zero unexplained host-process deaths.

### 2. Make the default 7B engine operationally self-contained

The application defaults to WSL7B, but the present machine has no server listening. The full verifier
therefore fails 5-pass/1-fail in its ignored real-model subset. The verifier's probe checks local model
presence, then unconditionally runs a test that assumes a separate 7B service is already alive.

Do not solve this by weakening the test. Either:

1. manage the WSL server lifecycle from Cortex (provision, start, health-check model/tokenizer/schema,
   restart, stop, and show logs), or
2. make the offline engine the reliable default and offer an explicit, visible switch to the champion.

Then split `ignored-real-model` into independently probed gates so an unavailable optional service is
`INCOMPLETE/SKIP-ENV`, not a misleading product regression.

**Done when:** a fresh launch can transcribe without terminal instructions, and the full 32-gate sweep
is green at the exact release commit.

### 3. Close the remaining total-error-boundary holes

One ready fix already exists on `codex/continuous-20260809-gate-audit` at `30745dd`: it makes global
unhandled-rejection reporting non-throwing even when a rejected value has hostile `toJSON` and
`toString` behavior. It includes two tests but is not integrated into the main branch.

The same class remains in `src/lib/errors.ts`: `parseActionableError` calls `String(error)` and can
throw while the error boundary is trying to render an error. There are also 77 direct
`detail: String(...)` conversions across 16 frontend files.

Create one total, non-throwing formatter for unknown values, use it at every UI error boundary, and
ban raw conversions there with a lint/policy test.

**Done when:** symbols, circular objects, throwing getters, hostile primitive conversion, huge values,
and ordinary `Error` instances all produce bounded readable output without throwing.

### 4. Validate alignment metadata structurally at the write boundary

`validate_alignment_json` currently proves only JSON syntax and a 500 KB limit. The frontend then casts
`words` directly to `WordTimestamp[]`. A valid JSON value such as a string `start` and object `end` can
therefore enter the database and become `NaN` playback bounds.

Define a versioned alignment schema and validate:

- finite, non-negative numeric `start`/`end`, with `end >= start`;
- non-empty word text and a sane word-count/size ceiling;
- finite confidence in its declared range when present;
- finite source/chunk bounds and consistent chunk indices;
- either the supported legacy array or the versioned object, never arbitrary JSON.

Use a separate generic JSON-blob validator for evidence metadata; do not overload alignment validation.
Validate in the shared DB write path as well as commands/imports. On the frontend, fail the whole
alignment closed rather than filtering entries and shifting word indices.

**Done when:** malformed-but-valid JSON is rejected before persistence and cannot reach waveform,
word-playback, export, merge/import, or review paths.

### 5. Repair the one-word playback bound contract

`wordPlayBounds` clamps `end` to the clip and then applies its 50 ms minimum, which can push `end` past
`clipEnd` for short or degenerate clips. Rework the minimum window by shifting the start left inside the
available clip, and gracefully accept a shorter-than-50 ms window when the clip itself is shorter.

**Done when:** property tests prove finite output and
`clipStart <= start <= end <= clipEnd` for every bounded clip, including zero-length and sub-50 ms data.

## P1 — required for evidence-backed product excellence

### 6. Finish the human evidence, not more synthetic machinery

The current library census recorded 67 decisions, but only 40 verified-good clips. The charter requires
at least 500 real review decisions for in-product Refinery evidence. The current jury escalates every
clip and the active-learning rule is intentionally still the old nearest-threshold policy because a
frozen human-labelled calibration set does not yet exist.

Run the Gold Marathon with immutable decision provenance, then:

- freeze train/calibration/test splits by recording identity;
- fit calibration only on human-adjudicated data;
- report selective CER versus coverage, ECE/Brier score, conformal coverage, escalation rate, and
  reviewer minutes;
- compare queue policies: random, current threshold proximity, uncertainty, and
  uncertainty × diversity × expected correction value;
- measure actual downstream CER lift, not only synthetic injected-error lift.

### 7. Recruit independent Sorani annotators and measure label quality

At least two independent annotators must label an overlapping frozen set without seeing one another's
answers. Report agreement with confidence intervals, disagreement categories, adjudication outcomes,
and per-annotator bias/time. A single owner reviewing more clips cannot substitute for independence.

### 8. Add dialect and worst-slice evidence

Resolve the CORDI agreement/license path, then evaluate dialect/domain slices. The present gender gate is
underpowered (female N=25) and several age slices are directional only. A 10/10 claim needs minimum
sample floors, confidence intervals, worst-slice error, and an explicit policy for slices too small to
certify. Keep Northern Kurdish (`kmr`) separate from Central Kurdish (`ckb`).

### 9. Make one canonical accuracy record

The app now has a clean one-recording-per-sentence FLEURS run at N=348 (stock CTC-300M: 12.58% CER,
52.05% WER, with bootstrap-CI machinery added afterward). However, `docs/EVAL.md` and
`docs/MEASUREMENTS.md` still lead with the older duplication-weighted 922-row/348-distinct comparison
and say a clean rescore is pending.

Create one generated, machine-readable scorecard per release containing dataset revision and manifest
hash, unique recording count, model/artifact hashes, normalization basis, decoding parameters, CER/WER
with block/speaker-aware confidence intervals where possible, paired significance tests, overlap audit,
slice results, command, hardware, and app commit. Do not let the ledger be the only place the newer
measurement exists.

### 10. Resolve AsoSoft-600 rights before using it for a claim

The verifier still marks this owner-gated. Keep the set evaluation-only and out of distributed artifacts
until the actual license/permission is documented. FLEURS remains the reproducible public benchmark.

## P2 — required for a genuinely excellent user experience

### 11. Make long imports progressively useful

The measured 30.2-minute import took 235 seconds before the first reviewable clip appeared. Both normal
and “streaming” pipelines still persist segments only after whole-file processing; streaming currently
means bounded decode memory, not progressive availability.

Persist provisional clips in small atomic batches, expose them as reviewable when safe, and backfill
whole-file speaker clustering afterward with an explicit state/version. A crash must leave either a
resumable import or a clean rollback, never a half-trusted dataset.

**Target:** first useful clip within a declared small bound while progress, cancellation, pause/resume,
and recovery remain honest.

### 12. Finish first-run and recovery UX

The app needs one dominant journey: **Import → Transcribe → Review → Export**. Add a resumable readiness
check for model/server status, storage/backup, privacy defaults, a sample transcription, and the first
valid export. Keep advanced evaluation, agentic, and diagnostics tools behind progressive disclosure.

Every readiness blocker should deep-link to the exact clips or setting that resolves it. Instrument
time-to-first-export, review seconds, replay/edit/undo rates, abandoned drafts, and model failures
locally, with explicit consent for any diagnostic export.

### 13. Give the remote-review expired-link state a real recovery path

The current phone-sized state is readable, RTL-correct, and uses an alert role, but it is a large dead
end: it says to ask for a new link and offers no action, owner/contact context, or retry path.

![Current mobile expired-link state](C:/Users/Wareen/.codex/visualizations/2026/08/05/019fd36f-9878-73d0-b5d0-1532c3cb85ba/cortex-true10-audit-20260809/01-remote-review-expired-mobile.png)

Add a safe recovery affordance that matches the deployment model: retry if the session may be
temporarily unavailable; otherwise show the reviewer name/owner instruction and a one-tap way to copy a
short non-secret diagnostic or open the approved contact channel. Never expose the token.

### 14. Complete manual accessibility and real-device evidence

Automated axe coverage is valuable but insufficient. Store dated evidence for NVDA on Windows,
keyboard-only operation, 200%/400% reflow, Windows high contrast/text scaling, reduced motion, focus not
obscured by sticky actions, and the phone flow on real iOS/Android over the intended Tailscale/network
path. Include error recovery and async announcements, not only happy-path screens.

### 15. Reduce change risk in the three giant orchestration modules

Current sizes remain approximately:

- `src/App.svelte`: 3,528 lines
- `src-tauri/src/commands.rs`: 4,475 lines
- `src-tauri/src/couch.rs`: 4,900 lines

Continue incremental extraction behind characterization tests: workspace orchestration, command
domains, and couch transport/session/lease/audio/page concerns. The goal is ownership and reviewability,
not arbitrary line-count reduction.

## P3 — required only for a literal public/full-charter 10/10

Reopen and complete the eight currently descoped legs:

1. Authenticode-signed Windows installer with verified timestamp and key-custody/runbook evidence.
2. SLSA provenance/attestations verified against release artifacts.
3. Signed updater with staged rollout, rollback drill, and protected updater-key custody.
4. Proven install/update/uninstall behavior for every claimed store/platform path.
5. Public Hugging Face model card with exact eval metadata, limitations, ethics, license, and hashes.
6. macOS signing/notarization if macOS is claimed.
7. OpenSSF Scorecard ≥8 as a required check, or amend the charter with a justified alternative.
8. Signed release tag and verified artifact/checksum chain. (`main` protection is already live.)

Also move the stable benchmark budget to a controlled required PR runner. Today the repo's own charter
text acknowledges that the every-PR benchmark clause is not satisfied and two noisy microbenchmarks are
not enforceable. Increase measured work per case or use statistical distributions; do not simply widen
thresholds.

## Proof-system cleanup

These are not product bugs, but they block an honest final claim:

- Remove `branch-protection` from `OWNER_GATED` and add an actual read-back gate. On 2026-08-09 the
  GitHub API reported `main` protected, admin enforcement enabled, pull requests required, and four
  required status contexts.
- Preserve the two successful nightly runs at `78bf875`, but label the “Real audio + ASR” job honestly:
  its fixture-dependent real-audio and fine-tuned steps complete immediately when private fixtures are
  absent. Green infrastructure is not proof that real audio ran.
- Integrate or explicitly reject `30745dd`; do not leave a tested correctness fix stranded in another
  worktree.
- Bring the equivalent-but-different local and remote commit histories back to one canonical branch
  before release, and regenerate `docs/STATUS.md` only from the final clean release commit.

## Final acceptance standard

Call Cortex 10/10 only when all of the following are simultaneously true at one signed release commit:

1. Every kept gate is green; no unexplained host-process crash remains.
2. A fresh machine reaches a successful transcription and export without terminal intervention.
3. Malformed metadata and hostile errors fail closed without corrupting data or killing the UI.
4. Human-gold calibration, independent annotation, dialect/worst-slice evidence, and a canonical paired
   scorecard are complete.
5. Long imports become useful progressively and survive cancellation/crash.
6. Manual accessibility and real-device review flows pass.
7. Every literal charter leg is either completed or the public claim is explicitly narrower than
   “full-charter 10/10.”
8. The installer, updater, release tag, checksums, attestations, model card, and rollback path verify as
   one reproducible chain.

Until then, the accurate claim is: **high-quality personal-use release candidate with excellent
engineering evidence, one unresolved native reliability risk, four real human/data gates, and a public
distribution tail.**
