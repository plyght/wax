use crate::api::ApiClient;
use crate::cache::Cache;
use crate::cask::CaskState;
use crate::commands::{install, uninstall};
use crate::error::{Result, WaxError};
use crate::install::{InstallMode, InstallState};
use console::style;
use tracing::instrument;

#[derive(Debug)]
pub struct OutdatedPackage {
    pub name: String,
    pub installed_version: String,
    pub latest_version: String,
    pub is_cask: bool,
    pub install_mode: Option<InstallMode>,
}

#[instrument(skip(cache))]
pub async fn upgrade(cache: &Cache, packages: &[String], dry_run: bool) -> Result<()> {
    let start = std::time::Instant::now();

    if packages.is_empty() {
        upgrade_all(cache, dry_run, start).await
    } else {
        for package in packages {
            upgrade_single(cache, package, dry_run).await?;
        }
        let elapsed = start.elapsed();
        println!("\n[{}ms] done", elapsed.as_millis());
        Ok(())
    }
}

async fn upgrade_all(cache: &Cache, dry_run: bool, start: std::time::Instant) -> Result<()> {
    let outdated = get_outdated_packages(cache).await?;

    if outdated.is_empty() {
        println!("all packages are up to date");
        let elapsed = start.elapsed();
        println!("\n[{}ms] done", elapsed.as_millis());
        return Ok(());
    }

    println!(
        "upgrading {} package{}...\n",
        style(outdated.len()).cyan(),
        if outdated.len() == 1 { "" } else { "s" }
    );

    if dry_run {
        for pkg in &outdated {
            let cask_indicator = if pkg.is_cask {
                format!(" {}", style("(cask)").yellow())
            } else {
                String::new()
            };
            println!(
                "{}{}: {} → {}",
                style(&pkg.name).magenta(),
                cask_indicator,
                style(&pkg.installed_version).dim(),
                style(&pkg.latest_version).green()
            );
        }
        let elapsed = start.elapsed();
        println!("\ndry run - no changes made [{}ms]", elapsed.as_millis());
        return Ok(());
    }

    let mut success_count = 0;
    let mut fail_count = 0;

    for pkg in outdated {
        let cask_indicator = if pkg.is_cask {
            format!(" {}", style("(cask)").yellow())
        } else {
            String::new()
        };
        println!(
            "upgrading {}{}: {} → {}",
            style(&pkg.name).magenta(),
            cask_indicator,
            style(&pkg.installed_version).dim(),
            style(&pkg.latest_version).green()
        );

        let result = if pkg.is_cask {
            upgrade_cask_internal(cache, &pkg.name, false).await
        } else {
            upgrade_formula_internal(cache, &pkg.name, pkg.install_mode, false).await
        };

        match result {
            Ok(()) => {
                success_count += 1;
                println!("  {} upgraded\n", style("✓").green());
            }
            Err(e) => {
                fail_count += 1;
                println!("  {} failed: {}\n", style("✗").red(), e);
            }
        }
    }

    let elapsed = start.elapsed();
    if fail_count > 0 {
        println!(
            "\n{} upgraded, {} failed [{}ms]",
            style(success_count).green(),
            style(fail_count).red(),
            elapsed.as_millis()
        );
    } else {
        println!(
            "\n{} package{} upgraded [{}ms]",
            style(success_count).green(),
            if success_count == 1 { "" } else { "s" },
            elapsed.as_millis()
        );
    }

    Ok(())
}

async fn upgrade_single(cache: &Cache, formula_name: &str, dry_run: bool) -> Result<()> {
    let state = InstallState::new()?;
    let installed_packages = state.load().await?;

    let installed = if let Some(pkg) = installed_packages.get(formula_name) {
        pkg.clone()
    } else {
        let cask_state = CaskState::new()?;
        let installed_casks = cask_state.load().await?;

        if installed_casks.contains_key(formula_name) {
            return upgrade_cask_single(cache, formula_name, dry_run).await;
        }

        state.sync_from_cellar().await?;
        let updated_packages = state.load().await?;

        updated_packages
            .get(formula_name)
            .cloned()
            .ok_or_else(|| WaxError::NotInstalled(formula_name.to_string()))?
    };

    let formulae = cache.load_formulae().await?;
    let formula = formulae
        .iter()
        .find(|f| f.name == formula_name)
        .ok_or_else(|| WaxError::FormulaNotFound(formula_name.to_string()))?;

    let latest_version = &formula.versions.stable;
    let installed_version = &installed.version;

    if installed_version == latest_version {
        println!(
            "{}@{} is already up to date",
            style(formula_name).magenta(),
            style(installed_version).dim()
        );
        return Ok(());
    }

    if dry_run {
        println!(
            "{}: {} → {}",
            style(formula_name).magenta(),
            style(installed_version).dim(),
            style(latest_version).magenta()
        );
        println!("\ndry run - no changes made");
        return Ok(());
    }

    upgrade_formula_internal(cache, formula_name, Some(installed.install_mode), false).await
}

async fn upgrade_cask_single(cache: &Cache, cask_name: &str, dry_run: bool) -> Result<()> {
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
            "{}@{} {} is already up to date",
            style(cask_name).magenta(),
            style(installed_version).dim(),
            style("(cask)").yellow()
        );
        return Ok(());
    }

    if dry_run {
        println!(
            "{} {}: {} → {}",
            style("(cask)").yellow(),
            style(cask_name).magenta(),
            style(installed_version).dim(),
            style(latest_version).magenta()
        );
        println!("\ndry run - no changes made");
        return Ok(());
    }

    upgrade_cask_internal(cache, cask_name, false).await
}

async fn upgrade_formula_internal(
    cache: &Cache,
    formula_name: &str,
    install_mode: Option<InstallMode>,
    _dry_run: bool,
) -> Result<()> {
    uninstall::uninstall(cache, formula_name, false, false).await?;

    let (user_flag, global_flag) = match install_mode {
        Some(InstallMode::User) => (true, false),
        Some(InstallMode::Global) => (false, true),
        None => (false, false),
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

    Ok(())
}

async fn upgrade_cask_internal(cache: &Cache, cask_name: &str, _dry_run: bool) -> Result<()> {
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

    Ok(())
}

pub async fn get_outdated_packages(cache: &Cache) -> Result<Vec<OutdatedPackage>> {
    let state = InstallState::new()?;
    state.sync_from_cellar().await?;
    let installed_packages = state.load().await?;

    let cask_state = CaskState::new()?;
    let installed_casks = cask_state.load().await?;

    let formulae = cache.load_formulae().await?;
    let casks = cache.load_casks().await?;

    let mut outdated = Vec::new();

    for (name, installed) in &installed_packages {
        if let Some(formula) = formulae.iter().find(|f| &f.name == name) {
            let latest = &formula.versions.stable;
            if &installed.version != latest {
                outdated.push(OutdatedPackage {
                    name: name.clone(),
                    installed_version: installed.version.clone(),
                    latest_version: latest.clone(),
                    is_cask: false,
                    install_mode: Some(installed.install_mode),
                });
            }
        }
    }

    let api_client = ApiClient::new();
    for (name, installed) in &installed_casks {
        if let Some(cask) = casks
            .iter()
            .find(|c| &c.token == name || &c.full_token == name)
        {
            if let Ok(details) = api_client.fetch_cask_details(&cask.token).await {
                if installed.version != details.version {
                    outdated.push(OutdatedPackage {
                        name: name.clone(),
                        installed_version: installed.version.clone(),
                        latest_version: details.version,
                        is_cask: true,
                        install_mode: None,
                    });
                }
            }
        }
    }

    outdated.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(outdated)
}
