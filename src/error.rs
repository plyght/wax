use thiserror::Error;

#[derive(Error, Debug)]
pub enum WaxError {
    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Formula not found: {0}")]
    FormulaNotFound(String),

    #[error("Cask not found: {0}")]
    CaskNotFound(String),

    #[error("Cache error: {0}")]
    CacheError(String),

    #[error("Checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("Bottle not available for platform: {0}")]
    BottleNotAvailable(String),

    #[error("Dependency cycle detected: {0}")]
    DependencyCycle(String),

    #[error("Installation failed: {0}")]
    InstallError(String),

    #[error("Package not installed: {0}")]
    NotInstalled(String),

    #[error("Lockfile error: {0}")]
    LockfileError(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[allow(dead_code)]
    #[error("Operation not supported on this platform: {0}")]
    PlatformNotSupported(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Build error: {0}")]
    BuildError(String),

    #[error("Tap error: {0}")]
    TapError(String),

    #[error("Self-update error: {0}")]
    SelfUpdateError(String),

    #[error("Service error: {0}")]
    ServiceError(String),

    #[error("Bundle error: {0}")]
    BundleError(String),

    #[error("Version not found: {0}")]
    VersionNotFound(String),

    #[error("TOML error: {0}")]
    TomlError(#[from] toml::de::Error),

    #[error("operation interrupted")]
    Interrupted,
}

pub type Result<T> = std::result::Result<T, WaxError>;

/// Validate that a package/formula name doesn't contain path traversal or injection characters.
/// Allows alphanumeric, hyphens, underscores, periods, plus signs, and `@` (for versioned names).
/// Also allows forward slashes for tap-qualified names (e.g., `user/repo/formula`), but only in
/// well-formed, relative-style paths (no leading/trailing '/', empty segments, or '.' segments).
pub fn validate_package_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(WaxError::InvalidInput(
            "Package name cannot be empty".to_string(),
        ));
    }
    if name.starts_with('/') || name.ends_with('/') {
        return Err(WaxError::InvalidInput(format!(
            "Package name must not start or end with '/': {}",
            name
        )));
    }
    for segment in name.split('/') {
        if segment.is_empty() {
            return Err(WaxError::InvalidInput(format!(
                "Package name contains empty path segment: {}",
                name
            )));
        }
        if segment == "." || segment == ".." {
            return Err(WaxError::InvalidInput(format!(
                "Package name contains invalid path segment '{}': {}",
                segment, name
            )));
        }
    }
    if name.contains("..") {
        return Err(WaxError::InvalidInput(format!(
            "Package name contains path traversal: {}",
            name
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || "-_.+@/".contains(c))
    {
        return Err(WaxError::InvalidInput(format!(
            "Package name contains invalid characters: {}",
            name
        )));
    }
    Ok(())
}
