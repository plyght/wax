use crate::bottle::{detect_platform, BottleDownloader};
use crate::cache::Cache;
use crate::cask::CaskState;
use crate::discovery::{discover_linux_system_packages, discover_manually_installed_casks};
use crate::error::{Result, WaxError};
use crate::install::{create_symlinks, InstallMode, InstallState, InstalledPackage};
use crate::lockfile::Lockfile;
use crate::signal::{check_cancelled, CriticalSection};
use crate::ui::{copy_dir_all, PROGRESS_BAR_CHARS, PROGRESS_BAR_TEMPLATE};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Semaphore;
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn sync(cache: &Cache) -> Result<()> {
    let start = std::time::Instant::now();

    let lockfile_path = Lockfile::default_path();

    let lockfile = Lockfile::load(&lockfile_path).await?;
    let package_count = lockfile.packages.len();
    let cask_count = lockfile.casks.len();

    if package_count == 0 && cask_count == 0 {
        println!("no packages or casks in lockfile");
        return Ok(());
    }

    let formulae = cache.load_formulae().await?;
    let state = InstallState::new()?;
    let mut installed_packages = state.load().await?;

    if cfg!(target_os = "linux") {
        for (name, package) in discover_linux_system_packages(&formulae).await? {
            installed_packages.entry(name).or_insert(package);
        }
    }

    // Save discovered packages to InstallState
    if !installed_packages.is_empty() {
        state.save(&installed_packages).await?;
    }

    let casks = cache.load_casks().await?;
    let cask_state = CaskState::new()?;
    let mut installed_casks = cask_state.load().await?;

    if cfg!(target_os = "macos") {
        for (name, cask) in discover_manually_installed_casks(&casks).await? {
            installed_casks.entry(name).or_insert(cask);
        }
        cask_state.save(&installed_casks).await?;
    }

    let current_platform = detect_platform();
    let mut packages_to_install = Vec::new();
    let mut casks_to_install = Vec::new();

    let mut up_to_date = Vec::new();
    let mut upgrades = Vec::new();

    for (name, lock_pkg) in &lockfile.packages {
        match installed_packages.get(name) {
            Some(installed) if installed.version != lock_pkg.version => {
                upgrades.push((
                    name.clone(),
                    installed.version.clone(),
                    lock_pkg.version.clone(),
                ));
                packages_to_install.push((name.clone(), lock_pkg.clone()));
            }
            Some(installed) if installed.platform != lock_pkg.bottle => {
                packages_to_install.push((name.clone(), lock_pkg.clone()));
            }
            Some(_) => {
                up_to_date.push(name.clone());
            }
            None => {
                packages_to_install.push((name.clone(), lock_pkg.clone()));
            }
        }
    }

    let mut casks_up_to_date = Vec::new();
    let mut cask_upgrades = Vec::new();

    for (name, lock_cask) in &lockfile.casks {
        match installed_casks.get(name) {
            Some(installed) if installed.version != lock_cask.version => {
                cask_upgrades.push((
                    name.clone(),
                    installed.version.clone(),
                    lock_cask.version.clone(),
                ));
                casks_to_install.push(name.clone());
            }
            Some(_) => {
                casks_up_to_date.push(name.clone());
            }
            None => {
                casks_to_install.push(name.clone());
            }
        }
    }

    // Show diff preview
    if !packages_to_install.is_empty() || !upgrades.is_empty() {
        let upgrade_index: HashMap<_, _> = upgrades
            .iter()
            .map(|(name, old_ver, new_ver)| (name.as_str(), (old_ver.as_str(), new_ver.as_str())))
            .collect();
        for (name, lock_pkg) in &packages_to_install {
            if let Some((old_ver, new_ver)) = upgrade_index.get(name.as_str()) {
                println!(
                    "  {} {} {} → {}",
                    style("↑").cyan(),
                    style(name).magenta(),
                    style(*old_ver).dim(),
                    style(*new_ver).green()
                );
            } else {
                println!(
                    "  {} {} {}",
                    style("+").green(),
                    style(name).magenta(),
                    style(format!("@{}", lock_pkg.version)).dim()
                );
            }
        }
    }

    if !casks_to_install.is_empty() || !cask_upgrades.is_empty() {
        let cask_upgrade_index: HashMap<_, _> = cask_upgrades
            .iter()
            .map(|(name, old_ver, new_ver)| (name.as_str(), (old_ver.as_str(), new_ver.as_str())))
            .collect();
        for name in &casks_to_install {
            if let Some((old_ver, new_ver)) = cask_upgrade_index.get(name.as_str()) {
                println!(
                    "  {} {} {} {} → {}",
                    style("↑").cyan(),
                    style(name).magenta(),
                    style("(cask)").yellow(),
                    style(*old_ver).dim(),
                    style(*new_ver).green()
                );
            } else {
                println!(
                    "  {} {} {}",
                    style("+").green(),
                    style(name).magenta(),
                    style("(cask)").yellow()
                );
            }
        }
    }

    if packages_to_install.is_empty() && casks_to_install.is_empty() {
        if !up_to_date.is_empty() || !casks_up_to_date.is_empty() {
            let total_up_to_date = up_to_date.len() + casks_up_to_date.len();
            println!(
                "{} {} packages/casks up to date",
                style("✓").green(),
                total_up_to_date
            );
        }
        return Ok(());
    }

    let sync_package_count = packages_to_install.len();

    if sync_package_count > 0 {
        let multi = MultiProgress::new();
        let downloader = Arc::new(BottleDownloader::new());
        // All packages download simultaneously; the semaphore only caps extreme cases.
        let concurrent_limit = sync_package_count.max(1).min(32);
        let semaphore = Arc::new(Semaphore::new(concurrent_limit));
        let temp_dir = Arc::new(TempDir::new()?);

        // Collect download entries and probe sizes concurrently for connection allocation.
        struct SyncEntry {
            name: String,
            version: String,
            platform: String,
            url: String,
            sha256: String,
        }

        let mut entries: Vec<SyncEntry> = Vec::new();
        for (name, lock_pkg) in packages_to_install {
            let formula = formulae
                .iter()
                .find(|f| f.name == name)
                .ok_or_else(|| WaxError::FormulaNotFound(name.clone()))?;

            if formula.versions.stable != lock_pkg.version {
                return Err(WaxError::LockfileError(format!(
                    "Package {} version mismatch: lockfile specifies {} but latest available is {}. The locked version may no longer be available.",
                    name, lock_pkg.version, formula.versions.stable
                )));
            }

            if lock_pkg.bottle != current_platform {
                println!(
                    "platform mismatch for {}: {} → {}",
                    name, lock_pkg.bottle, current_platform
                );
            }

            let bottle_info = formula
                .bottle
                .as_ref()
                .and_then(|b| b.stable.as_ref())
                .ok_or_else(|| {
                    WaxError::BottleNotAvailable(format!("{} (no bottle info)", name))
                })?;

            let bottle_file = bottle_info
                .files
                .get(&lock_pkg.bottle)
                .or_else(|| bottle_info.files.get("all"))
                .ok_or_else(|| {
                    WaxError::BottleNotAvailable(format!(
                        "{} for platform {}",
                        name, lock_pkg.bottle
                    ))
                })?;

            entries.push(SyncEntry {
                name: name.clone(),
                version: lock_pkg.version.clone(),
                platform: lock_pkg.bottle.clone(),
                url: bottle_file.url.clone(),
                sha256: bottle_file.sha256.clone(),
            });
        }

        // Probe all URLs concurrently for sizes so each download gets an appropriate
        // connection count (larger files get more parallel connections).
        let probe_tasks: Vec<_> = entries
            .iter()
            .map(|e| {
                let dl = Arc::clone(&downloader);
                let url = e.url.clone();
                tokio::spawn(async move { dl.probe_size(&url).await })
            })
            .collect();

        let mut sizes: Vec<u64> = Vec::with_capacity(entries.len());
        for task in probe_tasks {
            sizes.push(task.await.unwrap_or(0));
        }

        let mut tasks = Vec::new();
        for (entry, size) in entries.into_iter().zip(sizes) {
            let conns = BottleDownloader::num_connections(
                size,
                BottleDownloader::MAX_CONNECTIONS_PER_DOWNLOAD,
            );

            let downloader = Arc::clone(&downloader);
            let semaphore = Arc::clone(&semaphore);
            let temp_dir = Arc::clone(&temp_dir);

            let pb = multi.add(ProgressBar::new(0));
            let style = ProgressStyle::default_bar()
                .template(PROGRESS_BAR_TEMPLATE)
                .unwrap()
                .progress_chars(PROGRESS_BAR_CHARS);
            pb.set_style(style);
            pb.set_message(entry.name.clone());

            let task = tokio::spawn(async move {
                let permit = semaphore.acquire().await.unwrap();

                let tarball_path = temp_dir
                    .path()
                    .join(format!("{}-{}.tar.gz", entry.name, entry.version));

                downloader
                    .download(&entry.url, &tarball_path, Some(&pb), conns, None)
                    .await?;
                pb.finish_and_clear();

                // Release permit before extraction so another download can start.
                drop(permit);

                BottleDownloader::verify_checksum(&tarball_path, &entry.sha256)?;

                let extract_dir = temp_dir.path().join(&entry.name);
                BottleDownloader::extract(&tarball_path, &extract_dir)?;

                Ok::<_, WaxError>((entry.name, entry.version, entry.platform, extract_dir))
            });

            tasks.push(task);
        }

        let results = futures::future::join_all(tasks).await;

        let mut extracted_packages = Vec::new();
        for result in results {
            match result {
                Ok(Ok(data)) => extracted_packages.push(data),
                Ok(Err(e)) => return Err(e),
                Err(e) => {
                    return Err(WaxError::InstallError(format!(
                        "Download task failed: {}",
                        e
                    )))
                }
            }
        }

        let install_mode = InstallMode::detect();
        install_mode.validate()?;

        let cellar = install_mode.cellar_path()?;

        check_cancelled()?;

        println!();
        for (name, version, platform, extract_dir) in extracted_packages {
            let _critical = CriticalSection::new();
            let formula_cellar = cellar.join(&name).join(&version);
            tokio::fs::create_dir_all(&formula_cellar).await?;

            let actual_content_dir = extract_dir.join(&name).join(&version);
            if actual_content_dir.exists() {
                copy_dir_all(&actual_content_dir, &formula_cellar)?;
            } else {
                copy_dir_all(&extract_dir, &formula_cellar)?;
            }

            create_symlinks(
                &name,
                &version,
                &cellar,
                false, /* dry_run */
                install_mode,
            )
            .await?;

            let package = InstalledPackage {
                name: name.clone(),
                version: version.clone(),
                platform: platform.clone(),
                install_date: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64,
                install_mode,
                from_source: false,
                bottle_rebuild: 0,
                bottle_sha256: None,
                pinned: false,
            };
            state.add(package).await?;

            println!("+ {}", style(&name).magenta());
        }
    }

    if !casks_to_install.is_empty() {
        println!();
        crate::commands::install::install_quiet(
            cache,
            &casks_to_install,
            true,  // cask
            false, // user
            false, // global
        )
        .await?;
    }

    let elapsed = start.elapsed();

    let total_synced = sync_package_count + casks_to_install.len();

    println!();
    println!(
        "{} {} synced [{}ms]",
        total_synced,
        if total_synced == 1 {
            "package/cask"
        } else {
            "packages/casks"
        },
        elapsed.as_millis()
    );

    Ok(())
}
