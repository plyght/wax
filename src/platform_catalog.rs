//! Platform-specific catalogue rules (Windows vs Unix Homebrew).

use crate::error::Result;

#[cfg(target_os = "windows")]
use crate::error::WaxError;

#[cfg(target_os = "windows")]
use crate::package_spec::Ecosystem;

#[cfg(target_os = "windows")]
pub const BREW_UNAVAILABLE_MSG: &str =
    "Homebrew formulae and casks are not supported on Windows; use scoop/, winget/, or choco/ prefixes";

#[cfg(target_os = "windows")]
pub fn homebrew_unavailable() -> WaxError {
    WaxError::PlatformNotSupported(BREW_UNAVAILABLE_MSG.into())
}

#[cfg(target_os = "windows")]
pub fn reject_homebrew_cli(command: &str) -> Result<()> {
    Err(WaxError::PlatformNotSupported(format!(
        "'wax {command}' is not available on Windows. {BREW_UNAVAILABLE_MSG}"
    )))
}

#[cfg(target_os = "windows")]
pub fn reject_brew_ecosystem(force: Option<Ecosystem>) -> Result<()> {
    if force == Some(Ecosystem::Brew) {
        return Err(homebrew_unavailable());
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code, clippy::unnecessary_wraps)]
pub fn reject_homebrew_cli(_command: &str) -> Result<()> {
    Ok(())
}