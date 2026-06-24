.PHONY: help verify-10 gate test-rust test-frontend lint typecheck \
        fmt-check python-policies test-e2e audit deny ship-check

## help: list available targets
help:
	@grep -E '^##' $(MAKEFILE_LIST) | sed -e 's/## //'

## verify-10: the charter's 10/10 gate (manifest sync, ledger schema, license compatibility)
verify-10:
	python scripts/verify_10.py

## gate: alias for verify-10
gate: verify-10

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

## ship-check: full pre-release gate — a SUPERSET of the CI Windows release gate.
## Running this green locally must imply CI green: it runs every required CI step.
ship-check: verify-10 typecheck lint fmt-check python-policies test-frontend test-rust test-e2e audit deny
	@echo ""
	@echo "================================================="
	@echo "  CORTEX ship-check complete — all CI gates green."
	@echo "================================================="
