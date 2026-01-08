# Wax - Fast Homebrew-Compatible Package Manager

## Problem Statement

Homebrew is slow and has poor UX:
- `brew update` takes 15-30s (git pulls entire tap)
- Sequential installs even when possible to parallelize
- Cluttered output, no modern progress indicators
- Auto-updates constantly without asking
- No lockfile support for reproducible environments
- Ruby runtime overhead

## Solution

Build a fast, modern CLI tool that uses Homebrew's existing ecosystem (formulae, bottles, casks) but with better performance and UX.

## Goals

### Primary
- 10x faster tap updates (< 2s vs 15-30s)
- 5x faster installs via parallel downloads
- Modern terminal UI with progress bars
- Lockfile support for reproducibility
- 100% compatible with Homebrew formulae/bottles

### Secondary
- Coexist with `brew` (users can use both)
- Single binary distribution
- Better error messages
- Cask support (GUI apps)

### Non-Goals
- Building from source (use Homebrew's bottles only)
- Custom formula format (use Homebrew's)
- Replacing Homebrew entirely (complement it)

## Technical Architecture

### Language: Rust

**Core Dependencies**:

```toml
[dependencies]
# CLI Framework
clap = { version = "4", features = ["derive", "cargo"] }

# Async Runtime & HTTP
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json", "stream"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"  # For wax.lock lockfiles

# Terminal UI
indicatif = "0.17"  # Progress bars, spinners, multi-progress
console = "0.15"    # Terminal colors, styles
inquire = "0.7"     # Interactive prompts (confirmations, selections)

# Error Handling
anyhow = "1"        # Ergonomic error handling
thiserror = "1"     # Custom error types

# Logging
tracing = "0.1"                    # Structured logging
tracing-subscriber = "0.3"         # Log formatting
tracing-appender = "0.2"           # File rotation

# File Operations
tar = "0.4"         # Extract tarballs
flate2 = "1"        # Gzip decompression
sha2 = "0.10"       # SHA256 checksum verification
tempfile = "3"      # Temporary directories

# System Integration
directories = "5"   # Cross-platform paths (~/.wax)
dunce = "1"         # Normalize Windows paths (future Linux support)
```

**Key Library Details**:

- **indicatif**: Multi-progress bar support for parallel downloads
  - `MultiProgress` for coordinating multiple progress bars
  - `ProgressBar` with templates for custom styling
  - Spinner styles for indeterminate operations
  
- **console**: Terminal capability detection
  - Auto-disable colors in non-TTY environments
  - Terminal width detection for responsive layouts
  
- **inquire**: User interaction
  - `Confirm::new()` for yes/no prompts
  - `Select::new()` for choosing between options
  - `MultiSelect::new()` for batch operations
  
- **anyhow/thiserror**: Layered error handling
  - `anyhow` for application errors with context
  - `thiserror` for library-level typed errors
  
- **tracing**: Structured logging to `~/.wax/logs/wax.log`
  - JSON formatted logs for debugging
  - Configurable log levels (debug, info, warn, error)

### Data Sources

1. **Homebrew JSON API**: https://formulae.brew.sh/api/formula.json
   - All formulae metadata
   - Bottle URLs and checksums
   - Dependencies

2. **Homebrew Cask API**: https://formulae.brew.sh/api/cask.json
   - GUI app metadata

3. **Bottle Storage**: Homebrew's CDN
   - Pre-built binaries (`.tar.gz`)

### Installation Flow

```
1. Fetch formula metadata (JSON API)
2. Resolve dependencies (topological sort)
3. Download bottles in parallel (tokio)
4. Extract to temp directory
5. Symlink to /opt/homebrew or /usr/local
6. Run post-install hooks if any
```

### File Structure

```
~/.wax/
  cache/
    formulae.json      # Cached formula index
    casks.json         # Cached cask index
  locks/
    wax.lock          # Lockfile for current env
  logs/
    wax.log           # Operation logs
```

## Features

### Phase 1: Read-Only (MVP)

**Commands**:
- `wax search <query>` - Search formulae/casks
- `wax info <formula>` - Show formula details
- `wax list` - List installed packages (read from Homebrew)
- `wax update` - Update formula index (fast, < 2s)

**Goal**: Prove faster updates + better UX

### Phase 2: Installation

**Commands**:
- `wax install <formula>` - Install formula (bottles only)
- `wax uninstall <formula>` - Remove formula
- `wax upgrade <formula>` - Upgrade to latest

**Features**:
- Parallel downloads (max 8 concurrent)
- Dependency resolution
- Progress bars per download
- Dry-run mode (`--dry-run`)

### Phase 3: Lockfiles

**Commands**:
- `wax lock` - Generate `wax.lock` from current state
- `wax sync` - Install exact versions from `wax.lock`

**Lockfile Format** (`wax.lock`):
```toml
[packages]
nginx = { version = "1.25.3", bottle = "arm64_ventura" }
openssl = { version = "3.1.4", bottle = "arm64_ventura" }
```

### Phase 4: Cask Support

**Commands**:
- `wax install --cask <app>` - Install GUI app
- `wax uninstall --cask <app>` - Remove GUI app

**Challenges**:
- DMG mounting
- App bundle copying to `/Applications`
- Handling installers (`.pkg`)

## UI/UX Design

### Progress Indicators (using indicatif)

**Multi-download progress**:
```rust
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

let multi = MultiProgress::new();
let style = ProgressStyle::default_bar()
    .template("{prefix:.bold} {bar:40.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec}")
    .unwrap()
    .progress_chars("█▓▒░ ");

// Create progress bar per download
let pb = multi.add(ProgressBar::new(total_size));
pb.set_style(style.clone());
pb.set_prefix(format!("[>] {}", formula_name));
```

**Output**:
```
[>] Downloading dependencies (3/5)
  [✓] openssl@3.1.4       [████████████████████████████████████████] 15.2 MB/15.2 MB @ 12.3 MB/s
  [>] nginx@1.25.3        [████████████████████▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓] 8.7 MB/19.3 MB @ 8.1 MB/s
  [ ] pcre2@10.42         [░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░] queued

[✓] Installed nginx 1.25.3 in 4.2s
```

**Spinner for indeterminate tasks**:
```rust
let spinner = ProgressBar::new_spinner();
spinner.set_style(
    ProgressStyle::default_spinner()
        .template("{spinner:.green} {msg}")
        .unwrap()
);
spinner.set_message("Resolving dependencies...");
```

**Interactive confirmations (using inquire)**:
```rust
use inquire::Confirm;

let confirm = Confirm::new("Upgrade will modify 12 packages. Continue?")
    .with_default(true)
    .prompt()?;
```

### Error Messages (using console)

```rust
use console::style;

eprintln!("{}", style("Error:").red().bold());
eprintln!("  Failed to install nginx");
eprintln!();
eprintln!("{}", style("Reason:").yellow());
eprintln!("  Dependency openssl@3 not found");
eprintln!();
eprintln!("{}", style("Suggestions:").cyan());
eprintln!("  - Run: wax install openssl@3");
eprintln!("  - Or: wax install nginx --ignore-dependencies");
```

**Output**:
```
Error: Failed to install nginx

Reason: Dependency openssl@3 not found

Suggestions:
  - Run: wax install openssl@3
  - Or: wax install nginx --ignore-dependencies

Need help? https://github.com/user/wax/issues
```

## Success Metrics

### Performance
- `wax update` < 2s (vs `brew update` 15-30s)
- `wax install nginx` < 5s (vs `brew install nginx` 15-20s)
- Parallel installs 3-5x faster for multi-package operations

### Adoption
- 100 GitHub stars initially
- 10 active users providing feedback
- Zero corruption/breaking existing Homebrew installs

### Quality
- 95% formula compatibility (bottles only)
- Zero data loss
- Clear error messages for unsupported edge cases

## Milestones

### Phase 1: Foundation
- [ ] Project setup (Cargo, CLI skeleton)
- [ ] Fetch and parse Homebrew JSON API
- [ ] `wax search` command
- [ ] `wax info` command

### Phase 2: Core Install
- [ ] Download bottle to temp directory
- [ ] Extract tarball
- [ ] Symlink to correct location
- [ ] `wax install <simple-formula>` (no dependencies)

### Phase 3: Dependencies
- [ ] Dependency resolution algorithm
- [ ] Parallel downloads with progress bars
- [ ] Handle dep conflicts
- [ ] `wax install <complex-formula>` (with deps)

### Phase 4: Polish
- [ ] `wax uninstall`
- [ ] `wax upgrade`
- [ ] Better error messages
- [ ] Release v0.1.0

### Phase 5: Lockfiles
- [ ] `wax lock` - generate lockfile
- [ ] `wax sync` - install from lockfile
- [ ] Documentation

### Phase 6: Casks (optional)
- [ ] Parse cask JSON
- [ ] Handle DMG mounting
- [ ] Copy apps to `/Applications`
- [ ] `wax install --cask`

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Homebrew API changes | High | Version the API client, monitor changes |
| Bottle format changes | High | Fail gracefully, fallback to brew |
| Symlink conflicts | Medium | Detect existing files, prompt user |
| Corrupted installs | High | Atomic operations, rollback on failure |
| Security (malicious bottles) | Critical | Verify checksums from API |

## Distribution

### Initial
```bash
# Build from source
git clone https://github.com/user/wax
cd wax
cargo build --release
cp target/release/wax /usr/local/bin/
```

### Future
- Homebrew formula (ironic but effective)
- GitHub releases with pre-built binaries
- `curl | sh` installer

## Open Questions

1. **Coexistence**: How to detect Homebrew-installed packages to avoid conflicts?
   - Read Homebrew's Cellar directory
   - Store metadata in `~/.wax/installed.json`

2. **Building from source**: Support or enforce bottles-only?
   - Phase 1: Bottles only
   - Phase 2: Fallback to `brew install --build-from-source`

3. **Tap management**: Support custom taps?
   - Phase 1: Only homebrew/core and homebrew/cask
   - Phase 2: Custom taps via `wax tap add`

4. **Linux support**: macOS only or cross-platform?
   - Phase 1: macOS only (Homebrew's primary platform)
   - Phase 2: Linux support if demand exists

## References

- Homebrew JSON API: https://formulae.brew.sh/api/
- Homebrew Source: https://github.com/Homebrew/brew
- Bottle Format: https://docs.brew.sh/Bottles
- Rust CLI Book: https://rust-cli.github.io/book/
