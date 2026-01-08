# Development Guide

## Getting Started

### Prerequisites

- Rust 1.70 or later
- macOS or Linux operating system
- Homebrew or Linuxbrew (optional, for testing compatibility)

### Building from Source

```bash
git clone https://github.com/yourusername/wax.git
cd wax
cargo build --release
```

The compiled binary will be at `target/release/wax`.

### Development Build

```bash
cargo build
```

Debug builds include additional logging and checks.

### Running Tests

Currently, Wax uses integration tests via shell scripts:

```bash
./test_phase2.sh
```

### Running the Binary

```bash
cargo run -- <command> [args]

cargo run -- update
cargo run -- search nginx
cargo run -- install tree
cargo run -- --verbose install jq
```

### Code Quality Checks

```bash
cargo clippy
cargo fmt --check
cargo test
```

## Project Structure

```
wax/
├── src/
│   ├── main.rs              # Entry point and CLI parsing
│   ├── api.rs               # Homebrew API client
│   ├── cache.rs             # Local cache management
│   ├── bottle.rs            # Bottle download and extraction
│   ├── cask.rs              # Cask installation (macOS only)
│   ├── install.rs           # Installation state and symlinks
│   ├── deps.rs              # Dependency resolution
│   ├── lockfile.rs          # Lockfile support
│   ├── ui.rs                # Terminal UI components
│   ├── error.rs             # Error types
│   └── commands/            # CLI command implementations
│       ├── mod.rs
│       ├── search.rs
│       ├── info.rs
│       ├── install.rs
│       ├── uninstall.rs
│       ├── upgrade.rs
│       ├── update.rs
│       ├── list.rs
│       ├── lock.rs
│       └── sync.rs
├── docs/                    # Documentation
├── test_phase2.sh          # Integration tests
├── Cargo.toml              # Dependencies and metadata
└── README.md               # User-facing documentation
```

## Development Workflow

### Adding a New Command

1. Create new file in `src/commands/` (e.g., `cleanup.rs`)
2. Implement command function:

```rust
use crate::error::Result;

pub async fn cleanup() -> Result<()> {
    // Implementation
    Ok(())
}
```

3. Export from `src/commands/mod.rs`:

```rust
pub mod cleanup;
```

4. Add command variant to `src/main.rs`:

```rust
#[derive(Subcommand)]
enum Commands {
    // ...
    #[command(about = "Clean up old bottles and caches")]
    Cleanup,
}
```

5. Handle command in main match:

```rust
Commands::Cleanup => {
    commands::cleanup::cleanup().await?;
}
```

### Adding a New Error Type

1. Add variant to `WaxError` enum in `src/error.rs`:

```rust
#[derive(Error, Debug)]
pub enum WaxError {
    // ...
    #[error("Custom error: {0}")]
    CustomError(String),
}
```

2. Use in code:

```rust
return Err(WaxError::CustomError("Something went wrong".to_string()));
```

### Adding Dependencies

Edit `Cargo.toml`:

```toml
[dependencies]
new-crate = "1.0"
```

Run `cargo build` to fetch and compile.

## Debugging

### Enable Verbose Logging

```bash
cargo run -- --verbose install tree
```

Logs are written to:
- macOS: `~/Library/Caches/wax/logs/wax.log`
- Linux: `~/.cache/wax/logs/wax.log`

### Add Tracing

```rust
use tracing::{info, debug, warn, error};

#[instrument]
pub async fn my_function() -> Result<()> {
    debug!("Starting operation");
    info!("Processing {}", name);
    Ok(())
}
```

### Common Debug Scenarios

**Cache Issues:**
```bash
rm -rf ~/.cache/wax/
cargo run -- update
```

**Installation Issues:**
```bash
cargo run -- --verbose install <formula>
cat ~/Library/Caches/wax/logs/wax.log
```

**Dependency Resolution:**
```bash
cargo run -- --verbose install jq  # Has dependencies
```

## Testing

### Integration Tests

The `test_phase2.sh` script validates core functionality:

```bash
./test_phase2.sh
```

Tests include:
- Formula installation
- Dependency resolution
- Symlink creation
- Uninstallation
- Upgrade workflow

### Manual Testing Checklist

Before releases, manually test:

- [ ] `wax update` completes successfully
- [ ] `wax search <query>` returns results
- [ ] `wax info <formula>` shows details
- [ ] `wax install <simple-formula>` works (e.g., tree)
- [ ] `wax install <formula-with-deps>` resolves dependencies (e.g., jq)
- [ ] `wax list` shows installed packages
- [ ] `wax uninstall <formula>` removes package
- [ ] `wax upgrade <formula>` updates to latest
- [ ] `wax lock` generates lockfile
- [ ] `wax sync` installs from lockfile
- [ ] Error messages are clear and actionable
- [ ] Progress bars display correctly
- [ ] Dry-run mode works for install/uninstall/upgrade

### Unit Tests (Future)

Add unit tests for core logic:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_detection() {
        let platform = detect_platform();
        assert!(platform.contains("arm64") || platform.contains("x86_64"));
    }

    #[tokio::test]
    async fn test_dependency_resolution() {
        // Test dependency graph
    }
}
```

Run with:
```bash
cargo test
```

## Code Style

### Formatting

Use rustfmt:

```bash
cargo fmt
```

Configuration in `rustfmt.toml`:

```toml
max_width = 100
edition = "2021"
```

### Linting

Use clippy:

```bash
cargo clippy
```

Fix warnings before committing.

### Naming Conventions

- Functions: `snake_case`
- Types: `PascalCase`
- Constants: `SCREAMING_SNAKE_CASE`
- Modules: `snake_case`

### Error Handling

Always use `Result<T>` for fallible operations:

```rust
pub fn my_function() -> Result<String> {
    let value = risky_operation()?;
    Ok(value)
}
```

Add context to errors:

```rust
use crate::error::WaxError;

std::fs::read_to_string(path)
    .map_err(|e| WaxError::IoError(e))?
```

### Async Functions

Use async/await for I/O operations:

```rust
pub async fn fetch_data() -> Result<Vec<u8>> {
    let response = client.get(url).send().await?;
    let bytes = response.bytes().await?;
    Ok(bytes.to_vec())
}
```

## Performance Optimization

### Profiling

Use cargo flamegraph:

```bash
cargo install flamegraph
sudo cargo flamegraph -- install nginx
```

### Benchmarking

Add benchmarks with criterion:

```toml
[dev-dependencies]
criterion = "0.5"
```

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn benchmark_search(c: &mut Criterion) {
    c.bench_function("search nginx", |b| {
        b.iter(|| search(black_box("nginx")))
    });
}

criterion_group!(benches, benchmark_search);
criterion_main!(benches);
```

Run with:
```bash
cargo bench
```

### Common Optimizations

1. Use streaming for large downloads
2. Parallelize independent operations
3. Cache expensive computations
4. Minimize allocations in hot paths
5. Use buffered I/O

## Platform-Specific Development

### macOS-Specific Code

Use conditional compilation:

```rust
#[cfg(target_os = "macos")]
fn macos_only_function() {
    // Implementation
}
```

### Linux Support

Test on Linux or use cross-compilation:

```bash
rustup target add x86_64-unknown-linux-gnu
cargo build --target x86_64-unknown-linux-gnu
```

## Troubleshooting

### Compilation Errors

**Missing dependencies:**
```bash
cargo update
cargo clean
cargo build
```

**Link errors:**
Check system libraries are installed (OpenSSL, etc.)

### Runtime Errors

**Permission denied:**
- Run with appropriate privileges
- Use `--user` flag for local installation

**Homebrew not found:**
- Ensure Homebrew/Linuxbrew is installed
- Check `brew --prefix` works

**Network timeouts:**
- Check internet connection
- Increase timeout in `api.rs`

## Release Process

1. Update version in `Cargo.toml`
2. Update CHANGELOG.md
3. Run full test suite
4. Build release binary: `cargo build --release`
5. Tag release: `git tag v0.x.x`
6. Push to GitHub: `git push --tags`
7. Create GitHub release with binary attachments

## Contributing Guidelines

### Code Review Checklist

- [ ] Code follows style guidelines
- [ ] Tests added for new functionality
- [ ] Documentation updated
- [ ] No clippy warnings
- [ ] Error handling is comprehensive
- [ ] Performance impact is acceptable
- [ ] Cross-platform compatibility maintained

### Commit Messages

Format:
```
<type>: <subject>

<body>
```

Types:
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation
- `refactor`: Code restructuring
- `test`: Test additions
- `perf`: Performance improvement

Example:
```
feat: add parallel download support

Implements concurrent bottle downloads with progress tracking.
Uses tokio semaphore to limit concurrency to 8 simultaneous downloads.
```

## Resources

### Documentation

- Rust Book: https://doc.rust-lang.org/book/
- Tokio Tutorial: https://tokio.rs/tokio/tutorial
- Clap Documentation: https://docs.rs/clap/
- Homebrew Formula API: https://formulae.brew.sh/docs/api/

### Tools

- Rust Analyzer: IDE integration
- cargo-watch: Auto-rebuild on changes
- cargo-expand: Macro expansion debugging
- cargo-audit: Security vulnerability checks

## Getting Help

- Check existing documentation in `docs/`
- Review code comments in source files
- Search GitHub issues
- Ask questions in discussions
