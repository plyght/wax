use crate::bottle::cellar_path;
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

    let package = installed_packages
        .get(formula_name)
        .ok_or_else(|| WaxError::NotInstalled(formula_name.to_string()))?;

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
                Ok(true) => {},
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

    let cellar = cellar_path();
    let removed_links = remove_symlinks(formula_name, &package.version, &cellar, false).await?;

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

    let app_path = std::path::PathBuf::from("/Applications").join(format!("{}.app", cask_name));

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
