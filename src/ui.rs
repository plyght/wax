use crate::error::{Result, WaxError};
use crate::sudo;
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use inquire::Confirm;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;
use tracing::debug;

pub const PROGRESS_BAR_CHARS: &str = "█▓▒░ ";
pub const PROGRESS_BAR_TEMPLATE: &str =
    "{msg} {bar:40.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec}  eta {eta}";
pub const PROGRESS_BAR_PREFIX_TEMPLATE: &str =
    "{prefix:.bold} {bar:40.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec}  eta {eta}";
pub const SPINNER_TICK_CHARS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

pub fn copy_dir_all(src: &PathBuf, dst: &PathBuf) -> Result<()> {
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

fn copy_dir_all_inner(src: &PathBuf, dst: &PathBuf) -> Result<()> {
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
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
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
        std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            WaxError::InstallError(
                "$HOME environment variable is not set. Cannot determine home directory."
                    .to_string(),
            )
        })
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
