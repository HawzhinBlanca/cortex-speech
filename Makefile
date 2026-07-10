.PHONY: help governance-proof verify-10 verify-10-quick gate test-rust test-frontend lint typecheck \
        fmt-check python-policies test-e2e audit deny eval-ckb egress-offline \
        bench-rtf release-proof ship-check build-app check-fresh ship-check-local

## help: list available targets
help:
	@grep -E '^##' $(MAKEFILE_LIST) | sed -e 's/## //'

## governance-proof: manifest sync, required assets, ledger schema, license compatibility (static; CI contract)
governance-proof:
	python scripts/verify_10.py --static

## verify-10: the personal-use full-charter aggregator (owner amendment 2026-07-10). One honest
## verdict line: RED / INCOMPLETE / GREEN-PERSONAL-USE-SHIP-READY; "10/10" is only printable when
## nothing is descoped or owner-gated (post P7 re-audit). Use `-- --quick` via verify-10-quick.
verify-10:
	python scripts/verify_10.py

## verify-10-quick: tiers 0-1 only (static governance + CI-equivalent code gates)
verify-10-quick:
	python scripts/verify_10.py --quick

## gate: alias for the narrow governance proof (use verify-10 for the full charter gate)
gate: governance-proof

## test-rust: backend unit + integration tests
test-rust:
	cargo test --manifest-path cortex-speech-app/src-tauri/Cargo.toml

## test-frontend: Svelte/TS unit tests
test-frontend:
	cd cortex-speech-app && npm test

## typecheck: svelte-check + tsc
typecheck:
	cd cortex-speech-app && npm run typecheck

## lint: eslint + clippy (-D warnings)
lint:
	cd cortex-speech-app && npm run lint
	cargo clippy --manifest-path cortex-speech-app/src-tauri/Cargo.toml --all-targets -- -D warnings

## fmt-check: rustfmt verification (CI: cargo fmt --all --check)
fmt-check:
	cargo fmt --manifest-path cortex-speech-app/src-tauri/Cargo.toml --all -- --check

## python-policies: honesty/privacy/CI/dataset policy regressions
python-policies:
	cd cortex-speech-app && npm run test:python-policies

## test-e2e: Playwright UI smoke (mocked backend) — NOT the real-app gate
test-e2e:
	cd cortex-speech-app && npm run test:e2e

## audit: npm supply-chain audit (prod deps), mirrors CI
audit:
	cd cortex-speech-app && npm audit --omit=dev

## deny: cargo supply-chain / license gate, mirrors CI
deny:
	cargo deny --manifest-path cortex-speech-app/src-tauri/Cargo.toml check

## build-app: rebuild the desktop app the RIGHT way — frontend FIRST, then the release exe. Use this
## (or `npm run tauri build`, which runs beforeBuildCommand=`npm run build`) for ANY src/** change. A
## bare `cargo build --release` skips the frontend build and ships a STALE UI (deep-audit F4).
build-app:
	cd cortex-speech-app && npm run build
	cargo build --release --manifest-path cortex-speech-app/src-tauri/Cargo.toml
	@echo "app rebuilt (fresh frontend): cortex-speech-app/src-tauri/target/release/cortex-speech-app.exe"

## measure-10: run the REAL accuracy scorecards on a gold manifest and record the numbers into
## docs/MEASUREMENTS.md (git SHA + manifest SHA-256 + exact command + full output). Owner-gated: run
## inside WSL on the 4090 box with the warm 7B server up and CORTEX_FINETUNED_{MODEL,ONNX} set.
## Usage:  make measure-10 GOLD=/mnt/c/path/to/gold.tsv   [BOOTSTRAP=3000] [ENGINES=7b,finetuned]
## Build a gold manifest first if needed:  python cortex-speech-app/scripts/build_ckb_gold.py 900
BOOTSTRAP ?= 3000
ENGINES ?= 7b,finetuned
measure-10:
	@test -n "$(GOLD)" || (echo "set GOLD=<gold_manifest.tsv> (WSL-visible path)"; exit 2)
	python cortex-speech-app/scripts/run_measurements.py "$(GOLD)" --engines "$(ENGINES)" --bootstrap $(BOOTSTRAP)

## eval-ckb: public CKB accuracy proof; fail-closed until FLEURS + AsoSoft artifact harness is wired
eval-ckb:
	@echo "eval-ckb is not yet a public benchmark gate."
	@echo "Required: pinned FLEURS ckb_iq + AsoSoft run, local ASR-on-gold, Sorani normalization, JSONL edits, 95% CI, MAPSSWE."
	@echo "Current smoke to run manually: cd cortex-speech-app && cargo test --manifest-path src-tauri/Cargo.toml --test real_audio gold_eval_asr_uses_real_engine_not_caller_hypotheses -- --nocapture"
	@exit 2

## egress-offline: runtime zero-outbound-socket proof for the default ASR/jury/eval/updater path
egress-offline:
	@echo "egress-offline is not yet a runtime socket gate."
	@echo "Required: default config ASR + jury + eval + updater under a network monitor with zero outbound connects."
	@exit 2

## bench-rtf: local real-time-factor benchmark on a pinned fixture/model
bench-rtf:
	@test -f cortex-speech-app/src-tauri/models/omniasr-ctc-300m/model.int8.onnx || (echo "missing OmniASR CTC-300M model; run cd cortex-speech-app && npm run fetch-models"; exit 2)
	cd cortex-speech-app && cargo test --manifest-path src-tauri/Cargo.toml --test real_audio -- --ignored omniasr_rtf_on_committed_fleurs_ckb_fixture --nocapture

## release-proof: signed installer/updater, SBOM, and artifact-attestation verification
release-proof:
	@test -n "$(RELEASE_DIR)" || (echo "set RELEASE_DIR=<directory containing signed installer, updater signature, SBOM, and attestation>"; exit 2)
	@test -f "$(RELEASE_DIR)/sbom.cdx.json" || (echo "missing $(RELEASE_DIR)/sbom.cdx.json"; exit 2)
	@test -f "$(RELEASE_DIR)/attestation.jsonl" || (echo "missing $(RELEASE_DIR)/attestation.jsonl"; exit 2)
	@test -f "$(RELEASE_DIR)/latest.json" || (echo "missing signed updater latest.json"; exit 2)
	@echo "release-proof artifact presence checks passed; signature and attestation cryptographic verification still require release CI credentials."

## ship-check: alias of verify-10 — one aggregator, one truth (superset of the CI release gate).
ship-check: verify-10

## check-fresh: P0.2 stale-exe guard — assert the built release exe is newer than every source
## file AND was compiled from the current git HEAD (SHA recovered from the binary, no execution).
## LOCAL-ONLY (CI has no Windows exe); the logic is unit-tested CI-safely by test_exe_freshness.py.
check-fresh:
	python cortex-speech-app/scripts/check_exe_freshness.py

## ship-check-local: the owner's one-command "everything green = ship" gate. Rebuilds the app the
## right way (frontend first), PROVES the running exe is HEAD-fresh, then runs the full CI gate set.
## Use this — not bare ship-check — before shipping a build to daily use.
ship-check-local: build-app check-fresh ship-check
	@echo ""
	@echo "================================================="
	@echo "  CORTEX ship-check-local green — exe is HEAD-fresh."
	@echo "================================================="
