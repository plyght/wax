# Linux Support

## Overview
wax now supports Linux alongside macOS. Core functionality (formula installation) works on both platforms, while macOS-specific features (casks) are properly guarded.

## Platform Detection

### Bottle Platform Detection
- **macOS**: `arm64_sonoma`, `sonoma`, `arm64_ventura`, `ventura`, etc. (based on OS version)
- **Linux**: `x86_64_linux`, `aarch64_linux`

### Homebrew Prefix Detection
The system automatically detects the Homebrew installation location:

**macOS**:
- ARM64: `/opt/homebrew` (default)
- x86_64: `/usr/local` (default)
- Falls back to `brew --prefix` command

**Linux**:
- Linuxbrew: `/home/linuxbrew/.linuxbrew` (default)
- Falls back to `/usr/local` or `brew --prefix` command

## Cross-Platform Directories

Uses the `directories` crate for platform-appropriate paths:

**macOS**:
- Cache: `~/Library/Caches/wax`
- Data: `~/Library/Application Support/wax`
- Logs: `~/Library/Caches/wax/logs`

**Linux**:
- Cache: `~/.cache/wax`
- Data: `~/.local/share/wax`
- Logs: `~/.cache/wax/logs`

Fallback to `~/.wax` if directories cannot be determined.

## Platform-Specific Features

### Casks (macOS Only)
Cask operations are restricted to macOS:
- DMG mounting (`hdiutil`)
- PKG installation
- `/Applications` directory
- App bundle management

On Linux, attempting cask operations returns:
```
Error: Operation not supported on this platform: Cask installation is only supported on macOS. Use formulae for Linux packages.
```

### Formulae (Cross-Platform)
Core formula installation works on both platforms:
- Bottle download and extraction
- Symlink creation (`/usr/local/bin`, `/opt/homebrew/bin`, or `/home/linuxbrew/.linuxbrew/bin`)
- Dependency resolution
- Package state tracking

## Implementation Details

### Modified Files
1. **src/error.rs**: Added `PlatformNotSupported` error variant
2. **src/bottle.rs**: 
   - Extended `detect_platform()` for Linux
   - Updated `homebrew_prefix()` for Linuxbrew
   - Added macOS-specific guards to `macos_version()`
3. **src/cask.rs**:
   - Added `check_platform_support()` guard
   - Added `applications_dir()` helper
   - Guarded DMG/PKG/ZIP installation methods
4. **src/cache.rs**, **src/install.rs**: Use `directories` crate for cross-platform paths
5. **src/main.rs**: Cross-platform log directory detection
6. **src/commands/list.rs**: Enhanced Homebrew prefix detection
7. **src/commands/uninstall.rs**: Guarded cask uninstallation

### Platform Conditionals
Uses Rust's `cfg!` and `#[cfg(...)]` for compile-time platform detection:
- `#[cfg(target_os = "macos")]` - macOS-specific code
- `#[cfg(not(target_os = "macos"))]` - Non-macOS fallback
- `std::env::consts::OS` - Runtime OS detection ("macos", "linux")
- `std::env::consts::ARCH` - Runtime architecture ("x86_64", "aarch64")

## Testing

### Compilation Test
```bash
# Test macOS build (default)
cargo build --release

# Test Linux build (cross-compilation or on Linux)
cargo check --target x86_64-unknown-linux-gnu
cargo check --target aarch64-unknown-linux-gnu
```

### Runtime Testing
1. **Linux**: Install Homebrew/Linuxbrew first
2. Test formula installation: `wax install wget`
3. Verify error on cask attempt: `wax install --cask firefox` (should fail gracefully)
4. Check platform detection: Look for correct bottle platform in logs

## Known Limitations

### Linux
- Casks are not supported (macOS GUI apps)
- Some formulae may not have Linux bottles available
- System dependencies may differ from macOS

### Both Platforms
- Requires Homebrew/Linuxbrew installation
- Unix-only (Windows not supported - symlinks are Unix-specific)

## Future Enhancements
- Add Linux-specific package formats (AppImage, Flatpak) if casks are requested on Linux
- Improve error messages when bottles aren't available for Linux
- Add platform-specific configuration options
- Support for custom Homebrew installation paths
