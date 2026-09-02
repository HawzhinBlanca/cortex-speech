# Release checklist - Cortex Kurdish Speech Processor

Use this before tagging a production release (`v*`). Windows Pro is the primary release path.

## Required toolchain

- Rust stable with `rustfmt` and `clippy`: `rustup component add rustfmt clippy`
- Node.js 22 and npm
- Python 3.12
- Playwright browser dependencies
- `cargo-deny` 0.19.8: `cargo install cargo-deny --version 0.19.8 --locked`
- `cargo-llvm-cov` 0.8.7: `cargo install cargo-llvm-cov --version 0.8.7 --locked`
- Branch-coverage-only Rust authority: `rustup toolchain install nightly-2026-07-11 --profile minimal --component llvm-tools-preview`.
  Normal builds remain on stable 1.95.0. The separate nightly identity is locked in
  `scripts/rust_coverage_toolchain.json`; a rolling `nightly` or branch-free report cannot certify.

## Clean release gate

- [ ] `npm ci`
- [ ] `npx playwright install chromium`
- [ ] `npm run typecheck`
- [ ] `npm test`
- [ ] `npm run setup:python-policies`
- [ ] `npm run test:python-policies`
- [ ] `python scripts/rust_quality_gate.py architecture`
- [ ] From the repository root, run `python scripts/verify_10.py --rust-coverage-prerequisite`.
      This is a separate no-retry phase because its explicit 7,200-second measurement budget would
      make the in-process verifier registry exceed six hours. A later aggregate accepts only its
      fresh immutable pointer for the exact SHA, checkout bytes, toolchain, command registry, branch
      evidence, thresholds, journal, and LLVM artifact hash; missing or copied JSON cannot certify.
- [ ] Run the selected `verify_10.py --profile ...` aggregate before the prerequisite expires. The
      aggregate copies the complete phase into its proof bundle and binds it in ProductAttestationV1.
- [ ] `npm run lint`
- [ ] `cargo fmt --manifest-path src-tauri/Cargo.toml --all --check`
- [ ] `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings`
- [ ] `cargo test --manifest-path src-tauri/Cargo.toml --all-targets --all-features`
- [ ] `npm run test:e2e`
- [ ] `npm audit --omit=dev`
- [ ] `cargo deny --manifest-path src-tauri/Cargo.toml check`

## Dataset trust gate

- [ ] Draft bundle export writes `manifest.json`, `validation_report.json`, `quality_report.json`, `model_manifest.json`, `dataset_card.md`, JSON, JSONL, CSV, and Parquet.
- [ ] Production bundle export blocks on validation errors and warnings above the configured threshold.
- [ ] Every installed/downloaded model has filename, size, SHA256, version, source, and install time recorded.
- [ ] Downloaded model archives have pinned SHA256 values before release. Empty hash constants are release blockers.

## Code signing (Windows Authenticode)

The release workflow (`.github/workflows/release.yml`) signs + timestamps the application EXE
**before** Tauri packages it, then signs the MSI and NSIS installers. It **fails closed** before
upload or publication when any signing secret is absent.
Unsigned Windows artifacts may be built locally for development, but a `v*` public release cannot
publish them. To enable releases:

1. Obtain an **Authenticode (EV or OV) code-signing certificate** as a password-protected `.pfx`.
2. Add five repository secrets: **`WINDOWS_CERT_BASE64`** (`base64 -w0 cert.pfx`),
   **`WINDOWS_CERT_PASSWORD`**, **`WINDOWS_CERT_THUMBPRINT`** (the exact 40-hex SHA-1 Windows
   certificate-store thumbprint), and **`WINDOWS_CERT_SHA256`** (the exact 64-hex SHA-256 of the
   certificate's DER `RawData`). The workflow proves both pinned identities on every installer; a
   merely valid certificate from the same PFX or store is insufficient. Add
   **`RULESET_AUDIT_TOKEN`** as a fine-grained, repository-scoped token with ruleset write visibility
   (repository Administration write permission). GitHub hides `bypass_actors` from weaker callers,
   so the release fails closed if this token is missing or cannot prove that the list is empty.
3. Tag a `v*` release. The workflow imports the cert and runs (verified locally with a self-signed cert):
   `signtool sign /f cert.pfx /p <pw> /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 <binary-or-installer>`
   then `signtool verify /pa /all /v /tw <binary-or-installer>` on each artifact and independently requires
   PowerShell's `Get-AuthenticodeSignature` status to be `Valid`.

- [ ] The installed application EXE and both installers are Authenticode-signed + timestamped;
      `signtool verify /pa /all /v /tw` succeeds for the EXE, MSI, and NSIS artifacts.
- [ ] The repository has one active tag ruleset for `refs/tags/v*`, with no exclusions or bypass
      actors, and deletion plus non-fast-forward updates blocked.
- [ ] `main` is the default branch and has an active, exclusion-free, no-bypass ruleset requiring
      signed commits, reviewed pull requests (at least one fresh approval after the last push),
      resolved review threads, strict **Provenance & License Gate** and **Windows Release Gate**
      status checks, and deletion/non-fast-forward blocks.
- [ ] The release tag is stable `vMAJOR.MINOR.PATCH`, equals both package versions, is an annotated
      GitHub-verified signature, points directly at the workflow SHA, and is reachable from
      `origin/main`.

## Integrity and provenance

The tag path creates a **private draft release candidate only**. The workflow's separate manual
promotion path is the sole stable-publication path: it downloads a named proof artifact from one
exact workflow run, reconstructs the complete proof/attestation/journal, requires the completed
`windows-product` verdict for the signed tag SHA, independently re-verifies Authenticode,
timestamping and GitHub/Sigstore provenance, and compares every required role/hash/size against the
downloaded bundle. It then re-reads the draft's server-side asset digests immediately before
promotion. Missing keys, updater, proof, external evidence, or asset-digest authority leaves the
candidate private. Manual publication is not proof and is forbidden by policy.

The eventual public channel is Windows 11 x64 only; macOS and Linux are not claimed release
platforms. The exact inner directory tree gets deterministic `SHA256SUMS-windows-11-x64` coverage,
then is packaged as `cortex-speech-windows-11-x64.zip`; a second checksum manifest and a separate
Sigstore bundle cover the outer release assets without relying on GitHub Release's flat filename
storage. The workflow creates GitHub/Sigstore build provenance for every checksum-listed inner
subject and the outer archive using SHA-pinned `actions/attest`. A separate Windows publisher
downloads the package, verifies the outer and inner inventories, rechecks Authenticode/timestamps and
provenance against the exact workflow SHA/tag/ref/repository, and only then stages the private draft.

- [ ] Verify the downloaded archive and Sigstore bundle against `SHA256SUMS-release`, then every
      extracted file against `SHA256SUMS-windows-11-x64`.
- [ ] Verify GitHub provenance with `gh attestation verify <artifact> --repo HawzhinBlanca/cortex-speech`.
- [ ] Confirm the attestation names the expected tag commit and release workflow run.
- [ ] Invoke the promotion workflow with the exact draft tag and proof workflow-run ID. Confirm its
      `--require-certifying-proof --profile windows-product` consumer binds the EXE, MSI, NSIS,
      updater, checksum, SBOM and provenance roles before the draft becomes public.

## Security gate

- [ ] Tauri asset protocol remains scoped to app data only.
- [ ] No `shell:default` permission unless a specific feature requires it.
- [ ] Updater artifacts remain disabled until a real signed pubkey, endpoint, private-key handling,
      opt-in network UX, signature-rejection drill, interrupted-update drill, and rollback proof all
      exist. A release with the updater disabled cannot receive the `windows-product` verdict. When
      enabled, the detached signature must verify with the exact public key compiled into
      `tauri.conf.json` through the pinned `minisign-verify` helper; filename pairing or a signature
      file hash alone is never accepted.
- [ ] Gemini/cloud LLM use is explicitly opted in and visibly marked as sending text to a provider.
- [ ] No API keys, settings files, logs, local media, temp DBs, reports, or private paths are included in release artifacts.

## Manual expert workflow

- [ ] Clean Windows install.
- [ ] App starts without WSL configured.
- [ ] Import representative long-form Kurdish media.
- [ ] Review, annotate, validate, verify, and export a draft bundle.
- [ ] Resolve blocking validation issues and export a production bundle.
- [ ] Offline mode works after required models are installed.
