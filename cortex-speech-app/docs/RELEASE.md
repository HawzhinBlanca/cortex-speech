# Release checklist - Cortex Kurdish Speech Processor

Use this before tagging a production release (`v*`). Windows Pro is the primary release path.

## Required toolchain

- Rust stable with `rustfmt` and `clippy`: `rustup component add rustfmt clippy`
- Node.js 22 and npm
- Python 3.12
- Playwright browser dependencies
- `cargo-deny` 0.19.8: `cargo install cargo-deny --version 0.19.8 --locked`

## Clean release gate

- [ ] `npm ci`
- [ ] `npx playwright install chromium`
- [ ] `npm run typecheck`
- [ ] `npm test`
- [ ] `npm run test:python-policies`
- [ ] `npm run lint`
- [ ] `cargo fmt --manifest-path src-tauri/Cargo.toml --all --check`
- [ ] `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings`
- [ ] `cargo test --manifest-path src-tauri/Cargo.toml`
- [ ] `npm run test:e2e`
- [ ] `npm audit --omit=dev`
- [ ] `cargo deny --manifest-path src-tauri/Cargo.toml check`

## Dataset trust gate

- [ ] Draft bundle export writes `manifest.json`, `validation_report.json`, `quality_report.json`, `model_manifest.json`, `dataset_card.md`, JSON, JSONL, CSV, and Parquet.
- [ ] Production bundle export blocks on validation errors and warnings above the configured threshold.
- [ ] Every installed/downloaded model has filename, size, SHA256, version, source, and install time recorded.
- [ ] Downloaded model archives have pinned SHA256 values before release. Empty hash constants are release blockers.

## Security gate

- [ ] Tauri asset protocol remains scoped to app data only.
- [ ] No `shell:default` permission unless a specific feature requires it.
- [ ] Updater is disabled for local/offline builds or configured with a real signed pubkey and endpoint.
- [ ] Gemini/cloud LLM use is explicitly opted in and visibly marked as sending text to a provider.
- [ ] No API keys, settings files, logs, local media, temp DBs, reports, or private paths are included in release artifacts.

## Manual expert workflow

- [ ] Clean Windows install.
- [ ] App starts without WSL configured.
- [ ] Import representative long-form Kurdish media.
- [ ] Review, annotate, validate, verify, and export a draft bundle.
- [ ] Resolve blocking validation issues and export a production bundle.
- [ ] Offline mode works after required models are installed.
