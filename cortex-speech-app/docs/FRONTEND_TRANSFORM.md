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

## Verify visually when you're back (`npm run dev` → localhost:1420)

1. **Light mode** (Settings → theme → Light): the *status* colors (red/amber/emerald/
   indigo badges for confidence/speaker) are still raw Tailwind shades — they read fine
   in dark but want a token pass for ideal light-mode contrast. Everything else themes.
2. **RTL** (Kurdish): the header/search/badges use a mix of physical + logical spacing;
   a `ms-/me-/ps-/pe-/start-/end-` sweep would make Sorani pixel-perfect.

## Bigger items that genuinely need your eyes (deferred on purpose)

- **Activity rail + `viewMode` IA** — turn the ~13-button toolbar into navigable
  workspaces (Library / Review / Insights / Settings); the dead `ViewMode` union in
  `uiStore.ts` is ready to wire. (The audit's #1 structural item.)
- Adopt `lucide-svelte` to replace the ~32 inline SVGs + emoji for one icon language.
- Multi-select segment **action bar**; rebuild **ReviewInbox** on the design system.
