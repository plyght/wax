use crate::bottle::homebrew_prefix;
use crate::cask::CaskState;
use crate::error::Result;
use crate::install::InstallState;
use console::style;
use tracing::instrument;

#[instrument]
pub async fn list() -> Result<()> {
    let candidates = [
        homebrew_prefix().join("Cellar"),
        crate::ui::dirs::home_dir()
            .unwrap_or_else(|_| homebrew_prefix())
            .join(".local/wax/Cellar"),
    ];

    let cellar_path = candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .unwrap_or_else(homebrew_prefix);

    let cask_state = CaskState::new()?;
    let installed_casks = cask_state.load().await?;

    let install_state = InstallState::new()?;
    let installed_packages = install_state.load().await?;

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

                let from_source = installed_packages
                    .get(&package_name)
                    .map(|p| p.from_source)
                    .unwrap_or(false);

                packages.push((package_name, versions, from_source));
            }
        }
    }

    if packages.is_empty() && installed_casks.is_empty() {
        println!("no packages installed");
        return Ok(());
    }

    println!();

    if !packages.is_empty() {
        packages.sort_by(|a, b| a.0.cmp(&b.0));

        for (package, versions, from_source) in &packages {
            let version_str = versions.join(", ");
            if *from_source {
                println!(
                    "{} {} {}",
                    style(package).magenta(),
                    style(&version_str).dim(),
                    style("(source)").yellow()
                );
            } else {
                println!("{} {}", style(package).magenta(), style(&version_str).dim());
            }
        }
    }

    if !installed_casks.is_empty() {
        let mut cask_list: Vec<_> = installed_casks.iter().collect();
        cask_list.sort_by_key(|(name, _)| *name);

        for (cask_name, cask) in cask_list {
            println!(
                "{} {} {}",
                style(cask_name).magenta(),
                style(&cask.version).dim(),
                style("(cask)").yellow()
            );
        }
    }

    Ok(())
}
