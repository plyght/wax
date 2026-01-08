# Troubleshooting Guide

## Common Issues

### Installation Problems

#### Permission Denied Errors

**Symptom:**
```
Error: Permission denied (os error 13)
Failed to write to /opt/homebrew/Cellar/
```

**Cause:** Insufficient permissions to write to system directories.

**Solutions:**

1. Use user-local installation:
```bash
wax install --user <formula>
```

2. Fix Homebrew directory permissions:
```bash
sudo chown -R $(whoami) /opt/homebrew
```

3. Run with sudo (not recommended):
```bash
sudo wax install <formula>
```

#### Bottle Not Available

**Symptom:**
```
Error: Bottle not available for platform: arm64_monterey
```

**Cause:** No pre-compiled bottle exists for your platform.

**Solutions:**

1. Update wax cache to get latest bottles:
```bash
wax update
wax install <formula>
```

2. Use Homebrew to build from source:
```bash
brew install <formula>
```

3. Check if formula has bottles at all:
```bash
wax info <formula>
```

#### Checksum Mismatch

**Symptom:**
```
Error: Checksum mismatch
Expected: abc123...
Got: def456...
```

**Cause:** Downloaded bottle is corrupted or tampered.

**Solutions:**

1. Retry download:
```bash
wax install <formula>
```

2. Clear cache and update:
```bash
rm -rf ~/.cache/wax/
wax update
wax install <formula>
```

3. Check network connection for corruption

4. Report issue if persists (potential security concern)

#### Dependency Cycle Detected

**Symptom:**
```
Error: Dependency cycle detected: A -> B -> C -> A
```

**Cause:** Circular dependency in formula definitions (rare, indicates Homebrew API issue).

**Solutions:**

1. Report to Homebrew (this is a formula bug)
2. Install dependencies manually in order
3. Use Homebrew for this specific formula

### Network Issues

#### Connection Timeout

**Symptom:**
```
Error: HTTP request failed: operation timed out
```

**Cause:** Network connectivity issues or slow connection.

**Solutions:**

1. Check internet connection
2. Retry operation
3. Use VPN if firewall blocking access
4. Try again during off-peak hours

#### DNS Resolution Failures

**Symptom:**
```
Error: HTTP request failed: failed to lookup address
```

**Cause:** DNS issues preventing resolution of formulae.brew.sh or GitHub domains.

**Solutions:**

1. Check DNS settings
2. Try different DNS servers (e.g., 8.8.8.8)
3. Clear DNS cache:
```bash
sudo dscacheutil -flushcache  # macOS
sudo systemd-resolve --flush-caches  # Linux
```

### Cache Issues

#### Cache Corruption

**Symptom:**
```
Error: JSON parsing failed
Error: Cache error: invalid data
```

**Cause:** Corrupted cache files from interrupted downloads or disk errors.

**Solution:**

Delete cache and rebuild:
```bash
rm -rf ~/.cache/wax/
wax update
```

#### Cache Not Initializing

**Symptom:**
```
Error: Cache not initialized. Run 'wax update' first.
```

**Cause:** First-time use without updating cache.

**Solution:**
```bash
wax update
```

### Platform-Specific Issues

#### macOS: Cask Installation Fails

**Symptom:**
```
Error: Failed to mount DMG
Error: Failed to copy app bundle
```

**Cause:** macOS security restrictions or disk space issues.

**Solutions:**

1. Check available disk space:
```bash
df -h
```

2. Grant Disk Access permission in System Settings

3. Verify DMG manually:
```bash
hdiutil verify /path/to/downloaded.dmg
```

4. Use Homebrew directly for problematic casks:
```bash
brew install --cask <cask>
```

#### Linux: Missing System Libraries

**Symptom:**
```
Error: error while loading shared libraries: libssl.so.3
```

**Cause:** Missing system dependencies not provided by formula.

**Solutions:**

1. Install system dependencies:
```bash
sudo apt install libssl3  # Debian/Ubuntu
sudo dnf install openssl-libs  # Fedora/RHEL
```

2. Check formula dependencies:
```bash
wax info <formula>
```

#### Linux: Cask Not Supported

**Symptom:**
```
Error: Operation not supported on this platform: Cask installation is only supported on macOS
```

**Cause:** Casks are macOS-specific (GUI applications).

**Solution:**

Use Linux-native package manager or AppImage/Flatpak alternatives:
```bash
sudo apt install <package>  # Debian/Ubuntu
flatpak install <app>       # Flatpak
```

### Command Issues

#### Command Not Found After Install

**Symptom:**
```bash
wax install tree
tree
bash: tree: command not found
```

**Cause:** Binary symlinks not in PATH.

**Solutions:**

1. Check symlink was created:
```bash
ls -la /opt/homebrew/bin/tree
```

2. Add Homebrew bin to PATH:
```bash
export PATH="/opt/homebrew/bin:$PATH"  # macOS ARM
export PATH="/usr/local/bin:$PATH"     # macOS Intel / Linux
```

3. For user-local installs:
```bash
export PATH="$HOME/.local/wax/bin:$PATH"
```

4. Make permanent by adding to shell config:
```bash
echo 'export PATH="/opt/homebrew/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
```

#### Symlink Conflicts

**Symptom:**
```
Error: Symlink already exists: /opt/homebrew/bin/node
```

**Cause:** Existing file or symlink at target location.

**Solutions:**

1. Check what installed it:
```bash
ls -la /opt/homebrew/bin/node
readlink /opt/homebrew/bin/node
```

2. Remove conflicting symlink:
```bash
rm /opt/homebrew/bin/node
wax install node
```

3. If from Homebrew, uninstall first:
```bash
brew uninstall node
wax install node
```

### Upgrade Issues

#### Upgrade Fails But Package Unusable

**Symptom:**
After failed upgrade, original package no longer works.

**Cause:** Upgrade removed old version before installing new one.

**Solution:**

Reinstall the package:
```bash
wax install <formula>
```

**Prevention:**

Use dry-run first:
```bash
wax upgrade <formula> --dry-run
```

### Lockfile Issues

#### Lockfile Sync Fails

**Symptom:**
```
Error: Lockfile error: version not found
```

**Cause:** Specified version no longer available in Homebrew.

**Solutions:**

1. Update cache and retry:
```bash
wax update
wax sync
```

2. Regenerate lockfile with current versions:
```bash
wax lock
wax sync
```

3. Manually edit `wax.lock` to use available versions

## Diagnostic Commands

### Check System Information

```bash
wax --version
uname -a
sw_vers  # macOS only
```

### Check Cache Status

```bash
ls -lh ~/.cache/wax/
cat ~/.cache/wax/metadata.json
```

### Check Installation State

```bash
cat ~/.local/share/wax/installed.json
```

### Check Homebrew Prefix

```bash
brew --prefix
echo $HOMEBREW_PREFIX
```

### Check Logs

```bash
tail -f ~/Library/Caches/wax/logs/wax.log  # macOS
tail -f ~/.cache/wax/logs/wax.log          # Linux
```

### Verbose Mode

Enable detailed logging:
```bash
wax --verbose <command>
```

## Getting Help

### Before Reporting Issues

1. Update wax to latest version
2. Run `wax update` to refresh cache
3. Check this troubleshooting guide
4. Search existing GitHub issues
5. Try operation with `--verbose` flag
6. Collect logs from cache directory

### Reporting Bugs

Include in bug report:

1. Wax version: `wax --version`
2. Operating system and version
3. Full command that failed
4. Complete error message
5. Relevant log entries (with `--verbose`)
6. Steps to reproduce

Submit to: https://github.com/yourusername/wax/issues

### Community Support

- GitHub Discussions: Q&A and general help
- GitHub Issues: Bug reports and feature requests

## Known Limitations

### Not Bugs

These are expected behavior:

**No Source Building:**
Wax only installs bottles (pre-compiled binaries). If bottle unavailable, use Homebrew.

**No Post-Install Scripts:**
Wax does not execute post-install scripts for security and simplicity. Some formulae requiring setup will not work correctly.

**No Binary Relocation:**
Wax does not relocate library paths in bottles. Some formulae with complex shared library dependencies may not work.

**No Custom Taps:**
Wax only supports homebrew/core and homebrew/cask. Custom taps are not yet supported.

**No Service Management:**
Wax does not start or manage services. Use system tools (launchctl, systemd).

## Performance Issues

### Slow Downloads

**Cause:** Network bandwidth or CDN issues.

**Solutions:**

1. Check network speed
2. Try different network connection
3. Retry during off-peak hours
4. Check CDN status at https://status.github.com (bottles hosted on GitHub)

### Slow Updates

**Cause:** Large JSON API responses (15,000+ items).

**Solutions:**

1. This is expected (2-4 seconds is normal)
2. Updates are infrequent (once per day sufficient)
3. Cached data used for all other operations

### High Disk Usage

**Cause:** Multiple package versions in Cellar.

**Solutions:**

1. Remove old versions:
```bash
rm -rf /opt/homebrew/Cellar/<formula>/<old-version>
```

2. Clean cache:
```bash
rm -rf ~/.cache/wax/
```

## Advanced Troubleshooting

### Debugging with Logs

Enable maximum verbosity:
```bash
wax --verbose install <formula> 2>&1 | tee wax-debug.log
```

### Manual Bottle Download

If automated download fails, try manual:
```bash
curl -L -o bottle.tar.gz "<bottle-url>"
shasum -a 256 bottle.tar.gz
```

### Inspect Installation State

```bash
cat ~/.local/share/wax/installed.json | jq .
```

### Test Network Connectivity

```bash
curl -I https://formulae.brew.sh/api/formula.json
curl -I https://ghcr.io
```

### Check Filesystem Permissions

```bash
ls -ld /opt/homebrew
ls -ld /opt/homebrew/Cellar
touch /opt/homebrew/test && rm /opt/homebrew/test
```

## Recovery Procedures

### Complete Reset

Remove all wax data:
```bash
rm -rf ~/.cache/wax/
rm -rf ~/.local/share/wax/
wax update
```

### Reinstall Wax

```bash
cargo clean
cargo build --release
sudo cp target/release/wax /usr/local/bin/
```

### Fallback to Homebrew

If wax fails persistently, use Homebrew:
```bash
brew install <formula>
```

Wax and Homebrew coexist safely.
