<!-- Produced 2026-07-29 by an 12-agent research workflow: 5 parallel researchers (current-state
audit against the real code, access-layer options verified against July-2026 primary docs, PWA/secure
-context capabilities, Windows always-on design, threat model), 6 adversarial verifiers attacking the
load-bearing claims (5 held, 1 refuted and corrected in the text), 1 synthesis pass. Every phase has
a fail-before/real-run acceptance test per the project charter. -->

# Remote Review: Public Links Plan

**Requirement:** any reviewer opens a link on any device (nothing installed), reviews, closes, returns later — owner's PC always on, links survive everything until explicitly revoked.

**Merge notes honored:** the adversarial pass refuted one claim — Pinggy and LocalXpose *do* offer device-terminated TLS as hosted services. The corrected comparative is: **nothing matches Funnel's zero-config device-terminated TLS** (auto certs, stable ts.net URL, already installed), not "nothing else offers it." Primary choice unchanged; the plan's rationale text below uses the corrected wording. All other verified verdicts hold.

---

## Phase 0 — What already works (no work; baseline for every acceptance test)

Verified in code and pinned by tests:

- **Durable sessions** (just shipped): `couch_session.json` — DPAPI-protected tokens, atomic write, db-path-bound, token reuse by reviewer name, resume at app launch, Stop deletes and revokes everything. Test `a_link_survives_closing_the_app_but_not_pressing_stop` pins it. (`couch.rs:217-331`, `lib.rs:778-793`)
- **Multi-reviewer server** on `0.0.0.0:8737`: 244-bit tokens, attribution, per-reviewer undo, 15-min leases + heartbeat, 409 collision/late-submit guards, idempotent retry, spot checks, per-reviewer rate limit, append-only audit trail, per-reviewer revoke, restore-fence integration.
- **Phone page**: Kurdish-first RTL, localStorage outbox with correct hold/drop semantics, drafts, waveform, transport, iOS home-screen meta. Token moves `?t=` → HttpOnly SameSite=Strict cookie, URL stripped; empty-`?t=` fallthrough regression-tested over real HTTP.
- **Two URLs per reviewer** (LAN + CGNAT-verified Tailscale), reliable Stop/port release, Content-Length on audio.

**What Phase 0 does NOT solve:** a link only works for someone on the owner's LAN or enrolled in the tailnet; the server dies with the app process; several docs and the `couch.rs` header now lie about token persistence.

---

## Phase 1 — Tell the truth (docs + header reconciliation)

**Goal:** the repo stops contradicting its own shipped behavior before anything is built on top of it. (Gap 8 — honesty-gate violation, already live.)

**Steps:**
1. Rewrite `couch.rs:22-35` header: tokens ARE persisted (DPAPI, db-bound), revoked only by Stop/revoke; keep the "must not be naively port-forwarded" stance but note the sanctioned path is device-terminated TLS (Phase 5).
2. Fix `docs/REMOTE_REVIEW_RUNBOOK.md:15-17, 39`: closing the app no longer revokes links; Stop does. Document the lost-phone procedure: Settings → revoke reviewer (immediate), Stop = nuke all.
3. Fix `docs/REMOTE_REVIEW_PLAN.md` "Still open" list: 3.6 waveform, 3.7 revoke, 3.8 cookie, 2.3 audit log have shipped.

**Acceptance test:** grep proof — `grep -n "never persisted" src-tauri/src/couch.rs docs/REMOTE_REVIEW_RUNBOOK.md` returns zero hits (fails today: 3 hits). Manual read-through of the three files against `couch.rs:229-243` behavior.

**Risks:** none technical; risk of half-updating (fix the header, miss the runbook).

**Does NOT solve:** any functional gap. Pure honesty. Links still tailnet/LAN-only, server still dies with the app.

---

## Phase 2 — Fragment bootstrap: make links safe to send through a chat app

**Goal:** eliminate the certain token leak before any public exposure. Today `?t=TOKEN` in a WhatsApp/Telegram message means the platform's preview bot fetches the URL; behind Funnel that fetch would *succeed* and hand the platform a durable credential to biometric audio. Fragments (`#t=`) are never sent to any server — bots, proxies, and Tailscale relays never see the token. (Threat-model Risk 1; verified certain, not tail-risk.)

**Steps:**
1. Serve the static HTML shell **unauthenticated** on `GET /` (it contains no data; all data lives behind `/api/*`, which stay token-gated). This trades "every route gated" for "every DATA route gated" — the right trade once the URL is public.
2. Add `POST /api/claim`: body carries the token once; reply sets the existing HttpOnly SameSite=Strict cookie. Reuse the existing cookie-mint code at `couch.rs:652-657`.
3. `couch.html`: read `location.hash`, POST to `/api/claim`, then `history.replaceState` to strip the fragment — the strip logic at `couch.html:205-214` adapts.
4. Change issued-URL format in `status_of` (`couch.rs:559-560`) and Settings display from `?t=` to `#t=`.
5. Keep `?t=` accepted server-side for one transition release so already-sent links keep working on the tailnet; remove it in the same commit that enables Funnel (Phase 5) — a public server must never authenticate via query string.
6. Do NOT make links single-use or short-lived: preview bots fetch within seconds, before the human clicks — a one-time link would be burned by the bot and 401 the reviewer. (Verified failure mode; explicitly rejected.)
7. Fix Gap 5 in the same file: re-set the cookie (sliding `Max-Age=604800`) on every authenticated page load, so an active reviewer's bookmark never expires while the session lives. The `#t=` home-screen URL also re-plants the cookie on iOS standalone launches (separate cookie jar — already handled by the saved-URL pattern).

**Acceptance test (fail-before / real-run):**
- *Fail-before:* `curl http://127.0.0.1:8737/?t=<valid>` today returns the page + `Set-Cookie` (the leak). After: query-string auth path removed (post-transition) — same curl gets the bare shell, **no** `Set-Cookie`, no data.
- New Rust test over real HTTP: `GET /` unauthenticated → 200 shell, zero data, zero cookie; `POST /api/claim` with valid token → cookie; with garbage → 401; `/api/*` without cookie → 401.
- Phone drill: send a `#t=` link to yourself via WhatsApp; confirm the preview bot's fetch (server sees `GET /` with no fragment — check audit/log) gains nothing; then open it on the phone and complete a review.
- Regression: rerun the existing empty-`?t=`-falls-to-cookie test suite.

**Risks:** shell is now public — confirm by review that it embeds no clip data, reviewer names, or counts; transition window where both `?t=` and `#t=` work must not outlive Phase 5.

**Does NOT solve:** reachability (still tailnet/LAN only until Phase 5); an owner who pastes a link into a channel they don't trust — the link is still a live durable credential and the runbook must say so.

---

## Phase 3 — Always-on: the server survives reboot, crash, and wedge without a human at the PC

**Goal:** "PC always on" actually implies "server always up." Today the server lives inside the desktop app; resume runs only in the Tauri setup hook; a reboot, crash, or silent resume failure (port conflict → `tracing::warn` → nothing) means dead links until someone launches the app. (Gaps 3 + 4.)

**Design (one recurring mechanism, zero app-code changes):** recovery over prevention — don't fight Windows reboots; make every reboot self-heal in ~2 minutes.

**Steps:**
1. **Watchdog script** `scripts\ops\cortex-watchdog.ps1` (outside `target\` so `cargo clean` can't eat it): probe `http://127.0.0.1:8737/` with 5 s timeout; **401 counts as alive** (auth working = server working — verified: 401 path is the unauthenticated answer on every route). Only refused/timeout = dead → `Stop-Process -Force cortex-speech-app` (kills a wedged process; safe — `flock.rs` clears the stale lock, verified with tests), sleep 2, `Start-Process` the release exe detached. Probe-before-launch means it never pops the "Another instance" dialog on a healthy app. Optional one-line healthchecks.io dead-man ping on the alive path (bare HTTPS GET to a UUID URL — liveness + source IP only, no content; owner's phone gets an email/push when pings stop).
2. **One scheduled task, two triggers** (`Register-ScheduledTask 'CortexWatchdog'`): at-logon + every-5-minutes repetition. Settings: `ExecutionTimeLimit` PT0S (default 72 h kills the process tree), `StartWhenAvailable`, `MultipleInstances IgnoreNew`, **"run only when user is logged on"** (never "whether or not" — that's Session 0, the WebView2 GUI can't render there; same reason NSSM/service wrappers are ruled out). This one task IS the autostart answer — beats tauri-plugin-autostart (a rebuild + code surface for what is just an HKCU Run key) because the same mechanism is also the crash/wedge healer.
3. **Auto-login** (survives Windows Update reboots): Sysinternals Autologon (encrypted LSA secret, not plaintext registry) — *owner runs this personally; the plan does not script credential entry*. Win11 24H2 Credential Guard may break it; fallback is `DevicePasswordLessBuildVersion=0` + netplwiz. Immediately pair with a `LockAtLogon` HKCU Run key (`rundll32 user32.dll,LockWorkStation`) — a locked session keeps the app and HTTP server running.
4. **One-time system tweaks:** `powercfg /h off` (kills fast-startup hiberboot, whose boots are documented to skip startup triggers; logon trigger + real boots = immune), `powercfg /change disk-timeout-ac 0`, `Disable-NetAdapterPowerManagement` + disable Energy-Efficient-Ethernet on the live NIC, Ethernet not Wi-Fi, Windows Update active hours set to the 18 h max. Do NOT set `NoAutoRebootWithLoggedOnUsers` — with auto-login someone is always logged on, so updates would pend forever (silent security debt); active-hours only.
5. **Stop procedure** documented in the runbook: `schtasks /change /tn CortexWatchdog /disable` before intentionally quitting the app, or it resurrects within 5 minutes and feels haunted.

**Acceptance test (fail-before / real-run — nothing here counts as done until physically drilled):**
- *Fail-before:* `Stop-Process -Force cortex-speech-app`; confirm phone link is dead and stays dead (today's behavior). Then install the task and repeat: link answers again within ≤5 min with **no keyboard touched**.
- *Reboot drill:* `Restart-Computer`; walk away; phone link answers within ~2 min of desktop appearing; session is locked; same tokens still valid (durable-session test semantics, now across a full power cycle).
- *Wedge drill:* suspend the process (`Debug-Process` or firewall-block the port) so it's alive-but-not-listening; watchdog kills and relaunches on the next cycle.
- *Dead-man:* pull Ethernet for 25 min; owner's phone receives the healthchecks alert.

**Risks:** auto-login is a real physical-security trade (seconds-window before lock; local admin can decrypt the LSA secret) — acceptable for a home box, stated not hidden; NIC driver updates silently re-enable power management (recheck after updates); `cargo test` binds real port 8737, so during a test run the watchdog sees a false "alive" and the app can't rebind until tests finish — transient, self-heals, documented in the runbook; worst-case ~5 min downtime after a crash (tighten interval if it matters); Credential Guard must be verified with an actual restart before trusting it.

**Does NOT solve:** power outage or ISP outage (dead-man ping at least tells the owner); resume failures inside a *running* app are healed by kill-and-relaunch, not surfaced in the UI — the owner learns from the healthchecks alert or not at all.

---

## Phase 4 — Restart-surviving spot-check state (stop silently losing integrity scores)

**Goal:** durable sessions invite app restarts that the in-memory runtime state doesn't survive. Concretely: a spot check served before a restart and submitted after it hits the late-submit 409 (`couch.rs:1067-1072`), the score is never recorded, and the outbox drops it as answered — silently weakening the one mechanism that detects a blind-accepter. (Gap 7; traced in code, "inferred" — the acceptance test below is also the verification.)

**Steps:**
1. Extend `couch_session.json`'s `SavedSession` with the served-spot-check id set (per reviewer). It's a handful of ids; same DPAPI/atomic-write path, no new file.
2. On resume, rehydrate the set into `CouchState` so a post-restart submit of a pre-restart check is recognized and scored.
3. **Deliberately accept** losing undo stacks across restart: undo answers 409 "nothing to undo" for pre-restart decisions — the desktop review flow is the correction path for those. One line in the runbook. (Persisting undo means persisting pre-decision row snapshots — real surface for a rare convenience.)

**Acceptance test (fail-before / real-run):**
1. *Fail-before, proving the inferred bug is real:* start server, serve a spot check to a phone, close the app (not Stop), relaunch, submit the check from the phone → observe the 409 and the missing score. If this does NOT reproduce, Gap 7 is wrong — record that and skip the phase.
2. After the fix: same drill → submit accepted, score recorded in the audit trail.
3. Rust test: serialize session with served-set → reload → served-set intact; plus the restart-crossing submit path over real HTTP.

**Risks:** session-file schema change must tolerate the old schema (missing field = empty set), or resume breaks for the session that's currently keeping real links alive.

**Does NOT solve:** cross-restart undo (accepted, documented); leases (they expire in 15 min anyway — a restart's lease loss self-heals by design).

---

## Phase 5 — Public reach: Tailscale Serve (tailnet HTTPS) then Funnel (internet)

**Goal:** the actual requirement — a person with nothing installed opens the link. Two-notch rollout: Serve first (same URL machinery, tailnet-only — validates the whole HTTPS/proxy/cookie path with zero public exposure), then Funnel (public).

### GDPR / privacy decision point — may clip audio transit a Funnel?

**Decision: YES, Funnel is the sanctioned path — and it is the only zero-cost hosted option that needs no decision-by-exception.** The verify pass confirmed against current primary Tailscale docs (July 2026): TLS **terminates in `tailscaled` on the owner's PC**; "Funnel relay servers do not decrypt the traffic"; certificate private keys are generated and stored locally and Tailscale never sees them. The relay TCP-proxies ciphertext. So the audio is encrypted end-to-end from the reviewer's phone to the owner's machine — equivalent to a reverse proxy that cannot decrypt, which is a defensible Art. 9 transit posture. Tailscale still sees connection **metadata** (SNI, IPs, timing, volume) — accepted and stated here, not hidden. Two irreversibles to acknowledge before enabling: the `machine.tailnet.ts.net` hostname enters the public Certificate Transparency ledger permanently (hostname obscurity is therefore *not* part of the security model — only the token gate counts), and non-configurable bandwidth limits apply (verified still undisclosed; a ~11.6 MB 29-clip batch is a throughput question, not a cap — verified: it's a rate limit, not a size limit).

**Explicitly rejected for audio:** Cloudflare Tunnel — verified against Cloudflare's own docs that TLS terminates at Cloudflare's edge and plaintext exists there even with Keyless SSL; that makes Cloudflare a processor of plaintext special-category biometric data (DPA + transfer homework). It remains the documented *fallback* only if Funnel dies as a product, never silently. ngrok free: interstitial page breaks bare `<audio>` fetches + 1 GB/mo — disqualified. Pinggy/LocalXpose: per the corrected verdict they *do* offer device-terminated TLS, but require manual local cert setup and a new vendor relationship for zero benefit over Funnel, which is already installed — skipped on laziness, not on a false technical claim.

**Steps:**
1. Prereq: Phase 2 shipped (fragment links) and Phase 3 shipped (a dead upstream behind a live funnel is worse than a dead link). Confirm the server answers on `127.0.0.1` (it binds `0.0.0.0`, so yes — Serve/Funnel proxy only to loopback).
2. Remove the `?t=` query-string auth path (end of Phase 2 transition) in the same commit.
3. Conditional `; Secure` on the cookie only when the request carries `X-Forwarded-Proto: https` (~3 lines in `page_reply_with_cookie`) — unconditional Secure would silently break the plain-HTTP LAN/tailnet-IP fallback. The header is trustworthy only because Serve strips inbound spoofed forwarding headers; note the worst case if wrong is a Secure cookie over HTTP that the browser refuses — annoying, not a breach.
4. One-time tailnet setup: enable MagicDNS + HTTPS certificates in the admin console; approve the `funnel` node attribute when the CLI prompts.
5. `tailscale serve --bg 8737` → validate everything over `https://<pc>.<tailnet>.ts.net` from the owner's own phone (tailnet-only, reboot-persistent).
6. `tailscale funnel --bg 8737` → same URL, now public (Funnel supersedes Serve on the port; Funnel includes tailnet access). First enable: DNS can take up to 10 min — turn it on before, not during, a session. Send a `#t=` link to a person on cellular with nothing installed.
7. **Exposure hygiene:** the runbook documents flipping `tailscale funnel off` / back to `serve --bg` between review campaigns — shrinks both the DoS surface and the GDPR exposure window at zero code cost. This is also the chosen answer to thread-starvation DoS (1–8 worker threads, no pre-auth per-IP limit — verified): the 401 path is already allocation-free and unlogged; a pre-auth limiter keyed on `X-Forwarded-For` stays **out** until (open question) it's confirmed Serve actually sets XFF — unverified, do not build on it.
8. Update `couch.rs` header + runbook + PLAN in the same commit: the "no tunneling" stance is replaced by this decision record, with the Tailscale citations inline. Settings UI adds the ts.net URL as the canonical third link.

**Acceptance test (fail-before / real-run):**
- *Fail-before:* cellular phone (Wi-Fi off, no Tailscale app) opens the LAN and 100.x links → both dead (today's behavior, proving the gap).
- Serve gate: owner's phone on the tailnet loads the ts.net URL, cookie round-trips with `Secure`, audio plays, a full review completes; then over plain `http://100.x` the cookie still works (no `Secure` — the conditional held).
- Funnel gate, the requirement sentence made literal: an outside reviewer on cellular, nothing installed, opens the `#t=` link → claims cookie → reviews clips → closes browser → returns hours later → still authenticated → owner reboots the PC mid-drill → reviewer retries after ~2 min and continues with the same link (Phases 3+4 composing).
- Revocation drill: Settings → revoke that reviewer → their very next request is 401; Stop → everyone 401, session file gone.
- Kill-switch drill: `tailscale funnel off` → public URL dead within seconds; LAN and tailnet paths unaffected.

**Risks:** hostname permanently public via CT (accepted — token gate is the entire perimeter, which is why Phase 2 is a hard prerequisite); anything else ever bound on 8737 gets exposed — audit routes before enabling; one-reviewer config = one worker thread, so scanner background noise can make the page feel sluggish (untested under real internet noise — mitigated by funnel-off-between-sessions; add accept threads only if the drill shows pain); renaming the PC or tailnet breaks every sent link; tiny_http slowloris behavior unverified at the library level (open question).

**Does NOT solve:** Tailscale seeing connection metadata; availability hardening against a determined DoS (out of scope for a low-profile 8-reviewer tool — the kill switch is the answer); anything if Tailscale-the-company changes Funnel's terms.

---

## Phase 6 — Small PWA/UX wins the new HTTPS unlocks (strictly capped)

**Goal:** the two or three genuinely useful client improvements — and nothing else. (Background Sync remains unsupported on iOS Safari in 2026; the localStorage outbox already *is* the documented iOS pattern.)

**Steps:**
1. **Icon** (needs no HTTPS — do it even if Phase 5 slips): one 512×512 PNG served by the Rust router + `apple-touch-icon` link. Fixes today's screenshot-icon on iOS (verified: no icon of any kind is declared).
2. **Wake lock** (~10 lines, the one real secure-context win for listen-and-type): feature-detected `navigator.wakeLock.request('screen')` on first play, re-acquire on `visibilitychange`. iOS ≥16.4, home-screen bug fixed in 18.4.
3. **Minimal manifest** for Android Chrome install: name, icons, `start_url: "/"` (**never** embed the token — a revoked token frozen into an installed app; the cookie carries auth, and a revoked cookie means re-opening the invite link), `display: standalone`. No service worker required for install since Chrome 108.
4. Rewrite the now-false constraint comment at `couch.html:11-17` in the same commit.
5. **Judgment call, default NO:** the ~15-line network-first service worker caching only "/". It's the sole item that introduces a new failure mode (stale shell after a rebuild of the `include_str!` page). Skip unless reviewers actually report the shell failing to open offline.

**Acceptance test:** iOS add-to-home-screen shows the real icon (fails today: screenshot); screen stays awake through a 3-minute review session on iOS (fails today: dims); Android Chrome offers install over the ts.net URL and the installed app opens to the cookie-authenticated page; grep proves the stale comment is gone.

**Risks:** iOS EU/DMA standalone-web-app reports still conflict in 2026 sources — don't promise standalone behavior on EU-region devices without testing that one device.

**Does NOT solve:** clearing Safari data still wipes the outbox and drafts (unchanged by HTTPS — the sync-on-load flush is the defense; nothing should ever sit in the outbox for long); Gap 6 (origin-split localStorage between LAN/Tailscale/ts.net URLs) is not "fixed" — it dissolves for reviewers who use only the canonical ts.net link, which the runbook now designates as the one URL to send; drafts made on an old-origin URL stay there.

---

## Open questions (no data — do not treat as decided)

- Whether Tailscale Serve/Funnel sets a trustworthy `X-Forwarded-For` — required before any per-IP pre-auth limiter exists.
- tiny_http's handling of slow-header clients / undrained bodies (slowloris class) — thread-count assessment rests on the verified 1–8 pool, not library internals.
- Whether Gap 7's restart-crossing 409 reproduces live (Phase 4's fail-before test doubles as the verification).

## Deliberately NOT doing

- **Port-forwarding or any raw public exposure of 8737** — forbidden by design; the only public path is device-terminated TLS.
- **Cloudflare Tunnel as primary** — verified plaintext at Cloudflare's edge; Art. 9 audio does not transit a decrypting third party. Documented fallback only.
- **ngrok / TryCloudflare / Pinggy / LocalXpose / VPS+frp** — interstitials, ephemeral URLs, or manual TLS setup and a new vendor for nothing Funnel doesn't already do zero-config.
- **One-time or short-lived links** — actively harmful: preview bots burn them before the human clicks. Fragments solve the same threat correctly.
- **Fail2ban/lockout, IP allowlists, CAPTCHA** — 244-bit tokens make guessing mathematically dead; lockouts add a self-DoS lever; reviewers roam; CAPTCHAs break the "nothing installed" promise.
- **Background Sync, Web Push, Badging, audio precaching, IndexedDB outbox, workbox, Media Session** — unsupported on the actual devices, or large surface for niceties, or (audio caching) actively fights the lease model.
- **NSSM/WinSW service wrappers** — Session 0 cannot host a WebView2 GUI (verified).
- **tauri-plugin-autostart** — a rebuild plus new code surface for what one scheduled task does while also being the watchdog.
- **`NoAutoRebootWithLoggedOnUsers`** — with auto-login it converts security updates into indefinitely pending debt.
- **Persisting undo stacks across restarts** — snapshot surface for a rare convenience; desktop review is the correction path.
- **Uptime Kuma / self-hosted monitoring on the same box** — it would monitor itself; one healthchecks.io ping line or nothing.
- **New features of any kind** — this plan adds reach and reliability to the existing review loop; the review surface itself does not grow.
