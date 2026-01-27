use crate::error::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::time::Duration;

pub const PROGRESS_BAR_CHARS: &str = "█▓▒░ ";
pub const PROGRESS_BAR_TEMPLATE: &str =
    "{msg} {bar:40.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec}";
pub const PROGRESS_BAR_PREFIX_TEMPLATE: &str =
    "{prefix:.bold} {bar:40.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec}";

pub fn copy_dir_all(src: &PathBuf, dst: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if ty.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else if ty.is_symlink() {
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&src_path)?;
                std::os::unix::fs::symlink(target, &dst_path)?;
            }
            #[cfg(not(unix))]
            {
                std::fs::copy(&src_path, &dst_path)?;
            }
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
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

pub fn print_success(message: &str) {
    println!("{}", message);
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
}
