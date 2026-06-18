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

## Iteration 2 — populated curate UI (filters, badges, contrast)

### ✅ Filters verified
- All / Verified / Pending filters return correct counts (8 / 4 / 4 / back to 8)
  against the sample dataset. No defect.

### ✅ D3 — Status badges unreadable in light mode
- **Found:** `.badge-verified` rendered light-emerald `#6ee7b7` on the light
  success-soft background = **1.52:1** (invisible). Same for pending/danger/info —
  the badge recipes hardcoded dark-theme text colors that don't invert.
- **Fix:** badge text → `var(--success|warning|danger|info)` (theme-aware) +
  `color-mix` borders. Then darkened the **light** semantic tokens so 11px-bold
  badge text clears strict AA on the soft tints: success `#059669→#047857`,
  warning `#d97706→#92400e`, danger `#e11d48→#be123c`, info `#2563eb→#1d4ed8`
  (dark tokens untouched).
- **Verified (transitions disabled):** light badges 1.52→**4.54** (verified),
  **4.5+** pending, **5.01** danger, **5.45** info; dark 9.16. Build + typecheck green.

### 🛈 D4 — "dim filename / timestamp" fails were measurement artifacts
- My theme-flip audits toggled `html.className`, but the app's settingsStore
  re-applies the real theme and elements have `transition: color`, so values read
  mid-transition (e.g. filename showed 1.88). **Disabling CSS transitions before
  measuring** gives the true values: filename 12.73 (dark) / 9.65 (light) — fine.
- **Methodology fix:** all theme-flip contrast audits now inject
  `*{transition:none!important}` first. This retroactively explains several
  earlier phantom "fails".

### 🔄 D5 — Small secondary text just under strict AA (4.5) — NEXT
- With transitions disabled, content/secondary text sits at **4.0–4.4:1**:
  transcript previews (`text-cortex-500`, ~4.14 dark), timestamps + filter chips
  (`text-cortex-400`, ~4.43 light), version label. Passes AA-large (3.0) but
  misses strict AA-normal. Fix = nudge the cortex mid-tiers (dark-500 brighter,
  light-400 darker) and re-audit. Deferred to next iteration for a clean, fully
  re-verified pass (avoid half-tuning the scale).

## Iteration 3 — strict AA-4.5 contrast pass (transitions-disabled audit)

### ✅ D5 — cortex mid-tier text just under strict AA
- **Found (clean audit):** light `cortex-400` 4.43 (timestamps/filter chips, ×10);
  dark `cortex-500` 4.14 (transcript previews, ×~11); dark `--text-subtle` 3.83.
- **Fix:** dark `--cortex-500` 62,121,163→76,138,180; light `--cortex-400`
  100,116,139→90,106,130; dark `--text-subtle` #61707f→#6d7c8c.
- **Verified:** light **0 fails at 4.5**; dark below-3.0 = **0**.

### ✅ D6 — HistoryPanel + Waveform hardcoded `slate-*` (not theme-aware)
- **Found:** `text-slate-400`=2.39/2.56 (light), `slate-600`=2.57 (dark). These two
  components used Tailwind `slate-*` (never made theme-aware), so they didn't
  invert and failed contrast.
- **Fix:** migrated `-slate-` → `-cortex-` (theme-aware) across both files; they now
  theme correctly and inherit the tuned contrast.

### ✅ D7 — Editor tab toggle: white text invisible in light
- **Found:** active tab `bg-cortex-700 text-white` = **1.48** in light (bg-cortex-700
  is light gray in light mode; white text on it is invisible). App.svelte ×3.
- **Fix:** `text-white` → `text-default` (themes both ways).

### 🛈 Accepted — dark `text-cortex-600` hint tier (3.1–4.5, AA-large)
- Remaining sub-4.5 dark elements (×9) are all `text-cortex-600`: the ⌘/hotkey
  hint tier + micro-labels. All ≥3.0 (AA-large). Kept as deliberate de-emphasis
  (hints are also surfaced via ⌘K + the shortcuts modal); pushing to 4.5 would
  collapse the cortex scale and brighten dark past the byte-identical baseline.
- **Net:** both themes AA-large clean everywhere; light fully strict-AA (4.5).

## Tooling

- **Dev preview now serves sample Sorani data** (`src/main.ts` mock): 8 segments
  with varied confidence / speakers / verified state, stats, waveform peaks — so
  the populated curate UI (segment list, editor, badges, stats, RTL) can be
  exercised without the Rust backend. This is what surfaced D1.
- **Real end-to-end** (Tauri app + actual ASR) needs models downloaded into
  `models/` (currently empty); backend `cargo check` passes, so the app is
  buildable. Real audio fixtures exist on disk (`Desktop/CORTEX/podcast.wav`,
  `Desktop/HalwestVS/*.wav`).
