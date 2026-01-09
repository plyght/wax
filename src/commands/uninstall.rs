use crate::cache::Cache;
use crate::cask::CaskState;
use crate::error::{Result, WaxError};
use crate::install::{remove_symlinks, InstallState};
use crate::ui::print_success;
use console::style;
use inquire::Confirm;
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn uninstall(cache: &Cache, formula_name: &str, dry_run: bool, cask: bool) -> Result<()> {
    if cask {
        return uninstall_cask(cache, formula_name, dry_run).await;
    }

    let state = InstallState::new()?;
    let installed_packages = state.load().await?;

    let package_opt = installed_packages.get(formula_name);

    if package_opt.is_none() {
        let cask_state = CaskState::new()?;
        let installed_casks = cask_state.load().await?;

        if installed_casks.contains_key(formula_name) {
            println!(
                "{} {} is a cask, uninstalling as cask...",
                style("ℹ").blue().bold(),
                formula_name
            );
            return uninstall_cask(cache, formula_name, dry_run).await;
        }

        return Err(WaxError::NotInstalled(formula_name.to_string()));
    }

    let package = package_opt.unwrap();

    let formulae = cache.load_formulae().await?;
    let dependents: Vec<String> = formulae
        .iter()
        .filter(|f| {
            if let Some(deps) = &f.dependencies {
                if deps.contains(&formula_name.to_string()) {
                    return installed_packages.contains_key(&f.name);
                }
            }
            false
        })
        .map(|f| f.name.clone())
        .collect();

    if !dependents.is_empty() {
        println!(
            "{} {} is a dependency of:",
            style("⚠").yellow().bold(),
            formula_name
        );
        for dep in &dependents {
            println!("  - {}", dep);
        }

        if !dry_run {
            let confirm = Confirm::new("Continue with uninstall?")
                .with_default(false)
                .prompt();

            match confirm {
                Ok(true) => {}
                Ok(false) => {
                    println!("Uninstall cancelled");
                    return Ok(());
                }
                Err(_) => return Ok(()),
            }
        }
    }

    if dry_run {
        println!(
            "{} Would uninstall {} {}",
            style("→").cyan().bold(),
            formula_name,
            package.version
        );
        println!("\n{} Dry run - no changes made", style("✓").green().bold());
        return Ok(());
    }

    let install_mode = package.install_mode;
    let cellar = install_mode.cellar_path();
    let removed_links =
        remove_symlinks(formula_name, &package.version, &cellar, false, install_mode).await?;

    println!(
        "{} Removed {} symlinks",
        style("→").cyan().bold(),
        removed_links.len()
    );

    let formula_dir = cellar.join(formula_name);
    if formula_dir.exists() {
        tokio::fs::remove_dir_all(&formula_dir).await?;
        println!(
            "{} Removed {}",
            style("→").cyan().bold(),
            formula_dir.display()
        );
    }

    state.remove(formula_name).await?;

    print_success(&format!("Uninstalled {}", formula_name));

    Ok(())
}

async fn uninstall_cask(_cache: &Cache, cask_name: &str, dry_run: bool) -> Result<()> {
    let state = CaskState::new()?;
    let installed_casks = state.load().await?;

    let cask = installed_casks
        .get(cask_name)
        .ok_or_else(|| WaxError::NotInstalled(cask_name.to_string()))?;

    if dry_run {
        println!(
            "{} Would uninstall cask {} {}",
            style("→").cyan().bold(),
            cask_name,
            cask.version
        );
        println!("\n{} Dry run - no changes made", style("✓").green().bold());
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    let app_path = std::path::PathBuf::from("/Applications").join(format!("{}.app", cask_name));

    #[cfg(not(target_os = "macos"))]
    return Err(WaxError::PlatformNotSupported(
        "Cask uninstallation is only supported on macOS".to_string(),
    ));

    if !app_path.exists() {
        println!(
            "{} Application not found at {}",
            style("⚠").yellow().bold(),
            app_path.display()
        );
        println!("Removing from installed casks list anyway...");
    } else {
        tokio::fs::remove_dir_all(&app_path).await?;
        println!(
            "{} Removed {}",
            style("→").cyan().bold(),
            app_path.display()
        );
    }

    state.remove(cask_name).await?;

    print_success(&format!("Uninstalled cask {}", cask_name));

    Ok(())
}
