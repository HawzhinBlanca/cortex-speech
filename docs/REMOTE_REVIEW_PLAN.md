# Remote Review — the plan to make it genuinely professional

**Status:** written 2026-07-27, at HEAD `a64a241` (multi-reviewer Couch Review shipped in `2ce269c`).
**Scope:** everything between "two people *can* review from their phones" and "handing this to real
annotators is something you'd defend in front of an auditor."

---

## SHIPPED — status as of 2026-07-27 (HEAD after `3cde966`)

Phases 0, 1 (all), 2.1, 2.4, 2.6, 2.7 and 3.1–3.5 are **done, gated, and pushed**. Two items in this
plan were mis-scoped when it was written, and both corrections are recorded here rather than quietly
dropped:

* **3.1 PWA — a service worker is UNREACHABLE, not deferred.** Service workers require a *secure
  context*. This page is plain HTTP on a LAN or tailnet IP, and Tailscale does not change that (still
  `http://100.x`). Offline caching and Android installability therefore need TLS, which this plan
  deliberately rules out. What shipped is the iOS home-screen standalone app — the part that is
  actually reachable — plus the localStorage outbox, which covers the connectivity gap a service
  worker would otherwise have handled.
* **2.4 IAA was NOT the largest item.** It was sized as needing a per-decision table and a
  double-assignment mechanism. Both already existed: spot checks are deliberately **not leased**,
  because measuring two people independently is the point, so the required overlap is already produced
  as a side effect and `spot_checks` is already one row per (clip, reviewer). Only the export was
  missing. Kappa is still computed by `scripts/agreement_kappa.py`, never re-implemented in Rust.

**Defects found while building, none of them by reasoning:** late-submit overwrite (a stale page could
silently replace another human's verdict); `hidden` defeated by `display:flex` on the review card
(pre-existing); spot checks identified by row state, which broke ordinary reviewing; floor division
exempting short batches from measurement; `list_spot_check_candidates(0)` returning one; three real
a11y violations including an **unlabelled transcript box**; a peer's fresh correction being used as an
answer key (found by the soak test); and a null spot-check report **crashing the whole settings
dialog** (found only by the full e2e suite).

**Still open:** §2.2 (throughput panel + timestamps), §2.3 (audit log), §2.5 (two-browser e2e —
partially covered by the Rust soak test and the phone-page spec), §3.6 waveform, §3.7 per-reviewer
revoke, §3.8 token out of the query string, and a clean `couch.rs` mutation sweep run to completion.

---

## The thing that changes when other people use it

Up to now every reviewer was you. Your incentives were perfect: you wanted the corpus to be right.
The software's job was to not lose your work.

The moment the link goes to someone else, the dominant failure mode stops being a crash and becomes
**a human tapping "✓ Looks good" two hundred times without listening.** Nothing in the system today
detects that. Every gate in this repo — 1,055 tests, fuzzing, mutation testing, the refinery-lift
benchmark — measures whether the *machine* is honest. None of them measure whether the *reviewer*
was. That is the real gap, and Phase 2 exists for it.

The second-biggest gap is smaller but more immediate: **the phone page is in English.** The desktop
app is Kurdish-first with a full RTL Sorani UI; `assets/couch.html` is `<html lang="en">` with
hardcoded strings ("Looks good", "Save & next", "Queue reviewed") and only the *textarea* set to RTL.
A Sorani reviewer gets an English chrome around a Kurdish text box. For the owner that is a shrug;
for a reviewer who does not read English it is the whole experience.

---

## Phase 0 — Unblock (do first, nothing else counts until this is done)

| # | Item | Why | Size |
|---|------|-----|------|
| 0.1 | **Rebuild the exe** | `verify-10` is **RED** on `exe-freshness`: the shipped exe is baked at `a9b7b45`, HEAD is `a64a241`. The multi-reviewer code is committed and gated but **not in the binary you run**. | S |

Close the app, then `npm run tauri build`. First launch migrates the library to schema v43; after
that the old exe will refuse to open it (the forward-compat guard, working as designed).

**Acceptance:** `python scripts/verify_10.py` → `exe-freshness PASS`, verdict GREEN.

---

## Phase 1 — Whether a real reviewer succeeds or gives up

Ordered by how likely each is to end a reviewing session badly.

### 1.1 — Sorani UI on the phone page (**highest-value single change**) — M

Serve the page's strings from the existing `en`/`ckb` dictionaries instead of hardcoding English.
Default to `ckb`, set `<html lang="ckb" dir="rtl">`, and offer a one-tap language toggle (the desktop
already has `locale-toggle`).

The i18n parity gate (`scripts/test_i18n_consistency.py`) will then cover the phone strings too, which
is the right place for them to be enforced.

**Note:** the Sorani for these strings is **owner-gated**. Three strings added in `2ce269c`
(`settings.couchReviewers*`) already await native review; a full phone-page translation adds ~15 more.
I can produce drafts limited to vocabulary already in `ckb.ts`, but they are drafts until you read them.

### 1.2 — Never lose a correction to a flaky phone connection — M

Today a dropped request shows `Failed: …` in a toast and the reviewer's typed correction lives only in
a textarea. On a phone at the edge of Wi-Fi this *will* happen.

Three parts, all small:
- **Draft persistence** — mirror the textarea into `localStorage` keyed by segment id, restore on load.
- **Idempotent retry** — make `api_decision` return `200` when this *same reviewer* re-submits an
  identical decision on a row they already decided, so a blind retry is safe.
- **Retry-on-reconnect** — queue the failed submit and flush it on `navigator.onLine`.

**Acceptance:** a fail-before test that drops the request mid-submit and proves the decision still
lands exactly once, and a second that proves a double-submit does not create two learning pairs.

### 1.3 — Lease expiry must not eat an in-progress edit — S

A 15-minute lease can expire while someone is genuinely working on a hard clip. They then get a `409`
at save with their correction stranded. Fix: the page heartbeats the lease while the clip is open
(`POST /api/renew`), and the server extends it. If renewal fails, warn *before* they finish typing.

### 1.4 — Playback that a transcriber can actually use — M

The desktop has `AudioPlayer` with bounded clip playback, speed and loop. The phone has a bare
`<audio controls>`. Add speed (0.75× / 1× / 1.25×), a replay-last-2-seconds button, and loop. This is
the difference between a reviewer doing 40 clips an hour and 90.

### 1.5 — Prefetch the next clip's audio — S

Latency between clips is dead time. Preload the next one or two while the current is being reviewed.

### 1.6 — Rate limiting on the couch endpoints — S

Every desktop IPC command goes through `RATE_LIMITER` / `STRICT_RATE_LIMITER`. The couch HTTP routes
have **none** — a stuck page in a reload loop can hammer the DB unbounded. Add a per-token limiter
mirroring the IPC one, plus a lockout after repeated bad tokens.

---

## Phase 2 — Proof of work

Two different meanings, both of which you asked for. Both matter.

### A. Proof that each reviewer did real work

#### 2.1 — Gold spot-checks (**the one that actually protects the corpus**) — M

Seed each reviewer's queue with a small, invisible fraction (5–10%) of clips whose correct transcript
you already hold (`is_gold` exists). Score their answers against it silently.

This is how every professional annotation platform detects a blind-accepter, and the schema is already
90% there. Output: a per-reviewer accuracy on known answers. A reviewer at 95% is trustworthy; one at
55% is guessing, and you find out on day one rather than after you have trained on their labels.

**Acceptance:** a test that plants known-answer clips, simulates a blind-accepter, and asserts the
report flags them.

#### 2.2 — Per-reviewer throughput and quality panel — M

A Settings/Stats view: clips reviewed, edit rate, reject rate, median seconds per clip, gold accuracy.

Prerequisite: **the couch currently passes `None` as `timestamp_ms`**, so phone decisions never reach
`decision_log` and are invisible to `stats.rs`. Pass a real timestamp. Note this changes what the
existing median-seconds metric measures (it would start including phone reviews) — that is *more*
honest, but it is a deliberate change to a shipped number and should be recorded as one.

Once `decision_log` has phone rows, the `annotator` column I removed in `2ce269c` gains a real writer
and should be re-added — **with** a writer, not before one.

#### 2.3 — Append-only audit log — S

One immutable row per phone decision: reviewer, timestamp, segment, before/after text. Today only the
final state survives, so "who changed this and when" is answerable only for the *current* value.

### B. Proof that the system itself works

#### 2.4 — Inter-annotator agreement, properly — L

The math already exists and is unit-tested against the textbook example:
`cortex-speech-app/scripts/agreement_kappa.py`. Its own docstring anticipates exactly this moment.
What is missing is the data path, and it is a real schema change:

1. A **per-decision table** (multiple decisions per segment) — the current one-row-per-segment schema
   cannot express two people's answers to the same clip.
2. **Deliberate double-assignment** of a sampled percentage. Note this directly *opposes* the lease,
   which exists to prevent overlap — so it must be an explicit "this clip is an agreement sample",
   not a loosening of the lease.
3. Export the TSV the kappa script already consumes.

**This is the only item here that unblocks an owner-gated leg** (the CORDI/independent-annotator
agreement legs). It is also the largest. Do it after Phase 1 and 2.1, not before.

#### 2.5 — Multi-phone end-to-end gate — M

Current coverage is unit tests plus one real-HTTP test with two tokens. Add a Playwright gate driving
**two real browser sessions** against a live server: no clip overlap, correct attribution, undo
isolation, 409 on a stolen lease.

#### 2.6 — Soak test with injected network failure — M

Three simulated reviewers, 500 clips, randomly dropped requests. Assert: zero double-decisions, zero
lost decisions, zero leases stranded past TTL. This is the test that would have caught my own
denial-of-service-by-typo bug before I found it by writing a gate.

#### 2.7 — Extend the mutation and a11y gates to the phone surface — S

`.cargo/mutants.toml` examines five core modules; `couch.rs` is not among them, and it now carries
real decision logic. Likewise `e2e/axe.spec.ts` covers the desktop App root and Settings — **not the
phone page**. Both are one-line scope additions plus whatever they then surface.

---

## Phase 3 — What makes it feel professional

| # | Item | Why | Size |
|---|------|-----|------|
| 3.1 | **PWA: manifest + service worker** | A home-screen icon, full-screen, no browser chrome, survives a brief blip. Single biggest "this is a real app" signal on a phone. | M |
| 3.2 | **Swipe gestures + big tap targets** | Swipe right = accept, left = bad. Thumb-reachable buttons. Volume-of-work UX. | S |
| 3.3 | **Progress and pacing** | "23 of 180 · ~40 min left". Reviewers finish sessions they can see the end of. | S |
| 3.4 | **Font-size control** | 18px Sorani is small for some readers, and there is no control. Accessibility, not decoration. | S |
| 3.5 | **Light/dark** | The page is hardcoded dark; a phone in daylight is unreadable. Respect `prefers-color-scheme`. | S |
| 3.6 | **Waveform** | Seeing silence vs. speech makes clipped-audio judgements fast and consistent. | M |
| 3.7 | **Per-reviewer revoke** | Today removing one person means restarting the server and reissuing *everyone's* link. | S |
| 3.8 | **Token out of the query string** | Set it as a cookie on first load and `history.replaceState` it away, so it stops appearing in browser history and any proxy log. | S |

---

## Deliberately NOT doing — and why

- **TLS / HTTPS on the couch server.** A self-signed cert on a LAN buys browser warnings, a trust
  dialog on every phone, and certificate lifecycle work. **Tailscale already provides WireGuard
  encryption and device identity**, and it is already wired in and surfaced. The correct posture is
  to *mandate the tailnet for anyone off your LAN*, not to bolt weak TLS onto plain HTTP.
- **Public internet exposure.** No port-forwarding, no tunnel, no relay. Voice is biometric data
  under GDPR Art. 9 and the consent model in this repo assumes it never leaves your control.
- **Accounts, passwords, OAuth.** Per-reviewer tokens plus tailnet identity are sufficient and carry
  no credential-storage burden. Adding auth would be the single largest new attack surface here.
- **A fair-share queue scheduler.** Whoever loads first takes up to 25 clips. It is bounded,
  self-correcting (leases expire, batches drain), and now honestly reported via `heldByOthers`.
  A scheduler would be real complexity to fix a self-healing imbalance.
- **Krippendorff's alpha (>2 raters).** Cohen's kappa covers the two-rater case you will actually
  hit first. YAGNI until a third annotator exists.

---

## Recommended order

```
Phase 0.1                      →  clears RED, gets the shipped code into the binary
1.1  Sorani phone UI           →  the reviewer can read the app
1.2  never lose a correction   →  the reviewer's work survives their network
1.3  lease heartbeat           }
1.4  playback controls         }  →  the reviewer is fast and unfrustrated
1.5  prefetch                  }
1.6  rate limiting             →  one bad page cannot degrade everyone
2.1  gold spot-checks          →  you can trust what comes back      ← do not skip
2.2  reviewer panel + timestamps
2.3  audit log
2.5/2.6/2.7  the gates that prove the above
3.x  polish, in the order that annoys you most
2.4  inter-annotator agreement →  largest; unblocks the owner-gated legs
```

**If only three things get done: 0.1, 1.1, and 2.1.** Rebuild it, let the reviewer read it, and be
able to tell whether they were honest. Everything else is improvement on a system that already works;
those three are the difference between a tool you use and a tool you can hand to someone else.

---

## Honest notes

- **Sorani translations throughout Phase 1.1 are owner-gated.** I will not inject unverified Sorani
  into a Kurdish-first app. Drafts limited to existing vocabulary, yes; shipped without your read, no.
- **Nothing in this plan has been implemented.** It is a plan. Every item lands with its own
  fail-before gate and real gate output in `PROGRESS_LEDGER.md`, per the charter, or it does not land.
- **2.2 changes a shipped metric.** Feeding phone decisions into `decision_log` alters what
  `stats.rs`'s median-seconds-per-decision measures. More honest, but a deliberate change — flagged
  here so it is never a silent one.
