# Frontend Transformation — Cortex Speech Studio

The Svelte 5 UI was lifted from a dense, "neon-glass" prototype to a calm, high-end,
keyboard-first desktop app (Linear / Raycast direction). Everything below is
token-driven, **builds clean** (`vite build`), and is **type-clean** (`svelte-check && tsc`).

## What shipped (9 commits)

| Area | Change |
|------|--------|
| **Design system** | Full CSS-variable token layer in `app.css` (surfaces, text, accent, semantic, elevation, radii, motion) for a polished **dark theme + a real light theme**; refined component recipes (`.btn / .card / .input / .badge`) consumed app-wide; `tailwind.config` semantic colors + fonts. Trades neon glow/gradients/translate-hover for restrained, intentional hierarchy. |
| **Typography** | Inter (UI) · Vazirmatn (Kurdish/RTL) · JetBrains Mono (metrics), loaded **non-render-blocking + offline-resilient**. |
| **Theming engine** | The Light/System setting was **dead** — now applies `.dark`/`.light` to `<html>` and follows the OS in `system` mode. |
| **Command palette** | **Cmd-K** fuzzy palette over the shortcut registry, full keyboard nav, hosted in the shared Modal. |
| **Modal system** | One accessible dialog shell (`Modal.svelte` + `focusTrap` action): backdrop, focus trap, restore-focus, Escape, `role=dialog`/`aria-modal`, scale+fade transitions. ConfirmDialog + KeyboardShortcuts routed through it. |
| **Keyboard safety** | Bare shortcuts (Delete, j/k, …) no longer fire while typing Kurdish / composing (IME) — **was silently destroying segments**. |
| **ConfirmDialog safety** | A stray Enter no longer fires the **destructive** action (safe Cancel autofocused). |
| **Motion** | Animated toasts (fly / fade / flip), modal enter/exit transitions. |
| **Feedback & polish** | Real progress bar (replaces a frozen 30% pulse) with a true indeterminate animation; **polished, actionable empty states** (no-data vs no-results, with Import/Open CTAs); localized the hardcoded English (EN + Sorani); **panel widths persisted** across launches. |

## Preview it

A dev-only Tauri mock (`main.ts`) + non-blocking fonts make the whole UI render in a
plain browser — no Rust backend needed:

```
npm run dev   →   http://localhost:1420
```

Toggle the theme in Settings, press **Cmd-K**, resize panels and relaunch.

## Overnight pass — theming everywhere + states + a11y (3 more commits)

- **Real light/dark on every surface**: `cortex-*` is now driven by RGB-channel CSS
  vars (`--cortex-NNN-rgb`) with an inverted light set — so *every* `bg/text/border-cortex`
  class switches with the theme. **Dark values are byte-identical** (zero regression).
  All `text-gray-*` migrated to semantic tokens; shell chrome → `bg-app` / `.glass` /
  `border-line`; removed the broken `class:dark/light` (theming lives on `<html>`).
- **`EmptyState.svelte`** — one reusable, theme-aware empty/no-results/error/mic state
  with optional CTA snippet; wired into the center placeholder.
- **Focus-trap on every dialog** — Settings, Validation, WSL now match the Modal-routed
  ones (focus in / cycle / restore).

## Verified live in the preview (not just built — measured)

Using the running dev server I introspected computed styles directly and fixed
what the numbers showed:

1. **Contrast — both themes pass AA.** Walked every rendered text node and computed
   its ratio against the effective background. Fixed light `--text-subtle` (2.9→4.7:1)
   and dark `--cortex-600` (2.86→>3:1). Audit: **0 nodes below 3:1 in light or dark**.
2. **Status colors — fixed in light, byte-identical in dark.** red/amber/emerald/cyan/
   indigo/orange/blue are now theme-aware channel vars. Measured bare text + opacity
   chips: every pattern went from 1.4–2.6:1 (failing) to 4.7–16.6:1. Dark spot-checked
   identical (`text-red-400` == `rgb(248,113,113)`). No markup touched.
3. **RTL** (Kurdish, default): `dir=rtl` confirmed; the activity rail mirrors to the
   right with logical borders; physical spacing swept to `ms-/me-/ps-/pe-`.
4. **Interactions**: activity rail switches workspaces (`aria-current` toggles), ⌘K
   palette opens/filters/closes on Esc, **zero console errors**.
5. **Buttons** — found 68 variant buttons (`.btn-secondary` etc.) used WITHOUT the
   base `.btn` class: they rendered `display:block`, 0 padding, no focus ring, no
   active/disabled states (a header button measured 14×16px). Restored the base
   everywhere → proper padding, 31–33px height, focus rings, states. Sub-24px
   interactive targets: 3 → 0. Likely a big part of the old "basic" feel.
6. **a11y focus rings** — the global `:focus-visible{outline:none}` left custom
   elements (incl. the new rail) with no keyboard indicator; added a scoped default
   ring (no double-rings, no container outlines).
7. **`.btn-danger`** now themes (was hardcoded light-pink, failed on light); compact
   `.btn-sm` + `flex-wrap` so the sidebar's Kurdish filter chips wrap, not overflow.

Final settled audit (transitions waited out): **0 text nodes < 3:1 in light or dark**.
Open: a benign phantom horizontal scroll (scrollWidth 1390 vs 1280 but nothing
actually past the viewport — no visible cutoff); left sidebar content is tight at its
default 287px (resizable). Both worth a glance but neither cuts off content.

## Information architecture — DONE (the audit's #1 structural item)

A persistent **activity rail** (Curate / Insights / Review + Settings) bound to a
`viewMode` store now turns the toolbar into a navigable, mode-aware product:
- **Curate** (default) — the transcript workspace, byte-identical (zero regression).
- **Insights** — the StatsDashboard promoted to a first-class in-`main` workspace.
- **Review / Settings** — open from the rail.

## Bigger items still open (need your eyes / iteration)

- Adopt `lucide-svelte` to replace the ~32 inline SVGs + emoji for one icon language.
- Multi-select segment **action bar**; rebuild **ReviewInbox** on the design system.
- Promote more destinations into `viewMode` workspaces if you want the rail to own
  more of the modal long-tail.
