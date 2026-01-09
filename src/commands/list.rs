use crate::cask::CaskState;
use crate::error::{Result, WaxError};
use crate::ui::print_info;
use console::style;
use std::path::PathBuf;
use tracing::instrument;

#[instrument]
pub async fn list() -> Result<()> {
    let homebrew_prefix = detect_homebrew_prefix()?;
    let cellar_path = homebrew_prefix.join("Cellar");

    let cask_state = CaskState::new()?;
    let installed_casks = cask_state.load().await?;

    let mut packages = Vec::new();

    if cellar_path.exists() {
        let mut entries = tokio::fs::read_dir(&cellar_path).await?;

        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                let package_name = entry.file_name().to_string_lossy().to_string();

                let mut versions = Vec::new();
                let mut version_entries = tokio::fs::read_dir(entry.path()).await?;
                while let Some(version_entry) = version_entries.next_entry().await? {
                    if version_entry.file_type().await?.is_dir() {
                        versions.push(version_entry.file_name().to_string_lossy().to_string());
                    }
                }

                packages.push((package_name, versions));
            }
        }
    }

    if packages.is_empty() && installed_casks.is_empty() {
        print_info("No packages installed");
        return Ok(());
    }

    if !packages.is_empty() {
        packages.sort_by(|a, b| a.0.cmp(&b.0));

        println!("\n{}", style("Installed Formulae").bold().green());
        println!("{}", "─".repeat(80));

        for (package, versions) in &packages {
            let version_str = versions.join(", ");
            println!("{:<30} {}", style(package).cyan(), version_str);
        }

        println!("\n{} formulae installed", packages.len());
    }

    if !installed_casks.is_empty() {
        let mut cask_list: Vec<_> = installed_casks.iter().collect();
        cask_list.sort_by_key(|(name, _)| *name);

        println!("\n{}", style("Installed Casks").bold().green());
        println!("{}", "─".repeat(80));

        for (cask_name, cask) in cask_list {
            println!(
                "{:<30} {}",
                style(format!("{} {}", cask_name, style("(cask)").dim())).cyan(),
                cask.version
            );
        }

        println!("\n{} casks installed", installed_casks.len());
    }

    let total = packages.len() + installed_casks.len();
    if total > 0 && !packages.is_empty() && !installed_casks.is_empty() {
        println!("\n{} total packages installed", total);
    }

    Ok(())
}

fn detect_homebrew_prefix() -> Result<PathBuf> {
    if let Ok(output) = std::process::Command::new("brew").arg("--prefix").output() {
        if output.status.success() {
            if let Ok(prefix) = String::from_utf8(output.stdout) {
                let path = PathBuf::from(prefix.trim());
                if path.join("Cellar").exists() {
                    return Ok(path);
                }
            }
        }
    }

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let candidates = match os {
        "macos" => match arch {
            "aarch64" => vec![PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")],
            _ => vec![PathBuf::from("/usr/local"), PathBuf::from("/opt/homebrew")],
        },
        "linux" => vec![
            PathBuf::from("/home/linuxbrew/.linuxbrew"),
            PathBuf::from("/usr/local"),
        ],
        _ => vec![PathBuf::from("/usr/local")],
    };

    for path in candidates {
        if path.join("Cellar").exists() {
            return Ok(path);
        }
    }

    Err(WaxError::HomebrewNotFound)
}
