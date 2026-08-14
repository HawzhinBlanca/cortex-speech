# Cortex Speech — Kurdish Speech Data Governance Policy

This document governs the collection, curation, processing, and redistribution of Sorani Kurdish (Central Kurdish, `ckb`) speech data within the Cortex Speech ecosystem.

---

## ⚖️ Ethical Mandate & Minority Language Protection

Central Kurdish is a resource-constrained and politically sensitive minority language. Speech data is a high-value asset, but its misuse can expose speakers to surveillance, profiling, or unauthorized commercial exploitation. Cortex Speech is committed to:
1. **100% Transparency**: Every speech segment used or exported by the refinery must carry verifiable license, consent, and attribution provenance.
2. **Explicit Consent**: Zero silent collection. No audio or transcript leaves the local device for cloud services (Gemini/Claude) unless the user actively acknowledges and opts in via the **Informed Consent Dialog**.
3. **Right to Erasure (Takedown)**: Any individual has the right to request the removal of their voice assets from any dataset published or distributed by Cortex.

---

## 🎙️ Owner-Supplied Recordings — Full Licence (2026-08-14, FINAL)

Every audio and voice recording supplied by the owner is **fully licensed to this project**:

> Full permission and total authority, **including public use**. All speakers were **paid** and
> agreed to unrestricted use of their voices for this application and its datasets, anywhere — the
> recordings were made partly for this purpose. **No royalty or licensing obligation applies.**

**This question is closed.** Nothing in this project may block, gate, warn about, or hedge on rights
for owner-supplied audio, and it is not to be raised again. Every clip in the library carries:

| field | value |
|---|---|
| `rights_license` | `owner-full-rights` |
| `rights_consent_basis` | `speaker-agreement-paid-unrestricted-public` |
| `rights_permitted_use` | `unrestricted: train, evaluate, publish, redistribute, commercial` |
| `rights_attribution` | `Hawzhin (owner) — speakers paid and agreed to full public use` |
| `rights_source` | `owner-supplied recording` |

Third-party corpora below remain a separate matter — one of FACT rather than permission. FLEURS is
the frozen evaluation set, so training on it would invalidate every measured CER; Common Voice and
any future external dataset carry their own licences. That distinction protects the owner's own
measurements, and has nothing to do with consent.

---

## 📂 Source Corpora License & Consent Ledger

All ingested and benchmarked datasets must be audited against this provenance ledger before training, validation, or distribution.

### 1. FLEURS (Kurdish config `ckb_iq`)
* **Source URL**: https://huggingface.co/datasets/google/fleurs
* **SPDX License**: `CC-BY-4.0`
* **Attribution**: "FLEURS Kurdish Dataset, Google LLC, licensed under CC BY 4.0."
* **Consent Basis**: Academic/Public release under Google Terms of Service.
* **Redistribution Rights**: Permitted with attribution. Compatible with Apache 2.0.

### 2. AsoSoft-600 (Sorani Benchmark)
* **Source URL**: https://github.com/AsoSoft/AsoSoft-Library-py (exposed via `PawanKrd/asr-ckb-v2`)
* **SPDX License**: `CC-BY-SA-4.0`
* **Attribution**: "AsoSoft Kurdish Corpus, AsoSoft Team, licensed under CC BY-SA 4.0."
* **Consent Basis**: Academic public dataset.
* **Redistribution Rights**: Permitted under ShareAlike. 
* **License Compatibility Warning**: **SHARE-ALIKE CONTAMINATING**. Any derivative dataset derived from AsoSoft text or audio must also be distributed under `CC-BY-SA-4.0`. The core app's own source is licensed separately (PolyForm Noncommercial 1.0.0 — see root
`LICENSE`), but the exported AsoSoft-derived data is isolated and gated regardless.

### 3. CORDI (Central Kurdish Dialect Corpus)
* **Source URL**: Academic release
* **SPDX License**: `CC-BY-NC-SA-4.0`
* **Attribution**: "CORDI Dialect Corpus."
* **Consent Basis**: Research-only consent.
* **Redistribution Rights**: **NON-COMMERCIAL & SHARE-ALIKE CONTAMINATING**. Blocked from any commercial/redistributable model-training export.

### 4. Mozilla Common Voice Kurdish (`ckb`)
* **Source URL**: https://commonvoice.mozilla.org/ckb
* **SPDX License**: `CC0-1.0` (Public Domain)
* **Attribution**: "Mozilla Common Voice Kurdish Dataset, Public Domain."
* **Consent Basis**: Explicit voice donation under Mozilla Common Voice terms.
* **Redistribution Rights**: Fully permissive. Compatible with Apache 2.0.

---

## 🚫 Takedown & Right to Erasure Process

If you identify your voice or name in any dataset compiled or published by Cortex Speech, you may request immediate removal:
* **Contact**: data-governance@cortex-speech.org (or file an issue on GitHub)
* **Processing Timeline**: All valid takedown requests will result in segment deletion from the next nightly/minor update of the database and HF mirrors within **48 hours**.
* **Audit Lineage**: Deletion requests are recorded by audio hash in a local CRL (Consent Revocation List) to prevent re-ingestion.
