use crate::api::ApiClient;
use crate::cache::Cache;
use crate::cask::CaskState;
use crate::commands::{install, uninstall};
use crate::error::{Result, WaxError};
use crate::install::{InstallMode, InstallState};
use console::style;
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn upgrade(cache: &Cache, formula_name: &str, dry_run: bool) -> Result<()> {
    let state = InstallState::new()?;
    let installed_packages = state.load().await?;

    let installed_opt = installed_packages.get(formula_name);

    if installed_opt.is_none() {
        let cask_state = CaskState::new()?;
        let installed_casks = cask_state.load().await?;

        if installed_casks.contains_key(formula_name) {
            println!(
                "{} {} is a cask, upgrading as cask...",
                style("ℹ").blue().bold(),
                formula_name
            );
            return upgrade_cask(cache, formula_name, dry_run).await;
        }

        return Err(WaxError::NotInstalled(formula_name.to_string()));
    }

    let installed = installed_opt.unwrap();

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

async fn upgrade_cask(cache: &Cache, cask_name: &str, dry_run: bool) -> Result<()> {
    let cask_state = CaskState::new()?;
    let installed_casks = cask_state.load().await?;

    let installed = installed_casks
        .get(cask_name)
        .ok_or_else(|| WaxError::NotInstalled(cask_name.to_string()))?;

    let casks = cache.load_casks().await?;
    let _cask_summary = casks
        .iter()
        .find(|c| c.token == cask_name || c.full_token == cask_name)
        .ok_or_else(|| WaxError::CaskNotFound(cask_name.to_string()))?;

    let api_client = ApiClient::new();
    let cask_details = api_client.fetch_cask_details(cask_name).await?;

    let latest_version = &cask_details.version;
    let installed_version = &installed.version;

    if installed_version == latest_version {
        println!(
            "{} {} {} is already up to date",
            style("✓").green().bold(),
            cask_name,
            installed_version
        );
        return Ok(());
    }

    println!(
        "{} Upgrading cask {} from {} to {}",
        style("→").cyan().bold(),
        cask_name,
        installed_version,
        latest_version
    );

    if dry_run {
        println!("\n{} Dry run - no changes made", style("✓").green().bold());
        return Ok(());
    }

    uninstall::uninstall(cache, cask_name, false, true).await?;

    install::install(
        cache,
        &[cask_name.to_string()],
        false,
        true,
        false,
        false,
        false,
    )
    .await?;

    println!(
        "{} Upgraded cask {} to {}",
        style("✓").green().bold(),
        cask_name,
        latest_version
    );

    Ok(())
}
