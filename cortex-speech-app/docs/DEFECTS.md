# Cortex — Smoke-Test Defect Log

Living log from the continuous A→Z smoke test (`/loop`). Each entry: what broke,
root cause, fix, how it was verified.

Legend: ✅ fixed & verified · 🔄 in progress · 📋 noted / deferred

---

## Iteration 1 — populated curate UI (dev preview, sample data)

### ✅ D1 — Selecting a segment surfaced a raw `TypeError: Cannot read properties of null (reading 'id')`
- **Found:** With real segments loaded, clicking any segment showed a raw JS
  `TypeError` (+ "Retry") in the audio area of the center panel instead of the
  player.
- **Root cause:** `AudioPlayer.svelte` `resolveAudioUrl()` did
  `await getMediaAssetUrl(grant.id)` without guarding `grant`. When
  `registerMediaAsset()` returns null/denied — missing file, permission denied,
  or no backend — `grant.id` throws. The `catch` then showed `String(e)`, i.e.
  the raw `TypeError`, to the user.
- **Why it matters in production (not just the mock):** any moved/deleted audio
  file or denied media permission would crash into the same raw error.
- **Fix:** guard the grant (`grant?.id ? … : null`) and the resolved path; throw
  a controlled error; the `catch` now logs the technical detail to the console
  and shows the existing clean message `"Failed to load audio file"`.
- **Verified:** live preview — selecting a segment renders the full editor (2
  transcript textareas, interactive editor, word-timestamps panel); the player
  shows the clean "Failed to load audio file" + Retry; `rawErrorGone: true`.
  Build + typecheck green.

### 📋 D2 — `ErrorBoundary` listens on `window` globally (architectural smell)
- **Observation:** `ErrorBoundary.svelte` catches errors via a global
  `window.addEventListener('error', …, true)` rather than Svelte 5's
  `<svelte:boundary>`. Every mounted boundary therefore reacts to *every* global
  error, so one async failure can blank multiple regions at once. Did not cause
  D1 (that was caught locally), but worth migrating to true component boundaries.
- **Status:** noted; not yet changed (larger refactor, no current user-visible
  break).

---

## Tooling

- **Dev preview now serves sample Sorani data** (`src/main.ts` mock): 8 segments
  with varied confidence / speakers / verified state, stats, waveform peaks — so
  the populated curate UI (segment list, editor, badges, stats, RTL) can be
  exercised without the Rust backend. This is what surfaced D1.
- **Real end-to-end** (Tauri app + actual ASR) needs models downloaded into
  `models/` (currently empty); backend `cargo check` passes, so the app is
  buildable. Real audio fixtures exist on disk (`Desktop/CORTEX/podcast.wav`,
  `Desktop/HalwestVS/*.wav`).
