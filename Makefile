.PHONY: help verify-10 gate test-rust test-frontend lint typecheck \
        fmt-check python-policies test-e2e audit deny ship-check build-app \
        check-fresh ship-check-local

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

## build-app: rebuild the desktop app the RIGHT way — frontend FIRST, then the release exe. Use this
## (or `npm run tauri build`, which runs beforeBuildCommand=`npm run build`) for ANY src/** change. A
## bare `cargo build --release` skips the frontend build and ships a STALE UI (deep-audit F4).
build-app:
	cd cortex-speech-app && npm run build
	cargo build --release --manifest-path cortex-speech-app/src-tauri/Cargo.toml
	@echo "app rebuilt (fresh frontend): cortex-speech-app/src-tauri/target/release/cortex-speech-app.exe"

## ship-check: full pre-release gate — a SUPERSET of the CI Windows release gate.
## Running this green locally must imply CI green: it runs every required CI step.
ship-check: verify-10 typecheck lint fmt-check python-policies test-frontend test-rust test-e2e audit deny
	@echo ""
	@echo "================================================="
	@echo "  CORTEX ship-check complete — all CI gates green."
	@echo "================================================="

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
