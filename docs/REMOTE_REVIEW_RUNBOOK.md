# Remote Review — operating it

How to hand the phone-review page to someone who is not on your Wi-Fi, and what to check when it
misbehaves. Everything here was measured on the owner's rig on 2026-07-28; where a number appears it
came from a real run, not an estimate.

The design rationale lives in `REMOTE_REVIEW_PLAN.md`. This file is the operational half.

## What it is

The desktop app runs a small HTTP server (`src-tauri/src/couch.rs`) on port **8737**, bound to
`0.0.0.0`, serving one self-contained page (`src-tauri/assets/couch.html`). Each named reviewer gets
their **own** link carrying their **own** random token. The token is the identity: every decision is
stored with `speech_segments.reviewed_by` set to that reviewer's name.

It is **off by default** and **per-session** — tokens are generated at start, never persisted, and
die when you stop the server or close the app.

## Starting a session

1. Open the app → **Settings** → **📱 Couch Review**.
2. Type the reviewer names, comma-separated (max 8). One name = one link = one identity.
3. Press **Start**.
4. Each reviewer block shows two URLs:
   - **On Wi-Fi** — a `192.168.x.x` address. Only works on your local network.
   - **From anywhere (Tailscale)** — a `100.x.y.z` address. Works over mobile data, from another
     country, anywhere the tailnet reaches. Shown only when a tailnet is up.
5. Send each person **their own** link. Handing two people the same link merges them into one
   identity in the data, which silently destroys the attribution the whole design rests on.

Reviewers need no app and no account — just the link in a browser.

## Requirements for the "from anywhere" link

- **Tailscale installed and signed in on both devices**, same tailnet. Verify with
  `tailscale status`; both must appear without an `offline` marker.
- **This PC must stay awake.** On this rig, sleep and hibernate on AC are both set to *never*
  (`powercfg /query <scheme> SUB_SLEEP STANDBYIDLE` → `Current AC Power Setting Index: 0x00000000`).
- **The app must stay running.** Closing it stops the server and revokes every token.
- **The Windows Firewall must allow the app inbound.** The Tailscale adapter is categorised
  **Private**, and the `cortex-speech-app.exe` inbound rule allows Private and Public.

No port forwarding, no router changes, no public exposure. Tailscale is WireGuard: the traffic is
end-to-end encrypted between your devices, and nothing is reachable from the open internet.

## Measured behaviour (2026-07-28, this rig)

Against the live server, on both `127.0.0.1` and the Tailscale address:

| Request | Time | Size |
| --- | --- | --- |
| `GET /` (the whole page) | 6.4 ms | 28,968 B |
| `GET /api/queue` | 1.8 ms | 9,733 B |
| `GET /api/audio/<id>` | 82–111 ms | 330–460 KB |

Clip audio is cut from the source file per request. That does re-decode the source each time, and it
was measured rather than assumed: at ~85 ms it is not worth caching, and the page **prefetches the
next clip** while the reviewer works on the current one, so the cost is hidden anyway.

`tailscale ping` to the phone answered **via DERP(fra) in 184 ms** — a relayed, not direct,
connection. Relayed is the worst case and still comfortable, because of the prefetch.

## Two failures that were real, and what they look like now

Both were found by driving the live server, not by the test suite, and both are fixed with
regression tests (`1758d1e`, `475a1ca`).

- **Stop then Start used to fail** with `os error 10048`, and remote review stayed dead until the app
  was restarted. tiny_http's accept thread holds the listening socket and is woken on drop by a
  connection to its own listening address — which is `0.0.0.0`, and connecting to `0.0.0.0` fails on
  Windows. The socket stayed bound (measured: still LISTENing 120 s after stop returned) and kept
  accepting TCP with nobody left to answer, so an old link **hung** instead of failing. `stop()` now
  performs the wake itself and confirms the port is free by re-binding.
- **Clips had an infinite duration.** Every clip was sent `Transfer-Encoding: chunked` with no
  `Content-Length` (tiny_http chunks anything ≥ 32 KB; clips are 300–500 KB), so the browser reported
  `duration = Infinity`. The progress bar showed no total time and tap-to-seek multiplied by Infinity
  and did nothing. Every reply now declares its length.

## Troubleshooting

**"Another instance is already running" when opening the app.** A stale lock file from a process that
died without cleaning up. The app now retries and clears it automatically; if the message still
appears, no other Cortex window is open, and it names the lock file — delete it and reopen.

**The link opens but the page says everything is reviewed.** The queue is genuinely empty, or every
pending clip is currently **leased** to another reviewer. Leases last 15 minutes and are renewed by a
heartbeat while a clip is open, so a reviewer who closes their browser releases their clips within
15 minutes rather than stranding them.

**A reviewer's save says another reviewer is working on this clip (409).** Correct behaviour: someone
else holds the lease, or the clip was already decided. Their text is not lost — the page tells them
before they lose it, not after.

**A save appears to do nothing on a bad connection.** It went to the page's **outbox** and is replayed
on the next load. A request the server *refused* is dropped rather than retried forever, so one bad
decision cannot wedge everything behind it.

**Starting Couch Review fails to bind port 8737.** Something else holds the port. Note that
`cargo test` includes a test that binds the real 8737, so the Rust suite and a live review session
cannot run at the same time — that is deliberate, because a test that skipped itself would restore
exactly the blind spot it exists to remove.

## What is NOT measured yet

- **Spot-check volume is small, and it is capped by how much you have verified yourself.** A clip can
  serve as an answer key only if you verified it on the desktop (`reviewed_by IS NULL`) and the raw
  ASR draft actually differs from your answer — a draft that was already right cannot tell a reviewer
  who listened from one who tapped accept. At the time of writing the live library yields **15**
  usable answer keys, so early scores rest on a handful of checks. `SpotCheckScore.checks` reports
  the real count; do not read a verdict into two or three.

  This was worse until `3d1c418`: the predicate required `is_gold = 1`, which **nothing in the app
  ever sets**, so the mechanism could never fire at all. If you are reading an older build, spot
  checks are inert regardless of how much you review.

  A phone reviewer's own correction is never used as an answer key — that would grade the next
  reviewer against a peer's guess.
- **Seven Sorani strings on this surface have not had a native read** (`heldByOthers`,
  `settings.couchReviewers*`, `settings.couchSpotChecks*`, `settings.couchAgreement*`,
  `settings.couchRevoke`, `settings.couchThroughput`).
