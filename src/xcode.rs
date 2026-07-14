use crate::error::{Result, WaxError};
use std::path::{Path, PathBuf};

const XCODE_APP: &str = "/Applications/Xcode.app";
const XCODE_BETA_APP: &str = "/Applications/Xcode-beta.app";

fn developer_dir_in_app(app: &Path) -> PathBuf {
    app.join("Contents/Developer")
}

fn app_has_developer(app: &Path) -> bool {
    developer_dir_in_app(app).is_dir()
}

/// Prefer stable Xcode, then Xcode-beta (common on macOS betas).
pub fn resolve_developer_dir() -> Option<PathBuf> {
    let stable = PathBuf::from(XCODE_APP);
    if app_has_developer(&stable) {
        return Some(developer_dir_in_app(&stable));
    }
    let beta = PathBuf::from(XCODE_BETA_APP);
    if app_has_developer(&beta) {
        return Some(developer_dir_in_app(&beta));
    }
    None
}

/// Apply `DEVELOPER_DIR` and prepend Xcode `usr/bin` to `PATH` when an Xcode.app is present.
pub fn apply_to_command(cmd: &mut std::process::Command) {
    let Some(dev) = resolve_developer_dir() else {
        return;
    };
    cmd.env("DEVELOPER_DIR", &dev);
    let tool_bin = dev.join("usr/bin");
    if tool_bin.is_dir() {
        let path_key = "PATH";
        let merged = match std::env::var_os(path_key) {
            Some(existing) => {
                let mut v = std::ffi::OsString::from(&tool_bin);
                v.push(":");
                v.push(existing);
                v
            }
            None => tool_bin.into_os_string(),
        };
        cmd.env(path_key, merged);
    }
}

#[cfg(target_os = "macos")]
pub fn require_for_make_build() -> Result<()> {
    if resolve_developer_dir().is_some() {
        return Ok(());
    }
    Err(WaxError::BuildError(
        "Full Xcode is required for this build (Command Line Tools alone are not enough).\n\
         Install Xcode or Xcode Beta from the App Store, then run:\n\
           sudo xcode-select -s /Applications/Xcode.app/Contents/Developer\n\
         or for beta:\n\
           sudo xcode-select -s '/Applications/Xcode Beta.app/Contents/Developer'"
            .to_string(),
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn require_for_make_build() -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn developer_dir_paths_are_under_applications() {
        assert!(developer_dir_in_app(Path::new(XCODE_APP)).ends_with("Contents/Developer"));
    }
}
