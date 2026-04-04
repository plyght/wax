<div align="center">
  <img src="/assets/images/Halftone Dots2x.png" alt="Wax Logo" width="200" />
</div>

# Wax

A fast, modern package manager that leverages Homebrew's ecosystem without the overhead. Built in Rust for speed and reliability, wax provides 16-20x faster search operations and parallel installation workflows while maintaining full compatibility with Homebrew formulae and bottles.

## Current status

- cargo test passes on the current checkout.
- Recent work focuses on source-build and system package handling.

## Overview

Wax reimagines package management by replacing Homebrew's git-based tap system with direct JSON API access and parallel async operations. It reads from the same bottle CDN and formula definitions but executes operations through a compiled binary with modern concurrency primitives. The result is a package manager that feels instant for read operations and maximizes throughput for installations.

## Features

- **Lightning-Fast Queries**: Search and info commands execute in <100ms (16-20x faster than Homebrew)
- **Intelligent Auto-Detection**: Automatically detects formulae vs casks - no need to specify `--cask` flags
- **Parallel Operations**: Concurrent downloads with individual progress tracking for each package
- **Full Cask Support**: Install, uninstall, upgrade, and manage GUI applications seamlessly
- **Source Building**: Automatic fallback to source compilation when bottles unavailable, with support for Autotools, CMake, Meson, and Make
- **Custom Tap Support**: Add, manage, and update third-party Homebrew taps for extended package availability
- **Lockfile Support**: Reproducible environments via `wax.lock` with pinned versions
- **Native Homebrew Compatibility**: Uses official formulae, bottles, and casks from Homebrew's JSON API
- **Homebrew Interoperability**: Works standalone or alongside Homebrew without conflicts - installation order independent
- **Modern Terminal UI**: Real-time progress bars, clean output, and responsive feedback
- **Minimal Resource Usage**: Single compiled binary with async I/O, no Ruby runtime overhead
- **Smart Caching**: Local formula index for offline search and instant lookups
- **Flexible Installation**: User-local (`~/.local/wax`) or system-wide deployment options
- **Built-in Self-Updater**: Update wax itself from crates.io (stable) or GitHub (nightly)

## Installation

**One-liner (recommended)** — downloads the pre-built binary for your platform:

```bash
curl -fsSL https://raw.githubusercontent.com/semitechnological/wax/master/install.sh | bash
```

Installs to `~/.local/bin/wax`. Override the destination with `WAX_INSTALL_DIR=/usr/local/bin`.
Pin a specific release with `WAX_VERSION=v0.13.3`.

**From a git clone** (builds with your Rust toolchain; no GitHub download):

```bash
git clone https://github.com/semitechnological/wax.git
cd wax
./install.sh
```

On Windows, from the clone: `.\install.ps1` (same idea: `cargo build --release`, then copies `wax.exe`).

To **force** a pre-built release while standing in a clone, set `WAX_USE_RELEASE=1` before `./install.sh`, or `$env:WAX_USE_RELEASE = '1'` before `.\install.ps1`.

**Windows (PowerShell one-liner)** — downloads the matching pre-built binary from [GitHub Releases](https://github.com/semitechnological/wax/releases) (`wax-windows-x64.exe` or `wax-windows-arm64.exe`; requires a release produced after this asset set exists):

```powershell
irm https://raw.githubusercontent.com/semitechnological/wax/master/install.ps1 | iex
```

Installs to `%USERPROFILE%\.local\bin\wax.exe` by default. Override with `$env:WAX_INSTALL_DIR = 'C:\path\to\bin'` before running. Pin a build with `$env:WAX_VERSION = 'v0.14.3'`.

If `iex` is blocked, use: `powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://raw.githubusercontent.com/semitechnological/wax/master/install.ps1 | iex"`

From `cmd.exe` you can run that same `powershell -NoProfile ...` line.

**Execution policy (narrowest change):** To allow local scripts **only for the current PowerShell process** (recommended instead of changing machine-wide policy), run once per window before `.\install.ps1` or `iex`:

```powershell
Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass
```

That does not persist after you close the session. Prefer this over `RemoteSigned`/`Unrestricted` at `CurrentUser` or `LocalMachine` unless you intentionally want a broader policy.

Release archives include **Linux** (`wax-linux-x64`, `wax-linux-arm64`), **macOS** (`wax-macos-x64`, `wax-macos-arm64`), and **Windows** (`wax-windows-x64.exe`, `wax-windows-arm64.exe`), each with a `.sha256` sidecar when published by CI.

**Windows (build from source)** — with Rust from [rustup](https://rustup.rs/), use `.\install.ps1` from a clone (above), or manually:

```powershell
git clone https://github.com/semitechnological/wax.git
cd wax
cargo build --release
.\target\release\wax.exe --help
```

**Homebrew tap** — builds from source via cargo:

```bash
brew tap semitechnological/tap
brew install --HEAD wax
```

**Cargo:**

```bash
cargo install waxpkg
```

**From source (manual)** — equivalent to `./install.sh` from a clone:

```bash
git clone https://github.com/semitechnological/wax.git
cd wax
cargo build --release
cp target/release/wax ~/.local/bin/
```

## Usage

```bash
# Update formula index
wax update

# Update wax itself
wax update -s            # stable (from crates.io)
wax update --self        # same as above
wax update -sn           # nightly (from GitHub)
wax update -sf           # force reinstall

# Search packages
wax search nginx
wax s nginx          # shorthand

# Show package details (auto-detects formulae or casks)
wax info nginx
wax info iterm2
wax show nginx       # alias

# List installed packages
wax list
wax ls               # shorthand

# Install packages (auto-detects formulae or casks)
wax install tree
wax install iterm2
wax i tree           # shorthand
wax install tree --user    # to ~/.local/wax
wax install tree --global  # to system directory
wax install tree --build-from-source  # force source build

# Install casks with shorthand
wax cask iterm2
wax c firefox

# Manage custom taps
wax tap add user/repo
wax tap list
wax tap update user/repo
wax tap remove user/repo

# Uninstall packages (auto-detects formulae or casks)
wax uninstall tree
wax uninstall iterm2
wax rm tree          # shorthand

# Check for outdated packages
wax outdated

# Upgrade packages (auto-detects formulae or casks)
wax upgrade              # upgrade all outdated packages
wax upgrade nginx        # upgrade specific package
wax upgrade nginx tree   # upgrade multiple packages
wax up nginx             # shorthand

# Generate lockfile
# Includes packages discovered from manual installs and other package managers when present
wax lock

# Install from lockfile
# Uses the same discovery pass to include manual installs / other package managers in the installed view
wax sync
```

## Configuration

Wax stores configuration and cache in `~/.wax/` (or platform-specific cache directory):

```
~/.wax/
  cache/
    formulae.json      # Cached formula index (~8,100 packages)
    casks.json         # Cached cask index (~7,500 apps)
  locks/
    wax.lock          # Lockfile for reproducible installs
  logs/
    wax.log           # Operation logs with structured tracing
```

### Lockfile Format

`wax.lock` uses TOML for human-readable version pinning:

```toml
[packages]
nginx = { version = "1.25.3", bottle = "arm64_ventura" }
openssl = { version = "3.1.4", bottle = "arm64_ventura" }
tree = { version = "2.1.1", bottle = "arm64_ventura" }
```

## Architecture

- `api.rs`: Homebrew JSON API client with async HTTP requests
- `cache.rs`: Local formula/cask index management and invalidation
- `bottle.rs`: Bottle download, extraction, and verification (SHA256 checksums)
- `builder.rs`: Source compilation with multi-build-system support (Autotools, CMake, Meson, Make)
- `cask.rs`: Cask handling for GUI applications (DMG mounting, app bundle copying)
- `deps.rs`: Dependency resolution with topological sorting
- `formula_parser.rs`: Ruby formula parsing and build metadata extraction
- `install.rs`: Installation orchestration (download → extract → symlink → hooks)
- `lockfile.rs`: Lockfile generation and synchronization
- `tap.rs`: Custom tap management (add, remove, update, formula loading)
- `commands/`: CLI command implementations (search, install, upgrade, tap, etc.)
- `ui.rs`: Terminal UI components using indicatif for progress tracking
- `error.rs`: Typed error handling with anyhow context
- `main.rs`: CLI parsing with clap and logging initialization

### Key Design Decisions

**JSON API over Git**: Fetches all ~15,600 formulae/casks via single HTTP request rather than cloning entire tap repository. Enables instant search without filesystem traversal.

**Bottles First, Source When Needed**: Prioritizes precompiled bottles for speed but automatically falls back to source compilation when bottles are unavailable. Supports multiple build systems for broad compatibility.

**Custom Tap Support**: Clones third-party taps as Git repositories, parses Ruby formula files, and integrates them with core formulae for unified package management.

**Async-First**: Uses tokio runtime for all I/O operations. Parallel downloads with configurable concurrency limits (default 8 simultaneous).

**Homebrew Interoperability**: Designed to coexist peacefully with Homebrew. Installs to the same Cellar structure using architecture-appropriate standard paths (`/opt/homebrew` on Apple Silicon, `/usr/local` on Intel). Detects and respects existing Homebrew installations, allowing both package managers to operate independently or simultaneously without conflicts. Installation order does not matter - wax functions identically whether installed before or after Homebrew.

## Development

```bash
# Build debug binary
cargo build

# Build optimized release
cargo build --release

# Run tests
cargo test

# Run with verbose logging
cargo run -- --verbose install tree

# Check for issues
cargo clippy
```

Requires Rust 1.70+. Key dependencies:

- **CLI**: clap (parsing), console (colors), inquire (prompts)
- **Async**: tokio (runtime), reqwest (HTTP), futures (combinators)
- **Serialization**: serde, serde_json, toml
- **UI**: indicatif (progress bars)
- **Compression**: tar, flate2 (gzip), sha2 (checksums)
- **Error Handling**: anyhow, thiserror
- **Logging**: tracing, tracing-subscriber
- **Build Support**: num_cpus (parallel builds), tempfile (build directories)

## Performance

Benchmarked against Homebrew on macOS (Apple Silicon):

| Operation | Homebrew | Wax | Speedup |
|-----------|----------|-----|---------|
| Search    | 1.41s    | 0.09s | 16x |
| Info      | 1.49s    | 0.08s | 20x |
| Install   | 2.39s    | 0.55s | 4.3x |
| Update (warm) | 0.85s | 0.15s | 5.7x |
| Update (cold) | 13.2s | 1.0s | 13.2x |

**Note**: Warm cache updates use HTTP conditional requests (ETag/If-Modified-Since) for instant responses. Cold cache updates use gzip/brotli compression for efficient downloads.

See `docs/comparison.md` for detailed methodology and analysis.

## Limitations

- **Linux Bottles**: Linux bottles require `patchelf` for ELF binary relocation. Install it first: `wax install patchelf`
- **Linux GUI / cask flow**: On Linux, GUI-style installs use cask Ruby metadata when an `on_linux` block is present; otherwise Wax may try snap, flatpak, or the native system package manager—not the macOS DMG install path.
- **Build System Detection**: Source builds use heuristic detection of build systems. Complex or non-standard build configurations may fail.
- **Formula DSL Subset**: Parses essential Ruby formula syntax. Advanced features (conditional deps, patches, custom install blocks) may not be fully supported.
- **macOS Primary**: Developed for macOS. Linux support is functional but less tested.
- **No Post-Install Scripts**: Skips formula post-install hooks for security and performance. Some packages may require manual configuration.

## License

MIT License
