use crate::adopt::{self, AdoptOptions};
use crate::cache::Cache;
use crate::cask::{relink_installed_cask, unlink_installed_cask};
use crate::error::validate_package_name;
use crate::error::{Result, WaxError};
use crate::install::{create_symlinks, remove_symlinks};
use console::style;

pub async fn link(cache: &Cache, packages: &[String]) -> Result<()> {
    if packages.is_empty() {
        return Err(WaxError::InvalidInput(
            "Specify package name(s) to link".to_string(),
        ));
    }

    let snapshot = adopt::sync_installed_state(cache, AdoptOptions::default()).await?;
    let installed = snapshot.formulae;
    let installed_casks = snapshot.casks;

    for name in packages {
        validate_package_name(name)?;
        if let Some(pkg) = installed.get(name.as_str()) {
            let cellar = pkg.install_mode.cellar_path()?;
            let links =
                create_symlinks(&pkg.name, &pkg.version, &cellar, false, pkg.install_mode).await?;
            println!(
                "{} {} ({} links)",
                style("linked").green(),
                style(name).magenta(),
                links.len()
            );
            continue;
        }

        if let Some(cask) = installed_casks.get(name.as_str()) {
            let links = relink_installed_cask(cask).await?;
            println!(
                "{} {} ({} links)",
                style("linked").green(),
                style(name).magenta(),
                links.len()
            );
            continue;
        }

        eprintln!(
            "{}: {} is not installed",
            style("warning").yellow(),
            style(name).magenta()
        );
    }

    Ok(())
}

pub async fn unlink(cache: &Cache, packages: &[String]) -> Result<()> {
    if packages.is_empty() {
        return Err(WaxError::InvalidInput(
            "Specify package name(s) to unlink".to_string(),
        ));
    }

    let snapshot = adopt::sync_installed_state(cache, AdoptOptions::default()).await?;
    let installed = snapshot.formulae;
    let installed_casks = snapshot.casks;

    for name in packages {
        validate_package_name(name)?;
        if let Some(pkg) = installed.get(name.as_str()) {
            let cellar = pkg.install_mode.cellar_path()?;
            let removed =
                remove_symlinks(&pkg.name, &pkg.version, &cellar, false, pkg.install_mode).await?;
            println!(
                "{} {} ({} links removed)",
                style("unlinked").green(),
                style(name).magenta(),
                removed.len()
            );
            continue;
        }

        if let Some(cask) = installed_casks.get(name.as_str()) {
            let removed = unlink_installed_cask(cask).await?;
            println!(
                "{} {} ({} links removed)",
                style("unlinked").green(),
                style(name).magenta(),
                removed.len()
            );
            continue;
        }

        eprintln!(
            "{}: {} is not installed",
            style("warning").yellow(),
            style(name).magenta()
        );
    }

    Ok(())
}
