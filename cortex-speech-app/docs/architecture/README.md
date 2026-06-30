# Cortex Speech — Architecture pack

A complete, downloadable representation of the app's end-to-end architecture, built from a verified read of the real codebase (2026-06-30, `main @ 4787d81`).

## Files

| File | What it is | Best for |
|---|---|---|
| **Cortex-Speech-Architecture.pdf** | The full overview — diagram + write-up, print-formatted | reading, sharing, printing |
| **Cortex-Speech-Architecture.html** | Same overview, self-contained web page | opening in any browser, re-printing to PDF |
| **cortex-e2e-architecture.svg** / **.png** | The diagram (light theme), vector + high-res raster (2×) | embedding · crisp zoom · quick paste |
| **cortex-e2e-architecture-dark.svg** / **.png** | The diagram (dark theme), matches the app's dark UI | dark docs/slides |
| **ARCHITECTURE.md** | The full write-up in Markdown (editable source) | version control, editing |
| **README.md** | This index | — |

## The diagram at a glance

Top-to-bottom data flow across 9 layers:

`Capture/UI → IPC → Pipeline (decode · VAD · ASR · normalize · align · persist) → ASR engines → Jury (T0/T1/T2) → Human review → Storage → Validate & export → Cloud (opt-in)`

- **Solid violet** = the **OmniASR-7B Champion** primary path (the default engine).
- **Dashed** = cloud services — opt-in and off by default. The app is 100% offline out of the box.

## Regenerate the PNG/PDF

From this folder (Windows, Edge installed):

```powershell
$edge = "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
& $edge --headless=new --disable-gpu --force-device-scale-factor=2 --window-size=1300,1625 --screenshot="cortex-e2e-architecture.png" "file://$PWD/cortex-e2e-architecture.svg"
& $edge --headless=new --disable-gpu --no-pdf-header-footer --print-to-pdf="Cortex-Speech-Architecture.pdf" "file://$PWD/Cortex-Speech-Architecture.html"
```
