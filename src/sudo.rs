use crate::error::{Result, WaxError};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::debug;

static SUDO_VALIDATED: AtomicBool = AtomicBool::new(false);

pub fn is_permission_error(err: &WaxError) -> bool {
    match err {
        WaxError::IoError(io_err) => {
            matches!(io_err.kind(), std::io::ErrorKind::PermissionDenied)
        }
        WaxError::InstallError(msg) => {
            msg.contains("Permission denied") || msg.contains("os error 13")
        }
        _ => false,
    }
}

pub fn is_file_exists_error(err: &WaxError) -> bool {
    match err {
        WaxError::IoError(io_err) => {
            matches!(io_err.kind(), std::io::ErrorKind::AlreadyExists)
        }
        WaxError::InstallError(msg) => msg.contains("File exists") || msg.contains("os error 17"),
        _ => false,
    }
}

pub fn is_running_as_root() -> bool {
    #[cfg(unix)]
    {
        Command::new("id")
            .args(["-u"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

pub fn has_sudo_cached() -> bool {
    Command::new("sudo")
        .args(["-n", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn acquire_sudo() -> Result<()> {
    if is_running_as_root() || SUDO_VALIDATED.load(Ordering::SeqCst) {
        return Ok(());
    }

    if has_sudo_cached() {
        SUDO_VALIDATED.store(true, Ordering::SeqCst);
        debug!("sudo credentials already cached");
        return Ok(());
    }

    eprintln!(
        "{} elevated permissions required — authenticate with Touch ID or password",
        console::style("🔐").dim()
    );

    let status = Command::new("sudo")
        .args(["-v"])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| WaxError::InstallError(format!("failed to run sudo: {}", e)))?;

    if !status.success() {
        return Err(WaxError::InstallError(
            "sudo authentication failed".to_string(),
        ));
    }

    SUDO_VALIDATED.store(true, Ordering::SeqCst);
    debug!("sudo credentials acquired");
    Ok(())
}

pub fn sudo_remove(path: &Path) -> Result<()> {
    acquire_sudo()?;

    let status = Command::new("sudo")
        .args(["rm", "-rf"])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| WaxError::IoError(e))?;

    if !status.success() {
        return Err(WaxError::InstallError(format!(
            "sudo rm -rf {} failed",
            path.display()
        )));
    }
    Ok(())
}

pub fn sudo_copy(src: &Path, dst: &Path) -> Result<()> {
    acquire_sudo()?;

    let status = Command::new("sudo")
        .args(["cp", "-Rf"])
        .arg(src)
        .arg(dst)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| WaxError::IoError(e))?;

    if !status.success() {
        return Err(WaxError::InstallError(format!(
            "sudo cp -Rf {} {} failed",
            src.display(),
            dst.display()
        )));
    }
    Ok(())
}

pub fn sudo_mkdir(path: &Path) -> Result<()> {
    acquire_sudo()?;

    let status = Command::new("sudo")
        .args(["mkdir", "-p"])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| WaxError::IoError(e))?;

    if !status.success() {
        return Err(WaxError::InstallError(format!(
            "sudo mkdir -p {} failed",
            path.display()
        )));
    }
    Ok(())
}

pub fn sudo_symlink(src: &Path, dst: &Path) -> Result<()> {
    acquire_sudo()?;

    let _ = Command::new("sudo")
        .args(["rm", "-f"])
        .arg(dst)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let status = Command::new("sudo")
        .args(["ln", "-sf"])
        .arg(src)
        .arg(dst)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| WaxError::IoError(e))?;

    if !status.success() {
        return Err(WaxError::InstallError(format!(
            "sudo ln -sf {} {} failed",
            src.display(),
            dst.display()
        )));
    }
    Ok(())
}

#[allow(dead_code)]
pub fn sudo_chown_recursive(path: &Path) -> Result<()> {
    acquire_sudo()?;

    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_string());

    let status = Command::new("sudo")
        .args(["chown", "-R", &format!("{}:admin", user)])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| WaxError::IoError(e))?;

    if !status.success() {
        debug!("sudo chown failed for {:?}, continuing", path);
    }
    Ok(())
}
