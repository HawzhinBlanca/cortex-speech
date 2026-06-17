.PHONY: help verify-10 gate test-rust test-frontend lint typecheck ship-check

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

## ship-check: full pre-release gate — everything that must be green to ship
ship-check: verify-10 typecheck lint test-frontend test-rust
	@echo ""
	@echo "================================================="
	@echo "  CORTEX ship-check complete — all gates green."
	@echo "================================================="
