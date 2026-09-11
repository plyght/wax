# AGENTS.md

Guidance for coding agents working in the Wax repository (crate `waxpkg`, binary `wax`).

## Layout

- `src/main.rs` — clap CLI definition and command dispatch. New subcommands are added here.
- `src/commands/` — one module per user-facing command (`search`, `info`, `install`, `upgrade`, …).
- `src/api.rs`, `src/cache.rs`, `src/http_client.rs` — Homebrew JSON API client and local formula/cask index.
- `src/install.rs`, `src/bottle.rs`, `src/builder.rs`, `src/deps.rs` — install pipeline: bottle download/extract, source builds, dependency resolution.
- `src/cask.rs` — cask handling (DMG mount, app bundle copy); mostly macOS-specific.
- `src/package_spec.rs` — qualified package names (`scoop/`, `winget/`, `choco/`, `brew/`).
- `src/scoop.rs`, `src/winget_install.rs`, `src/chocolatey.rs`, `src/ecosystem_install.rs`, `src/remote_search.rs`, `src/windows_state.rs` — Windows package-source backends.
- `src/tap.rs`, `src/lockfile.rs`, `src/discovery.rs`, `src/adopt.rs`, `src/system_pm.rs` — taps, lockfiles, installed-package discovery, native OS package managers.
- `tests/cli.rs` — integration tests that run the real binary.
- `examples/` — small runnable scenarios (see `examples/README.md`).
- `docs/` — CLI, architecture, development, and troubleshooting notes.

## Commands

```bash
cargo build                 # debug binary at target/debug/wax
cargo run -- search nginx   # run the CLI from source
cargo test                  # unit + integration tests
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy, and `cargo test` on Linux, macOS, and Windows. Keep all three green.

## Testing

- `tests/cli.rs` runs the compiled binary via `CARGO_BIN_EXE_wax`. Tests set `WAX_CACHE_DIR` to a temp dir to stay hermetic; some use `WAX_TEST_CELLAR` for an isolated Cellar.
- Network-dependent tests are gated behind the `INTEGRATION` env var and are skipped without connectivity. Do not assume they run in CI.
- Keep source-build, cask, and package-management changes covered by `cargo test`.
- Windows backends are compiled on every platform via `#[cfg(any(target_os = "windows", test))]`, so their unit tests run under `cargo test` on unix too.

## Windows vs unix modules

- OS-specific behavior belongs behind `#[cfg(target_os = "...")]`. macOS is the primary development target; Linux is functional but less tested.
- Cask/DMG installs and Homebrew Cellar paths are unix/macOS. Linux bottles need `patchelf` for ELF relocation.
- Windows installs use portable Scoop/winget/Chocolatey layouts. Homebrew-only commands call `error::reject_homebrew_cli(...)` under `#[cfg(target_os = "windows")]`.
- Do not reference Windows-only APIs unguarded: it breaks the unix `cargo test` build.

## Do not

- Never invent or fabricate secrets, tokens, URLs, package names, or version numbers. Read them from the repo or ask.
- Never commit credentials, `.env` files, or private keys.
- Do not add Homebrew/Windows behavior the code does not already implement; check the source first.
- Do not disable checksum verification (`WAX_NO_VERIFY`) in committed scripts or tests.

## Conventions

- Edition 2021; match the surrounding code style.
- Commit messages: `<type>: <subject>` with `feat` / `fix` / `docs` / `refactor` / `test` / `perf`.
