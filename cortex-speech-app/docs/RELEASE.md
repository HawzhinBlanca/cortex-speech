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

## Code signing (Windows Authenticode)

The release workflow (`.github/workflows/release.yml`) signs + timestamps the MSI and NSIS
installers and **fails closed** before upload or publication when either signing secret is absent.
Unsigned Windows artifacts may be built locally for development, but a `v*` public release cannot
publish them. To enable releases:

1. Obtain an **Authenticode (EV or OV) code-signing certificate** as a password-protected `.pfx`.
2. Add two repository secrets: **`WINDOWS_CERT_BASE64`** (`base64 -w0 cert.pfx`) and
   **`WINDOWS_CERT_PASSWORD`**.
3. Tag a `v*` release. The workflow imports the cert and runs (verified locally with a self-signed cert):
   `signtool sign /f cert.pfx /p <pw> /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 <installer>`
   then `signtool verify /pa <installer>` on each artifact.

- [ ] Release installers are Authenticode-signed + timestamped; `signtool verify /pa` succeeds.

## Integrity and provenance

The public channel currently publishes only the supported, signed Windows bundle; macOS and Linux
remain build smoke checks until `CROSS_PLATFORM.md` is complete. The Windows bundle gets a deterministic
`SHA256SUMS-windows-latest` file before upload. The release workflow also creates a GitHub/Sigstore
build-provenance attestation for every bundled file using a SHA-pinned `actions/attest` action. Both
steps run before GitHub artifact upload. A separate publisher waits for every platform build, downloads
only the attested Windows bundle, and updates the release once; repository write permission is not
available to build or test steps.

- [ ] Verify a downloaded file against its platform's `SHA256SUMS-<platform>` entry.
- [ ] Verify GitHub provenance with `gh attestation verify <artifact> --repo HawzhinBlanca/cortex-speech`.
- [ ] Confirm the attestation names the expected tag commit and release workflow run.

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
