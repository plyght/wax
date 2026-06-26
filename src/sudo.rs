use crate::error::{Result, WaxError};
use crate::signal;
#[cfg(not(test))]
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

static SUDO_VALIDATED: AtomicBool = AtomicBool::new(false);
static SUDO_VALIDATED_AT: AtomicU64 = AtomicU64::new(0);
const SUDO_CACHE_TTL_SECS: u64 = 15 * 60;
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

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn sudo_cache_expired() -> bool {
    if !SUDO_VALIDATED.load(Ordering::SeqCst) {
        return true;
    }
    let validated_at = SUDO_VALIDATED_AT.load(Ordering::SeqCst);
    if validated_at == 0 {
        return false;
    }
    now_unix_secs().saturating_sub(validated_at) > SUDO_CACHE_TTL_SECS
}

fn mark_sudo_validated() {
    SUDO_VALIDATED.store(true, Ordering::SeqCst);
    SUDO_VALIDATED_AT.store(now_unix_secs(), Ordering::SeqCst);
}

fn clear_sudo_validated() {
    SUDO_VALIDATED.store(false, Ordering::SeqCst);
    SUDO_VALIDATED_AT.store(0, Ordering::SeqCst);
}

pub fn has_sudo_cached() -> bool {
    if SUDO_VALIDATED.load(Ordering::SeqCst) {
        if sudo_cache_expired() {
            clear_sudo_validated();
        } else {
            return true;
        }
    }

    let cached = Command::new("sudo")
        .args(["-n", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if cached {
        mark_sudo_validated();
    }
    cached
}

fn sudo_password_prompt() -> String {
    "[wax] Password for %p: ".to_string()
}

#[cfg(not(test))]
fn interactive_terminal_available() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .open("/dev/tty")
        .map(|f| f.is_terminal())
        .unwrap_or_else(|_| std::io::stdin().is_terminal())
}

#[cfg(test)]
static MOCK_INTERACTIVE_TERMINAL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
fn interactive_terminal_available() -> bool {
    MOCK_INTERACTIVE_TERMINAL.load(std::sync::atomic::Ordering::SeqCst)
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

        let password = inquire::Password::new(&sudo_password_prompt())
            .with_display_mode(inquire::PasswordDisplayMode::Hidden)
            .without_confirmation()
            .prompt()
            .map_err(|e| {
                WaxError::InstallError(format!("Failed to read password securely: {}", e))
            })?;

        let mut cmd = Command::new("sudo");
        cmd.args(["-v", "-S", "-p", ""]);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let mut child = cmd
            .spawn()
            .map_err(|e| WaxError::InstallError(format!("failed to spawn sudo: {}", e)))?;

        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(password.as_bytes()).map_err(|e| {
                WaxError::InstallError(format!("failed to write password to sudo: {}", e))
            })?;
            stdin.write_all(b"\n").map_err(|e| {
                WaxError::InstallError(format!("failed to write newline to sudo: {}", e))
            })?;
        }

        let status = child
            .wait()
            .map_err(|e| WaxError::InstallError(format!("failed to wait on sudo: {}", e)))?;

        if !status.success() {
            return Err(WaxError::InstallError(
                "sudo authentication failed or was cancelled".to_string(),
            ));
        }

        mark_sudo_validated();
        debug!("sudo credentials acquired");
        Ok(())
    })
}

pub fn acquire_sudo() -> Result<()> {
    acquire_sudo_for(None)
}

fn normalize_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
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

#[cfg(unix)]
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

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::is_running_as_root;
    use super::{
        acquire_sudo_for, is_file_exists_error, is_permission_error, normalize_path, sudo_copy,
        sudo_password_prompt, MOCK_INTERACTIVE_TERMINAL, SUDO_VALIDATED, SUDO_VALIDATED_AT,
    };
    use crate::error::WaxError;
    use std::io::{Error, ErrorKind};
    use std::path::Path;
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct MockSudoEnv {
        _dir: tempfile::TempDir,
        old_path: Option<std::ffi::OsString>,
    }

    impl MockSudoEnv {
        fn setup() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let sudo_path = dir.path().join("sudo");

            // A mock sudo script that acts like `cp` if called with `cp`,
            // and returns success for `-n true` (to mock cached sudo).
            let script = r#"#!/bin/bash
if [ "$1" = "-n" ] && [ "$2" = "true" ]; then
    /usr/bin/env true
elif [ "$1" = "cp" ]; then
    shift
    cp "$@"
else
    /usr/bin/env false
fi
"#;
            std::fs::write(&sudo_path, script).unwrap();

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&sudo_path, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }

            let old_path = std::env::var_os("PATH");
            let mut new_path = std::ffi::OsString::new();
            new_path.push(dir.path());
            if let Some(old) = &old_path {
                new_path.push(":");
                new_path.push(old);
            }
            std::env::set_var("PATH", new_path);

            // Force cache to be invalid initially so acquire_sudo uses our mock
            SUDO_VALIDATED.store(false, Ordering::SeqCst);

            Self {
                _dir: dir,
                old_path,
            }
        }
    }

    impl Drop for MockSudoEnv {
        fn drop(&mut self) {
            if let Some(old_path) = self.old_path.take() {
                std::env::set_var("PATH", old_path);
            } else {
                std::env::remove_var("PATH");
            }
        }
    }

    #[test]
    fn sudo_password_prompt_is_wax_branded() {
        let prompt = sudo_password_prompt();
        assert!(prompt.contains("wax"));
        assert!(prompt.contains("%p"));
    }

    #[test]
    fn test_normalize_path_does_not_resolve_symlinks() {
        let path = Path::new("some/relative/symlink");
        let normalized = normalize_path(path);

        assert!(normalized.is_absolute());
        assert!(normalized.ends_with(path));
    }

    #[test]
    fn test_is_permission_error() {
        let err = WaxError::IoError(Error::new(ErrorKind::PermissionDenied, "permission denied"));
        assert!(is_permission_error(&err));

        let err = WaxError::IoError(Error::new(ErrorKind::NotFound, "not found"));
        assert!(!is_permission_error(&err));

        let err = WaxError::InstallError("Failed: permission denied".to_string());
        assert!(is_permission_error(&err));

        let err = WaxError::InstallError("Failed: Permission Denied".to_string());
        assert!(is_permission_error(&err));

        let err = WaxError::InstallError("Failed: os error 13".to_string());
        assert!(is_permission_error(&err));

        let err = WaxError::InstallError("Failed: OS ERROR 13".to_string());
        assert!(is_permission_error(&err));

        let err = WaxError::InstallError("Failed: something else".to_string());
        assert!(!is_permission_error(&err));

        let err = WaxError::FormulaNotFound("formula".to_string());
        assert!(!is_permission_error(&err));
    }

    #[test]
    fn test_is_file_exists_error() {
        let io_err = std::io::Error::from(std::io::ErrorKind::AlreadyExists);
        let err = WaxError::IoError(io_err);
        assert!(is_file_exists_error(&err));

        let io_err = std::io::Error::from(std::io::ErrorKind::NotFound);
        let err = WaxError::IoError(io_err);
        assert!(!is_file_exists_error(&err));

        let err = WaxError::InstallError("Cannot proceed: File exists at path".to_string());
        assert!(is_file_exists_error(&err));

        let err = WaxError::InstallError("Failed with os error 17".to_string());
        assert!(is_file_exists_error(&err));

        let err = WaxError::InstallError("Permission denied".to_string());
        assert!(!is_file_exists_error(&err));

        let err = WaxError::CacheError("Corrupted cache".to_string());
        assert!(!is_file_exists_error(&err));
    }

    #[test]
    fn test_sudo_copy_file_success() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _env = MockSudoEnv::setup();

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("source.txt");
        let dst = dir.path().join("dest.txt");

        std::fs::write(&src, "hello world").unwrap();

        let result = sudo_copy(&src, &dst);
        assert!(result.is_ok(), "sudo_copy failed: {:?}", result.err());
        assert!(dst.exists());
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "hello world");
    }

    #[test]
    fn test_sudo_copy_dir_success() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _env = MockSudoEnv::setup();

        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src_dir");
        std::fs::create_dir(&src_dir).unwrap();
        let src_file = src_dir.join("test.txt");
        std::fs::write(&src_file, "nested data").unwrap();

        let dst_dir = dir.path().join("dst_dir");

        let result = sudo_copy(&src_dir, &dst_dir);
        assert!(result.is_ok(), "sudo_copy failed: {:?}", result.err());
        assert!(dst_dir.exists());
        assert!(dst_dir.join("test.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dst_dir.join("test.txt")).unwrap(),
            "nested data"
        );
    }

    #[test]
    fn test_sudo_copy_failure() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _env = MockSudoEnv::setup();

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("does_not_exist.txt");
        let dst = dir.path().join("dest.txt");

        let result = sudo_copy(&src, &dst);
        assert!(result.is_err(), "sudo_copy should have failed");
    }

    struct EnvGuard {
        original_path: std::ffi::OsString,
        original_sudo_state: bool,
        original_sudo_validated_at: u64,
        original_mock_terminal: bool,
    }

    impl EnvGuard {
        fn new() -> Self {
            Self {
                original_path: std::env::var_os("PATH").unwrap_or_default(),
                original_sudo_state: SUDO_VALIDATED.load(Ordering::SeqCst),
                original_sudo_validated_at: SUDO_VALIDATED_AT.load(Ordering::SeqCst),
                original_mock_terminal: MOCK_INTERACTIVE_TERMINAL.load(Ordering::SeqCst),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::set_var("PATH", &self.original_path);
            SUDO_VALIDATED.store(self.original_sudo_state, Ordering::SeqCst);
            SUDO_VALIDATED_AT.store(self.original_sudo_validated_at, Ordering::SeqCst);
            MOCK_INTERACTIVE_TERMINAL.store(self.original_mock_terminal, Ordering::SeqCst);
        }
    }

    #[cfg(unix)]
    fn setup_fake_sudo(dir: &std::path::Path, behavior: &str) {
        use std::os::unix::fs::PermissionsExt;
        let sudo_path = dir.join("sudo");
        let script = match behavior {
            "success" => {
                "#!/bin/sh\nif [ \"$1\" = \"-n\" ] && [ \"$2\" = \"true\" ]; then\n    exit 1\nfi\nif [ \"$1\" = \"-v\" ]; then\n    exit 0\nfi\nexit 1\n"
            }
            "failure" => {
                "#!/bin/sh\nexit 1\n"
            }
            _ => "#!/bin/sh\nexit 1\n",
        };
        std::fs::write(&sudo_path, script).unwrap();
        let mut perms = std::fs::metadata(&sudo_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&sudo_path, perms).unwrap();
    }

    #[test]
    fn test_acquire_sudo_for_cached() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::new();

        SUDO_VALIDATED.store(true, Ordering::SeqCst);

        let result = acquire_sudo_for(None);
        assert!(result.is_ok());
    }

    #[test]
    #[cfg(unix)]
    #[ignore] // requires real terminal for inquire::Password prompt
    fn test_acquire_sudo_for_prompt_success() {
        if is_running_as_root() {
            return;
        }

        let _guard = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::new();

        let temp_dir = tempfile::tempdir().unwrap();
        setup_fake_sudo(temp_dir.path(), "success");
        let mut new_path = temp_dir.path().to_path_buf().into_os_string();
        new_path.push(":");
        new_path.push(&_env_guard.original_path);
        std::env::set_var("PATH", new_path);

        SUDO_VALIDATED.store(false, Ordering::SeqCst);
        MOCK_INTERACTIVE_TERMINAL.store(true, Ordering::SeqCst);

        let result = acquire_sudo_for(Some("test successful prompt"));
        assert!(result.is_ok());
        assert!(SUDO_VALIDATED.load(Ordering::SeqCst));
    }

    #[test]
    #[cfg(unix)]
    #[ignore] // requires real terminal for inquire::Password prompt
    fn test_acquire_sudo_for_prompt_failure() {
        if is_running_as_root() {
            return;
        }

        let _guard = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::new();

        let temp_dir = tempfile::tempdir().unwrap();
        setup_fake_sudo(temp_dir.path(), "failure");
        let mut new_path = temp_dir.path().to_path_buf().into_os_string();
        new_path.push(":");
        new_path.push(&_env_guard.original_path);
        std::env::set_var("PATH", new_path);

        SUDO_VALIDATED.store(false, Ordering::SeqCst);
        MOCK_INTERACTIVE_TERMINAL.store(true, Ordering::SeqCst);

        let result = acquire_sudo_for(Some("test failing prompt"));

        match result {
            Err(WaxError::InstallError(msg)) => {
                assert!(
                    msg.contains("sudo authentication failed or was cancelled")
                        || msg.contains("failed to spawn sudo")
                );
            }
            _ => panic!("Expected InstallError for failed sudo prompt"),
        }

        assert!(!SUDO_VALIDATED.load(Ordering::SeqCst));
    }
}
