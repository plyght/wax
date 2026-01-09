# Architecture

## Overview

Wax is a fast, modern package manager built in Rust that leverages Homebrew's ecosystem without the overhead. This document describes the technical architecture, design decisions, and implementation patterns.

## System Design

### Core Philosophy

Wax replaces Homebrew's git-based tap system with direct JSON API access and parallel async operations. It reads from the same bottle CDN and formula definitions but executes operations through a compiled binary with modern concurrency primitives.

### Key Design Decisions

**JSON API over Git**: Fetches all ~15,600 formulae/casks via single HTTP request rather than cloning entire tap repository. Enables instant search without filesystem traversal.

**Bottles First, Source When Needed**: Prioritizes precompiled bottles for maximum speed. Automatically falls back to source compilation when bottles are unavailable. Detects build system (Autotools, CMake, Meson, Make) and executes appropriate build workflow.

**Custom Tap Support**: Extends beyond core taps by cloning third-party taps as Git repositories. Parses Ruby formula files to extract metadata and build instructions. Integrates tap formulae with core formulae for unified package discovery.

**Async-First**: Uses tokio runtime for all I/O operations. Parallel downloads with configurable concurrency limits (default 8 simultaneous).

**Homebrew Coexistence**: Installs to same Cellar structure and reads existing Homebrew-installed packages. Can be used alongside brew without conflicts.

## Module Architecture

### Core Modules

**main.rs**: Entry point and command routing
- CLI parsing with clap derive macros
- Command dispatch to appropriate handlers
- Logging initialization with tracing
- Async runtime setup with tokio

**api.rs**: Homebrew JSON API integration
- Formula and cask data fetching
- Data structure definitions matching Homebrew API schema
- HTTP client with timeout and error handling
- Asynchronous API operations

**cache.rs**: Local package index management
- Formula and cask index persistence
- Cache directory management
- JSON serialization/deserialization
- Cache invalidation logic

**bottle.rs**: Binary package handling
- Bottle download with streaming
- SHA256 checksum verification
- Tarball extraction
- Platform detection (macOS version, architecture)
- Homebrew prefix detection
- GHCR authentication

**builder.rs**: Source compilation orchestration
- Build system detection (Autotools, CMake, Meson, Make)
- Parallel compilation with CPU core detection
- ccache integration when available
- Source download and SHA256 verification
- Build failure reporting with error context

**cask.rs**: macOS GUI application management
- DMG mounting and extraction
- PKG installation
- App bundle copying
- Platform-specific operations (macOS only)
- Cask state tracking

**formula_parser.rs**: Ruby formula parsing
- Extract metadata from formula files
- Parse install blocks for build instructions
- Detect build system heuristically
- Extract configure arguments and dependencies
- Version extraction from source URLs

**tap.rs**: Custom tap management
- Tap registration and Git cloning
- Formula directory discovery
- Tap update via git pull
- Formula loading from Ruby files
- Tap state persistence

**install.rs**: Installation state and symlink management
- Installation mode detection (user vs global)
- Package state persistence
- Symlink creation and removal
- Directory structure validation
- Install location management

**deps.rs**: Dependency resolution
- Dependency graph construction
- Topological sorting
- Circular dependency detection
- Transitive dependency resolution
- Filter already-installed packages

**lockfile.rs**: Reproducible environments
- Lockfile generation from installed state
- TOML serialization
- Version pinning
- Platform-specific bottle references

**ui.rs**: Terminal user interface
- Progress bars with indicatif
- Status messages
- Error formatting
- Interactive prompts with inquire

**error.rs**: Error handling
- Typed error variants with thiserror
- Error context and propagation
- User-friendly error messages

### Command Modules

**commands/search.rs**: Package discovery
- Fuzzy search across formulae and casks
- Case-insensitive matching
- Results from cached index

**commands/info.rs**: Package information display
- Formula/cask details
- Dependencies
- Bottle availability
- Version information

**commands/install.rs**: Package installation orchestration
- Parallel dependency resolution
- Multi-package download coordination
- Progress tracking
- Dry-run support
- Install mode selection

**commands/uninstall.rs**: Package removal
- Dependency checking
- Symlink removal
- Cellar cleanup
- Interactive confirmation
- Dry-run support

**commands/upgrade.rs**: Package updates
- Version comparison
- Uninstall old version
- Install new version
- Dry-run support

**commands/update.rs**: Index synchronization
- Concurrent API fetching
- Cache update
- Progress indication

**commands/list.rs**: Installed package enumeration
- Read Homebrew Cellar
- Display installed packages
- Version information

**commands/lock.rs**: Lockfile generation
- Collect installed package state
- Generate TOML lockfile
- Include version and platform info

**commands/sync.rs**: Lockfile-based installation
- Parse lockfile
- Install exact versions
- Reproducible environment setup

**commands/tap.rs**: Tap management
- Add taps (clone from GitHub)
- Remove taps (delete local clone)
- List installed taps
- Update taps (git pull)

## Data Flow

### Installation Flow

1. User executes `wax install <formula>`
2. Load cached formula index from JSON API and custom taps
3. Find formula by name (supports tap/formula syntax)
4. Resolve dependencies with topological sort
5. Filter already-installed packages
6. Detect install mode (user vs global)
7. For each package:
   - If bottle available and not --build-from-source:
     - Download bottle in parallel (max 8 concurrent)
     - Authenticate with GHCR
     - Stream download with progress bar
     - Verify SHA256 checksum
     - Extract to temporary directory
   - If bottle unavailable or --build-from-source:
     - Fetch Ruby formula file
     - Parse build metadata
     - Download source tarball
     - Verify source SHA256
     - Extract to build directory
     - Detect build system
     - Execute build (configure → compile → install)
8. Copy to Cellar directory structure
9. Create symlinks to bin/lib/include directories
10. Update installation state
11. Report success

### Update Flow

1. User executes `wax update`
2. Fetch formulae from Homebrew JSON API
3. Fetch casks from Homebrew JSON API
4. Save to local cache
5. Report count and timing

### Search Flow

1. User executes `wax search <query>`
2. Load cached formulae from JSON API
3. Load formulae from installed custom taps
4. Load cached casks
5. Merge and deduplicate results
6. Filter by name/description (case-insensitive)
7. Display results

## Platform Support

### macOS

- Primary platform
- Full feature support
- Cask installation (DMG, PKG, ZIP)
- Bottle installation
- Homebrew prefix detection
- Version-specific bottles

### Linux

- Formula installation only
- Linuxbrew detection
- Linux-specific bottles (x86_64_linux, aarch64_linux)
- No cask support (platform-gated)
- XDG Base Directory specification

### Platform Detection

```
macOS ARM: arm64_sequoia, arm64_sonoma, arm64_ventura
macOS Intel: sequoia, sonoma, ventura
Linux x86_64: x86_64_linux
Linux ARM: aarch64_linux
```

## Concurrency Model

### Async Runtime

- Tokio with full features
- Multi-threaded work-stealing scheduler
- Non-blocking I/O for all operations

### Parallel Downloads

- Semaphore-limited concurrency (max 8)
- Independent progress bars per download
- Fail-fast on checksum mismatch
- Streaming to avoid memory pressure

### Error Handling

- Async Result propagation
- Context-rich error messages
- Graceful degradation
- User-actionable error text

## File System Layout

### Cache Directory

```
~/.cache/wax/                    (Linux)
~/Library/Caches/wax/            (macOS)
  formulae.json                  (~8,100 formulae)
  casks.json                     (~7,500 casks)
  metadata.json                  (cache timestamps)
  logs/
    wax.log                      (structured logs)
```

### Tap Directory

```
~/.local/share/wax/taps/         (Linux)
~/Library/Application Support/wax/taps/ (macOS)
  user/
    homebrew-repo/               (Git clone of tap)
      Formula/
        package1.rb
        package2.rb
  taps.json                      (tap registry)
```

### Data Directory

```
~/.local/share/wax/              (Linux)
~/Library/Application Support/wax/ (macOS)
  installed.json                 (formula state)
  installed_casks.json           (cask state)
```

### Installation Directories

**Global Mode:**
```
/opt/homebrew/                   (macOS ARM default)
/usr/local/                      (macOS Intel / Linux default)
/home/linuxbrew/.linuxbrew/      (Linuxbrew)
  Cellar/
    <formula>/
      <version>/
        bin/
        lib/
        include/
        share/
  bin/                           (symlinks)
  lib/                           (symlinks)
  include/                       (symlinks)
```

**User Mode:**
```
~/.local/wax/
  Cellar/
    <formula>/
      <version>/
        ...
  bin/
  lib/
  include/
```

## Performance Optimizations

### Fast Queries

- In-memory index after first load
- No git operations
- No Ruby interpreter startup
- Native string matching
- Compiled binary execution

### Fast Updates

- Single JSON API request
- Concurrent formulae/cask fetching
- Streaming JSON parsing
- Efficient cache serialization

### Fast Installs

- Parallel bottle downloads
- Concurrent checksum verification
- Streaming extraction
- Minimal filesystem operations
- No post-install scripts (trade-off for speed)

### Source Building

- CPU core detection for parallel compilation
- ccache integration when available
- Build system auto-detection
- Incremental build support (Ninja for CMake/Meson)
- Isolated build directories (tempdir)

## Security

### Checksum Verification

- SHA256 validation for all bottles
- Checksums from official Homebrew API
- Download rejection on mismatch
- Prevents corrupted/tampered packages

### GHCR Authentication

- Anonymous token request
- Scoped repository access
- Token per download session
- No stored credentials

### Safe Symlinks

- Validate target existence
- Check for conflicts before creation
- Safe removal (only remove owned symlinks)
- No overwrites without confirmation

## Limitations

### Current Limitations

1. Build system detection is heuristic-based and may fail for complex configurations
2. No binary relocation (@@HOMEBREW_CELLAR@@ placeholders)
3. No post-install script execution
4. No caveats display
5. Ruby formula parser supports common patterns but not advanced DSL features
6. No patch application during source builds

### Design Trade-offs

**Speed vs Compatibility**: Chose speed by skipping source builds and complex formula logic. Covers 95%+ of use cases.

**Simplicity vs Features**: Minimal state tracking and no service management. Users can fall back to brew for edge cases.

**Safety vs Performance**: Always verify checksums and validate symlinks, even at cost of extra I/O.

## Extension Points

### Future Enhancements

1. Binary relocation support (install_name_tool on macOS)
2. Post-install script execution (sandboxed)
3. Patch application during source builds
4. Advanced Ruby DSL parsing (conditions, variables)
5. HTTP caching (ETag, If-Modified-Since)
6. Partial index updates (delta fetching)
7. Distributed caching (CDN for index)
8. Build caching and reuse across installations
9. Tap formula caching (avoid re-parsing Ruby files)

## Development Guidelines

### Code Organization

- One module per concern
- Public API in module root
- Internal helpers as private functions
- Commands isolated in commands/ directory

### Error Handling

- Use Result<T> for all fallible operations
- Add context with specific error variants
- Provide user-actionable error messages
- Log errors with tracing for debugging

### Testing Strategy

- Integration tests for CLI commands
- Unit tests for core algorithms (deps, platform detection)
- Mock HTTP responses for API tests
- Cross-platform testing (macOS, Linux)

### Performance Considerations

- Async for all I/O operations
- Batch operations where possible
- Stream large downloads
- Minimize allocations
- Profile before optimizing
