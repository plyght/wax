<div align="center">
  <img src="/assets/images/Halftone Dots2x.png" alt="Wax Logo" width="200" />
</div>

# Wax

A fast, modern package manager that leverages Homebrew's ecosystem without the overhead. Built in Rust for speed and reliability, wax provides 16-20x faster search operations and parallel installation workflows while maintaining full compatibility with Homebrew formulae and bottles.

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
- **Built-in Self-Updater**: Update wax itself from crates.io (stable) or GitHub HEAD (nightly)
- **Windows Package Sources**: Scoop, winget, and Chocolatey integration via qualified names (`scoop/`, `winget/`, `choco/`) with portable installs to user-local paths

## Installation

### Linux and macOS

**One-liner (recommended)** — downloads the pre-built binary for your platform:

```bash
curl -fsSL https://raw.githubusercontent.com/plyght/wax/master/install.sh | bash
```

Installs to `~/.local/bin/wax`. Override the destination with `WAX_INSTALL_DIR=/usr/local/bin`.

**From a git clone** (builds with your Rust toolchain; no GitHub download):

```bash
git clone https://github.com/plyght/wax.git
cd wax
./install.sh
```

To **force** a pre-built release while standing in a clone, set `WAX_USE_RELEASE=1` before `./install.sh`.

### Windows

**One-liner (recommended)** — downloads the pre-built binary for your platform:

```powershell
irm https://raw.githubusercontent.com/plyght/wax/master/install.ps1 | iex
```

Installs to `%USERPROFILE%\.local\bin\wax.exe`. Override the destination with `$env:WAX_INSTALL_DIR = 'C:\Tools\bin'`.

**From a git clone** (builds with your Rust toolchain; no GitHub download):

```powershell
git clone https://github.com/plyght/wax.git
cd wax
.\install.ps1
```

To **force** a pre-built release while standing in a clone, set `$env:WAX_USE_RELEASE = '1'` before `.\install.ps1`.

### All platforms

GitHub Releases ship **Linux**, **macOS**, and **Windows** binaries (`wax-linux-*`, `wax-macos-*`, `wax-windows-x64.exe`, `wax-windows-arm64.exe`) with `.sha256` sidecars when published by CI.
The installer requires checksum verification by default. Set `WAX_NO_VERIFY=1` (or `$env:WAX_NO_VERIFY = '1'` on Windows) only when you explicitly accept installing without a `.sha256` sidecar.

**Homebrew tap** — coming soon.

**Cargo:**

```bash
cargo install waxpkg
```

**From source (manual)** — equivalent to `./install.sh` or `.\install.ps1` from a clone:

```bash
git clone https://github.com/plyght/wax.git
cd wax
cargo build --release
cp target/release/wax ~/.local/bin/
```

On Windows, copy `target\release\wax.exe` to a directory on your `PATH` (for example `%USERPROFILE%\.local\bin\`).

## Usage

```bash
# Update formula index
wax update

# Update wax itself
wax update s             # stable (from crates.io)
wax update self          # same as above
wax update -s            # same as above
wax update --self        # same as above
wax update sn            # nightly (from GitHub HEAD)
wax update self-nightly  # same as above
wax update -sn           # same as above
wax update -sf           # force reinstall stable
wax update sn --clean    # nightly + clean cargo git cache
wax update sn --no-clean # nightly + keep cargo cache
wax self-update          # stable self-update
wax self-update --nightly # nightly self-update from GitHub HEAD

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
wax install tree --no-script  # skip automatic post-install scripts

# Install casks with shorthand
wax cask iterm2
wax c firefox

# Manage custom taps (third-party taps must be trusted before install)
wax tap add user/repo --trust
wax tap list
wax tap trust user/repo
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
wax upgrade --self       # upgrade wax itself
wax upgrade -s -n        # nightly self-upgrade from GitHub HEAD
wax up -sn               # same as above
wax up nginx             # shorthand

# Generate lockfile
# Includes packages discovered from manual installs and other package managers when present
wax lock

# Install from lockfile
# Uses the same discovery pass to include manual installs / other package managers in the installed view
wax sync
```

### Windows package sources

On Windows, `wax install` and `wax search` accept **qualified package names** with an ecosystem prefix. Prefixes are case-insensitive. Use `brew/` or `homebrew/` to force the Homebrew formula index; omit a prefix to auto-pick the fastest matching source (Homebrew index, Scoop Main, winget-pkgs, or Chocolatey).

Supported prefixes: `scoop/`, `winget/`, `choco/`, `chocolatey/`, `brew/`, `homebrew/`.

```powershell
# Search a specific ecosystem
wax search scoop/ripgrep
wax search winget/Microsoft.WindowsTerminal
wax search choco/git

# Install from a specific ecosystem (portable layout under user-local paths)
wax install scoop/ripgrep
wax install winget/JesseDuffield.lazygit
wax install choco/git
wax install chocolatey/git   # alias for choco/

# Auto-pick the fastest matching source on Windows
wax install ripgrep

# Force Homebrew formula path (falls through to brew install flow)
wax install brew/openssl
wax install homebrew/openssl
```

Scoop and winget installs use portable zip/tar.gz layouts. Chocolatey support is limited to portable `tools/*.exe` packages (no MSI/system hooks). Packages with `/` in the id after a prefix are not supported.

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
- `error.rs`: Typed error handling with thiserror
- `package_spec.rs`: Qualified package name parsing (`scoop/`, `winget/`, `choco/`, `brew/`)
- `ecosystem_install.rs`: Routes installs to Homebrew, Scoop, winget, or Chocolatey on Windows
- `main.rs`: CLI parsing with clap and logging initialization

### Key Design Decisions

**JSON API over Git**: Fetches all ~15,600 formulae/casks via single HTTP request rather than cloning entire tap repository. Enables instant search without filesystem traversal.

**Bottles First, Source When Needed**: Prioritizes precompiled bottles for speed but automatically falls back to source compilation when bottles are unavailable. Supports multiple build systems for broad compatibility.

**Post-Install Control**: Runs supported post-install hooks when available through an installed Homebrew-compatible toolchain. Use `--no-script` on install/cask commands to skip automatic post-install work.

**Custom Tap Support**: Clones third-party taps as Git repositories, parses Ruby formula files, and integrates them with core formulae for unified package management.

**Async-First**: Uses the tokio multi-thread runtime for concurrent async I/O—HTTP, cache reads, and parallel install orchestration—with configurable download concurrency (default 8 simultaneous).

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
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test
cargo audit
```

Requires Rust 1.70+. Key dependencies:

- **CLI**: clap (parsing), console (colors), inquire (prompts)
- **Async**: tokio (multi-thread runtime, concurrent I/O), reqwest (HTTP), futures (combinators)
- **Serialization**: serde, serde_json, toml
- **UI**: indicatif (progress bars)
- **Compression**: tar, flate2 (gzip), sha2 (checksums)
- **Error Handling**: thiserror
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
- **macOS casks / DMG flow**: Full cask install, uninstall, and upgrade (DMG mounting, app bundle copying) is macOS-specific. Windows GUI apps are installed via portable Scoop/winget/Chocolatey packages, not the cask pipeline.
- **Windows package scope**: Scoop/winget/Chocolatey support covers portable installs only—no MSI installers, Chocolatey PowerShell hooks, or winget `winget.exe` delegation. Chocolatey packages must ship portable `tools/*.exe` binaries.
- **Build System Detection**: Source builds use heuristic detection of build systems. Complex or non-standard build configurations may fail.
- **Formula DSL Subset**: Parses essential Ruby formula syntax. Advanced features (conditional deps, patches, custom install blocks) may not be fully supported.
- **Platform maturity**: macOS is the primary development target. Linux support is functional but less tested. Windows is supported via Scoop, winget-pkgs, and Chocolatey portable installs alongside the Homebrew formula index.
- **Post-Install Coverage**: Wax can run supported post-install hooks when a compatible `brew postinstall` command is installed. Use `--no-script` to skip this behavior. Native post-install execution without Homebrew compatibility tooling is still limited.

## Acknowledgments

Huge thanks to [@undivisible](https://github.com/undivisible) on GitHub for being a massive help and contributor to this project. Wax wouldn't be what it is without his support. <br>
[@undivisible](https://github.com/undivisible): Huge thanks to [@plyght](https://github.com/plyght) for making the original base and contributing with all our projects together. Keep an eye on our org! We are going to make great things together!

## License

MIT License
