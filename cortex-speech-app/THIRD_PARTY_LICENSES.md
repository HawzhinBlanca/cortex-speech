# Third-Party Licenses

Cortex Speech is licensed under Apache-2.0. It builds on the open-source
components below. The license for each **direct** dependency was read from the
resolved crate metadata on disk (`cargo metadata --format-version 1`, license
field) and from `package.json`; it is not hand-asserted.

This file lists direct dependencies only. The full transitive license set
(including every indirectly-pulled crate and its license text) should be
generated in CI with a dedicated tool — e.g. `cargo about generate` or
`cargo deny check licenses` — which is the canonical, exhaustive source. This
curated summary is for quick human review of what the app directly links.

## Bundled runtime components (models / native libs)

See [`NOTICE`](../NOTICE) at the repository root for Meta Omnilingual ASR
(Apache-2.0), sherpa-onnx (Apache-2.0), Silero VAD (Apache-2.0), and the AsoSoft
Library normalization rules (MIT, as identified in source). The AsoSoft-600
evaluation **corpus** is CC BY-SA 4.0 and is governed separately by
[`DATA_GOVERNANCE.md`](../DATA_GOVERNANCE.md).

## Rust crates — runtime dependencies

| Crate | Version | License (SPDX) |
|-------|---------|----------------|
| arrow-array | 58.3.0 | Apache-2.0 AND MIT |
| arrow-schema | 58.3.0 | Apache-2.0 |
| blake3 | 1.8.5 | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception |
| bzip2 | 0.5.2 | MIT OR Apache-2.0 |
| chrono | 0.4.44 | MIT OR Apache-2.0 |
| csv | 1.4.0 | Unlicense OR MIT |
| flacenc | 0.4.0 | Apache-2.0 |
| flate2 | 1.1.9 | MIT OR Apache-2.0 |
| hound | 3.5.1 | Apache-2.0 |
| libc | 0.2.186 | MIT OR Apache-2.0 |
| lru | 0.16.4 | MIT |
| ndarray | 0.17.2 | MIT OR Apache-2.0 |
| ort | 2.0.0-rc.12 | MIT OR Apache-2.0 |
| parquet | 58.3.0 | Apache-2.0 |
| rayon | 1.12.0 | MIT OR Apache-2.0 |
| regex | 1.12.3 | MIT OR Apache-2.0 |
| rusqlite | 0.31.0 | MIT |
| rustfft | 6.4.1 | MIT OR Apache-2.0 |
| serde | 1.0.228 | MIT OR Apache-2.0 |
| serde_json | 1.0.150 | MIT OR Apache-2.0 |
| sha2 | 0.10.9 | MIT OR Apache-2.0 |
| sherpa-onnx | 1.13.2 | Apache-2.0 |
| sherpa-onnx-sys | 1.13.2 | Apache-2.0 |
| symphonia | 0.5.5 | **MPL-2.0** (weak copyleft — see note) |
| sysinfo | 0.33.1 | MIT |
| tar | 0.4.46 | MIT OR Apache-2.0 |
| tauri | 2.11.2 | Apache-2.0 OR MIT |
| tauri-plugin-dialog | 2.7.1 | Apache-2.0 OR MIT |
| thiserror | 2.0.18 | MIT OR Apache-2.0 |
| tokio | 1.52.3 | MIT |
| tracing | 0.1.44 | MIT |
| tracing-subscriber | 0.3.23 | MIT |
| unicode-normalization | 0.1.25 | MIT OR Apache-2.0 |
| ureq | 3.3.0 | MIT OR Apache-2.0 |
| uuid | 1.23.1 | Apache-2.0 OR MIT |

## Rust crates — build / dev dependencies

| Crate | Version | Kind | License (SPDX) |
|-------|---------|------|----------------|
| tauri-build | 2.6.2 | build | Apache-2.0 OR MIT |
| assert_cmd | 2.2.2 | dev | MIT OR Apache-2.0 |
| criterion | 0.5.1 | dev | Apache-2.0 OR MIT |
| proptest | 1.11.0 | dev | MIT OR Apache-2.0 |
| tempfile | 3.27.0 | dev | MIT OR Apache-2.0 |

## npm — runtime dependencies

| Package | Version | License (SPDX) |
|---------|---------|----------------|
| @tauri-apps/api | ^2.0.0 | Apache-2.0 OR MIT (Tauri project) |
| @tauri-apps/plugin-dialog | ^2.0.0 | Apache-2.0 OR MIT (Tauri project) |

(The npm devDependencies — Vite, Svelte, ESLint, Playwright, Vitest,
TypeScript, Tailwind, etc. — are build-time only and are not redistributed in
the packaged application.)

## Compliance notes

- **symphonia (MPL-2.0)** is the only weak-copyleft runtime dependency. MPL-2.0
  is file-level copyleft: it is compatible with shipping inside an Apache-2.0
  application as an unmodified dependency, but if any symphonia source file is
  modified, that file's source must be made available. We use it unmodified.
- All other runtime crates are permissive (MIT / Apache-2.0 / Unlicense /
  CC0-1.0), all compatible with the Apache-2.0 distribution.
- Licenses above were read from on-disk crate metadata at the resolved versions;
  re-generate after any dependency bump. The authoritative, exhaustive list
  (transitive + license texts) is produced by `cargo about` / `cargo deny` in CI.
