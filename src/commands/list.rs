use crate::cask::CaskState;
use crate::error::{Result, WaxError};
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
        println!("No packages installed");
        return Ok(());
    }

    println!();

    if !packages.is_empty() {
        packages.sort_by(|a, b| a.0.cmp(&b.0));

        for (package, versions) in &packages {
            let version_str = versions.join(", ");
            println!("{} {}", style(&package).dim(), style(&version_str).dim());
        }
    }

    if !installed_casks.is_empty() {
        let mut cask_list: Vec<_> = installed_casks.iter().collect();
        cask_list.sort_by_key(|(name, _)| *name);

        for (cask_name, cask) in cask_list {
            println!(
                "{} {} {}",
                style(cask_name).dim(),
                style(&cask.version).dim(),
                style("(cask)").dim()
            );
        }
    }

    Ok(())
}

fn detect_homebrew_prefix() -> Result<PathBuf> {
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

    if let Ok(output) = std::process::Command::new("brew").arg("--prefix").output() {
        if output.status.success() {
            if let Ok(prefix) = String::from_utf8(output.stdout) {
                let brew_prefix = PathBuf::from(prefix.trim());
                if brew_prefix.join("Cellar").exists() {
                    return Ok(brew_prefix);
                }
            }
        }
    }

    for path in candidates {
        if path.join("Cellar").exists() {
            return Ok(path);
        }
    }

    let home = std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let wax_user_cellar = home.join(".local/wax/Cellar");
    if wax_user_cellar.exists() {
        return Ok(home.join(".local/wax"));
    }

    Err(WaxError::HomebrewNotFound)
}
