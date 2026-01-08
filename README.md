# Wax

A fast, modern package manager that leverages Homebrew's ecosystem without the overhead. Built in Rust for speed and reliability, wax provides 16-20x faster search operations and parallel installation workflows while maintaining full compatibility with Homebrew formulae and bottles.

## Overview

Wax reimagines package management by replacing Homebrew's git-based tap system with direct JSON API access and parallel async operations. It reads from the same bottle CDN and formula definitions but executes operations through a compiled binary with modern concurrency primitives. The result is a package manager that feels instant for read operations and maximizes throughput for installations.

## Features

- **Lightning-Fast Queries**: Search and info commands execute in <100ms (16-20x faster than Homebrew)
- **Parallel Operations**: Concurrent downloads with individual progress tracking for each package
- **Lockfile Support**: Reproducible environments via `wax.lock` with pinned versions
- **Native Homebrew Compatibility**: Uses official formulae, bottles, and casks from Homebrew's JSON API
- **Modern Terminal UI**: Real-time progress bars, clean output, and responsive feedback
- **Minimal Resource Usage**: Single compiled binary with async I/O, no Ruby runtime overhead
- **Smart Caching**: Local formula index for offline search and instant lookups
- **Flexible Installation**: User-local (`~/.local/wax`) or system-wide deployment options

## Installation

```bash
# From source
git clone https://github.com/yourusername/wax.git
cd wax
cargo build --release
sudo cp target/release/wax /usr/local/bin/

# Using Cargo
cargo install wax-pm
```

## Usage

```bash
# Update formula index
wax update

# Search packages
wax search nginx
wax s nginx          # shorthand

# Show package details
wax info nginx
wax show nginx       # alias

# List installed packages
wax list
wax ls               # shorthand

# Install formulae
wax install tree
wax i tree           # shorthand
wax install tree --user    # to ~/.local/wax
wax install tree --global  # to system directory

# Install casks (GUI applications)
wax install --cask iterm2

# Uninstall packages
wax uninstall tree
wax rm tree          # shorthand

# Upgrade to latest version
wax upgrade nginx
wax up nginx         # shorthand

# Generate lockfile
wax lock

# Install from lockfile
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
- `cask.rs`: Cask handling for GUI applications (DMG mounting, app bundle copying)
- `deps.rs`: Dependency resolution with topological sorting
- `install.rs`: Installation orchestration (download → extract → symlink → hooks)
- `lockfile.rs`: Lockfile generation and synchronization
- `commands/`: CLI command implementations (search, install, upgrade, etc.)
- `ui.rs`: Terminal UI components using indicatif for progress tracking
- `error.rs`: Typed error handling with anyhow context
- `main.rs`: CLI parsing with clap and logging initialization

### Key Design Decisions

**JSON API over Git**: Fetches all ~15,600 formulae/casks via single HTTP request rather than cloning entire tap repository. Enables instant search without filesystem traversal.

**Bottles Only**: Does not build from source. Fails fast when bottles unavailable rather than triggering slow compilation. Ensures predictable performance.

**Async-First**: Uses tokio runtime for all I/O operations. Parallel downloads with configurable concurrency limits (default 8 simultaneous).

**Homebrew Coexistence**: Installs to same Cellar structure and reads existing Homebrew-installed packages. Can be used alongside `brew` without conflicts.

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

## Performance

Benchmarked against Homebrew 5.0.9 on macOS 15.6.1 (Apple M1):

| Operation | Homebrew | Wax | Speedup |
|-----------|----------|-----|---------|
| Search    | 1.41s    | 0.09s | 16x |
| Info      | 1.49s    | 0.08s | 20x |
| Install (simple) | 2.39s | 0.35s | 7x |
| Update (cold)    | 15-30s | 3.3s | 5-9x |

See `comparison.md` for detailed methodology and analysis.

## Limitations

- **Bottles Only**: Cannot build from source. Packages without bottles will fail to install.
- **Core Taps Only**: Currently supports homebrew/core and homebrew/cask. Custom taps not yet implemented.
- **macOS Primary**: Developed for macOS. Linux support planned but not yet complete.
- **No Formula DSL**: Uses pre-parsed JSON metadata. Complex formula logic (patches, conditional deps) may not be fully represented.

## License

MIT License
