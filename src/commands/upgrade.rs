use crate::api::ApiClient;
use crate::bottle::{detect_platform, homebrew_prefix, BottleDownloader, DownloadTotals};
use crate::cache::Cache;
use crate::cask::{CaskState, InstalledCask};
use crate::commands::self_update::{self_update, Channel};
use crate::commands::{install, uninstall};
use crate::discovery::{discover_manually_installed_casks, normalize_package_token};
use crate::error::{Result, WaxError};
use crate::install::{is_writable, InstallMode, InstallState};
use crate::signal::{
    check_cancelled, clear_active_multi, clear_current_op, set_active_multi, set_current_op,
    CriticalSection,
};
use crate::tap::TapManager;
use crate::ui::{
    confirm_prompt, PROGRESS_BAR_CHARS, PROGRESS_BAR_PREFIX_TEMPLATE, PROGRESS_BAR_TEMPLATE,
    SPINNER_TICK_CHARS,
};
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
        bar: ProgressBar,
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
pub async fn upgrade(
    cache: &Cache,
    packages: &[String],
    dry_run: bool,
    ask: bool,
    scope: Option<InstallMode>,
) -> Result<()> {
    let start = std::time::Instant::now();

    cache.ensure_fresh().await?;
    refresh_taps(cache).await?;

    if packages.is_empty() {
        upgrade_all(cache, dry_run, ask, start, scope).await
    } else {
        let installed_casks = sync_cask_state(cache).await?;
        if ask && !dry_run {
            for package in packages {
                if package == "wax" {
                    upgrade_single(cache, package, true).await?;
                } else if installed_casks.contains_key(package) {
                    upgrade_cask_single(cache, package, true).await?;
                } else {
                    upgrade_single(cache, package, true).await?;
                }
            }
            let proceed = confirm_prompt("Proceed with upgrade?")?;
            if !proceed {
                println!("{} upgrade cancelled", style("✗").red());
                return Ok(());
            }
        }
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
        if let Some(app_key) = manual_app_key(&discovered) {
            let stale_names = installed_casks
                .iter()
                .filter_map(|(installed_name, installed)| {
                    if installed_name == &name || caskroom_synced_names.contains(installed_name) {
                        return None;
                    }
                    (manual_app_key(installed).as_deref() == Some(app_key.as_str()))
                        .then(|| installed_name.clone())
                })
                .collect::<Vec<_>>();
            for stale_name in stale_names {
                installed_casks.remove(&stale_name);
            }
        }

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

fn manual_app_key(cask: &InstalledCask) -> Option<String> {
    if cask.artifact_type.as_deref() != Some("app") {
        return None;
    }

    cask.app_name
        .as_deref()
        .map(normalize_package_token)
        .filter(|name| !name.is_empty())
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

fn cask_failed_names_from_error(err: &WaxError) -> HashSet<String> {
    let message = err.to_string();
    message
        .strip_prefix("Install error: Some casks failed: ")
        .or_else(|| message.strip_prefix("Some casks failed: "))
        .map(|names| {
            names
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
async fn apply_one_formula_package_upgrade(
    cache: &Cache,
    multi: &MultiProgress,
    pkg: &OutdatedPackage,
    pre: Option<PreDownloaded>,
    install_bar: Option<ProgressBar>,
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
                let install_pb = if let Some(bar) = install_bar {
                    install::reuse_download_bar_as_install_spinner(&bar, &dl.name);
                    bar
                } else {
                    let install_pb = multi.insert_from_back(1, ProgressBar::new_spinner());
                    install_pb.set_style(
                        ProgressStyle::default_spinner()
                            .template("{spinner:.cyan} {msg}")
                            .unwrap()
                            .tick_chars(SPINNER_TICK_CHARS),
                    );
                    install_pb.enable_steady_tick(std::time::Duration::from_millis(80));
                    install_pb
                };
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
                let r = install::install_impl(
                    cache,
                    std::slice::from_ref(&pkg.name),
                    install::InstallArgs {
                        dry_run: false,
                        ask: false,
                        cask: false,
                        user: user_flag,
                        global: global_flag,
                        build_from_source: false,
                        head: false,
                        run_scripts: true,
                        quiet: true,
                        force_reinstall: false,
                        external_pb: Some(&pb),
                    },
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

async fn upgrade_all(
    cache: &Cache,
    dry_run: bool,
    ask: bool,
    start: std::time::Instant,
    scope: Option<InstallMode>,
) -> Result<()> {
    let outdated = get_outdated_packages_scoped(cache, scope).await?;

    if outdated.is_empty() {
        println!("all packages are up to date");
        if crate::ui::timing_enabled() {
            println!("\n[{}ms] done", start.elapsed().as_millis());
        }
        return Ok(());
    }

    let global_count = outdated
        .iter()
        .filter(|pkg| pkg.install_mode == Some(InstallMode::Global) || pkg.is_cask)
        .count();
    if !dry_run && global_count > 0 {
        let prefix = homebrew_prefix();
        if !is_writable(&prefix) {
            return Err(WaxError::InstallError(format!(
                "global upgrade blocked\n  {} {} global package{} would be changed\n  {} {} is not writable by {}\n\ntry:\n  wax upgrade --user\n  wax upgrade --dry-run\n  run global upgrades as the Homebrew-owning user",
                style("→").cyan(),
                global_count,
                if global_count == 1 { "" } else { "s" },
                style("→").cyan(),
                prefix.display(),
                std::env::var("USER").unwrap_or_else(|_| "this user".to_string())
            )));
        }
    }

    if dry_run || ask {
        println!();
        println!("{} upgrade plan", style("→").cyan().bold());
        for pkg in &outdated {
            let cask_indicator = if pkg.is_cask {
                format!(" {}", style("(cask)").yellow())
            } else {
                String::new()
            };
            println!(
                "  {} {}{} {} {}",
                style("↻").cyan(),
                style(&pkg.name).magenta(),
                cask_indicator,
                style(&pkg.installed_version).dim(),
                style(format!("→ {}", pkg.latest_version)).green()
            );
        }
        if dry_run {
            println!("\n{}", style("dry run - no changes made").dim());
            return Ok(());
        }
        let proceed = confirm_prompt("Proceed with upgrade?")?;
        if !proceed {
            println!("{} upgrade cancelled", style("✗").red());
            return Ok(());
        }
    }

    let formulae = cache.load_all_formulae().await?;

    let total = outdated.len();

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
                    pb.finish_and_clear();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                if hide.load(Ordering::Relaxed) {
                    pb.finish_and_clear();
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

    let formula_download_bars: HashMap<String, ProgressBar> = formula_packages
        .iter()
        .map(|pkg| {
            let pb = multi.add(ProgressBar::new(0));
            pb.set_style(
                ProgressStyle::default_bar()
                    .template(PROGRESS_BAR_PREFIX_TEMPLATE)
                    .unwrap()
                    .progress_chars(PROGRESS_BAR_CHARS),
            );
            pb.set_prefix(pkg.name.clone());
            (pkg.name.clone(), pb)
        })
        .collect();

    let connection_map_for_producer = upgrade_connections_map.clone();
    let producer_tx = tx.clone();
    let formula_packages_for_producer = formula_packages.clone();
    let upgrade_formulae_for_producer = Arc::clone(&upgrade_formulae);
    let platform_for_producer = platform.clone();
    let multi_for_producer = multi.clone();
    let formula_download_bars_for_producer = formula_download_bars.clone();
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
            let pb = formula_download_bars_for_producer
                .get(&name)
                .cloned()
                .unwrap_or_else(|| multi_ref.add(ProgressBar::new(0)));

            producer_js.spawn(async move {
                let task_name = name.clone();
                let inner = async {
                    let permit = sem.acquire().await.unwrap();
                    crate::signal::check_cancelled()?;

                    let tarball = tmp.path().join(format!("{}-{}.tar.gz", name, version));

                    let download_result = dl
                        .download(&url, &tarball, Some(&pb), conns, Some(totals.as_ref()))
                        .await;
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
                        let _ = tx
                            .send(FormulaUpgradeMsg::Ready { pkg, pre, bar: pb })
                            .await;
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
                    FormulaUpgradeMsg::Ready { pkg, pre, bar } => {
                        match apply_one_formula_package_upgrade(
                            &cache,
                            &multi,
                            &pkg,
                            Some(pre),
                            Some(bar),
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
            if !cask_packages.is_empty() {
                check_cancelled()?;
                let cask_names: Vec<String> =
                    cask_packages.iter().map(|p| p.name.clone()).collect();
                set_current_op(format!(
                    "upgrading {} casks",
                    cask_only_total_display.max(1)
                ));
                let r = install::install_impl(
                    &cache,
                    &cask_names,
                    install::InstallArgs {
                        dry_run: false,
                        ask: false,
                        cask: true,
                        user: false,
                        global: false,
                        build_from_source: false,
                        head: false,
                        run_scripts: true,
                        quiet: true,
                        force_reinstall: true,
                        external_pb: None,
                    },
                )
                .await;
                clear_current_op();

                match r {
                    Ok(()) => {
                        for pkg in cask_packages {
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
                    }
                    Err(e) => {
                        let failed_set = cask_failed_names_from_error(&e);
                        if failed_set.is_empty() {
                            c_fail += cask_packages.len();
                            for pkg in cask_packages {
                                let _ = multi.println(format!(
                                    "{} {} failed: {}",
                                    style("✗").red(),
                                    style(&pkg.name).magenta(),
                                    e
                                ));
                                c_failed.push(pkg.name);
                            }
                        } else {
                            for pkg in cask_packages {
                                if failed_set.contains(&pkg.name) {
                                    c_fail += 1;
                                    let _ = multi.println(format!(
                                        "{} {} failed: {}",
                                        style("✗").red(),
                                        style(&pkg.name).magenta(),
                                        e
                                    ));
                                    c_failed.push(pkg.name);
                                } else {
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
                            }
                        }
                    }
                }
            }
            Ok::<_, WaxError>((c_succ, c_fail, c_failed))
        }
    };

    let (mut success_count, mut fail_count, mut failed_names) = {
        let _critical = CriticalSection::new();
        formula_stats.await?
    };

    let (c_succ, c_fail, c_failed) = {
        let _critical = CriticalSection::new();
        cask_fut.await?
    };
    success_count += c_succ;
    fail_count += c_fail;
    failed_names.extend(c_failed);

    let elapsed = start.elapsed();
    if fail_count > 0 {
        println!(
            "\n{} upgraded, {} failed{}",
            style(success_count).green(),
            style(fail_count).red(),
            crate::ui::elapsed_suffix(elapsed)
        );
    } else {
        println!(
            "\n{} package{} upgraded{}",
            style(success_count).green(),
            if success_count == 1 { "" } else { "s" },
            crate::ui::elapsed_suffix(elapsed)
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

    let formula_names = vec![formula_name.to_string()];
    install::install_impl(
        cache,
        &formula_names,
        install::InstallArgs {
            dry_run: false,
            ask: false,
            cask: false,
            user: user_flag,
            global: global_flag,
            build_from_source: false,
            head: false,
            run_scripts: true,
            quiet: true,
            force_reinstall: false,
            external_pb: None,
        },
    )
    .await?;

    Ok(())
}

async fn upgrade_cask_internal(cache: &Cache, cask_name: &str) -> Result<()> {
    let _critical = CriticalSection::new();

    let cask_names = vec![cask_name.to_string()];
    install::install_impl(
        cache,
        &cask_names,
        install::InstallArgs {
            dry_run: false,
            ask: false,
            cask: true,
            user: false,
            global: false,
            build_from_source: false,
            head: false,
            run_scripts: true,
            quiet: true,
            force_reinstall: true,
            external_pb: None,
        },
    )
    .await?;

    Ok(())
}

async fn load_packages_from_scope(
    mode: InstallMode,
) -> Result<HashMap<String, crate::install::InstalledPackage>> {
    let cellar = match mode {
        InstallMode::User => crate::ui::dirs::home_dir()?
            .join(".local")
            .join("wax")
            .join("Cellar"),
        InstallMode::Global => homebrew_prefix().join("Cellar"),
    };
    let mut packages = HashMap::new();
    if !cellar.exists() {
        return Ok(packages);
    }
    let mut entries = tokio::fs::read_dir(&cellar).await?;
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let mut versions = Vec::new();
        let mut version_entries = tokio::fs::read_dir(entry.path()).await?;
        while let Some(version_entry) = version_entries.next_entry().await? {
            if version_entry.file_type().await?.is_dir() {
                versions.push(version_entry.file_name().to_string_lossy().to_string());
            }
        }
        if versions.is_empty() {
            continue;
        }
        crate::version::sort_versions(&mut versions);
        packages.insert(
            name.clone(),
            crate::install::InstalledPackage {
                name,
                version: versions.last().cloned().unwrap_or_default(),
                platform: detect_platform(),
                install_date: 0,
                install_mode: mode,
                from_source: false,
                bottle_rebuild: 0,
                bottle_sha256: None,
                pinned: false,
            },
        );
    }
    Ok(packages)
}

pub async fn get_outdated_packages(cache: &Cache) -> Result<Vec<OutdatedPackage>> {
    get_outdated_packages_scoped(cache, None).await
}

pub async fn get_outdated_packages_scoped(
    cache: &Cache,
    scope: Option<InstallMode>,
) -> Result<Vec<OutdatedPackage>> {
    let state = InstallState::new()?;
    state.sync_from_cellar().await?;
    let installed_packages = if let Some(mode) = scope {
        load_packages_from_scope(mode).await?
    } else {
        state.load().await?
    };

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
        if scope.is_some() && Some(installed.install_mode) != scope {
            continue;
        }
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
    if scope == Some(InstallMode::User) {
        outdated.sort_by(|a, b| a.name.cmp(&b.name));
        return Ok(outdated);
    }
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

    #[test]
    fn merge_discovered_casks_replaces_stale_manual_app_token() {
        let mut installed = HashMap::from([(
            "example".to_string(),
            InstalledCask {
                name: "example".to_string(),
                version: "1.0.0".to_string(),
                install_date: 1,
                artifact_type: Some("app".to_string()),
                binary_paths: None,
                app_name: Some("Example".to_string()),
            },
        )]);
        let discovered = HashMap::from([(
            "vendor-example".to_string(),
            InstalledCask {
                name: "vendor-example".to_string(),
                version: "2.0.0".to_string(),
                install_date: 2,
                artifact_type: Some("app".to_string()),
                binary_paths: None,
                app_name: Some("Example.app".to_string()),
            },
        )]);

        merge_discovered_casks(&mut installed, discovered, &HashSet::new());

        assert!(!installed.contains_key("example"));
        assert_eq!(installed.get("vendor-example").unwrap().version, "2.0.0");
    }

    #[test]
    fn merge_discovered_casks_keeps_caskroom_synced_same_app_token() {
        let mut installed = HashMap::from([(
            "example".to_string(),
            InstalledCask {
                name: "example".to_string(),
                version: "1.0.0".to_string(),
                install_date: 1,
                artifact_type: Some("app".to_string()),
                binary_paths: None,
                app_name: Some("Example".to_string()),
            },
        )]);
        let discovered = HashMap::from([(
            "vendor-example".to_string(),
            InstalledCask {
                name: "vendor-example".to_string(),
                version: "2.0.0".to_string(),
                install_date: 2,
                artifact_type: Some("app".to_string()),
                binary_paths: None,
                app_name: Some("Example".to_string()),
            },
        )]);

        merge_discovered_casks(
            &mut installed,
            discovered,
            &HashSet::from(["example".to_string()]),
        );

        assert!(installed.contains_key("example"));
        assert!(installed.contains_key("vendor-example"));
    }

    static HOME_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_get_outdated_packages() {
        let _lock = HOME_MUTEX.lock().unwrap();
        let original_home = std::env::var_os("HOME");

        use crate::api::{BottleFile, BottleInfo, BottleStable, Formula, Versions};
        use crate::cache::Cache;
        use crate::commands::upgrade::get_outdated_packages;
        use crate::install::{InstallMode, InstalledPackage};
        use std::collections::HashMap;
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        std::env::set_var("HOME", dir.path());

        let wax_dir = dir.path().join(".wax");
        let cache_dir = wax_dir.join("cache");
        fs::create_dir_all(&cache_dir).unwrap();
        let cellar_dir = dir.path().join(".local/wax/Cellar");
        for (name, version) in [
            ("pkg-uptodate", "1.0.0"),
            ("pkg-version", "1.0.0"),
            ("pkg-rebuild", "1.0.0"),
            ("pkg-sha", "1.0.0"),
            ("pkg-pinned", "1.0.0"),
        ] {
            fs::create_dir_all(cellar_dir.join(name).join(version)).unwrap();
        }

        let mut installed = HashMap::new();

        installed.insert(
            "pkg-uptodate".to_string(),
            InstalledPackage {
                name: "pkg-uptodate".to_string(),
                version: "1.0.0".to_string(),
                platform: "arm64_mac".to_string(),
                install_date: 0,
                install_mode: InstallMode::Global,
                from_source: false,
                bottle_rebuild: 0,
                bottle_sha256: Some("sha1".to_string()),
                pinned: false,
            },
        );

        installed.insert(
            "pkg-version".to_string(),
            InstalledPackage {
                name: "pkg-version".to_string(),
                version: "1.0.0".to_string(),
                platform: "arm64_mac".to_string(),
                install_date: 0,
                install_mode: InstallMode::Global,
                from_source: false,
                bottle_rebuild: 0,
                bottle_sha256: Some("sha1".to_string()),
                pinned: false,
            },
        );

        installed.insert(
            "pkg-rebuild".to_string(),
            InstalledPackage {
                name: "pkg-rebuild".to_string(),
                version: "1.0.0".to_string(),
                platform: "arm64_mac".to_string(),
                install_date: 0,
                install_mode: InstallMode::Global,
                from_source: false,
                bottle_rebuild: 0,
                bottle_sha256: Some("sha1".to_string()),
                pinned: false,
            },
        );

        installed.insert(
            "pkg-sha".to_string(),
            InstalledPackage {
                name: "pkg-sha".to_string(),
                version: "1.0.0".to_string(),
                platform: "arm64_mac".to_string(),
                install_date: 0,
                install_mode: InstallMode::Global,
                from_source: false,
                bottle_rebuild: 0,
                bottle_sha256: Some("sha_old".to_string()),
                pinned: false,
            },
        );

        installed.insert(
            "pkg-pinned".to_string(),
            InstalledPackage {
                name: "pkg-pinned".to_string(),
                version: "1.0.0".to_string(),
                platform: "arm64_mac".to_string(),
                install_date: 0,
                install_mode: InstallMode::Global,
                from_source: false,
                bottle_rebuild: 0,
                bottle_sha256: Some("sha1".to_string()),
                pinned: true,
            },
        );

        let installed_json = serde_json::to_string(&installed).unwrap();
        fs::write(wax_dir.join("installed.json"), installed_json).unwrap();
        fs::write(wax_dir.join("installed_casks.json"), "{}").unwrap();

        let mut formulae = Vec::new();

        let make_formula = |name: &str, version: &str, rebuild: u32, sha: &str| {
            let mut files = HashMap::new();
            files.insert(
                "all".to_string(),
                BottleFile {
                    url: "http://example.com".to_string(),
                    sha256: sha.to_string(),
                },
            );
            Formula {
                name: name.to_string(),
                full_name: name.to_string(),
                desc: None,
                homepage: "".to_string(),
                versions: Versions {
                    stable: version.to_string(),
                    bottle: true,
                },
                revision: 0,
                installed: None,
                dependencies: None,
                build_dependencies: None,
                bottle: Some(BottleInfo {
                    stable: Some(BottleStable { rebuild, files }),
                }),
                deprecated: false,
                disabled: false,
                deprecation_reason: None,
                disable_reason: None,
                post_install_defined: false,
                rb_path: None,
                keg_only: None,
                keg_only_reason: None,
            }
        };

        formulae.push(make_formula("pkg-uptodate", "1.0.0", 0, "sha1"));
        formulae.push(make_formula("pkg-version", "2.0.0", 0, "sha1"));
        formulae.push(make_formula("pkg-rebuild", "1.0.0", 1, "sha1"));
        formulae.push(make_formula("pkg-sha", "1.0.0", 0, "sha_new"));
        formulae.push(make_formula("pkg-pinned", "2.0.0", 0, "sha1"));

        let formulae_json = serde_json::to_string(&formulae).unwrap();
        fs::write(cache_dir.join("formulae.json"), formulae_json).unwrap();
        fs::write(cache_dir.join("casks.json"), "[]").unwrap();

        let cache = Cache::new().unwrap();
        let outdated = get_outdated_packages(&cache).await.unwrap();

        assert_eq!(outdated.len(), 3);

        let names: Vec<&str> = outdated.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"pkg-version"));
        assert!(names.contains(&"pkg-rebuild"));
        assert!(names.contains(&"pkg-sha"));
        assert!(!names.contains(&"pkg-pinned"));
        assert!(!names.contains(&"pkg-uptodate"));

        for pkg in outdated {
            if pkg.name == "pkg-version" {
                assert_eq!(pkg.latest_version, "2.0.0");
            } else if pkg.name == "pkg-rebuild" {
                assert_eq!(pkg.latest_version, "1.0.0 (rebuild 1)");
            } else if pkg.name == "pkg-sha" {
                assert_eq!(pkg.latest_version, "1.0.0 (bottle updated)");
            }
        }

        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
    }
}
