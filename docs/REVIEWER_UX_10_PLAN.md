# Reviewer UX 10/10 — the plan to a world-class review experience

**Owner directive (2026-07-30):** Tailscale (or similar) may live on the owner's PC, but reviewers
must have effortless access and "true best user experience, Apple-quality smoothness."

**Grounding:** every claim below was produced by a 10-agent research pass over the ACTUAL page
(`couch.html`, line-cited), the ACTUAL server (`couch.rs`), the settled decisions in
`REMOTE_PUBLIC_LINKS_PLAN.md`, and 2026 platform sources — then adversarially verified. The verify
pass killed two duplicate proposals, exposed two fake gates, and found one real defect in the shipped
page (R3.2 below). Findings that could not be verified are marked UNKNOWN, not assumed.

---

## What "10/10" means here, operationally

Feelings are not a plan. This is the bar, and every number must be MEASURED on a real device over
the real Funnel link before it may be claimed:

| # | The number | Target |
|---|---|---|
| N1 | Link tap → first clip audibly playing (cold, cellular, first-ever visit) | ≤ 15 s, exactly 2 taps |
| N2 | Taps per clip in steady state (listen → decide → next clip playing) | **1** |
| N3 | Decision tap → next clip audible | ≤ 1 s |
| N4 | Work lost under any single failure (offline, throttle, restart, reload, kill) | **0**, and the reviewer can SEE that |
| N5 | A first-time reviewer completes their first clip with zero instructions | yes, on a real stranger |

Two hard gates that no engineering can substitute for:
- **Sorani native review** of every `source: null` string (8 today, more added by this plan). A
  reviewer-facing product in draft Kurdish is not 10/10, whatever the code does.
- **The real-device hour** (R5). Nothing in this plan may be claimed on emulator evidence.

---

## Phase R0 — Go public safely (owner-gated, prerequisite for half the plan)

Already designed and code-ready in `REMOTE_PUBLIC_LINKS_PLAN.md` Phase 5. Owner clicks
`https://login.tailscale.com/f/serve?node=nQAFQWFnjm11CNTRL`, then Serve → verify over ts.net →
Funnel. Same commit: remove legacy `?t=` auth, add conditional `Secure` cookie.

Why it leads: HTTPS is what switches on the already-shipped Screen Wake Lock (verified: iOS 16.4+,
fixed in standalone apps since iOS 18.4), and the ts.net URL is the ONE canonical origin that
dissolves the localStorage origin-split. Every latency number in R1 must be measured over Funnel,
not LAN — the ~83 ms LAN fetch does not transfer (Funnel bandwidth is rate-limited, value
undisclosed — verified against Tailscale docs).

**Gate:** an outside phone on cellular, nothing installed, opens the `#t=` link → claims cookie →
plays a clip. Numbers logged in the ledger.

## Phase R1 — Transport: make audio feel instant (server, ~2 days)

The page pays for the same bytes up to three times and the server forbids caching and ranges.
Verified facts: audio is uncompressed 16 kHz WAV at 32 KB/s (300–500 KB/clip); every reply is an
unconditional 200 full body with ONLY Content-Type set (grep: zero `Range`, `ETag`, `Cache-Control`
in couch.rs); each request re-decodes and re-slices the source file; `loadWave()` re-downloads the
exact URL the `<audio>` element just fetched; prefetch fetches a third copy the player ignores.

1. **HTTP Range (206) + HEAD on `/api/audio`.** Verified platform contract: Safari probes media with
   a 2-byte range request and full streaming robustness expects 206. Playback DOES work today
   (measured: 26 clips reviewed on the owner's iPhone) — this is about scrubbing precision and
   cellular robustness, not a dead player. HEAD support because some mobile stacks probe with it
   (today every non-GET/POST is 404).
2. **`Cache-Control: private, max-age=31536000, immutable` + ETag on audio.** A segment's bytes are
   immutable by construction. This makes replay and prefetch free.
3. **Server-side byte cache** (small LRU keyed by segment id) so a replay does not re-decode the
   172 MB source. Bounded (~32 MB = ~80 clips).
4. **Single-fetch client buffer:** one fetch per clip feeds the player (blob URL), the waveform
   (`decodeAudioData` on a *copy* — it detaches its buffer), and the prefetch slot. Halves cellular
   data and battery per clip.
5. **`pendingTotal` in the queue payload** so the page can show honest overall progress (today the
   un-leased remainder beyond the 25-batch is invisible by design).
6. Truth-fix while in the file: the limiter doc says "120/min" but the bucket refills ~120/second
   (verified in throttle.rs) — correct the comment, keep the behavior.

**Gates:** `curl -H "Range: bytes=0-1"` → 206 with correct Content-Range; 10-clip drill shows
≤ 11 audio GETs total (today ≥ 20); repeat-open of a reviewed clip produces 0 audio bytes on the
wire; all existing 1094 Rust + 74 e2e tests stay green.

### R1 status — shipped 2026-07-30 (iteration 215), items 1/2/3/5/6

Items 1, 2, 3, 5 and 6 are in. Proven by six new Rust tests, two of which fail on pre-R1 code (206 +
Accept-Ranges; 304-without-decode) and two more under targeted reverts (HEAD routing; `pendingTotal`).
Real-HTTP coverage drives a decodable 16 kHz WAV through tiny_http, so HEAD's body suppression and
`Content-Length` are measured rather than read off the crate source. Measured gates: Rust
`1100 passed / 0 failed` (lib, exit 0), whole-workspace `cargo test` exit 0, clippy `-D warnings`
exit 0, `cargo fmt --check` exit 0, couch-page Playwright `32 passed` stable 3-for-3, python-policies
`45/45` exit 0, typecheck `426 files 0 errors` exit 0.

**Item 4 (single-fetch client buffer) is deliberately NOT shipped, and the reason is a real finding,
not a shortcut.** Item 2 largely subsumes it: the second and third fetches of a clip are the *same URL*
the `<audio>` element already fetched, so `Cache-Control: private, max-age=31536000, immutable` makes
them memory-cache hits at zero bytes on the wire — which is the benefit item 4 was scoped to deliver.
What item 4 would add on top is one fewer cache lookup, against rebuilding the player around blob URLs:
the one part of this page measured working on a real iPhone (26 clips reviewed). Not worth that risk
before the R5 device hour, and R2 rebuilds the player anyway. Revisit as part of R2, or drop it.

**Correction to this plan's own R1.3 rationale.** It said each request "re-decodes and re-slices the
source file". Half right: `audio.rs` already LRU-caches decoded source PCM, so the *decode* was cached.
What was not is worse and was missed — `pcm_cache_key` opens the source file and blake3-hashes **all of
it** before that cache can be consulted, then the hit does `cached.clone()` on the entire decoded PCM.
So every `/api/audio` request re-read 172 MB and memcpy'd ~172 MB more, then re-sliced and re-encoded
the WAV sample by sample. The byte cache short-circuits all of it; the justification is stronger than
the plan claimed, for a different reason than the plan gave.

**Measured live on the owner's running server** after the rebuild (exe at `df5d4d1`, freshness gate
OK), driven over real HTTP with a real reviewer token:

| Probe | Result |
|---|---|
| `POST /api/claim` → `GET /api/queue` | 200 / 200 — 29 items, `pendingTotal=116` |
| `GET /api/audio/<id>` | 200, 390060 bytes, `ETag: "b386c3e8ff2e48a3"`, `Cache-Control: private, max-age=31536000, immutable`, `Accept-Ranges: bytes` |
| `Range: bytes=0-1` (Safari's opening probe) | **206**, `Content-Range: bytes 0-1/390060`, exactly 2 bytes returned |
| `HEAD` | **200**, `Content-Length: 390060`, 0-byte body |
| `If-None-Match: "b386c3e8ff2e48a3"` | **304**, 0-byte body |
| `Range: bytes=99999999-` | **416**, `Content-Range: bytes */390060` |

`pendingTotal=116` matches the real backlog this plan cites in R1's motivation, so the honest-progress
denominator is now the corpus rather than the 25-clip batch.

**Still not measured, and not claimed:** the ≤ 11-audio-GETs-across-a-10-clip-drill count, and whether a
real browser actually elides the repeat fetch (the server-side 304 is proven; the client-side cache hit
is a device behaviour). Both are real-device numbers and belong to the R5 hour with a phone on cellular.

**Harness note for whoever verifies this next.** Windows PowerShell 5.1's `Invoke-WebRequest` refuses
restricted headers via `-Headers` (`The 'Range' header must be modified using the appropriate property
or method`), and the thrown error then poisons every later probe on the same session — which is exactly
how a first pass produced three blank results that looked like server failures and were not. Use
`System.Net.Http.HttpClient` with `HttpRequestMessage.Headers.Range`, or `curl.exe`. The token stays in
a variable and is presented via `POST /api/claim` so it never reaches a URL or a command line.

## Phase R2 — One tap per clip (client, ~2 days)

The Apple-quality core. Today every clip costs a reach-and-tap on the small native player control.

1. **First-visit welcome gate that doubles as the iOS audio unlock.** First time a reviewer (keyed
   by server-declared name) opens the page: a minimal panel — their name large, one line of what
   the job is, one **Start** button. That tap calls `player.play()` SYNCHRONOUSLY in the touch
   handler, unlocking the shared `<audio>` element for programmatic play for the session. This is
   the only new screen, and it is also the onboarding (N5) and the identity confirmation.
2. **Auto-play the next clip after a decision** (`show()` plays when the advance came from a
   decision). With the unlocked-element pattern this is the difference between 2 taps/clip and
   1 tap/clip across a 100-clip session. RISK, stated: iOS may refuse `play()` after an awaited
   POST even on an unlocked element — `.catch()` degrades to today's behavior; real-device verify
   in R5 decides.
3. **Pause on edit, rewind 2 s on resume** — the Express Scribe/Descript mechanic, reusing the
   existing ↺2s logic. Focus the textarea while playing → pause; next play → back 2 s.
4. **Keyboard-safe action row:** `interactive-widget=resizes-content` in the viewport meta (Android,
   declarative) + a ~15-line `visualViewport` listener driving a `--kb` bottom padding variable
   (iOS). Today the iOS keyboard covers the Save/Accept/Reject row and the toast — the single worst
   flow-breaker on the page (verified gap).
5. **3 px batch progress bar** under the header, RTL-aware, filling across the leased batch —
   endowed progress, complementing the existing "clip n of N · ✓k" counter.
6. **Safe-area insets** (`env(safe-area-inset-*)`) so installed/standalone mode does not draw
   under the notch (page ships `viewport-fit=cover` with a flat 12 px padding today).

### R2 status — shipped 2026-07-30 (iteration 217), items 2/3/4/6

Item 1 (welcome/Start gate) is NOT needed: its only job was unlocking the `<audio>` element for
programmatic play on iOS, and the first clip's own play button already does that — iOS permits
programmatic `play()` on an element a gesture has started. Auto-advance therefore works from clip 2
onward with no new screen, no first-visit keying, and ZERO new unreviewed Sorani strings. Cost: clip 1
takes two taps. Item 5 (3 px bar) skipped as decoration — it fixes no defect and the text counter now
counts the real backlog. Items 2, 3, 4 and 6 are in, with five fail-before reverts recorded in the
ledger. R2.4 needed two attempts: `scrollIntoView({block:'end'})` aligns to the LAYOUT viewport, whose
bottom edge is the part behind the keyboard, so it lands the action row exactly where it cannot be seen
(measured twice at y+h = 604px with 400px visible); the fix scrolls by the overshoot measured against
the VISUAL viewport. Gates: couch-page 36 passed stable 3-for-3, e2e 83 passed, policies 45/45,
typecheck 426, clippy/fmt/Rust all exit 0.

**Gates:** real-iPhone drill: 10 consecutive accepts = exactly 10 taps with clip N+1 audible within
1 s each (N2, N3); keyboard-open screenshot shows 100 % of the Save button and the toast inside the
visual viewport on one real iPhone and one real Android; bar at clip 13/25 measures 48 % ± 4 % and
mirrors under RTL.

## Phase R3 — Trust made visible (client, ~1.5 days)

N4 exists in the machinery (outbox, drafts, dedup, attribution) but is INVISIBLE at the moments
that matter.

1. **Ambient sync pill** in the header: hidden when clean; "{n} waiting to send" whenever the outbox
   is non-empty; offline state from `online/offline` events. Today the outbox count surfaces only in
   the drained-queue state or a 1.4 s toast — a reviewer on flaky signal has no ambient signal at
   the exact moment trust is decided. (NEW Sorani string — owner-gated draft.)
2. **No dead ends — and fix the false link-expired defect.** VERIFIED DEFECT found by the
   adversarial pass: the fragment→cookie claim is a one-shot `const` promise created at script
   parse. A first-ever visitor whose claim POST fails transiently (server restarting, cellular blip)
   is shown "link expired" — a false terminal state for a perfectly good link — until a manual
   reload, which an installed standalone app cannot even do. Fix: make the claim retryable; add ONE
   timeout implementation (AbortController inside `api()`, ~15 s); add a localized Retry button to
   every terminal state; gentle auto-retry (60 s cadence) while the server is unreachable, riding
   the cookie, never hammering claim. The two overlapping proposals from the design pass are merged
   into this single implementation (the verify pass flagged them as duplicates).
3. **Failures you can read:** failure toasts persist until dismissed (success keeps 1.4 s);
   NO raw English server text inside Sorani sentences — repo grep gate: zero `e.message` inside any
   `t(...)`/textContent assignment in couch.html.
4. **A11y floor:** `aria-live=polite` on toast/progress/warn/done; real `aria-label`s on the
   icon-only loop/undo/A± controls. Gate: the existing axe WCAG 2.2 AA suite extended to assert the
   live regions exist.
5. **Swipe affordance:** card follows the finger (CSS transform), edge tint past the 90 px
   threshold, snap-back under it. No haptics — VERIFIED: `navigator.vibrate` never existed on iOS
   Safari and the checkbox-switch hack was patched away in iOS 26.5. Gate is the visual drill, not
   network counts (the originally proposed network gate already passes today — fake gate, killed).

## Phase R4 — Install and polish (client + owner, ~1 day)

1. **Install nudge, exactly one line, only at the "all reviewed" celebration.** Zero install UI in
   the first-minute path. iOS 26 (verified): Share → Add to Home Screen now opens sites as web apps
   by default, so the value is real. TWO owner gates, stated: the line is new Sorani, and the settled
   Phase-6 scope cap ("icon + wake lock + manifest and nothing else") must be consciously amended —
   this plan requests that amendment rather than silently violating it.
2. **iOS standalone cookie trap — device-test before shipping the nudge.** VERIFIED: iOS does NOT
   share cookies/localStorage between Safari and an installed standalone app. The installed app
   opens token-free `start_url "/"` → possibly 401 on first launch. UNKNOWN whether iOS 26 routes
   the original `#t=` chat link into the installed app's context (which would mint the cookie there
   cleanly). The R5 device hour answers this BEFORE the nudge ships; if it fails, the nudge copy
   must say "open your link once after installing."
3. **Refused-banner follow-through:** the banner says "find those clips and review them again" but
   offers no way to find them. Smallest honest fix: when a refused clip re-enters the queue, badge
   it; tapping the banner jumps to the first such clip if present.
4. **"Unsure" escape (owner data-policy decision, surfaced not decided):** today a reviewer who can
   hear the audio but genuinely cannot judge it must pick accept/reject/edit or stall — Skip only
   appears on audio ERROR. An explicit no-verdict "unsure" that writes nothing but pushes the clip
   to the back changes corpus semantics, so it is the owner's call, not a UX default.

## Phase R5 — Proof: the real-device hour (owner + one hour, the only credible gate)

One sitting, over the FUNNEL link on cellular (not LAN, not simulator), logged in the ledger with
real numbers:

- iPhone Safari: N1–N5 measured; autoplay-after-decision verdict (R2.2 risk); standalone
  install → first-launch cookie behavior (R4.2 UNKNOWN resolved); wake lock lights up over HTTPS.
- Android Chrome (any available device): keyboard resize, install flow, N2/N3.
- Mac Safari or Chrome: sanity pass, keyboard-first editing.
- The bad-moments drill, live: airplane mode mid-clip → pill counts up → landing on reconnect;
  PC force-killed mid-batch → watchdog revives → same link resumes (already proven server-side,
  now witnessed from the reviewer's side).

**The 10/10 claim is only printable when:** N1–N5 all hit target on a real device over Funnel, AND
every `source: null` Sorani string has native review, AND zero open defects in the couch suites.

---

## Explicitly rejected or descoped (so future sessions do not relitigate)

- **Undo-survives-reload, as proposed** — REJECTED by the verify pass: the proposed localStorage
  counter corrupts the honest per-sitting pace counter it shares a variable with, and conflates the
  server's three distinct undo-409 meanings (empty stack / staleness fence / another reviewer's
  work) into a lying "nothing to undo." A correct version needs a server `undoDepth` in the queue
  payload — parked as a candidate, not scheduled.
- **Haptics on iOS** — dead. Verified patched away (26.5). Visual snap-back carries the affordance.
- **Service worker** — the settled no-SW decision STANDS (stale-shell risk vs include_str! page;
  outbox covers offline). Nothing in this plan needs one.
- **Web Audio API for playback** — rejected: verified iOS behavior routes Web Audio through the
  ringer/silent switch while `<audio>` elements ignore it. A review app that goes silent when the
  phone's mute switch is on would be a support nightmare. The `<audio>` element stays the player.
- **Duplicate timeout/retry proposals** — merged into R3.2 (verify pass caught the collision).
- **Opus/AAC transcoding** — deferred, not rejected: real ~10× bandwidth win but needs an encoder
  dependency and an iOS-decode matrix; R1's cache + single-fetch + Range lands most of the felt
  improvement without a new codec surface. Revisit only if R5's cellular numbers miss N3.

## Owner-gated ledger (nothing here moves without the owner)

1. R0 Serve/Funnel enablement click.
2. Native Sorani review: the 8 existing `source: null` strings + every new string this plan adds
   (sync pill, retry, install nudge, welcome gate). The single largest gap between "engineered
   well" and "feels native."
3. Phase-6 scope-cap amendment for the install nudge (R4.1).
4. "Unsure" verdict data-policy decision (R4.4).
5. The R5 device hour.

## Sequencing and effort

R1 and R3.2 (the found defect) can start immediately — they need no owner input and no HTTPS.
R2 lands next (welcome gate + autoplay are the N2/N3 unlock). R0 can happen any time and gates the
wake-lock/installed-app legs. R4 waits on R5's device answers. Total engineering: roughly 6–7 focused
days, all inside the existing gate discipline (fail-before proofs, unmasked exits, 3× Playwright
stability, ledger per change).

---

## Where this plan stands — 2026-07-30, end of the build-out

**Everything in this plan that does not need the owner is shipped, gated and verified live on the
running server.** Six iterations (215–220), commits `df5d4d1 … 6337262`, both refs aligned.

| Phase | Item | State |
|---|---|---|
| R1 | 1 Range/206 + HEAD | **shipped**, verified live (206 `bytes 0-1/390060`; HEAD 200 + 0-byte body) |
| R1 | 2 immutable + ETag | **shipped**, verified live (304, 0 bytes) |
| R1 | 3 server byte cache | **shipped** (32 MB, fingerprint-keyed) |
| R1 | 4 single-fetch client buffer | **descoped** — item 2 delivers the same bytes-on-wire saving; see the R1 status block |
| R1 | 5 `pendingTotal` | **shipped**, verified live (`pendingTotal=116`) |
| R1 | 6 limiter truth fix | **shipped** (was wrong by 60×) |
| R2 | 1 welcome/Start gate | **not needed** — the first clip's own play button unlocks the element; see the R2 status block |
| R2 | 2 auto-advance | **shipped** |
| R2 | 3 pause on edit, rewind 2 s | **shipped** |
| R2 | 4 keyboard-safe action row | **shipped** (took two attempts — `scrollIntoView` aligns to the wrong viewport) |
| R2 | 5 3 px progress bar | **skipped as decoration** — the text counter now counts the real backlog |
| R2 | 6 safe-area insets | **shipped** |
| R3 | 1 ambient sync pill | **OWNER-GATED** — needs a new Sorani string |
| R3 | 2 no dead ends / false link-expired | **shipped** (iteration 214) |
| R3 | 3 failures you can read | **shipped** + new policy gate against raw English |
| R3 | 4 a11y live regions | **shipped** |
| R3 | 5 swipe affordance | **shipped** |
| R4 | 1 install nudge | **OWNER-GATED** — new Sorani *and* a conscious Phase-6 scope amendment |
| R4 | 2 iOS standalone cookie trap | **OWNER-GATED** — needs the R5 device hour, and it gates R4.1 |
| R4 | 3 refused-banner follow-through | **shipped** |
| R4 | 4 "unsure" verdict | **OWNER-GATED** — changes corpus semantics; a data-policy decision, not a UX default |
| R5 | the real-device hour | **OWNER-GATED** — one hour with a real iPhone on cellular |
| R0 | Tailscale Serve → Funnel | **OWNER-GATED** — one click at the Tailscale admin URL |

### What only the owner can do now, in the order that unblocks the most

1. **Enable Tailscale Serve, then Funnel** (R0). Until this happens the link is LAN/tailnet-only, and
   every latency number in R1 stays a LAN number. `tailscale serve status` still reports "No serve config".
2. **One elevated run of `scripts/ops/cortex-once-admin.ps1`.** Verified state: `FastStartup=1` (want 0),
   `ARSO` unset (want 1), ActiveHours unset. **A reboot still leaves the review server down** until this
   runs — the watchdog cannot start what the login screen has not unlocked.
3. **The R5 device hour** (R5, and it gates R4.2 → R4.1). The three things only a real phone can answer:
   does auto-advance actually play on iOS after an awaited POST; is 100 % of the Save button inside the
   visual viewport with the keyboard open; does the `#t=` link mint the cookie inside an installed
   standalone app.
4. **Native Sorani read of the 8 unreviewed strings** (`audioMissing`, `heldByOthers`, `linkExpired`,
   `loadingMore`, `queued`, `refused`, `skip`, `stillSending`). Nothing added them this week — R1–R5 as
   built needed **zero** new Sorani.
5. **Two decisions**: amend the Phase-6 scope cap for the install nudge (R4.1), and rule on the "unsure"
   verdict's corpus semantics (R4.4).

### Not measured, and therefore not claimed

The ≤ 11-audio-GETs-per-10-clip-drill count, whether a real browser elides the repeat fetch (the 304 is
proven server-side; the client cache hit is device behaviour), and every R2 real-device gate. All belong
to the R5 hour.
