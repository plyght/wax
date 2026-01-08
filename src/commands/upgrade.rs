use crate::cache::Cache;
use crate::commands::{install, uninstall};
use crate::error::{Result, WaxError};
use crate::install::{InstallMode, InstallState};
use console::style;
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn upgrade(cache: &Cache, formula_name: &str, dry_run: bool) -> Result<()> {
    let state = InstallState::new()?;
    let installed_packages = state.load().await?;

    let installed = installed_packages
        .get(formula_name)
        .ok_or_else(|| WaxError::NotInstalled(formula_name.to_string()))?;

    let formulae = cache.load_formulae().await?;
    let formula = formulae
        .iter()
        .find(|f| f.name == formula_name)
        .ok_or_else(|| WaxError::FormulaNotFound(formula_name.to_string()))?;

    let latest_version = &formula.versions.stable;
    let installed_version = &installed.version;

    if installed_version == latest_version {
        println!(
            "{} {} {} is already up to date",
            style("✓").green().bold(),
            formula_name,
            installed_version
        );
        return Ok(());
    }

    println!(
        "{} Upgrading {} from {} to {}",
        style("→").cyan().bold(),
        formula_name,
        installed_version,
        latest_version
    );

    if dry_run {
        println!("\n{} Dry run - no changes made", style("✓").green().bold());
        return Ok(());
    }

    let install_mode = installed.install_mode;

    uninstall::uninstall(cache, formula_name, false, false).await?;

    let (user_flag, global_flag) = match install_mode {
        InstallMode::User => (true, false),
        InstallMode::Global => (false, true),
    };
    install::install(
        cache,
        &[formula_name.to_string()],
        false,
        false,
        user_flag,
        global_flag,
        false,
    )
    .await?;

    println!(
        "{} Upgraded {} to {}",
        style("✓").green().bold(),
        formula_name,
        latest_version
    );

    Ok(())
}
