use crate::error::{Result, WaxError};
use crate::sudo;
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use inquire::Confirm;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::debug;

static SHOW_TIMING: AtomicBool = AtomicBool::new(false);

pub fn set_timing_enabled(enabled: bool) {
    SHOW_TIMING.store(enabled, Ordering::Relaxed);
}

pub fn timing_enabled() -> bool {
    SHOW_TIMING.load(Ordering::Relaxed)
}

pub fn elapsed_suffix(elapsed: Duration) -> String {
    if timing_enabled() {
        format!(" [{}ms]", elapsed.as_millis())
    } else {
        String::new()
    }
}

pub const PROGRESS_BAR_CHARS: &str = "█▓▒░ ";
pub const PROGRESS_BAR_TEMPLATE: &str =
    "{msg} {wide_bar:.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec}  eta {eta}";
pub const PROGRESS_BAR_PREFIX_TEMPLATE: &str =
    "{prefix:.bold} {wide_bar:.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec}  eta {eta}";
pub const SPINNER_TICK_CHARS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

pub fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    match copy_dir_all_inner(src, dst) {
        Ok(()) => Ok(()),
        Err(ref e) if sudo::is_permission_error(e) || sudo::is_file_exists_error(e) => {
            debug!(
                "copy_dir_all failed ({:?}), retrying with sudo: {} -> {}",
                e,
                src.display(),
                dst.display()
            );
            sudo::sudo_copy(src, dst)
        }
        Err(e) => Err(e),
    }
}

fn copy_dir_all_inner(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if ty.is_dir() {
            if let Ok(dst_meta) = dst_path.symlink_metadata() {
                if dst_meta.is_symlink() || dst_meta.is_file() {
                    std::fs::remove_file(&dst_path).or_else(|_| sudo::sudo_remove(&dst_path))?;
                }
            }
            copy_dir_all_inner(&src_path, &dst_path)?;
        } else if ty.is_symlink() {
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&src_path)?;
                if let Ok(dst_meta) = dst_path.symlink_metadata() {
                    if dst_meta.is_dir() && !dst_meta.is_symlink() {
                        std::fs::remove_dir_all(&dst_path)
                            .or_else(|_| sudo::sudo_remove(&dst_path).map(|_| ()))?;
                    } else {
                        std::fs::remove_file(&dst_path)
                            .or_else(|_| sudo::sudo_remove(&dst_path).map(|_| ()))?;
                    }
                }
                std::os::unix::fs::symlink(&target, &dst_path)
                    .or_else(|_| sudo::sudo_symlink(target.as_ref(), &dst_path).map(|_| ()))?;
            }
            #[cfg(not(unix))]
            {
                std::fs::copy(&src_path, &dst_path)?;
            }
        } else {
            if let Ok(dst_meta) = dst_path.symlink_metadata() {
                if dst_meta.is_dir() && !dst_meta.is_symlink() {
                    std::fs::remove_dir_all(&dst_path)
                        .or_else(|_| sudo::sudo_remove(&dst_path).map(|_| ()))?;
                } else if dst_meta.is_symlink() {
                    std::fs::remove_file(&dst_path)
                        .or_else(|_| sudo::sudo_remove(&dst_path).map(|_| ()))?;
                }
            }
            copy_regular_file(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn copy_regular_file(src: &Path, dst: &Path) -> Result<()> {
    std::fs::copy(src, dst)?;
    Ok(())
}

pub fn find_in_path(program: &str) -> Option<PathBuf> {
    if program.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(program);
        return path.is_file().then_some(path);
    }

    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
}

pub fn confirm_prompt(message: &str) -> Result<bool> {
    if io::stdin().is_terminal() {
        return Confirm::new(message)
            .with_default(false)
            .prompt_skippable()
            .map(|answer| answer.unwrap_or(false))
            .map_err(|e| WaxError::InstallError(format!("prompt failed: {}", e)));
    }

    print!(
        "{} {} {} ",
        style("?").cyan().bold(),
        message,
        style("[y/N]").dim()
    );
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

pub fn create_spinner(message: &str) -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner.set_message(message.to_string());
    spinner
}

pub mod dirs {
    use crate::error::{Result, WaxError};
    use std::path::PathBuf;

    pub fn home_dir() -> Result<PathBuf> {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            return Ok(home);
        }

        #[cfg(windows)]
        if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
            return Ok(home);
        }

        Err(WaxError::InstallError(
            "Home directory is not set ($HOME or USERPROFILE). Cannot determine home directory."
                .to_string(),
        ))
    }

    /// Central wax data directory: ~/.wax
    pub fn wax_dir() -> Result<PathBuf> {
        Ok(home_dir()?.join(".wax"))
    }

    pub fn wax_cache_dir() -> Result<PathBuf> {
        Ok(wax_dir()?.join("cache"))
    }

    pub fn wax_logs_dir() -> Result<PathBuf> {
        Ok(wax_dir()?.join("logs"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::tempdir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_create_spinner() {
        let message = "Loading...";
        let spinner = create_spinner(message);
        assert_eq!(spinner.message(), message);
    }

    #[test]
    fn test_dirs_resolution() {
        let _guard = ENV_LOCK.lock().unwrap();

        let original_home = env::var_os("HOME");
        #[cfg(windows)]
        let original_userprofile = env::var_os("USERPROFILE");
        let dummy_home = tempdir().unwrap().path().to_path_buf();
        env::set_var("HOME", &dummy_home);
        #[cfg(windows)]
        env::remove_var("USERPROFILE");

        assert_eq!(dirs::home_dir().unwrap(), dummy_home);
        assert_eq!(dirs::wax_dir().unwrap(), dummy_home.join(".wax"));
        assert_eq!(
            dirs::wax_cache_dir().unwrap(),
            dummy_home.join(".wax/cache")
        );
        assert_eq!(dirs::wax_logs_dir().unwrap(), dummy_home.join(".wax/logs"));

        env::remove_var("HOME");
        #[cfg(windows)]
        env::remove_var("USERPROFILE");
        assert!(dirs::home_dir().is_err());

        if let Some(h) = original_home {
            env::set_var("HOME", h);
        } else {
            env::remove_var("HOME");
        }
        #[cfg(windows)]
        if let Some(p) = original_userprofile {
            env::set_var("USERPROFILE", p);
        } else {
            env::remove_var("USERPROFILE");
        }
    }

    #[test]
    fn test_copy_dir_all_basic() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");

        fs::create_dir(&src).unwrap();
        fs::write(src.join("file1.txt"), "hello").unwrap();

        let src_sub = src.join("subdir");
        fs::create_dir(&src_sub).unwrap();
        fs::write(src_sub.join("file2.txt"), "world").unwrap();

        copy_dir_all(&src, &dst).unwrap();

        assert!(dst.exists());
        assert!(dst.join("file1.txt").exists());
        assert_eq!(fs::read_to_string(dst.join("file1.txt")).unwrap(), "hello");

        let dst_sub = dst.join("subdir");
        assert!(dst_sub.exists());
        assert!(dst_sub.join("file2.txt").exists());
        assert_eq!(
            fs::read_to_string(dst_sub.join("file2.txt")).unwrap(),
            "world"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_all_with_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");

        fs::create_dir(&src).unwrap();
        fs::write(src.join("target.txt"), "target").unwrap();
        symlink("target.txt", src.join("link.txt")).unwrap();

        copy_dir_all(&src, &dst).unwrap();

        assert!(dst.join("link.txt").exists());
        let meta = dst.join("link.txt").symlink_metadata().unwrap();
        assert!(meta.file_type().is_symlink());
        assert_eq!(
            fs::read_link(dst.join("link.txt"))
                .unwrap()
                .to_str()
                .unwrap(),
            "target.txt"
        );
        assert_eq!(fs::read_to_string(dst.join("link.txt")).unwrap(), "target");
    }

    #[test]
    fn test_copy_dir_all_overwrite() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");

        fs::create_dir(&src).unwrap();
        fs::write(src.join("file1.txt"), "new content").unwrap();

        fs::create_dir(&dst).unwrap();
        fs::write(dst.join("file1.txt"), "old content").unwrap();

        copy_dir_all(&src, &dst).unwrap();

        assert_eq!(
            fs::read_to_string(dst.join("file1.txt")).unwrap(),
            "new content"
        );
    }
}
