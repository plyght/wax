use crate::error::{Result, WaxError};
use crate::signal;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
            nix::unistd::getuid().is_root()
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

fn sudo_password_prompt() -> String {
    "[wax] Password for %p: ".to_string()
}

fn interactive_terminal_available() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .open("/dev/tty")
        .map(|f| f.is_terminal())
        .unwrap_or_else(|_| std::io::stdin().is_terminal())
}

/// Prompt for administrator credentials when needed.
///
/// `reason` is shown above the password prompt (e.g. why sudo is required).
pub fn acquire_sudo_for(reason: Option<&str>) -> Result<()> {
    if is_running_as_root() || has_sudo_cached() {
        return Ok(());
    }

    if !interactive_terminal_available() {
        return Err(WaxError::InstallError(
            "Administrator privileges are required but no interactive terminal is available. \
             Use `wax install --user` for a user-local install, or run from a terminal."
                .to_string(),
        ));
    }

    signal::with_suspended_progress(|| {
        if let Some(reason) = reason {
            eprintln!();
            eprintln!("{}", reason);
        }
        eprintln!();
        eprintln!("Administrator privileges are required. Enter your password when prompted.");

        let mut cmd = Command::new("sudo");
        cmd.args(["-v", "-p", &sudo_password_prompt()]);

        if let Ok(tty) = std::fs::File::open("/dev/tty") {
            cmd.stdin(Stdio::from(tty.try_clone().map_err(WaxError::IoError)?))
                .stderr(Stdio::from(tty));
        } else {
            cmd.stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        }

        let status = cmd
            .status()
            .map_err(|e| WaxError::InstallError(format!("failed to run sudo: {}", e)))?;

        if !status.success() {
            return Err(WaxError::InstallError(
                "sudo authentication failed or was cancelled".to_string(),
            ));
        }

        SUDO_VALIDATED.store(true, Ordering::SeqCst);
        debug!("sudo credentials acquired");
        Ok(())
    })
}

pub fn acquire_sudo() -> Result<()> {
    acquire_sudo_for(None)
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

#[cfg(test)]
mod tests {
    use super::sudo_password_prompt;

    #[test]
    fn sudo_password_prompt_is_wax_branded() {
        let prompt = sudo_password_prompt();
        assert!(prompt.contains("wax"));
        assert!(prompt.contains("%p"));
    }
}

pub fn get_current_user() -> String {
    #[cfg(unix)]
    {
        let uid = nix::unistd::getuid();
        if let Ok(Some(user)) = nix::unistd::User::from_uid(uid) {
            return user.name;
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
