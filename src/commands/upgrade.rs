use crate::api::ApiClient;
use crate::bottle::{detect_platform, BottleDownloader};
use crate::cache::Cache;
use crate::cask::CaskState;
use crate::commands::self_update::{self_update, Channel};
use crate::commands::{install, uninstall};
use crate::deps::find_installed_reverse_dependencies;
use crate::error::{Result, WaxError};
use crate::install::{InstallMode, InstallState};
use crate::signal::{
    check_cancelled, clear_active_multi, clear_current_op, set_active_multi, set_current_op,
    CriticalSection,
};
use crate::ui::{PROGRESS_BAR_CHARS, PROGRESS_BAR_TEMPLATE, SPINNER_TICK_CHARS};
use crate::version::{is_same_or_newer, WAX_VERSION};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::instrument;

#[derive(Debug)]
pub struct OutdatedPackage {
    pub name: String,
    pub installed_version: String,
    pub latest_version: String,
    pub is_cask: bool,
    pub install_mode: Option<InstallMode>,
}

struct UpgradeMultiGuard;

impl Drop for UpgradeMultiGuard {
    fn drop(&mut self) {
        clear_current_op();
        clear_active_multi();
    }
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
            if let Err(e) = if package == "wax" {
                upgrade_single(cache, package, dry_run).await
            } else {
                let cask_state = CaskState::new()?;
                let installed_casks = cask_state.load().await?;
                if installed_casks.contains_key(package) {
                    upgrade_cask_single(cache, package, dry_run).await
                } else {
                    upgrade_single(cache, package, dry_run).await
                }
            } {
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
        println!("\n[{}ms] done", start.elapsed().as_millis());
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

    // --- Pre-compute the full plan before touching anything ---
    let outdated_names: HashSet<String> = outdated.iter().map(|p| p.name.clone()).collect();

    let formulae = cache.load_all_formulae().await?;
    let state = InstallState::new()?;
    let installed_packages = state.load().await?;
    let installed_names: HashSet<String> = installed_packages.keys().cloned().collect();
    let install_modes: HashMap<String, InstallMode> = installed_packages
        .iter()
        .map(|(k, v)| (k.clone(), v.install_mode))
        .collect();

    // Collect all reverse-deps across every outdated formula, excluding packages
    // that are themselves outdated (they'll be handled by their own upgrade slot).
    let mut dependents_to_reinstall: Vec<String> = Vec::new();
    for pkg in &outdated {
        if pkg.is_cask {
            continue;
        }
        let rev_deps = find_installed_reverse_dependencies(&pkg.name, &formulae, &installed_names);
        for dep in rev_deps {
            if !outdated_names.contains(&dep) && !dependents_to_reinstall.contains(&dep) {
                dependents_to_reinstall.push(dep);
            }
        }
    }

    let total = outdated.len();
    let dep_total = dependents_to_reinstall.len();

    // Print plan summary
    let names: Vec<String> = outdated
        .iter()
        .map(|p| {
            if p.is_cask {
                format!("{} (cask)", p.name)
            } else {
                p.name.clone()
            }
        })
        .collect();
    println!("upgrading {}\n", style(names.join(", ")).magenta());
    if dep_total > 0 {
        println!(
            "  will reinstall {} dependent{} after: {}\n",
            dep_total,
            if dep_total == 1 { "" } else { "s" },
            dependents_to_reinstall
                .iter()
                .map(|s| style(s).dim().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let multi = MultiProgress::new();
    set_active_multi(multi.clone());
    let _guard = UpgradeMultiGuard;

    // --- Phase 0: pre-download all formula bottles concurrently ---
    let platform = detect_platform();
    let formula_by_name: HashMap<&str, &crate::api::Formula> =
        formulae.iter().map(|f| (f.name.as_str(), f)).collect();

    struct PreDownloaded {
        name: String,
        version: String,
        extract_dir: std::path::PathBuf,
        bottle_sha: String,
        bottle_rebuild: u32,
        _temp_dir: Arc<TempDir>,
    }

    let downloader = Arc::new(BottleDownloader::new());

    // Collect (name, url) for all formula bottles to be downloaded.
    let formula_bottle_urls: Vec<(String, String)> = outdated
        .iter()
        .filter(|p| !p.is_cask)
        .filter_map(|pkg| {
            let formula = formula_by_name.get(pkg.name.as_str())?;
            let bottle_info = formula.bottle.as_ref()?.stable.as_ref()?;
            let bottle_file = bottle_info
                .files
                .get(&platform)
                .or_else(|| bottle_info.files.get("all"))?;
            Some((pkg.name.clone(), bottle_file.url.clone()))
        })
        .collect();

    // Probe all bottle sizes concurrently, then allocate connections proportionally.
    // All upgrades download simultaneously; limit only caps extreme scenarios.
    let formula_upgrade_count = formula_bottle_urls.len().max(1);
    let upgrade_concurrent_limit = formula_upgrade_count.min(32);
    let upgrade_connections_map: HashMap<String, usize> = {
        let probe_tasks: Vec<_> = formula_bottle_urls
            .iter()
            .map(|(name, url)| {
                let dl = Arc::clone(&downloader);
                let url = url.clone();
                let name = name.clone();
                tokio::spawn(async move { (name, dl.probe_size(&url).await) })
            })
            .collect();

        let mut sizes: HashMap<String, u64> = HashMap::new();
        for task in probe_tasks {
            if let Ok((name, size)) = task.await {
                sizes.insert(name, size);
            }
        }

        let total_size: u64 = sizes.values().sum();
        let pool = BottleDownloader::GLOBAL_CONNECTION_POOL;
        let n = formula_bottle_urls.len().max(1);
        // Guarantee at least 2 connections per package when the pool allows it
        // (multipart requires max_connections > 1 to activate).
        let min_conns = if pool / n >= 2 { 2usize } else { 1usize };
        let mut allocs: Vec<(String, usize, f64)> = sizes
            .iter()
            .map(|(name, &size)| {
                if total_size == 0 {
                    let base = pool / n;
                    (name.clone(), base.max(min_conns), 0.0)
                } else {
                    let exact = pool as f64 * size as f64 / total_size as f64;
                    let base = (exact.floor() as usize).max(min_conns);
                    (name.clone(), base, exact - base as f64)
                }
            })
            .collect();
        // Distribute remaining connections by largest fractional part
        let used: usize = allocs.iter().map(|(_, c, _)| *c).sum();
        let mut remaining = pool.saturating_sub(used);
        allocs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        for (_, c, _) in allocs.iter_mut() {
            if remaining == 0 {
                break;
            }
            *c += 1;
            remaining -= 1;
        }
        allocs.into_iter().map(|(name, c, _)| (name, c)).collect()
    };

    let semaphore = Arc::new(Semaphore::new(upgrade_concurrent_limit));
    let temp_dir = Arc::new(TempDir::new()?);

    let mut download_tasks: JoinSet<Result<PreDownloaded>> = JoinSet::new();
    for pkg in outdated.iter().filter(|pkg| !pkg.is_cask) {
        let Some(formula) = formula_by_name.get(pkg.name.as_str()) else {
            continue;
        };
        let Some(bottle_info) = formula.bottle.as_ref().and_then(|b| b.stable.as_ref()) else {
            continue;
        };
        let Some(bottle_file) = bottle_info
            .files
            .get(&platform)
            .or_else(|| bottle_info.files.get("all"))
        else {
            continue;
        };

        let url = bottle_file.url.clone();
        let sha256 = bottle_file.sha256.clone();
        let name = pkg.name.clone();
        let version = formula.versions.stable.clone();
        let rebuild = formula.bottle_rebuild();
        let dl = Arc::clone(&downloader);
        let sem = Arc::clone(&semaphore);
        let tmp = Arc::clone(&temp_dir);
        let multi_ref = multi.clone();
        let conns = upgrade_connections_map.get(&pkg.name).copied().unwrap_or(1);

        download_tasks.spawn(async move {
            let permit = sem.acquire().await.unwrap();
            crate::signal::check_cancelled()?;

            let tarball = tmp.path().join(format!("{}-{}.tar.gz", name, version));
            let pb = multi_ref.insert_from_back(1, ProgressBar::new(0));
            pb.set_style(
                ProgressStyle::default_bar()
                    .template(PROGRESS_BAR_TEMPLATE)
                    .unwrap()
                    .progress_chars(PROGRESS_BAR_CHARS),
            );
            pb.set_message(name.clone());

            dl.download(&url, &tarball, Some(&pb), conns, None).await?;
            pb.finish_and_clear();

            // Release the download permit before extraction.
            drop(permit);

            BottleDownloader::verify_checksum(&tarball, &sha256)?;

            let extract_dir = tmp.path().join(&name);
            BottleDownloader::extract(&tarball, &extract_dir)?;

            Ok::<_, WaxError>(PreDownloaded {
                name,
                version,
                extract_dir,
                bottle_sha: sha256,
                bottle_rebuild: rebuild,
                _temp_dir: tmp,
            })
        });
    }

    let mut pre_downloaded: HashMap<String, PreDownloaded> = HashMap::new();
    while let Some(task) = download_tasks.join_next().await {
        match task {
            Ok(Ok(d)) => {
                pre_downloaded.insert(d.name.clone(), d);
            }
            Ok(Err(e)) => {
                let _ = multi.println(format!("{} download failed: {}", style("✗").red(), e));
            }
            Err(e) => {
                let _ = multi.println(format!("{} task error: {}", style("✗").red(), e));
            }
        }
    }

    // --- Phase 1: serial uninstall + install using pre-downloaded bottles ---
    let install_state = InstallState::new()?;
    let install_mode_global = InstallMode::detect();

    let mut success_count = 0;
    let mut fail_count = 0;
    let mut failed_names: Vec<String> = Vec::new();

    for (i, pkg) in outdated.into_iter().enumerate() {
        check_cancelled()?;
        let _critical = CriticalSection::new();

        let label = format!("({}/{}) {}", i + 1, total, pkg.name);

        let spinner = multi.insert_from_back(1, ProgressBar::new_spinner());
        spinner.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars(SPINNER_TICK_CHARS),
        );
        spinner.enable_steady_tick(std::time::Duration::from_millis(80));
        set_current_op(format!("removing {}", pkg.name));
        spinner.set_message(format!(
            "{} removing {}...",
            style(&label).dim(),
            style(&pkg.name).magenta()
        ));

        let uninstall_result = if pkg.is_cask {
            Ok(())
        } else {
            uninstall::uninstall_quiet(cache, &pkg.name, false).await
        };
        spinner.finish_and_clear();

        let result = match uninstall_result {
            Ok(()) => {
                set_current_op(format!("installing {}", pkg.name));

                if pkg.is_cask {
                    // Reinstall in place so an interrupted run keeps the previous app.
                    let r = install::install_quiet_with_progress(
                        cache,
                        std::slice::from_ref(&pkg.name),
                        true,
                        false,
                        false,
                        &ProgressBar::hidden(),
                        true,
                    )
                    .await;
                    r
                } else if let Some(dl) = pre_downloaded.remove(&pkg.name) {
                    // Formula: use pre-downloaded bottle.
                    // Pass a spinner as existing_pb so step!() messages update
                    // it in-place instead of printing new lines.
                    let pkg_install_mode = pkg.install_mode.unwrap_or(install_mode_global);
                    let pkg_cellar = pkg_install_mode.cellar_path()?;
                    let install_pb = multi.insert_from_back(1, ProgressBar::new_spinner());
                    install_pb.set_style(
                        ProgressStyle::default_spinner()
                            .template("{spinner:.cyan} {msg}")
                            .unwrap()
                            .tick_chars(SPINNER_TICK_CHARS),
                    );
                    install_pb.enable_steady_tick(std::time::Duration::from_millis(80));
                    let r = install::install_extracted_bottle(
                        &dl.name,
                        &dl.version,
                        &dl.extract_dir,
                        dl.bottle_sha,
                        dl.bottle_rebuild,
                        &pkg_cellar,
                        pkg_install_mode,
                        &platform,
                        &install_state,
                        false,
                        Some(&multi),
                        Some(install_pb.clone()),
                    )
                    .await;
                    install_pb.finish_and_clear();
                    r
                } else {
                    // Fallback: bottle wasn't pre-downloaded (e.g. source-only)
                    let (user_flag, global_flag) = match pkg.install_mode {
                        Some(InstallMode::User) => (true, false),
                        Some(InstallMode::Global) => (false, true),
                        _ => (false, false),
                    };
                    let pb = multi.insert_from_back(1, ProgressBar::new(0));
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
                    let r = install::install_quiet_with_progress(
                        cache,
                        std::slice::from_ref(&pkg.name),
                        false,
                        user_flag,
                        global_flag,
                        &pb,
                        false,
                    )
                    .await;
                    pb.finish_and_clear();
                    r
                }
            }
            Err(e) => Err(e),
        };

        clear_current_op();

        match result {
            Ok(()) => {
                let cask_indicator = if pkg.is_cask {
                    format!(" {}", style("(cask)").yellow())
                } else {
                    String::new()
                };
                let _ = multi.println(format!(
                    "{} {}{} {} → {}",
                    style("✓").green(),
                    style(&pkg.name).magenta(),
                    cask_indicator,
                    style(&pkg.installed_version).dim(),
                    style(&pkg.latest_version).green()
                ));
                success_count += 1;
            }
            Err(e) => {
                fail_count += 1;
                let _ = multi.println(format!(
                    "{} {} failed: {}",
                    style("✗").red(),
                    style(&pkg.name).magenta(),
                    e
                ));
                failed_names.push(pkg.name.clone());
            }
        }
    }

    // Reinstall all affected dependents — each exactly once.
    if !dependents_to_reinstall.is_empty() {
        let _ = multi.println(format!(
            "  {} reinstalling {} dependent{}",
            style("→").cyan(),
            dep_total,
            if dep_total == 1 { "" } else { "s" },
        ));

        for dep_name in &dependents_to_reinstall {
            check_cancelled()?;

            let dep_mode = install_modes.get(dep_name).copied();
            let (user_flag, global_flag) = match dep_mode {
                Some(InstallMode::User) => (true, false),
                Some(InstallMode::Global) => (false, true),
                _ => (false, false),
            };

            let spinner = multi.insert_from_back(1, ProgressBar::new_spinner());
            spinner.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.cyan} {msg}")
                    .unwrap()
                    .tick_chars(SPINNER_TICK_CHARS),
            );
            spinner.enable_steady_tick(std::time::Duration::from_millis(80));
            set_current_op(format!("reinstalling {}", dep_name));
            spinner.set_message(format!("  reinstalling {}...", style(dep_name).magenta()));

            let result = async {
                uninstall::uninstall_quiet(cache, dep_name, false).await?;
                install::install_quiet(
                    cache,
                    std::slice::from_ref(dep_name),
                    false,
                    user_flag,
                    global_flag,
                )
                .await
            }
            .await;

            spinner.finish_and_clear();
            clear_current_op();

            match result {
                Ok(()) => {
                    let _ = multi.println(format!(
                        "  {} {} reinstalled",
                        style("✓").green(),
                        style(dep_name).magenta()
                    ));
                }
                Err(e) => {
                    let _ = multi.println(format!(
                        "  {} {} reinstall failed: {}",
                        style("✗").red(),
                        style(dep_name).magenta(),
                        e
                    ));
                }
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
    state.sync_from_cellar().await?;
    let installed_packages = state.load().await?;

    let installed = if let Some(pkg) = installed_packages.get(formula_name) {
        pkg.clone()
    } else {
        let cask_state = CaskState::new()?;
        let installed_casks = cask_state.load().await?;

        if installed_casks.contains_key(formula_name) {
            return upgrade_cask_single(cache, formula_name, dry_run).await;
        }

        let updated_packages = state.load().await?;

        if let Some(pkg) = updated_packages.get(formula_name).cloned() {
            pkg
        } else if formula_name == "wax" {
            if dry_run {
                println!(
                    "{}: {} → latest (self-update)",
                    style("wax").magenta(),
                    style(WAX_VERSION).dim()
                );
                println!("\ndry run - no changes made");
                return Ok(());
            }
            return self_update(Channel::Stable, false, None).await;
        } else {
            return Err(WaxError::NotInstalled(formula_name.to_string()));
        }
    };

    if installed.pinned {
        println!(
            "{}@{} is pinned — skipping (run `wax unpin {}` to allow upgrades)",
            style(formula_name).magenta(),
            style(&installed.version).dim(),
            formula_name
        );
        return Ok(());
    }

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
    let cask_summary = casks
        .iter()
        .find(|c| c.token == cask_name || c.full_token == cask_name)
        .ok_or_else(|| WaxError::CaskNotFound(cask_name.to_string()))?;

    let api_client = ApiClient::new();
    let cask_details = api_client.fetch_cask_details(&cask_summary.token).await?;

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

        let (user_flag, global_flag) = match dep_mode {
            Some(InstallMode::User) => (true, false),
            Some(InstallMode::Global) => (false, true),
            _ => (false, false),
        };

        let result = async {
            uninstall::uninstall_quiet(cache, dep_name, false).await?;
            install::install_quiet(
                cache,
                std::slice::from_ref(dep_name),
                false,
                user_flag,
                global_flag,
            )
            .await
        }
        .await;

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
    let formula_index: HashMap<_, _> = formulae.iter().map(|f| (f.name.as_str(), f)).collect();
    let cask_index: HashMap<_, _> = casks
        .iter()
        .map(|c| (c.token.as_str(), c))
        .chain(casks.iter().map(|c| (c.full_token.as_str(), c)))
        .collect();

    let mut outdated = Vec::new();

    let platform = detect_platform();
    for (name, installed) in &installed_packages {
        if installed.pinned {
            continue;
        }
        if let Some(formula) = formula_index.get(name.as_str()) {
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
                    } else if sha_outdated {
                        format!("{} (bottle updated)", latest)
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
        if let Some(cask) = cask_index.get(name.as_str()) {
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
