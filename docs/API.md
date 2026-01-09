# API Reference

## Overview

This document describes the public APIs and data structures in the Wax codebase. These interfaces are stable and intended for internal use across modules.

## Module: api

Homebrew JSON API client and data structures.

### ApiClient

HTTP client for Homebrew's JSON APIs.

```rust
pub struct ApiClient
```

#### Methods

```rust
pub fn new() -> Self
```

Creates a new API client with default timeout (30 seconds).

```rust
pub async fn fetch_formulae(&self) -> Result<Vec<Formula>>
```

Fetches all formulae from https://formulae.brew.sh/api/formula.json.

Returns approximately 8,100 formula definitions.

```rust
pub async fn fetch_casks(&self) -> Result<Vec<Cask>>
```

Fetches all casks from https://formulae.brew.sh/api/cask.json.

Returns approximately 7,500 cask definitions.

```rust
pub async fn fetch_cask_details(&self, cask_name: &str) -> Result<CaskDetails>
```

Fetches detailed information for a specific cask.

### Formula

Represents a Homebrew formula (CLI tool or library).

```rust
pub struct Formula {
    pub name: String,
    pub full_name: String,
    pub desc: Option<String>,
    pub homepage: String,
    pub versions: Versions,
    pub installed: Option<Vec<InstalledVersion>>,
    pub dependencies: Option<Vec<String>>,
    pub build_dependencies: Option<Vec<String>>,
    pub bottle: Option<BottleInfo>,
}
```

### Cask

Represents a Homebrew cask (GUI application).

```rust
pub struct Cask {
    pub token: String,
    pub full_token: String,
    pub name: Vec<String>,
    pub desc: Option<String>,
    pub homepage: String,
    pub version: String,
}
```

### BottleInfo

Bottle availability and download information.

```rust
pub struct BottleInfo {
    pub stable: Option<BottleStable>,
}

pub struct BottleStable {
    pub files: HashMap<String, BottleFile>,
}

pub struct BottleFile {
    pub url: String,
    pub sha256: String,
}
```

## Module: cache

Local formula and cask index management.

### Cache

Manages local cache of formula and cask data.

```rust
pub struct Cache
```

#### Methods

```rust
pub fn new() -> Result<Self>
```

Creates a new cache instance. Automatically determines cache directory based on platform.

```rust
pub async fn ensure_cache_dir(&self) -> Result<()>
```

Creates cache directory if it does not exist.

```rust
pub fn is_initialized(&self) -> bool
```

Checks if cache contains valid data.

```rust
pub async fn save_formulae(&self, formulae: &[Formula]) -> Result<()>
```

Saves formulae to cache.

```rust
pub async fn load_formulae(&self) -> Result<Vec<Formula>>
```

Loads formulae from cache.

```rust
pub async fn save_casks(&self, casks: &[Cask]) -> Result<()>
```

Saves casks to cache.

```rust
pub async fn load_casks(&self) -> Result<Vec<Cask>>
```

Loads casks from cache.

## Module: bottle

Bottle download, verification, and extraction.

### BottleDownloader

Handles bottle download and verification.

```rust
pub struct BottleDownloader
```

#### Methods

```rust
pub fn new() -> Self
```

Creates a new bottle downloader.

```rust
pub async fn download(
    &self,
    url: &str,
    dest_path: &Path,
    progress: Option<&ProgressBar>
) -> Result<()>
```

Downloads a bottle from URL to destination path. Optionally updates progress bar.

```rust
pub fn verify_checksum(path: &Path, expected_sha256: &str) -> Result<()>
```

Verifies bottle SHA256 checksum. Returns error on mismatch.

```rust
pub fn extract(tarball_path: &Path, dest_dir: &Path) -> Result<()>
```

Extracts bottle tarball to destination directory.

### Functions

```rust
pub fn detect_platform() -> String
```

Detects current platform for bottle selection.

Returns values like: `arm64_sonoma`, `x86_64_linux`, etc.

```rust
pub fn homebrew_prefix() -> PathBuf
```

Detects Homebrew installation prefix.

Checks `brew --prefix`, falls back to platform defaults.

## Module: install

Installation state management and symlink handling.

### InstallMode

Determines installation location (user-local or system-wide).

```rust
pub enum InstallMode {
    User,    // ~/.local/wax
    Global,  // System directories
}
```

#### Methods

```rust
pub fn detect() -> Self
```

Automatically detects appropriate install mode based on write permissions.

```rust
pub fn from_flags(user: bool, global: bool) -> Result<Option<Self>>
```

Creates install mode from command-line flags. Returns error if both flags set.

```rust
pub fn validate(&self) -> Result<()>
```

Validates that install mode is usable (directory writable).

```rust
pub fn prefix(&self) -> PathBuf
```

Returns installation prefix for this mode.

```rust
pub fn cellar_path(&self) -> PathBuf
```

Returns Cellar directory path.

```rust
pub fn bin_path(&self) -> PathBuf
```

Returns bin directory path for symlinks.

### InstalledPackage

Represents an installed package.

```rust
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub install_date: i64,
    pub install_mode: InstallMode,
}
```

### Functions

```rust
pub async fn create_symlinks(
    formula_name: &str,
    version: &str,
    cellar_path: &Path,
    dry_run: bool,
    install_mode: InstallMode
) -> Result<Vec<PathBuf>>
```

Creates symlinks for installed formula. Returns list of created symlinks.

```rust
pub async fn remove_symlinks(
    formula_name: &str,
    version: &str,
    cellar_path: &Path,
    dry_run: bool,
    install_mode: InstallMode
) -> Result<Vec<PathBuf>>
```

Removes symlinks for formula. Returns list of removed symlinks.

## Module: deps

Dependency resolution algorithms.

### DependencyGraph

Directed graph for dependency resolution.

```rust
pub struct DependencyGraph
```

#### Methods

```rust
pub fn new() -> Self
```

Creates an empty dependency graph.

```rust
pub fn add_node(&mut self, name: String, deps: Vec<String>)
```

Adds a node with its dependencies.

```rust
pub fn topological_sort(&self) -> Result<Vec<String>>
```

Returns dependencies in installation order. Returns error on cycles.

### Functions

```rust
pub fn resolve_dependencies(
    formula: &Formula,
    formulae: &[Formula],
    installed: &HashSet<String>
) -> Result<Vec<String>>
```

Resolves all dependencies for a formula. Returns installation order, excluding already-installed packages.

## Module: lockfile

Lockfile generation and parsing for reproducible environments.

### Lockfile

Represents a wax.lock file.

```rust
pub struct Lockfile {
    pub packages: HashMap<String, LockfilePackage>,
}
```

#### Methods

```rust
pub async fn generate() -> Result<Self>
```

Generates lockfile from currently installed packages.

```rust
pub async fn save(&self, path: &Path) -> Result<()>
```

Saves lockfile to disk in TOML format.

```rust
pub async fn load(path: &Path) -> Result<Self>
```

Loads lockfile from disk.

```rust
pub fn default_path() -> PathBuf
```

Returns default lockfile path (./wax.lock).

### LockfilePackage

Represents a locked package version.

```rust
pub struct LockfilePackage {
    pub version: String,
    pub bottle: String,  // Platform identifier
}
```

## Module: error

Error types for the Wax package manager.

### WaxError

All errors in Wax.

```rust
pub enum WaxError {
    HttpError(reqwest::Error),
    JsonError(serde_json::Error),
    IoError(std::io::Error),
    FormulaNotFound(String),
    CaskNotFound(String),
    CacheError(String),
    HomebrewNotFound,
    ChecksumMismatch { expected: String, actual: String },
    BottleNotAvailable(String),
    DependencyCycle(String),
    InstallError(String),
    NotInstalled(String),
    LockfileError(String),
    PlatformNotSupported(String),
    BuildError(String),
    ParseError(String),
    TapError(String),
}
```

Each variant provides context-specific error information.

### Result Type

```rust
pub type Result<T> = std::result::Result<T, WaxError>;
```

Standard Result type used throughout Wax.

## Module: ui

Terminal UI components.

### Functions

```rust
pub fn create_progress_bar(total_size: u64) -> ProgressBar
```

Creates a progress bar for downloads.

```rust
pub fn create_spinner(message: &str) -> ProgressBar
```

Creates a spinner for indeterminate operations.

```rust
pub fn success_message(message: &str)
```

Prints a success message with green checkmark.

```rust
pub fn error_message(message: &str)
```

Prints an error message with red cross.

```rust
pub fn info_message(message: &str)
```

Prints an info message with blue arrow.

## Module: builder

Source compilation orchestration.

### Builder

Handles compilation from source with multi-build-system support.

```rust
pub struct Builder
```

#### Methods

```rust
pub fn new() -> Self
```

Creates a new builder instance. Detects CPU cores and ccache availability.

```rust
pub async fn build_from_source(
    &self,
    formula: &ParsedFormula,
    source_tarball: &Path,
    build_dir: &Path,
    install_prefix: &Path,
    progress: Option<&ProgressBar>
) -> Result<()>
```

Builds a formula from source. Automatically detects build system and executes appropriate workflow.

**Supported Build Systems:**
- Autotools (./configure && make && make install)
- CMake (cmake -B build && cmake --build build)
- Meson (meson setup build && ninja -C build)
- Make (make && make install)

**Features:**
- Parallel compilation using detected CPU cores
- ccache integration when available
- Ninja preference for CMake/Meson when available
- Build failure diagnostics

## Module: formula_parser

Ruby formula parsing and metadata extraction.

### FormulaParser

Parses Homebrew Ruby formula files to extract build metadata.

```rust
pub struct FormulaParser
```

#### Methods

```rust
pub fn parse_ruby_formula(name: &str, ruby_content: &str) -> Result<ParsedFormula>
```

Parses a Ruby formula file and extracts metadata.

**Extracted Information:**
- Source URL and SHA256
- Description, homepage, license
- Runtime and build dependencies
- Build system detection
- Configure arguments
- Install commands

```rust
pub async fn fetch_formula_rb(formula_name: &str) -> Result<String>
```

Fetches a Ruby formula file from Homebrew's GitHub repository.

### ParsedFormula

Represents parsed formula metadata.

```rust
pub struct ParsedFormula {
    pub name: String,
    pub desc: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub source: FormulaSource,
    pub runtime_dependencies: Vec<String>,
    pub build_dependencies: Vec<String>,
    pub build_system: BuildSystem,
    pub install_commands: Vec<String>,
    pub configure_args: Vec<String>,
}
```

### BuildSystem

Enumeration of supported build systems.

```rust
pub enum BuildSystem {
    Autotools,
    CMake,
    Meson,
    Make,
    Unknown,
}
```

## Module: tap

Custom tap management for third-party formula sources.

### Tap

Represents a Homebrew tap.

```rust
pub struct Tap {
    pub user: String,
    pub repo: String,
    pub full_name: String,
    pub url: String,
    pub path: PathBuf,
}
```

#### Methods

```rust
pub fn new(user: &str, repo: &str) -> Self
```

Creates a tap reference for user/repo.

```rust
pub fn is_installed(&self) -> bool
```

Checks if tap is cloned locally.

```rust
pub fn formula_dir(&self) -> PathBuf
```

Returns path to tap's Formula directory.

### TapManager

Manages tap registration and formula loading.

```rust
pub struct TapManager
```

#### Methods

```rust
pub fn new() -> Result<Self>
```

Creates a tap manager and loads tap registry.

```rust
pub async fn add_tap(&mut self, user: &str, repo: &str) -> Result<()>
```

Clones a tap from GitHub and registers it.

```rust
pub async fn remove_tap(&mut self, user: &str, repo: &str) -> Result<()>
```

Unregisters and removes a tap.

```rust
pub async fn update_tap(&mut self, user: &str, repo: &str) -> Result<()>
```

Updates a tap via git pull.

```rust
pub fn list_taps(&self) -> Vec<&Tap>
```

Returns all registered taps.

```rust
pub async fn load_formulae_from_tap(&self, tap: &Tap) -> Result<Vec<Formula>>
```

Loads and parses all formulae from a tap.

```rust
pub async fn load_all_tap_formulae(&self) -> Result<Vec<Formula>>
```

Loads formulae from all registered taps.

```rust
pub fn resolve_formula_name(&self, formula_name: &str) -> Option<(&Tap, String)>
```

Resolves a tap-qualified formula name (user/repo/formula) to a tap and formula name.

## Module: cask

Cask installation handling (macOS only).

### CaskInstaller

Handles cask installation operations.

```rust
pub struct CaskInstaller
```

#### Methods

```rust
pub fn new() -> Self
```

Creates a new cask installer.

```rust
pub async fn install(
    &self,
    cask: &CaskDetails,
    dry_run: bool
) -> Result<()>
```

Installs a cask. Automatically detects installer type (DMG, PKG, ZIP).

```rust
pub async fn uninstall(
    &self,
    cask_name: &str,
    dry_run: bool
) -> Result<()>
```

Uninstalls a cask by removing from /Applications.

Platform-specific: All cask operations return `PlatformNotSupported` error on non-macOS systems.

## Command Modules

Command modules implement CLI operations. Each exports a single public function.

### commands::search

```rust
pub async fn search(
    api_client: &ApiClient,
    cache: &Cache,
    query: &str
) -> Result<()>
```

Searches formulae and casks by name or description.

### commands::info

```rust
pub async fn info(
    api_client: &ApiClient,
    cache: &Cache,
    formula: &str
) -> Result<()>
```

Displays detailed information about a formula or cask.

### commands::install

```rust
pub async fn install(
    cache: &Cache,
    packages: &[String],
    dry_run: bool,
    cask: bool,
    user: bool,
    global: bool,
    build_from_source: bool
) -> Result<()>
```

Installs one or more formulae or casks with dependencies. Supports tap-qualified names.

### commands::uninstall

```rust
pub async fn uninstall(
    cache: &Cache,
    formula: &str,
    dry_run: bool,
    cask: bool
) -> Result<()>
```

Uninstalls a formula or cask.

### commands::upgrade

```rust
pub async fn upgrade(
    cache: &Cache,
    formula: &str,
    dry_run: bool
) -> Result<()>
```

Upgrades a formula to the latest version.

### commands::update

```rust
pub async fn update(
    api_client: &ApiClient,
    cache: &Cache
) -> Result<()>
```

Updates local formula and cask index.

### commands::list

```rust
pub async fn list() -> Result<()>
```

Lists installed packages.

### commands::lock

```rust
pub async fn lock() -> Result<()>
```

Generates lockfile from installed packages.

### commands::sync

```rust
pub async fn sync(cache: &Cache) -> Result<()>
```

Installs packages from lockfile.

### commands::tap

```rust
pub async fn tap(action: Option<TapAction>) -> Result<()>
```

Manages custom taps. Supports add, remove, list, and update operations.
