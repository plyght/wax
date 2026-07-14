use std::path::{Path, PathBuf};
use std::process::Command;

const XCODE_APP: &str = "/Applications/Xcode.app";
const XCODE_BETA_APP: &str = "/Applications/Xcode Beta.app";

fn developer_dir_in_app(app: &Path) -> PathBuf {
    app.join("Contents/Developer")
}

/// Resolve the active developer directory.
/// 1. `xcode-select -p` (respects user's selection)
/// 2. /Applications/Xcode.app
/// 3. /Applications/Xcode Beta.app
pub fn resolve_developer_dir() -> Option<PathBuf> {
    // xcode-select -p is the source of truth
    if let Ok(out) = Command::new("xcode-select").arg("-p").output() {
        if out.status.success() {
            let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            if path.is_dir() {
                return Some(path);
            }
        }
    }
    // Fallback: probe well-known app bundles
    for app in [XCODE_APP, XCODE_BETA_APP] {
        let dev = developer_dir_in_app(Path::new(app));
        if dev.is_dir() {
            return Some(dev);
        }
    }
    None
}

/// Apply `DEVELOPER_DIR` and prepend Xcode `usr/bin` to `PATH`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beta_path_has_space() {
        assert!(XCODE_BETA_APP.contains("Beta.app"));
        assert!(!XCODE_BETA_APP.contains("beta.app"));
    }
}
