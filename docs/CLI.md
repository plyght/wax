# Command-Line Interface Reference

## Overview

Wax provides a fast, modern command-line interface for package management. All commands support standard Unix conventions and provide clear, actionable feedback.

## Global Options

```bash
wax [OPTIONS] <COMMAND>
```

### Options

`--verbose, -v`
Enable verbose logging. Writes detailed logs to cache directory.

`--time-to-action`, `--tta`, `--time`
Show command duration in result output.

`--help, -h`
Display help information for wax or specific commands.

`--version, -V`
Display version information.

## Commands

### update

Update the local formula and cask index from Homebrew's JSON API.

```bash
wax update
```

**Behavior:**
- Fetches all formulae (approximately 8,100) from Homebrew API
- Fetches all casks (approximately 7,500) from Homebrew API
- Saves to local cache for offline access
- Displays progress and timing information

**Performance:** Typically completes in 2-4 seconds with good network connection.

**Cache Location:**
- macOS: `~/Library/Caches/wax/`
- Linux: `~/.cache/wax/`

### search

Search for formulae and casks by name or description.

```bash
wax search <query>
wax s <query>           # Shorthand
wax find <query>        # Alias
```

**Arguments:**
- `<query>`: Search term (case-insensitive)

**Examples:**
```bash
wax search nginx
wax search "web server"
wax s python
```

**Behavior:**
- Searches formula names and descriptions
- Searches cask names and descriptions
- Returns all matches (no limit)
- Case-insensitive matching
- Uses cached index (offline capable)

**Output:**
```
Formulae (4):
  nginx         - HTTP and reverse proxy server
  nginx-full    - HTTP server with additional modules
  openresty     - Scalable Web Platform by Extending Nginx
  tengine       - Drop-in replacement for Nginx

Casks (1):
  nginx         - Nginx GUI for macOS
```

### info

Display detailed information about a formula or cask.

```bash
wax info <name>
wax show <name>         # Alias
```

**Arguments:**
- `<name>`: Formula or cask name

**Examples:**
```bash
wax info nginx
wax info --cask firefox
wax show tree
```

**Behavior:**
- Shows version, description, homepage
- Lists dependencies
- Shows bottle availability for current platform
- Indicates if already installed

**Output:**
```
nginx 1.25.3
HTTP and reverse proxy server
https://nginx.org/

Dependencies: pcre2, openssl@3
Bottle: Available for arm64_sonoma
Status: Not installed
```

### list

List all installed packages.

```bash
wax list
wax ls              # Shorthand
```

**Behavior:**
- Reads from Homebrew Cellar directory
- Shows formula name and version
- Detects both Homebrew and Wax installations
- Sorts alphabetically

**Output:**
```
Installed packages (3):
  jq 1.7.1
  oniguruma 6.9.9
  tree 2.1.1
```

### provenance

Inspect local executable origins and PATH precedence, record observations, or select an already-installed Wax formula through a user entrypoint.

```bash
wax provenance inspect NAME [--json]
wax provenance inspect ABSOLUTE_PATH [--json]
wax provenance record ABSOLUTE_PATH
wax provenance replace ABSOLUTE_PATH --with INSTALLED_FORMULA [--dry-run]
wax provenance restore ABSOLUTE_PATH [--dry-run]
```

**Subcommands:**

`inspect NAME [--json]`
Inspect matching entrypoints in PATH order. Marks the first executable candidate as active, including one whose hash cannot be read. An absolute path inspects only that entrypoint, without assigning PATH precedence. Relative paths are not accepted.

`record ABSOLUTE_PATH`
Save an observation and same-name PATH candidates. This is observational adoption, not an ownership takeover. External observations live in separate `~/.wax/provenance/*.json` records, not `installed.json`, and do not make external packages Wax-installed packages.

`replace ABSOLUTE_PATH --with INSTALLED_FORMULA [--dry-run]`
Select a formula that is already installed and recorded in Wax state. The entrypoint basename must exist as an executable in the formula's `bin` directory. Wax creates a symlink to the stable `<prefix>/opt/<formula>/bin/<name>` path after checking that it resolves to the recorded installed version.

`restore ABSOLUTE_PATH [--dry-run]`
Restore the preserved entrypoint, or remove the Wax selection if the entrypoint was originally absent. Refuses intervening changes to the entrypoint or backup rather than overwriting them. Restore each replacement before uninstalling its target formula with Wax.

**Options:**
- `--json`: Print inspection results as a JSON array instead of text.
- `--with INSTALLED_FORMULA`: Required replacement target. Does not install a package.
- `--dry-run`: Validate and preview replacement or restoration without changing files or records.

**Output:**
- Text inspection shows active/candidate status, PATH index, entrypoint, resolved executable, manager evidence, and version hint.
- JSON inspection includes `entrypoint`, `symlinks`, `executable`, `executable_sha256`, `origin`, `status`, `path_index`, and `active`.
- Record, replace, and restore print a JSON record with observations, PATH candidates, and replacement state when present.

**Safety and Limits:**
- Inspection is read-only. It never runs discovered executables or package managers. Receipts and directory layouts associate names and versions; they do not authenticate executable bytes. A SHA256 observation is not proof of origin.
- Record, replace, and restore are supported only on macOS and Linux.
- Replacement requires an absolute entrypoint under canonical HOME, with no symlinked parents. Its parent must exist, be user-owned, and not be group- or world-writable. An existing entrypoint must also be user-owned.
- Existing files or symlinks are preserved in a sibling `.wax-<path-hash>-backup` until restore. Wax refuses to overwrite an existing backup.
- Known Cargo, Bun, npm, pnpm, Node, and Wax manager-owned entries are refused, including symlinks into recognized manager layouts. A new entrypoint in a separate user directory can select Wax without modifying manager files. Its effect depends on the existing PATH order.
- An unknown origin is not proof that an entrypoint is unmanaged. Inspect and dry-run the exact entrypoint before explicitly replacing it; prefer a new user shim when ownership is uncertain.
- No `provenance` command changes global PATH or uninstalls a foreign package. Package-management migration is a separate [`tools` lifecycle](#tools).
- Observations are snapshots. Replacement and restore serialize cooperating Wax operations and recheck entrypoint/backup identities, but do not form an atomic transaction with other managers or protect against a hostile process running as the same user.
- Detection uses local receipts and recognized layouts, including `CARGO_HOME`, `BUN_INSTALL`, and known Wax Cellar roots. Custom global Wax prefixes need `HOMEBREW_PREFIX` for inspection; unrecognized layouts or missing receipts can leave origins unknown or ambiguous.
- PATH precedence describes the current process environment, not shell aliases, functions, or cached command resolution. Unix executable checks use mode bits, not effective-user ACL evaluation.
- On Windows, inspection checks the exact name in PATH without PATHEXT expansion. Executable recognition is limited to `.exe`, `.com`, `.cmd`, and `.bat`; it does not reproduce full Windows shell resolution.

### tools

Transfer management of a supported, already-installed Cargo, npm, or Bun package to Wax. Unlike `provenance record` or `provenance replace`, migration removes the selected package from its original manager's installation and updates that manager's receipts. Wax then owns upgrades and removal. The runtime, toolchain, and package-manager backend remain installed.

```bash
wax tools migrate ENTRY --from cargo|npm|bun --root ABS [--dry-run]
wax tools list
wax tools upgrade ID --version VERSION [--artifact ABS] [--dry-run]
wax tools remove ID [--dry-run]
wax tools restore ID [--dry-run]
```

`cargo|npm|bun` denotes a choice of one manager. `ID` is the ecosystem-qualified package name, not necessarily the executable name: `cargo/NAME`, `npm/NAME`, or `bun/NAME`. Scoped Node packages use `npm/@SCOPE/NAME` or `bun/@SCOPE/NAME`.

**Subcommands:**

`migrate ENTRY --from MANAGER --root ABS [--dry-run]`
Discover the package that owns the absolute `ENTRY` directly inside `ABS/bin`. The root must be an existing absolute canonical directory under HOME, without symlinked parents, unsafe permissions, or elevated privileges. Cargo uses its install root, npm uses its global prefix, and Bun uses its install root containing `install/global` and `bin`. Conflicting Bun global-directory or bin overrides are refused.

Migration transfers the complete receipted bin set, not only `ENTRY`. Wax verifies and copies the installed payload into a private generation, preserving original artifacts and manager receipts for restoration. The native manager calculates removal and receipt changes in an isolated projection of the source installation. Wax refuses a plan that changes unrelated projected package content before touching the source. In particular, Bun removal never runs against the live root. Wax applies the verified selected-package removal and receipt changes to the source, then creates entrypoints in `~/.local/wax/bin`.

`migrate --dry-run` prints the discovered package as JSON and checks ownership and entrypoint conflicts. It does not import payloads, snapshot originals, or execute the removal plan, so it does not prove that native removal will succeed. Node discovery can run `npm ls` for receipt corroboration.

`list`
Print each migration record's qualified ID, selected generation's version, and lifecycle state. Inactive records retain the last recorded version. This list is separate from the Homebrew-oriented `wax list` and includes retained removed or restored records.

`upgrade ID --version VERSION [--artifact ABS] [--dry-run]`
Upgrade an active migrated package to an exact SemVer, not a version range or `latest`. Wax installs into a new private generation with the selected ecosystem backend, verifies the resulting name, version, and unchanged bin set, and switches its current pointer. Published generations are treated as immutable rather than updated in place. Prior generations remain until removal or restoration.

For Cargo, `--artifact` is an existing absolute source directory whose `Cargo.toml` declares the requested package name and version. Wax runs `cargo install --path` with `--offline --locked`, so dependencies must already be available locally. Without an artifact, Cargo installs the exact version from crates.io with `--locked`, even if the original installation came from a path.

For npm or Bun, `--artifact` is an absolute local `.tgz` containing `package/package.json` with the requested name and version. Archive entries must be regular files or directories under `package`, without traversal, links, or native addons. Without an artifact, Wax passes `NAME@VERSION` to the backend. npm uses offline mode and a fresh private cache, so registry-only upgrades generally cannot resolve packages: a local `.tgz` is required in practice. Bun can resolve a registry package with its backend. Node lifecycle scripts are disabled.

`upgrade --dry-run` checks the active record and Wax links and prints the requested version change. It does not install, validate the artifact, or check backend resolution.

`remove ID [--dry-run]`
Remove the Wax entrypoints, current pointer, and verified private generations. Keep the original-manager rollback snapshots and migration record. Removal does not restore the original installation. Changed payloads or conflicting entrypoints are refused rather than deleted. Dry-run checks these conditions without removing files.

`restore ID [--dry-run]`
Restore the original package artifacts and manager receipts from their verified snapshots, then remove the Wax selection and verified generations. Restore works after `remove` as well as while active. It restores the original version, not the last upgraded version. If source artifacts or shared manager receipts drift from the recorded states, Wax refuses to overwrite them. Installing or removing another package with the original manager can therefore prevent restoration. Dry-run validates the snapshots, source state, and links without restoring files.

**Supported sources:**
- Cargo: packages claimed unambiguously by `.crates2.json`, `.crates.toml`, or both. When both receipts exist, they must agree. Only crates.io registry receipts and absolute `path+file://` sources are accepted, not Git or alternate registries. All claimed binaries must be safe regular executable files.
- npm: global packages under `ROOT/lib/node_modules`, corroborated by `npm ls --global --depth=0 --json`.
- Bun: global packages under `ROOT/install/global/node_modules`, corroborated by the global `package.json` and text `bun.lock`. A binary lock alone is insufficient. Local tarball references also require a matching artifact manifest and SHA512 lock integrity.
- Node packages may be scoped or unscoped, with a string or object `bin` mapping. Every mapped global executable must have a matching symlink in `ROOT/bin`. Symlinked package directories, escaping links, incomplete bin sets, and extra unreceipted outer bin links are refused.
- Node support is restricted to dependency-free packages. Any declaration of `dependencies`, `optionalDependencies`, `peerDependencies`, `bundledDependencies`, `bundleDependencies`, or `workspaces` is refused, even if empty. Nested `node_modules`, `.node` addons, `binding.gyp`, and a `gypfile` value other than `false` are unsupported.
- Node manifests with any of these hooks are refused: `preinstall`, `install`, `postinstall`, `preprepare`, `prepare`, `postprepare`, `preuninstall`, `uninstall`, `postuninstall`, or `prepublish`. Native builds are unsupported. Runtime and package-manager names are refused as packages or bin names.

**Safety and limits:**
- All `tools` commands require macOS or Linux. pnpm, Yarn, Windows layouts, and runtime or toolchain migration are unsupported.
- Do not run foreign package managers concurrently with migration, upgrade, removal, or restoration. Wax's lifecycle lock coordinates Wax operations, not external managers. Snapshots and identity checks are not an atomic transaction with another manager or protection from a hostile process under the same user.
- State, snapshots, and generations live separately under `~/.wax/tools`, not in the formula installation state. Existing Wax bin entries and an existing migration record block a new migration. A restored record is retained, so restore does not by itself enable remigration of the same ID.
- Wax retains the runtime and manager backend rather than replacing them. Later upgrades still require a safe ecosystem backend on PATH. Imported Node scripts still require their runtime.
- Backends must be trusted-owned executable files, without group/world write permissions or unsafe parents. Trusted-owned children beneath sticky directories are allowed. Wax reports the rejected path and permissions rather than silently changing them.
- No command edits global PATH. Wax entrypoints take effect only if `~/.local/wax/bin` is already on PATH in the intended position. Current-shell aliases, functions, and cached command resolution are outside this lifecycle's scope.

### install

Install a formula or cask with all dependencies.

```bash
wax install <name> [OPTIONS]
wax i <name> [OPTIONS]        # Shorthand
wax add <name> [OPTIONS]      # Alias
```

**Arguments:**
- `<name>`: Formula or cask name to install. Supports tap-qualified names (user/repo/formula)

**Options:**

`--dry-run`
Show what would be installed without making changes.

`--cask`
Install as cask (GUI application) instead of formula.

`--user`
Install to user-local directory (~/.local/wax). No sudo required.

`--global`
Install to system directory. May require sudo.

`--build-from-source`
Force compilation from source even if bottle is available. Useful for custom builds or when bottles are outdated.

`--no-script`
Skip automatic post-install scripts.

**Examples:**
```bash
wax install tree
wax install jq --dry-run
wax install --cask iterm2
wax install nginx --user
wax install nginx --build-from-source
wax install nginx --no-script
wax install user/tap/custom-package
wax i -v ripgrep
```

**Behavior:**
1. Loads formula from cache and custom taps
2. Resolves all dependencies with topological sort
3. Filters already-installed packages
4. Detects install mode (user vs global)
5. For each package:
   - If bottle available and not --build-from-source: downloads bottles in parallel (max 8 concurrent)
   - If bottle unavailable or --build-from-source: builds from source with detected build system
6. Verifies SHA256 checksums (bottle or source)
7. Extracts to Cellar directory
8. Creates symlinks to bin/lib/include
9. Updates installation state

**Progress Display:**
```
Installing jq with 1 dependency
  Packages: oniguruma, jq

[>] oniguruma 6.9.9  [████████████████████] 658 KB/658 KB @ 2.1 MB/s
[✓] jq 1.7.1         [████████████████████] 1.2 MB/1.2 MB @ 3.4 MB/s

✓ Installed jq in 0.8s
```

**Install Modes:**

Global mode (default):
- Installs to system Homebrew prefix
- Requires write permissions
- `/opt/homebrew` (macOS ARM)
- `/usr/local` (macOS Intel, Linux)
- `/home/linuxbrew/.linuxbrew` (Linuxbrew)

User mode (--user):
- Installs to `~/.local/wax`
- No sudo required
- Fully isolated from system

**Error Handling:**
- Clear error messages for missing bottles
- Checksum verification failures
- Permission issues
- Network errors

### uninstall

Remove an installed formula or cask.

```bash
wax uninstall <name> [OPTIONS]
wax rm <name> [OPTIONS]         # Shorthand
wax remove <name> [OPTIONS]     # Alias
wax delete <name> [OPTIONS]     # Alias
```

**Arguments:**
- `<name>`: Formula or cask name to remove

**Options:**

`--dry-run`
Show what would be removed without making changes.

`--cask`
Uninstall cask instead of formula.

**Examples:**
```bash
wax uninstall tree
wax rm jq --dry-run
wax uninstall --cask iterm2
```

For migrated tools, `wax uninstall ID [--dry-run]` routes qualified `cargo/`, `npm/`, and `bun/` IDs to `wax tools remove`. It retains the original-manager rollback snapshot and does not prompt for formula confirmation. Migrated tools cannot be mixed with formulae or casks, `--cask`, or `--all` in the same removal request.

**Behavior:**
1. Checks if package is installed
2. Warns if package is dependency of others
3. Prompts for confirmation (unless dry-run)
4. Removes symlinks from bin/lib/include
5. Removes from Cellar directory
6. Updates installation state

**Interactive Confirmation:**
```
Uninstall oniguruma 6.9.9?
Warning: The following packages depend on this package:
  - jq 1.7.1

Are you sure? (y/N)
```

### upgrade

Upgrade an installed formula to the latest version.

```bash
wax upgrade <name> [OPTIONS]
wax up <name> [OPTIONS]         # Shorthand
```

**Arguments:**
- `<name>`: Formula name to upgrade

**Options:**

`--dry-run`
Show what would be upgraded without making changes.

**Examples:**
```bash
wax upgrade nginx
wax up tree --dry-run
```

For migrated tools, `wax upgrade ID@VERSION [--dry-run]` requires an exact version and routes qualified `cargo/`, `npm/`, and `bun/` IDs to the tools lifecycle. It does not select a latest version. Use `wax tools upgrade ID --version VERSION --artifact ABS` for local artifacts, particularly npm tarballs. Migrated tools cannot be mixed with formulae, system packages, or `--system`, `--user`, `--global`, or `--ask` in the same upgrade request.

**Behavior:**
1. Checks installed version
2. Fetches latest version from cache
3. Compares versions
4. If outdated:
   - Uninstalls old version
   - Installs new version
5. If up-to-date, reports no action needed

**Output:**
```
Upgrading nginx: 1.25.2 → 1.25.3
[Progress bars for download]
✓ Upgraded nginx to 1.25.3 in 0.5s
```

**Already Up-to-Date:**
```
nginx is already up-to-date (1.25.3)
```

### lock

Generate a lockfile from currently installed packages.

```bash
wax lock
```

**Behavior:**
- Reads installation state
- Collects package names, versions, and platforms
- Generates `wax.lock` in current directory
- TOML format for human readability

**Output File (wax.lock):**
```toml
[packages]
nginx = { version = "1.25.3", bottle = "arm64_sonoma" }
openssl = { version = "3.1.4", bottle = "arm64_sonoma" }
tree = { version = "2.1.1", bottle = "arm64_sonoma" }
```

**Use Case:**
Create reproducible development environments across machines.

### sync

Install packages from lockfile with exact versions.

```bash
wax sync
```

**Behavior:**
1. Reads `wax.lock` from current directory
2. Installs each package at specified version
3. Uses specified bottle platform
4. Skips already-installed matching versions

**Requirements:**
- `wax.lock` must exist in current directory
- Specified versions must be available in Homebrew

**Output:**
```
Syncing from wax.lock
  Found 3 packages

Installing nginx 1.25.3
Installing openssl 3.1.4
Installing tree 2.1.1

✓ Synced 3 packages in 2.1s
```

### tap

Manage custom Homebrew taps for extended package availability.

```bash
wax tap [ACTION]
```

**Subcommands:**

`add <user/repo>`
Clone and register a custom tap from GitHub.

`remove <user/repo>`
Unregister and delete a custom tap.

`list`
Display all installed custom taps.

`update <user/repo>`
Update a tap to latest version (git pull).

**Arguments:**
- `<user/repo>`: Tap identifier in GitHub format (e.g., homebrew/cask-versions)

**Examples:**
```bash
wax tap add homebrew/cask-versions
wax tap list
wax tap update homebrew/cask-versions
wax tap remove homebrew/cask-versions
```

**Behavior:**

For `add`:
1. Validates tap format
2. Clones repository from https://github.com/user/homebrew-repo
3. Registers tap in local state
4. Makes formulae available for search and install

For `remove`:
1. Checks if tap is installed
2. Removes local Git clone
3. Unregisters tap from state

For `list`:
1. Displays all registered taps
2. Shows GitHub URL for each tap

For `update`:
1. Runs git pull in tap directory
2. Refreshes formula metadata

**Tap Formula Usage:**
```bash
wax tap add user/custom
wax search user/custom/package
wax install user/custom/package
```

**Output:**
```
→ Adding tap: user/custom
✓ Added tap user/custom

Installed taps:
  user/custom (https://github.com/user/homebrew-custom.git)
```

## Exit Codes

- `0`: Success
- `1`: General error
- `2`: Command-line usage error
- Other: Specific error codes (future)

## Environment Variables

Currently, Wax does not use environment variables for configuration. All paths are automatically detected based on platform.

**Future:**
- `WAX_CACHE_DIR`: Override cache directory
- `WAX_LOG_LEVEL`: Override log level
- `WAX_HOMEBREW_PREFIX`: Override Homebrew prefix detection

## Logging

Logs are written to:
- macOS: `~/Library/Caches/wax/logs/wax.log`
- Linux: `~/.cache/wax/logs/wax.log`

Enable verbose logging:
```bash
wax --verbose <command>
```

Log format: Structured JSON with timestamps and context.

## Examples

### Daily Workflow

```bash
wax update
wax search ripgrep
wax install ripgrep
wax list
```

### Development Environment Setup

```bash
wax install node
wax install postgresql
wax install redis
wax lock
git add wax.lock
git commit -m "Add dependency lockfile"
```

### On Another Machine

```bash
git clone <repo>
cd <repo>
wax sync
```

### Cleanup

```bash
wax uninstall node
wax uninstall postgresql
wax uninstall redis
```

## Comparison with Homebrew

| Command | Homebrew | Wax |
|---------|----------|-----|
| Update index | `brew update` | `wax update` |
| Search packages | `brew search <query>` | `wax search <query>` |
| Package info | `brew info <formula>` | `wax info <formula>` |
| List installed | `brew list` | `wax list` |
| Install | `brew install <formula>` | `wax install <formula>` |
| Install from source | `brew install --build-from-source <formula>` | `wax install --build-from-source <formula>` |
| Uninstall | `brew uninstall <formula>` | `wax uninstall <formula>` |
| Upgrade | `brew upgrade <formula>` | `wax upgrade <formula>` |
| Add tap | `brew tap <user/repo>` | `wax tap add <user/repo>` |
| Remove tap | `brew untap <user/repo>` | `wax tap remove <user/repo>` |
| List taps | `brew tap` | `wax tap list` |
| Lockfile | N/A | `wax lock` / `wax sync` |

## Shell Completion

Future feature: Generate shell completion scripts for bash, zsh, fish.

```bash
wax completion bash > /etc/bash_completion.d/wax
wax completion zsh > /usr/local/share/zsh/site-functions/_wax
```

## Tips and Tricks

### Faster Searches

Searches are instant after running `wax update` once. The cache persists across restarts.

### Offline Usage

After initial `wax update`, search and info commands work offline.

### Dry-Run Everything

Use `--dry-run` to preview changes before committing:
```bash
wax install nginx --dry-run
wax upgrade --all --dry-run
```

### Parallel Downloads

Wax automatically downloads up to 8 packages simultaneously. No configuration needed.

### User-Local Installs

Use `--user` to avoid sudo:
```bash
wax install --user <formula>
```

Add `~/.local/wax/bin` to PATH:
```bash
export PATH="$HOME/.local/wax/bin:$PATH"
```
