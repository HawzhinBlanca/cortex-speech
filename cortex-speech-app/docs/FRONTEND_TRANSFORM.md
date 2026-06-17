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

## Highest-value next (best done with the live preview)

These are deliberately deferred — they're structural layout changes that need visual
iteration (the audit's biggest item):

- **Activity rail + `viewMode` IA** — turn the ~13-button toolbar into navigable
  workspaces (Library / Review / Insights / Settings); the dead `ViewMode` union in
  `uiStore.ts` is already there to wire.
- Route the remaining 5 panels (Settings, Validation, Speaker, Merge, WSL) through `Modal`.
- Adopt `lucide-svelte` to replace the ~32 inline SVGs + emoji for one icon language.
- Multi-select segment **action bar**; rebuild **ReviewInbox** on the design system.
