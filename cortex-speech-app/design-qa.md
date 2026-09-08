# Compact Review — owner-directed phone refinement

## Finishing pass — 2026-09-08, late evening

Owner approved the compact structure and requested high-end polish. This pass
keeps that structure, the 18px adjustable transcript, warnings and review authority.
HTML SHA-256: `c0b1c9ae69491ade0028730215ae3309e53bf7b9c6d4141c317400ce3d679fc8`.

- Unified control typography, balanced transport icons/labels, a distinct speed
  value/caption, quiet count/coin pills, lighter secondary surfaces and consistent
  focus/hover/press feedback. Reduced-motion preference disables the new transitions.
- Real waveform peaks are grouped to distinct DPR-aligned phone bars. No generated
  waveform or listening certificate. Late fetch/decode completions are fenced by
  generation, prior fetches aborted, and the waveform cleared when the clip changes.
  Temporary decoding AudioContexts are closed on success and failure.
- Three new unit regressions failed before the fix: old decoding overwrote the
  current waveform, corrupt decoding leaked its context, and a changed revision
  retained the prior waveform. All three now pass; focused unit total is 79/79.
- Complete browser suite: 66/66, 34.9s, no retries/skips. The compact tests now
  capture both themes and assert count/coins stay aligned at 320px as well as 390px.
  Follow-up expanded compact checks: 2/2, 4.9s, with zero scoped axe WCAG 2.2 AA
  violations in the edited/returned state for both locales and both themes.
- Final TypeScript, targeted ESLint/Prettier, diff whitespace, storage policy 5/5,
  frontend guard policy 33 pins, and i18n policy pass. The 17 existing Kurdish
  strings pending native review are not certified by those structural checks.
- Four final 390 × 844 captures were inspected together: English/Sorani × dark/light,
  under `test-results/polish-acceptance/`, filenames `compact-{en,ckb}-390.png` and
  `compact-{en,ckb}-390-light.png`. Synthetic edited returned-clip state with native
  one-second tone, completed playback and fixture-only coin/previous-save values.
  English judgments end near y=455, Sorani near y=475; text is not shrunk to fit.
  Before captures remain under `test-results/polish-before/`.

Final embedded-page real-backend verification: 3/3 passed in 19.73s after a 2m02s
rebuild. Disposable Chromium/HTTPS/SQLite scenarios cover actual owner reopen,
lost request/reply with restart and Undo, and competing tabs. Expected draft/history
preservation and single credit/reversal effects verified. No deployment or release GO.
The existing native Kurdish-copy and physical-phone acceptance limits below remain.

## Latest scope and evidence — 2026-09-08, evening

The owner explicitly changed the phone target during this session: Cortex, clip
count and earned coins in one line; audio immediately below; no numbered Listen /
Transcript / Submit headings; content-sized transcription; decisions immediately
after the text. This supersedes the earlier mobile-density finding and the old
mock's numbered-section fidelity requirement. The original Option 3 palette and
control language remain the base, not a requirement to restore removed headings.

Implemented at HTML SHA-256
`231593fe579a621191c903148886d4d263d81a17e9c629c0367f77ba7b091c74`:

- One wrapping-safe phone header with Cortex, clip position/backlog, and only the
  server-provided earned total. Unknown accounting is hidden, not fabricated as
  zero. Identity changes clear the previous reviewer's coin balance immediately.
- Audio, growing transcript, thin edit/warning metadata, then judgments. Detailed
  progress and identity are under Help; text-size control, Undo and local recovery
  remain accessible. Returned-round and audio-failure warnings are not suppressed.
- Native content-sized textarea with an older-engine measured-height fallback.
  Short text shrinks; long text, larger type and narrower widths grow without
  requiring an inner textarea scroll. Height changes are not animated over a caret.
- Optional display-preference storage cannot abort initialization. Decision and
  identity storage remain strict. Same-revision refresh preserves unstored text,
  active composition, edit-pause rewind, and a known native audio failure.

### Current visual comparison

Source: the exact 853 × 1844 Option 3 PNG below, normalized unchanged to 390 × 844,
plus the owner's explicit compact-layout amendments above. Implementation captures
are both 390 × 844 pixels at a 390 × 844 CSS viewport, density 1:

- `test-results/compact-acceptance/couch-page-Couch-Review-ph-bde91-th-decisions-below-text-en--chromium/compact-en-390.png`
- `test-results/compact-acceptance/couch-page-Couch-Review-ph-a3572-h-decisions-below-text-ckb--chromium/compact-ckb-390.png`

The normalized source and both latest screenshots were opened together in one
comparison input. State: dark, returned clip, edited synthetic Kurdish text,
completed native synthetic audio and synthetic previous-save/coin presentation.
The reference uses an 8s waveform; the fixture uses a 1s tone. These are deliberate
test-data differences, not invented production work. The tone's dense waveform
is actual decoded signal, not decorative art or a listening certificate.

All controls and text are readable at native 390px capture size, so a separate
enlarged crop was unnecessary for the header/editor/decision comparison. The
English primary and secondary judgments end around y=457; Sorani around y=477.
Both fit without page scrolling in this short-transcript fixture, including Undo
and local-draft recovery below. Long text intentionally increases page height.

| Required surface | Current observation |
|---|---|
| Typography | Existing system/Kurdish font and 18px transcript retained; adjustable to 28px. No smaller transcript type was used to force the fit. |
| Spacing/layout | Owner-requested sections removed on phones; short textarea 64px, actions directly below status. Help moved after judgments. Desktop retains section context. |
| Color/tokens | Navy/cyan source palette retained; edited text remains pending-colored. Light/dark scoped axe checks pass. |
| Assets/icons | Existing installed Lucide controls, plus verbatim Lucide coins data; no generated imagery needed. No fake success/listening badge. |
| Copy/content | No new Kurdish claims or translations introduced. Existing generic Kurdish Help guidance and 17 unreviewed strings still need native approval. |

### Verification and iteration history

- 76/76 focused reviewer unit tests pass; TypeScript passes.
- Complete reviewer browser suite: 66/66 pass, 33.9s, no retries or skipped tests.
  Includes 320/390/768/1440 English/Sorani layouts, 44px targets, light/dark axe,
  CSS 200% layout zoom, keyboard clearance, native and fallback textarea sizing,
  plus native corrupt/404/416/503 audio failure and Retry recovery. HTTP responses
  in this suite are mocked; this is not the complete real-backend failure matrix.
- Final embedded HTML also passes all three explicit real Chromium/HTTPS/SQLite
  scenarios: 3/3, 19.48s after a 2m35s rebuild. Actual owner reopen rejects the old
  operation, recovers the draft and credits one fresh decision; lost request/reply
  with restart replays once and Undo reverses once; competing tabs retain their
  edits and produce one canonical effect/credit. All profiles/audio are disposable.
- Targeted ESLint, Prettier, TypeScript and `git diff --check` pass. Storage policy
  5/5, frontend guards 33 pins, and i18n 47 strings (30 reuse / 17 pending) pass.
- Before fixes, four native media cases reproduced enabled judgments after a
  repaint of a broken clip; two unit cases reproduced erased in-memory/IME text;
  preference-storage denial aborted initialization. The new cases now pass.
- Initial compact screenshots accidentally retained a failed synthetic startup
  request. The fixture now loads a settled explicit queue before capturing; those
  old captures are rejected. The keyboard test's old total-document-height
  assumption was replaced by exact +320px clearance padding plus unchanged actual
  decision-button bounds assertions, because compact pages already have spare space.

### Remaining gate

The requested compact layout has no remaining observed P0/P1/P2 layout finding in
these captures. The broader design/release gate remains blocked by the previously
recorded critical Kurdish-copy/native-device acceptance. This report does not
declare the entire frontend finished, full WCAG conformance, production safety,
or a deployed release. No live reviewer state, payroll, corpus or port 8737 changed.
The historical checkpoint below is retained for provenance; its counts and mobile
layout findings are superseded by this section.

Date: 2026-09-07, Asia/Baghdad. Base: `9c5b4c43967639d695624a620be998744cb8d015`.

Historical checkpoint (superseded above): source passed 55/55 browser and 73/73 focused unit tests.
The recovery/handover checkpoint below supersedes older counts and the missing
read-only recovery route. Visual/native-language findings still block release
approval; this is not a full release-gate result.

final result: blocked

This is an uncommitted working-copy checkpoint, not release approval. The existing
production reviewer page was not changed. Product Design image-to-code supplied
the selected layout and its blocking visual-comparison gate.

## Visual evidence and state

- Source visual truth: owner-selected Option 3 image (853 × 1844); exact private
  source location is retained in the owner's local audit and project vault.
- Implementation: `http://127.0.0.1:18739/review`, isolated synthetic fixture;
  no production accounts, database, audio, or payment writes.
- English viewport capture: local audit `option3-en-viewport.jpg` (375 × 812).
- Kurdish viewport capture: local audit `option3-ckb-viewport.jpg` (390 × 844).
- Requested viewport: 390 × 844 CSS pixels. The English screenshot backend
  returned a different pixel size. Full-page/clip captures also showed an
  inconsistent RTL crop despite no horizontal DOM overflow. Those captures
  (`option3-working-*`, `option3-final-ckb.jpg`) are rejected as QA evidence.
- State: returned clip, edited Kurdish synthetic transcript, dark theme. Source
  depicts completed playback and previous saved work; the captured preview is
  at playback start without previous saved work. These differences prevent an
  exact same-state comparison.
- The initial captures above are historical. A new direct-Playwright capture path
  resolves the crop/state mismatch: source rendered unchanged at CSS width 390 is
  local audit `option3-reference-normalized.png`
  (390 × 844); current full-page screenshots are under
  `test-results/guided-final/`, named `guided-en-390.png` (390 × 925) and
  `guided-ckb-390.png` (390 × 947). Their state is returned clip, completed real
  synthetic-tone playback, edited transcript and synthetic previous-save status.
  This status is presentation setup, not evidence of a paid commit.
- Normalized source and English full-page capture were opened in the same image
  comparison input; Kurdish full-page was also inspected. Three-section hierarchy,
  cyan primary and secondary Skip/Reject/Undo are retained. The implementation is
  still taller than the source and has a full-width return banner, larger text and
  real metadata/text-size controls. No pixel-perfect fidelity claim is made.

## Findings

- [P2] Mobile vertical density remains above the source, but full-page height fell
  by 167 CSS pixels in both languages (English 1092 → 925; Kurdish 1114 → 947).
  The waveform now contains the native 44px seek target; section spacing is tighter
  and transcript metadata shares a wrapping status row. English secondary actions
  fit near the 844px fold; Undo still needs scrolling, as do Kurdish secondary
  actions. All remain reachable. Final design acceptance is still outstanding;
  do not shrink touch targets or remove safety/status information just to fit.
- [P2] Kurdish guidance is not yet semantically complete. Reused reviewed labels
  avoid invented translations, but generic edit guidance does not explain Skip
  versus Reject or the three distinct stages. Obtain native review of critical
  action/help/pending copy before deployment; source-string reuse alone is not
  approval of new semantics.
- [Resolved evidence gap] Direct captures now have correct dimensions and matching
  interaction state. This resolves the earlier screenshot blocker, not the density
  or native-copy findings.

## Required fidelity surfaces

| Surface | Observed result / remaining work |
|---|---|
| Typography | Existing Cortex typography retained; English and Kurdish are readable in viewport captures. Exact source font is not established. Heading/help wrapping and density require normalized comparison. |
| Spacing/layout | Three ordered sections and action hierarchy implemented. No DOM horizontal overflow at 320/390/768/1440 widths. Mobile vertical density remains P2. |
| Colors/tokens | Dark navy, cyan primary, and warning return banner follow the selected concept. Pending edits deliberately use a non-success state. Contrast testing on the new styles remains required. |
| Images/icons | No raster illustration required. Playback/replay/loop/skip/reject use verbatim installed Lucide icon data with license and parity test. Waveform remains real signal visualization, not mock artwork. No fabricated listening-verification check. |
| Copy/content | English has explicit unchanged-versus-correction actions and Skip/Reject help. Kurdish critical guidance remains P2. Synthetic transcript/progress differs intentionally from the reference; never presented as live work. |

## Iteration history

1. Initial three-step implementation: retained player/editor and submission IDs,
   added primary-action switching and persistent pending state. Existing 44 tests
   passed. Browser exercise of Save/Undo revealed stale Saved wording after Undo;
   corrected to use the acknowledged operation's label.
2. Control/copy pass: added installed-library icons, distinct English guidance,
   composition and storage guards, and secondary-action keyboard clearance.
   Enlarged seek target from 24 to 44 CSS pixels. Added 18 test cases.
3. Responsive/RTL inspection: all measured custom buttons and seek target at least
   44 CSS pixels; no horizontal DOM overflow in four widths. Correct viewport
   captures exist, but full-page crop and density mismatch prevent final visual
   acceptance. Remaining findings above have not been declared fixed.

## Earlier verification (historical; latest evidence follows)

- Focused Vitest reviewer suite: 9 files, **62/62 passed** (44 prior + 18 added).
- TypeScript: passed; Svelte check: zero errors/warnings.
- ESLint source/E2E and new test file: passed; `git diff --check`: passed.
- Direct Python policies: i18n 45 strings (28 reviewed-source reuses, 17 existing
  unreviewed strings), storage 5/5, frontend guards 33 pins passed.
- In-app browser synthetic fixture: edit switches primary action; Save advances
  only after mock acknowledgement; Undo returns the clip; missing-audio state
  disables judgments but allows Skip; retry preserves draft; locale preserves
  editing. No console errors before intentionally testing missing-audio 404s.
- Fixture backend is permissive. These interactions do **not** prove real-server
  playback, durability, accounting, or production reliability.
- Initial browser run: 43 passed, 2 failed (44.7s). Both failing fixtures used
  nonexistent file-based audio; native media errors correctly disabled judgments.
  The guard was retained. The fixture now fulfills the exact HTML and a decodable
  synthetic WAV through intercepted loopback requests, aborting other requests.
  API behavior remains mocked, and no real backend or production data is used.
- Strengthened the missing-audio test to require disabled primary/Reject controls
  and enabled Skip. Added a native synthetic-audio decoding/timeline/Play/Pause
  test with no media method stub. Focused tests: 4/4 passed (2.4s).
- Complete reviewer browser regression: **46/46 passed (15.0s)**, no retries or
  skipped cases. Includes light/dark axe WCAG 2.2 AA checks (Sorani reviewing state),
  keyboard clearance, offline/outbox, expiry, retries, Undo and autoplay behavior.
  Command: `node node_modules/@playwright/test/cli.js test --config playwright.couch.config.ts --workers=2 --reporter=line`.
- Post-fixture TypeScript, targeted ESLint and diff whitespace checks passed.
- No Rust, full-app/full-release suite, actual phone/IME, screen-reader, zoom,
  performance baseline, or new production verification was performed. Automated
  axe results do not establish complete WCAG conformance or human audibility.

## Implementation checklist

1. Capture/state mismatch resolved; finish mobile-density design acceptance.
2. Review Kurdish critical instructions with a native speaker.
3. Reviewer browser regressions and the new coupled backend recovery case pass;
   rerun after further implementation changes.
4. Expand the remaining real-server failure matrix and run full Rust/release gates;
   do not treat this one recovery case as proof of every failure path.
5. Complete real-phone, accessibility and keyboard/IME checks, then controlled
   canary rollout with rollback evidence. No rollout GO from this checkpoint.

## Follow-up polish

- Consolidate layered reviewer CSS only after behavioral and visual parity is
  established; it is not a prerequisite for pretending the current gate passed.

## Latest verification and defects fixed — 2026-09-08

Working HTML SHA-256:
`88418682392f49192cc4bda636807b2236b5c1742e7c1968397c310f227f1ef6`.
All changes remain uncommitted on the base SHA above.

- **Recovered-save presentation:** an outbox ACK after reload removed the pending
  operation but left Undo hidden and could leave stale not-sent text. Regression
  failed before the fix and passes afterward. Current-author ACK now refreshes
  session progress, Saved/Undo, and the visible empty state. Attribution fences
  remain; this does not establish Undo availability after every ordinary reload.
- **Terminal playback traversal:** the coupled real-server test exposed HTTP 428
  after complete 1.5-second playback. Chromium emitted a paused `timeupdate` before
  pause/ended, clearing the last anchor and losing the final interval (sampled
  coverage stopped at 1259ms). The handler now records the bounded continuous tail
  before clearing the anchor. Seeking is excluded; unique interval/revision rules
  and the server's 85% threshold are unchanged. New unit regression failed at
  1250 versus 1500ms before the fix; terminal-event and paused-seek tests now pass.
- Final focused unit suite: **64/64, 9 files, 2.17s**.
- Final reviewer browser suite: **54/54, 26.6s**, no retries/skips. Four viewport
  widths × English/Sorani, actual native seek, 44px controls, keyboard clearance,
  recovered Undo and existing failure cases. Latest screenshots:
  `test-results/guided-terminal-final/`. API behavior in this suite remains mocked.
- TypeScript, targeted ESLint, Prettier, cargo fmt and diff whitespace checks pass.
- Python policies pass: i18n 45 strings (28 reviewed-source reuses / 17 existing
  unreviewed), storage 5/5, frontend guards 33, playback readiness 40.
- Existing real-HTTPS backend test passes on final HTML: **1/1, 4.32s**.
- New explicit real Chromium + HTTPS + SQLite test passes twice: **1/1, 7.20s**
  and **1/1, 6.77s**. Real native playback and finalization, request aborted before
  forwarding, response withheld after commit, browser-state restoration, actual
  server-process kill/restart, same-UUID duplicate ACK, and actual UI Undo.
  Independent Rust DB reads prove zero effects/credits before forwarding; exactly
  one retained correction/effect/credit after commit; after replay and Undo, one
  reversal, two immutable ledger rows and zero net credit. No production data,
  tokens, audio or port 8737 are used. Listener uses the existing disposable
  server fixture; browser requests are restricted to its exact loopback origin.

### Required explicit browser/backend command

From `cortex-speech-app/src-tauri`, after installing the repository's locked Node
dependencies and Playwright Chromium:

```powershell
cargo test --locked --test reviewer_serving_path real_browser_replays_lost_request_and_response_once_across_restart -- --ignored --exact --nocapture
```

The browser-dependent test is deliberately ignored by ordinary Rust-only runs.
For reviewer releases this command is a required separate checklist item; an
ordinary `cargo test` result is not evidence it ran. The local verification also
used `--offline --jobs 2` and the existing compatible target cache. This command
  is now wired into the Windows CI and release workflows, alongside the two-tab
  regression described below. Remote workflow execution has not been performed.

Remaining coupled coverage includes distinct-clip/multi-reviewer concurrent tabs, stale reopened rounds,
real corrupt/range-failed media, identity switches and the full HTTP-failure
matrix. Native phones/IME, manual accessibility, critical Kurdish copy, performance
and full candidate release/rollback/canary gates remain open. Synthetic audio
traversal is not evidence of human audibility or transcript/dataset quality.

## Subsequent backend checkpoint — simultaneous-tab replay

The new two-tab test exposed a real false-refusal race. Two copies of the same
operation both passed the early operation lookup. One committed and consumed the
playback receipt; the other later answered 428 instead of acknowledging that same
committed operation. This could make saved work appear refused. It was not evidence
of duplicate payment.

`src-tauri/src/couch/decisions.rs` now acquires the existing canonical commit lock
before mutable canonical validation and rereads immutable operation truth after
waiting. Exact replay republishes its Undo identity and authoritative accounting;
different payload/identity remains a conflict. No playback threshold or pay policy
changed. Source SHA256:
`f70c60fc4dedfd645910801b1a524d7846f3dcbffee367917cb489a26dca6185`.

The real-browser test uses two pages sharing cookie/localStorage, different typed
edits on one clip and two valid native-playback receipts. Network barriers require
both locally queued UUIDs to coexist, then force simultaneous copies of the first
replayed operation. Every winning-copy response must be 200; every competing edit
must be 409. Both tabs drain pending operations; the losing draft remains in that
tab's sessionStorage and the refusal is visible. Independent DB reads require the
winning operation receipt only, exactly one effect/ledger credit and exact winning
transcript. A complete user-facing recovery route for the losing text is still
unproven; stored text plus a warning is not sufficient to close that requirement.

Final-source evidence:

- Reviewer-server Rust suite: **184/184**, no ignored tests, 70.16s (2 test threads).
- Explicit real-browser tests: **2/2**, 10.46s, including restart/lost-request/
  lost-response/Undo and the strengthened two-tab case.
- Existing HTTPS serving-path test: **1/1**, 4.10s.
- Targeted Rust clippy (`--lib --test reviewer_serving_path -- -D warnings`)
  passes on the final source; this is not the full all-targets/all-features gate.
- Decision observability and pool-pay policy scripts pass; architecture gate scans
  179 modules and passes; targeted JS lint/format and diff whitespace pass.
- Both Windows workflow YAML files parse and each contains two explicit required
  browser/backend steps, without continue-on-error. This is local configuration
  verification, not a remote CI result.

The second explicit command is:

```powershell
cargo test --locked --test reviewer_serving_path real_browser_two_tabs_preserve_competing_edits_and_credit_only_once -- --ignored --exact --nocapture
```

Implementation Validator verdict: this contention/replay contract is proven in the
disposable fixture; **overall frontend completion remains FAIL/incomplete** because
native copy, recovery UX, broader failure matrix, devices and release gates remain.
No production changes, commit, deployment or new monitoring task.

## Subsequent recovery and shared-phone checkpoint

Read-only draft recovery now provides native text selection/copy, with an
author-scoped clip/revision selector. No auto-resubmit, delete, external upload or
new clipboard-specific permission is required by the application. The browser
test grants its own clipboard permission to inspect the actual copied value.

New drafts bind exact text to reviewer and rowVersion in a sessionStorage sidecar.
Revision mismatch archives/readbacks the original before removing its raw key;
archive failure keeps the original and displays the fresh server transcript with
a storage warning. Legacy drafts without metadata keep ordinary reload behavior,
but are archived instead of autofilled for returned rounds. Closing a tab can
discard drafts: this does not add indefinite transcript retention to localStorage.

Fault injection reproduced a shared-phone privacy failure: when removing an old
reviewer's draft threw, the previous correction stayed visible under the new
identity. The ownership fence now clears old text, playback and decision targets
before changing reviewer; a failed storage read/write/removal leaves no published
reviewer/queue, so direct decision calls cannot post. A later successful retry
loads only the new person's server transcript. Normal identity changes retain the
existing deliberate draft purge rather than exposing another person's work.

Verified current source:

- `npx vitest run couch_page`: 71/71 across nine files, 2.12s. Includes three
  storage-failure handover tests with successful retry, archive failure, same-round
  restoration, stale-round separation and unknown/foreign-owner recovery hiding.
- `npx playwright test --config playwright.couch.config.ts --output
  test-results/draft-recovery-handover-final`: 55/55, 25.6s. The stale-round test
  copies exact Kurdish fixture text via native clipboard, keeps the fresh editor
  unchanged, creates no outbox operation and has zero scoped axe violations.
- Focused recovery screenshot: `test-results/draft-recovery-handover-final/couch-page-Couch-Review-ph-2bcab-anging-the-fresh-transcript-chromium/draft-recovery.png`,
  366 x 261; SHA256 `fdfa28c3536582fc82de2a1f5f0e8ae99325b4c41bc1424b5c41a5257d3311eb`.
  It matches the inspected prior recovery capture byte-for-byte. This is not
  real-phone keyboard/copy or full-layout approval.
- `npx tsc --noEmit`, targeted ESLint and Prettier checks pass. Storage policy
  5/5 and frontend guard policy 33 pass. The storage source policy was strengthened
  for the extracted fence: all three namespaces, failure clearing and ownership
  before queue publication, backed by behavioral tests rather than source alone.
- I18n: 47 strings, 30 exact reviewed-source reuses, 17 pending. `localDraft` and
  `localOnly` reuse existing approved desktop copy. The existing unreviewed
  `refused` text changed to point to recovery and remains native-review pending.
- Current HTML SHA256: `3696fa21d0d8dde710b0fe85e876ea31fee965d3d2431093a19109881474af74`.
  Browser/backend harness SHA256: `6b817b062dc413e8c1d3ad1ade6f3f4affa31aadf96eee6afe2b452874b205b3`.
- Final rebuilt browser/backend test pair: **2/2, 10.47s** (compile 2m01s), using
  `cargo test --locked --offline --jobs 2 --target-dir
  <local-build-cache> --test
  reviewer_serving_path real_browser_ -- --ignored --test-threads=1 --nocapture`
  from `src-tauri`. The two-tab test now also opens recovery, selects the losing
  draft and checks exact native clipboard contents; independent SQLite assertions
  still require one winning effect/credit. The restart/Undo scenario passes too.
  This supersedes the earlier 10.75s pre-handover run, not a remote CI result.

Implementation Validator: PASS for these tested draft/handover contracts; WARN
for session-only retention and unversioned ordinary legacy drafts; FAIL/incomplete
for the full release plan. Browser stale-round tests currently mutate fixture
revision; real owner-reopen with queued prior-round work remains a separate coupled
test, along with the broader failure matrix, native phones/Kurdish approval,
performance and complete release/rollback/canary evidence. Production unchanged.

## Real owner-reopen checkpoint — initial replay ordering corrected

The owner-reopen coverage above is now extended through the actual offline owner
CLI, not a manually changed browser revision. The Rust fixture seeds a canonical
correction through HTTPS, and a second reviewer uses native browser audio to queue
a different edit whose request is deliberately dropped. That same browser tab
stays alive while the parent stops only its temporary server, runs `pool_admin
plan-reopen` and `apply-reopen --confirm-quality-hold`, verifies the pinned snapshot
result and unchanged historical text/ledger count, then resumes the server.

Red reproduction: stale replay correctly answered 409, but the initial page queue
had filtered that operation's clip before learning identity and finishing replay.
The display stayed on a different clip, so same-load fresh-round recovery was not
usable. `doLoad` now finishes identity-held replay and performs one bounded fresh
queue read if any held operation settled. Offline work remains queued/hidden; ACKs
also receive fresh eligibility instead of a stale pre-commit batch.

Required browser assertions: exact old operation bytes on replay, 409 refusal,
empty outbox, fresh served revision/redo marker, no old-text autofill, exact native
copy from read-only recovery, fresh actual playback receipt and new operation UUID,
successful pool correction, and duplicate replay acknowledging that same pool ID.
Independent database checks require no old canonical/pool receipt, one historical
canonical effect, one fresh pool edit, exactly one new 7.5-IQD credit, preserved
historical transcript, and no false two-person resolution of the new round.

- Unit suite: 73/73, 2.35s; new startup cases cover both ACK and refusal. The
  pre-existing unacknowledged-outbox fixture was corrected to actually fail its
  decision transport, with an added assertion that its UUID remains pending.
- Browser suite: 55/55, 26.7s, `test-results/owner-reopen-order-final`.
- TypeScript, targeted ESLint/Prettier, storage 5/5, frontend guard 33 and i18n
  checks pass. I18n counts unchanged (47 / 30 reviewed reuses / 17 pending).
- Both Windows CI and release YAML parse and contain three required, non-optional
  browser/backend commands. No remote workflow result is claimed.
- Targeted final Rust clippy passes with warnings denied (`--test
  reviewer_serving_path -- -D warnings`, 28.68s). This is not the full
  all-targets/all-features release gate.
- Initial corrected owner-reopen case: 1/1, 9.16s. Final combined rerun: **3/3,
  18.65s**, including fresh-operation duplicate and pool-receipt assertions,
  lost-request/lost-response/restart/Undo, and simultaneous competing tabs.
- HTML SHA256: `00517eb20ec59d317a4a5517dcd91b06f17272fb8c3eb18a4323edd3112f72dd`.
- Harness SHA256: `20734b23dcb4b08cc26ae67128a103b3a2bff07bf7600a13a6e3df7e0b26465c`.
- Rust serving fixture SHA256: `5838dcfcba390913a2be355d04d5d20d9fce81ddc5f60779acffcc04cb7d6fe7`.

Explicit additional required command, from `src-tauri`:

```powershell
cargo test --locked --test reviewer_serving_path real_browser_owner_reopen_fences_queued_work_and_recovers_the_draft -- --ignored --exact --nocapture
```

This covers one real owner round crossing a held operation, not every reopen/
multi-reviewer interleaving. Missing/corrupt/range-failed media, the broader HTTP
failure matrix, native phone/IME/Kurdish checks, performance, full release gates,
rollback rehearsal and canary remain. Production and datasets were not changed.
