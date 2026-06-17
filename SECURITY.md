# Security Policy

Cortex Speech is an **offline-first** desktop tool that processes speech for an
at-risk, surveilled minority language (Central Kurdish / Sorani). Security and
privacy are therefore first-class concerns, not afterthoughts.

## Supported versions

| Version | Supported |
|---------|-----------|
| 2.1.x   | ✅ Security fixes |
| < 2.1   | ❌ Please upgrade |

## Reporting a vulnerability

**Do not open a public issue for security vulnerabilities.**

Report privately via **GitHub Security Advisories** ("Report a vulnerability"
under the repository's *Security* tab). Please include:

- affected version / commit,
- a reproduction or proof-of-concept,
- impact (what an attacker gains), and
- any suggested remediation.

We aim to acknowledge within **72 hours** and to ship a fix or mitigation for
confirmed high-severity issues within **30 days**. Coordinated disclosure is
appreciated; we will credit reporters who wish to be named.

## What is in scope

- The Tauri/Rust backend and IPC command surface (`src-tauri/`).
- The Svelte frontend and its handling of untrusted dataset content.
- Model download / integrity verification and the audio decode path.
- The data-governance and provenance tooling (`scripts/verify_10.py`,
  `docs/provenance_ledger.json`).

## Privacy & data-handling posture

- **Offline by default.** Transcription, the Disagreement-Refinery jury, and all
  calibration run **100% on-device** with no network egress in the default
  configuration. Audio and transcripts do not leave the machine.
- **Cloud is opt-in.** Any cloud model (e.g. Gemini) is **off by default** and
  must be explicitly enabled. Enabling it transfers audio to a third party;
  published headline accuracy numbers are always the **local-only** numbers.
- **No telemetry.** The app does not phone home or collect analytics.
- **Model integrity.** Downloaded ONNX models are verified by checksum before
  use; report any gap in this verification as a security issue.
- **Dataset provenance.** Every ingested corpus carries license + consent +
  takedown metadata (`docs/provenance_ledger.json`), enforced in CI. To request
  removal of data attributed to a source, use that corpus's `takedownContact`.

## Dependency & supply-chain hygiene

- Rust dependencies are checked with `cargo-deny`; npm with `npm audit`.
- Pull requests that add network calls, new dependencies, or new IPC commands
  receive extra scrutiny.

Thank you for helping keep Cortex Speech and its users safe.
