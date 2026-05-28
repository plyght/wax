use crate::api::ApiClient;
use crate::bottle::{detect_platform, BottleDownloader, DownloadTotals};
use crate::cache::Cache;
use crate::cask::{CaskState, InstalledCask};
use crate::commands::self_update::{self_update, Channel};
use crate::commands::{install, uninstall};
use crate::deps::find_installed_reverse_dependencies;
use crate::discovery::discover_manually_installed_casks;
use crate::error::{Result, WaxError};
use crate::install::{InstallMode, InstallState};
use crate::signal::{
    check_cancelled, clear_active_multi, clear_current_op, set_active_multi, set_current_op,
    CriticalSection,
};
use crate::tap::TapManager;
use crate::ui::{PROGRESS_BAR_CHARS, PROGRESS_BAR_TEMPLATE, SPINNER_TICK_CHARS};
use crate::version::{is_same_or_newer, WAX_VERSION};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::instrument;

#[derive(Debug, Clone)]
pub struct OutdatedPackage {
    pub name: String,
    pub installed_version: String,
    pub latest_version: String,
    pub is_cask: bool,
    pub install_mode: Option<InstallMode>,
}

struct PreDownloaded {
    name: String,
    version: String,
    extract_dir: std::path::PathBuf,
    bottle_sha: String,
    bottle_rebuild: u32,
    _temp_dir: Arc<TempDir>,
}

enum FormulaUpgradeMsg {
    Ready {
        pkg: OutdatedPackage,
        pre: PreDownloaded,
    },
    Fallback(OutdatedPackage),
    DownloadFailed {
        name: String,
        err: WaxError,
    },
}

struct UpgradeMultiGuard {
    owns_multi: bool,
}

impl UpgradeMultiGuard {
    fn new(owns_multi: bool) -> Self {
        Self { owns_multi }
    }
}

impl Drop for UpgradeMultiGuard {
    fn drop(&mut self) {
        clear_current_op();
        if self.owns_multi {
            clear_active_multi();
        }
    }
}

#[instrument(skip(cache))]
pub async fn upgrade(cache: &Cache, packages: &[String], dry_run: bool) -> Result<()> {
    let start = std::time::Instant::now();

    cache.ensure_fresh().await?;
    refresh_taps(cache).await?;

    if packages.is_empty() {
        upgrade_all(cache, dry_run, start).await
    } else {
        let installed_casks = sync_cask_state(cache).await?;
        let mut failed_names = Vec::new();
        for package in packages {
            if let Err(e) = if package == "wax" {
                upgrade_single(cache, package, dry_run).await
            } else if installed_casks.contains_key(package) {
                upgrade_cask_single(cache, package, dry_run).await
            } else {
                upgrade_single(cache, package, dry_run).await
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

async fn refresh_taps(cache: &Cache) -> Result<()> {
    let mut tap_manager = TapManager::new()?;
    tap_manager.load().await?;
    let taps = tap_manager
        .list_taps()
        .iter()
        .map(|tap| tap.full_name.clone())
        .collect::<Vec<_>>();

    for tap in taps {
        tap_manager.update_tap(&tap).await?;
        cache.invalidate_tap_cache(&tap).await?;
    }

    Ok(())
}

fn merge_discovered_casks(
    installed_casks: &mut HashMap<String, InstalledCask>,
    discovered_casks: HashMap<String, InstalledCask>,
    caskroom_synced_names: &HashSet<String>,
) {
    for (name, discovered) in discovered_casks {
        installed_casks
            .entry(name.clone())
            .and_modify(|installed| {
                if !caskroom_synced_names.contains(&name) && discovered.version != "unknown" {
                    installed.version = discovered.version.clone();
                }
                if !caskroom_synced_names.contains(&name) && discovered.install_date > 0 {
                    installed.install_date = discovered.install_date;
                }
                if installed.artifact_type.is_none() {
                    installed.artifact_type = discovered.artifact_type.clone();
                }
                if installed.binary_paths.is_none() {
                    installed.binary_paths = discovered.binary_paths.clone();
                }
                if installed.app_name.is_none() {
                    installed.app_name = discovered.app_name.clone();
                }
            })
            .or_insert(discovered);
    }
}

async fn sync_cask_state(cache: &Cache) -> Result<HashMap<String, InstalledCask>> {
    let cask_state = CaskState::new()?;
    let caskroom_synced_names = cask_state.sync_from_caskrooms().await?;

    let mut installed_casks = cask_state.load().await?;
    if cfg!(target_os = "macos") {
        let casks = cache.load_casks().await?;
        let discovered_casks = discover_manually_installed_casks(&casks).await?;
        merge_discovered_casks(
            &mut installed_casks,
            discovered_casks,
            &caskroom_synced_names,
        );
        cask_state.save(&installed_casks).await?;
    }

    Ok(installed_casks)
}

fn package_name_from_qualified_name(package_name: &str) -> &str {
    package_name.rsplit('/').next().unwrap_or(package_name)
}

async fn apply_one_formula_package_upgrade(
    cache: &Cache,
    multi: &MultiProgress,
    pkg: &OutdatedPackage,
    pre: Option<PreDownloaded>,
    install_mode_global: InstallMode,
    platform: &str,
    install_state: &InstallState,
) -> Result<()> {
    check_cancelled()?;

    let label = pkg.name.to_string();

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

    let uninstall_result = uninstall::uninstall_quiet(cache, &pkg.name, false).await;
    spinner.finish_and_clear();

    let result = match uninstall_result {
        Ok(()) => {
            set_current_op(format!("installing {}", pkg.name));

            if let Some(dl) = pre {
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
                    platform,
                    install_state,
                    false,
                    true,
                    Some(multi),
                    Some(install_pb.clone()),
                )
                .await;
                install_pb.finish_and_clear();
                r
            } else {
                let (user_flag, global_flag) = match pkg.install_mode {
                    Some(InstallMode::User) => (true, false),
                    Some(InstallMode::Global) => (false, true),
                    _ => (false, false),
                };
                let pb = multi.insert_from_back(1, ProgressBar::new(0));
                pb.set_style(
                    ProgressStyle::default_bar()
                        .template(&format!(
                            "{{spinner:.green}} {} {{wide_bar:.cyan/blue}} {{bytes}}/{{total_bytes}} {{bytes_per_sec}}",
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
    result
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
    let owns_multi_globals = crate::signal::clone_active_multi().is_none();
    if owns_multi_globals {
        set_active_multi(multi.clone());
    }
    let _guard = UpgradeMultiGuard::new(owns_multi_globals);

    let (cask_packages, formula_packages): (Vec<OutdatedPackage>, Vec<OutdatedPackage>) =
        outdated.into_iter().partition(|pkg| pkg.is_cask);
    let formula_total = formula_packages.len();

    // --- Phase 0: pre-download all formula bottles concurrently ---
    let platform = detect_platform();
    let formula_by_name: HashMap<&str, &crate::api::Formula> =
        formulae.iter().map(|f| (f.name.as_str(), f)).collect();

    let upgrade_formulae: Arc<HashMap<String, crate::api::Formula>> = Arc::new(
        formula_packages
            .iter()
            .filter_map(|p| {
                formula_by_name
                    .get(p.name.as_str())
                    .map(|f| (p.name.clone(), (*f).clone()))
            })
            .collect(),
    );

    let downloader = Arc::new(BottleDownloader::new());

    // Collect (name, url) for all formula bottles to be downloaded.
    let formula_bottle_urls: Vec<(String, String)> = formula_packages
        .iter()
        .filter_map(|pkg| {
            let formula = formula_by_name.get(pkg.name.as_str())?;
            let bottle_info = formula.bottle.as_ref()?.stable.as_ref()?;
            let bottle_file = bottle_info.file_for_platform(&platform)?;
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

    let formula_totals = Arc::new(DownloadTotals::default());
    let hide_formula_dl = Arc::new(AtomicBool::new(false));

    let overall_formula_pb = if formula_bottle_urls.len() > 1 {
        let pb = multi.insert(0, ProgressBar::new(0));
        pb.set_style(
            ProgressStyle::default_bar()
                .template(PROGRESS_BAR_TEMPLATE)
                .unwrap()
                .progress_chars(PROGRESS_BAR_CHARS),
        );
        pb.set_message("All formula downloads");
        Some(pb)
    } else {
        None
    };

    let update_formula_totals = if let Some(ref pb) = overall_formula_pb {
        let totals = formula_totals.clone();
        let hide = Arc::clone(&hide_formula_dl);
        let pb = pb.clone();
        Some(tokio::spawn(async move {
            loop {
                if hide.load(Ordering::Relaxed) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                if hide.load(Ordering::Relaxed) {
                    return;
                }
                let pos = totals.downloaded.load(Ordering::Relaxed);
                let len = totals.expected.load(Ordering::Relaxed);
                let cap = len.max(pos).max(1);
                pb.set_length(cap);
                pb.set_position(pos);
            }
        }))
    } else {
        None
    };

    let ch_cap = formula_total.clamp(1, 64);
    let (tx, mut rx) = mpsc::channel::<FormulaUpgradeMsg>(ch_cap);

    let install_state = InstallState::new()?;
    let install_mode_global = InstallMode::detect();

    let cask_only_total_display = total.saturating_sub(formula_total);

    let hide_dl = Arc::clone(&hide_formula_dl);
    let poller_task = update_formula_totals;
    let overall_pb_done = overall_formula_pb.clone();

    let connection_map_for_producer = upgrade_connections_map.clone();
    let producer_tx = tx.clone();
    let formula_packages_for_producer = formula_packages.clone();
    let upgrade_formulae_for_producer = Arc::clone(&upgrade_formulae);
    let platform_for_producer = platform.clone();
    let multi_for_producer = multi.clone();
    let producer_handle = tokio::spawn(async move {
        let mut producer_js: JoinSet<std::result::Result<(), WaxError>> = JoinSet::new();
        for pkg in formula_packages_for_producer.iter().cloned() {
            let tx = producer_tx.clone();
            let sem = Arc::clone(&semaphore);
            let tmp = Arc::clone(&temp_dir);
            let multi_ref = multi_for_producer.clone();
            let dl = Arc::clone(&downloader);
            let totals = Arc::clone(&formula_totals);
            let platform_s = platform_for_producer.clone();
            let conns = connection_map_for_producer
                .get(&pkg.name)
                .copied()
                .unwrap_or(1);
            let formula_opt = upgrade_formulae_for_producer.get(&pkg.name).cloned();

            if formula_opt.is_none() {
                producer_js.spawn(async move {
                    let _ = tx.send(FormulaUpgradeMsg::Fallback(pkg)).await;
                    Ok::<(), WaxError>(())
                });
                continue;
            }
            let formula = formula_opt.unwrap();
            let Some(bottle_info) = formula.bottle.as_ref().and_then(|b| b.stable.as_ref()) else {
                producer_js.spawn(async move {
                    let _ = tx.send(FormulaUpgradeMsg::Fallback(pkg)).await;
                    Ok::<(), WaxError>(())
                });
                continue;
            };
            let Some(bottle_file) = bottle_info.file_for_platform(&platform_s) else {
                producer_js.spawn(async move {
                    let _ = tx.send(FormulaUpgradeMsg::Fallback(pkg)).await;
                    Ok::<(), WaxError>(())
                });
                continue;
            };

            let url = bottle_file.url.clone();
            let sha256 = bottle_file.sha256.clone();
            let name = pkg.name.clone();
            let version = formula.versions.stable.clone();
            let rebuild = formula.bottle_rebuild();

            producer_js.spawn(async move {
                let task_name = name.clone();
                let inner = async {
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

                    let download_result = dl
                        .download(&url, &tarball, Some(&pb), conns, Some(totals.as_ref()))
                        .await;
                    pb.finish_and_clear();
                    download_result?;

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
                }
                .await;

                match inner {
                    Ok(pre) => {
                        let _ = tx.send(FormulaUpgradeMsg::Ready { pkg, pre }).await;
                    }
                    Err(e) => {
                        let _ = tx
                            .send(FormulaUpgradeMsg::DownloadFailed {
                                name: task_name,
                                err: e,
                            })
                            .await;
                    }
                }
                Ok::<(), WaxError>(())
            });
        }

        while let Some(task_res) = producer_js.join_next().await {
            task_res.map_err(|e| {
                WaxError::InstallError(format!(
                    "download worker failed before upgrade started: {}",
                    e
                ))
            })??;
        }

        drop(producer_tx);
        hide_dl.store(true, Ordering::SeqCst);
        if let Some(poller) = poller_task {
            let _ = poller.await;
        }
        if let Some(pb) = overall_pb_done {
            pb.finish_and_clear();
        }

        Ok::<(), WaxError>(())
    });
    drop(tx);

    let formula_stats = {
        let cache = cache.clone();
        let multi = multi.clone();
        let platform = platform.clone();
        async move {
            let mut succ = 0usize;
            let mut fail = 0usize;
            let mut fails: Vec<String> = Vec::new();
            while let Some(msg) = rx.recv().await {
                check_cancelled()?;
                match msg {
                    FormulaUpgradeMsg::DownloadFailed { name, err } => {
                        let _ = multi.println(format!(
                            "{} {} download failed: {}",
                            style("✗").red(),
                            style(&name).magenta(),
                            err
                        ));
                        fail += 1;
                        fails.push(name);
                    }
                    FormulaUpgradeMsg::Fallback(pkg) => {
                        match apply_one_formula_package_upgrade(
                            &cache,
                            &multi,
                            &pkg,
                            None,
                            install_mode_global,
                            &platform,
                            &install_state,
                        )
                        .await
                        {
                            Ok(()) => {
                                let _ = multi.println(format!(
                                    "{} {} {} → {}",
                                    style("✓").green(),
                                    style(&pkg.name).magenta(),
                                    style(&pkg.installed_version).dim(),
                                    style(&pkg.latest_version).green()
                                ));
                                succ += 1;
                            }
                            Err(e) => {
                                fail += 1;
                                let _ = multi.println(format!(
                                    "{} {} failed: {}",
                                    style("✗").red(),
                                    style(&pkg.name).magenta(),
                                    e
                                ));
                                fails.push(pkg.name.clone());
                            }
                        }
                    }
                    FormulaUpgradeMsg::Ready { pkg, pre } => {
                        match apply_one_formula_package_upgrade(
                            &cache,
                            &multi,
                            &pkg,
                            Some(pre),
                            install_mode_global,
                            &platform,
                            &install_state,
                        )
                        .await
                        {
                            Ok(()) => {
                                let _ = multi.println(format!(
                                    "{} {} {} → {}",
                                    style("✓").green(),
                                    style(&pkg.name).magenta(),
                                    style(&pkg.installed_version).dim(),
                                    style(&pkg.latest_version).green()
                                ));
                                succ += 1;
                            }
                            Err(e) => {
                                fail += 1;
                                let _ = multi.println(format!(
                                    "{} {} failed: {}",
                                    style("✗").red(),
                                    style(&pkg.name).magenta(),
                                    e
                                ));
                                fails.push(pkg.name.clone());
                            }
                        }
                    }
                }
            }
            producer_handle.await.map_err(|e| {
                WaxError::InstallError(format!("formula upgrade producer task: {}", e))
            })??;
            Ok::<_, WaxError>((succ, fail, fails))
        }
    };

    let cask_fut = {
        let cache = cache.clone();
        let multi = multi.clone();
        async move {
            let mut c_succ = 0usize;
            let mut c_fail = 0usize;
            let mut c_failed: Vec<String> = Vec::new();
            for (i, pkg) in cask_packages.into_iter().enumerate() {
                check_cancelled()?;
                let label = format!(
                    "({}/{}) {}",
                    i + 1,
                    cask_only_total_display.max(1),
                    pkg.name
                );
                let spinner = multi.insert_from_back(1, ProgressBar::new_spinner());
                spinner.set_style(
                    ProgressStyle::default_spinner()
                        .template("{spinner:.cyan} {msg}")
                        .unwrap()
                        .tick_chars(SPINNER_TICK_CHARS),
                );
                spinner.enable_steady_tick(std::time::Duration::from_millis(80));
                set_current_op(format!("upgrading {}", pkg.name));
                spinner.set_message(format!(
                    "{} upgrading {}...",
                    style(&label).dim(),
                    style(&pkg.name).magenta()
                ));

                let r = install::install_quiet_force(
                    &cache,
                    std::slice::from_ref(&pkg.name),
                    true,
                    false,
                    false,
                )
                .await;
                spinner.finish_and_clear();
                clear_current_op();

                match r {
                    Ok(()) => {
                        c_succ += 1;
                        let _ = multi.println(format!(
                            "{} {} {} {} → {}",
                            style("✓").green(),
                            style(&pkg.name).magenta(),
                            style("(cask)").yellow(),
                            style(&pkg.installed_version).dim(),
                            style(&pkg.latest_version).green()
                        ));
                    }
                    Err(e) => {
                        c_fail += 1;
                        let _ = multi.println(format!(
                            "{} {} failed: {}",
                            style("✗").red(),
                            style(&pkg.name).magenta(),
                            e
                        ));
                        c_failed.push(pkg.name);
                    }
                }
            }
            Ok::<_, WaxError>((c_succ, c_fail, c_failed))
        }
    };

    let ((mut success_count, mut fail_count, mut failed_names), (c_succ, c_fail, c_failed)) = {
        let _critical = CriticalSection::new();
        tokio::try_join!(formula_stats, cask_fut)?
    };
    success_count += c_succ;
    fail_count += c_fail;
    failed_names.extend(c_failed);

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
                    fail_count += 1;
                    let _ = multi.println(format!(
                        "  {} {} reinstall failed: {}",
                        style("✗").red(),
                        style(dep_name).magenta(),
                        e
                    ));
                    failed_names.push(dep_name.clone());
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
    let installed_name = package_name_from_qualified_name(formula_name);

    let installed = if let Some(pkg) = installed_packages
        .get(formula_name)
        .or_else(|| installed_packages.get(installed_name))
    {
        pkg.clone()
    } else {
        let installed_casks = sync_cask_state(cache).await?;

        if installed_casks.contains_key(formula_name)
            || installed_casks.contains_key(installed_name)
        {
            return upgrade_cask_single(cache, installed_name, dry_run).await;
        }

        let updated_packages = state.load().await?;

        if let Some(pkg) = updated_packages
            .get(formula_name)
            .or_else(|| updated_packages.get(installed_name))
            .cloned()
        {
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
            installed_name
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
            "{} is already on the latest version ({}).",
            style(formula_name).magenta(),
            style(installed_version).dim()
        );
        if dry_run {
            println!("\ndry run - no changes made");
        }
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

    println!(
        "upgrading {}: {} → {}",
        style(formula_name).magenta(),
        style(installed_version).dim(),
        style(&latest_version).green()
    );

    upgrade_formula_internal(
        cache,
        &installed.name,
        &formula.full_name,
        Some(installed.install_mode),
    )
    .await?;

    println!(
        "{} {} upgraded",
        style("✓").green(),
        style(formula_name).magenta()
    );

    Ok(())
}

async fn upgrade_cask_single(cache: &Cache, cask_name: &str, dry_run: bool) -> Result<()> {
    let installed_casks = sync_cask_state(cache).await?;

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
            "{} {} is already on the latest version ({}).",
            style(cask_name).magenta(),
            style("(cask)").yellow(),
            style(installed_version).dim()
        );
        if dry_run {
            println!("\ndry run - no changes made");
        }
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

    println!(
        "upgrading {} {}: {} → {}",
        style(cask_name).magenta(),
        style("(cask)").yellow(),
        style(installed_version).dim(),
        style(latest_version).green()
    );

    upgrade_cask_internal(cache, cask_name).await?;

    println!(
        "{} {} {} upgraded",
        style("✓").green(),
        style(cask_name).magenta(),
        style("(cask)").yellow()
    );

    Ok(())
}

async fn upgrade_formula_internal(
    cache: &Cache,
    installed_name: &str,
    formula_name: &str,
    install_mode: Option<InstallMode>,
) -> Result<()> {
    let _critical = CriticalSection::new();

    uninstall::uninstall_quiet(cache, installed_name, false).await?;

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

    install::install_quiet_force(cache, &[cask_name.to_string()], true, false, false).await?;

    Ok(())
}

pub async fn get_outdated_packages(cache: &Cache) -> Result<Vec<OutdatedPackage>> {
    let state = InstallState::new()?;
    state.sync_from_cellar().await?;
    let installed_packages = state.load().await?;

    let installed_casks = sync_cask_state(cache).await?;

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
                    .and_then(|s| s.file_for_platform(&platform))
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

#[cfg(test)]
mod tests {
    use super::{merge_discovered_casks, package_name_from_qualified_name};
    use crate::cask::InstalledCask;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn package_name_from_qualified_name_uses_last_segment() {
        assert_eq!(
            package_name_from_qualified_name("undivisible/tap/vro"),
            "vro"
        );
        assert_eq!(package_name_from_qualified_name("vro"), "vro");
    }

    #[test]
    fn merge_discovered_casks_updates_existing_versions() {
        let mut installed = HashMap::from([(
            "example-cask".to_string(),
            InstalledCask {
                name: "example-cask".to_string(),
                version: "1.0.0".to_string(),
                install_date: 1,
                artifact_type: Some("dmg".to_string()),
                binary_paths: None,
                app_name: Some("Example.app".to_string()),
            },
        )]);
        let discovered = HashMap::from([(
            "example-cask".to_string(),
            InstalledCask {
                name: "example-cask".to_string(),
                version: "2.0.0".to_string(),
                install_date: 2,
                artifact_type: Some("app".to_string()),
                binary_paths: None,
                app_name: Some("Example".to_string()),
            },
        )]);

        merge_discovered_casks(&mut installed, discovered, &HashSet::new());

        let cask = installed.get("example-cask").unwrap();
        assert_eq!(cask.version, "2.0.0");
        assert_eq!(cask.install_date, 2);
        assert_eq!(cask.artifact_type.as_deref(), Some("dmg"));
        assert_eq!(cask.app_name.as_deref(), Some("Example.app"));
    }

    #[test]
    fn merge_discovered_casks_preserves_caskroom_synced_versions() {
        let mut installed = HashMap::from([(
            "example-cask".to_string(),
            InstalledCask {
                name: "example-cask".to_string(),
                version: "2.0.0".to_string(),
                install_date: 2,
                artifact_type: Some("dmg".to_string()),
                binary_paths: None,
                app_name: Some("Example.app".to_string()),
            },
        )]);
        let discovered = HashMap::from([(
            "example-cask".to_string(),
            InstalledCask {
                name: "example-cask".to_string(),
                version: "1.0.0".to_string(),
                install_date: 1,
                artifact_type: Some("app".to_string()),
                binary_paths: None,
                app_name: Some("Example".to_string()),
            },
        )]);

        merge_discovered_casks(
            &mut installed,
            discovered,
            &HashSet::from(["example-cask".to_string()]),
        );

        let cask = installed.get("example-cask").unwrap();
        assert_eq!(cask.version, "2.0.0");
        assert_eq!(cask.install_date, 2);
    }
}
