# Cross-Platform Build & Bundle Status

Target: full Windows / macOS / Linux support. This file tracks what is wired,
what is verified, and what still needs platform-specific work before a release on
each OS. Nothing here has been validated by an actual `tauri build` on macOS or
Linux from this machine — those require the platform toolchains and run in the
`release.yml` CI matrix (ubuntu/windows/macos), which triggers on `v*` tags.

## ONNX Runtime (the `ort` crate, `load-dynamic`)

`ort` dlopen's the ONNX Runtime shared library at runtime via `ORT_DYLIB_PATH`.
`models::init_ort_dylib_path()` now searches (exe dir → parent → active models
dir → cwd) on **every** platform using the per-OS library name
(`models::ort_dylib_filename()`):

| OS      | Library filename          | Bundled today? |
|---------|---------------------------|----------------|
| Windows | `onnxruntime.dll`         | Yes — `models/onnxruntime.dll/` + `tauri.windows.conf.json` resources |
| macOS   | `libonnxruntime.dylib`    | **No** — must be provided (system, Homebrew, or bundled) |
| Linux   | `libonnxruntime.so`       | **No** — must be provided (system package or bundled) |

If the library is not found, `init_ort_dylib_path()` leaves `ORT_DYLIB_PATH`
unset and the system loader's default search applies. sherpa-onnx links its own
ONNX Runtime copy via `sherpa-onnx-sys`, which builds per-platform; only the
standalone `ort` path (Silero VAD) depends on the discovery above.

**TODO (mac/linux):** ship `libonnxruntime.{dylib,so}` as a bundle resource in
`tauri.macos.conf.json` / `tauri.linux.conf.json` (mirroring the Windows DLL
split), or document the system-package prerequisite.

## Bundle configuration

- `bundle.targets` is `"all"` — Tauri picks the native bundle per platform
  (Windows: msi + nsis; macOS: app + dmg; Linux: deb + appimage + rpm).
- `bundle.resources` in the base `tauri.conf.json` lists only cross-platform
  `.onnx` model files. The Windows-only ONNX Runtime DLLs live in
  `tauri.windows.conf.json`, which Tauri auto-merges for Windows builds (the
  platform-specific `resources` array overrides the base array). This keeps
  macOS/Linux builds from failing on missing `*.dll` resource paths while leaving
  the Windows bundle byte-for-byte what it was.

## Icons

`src-tauri/icons/` currently contains only `icon.ico` (Windows) and a small
`icon.png` (Linux). **Missing:**

- `icon.icns` — required for a proper macOS app icon.
- Multi-resolution PNGs (`32x32.png`, `128x128.png`, `128x128@2x.png`) that
  `tauri icon` normally generates for crisp rendering across platforms.

**TODO:** run `npm run tauri icon <source.png>` from a high-resolution source to
regenerate the full icon set (including `.icns`) before a macOS/Linux release.

## Signing / notarization (release item, needs secrets)

Not configured yet. A public release needs:

- **Windows:** Authenticode code-signing cert (`bundle.windows.certificateThumbprint` / signing via CI secret).
- **macOS:** Apple Developer ID cert + notarization (`APPLE_CERTIFICATE`, `APPLE_ID`, `APPLE_PASSWORD`, team id) — `tauri build` notarizes when these are present.
- **Linux:** no mandatory signing; optional GPG signing of the repo metadata.

These require certificates/credentials and are tracked separately from the
build-correctness work above.
