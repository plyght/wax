use crate::api::ApiClient;
use crate::bottle::detect_platform;
use crate::cache::Cache;
use crate::cask::CaskState;
use crate::commands::{install, uninstall};
use crate::deps::find_installed_reverse_dependencies;
use crate::error::{Result, WaxError};
use crate::install::{InstallMode, InstallState};
use crate::signal::{check_cancelled, CriticalSection};
use crate::ui::{create_spinner, PROGRESS_BAR_CHARS};
use crate::version::is_same_or_newer;
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashSet;
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

    cache.ensure_fresh().await?;

    if packages.is_empty() {
        upgrade_all(cache, dry_run, start).await
    } else {
        let mut failed_names = Vec::new();
        for package in packages {
            if let Err(e) = upgrade_single(cache, package, dry_run).await {
                eprintln!(
                    "{} {} failed: {}",
                    style("✗").red(),
                    style(package).magenta(),
                    e
                );
                failed_names.push(package.clone());
            }
        }
        if !failed_names.is_empty() {
            eprintln!(
                "\n{} package{} failed to upgrade: {}",
                style(failed_names.len()).red(),
                if failed_names.len() == 1 { "" } else { "s" },
                failed_names.join(", ")
            );
        }
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
        println!("\ndry run - no changes made");
        return Ok(());
    }

    let total = outdated.len();
    let mut success_count = 0;
    let mut fail_count = 0;
    let mut failed_names = Vec::new();

    for (i, pkg) in outdated.into_iter().enumerate() {
        check_cancelled()?;
        let _critical = CriticalSection::new();

        let label = format!("({}/{}) {}", i + 1, total, pkg.name);

        let spinner = create_spinner(&format!("removing {} ({}/{})", pkg.name, i + 1, total));
        let uninstall_result = if pkg.is_cask {
            uninstall::uninstall_quiet(cache, &pkg.name, true).await
        } else {
            uninstall::uninstall_quiet(cache, &pkg.name, false).await
        };
        spinner.finish_and_clear();

        let result = match uninstall_result {
            Ok(()) => {
                let pb = ProgressBar::new(0);
                pb.set_style(
                    ProgressStyle::default_bar()
                        .template(&format!(
                            "{{spinner:.green}} {} {{bar:30.cyan/blue}} {{bytes}}/{{total_bytes}} {{bytes_per_sec}}",
                            label
                        ))
                        .unwrap()
                        .progress_chars(PROGRESS_BAR_CHARS),
                );
                pb.enable_steady_tick(std::time::Duration::from_millis(80));

                let install_result = if pkg.is_cask {
                    install::install_quiet_with_progress(
                        cache,
                        &[pkg.name.clone()],
                        true,
                        false,
                        false,
                        &pb,
                    )
                    .await
                } else {
                    let (user_flag, global_flag) = match pkg.install_mode {
                        Some(InstallMode::User) => (true, false),
                        Some(InstallMode::Global) => (false, true),
                        _ => (false, false),
                    };
                    install::install_quiet_with_progress(
                        cache,
                        &[pkg.name.clone()],
                        false,
                        user_flag,
                        global_flag,
                        &pb,
                    )
                    .await
                };
                pb.finish_and_clear();
                install_result
            }
            Err(e) => Err(e),
        };

        match result {
            Ok(()) => {
                let cask_indicator = if pkg.is_cask {
                    format!(" {}", style("(cask)").yellow())
                } else {
                    String::new()
                };
                println!(
                    "{} {}{} {} → {}",
                    style("✓").green(),
                    style(&pkg.name).magenta(),
                    cask_indicator,
                    style(&pkg.installed_version).dim(),
                    style(&pkg.latest_version).green()
                );
                success_count += 1;

                if !pkg.is_cask {
                    if let Err(e) = reinstall_dependents(cache, &pkg.name).await {
                        eprintln!(
                            "{} failed to reinstall dependents of {}: {}",
                            style("!").yellow(),
                            style(&pkg.name).magenta(),
                            e
                        );
                    }
                }
            }
            Err(e) => {
                fail_count += 1;
                eprintln!(
                    "{} {} failed: {}",
                    style("✗").red(),
                    style(&pkg.name).magenta(),
                    e
                );
                failed_names.push(pkg.name.clone());
            }
        }
    }

    let elapsed = start.elapsed();
    if fail_count > 0 {
        println!(
            "{} upgraded, {} failed [{}ms]",
            style(success_count).green(),
            style(fail_count).red(),
            elapsed.as_millis()
        );
    } else {
        println!(
            "{} package{} upgraded [{}ms]",
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

    let formulae = cache.load_all_formulae().await?;
    let formula = formulae
        .iter()
        .find(|f| f.name == formula_name || f.full_name == formula_name)
        .ok_or_else(|| WaxError::FormulaNotFound(formula_name.to_string()))?;

    let latest_version = formula.full_version();
    let installed_version = &installed.version;

    if is_same_or_newer(installed_version, &latest_version) {
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
            style(&latest_version).magenta()
        );
        println!("\ndry run - no changes made");
        return Ok(());
    }

    upgrade_formula_internal(cache, formula_name, Some(installed.install_mode)).await
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

    if is_same_or_newer(installed_version, latest_version) {
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

    upgrade_cask_internal(cache, cask_name).await
}

async fn upgrade_formula_internal(
    cache: &Cache,
    formula_name: &str,
    install_mode: Option<InstallMode>,
) -> Result<()> {
    let _critical = CriticalSection::new();

    uninstall::uninstall_quiet(cache, formula_name, false).await?;

    let (user_flag, global_flag) = match install_mode {
        Some(InstallMode::User) => (true, false),
        Some(InstallMode::Global) => (false, true),
        None => (false, false),
    };

    install::install_quiet(
        cache,
        &[formula_name.to_string()],
        false,
        user_flag,
        global_flag,
    )
    .await?;

    reinstall_dependents(cache, formula_name).await?;

    Ok(())
}

async fn reinstall_dependents(cache: &Cache, upgraded_package: &str) -> Result<()> {
    let formulae = cache.load_all_formulae().await?;
    let state = InstallState::new()?;
    let installed_packages = state.load().await?;
    let installed_names: HashSet<String> = installed_packages.keys().cloned().collect();

    let reverse_deps =
        find_installed_reverse_dependencies(upgraded_package, &formulae, &installed_names);

    if reverse_deps.is_empty() {
        return Ok(());
    }

    println!(
        "  {} reinstalling {} dependent{}: {}",
        style("→").cyan(),
        reverse_deps.len(),
        if reverse_deps.len() == 1 { "" } else { "s" },
        reverse_deps
            .iter()
            .map(|s| style(s).magenta().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    for dep_name in &reverse_deps {
        let dep_mode = installed_packages.get(dep_name).map(|p| p.install_mode);

        let spinner = create_spinner(&format!("reinstalling {}", dep_name));

        let (user_flag, global_flag) = match dep_mode {
            Some(InstallMode::User) => (true, false),
            Some(InstallMode::Global) => (false, true),
            _ => (false, false),
        };

        let result = async {
            uninstall::uninstall_quiet(cache, dep_name, false).await?;
            install::install_quiet(cache, &[dep_name.clone()], false, user_flag, global_flag).await
        }
        .await;

        spinner.finish_and_clear();

        match result {
            Ok(()) => {
                println!(
                    "  {} {} reinstalled",
                    style("✓").green(),
                    style(dep_name).magenta()
                );
            }
            Err(e) => {
                eprintln!(
                    "  {} {} reinstall failed: {}",
                    style("✗").red(),
                    style(dep_name).magenta(),
                    e
                );
            }
        }
    }

    Ok(())
}

async fn upgrade_cask_internal(cache: &Cache, cask_name: &str) -> Result<()> {
    let _critical = CriticalSection::new();

    uninstall::uninstall_quiet(cache, cask_name, true).await?;

    install::install_quiet(cache, &[cask_name.to_string()], true, false, false).await?;

    Ok(())
}

pub async fn get_outdated_packages(cache: &Cache) -> Result<Vec<OutdatedPackage>> {
    let state = InstallState::new()?;
    state.sync_from_cellar().await?;
    let installed_packages = state.load().await?;

    let cask_state = CaskState::new()?;
    let installed_casks = cask_state.load().await?;

    let formulae = cache.load_all_formulae().await?;
    let casks = cache.load_casks().await?;

    let mut outdated = Vec::new();

    let platform = detect_platform();
    for (name, installed) in &installed_packages {
        if let Some(formula) = formulae.iter().find(|f| &f.name == name) {
            let latest = formula.full_version();
            let version_outdated = !is_same_or_newer(&installed.version, &latest);

            let rebuild_outdated = !version_outdated
                && installed.version == latest
                && installed.bottle_rebuild < formula.bottle_rebuild();

            let sha_outdated = !version_outdated
                && !rebuild_outdated
                && installed.bottle_sha256.is_some()
                && formula
                    .bottle
                    .as_ref()
                    .and_then(|b| b.stable.as_ref())
                    .and_then(|s| s.files.get(&platform).or_else(|| s.files.get("all")))
                    .map(|f| Some(&f.sha256) != installed.bottle_sha256.as_ref())
                    .unwrap_or(false);

            if version_outdated || rebuild_outdated || sha_outdated {
                outdated.push(OutdatedPackage {
                    name: name.clone(),
                    installed_version: installed.version.clone(),
                    latest_version: if rebuild_outdated {
                        format!("{} (rebuild {})", latest, formula.bottle_rebuild())
                    } else {
                        latest
                    },
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
                if !is_same_or_newer(&installed.version, &details.version) {
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
