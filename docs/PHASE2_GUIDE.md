# Wax Phase 2 - Quick Reference

## Installation Commands

### Install a formula
```bash
wax install <formula>
wax install tree
wax install jq  # installs with dependencies
```

### Install with dry-run
```bash
wax install <formula> --dry-run
wax install nginx --dry-run  # see what would be installed
```

### Uninstall a formula
```bash
wax uninstall <formula>
wax uninstall tree
```

### Uninstall with dry-run
```bash
wax uninstall <formula> --dry-run
```

### Upgrade a formula
```bash
wax upgrade <formula>
wax upgrade tree
```

### Upgrade with dry-run
```bash
wax upgrade <formula> --dry-run
```

## Examples

### Basic workflow
```bash
# Update formula index
wax update

# Search for a formula
wax search tree

# Get info about a formula
wax info tree

# Install it
wax install tree

# Check installed packages
wax list

# Upgrade to latest version
wax upgrade tree

# Uninstall when done
wax uninstall tree
```

### Installing with dependencies
```bash
# jq has dependencies (oniguruma)
wax install jq

# Output:
# → Installing jq with 2 dependencies
#   Packages: oniguruma, jq
# ✓ Installed oniguruma
# ✓ Installed jq
# ✓ Installed jq in 0.3s
```

### Dry-run mode
```bash
# See what would be installed without making changes
wax install nginx --dry-run

# Output:
# → Installing nginx with 3 dependencies
#   Packages: pcre2, openssl@3, nginx
#
# ✓ Dry run - no changes made
```

## Features

- ✅ Parallel downloads (max 8 concurrent)
- ✅ Progress bars for each download
- ✅ SHA256 checksum verification
- ✅ Dependency resolution
- ✅ Symlink management
- ✅ Installation state tracking
- ✅ Dry-run support
- ✅ Interactive confirmations for uninstall

## Installation Locations

Wax uses the same directory structure as Homebrew:
- **Cellar**: `{homebrew_prefix}/Cellar/{formula}/{version}/`
- **Symlinks**: `{homebrew_prefix}/bin/`, `{homebrew_prefix}/lib/`, etc.
- **State**: `~/.wax/installed.json`

Where `{homebrew_prefix}` is:
- Custom location if Homebrew installed (detected via `brew --prefix`)
- `/opt/homebrew` on ARM Macs
- `/usr/local` on Intel Macs

## Performance

Typical install times (network-dependent):
- Simple formula (no deps): ~0.3-0.5s
- Complex formula (with deps): ~0.5-1.0s
- Parallel downloads: 3-5x faster than sequential

## Supported Formulae

Works with:
- ✅ Formulae with bottles for your platform
- ✅ Formulae with dependencies
- ✅ Formulae with "all" platform bottles

Known limitations:
- ⚠️ Some formulae with complex shared library dependencies may not work due to missing binary relocation
- ⚠️ Post-install scripts are not executed
- ⚠️ Installation caveats are not displayed

## Troubleshooting

### "Permission denied" error
Ensure you have write permissions to the Homebrew prefix directory.

### "Bottle not available for platform"
The formula doesn't have a pre-built bottle for your platform. Try a different formula or use Homebrew.

### "Checksum mismatch"
The downloaded bottle doesn't match the expected checksum. This could indicate a network issue or corrupted download. Try again.

### Formula installed but binary doesn't work
Some formulae require binary relocation which wax doesn't currently support. Use Homebrew for these formulae.

## Comparison with Homebrew

| Feature | wax | brew |
|---------|-----|------|
| Update speed | ~2-4s | 15-30s |
| Parallel downloads | ✅ (8 concurrent) | ❌ (sequential) |
| Progress bars | ✅ Modern | ⚠️ Basic |
| Dependency resolution | ✅ | ✅ |
| Building from source | ❌ | ✅ |
| Post-install scripts | ❌ | ✅ |
| Binary relocation | ❌ | ✅ |
| Cask support | ❌ (Phase 4) | ✅ |
| Lockfiles | ❌ (Phase 3) | ❌ |

## Next Steps

Phase 3 will add:
- `wax lock` - Generate lockfile
- `wax sync` - Install from lockfile
- Reproducible environments

Phase 4 will add:
- `wax install --cask` - GUI app installation
- DMG mounting and extraction
