use crate::error::{Result, WaxError};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tracing::debug;

static SUDO_VALIDATED: AtomicBool = AtomicBool::new(false);
static IS_ROOT: OnceLock<bool> = OnceLock::new();

pub fn is_permission_error(err: &WaxError) -> bool {
    match err {
        WaxError::IoError(io_err) => {
            matches!(io_err.kind(), std::io::ErrorKind::PermissionDenied)
        }
        WaxError::InstallError(msg) => {
            let msg = msg.to_lowercase();
            msg.contains("permission denied") || msg.contains("os error 13")
        }
        _ => false,
    }
}

pub fn is_file_exists_error(err: &WaxError) -> bool {
    match err {
        WaxError::IoError(io_err) => {
            matches!(io_err.kind(), std::io::ErrorKind::AlreadyExists)
        }
        WaxError::InstallError(msg) => {
            let msg = msg.to_lowercase();
            msg.contains("file exists") || msg.contains("os error 17")
        }
        _ => false,
    }
}

pub fn is_running_as_root() -> bool {
    *IS_ROOT.get_or_init(|| {
        #[cfg(unix)]
        {
            // Use getuid() for better performance and reliability
            unsafe { libc::getuid() == 0 }
        }
        #[cfg(not(unix))]
        {
            false
        }
    })
}

pub fn has_sudo_cached() -> bool {
    if SUDO_VALIDATED.load(Ordering::SeqCst) {
        return true;
    }

    let cached = Command::new("sudo")
        .args(["-n", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if cached {
        SUDO_VALIDATED.store(true, Ordering::SeqCst);
    }
    cached
}

pub fn acquire_sudo() -> Result<()> {
    if is_running_as_root() || has_sudo_cached() {
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

fn normalize_path(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn sudo_remove(path: &Path) -> Result<()> {
    acquire_sudo()?;
    let path = normalize_path(path);

    let status = Command::new("sudo")
        .args(["rm", "-rf", "--"])
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(WaxError::IoError)?;

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
    let src = normalize_path(src);
    let dst = normalize_path(dst);

    let status = Command::new("sudo")
        .args(["cp", "-Rf", "--"])
        .arg(&src)
        .arg(&dst)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(WaxError::IoError)?;

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
    let path = normalize_path(path);

    let status = Command::new("sudo")
        .args(["mkdir", "-p", "--"])
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(WaxError::IoError)?;

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
    let src = normalize_path(src);
    let dst = normalize_path(dst);

    // Remove target if it exists, using sudo to be sure
    let _ = Command::new("sudo")
        .args(["rm", "-f", "--"])
        .arg(&dst)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let status = Command::new("sudo")
        .args(["ln", "-sf", "--"])
        .arg(&src)
        .arg(&dst)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(WaxError::IoError)?;

    if !status.success() {
        return Err(WaxError::InstallError(format!(
            "sudo ln -sf {} {} failed",
            src.display(),
            dst.display()
        )));
    }
    Ok(())
}

pub fn get_current_user() -> String {
    #[cfg(unix)]
    {
        use std::ffi::CStr;
        let uid = unsafe { libc::getuid() };
        let passwd = unsafe { libc::getpwuid(uid) };
        if !passwd.is_null() {
            let name = unsafe { CStr::from_ptr((*passwd).pw_name) };
            return name.to_string_lossy().into_owned();
        }
    }
    std::env::var("USER").unwrap_or_else(|_| "root".to_string())
}

#[allow(dead_code)]
pub fn sudo_chown_recursive(path: &Path) -> Result<()> {
    acquire_sudo()?;
    let path = normalize_path(path);
    let user = get_current_user();

    let status = Command::new("sudo")
        .args(["chown", "-R", &format!("{}:admin", user), "--"])
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(WaxError::IoError)?;

    if !status.success() {
        debug!("sudo chown failed for {:?}, continuing", path);
    }
    Ok(())
}
