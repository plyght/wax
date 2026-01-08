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

### install

Install a formula or cask with all dependencies.

```bash
wax install <name> [OPTIONS]
wax i <name> [OPTIONS]        # Shorthand
wax add <name> [OPTIONS]      # Alias
```

**Arguments:**
- `<name>`: Formula or cask name to install

**Options:**

`--dry-run`
Show what would be installed without making changes.

`--cask`
Install as cask (GUI application) instead of formula.

`--user`
Install to user-local directory (~/.local/wax). No sudo required.

`--global`
Install to system directory. May require sudo.

**Examples:**
```bash
wax install tree
wax install jq --dry-run
wax install --cask iterm2
wax install nginx --user
wax i -v ripgrep
```

**Behavior:**
1. Loads formula from cache
2. Resolves all dependencies with topological sort
3. Filters already-installed packages
4. Detects install mode (user vs global)
5. Downloads bottles in parallel (max 8 concurrent)
6. Verifies SHA256 checksums
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
| Uninstall | `brew uninstall <formula>` | `wax uninstall <formula>` |
| Upgrade | `brew upgrade <formula>` | `wax upgrade <formula>` |
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
