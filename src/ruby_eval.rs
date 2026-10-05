use crate::api::CaskDetails;
use crate::error::{Result, WaxError};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::debug;

const SHIM: &str = include_str!("ruby_eval/shim.rb");
const MARKER: &str = "__WAX_JSON__";
const ERROR_MARKER: &str = "__WAX_ERROR__";
const TIMEOUT: Duration = Duration::from_secs(60);

fn candidates() -> Vec<PathBuf> {
    let prefix = crate::bottle::homebrew_prefix();
    let mut out = Vec::new();
    if let Some(explicit) = std::env::var_os("WAX_RUBY").filter(|v| !v.is_empty()) {
        out.push(PathBuf::from(explicit));
    }
    out.push(prefix.join("Library/Homebrew/vendor/portable-ruby/current/bin/ruby"));
    out.push(prefix.join("opt/ruby/bin/ruby"));
    if let Ok(home) = crate::ui::dirs::home_dir() {
        out.push(home.join(".local/wax/opt/ruby/bin/ruby"));
    }
    if let Some(path) = crate::ui::find_in_path("ruby") {
        out.push(path);
    }
    out.push(PathBuf::from("/usr/bin/ruby"));
    out
}

pub fn find_ruby() -> Option<PathBuf> {
    candidates().into_iter().find(|p| is_executable(p))
}

pub async fn provision() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let output = tokio::process::Command::new(exe)
        .args(["install", "-y", "ruby"])
        .stdin(std::process::Stdio::null())
        .output()
        .await?;
    if !output.status.success() {
        return Err(WaxError::InstallError(
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or("wax install ruby failed")
                .to_string(),
        ));
    }
    find_ruby().ok_or_else(|| WaxError::InstallError("ruby installed but not found".into()))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn shim_path() -> Result<PathBuf> {
    let dir = crate::ui::dirs::wax_cache_dir()?.join("ruby");
    let path = dir.join(format!(
        "shim-{}.rb",
        &crate::digest::sha256_digest_hex(SHIM)[..16]
    ));
    if !path.exists() {
        std::fs::create_dir_all(&dir)?;
        let mut staged = tempfile::NamedTempFile::new_in(&dir)?;
        std::io::Write::write_all(&mut staged, SHIM.as_bytes())?;
        staged.persist(&path).map_err(|e| e.error)?;
    }
    Ok(path)
}

fn parse_output(stdout: &str) -> Option<&str> {
    stdout.lines().rev().find_map(|l| l.strip_prefix(MARKER))
}

pub async fn eval_cask(ruby: &Path, rb_path: &Path) -> Result<CaskDetails> {
    let shim = shim_path()?;
    let mut cmd = tokio::process::Command::new(ruby);
    cmd.arg("-W0")
        .arg(&shim)
        .arg("cask")
        .arg(rb_path)
        .env(
            "WAX_OS",
            if cfg!(target_os = "macos") {
                "macos"
            } else {
                "linux"
            },
        )
        .env(
            "WAX_ARCH",
            if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "x86_64"
            },
        )
        .env("WAX_MACOS_VERSION", crate::bottle::macos_version())
        .env("HOMEBREW_PREFIX", crate::bottle::homebrew_prefix())
        .env(
            "WAX_APPDIR",
            crate::cask::CaskInstaller::applications_dir()?,
        )
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(TIMEOUT, cmd.output())
        .await
        .map_err(|_| {
            WaxError::ParseError(format!(
                "Ruby evaluation of {} timed out",
                rb_path.display()
            ))
        })??;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(message) = stdout.lines().find_map(|l| l.strip_prefix(ERROR_MARKER)) {
        return Err(WaxError::ParseError(format!(
            "Ruby could not evaluate {}: {}",
            rb_path.display(),
            message
        )));
    }
    let json = parse_output(&stdout).ok_or_else(|| {
        WaxError::ParseError(format!(
            "Ruby produced no cask for {}: {}",
            rb_path.display(),
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or("no output")
        ))
    })?;
    debug!("ruby evaluated {}", rb_path.display());
    Ok(serde_json::from_str(json)?)
}

#[cfg(test)]
mod tests;
