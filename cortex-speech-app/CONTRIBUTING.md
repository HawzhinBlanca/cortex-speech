# Contributing to Cortex Speech

## Development Setup

1. **Prerequisites**
   - Rust 1.81+
   - Node.js 22
   - Python 3.12
   - PowerShell 5.1+ (Windows) or bash (Linux/macOS)

2. **Clone and install**
   ```bash
   git clone <repo>
   cd cortex-speech-app
   npm install
   ```

3. **Run in development mode**
   ```bash
   npm run tauri dev
   ```

## Code Standards

### Rust
- Edition 2021, no nightly features
- Use `thiserror` for error types
- All public items must have doc comments
- Run `cargo clippy` before committing
- Benchmark hot paths with `cargo bench`

### TypeScript/Svelte
- TypeScript strict mode
- Svelte 5 runes (`$state`, `$derived`, `$effect`) preferred over stores
- Use `onMount` for side effects
- Accessibility: all interactive elements need keyboard handlers
- Format with Prettier, lint with ESLint

## Commit Convention

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add batch speaker assignment
fix: correct audio decode empty buffer bug
test: add proptest for normalizer idempotence
perf: parallelize batch normalize with rayon
docs: update API reference
chore: bump dependencies
```

## Pull Request Process

1. Create a feature branch from `main`
2. Run full test suite:
   ```bash
   cd src-tauri && cargo test
   cd .. && npm run test
   npm run test:python-policies
   ```
3. Ensure `cargo check` and `vite build` pass with zero warnings
4. Update CHANGELOG.md
5. Open PR with description of changes

## Architecture Notes

- `src-tauri/src/commands.rs` - All IPC commands (one function = one command)
- `src-tauri/src/pipeline.rs` - Async-free sync pipeline (no MutexGuard across await)
- `src/lib/stores/` - Svelte stores for reactive state
- Pipeline opens its own DB connection via `db_path` string
- All modules are public for integration testing
