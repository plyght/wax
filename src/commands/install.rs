use crate::api::Formula;
use crate::bottle::{detect_platform, BottleDownloader};
use crate::builder::Builder;
use crate::cache::Cache;
use crate::cask::{detect_artifact_type, CaskInstaller, CaskState, InstalledCask};
use crate::commands::version_install;
use crate::deps::resolve_dependencies;
use crate::error::{Result, WaxError};
use crate::formula_parser::FormulaParser;
use crate::install::{create_symlinks, InstallMode, InstallState, InstalledPackage};
use crate::signal::{check_cancelled, CriticalSection};
use crate::tap::TapManager;
use crate::ui::{
    copy_dir_all, PROGRESS_BAR_CHARS, PROGRESS_BAR_PREFIX_TEMPLATE, PROGRESS_BAR_TEMPLATE,
};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use sha2::Digest;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Semaphore;
use tracing::{debug, info, instrument};

async fn install_from_source_task(
    formula: Formula,
    cellar: &Path,
    install_mode: InstallMode,
    state: &InstallState,
    platform: &str,
) -> Result<()> {
    info!("Installing {} from source", formula.name);

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {prefix:.bold} {msg}")
            .unwrap(),
    );
    spinner.set_prefix("[>]".to_string());
    spinner.set_message(format!("Fetching formula for {}...", formula.name));
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));

    let ruby_content = FormulaParser::fetch_formula_rb(&formula.name).await?;

    spinner.set_message("Parsing formula...");
    let parsed_formula = FormulaParser::parse_ruby_formula(&formula.name, &ruby_content)?;

    spinner.set_message("Building from source (this may take several minutes)...".to_string());

    let temp_dir = TempDir::new()?;
    let source_tarball = temp_dir.path().join(format!(
        "{}-{}.tar.gz",
        formula.name, parsed_formula.source.version
    ));

    let client = reqwest::Client::new();
    let response = client.get(&parsed_formula.source.url).send().await?;

    if !response.status().is_success() {
        return Err(WaxError::BuildError(format!(
            "Failed to download source: HTTP {}",
            response.status()
        )));
    }

    let content = response.bytes().await?;
    let sha256 = format!("{:x}", sha2::Sha256::digest(&content));
    tokio::fs::write(&source_tarball, &content).await?;
    if sha256 != parsed_formula.source.sha256 {
        return Err(WaxError::ChecksumMismatch {
            expected: parsed_formula.source.sha256.clone(),
            actual: sha256,
        });
    }

    let build_dir = temp_dir.path().join("build");
    let install_prefix = temp_dir.path().join("install");
    tokio::fs::create_dir_all(&install_prefix).await?;

    let builder = Builder::new();
    builder
        .build_from_source(
            &parsed_formula,
            &source_tarball,
            &build_dir,
            &install_prefix,
            Some(&spinner),
        )
        .await?;

    spinner.set_message("Installing to Cellar...");

    let version = &parsed_formula.source.version;
    let formula_cellar = cellar.join(&formula.name).join(version);
    tokio::fs::create_dir_all(&formula_cellar).await?;

    copy_dir_all(&install_prefix, &formula_cellar)?;

    create_symlinks(
        &formula.name,
        version,
        cellar,
        false, /* dry_run */
        install_mode,
    )
    .await?;

    let package = InstalledPackage {
        name: formula.name.clone(),
        version: version.clone(),
        platform: platform.to_string(),
        install_date: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        install_mode,
        from_source: true,
        bottle_rebuild: 0,
        bottle_sha256: None,
        pinned: false,
    };
    state.add(package).await?;

    spinner.finish_and_clear();
    println!(
        "+ {}@{} {}",
        style(&formula.name).magenta(),
        style(version).dim(),
        style("(source)").yellow()
    );

    Ok(())
}

struct InstallArgs<'a> {
    dry_run: bool,
    cask: bool,
    user: bool,
    global: bool,
    build_from_source: bool,
    quiet: bool,
    external_pb: Option<&'a ProgressBar>,
}

#[instrument(skip(cache))]
pub async fn install(
    cache: &Cache,
    package_names: &[String],
    dry_run: bool,
    cask: bool,
    user: bool,
    global: bool,
    build_from_source: bool,
) -> Result<()> {
    install_impl(
        cache,
        package_names,
        InstallArgs {
            dry_run,
            cask,
            user,
            global,
            build_from_source,
            quiet: false,
            external_pb: None,
        },
    )
    .await
}

pub async fn install_quiet(
    cache: &Cache,
    package_names: &[impl AsRef<str>],
    cask: bool,
    user: bool,
    global: bool,
) -> Result<()> {
    let names: Vec<String> = package_names
        .iter()
        .map(|s| s.as_ref().to_string())
        .collect();
    install_impl(
        cache,
        &names,
        InstallArgs {
            dry_run: false,
            cask,
            user,
            global,
            build_from_source: false,
            quiet: true,
            external_pb: None,
        },
    )
    .await
}

pub async fn install_quiet_with_progress(
    cache: &Cache,
    package_names: &[impl AsRef<str>],
    cask: bool,
    user: bool,
    global: bool,
    pb: &ProgressBar,
) -> Result<()> {
    let names: Vec<String> = package_names
        .iter()
        .map(|s| s.as_ref().to_string())
        .collect();
    install_impl(
        cache,
        &names,
        InstallArgs {
            dry_run: false,
            cask,
            user,
            global,
            build_from_source: false,
            quiet: true,
            external_pb: Some(pb),
        },
    )
    .await
}

async fn install_impl(
    cache: &Cache,
    package_names: &[String],
    args: InstallArgs<'_>,
) -> Result<()> {
    let InstallArgs {
        dry_run,
        cask,
        user,
        global,
        build_from_source,
        quiet,
        external_pb,
    } = args;
    if package_names.is_empty() {
        return Err(WaxError::InvalidInput("No packages specified".to_string()));
    }

    for name in package_names {
        crate::error::validate_package_name(name)?;
    }

    cache.ensure_fresh().await?;

    if cask {
        return install_casks(cache, package_names, dry_run).await;
    }

    let install_mode = match InstallMode::from_flags(user, global)? {
        Some(mode) => mode,
        None => InstallMode::detect(),
    };

    install_mode.validate()?;

    let start = std::time::Instant::now();

    let mut tap_manager = TapManager::new()?;
    tap_manager.load().await?;

    let formulae = cache.load_all_formulae().await?;
    let state = InstallState::new()?;
    state.sync_from_cellar().await.ok();
    let installed_packages = state.load().await?;
    let installed: HashSet<String> = installed_packages.keys().cloned().collect();

    // Pre-build lookup maps for O(1) formula resolution instead of O(n) linear scans
    let by_name: std::collections::HashMap<&str, &crate::api::Formula> =
        formulae.iter().map(|f| (f.name.as_str(), f)).collect();
    let by_full_name: std::collections::HashMap<&str, &crate::api::Formula> =
        formulae.iter().map(|f| (f.full_name.as_str(), f)).collect();

    let mut all_to_install = Vec::new();
    let mut already_installed = Vec::new();
    let mut errors = Vec::new();
    let mut detected_casks: Vec<String> = Vec::new();

    for package_name in package_names {
        if installed.contains(package_name) {
            already_installed.push(package_name.clone());
            continue;
        }

        let formula = if package_name.contains('/') {
            by_full_name
                .get(package_name.as_str())
                .or_else(|| by_name.get(package_name.as_str()))
                .or_else(|| {
                    let parts: Vec<&str> = package_name.split('/').collect();
                    if parts.len() >= 3 {
                        by_name.get(parts[parts.len() - 1])
                    } else {
                        None
                    }
                })
                .copied()
        } else {
            by_name.get(package_name.as_str()).copied()
        };

        let formula = match formula {
            Some(f) => f,
            None => {
                let casks = cache.load_casks().await?;
                let cask_exists = casks
                    .iter()
                    .any(|c| &c.token == package_name || &c.full_token == package_name);

                if cask_exists {
                    // Collect for batch install — all casks will be downloaded concurrently below
                    detected_casks.push(package_name.clone());
                    continue;
                }

                if let Some((name, ver)) = package_name.rsplit_once('@') {
                    if !name.is_empty() && !ver.is_empty() {
                        if let Err(e) =
                            version_install::version_install(cache, name, ver, user, global).await
                        {
                            errors.push((package_name.clone(), format!("{}", e)));
                        }
                        continue;
                    }
                }

                let error_msg = if package_name.contains('/') {
                    let parts: Vec<&str> = package_name.split('/').collect();
                    if parts.len() >= 2 {
                        let tap_name = if parts.len() >= 3 {
                            format!("{}/{}", parts[0], parts[1])
                        } else {
                            parts[0].to_string()
                        };
                        let formula_name = parts[parts.len() - 1];

                        let tap_exists = tap_manager.has_tap(&tap_name).await;
                        if tap_exists {
                            format!(
                                "Formula '{}' not found in tap '{}'. The formula might not exist in this tap. Try: wax install {}",
                                formula_name, tap_name, formula_name
                            )
                        } else {
                            format!(
                                "Tap '{}' not installed. Add it with: wax tap add {}",
                                tap_name, tap_name
                            )
                        }
                    } else {
                        "Not found as formula or cask".to_string()
                    }
                } else {
                    "Not found as formula or cask".to_string()
                };

                errors.push((package_name.clone(), error_msg));
                continue;
            }
        };

        match resolve_dependencies(formula, &formulae, &installed) {
            Ok(deps) => {
                for dep in deps {
                    if !all_to_install.contains(&dep) {
                        all_to_install.push(dep);
                    }
                }
            }
            Err(e) => {
                errors.push((package_name.clone(), format!("{}", e)));
                continue;
            }
        }
    }

    if !already_installed.is_empty() && !quiet {
        for pkg in &already_installed {
            println!("{} is already installed", style(pkg).magenta());
        }
    }

    if !errors.is_empty() && !quiet {
        for (pkg, err) in &errors {
            eprintln!("{}: {}", pkg, err);
        }
        if all_to_install.is_empty() && detected_casks.is_empty() {
            return Err(WaxError::InstallError(
                "Cannot install any packages (all failed validation)".to_string(),
            ));
        }
    }

    // Install all auto-detected casks concurrently (batch download + serial install)
    if !detected_casks.is_empty() {
        install_casks(cache, &detected_casks, dry_run).await?;
    }

    if all_to_install.is_empty() {
        return Ok(());
    }

    let requested: Vec<&str> = package_names
        .iter()
        .filter(|p| !already_installed.contains(p) && !errors.iter().any(|(e, _)| e == *p))
        .map(|s| s.as_str())
        .collect();
    let package_list = requested.join(", ");

    let dep_count = all_to_install.len().saturating_sub(requested.len());
    if dep_count > 0 && !quiet {
        println!();
        println!(
            "installing {} + {} {}",
            package_list,
            dep_count,
            if dep_count == 1 {
                "dependency"
            } else {
                "dependencies"
            }
        );
    }

    if dry_run {
        if !quiet {
            println!();
            for name in &all_to_install {
                println!("+ {}", name);
            }
            println!("\ndry run - no changes made");
        }
        return Ok(());
    }

    let platform = detect_platform();
    debug!("Detected platform: {}", platform);

    let cellar = install_mode.cellar_path()?;

    let multi = MultiProgress::new();
    let downloader = Arc::new(BottleDownloader::new());

    let packages_to_install: Vec<_> = all_to_install
        .iter()
        .map(|name| {
            by_name
                .get(name.as_str())
                .copied()
                .ok_or_else(|| WaxError::FormulaNotFound(name.clone()))
        })
        .collect::<Result<_>>()?;

    // How many packages need bottle downloads (not source builds)?
    let bottle_count = packages_to_install
        .iter()
        .filter(|pkg| {
            !build_from_source
                && pkg
                    .bottle
                    .as_ref()
                    .and_then(|b| b.stable.as_ref())
                    .and_then(|s| s.files.get(&platform).or_else(|| s.files.get("all")))
                    .is_some()
        })
        .count();

    // Divide the global HTTP connection pool across concurrently-downloading packages.
    // Semaphore allows up to 8 simultaneous downloads; bigger packages get more of their share.
    const CONCURRENT_LIMIT: usize = 8;
    let concurrent = bottle_count.min(CONCURRENT_LIMIT).max(1);
    let connections_per_pkg =
        (crate::bottle::BottleDownloader::GLOBAL_CONNECTION_POOL / concurrent).max(1);

    let semaphore = Arc::new(Semaphore::new(CONCURRENT_LIMIT));
    let mut tasks = Vec::new();
    let inline_extracted: Vec<(String, String, std::path::PathBuf, String, u32)> = Vec::new();
    let mut source_install_count = 0usize;

    let temp_dir = Arc::new(TempDir::new()?);

    for pkg in packages_to_install {
        let has_bottle = pkg
            .bottle
            .as_ref()
            .and_then(|b| b.stable.as_ref())
            .and_then(|s| s.files.get(&platform).or_else(|| s.files.get("all")))
            .is_some();

        if !has_bottle || build_from_source {
            check_cancelled()?;

            if build_from_source && has_bottle && !quiet {
                println!();
                println!("building {} from source", pkg.name);
            }

            install_from_source_task(pkg.clone(), &cellar, install_mode, &state, &platform).await?;
            source_install_count += 1;
            continue;
        }

        let bottle_info = pkg
            .bottle
            .as_ref()
            .and_then(|b| b.stable.as_ref())
            .ok_or_else(|| {
                WaxError::BottleNotAvailable(format!("{} (no bottle info)", pkg.name))
            })?;

        let bottle_file = bottle_info
            .files
            .get(&platform)
            .or_else(|| bottle_info.files.get("all"))
            .ok_or_else(|| {
                WaxError::BottleNotAvailable(format!("{} for platform {}", pkg.name, platform))
            })?;

        let url = bottle_file.url.clone();
        let sha256 = bottle_file.sha256.clone();
        let name = pkg.name.clone();
        let version = pkg.versions.stable.clone();
        let rebuild = pkg.bottle_rebuild();

        if let Some(ext_pb) = external_pb {
            let tarball_path = temp_dir.path().join(format!("{}-{}.tar.gz", name, version));

            downloader
                .download(&url, &tarball_path, Some(ext_pb), connections_per_pkg)
                .await?;

            BottleDownloader::verify_checksum(&tarball_path, &sha256)?;

            let extract_dir = temp_dir.path().join(&name);
            BottleDownloader::extract(&tarball_path, &extract_dir)?;

            // Transition download bar → install spinner in-place by cloning the handle
            // (indicatif clones share the same underlying state).
            ext_pb.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.cyan} {msg}")
                    .unwrap()
                    .tick_chars(crate::ui::SPINNER_TICK_CHARS),
            );
            ext_pb.enable_steady_tick(std::time::Duration::from_millis(80));

            install_extracted_bottle(
                &name,
                &version,
                &extract_dir,
                sha256,
                rebuild,
                &cellar,
                install_mode,
                &platform,
                &state,
                false,
                None,
                Some(ext_pb.clone()),
            )
            .await?;
            continue;
        }

        let downloader = Arc::clone(&downloader);
        let semaphore = Arc::clone(&semaphore);
        let temp_dir = Arc::clone(&temp_dir);

        let pb = if quiet {
            ProgressBar::hidden()
        } else {
            let pb = multi.add(ProgressBar::new(0));
            let style = ProgressStyle::default_bar()
                .template(PROGRESS_BAR_TEMPLATE)
                .unwrap()
                .progress_chars(PROGRESS_BAR_CHARS);
            pb.set_style(style);
            pb.set_message(name.clone());
            pb
        };

        let task = tokio::spawn(async move {
            let _permit = semaphore.acquire().await.unwrap();
            // Don't even start if already cancelled
            crate::signal::check_cancelled()?;
            crate::signal::set_current_op(format!("downloading {}", name));

            let tarball_path = temp_dir.path().join(format!("{}-{}.tar.gz", name, version));

            downloader.download(&url, &tarball_path, Some(&pb), connections_per_pkg).await?;
            pb.finish_and_clear();

            BottleDownloader::verify_checksum(&tarball_path, &sha256)?;

            let extract_dir = temp_dir.path().join(&name);
            BottleDownloader::extract(&tarball_path, &extract_dir)?;

            Ok::<_, WaxError>((name, version, extract_dir, sha256, rebuild))
        });

        tasks.push(task);
    }

    // Collect results; abort remaining tasks immediately on cancellation
    let mut extracted_packages: Vec<(String, String, std::path::PathBuf, String, u32)> = Vec::new();
    let mut failed_packages = Vec::new();
    let mut cancelled = false;

    for handle in tasks {
        if cancelled || crate::signal::is_shutdown_requested() {
            handle.abort();
            cancelled = true;
            continue;
        }
        match handle.await {
            Ok(Ok(data)) => extracted_packages.push(data),
            Ok(Err(WaxError::Interrupted)) => {
                cancelled = true;
            }
            Ok(Err(e)) => {
                failed_packages.push(format!("{}", e));
            }
            Err(e) if e.is_cancelled() => {
                cancelled = true;
            }
            Err(e) => {
                failed_packages.push(format!("Task error: {}", e));
            }
        }
    }

    extracted_packages.extend(inline_extracted);

    if cancelled {
        return Err(WaxError::Interrupted);
    }

    if !failed_packages.is_empty() && !quiet {
        for err in &failed_packages {
            eprintln!("{}", err);
        }
        if extracted_packages.is_empty() {
            return Err(WaxError::InstallError(
                "All package downloads failed".to_string(),
            ));
        }
    }

    let extracted_packages_count = extracted_packages.len();
    check_cancelled()?;

    // Drop MultiProgress before the install phase so its draw layer releases
    // stdout and plain println! calls in install_extracted_bottle are visible.
    drop(multi);

    if !quiet {
        println!();
    }
    for (name, version, extract_dir, bottle_sha, bottle_rebuild) in extracted_packages {
        install_extracted_bottle(
            &name,
            &version,
            &extract_dir,
            bottle_sha,
            bottle_rebuild,
            &cellar,
            install_mode,
            &platform,
            &state,
            quiet,
            None,
            None,
        )
        .await?;
    }

    let state_snapshot = state.load().await?;
    let installed_names: std::collections::HashSet<String> =
        state_snapshot.keys().cloned().collect();

    for pkg_name in package_names {
        if pkg_name.ends_with("-full") {
            let base_name = pkg_name.trim_end_matches("-full");
            if !installed_names.contains(base_name) {
                let opt_dir = install_mode.prefix()?.join("opt");
                let base_link = opt_dir.join(base_name);
                let full_link = opt_dir.join(pkg_name);

                if full_link.exists() && !base_link.exists() {
                    #[cfg(unix)]
                    {
                        if let Ok(target) = std::fs::read_link(&full_link) {
                            let _ = std::os::unix::fs::symlink(&target, &base_link);
                            if !quiet {
                                println!(
                                    "  {} auto-linked {} → {}",
                                    style("→").cyan(),
                                    style(base_name).magenta(),
                                    style(pkg_name).dim()
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    if !quiet {
        let elapsed = start.elapsed();
        let successful_count = extracted_packages_count + source_install_count;
        println!(
            "\n{} {} installed [{}ms]",
            successful_count,
            if successful_count == 1 {
                "package"
            } else {
                "packages"
            },
            elapsed.as_millis()
        );
    }

    Ok(())
}

pub async fn install_extracted_bottle(
    name: &str,
    version: &str,
    extract_dir: &std::path::Path,
    bottle_sha: String,
    bottle_rebuild: u32,
    cellar: &std::path::Path,
    install_mode: InstallMode,
    platform: &str,
    state: &InstallState,
    quiet: bool,
    _multi: Option<&MultiProgress>,
    existing_pb: Option<ProgressBar>,
) -> Result<()> {
    crate::signal::set_current_op(format!("installing {}", name));
    let _critical = CriticalSection::new();

    // Step messages are printed immediately (not via spinner) so they always
    // show even when the operation completes in <80ms. When a MultiProgress
    // is active we use multi.println() to avoid clobbering its render area;
    // when an existing_pb is provided (reinstall path) we update its message
    // so the single bar transitions from "downloading" to each install step.
    macro_rules! step {
        ($msg:expr) => {
            if !quiet {
                if let Some(ref pb) = existing_pb {
                    pb.set_message(format!("{} {}", style(name).magenta(), style($msg).dim()));
                    pb.tick();
                } else {
                    println!("  {} {}", style(name).magenta(), style($msg).dim());
                }
            }
        };
    }
    step!("resolving...");

    // Detect the actual version directory from what's in the extracted bottle.
    // Homebrew bottles embed {version}_{rebuild} paths, but the API's rebuild
    // field can lag behind. Scanning the extracted dir gives us the ground truth.
    let name_dir = extract_dir.join(name);
    let cellar_version: String = if name_dir.exists() {
        let mut found = None;
        if let Ok(mut entries) = std::fs::read_dir(&name_dir) {
            while let Some(Ok(entry)) = entries.next() {
                let entry_name = entry.file_name().to_string_lossy().to_string();
                if entry_name.starts_with(version) && entry.path().is_dir() {
                    found = Some(entry_name);
                    break;
                }
            }
        }
        found.unwrap_or_else(|| {
            if bottle_rebuild > 0 {
                format!("{}_{}", version, bottle_rebuild)
            } else {
                version.to_string()
            }
        })
    } else if bottle_rebuild > 0 {
        format!("{}_{}", version, bottle_rebuild)
    } else {
        version.to_string()
    };

    let formula_cellar = cellar.join(name).join(&cellar_version);
    if formula_cellar.exists() {
        step!("cleaning old version...");
        tokio::fs::remove_dir_all(&formula_cellar)
            .await
            .or_else(|_| crate::sudo::sudo_remove(&formula_cellar).map(|_| ()))?;
    }
    tokio::fs::create_dir_all(&formula_cellar)
        .await
        .or_else(|_| crate::sudo::sudo_mkdir(&formula_cellar))?;

    step!("copying to cellar...");
    let actual_content_dir = name_dir.join(&cellar_version);
    if actual_content_dir.exists() {
        copy_dir_all(&actual_content_dir, &formula_cellar)?;
    } else if name_dir.exists() {
        copy_dir_all(&name_dir, &formula_cellar)?;
    } else {
        copy_dir_all(&extract_dir.to_path_buf(), &formula_cellar)?;
    }

    step!("relocating...");
    {
        let prefix = install_mode.prefix()?;
        let default_prefix = if cfg!(target_os = "macos") {
            "/opt/homebrew"
        } else {
            "/home/linuxbrew/.linuxbrew"
        };
        BottleDownloader::relocate_bottle(
            &formula_cellar,
            prefix.to_str().unwrap_or(default_prefix),
        )?;
    }

    step!("symlinking...");
    create_symlinks(name, &cellar_version, cellar, false, install_mode).await?;

    let package = InstalledPackage {
        name: name.to_string(),
        version: cellar_version.clone(),
        platform: platform.to_string(),
        install_date: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        install_mode,
        from_source: false,
        bottle_rebuild,
        bottle_sha256: Some(bottle_sha),
        pinned: false,
    };
    state.add(package).await?;

    if !quiet && existing_pb.is_none() {
        println!("+ {}@{}", style(name).magenta(), style(&cellar_version).dim());
    }

    Ok(())
}

fn extract_app_name(artifacts: &[crate::api::CaskArtifact]) -> Option<String> {
    use crate::api::CaskArtifact;

    for artifact in artifacts {
        match artifact {
            CaskArtifact::App { app } => {
                if let Some(app_name) = app.first() {
                    return Some(app_name.clone());
                }
            }
            _ => continue,
        }
    }
    None
}

struct DownloadedCask {
    name: String,
    details: crate::api::CaskDetails,
    artifact_type: &'static str,
    download_path: std::path::PathBuf,
    // keep temp dir alive until install is done
    _temp_dir: TempDir,
}

#[instrument(skip(cache))]
async fn install_casks(cache: &Cache, cask_names: &[String], dry_run: bool) -> Result<()> {
    let start = std::time::Instant::now();
    let casks = cache.load_casks().await?;
    let state = CaskState::new()?;
    let installed_casks = state.load().await?;

    let mut to_install = Vec::new();
    let mut already_installed = Vec::new();

    for cask_name in cask_names {
        if installed_casks.contains_key(cask_name) {
            already_installed.push(cask_name.clone());
        } else if casks.iter().any(|c| &c.token == cask_name || &c.full_token == cask_name) {
            to_install.push(cask_name.clone());
        } else {
            eprintln!("{}: cask not found", style(cask_name).magenta());
        }
    }

    if !already_installed.is_empty() {
        for name in &already_installed {
            println!("{} is already installed", style(name).magenta());
        }
    }

    if to_install.is_empty() {
        return Ok(());
    }

    println!(
        "installing {}\n",
        to_install
            .iter()
            .map(|n| format!("{} (cask)", style(n).magenta()))
            .collect::<Vec<_>>()
            .join(", ")
    );

    if dry_run {
        println!("dry run - no changes made");
        return Ok(());
    }

    // --- Phase 1: fetch all details + probe artifact types concurrently ---
    let api_client = Arc::new(crate::api::ApiClient::new());
    let installer = Arc::new(CaskInstaller::new());
    let semaphore = Arc::new(Semaphore::new(8));

    let detail_tasks: Vec<_> = to_install
        .iter()
        .map(|name| {
            let api = Arc::clone(&api_client);
            let inst = Arc::clone(&installer);
            let sem = Arc::clone(&semaphore);
            let name = name.clone();
            tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let details = api.fetch_cask_details(&name).await?;
                let artifact_type = if let Some(t) = detect_artifact_type(&details.url) {
                    t
                } else {
                    inst.probe_artifact_type(&details.url).await.ok_or_else(|| {
                        WaxError::InstallError(format!(
                            "Unsupported artifact type for URL: {}",
                            details.url
                        ))
                    })?
                };
                Ok::<_, WaxError>((name, details, artifact_type))
            })
        })
        .collect();

    let mut resolved = Vec::new();
    for task in detail_tasks {
        match task.await {
            Ok(Ok(data)) => resolved.push(data),
            Ok(Err(e)) => eprintln!("{} {}", style("✗").red(), e),
            Err(e) => eprintln!("{} task error: {}", style("✗").red(), e),
        }
    }

    if resolved.is_empty() {
        return Err(WaxError::InstallError("No casks could be resolved".to_string()));
    }

    // --- Phase 2: download all concurrently with shared MultiProgress ---
    let multi = Arc::new(MultiProgress::new());

    let download_tasks: Vec<_> = resolved
        .into_iter()
        .map(|(name, details, artifact_type)| {
            let inst = Arc::clone(&installer);
            let multi = Arc::clone(&multi);
            tokio::spawn(async move {
                let temp_dir = TempDir::new()?;
                let download_path = temp_dir
                    .path()
                    .join(format!("{}.{}", name, artifact_type));

                let pb = multi.add(ProgressBar::new(0));
                pb.set_style(
                    ProgressStyle::default_bar()
                        .template(PROGRESS_BAR_PREFIX_TEMPLATE)
                        .unwrap()
                        .progress_chars(PROGRESS_BAR_CHARS),
                );
                pb.set_prefix(name.clone());

                inst.download_cask(&details.url, &download_path, Some(&pb)).await?;
                pb.finish_and_clear();

                Ok::<_, WaxError>(DownloadedCask {
                    name,
                    details,
                    artifact_type,
                    download_path,
                    _temp_dir: temp_dir,
                })
            })
        })
        .collect();

    let mut downloaded = Vec::new();
    for task in download_tasks {
        match task.await {
            Ok(Ok(d)) => downloaded.push(d),
            Ok(Err(e)) => eprintln!("{} download failed: {}", style("✗").red(), e),
            Err(e) => eprintln!("{} task error: {}", style("✗").red(), e),
        }
    }

    // --- Phase 3: verify checksums + install serially ---
    let mut installed_count = 0;
    let mut failed = Vec::new();

    for d in &downloaded {
        check_cancelled()?;

        if let Err(e) = CaskInstaller::verify_checksum(&d.download_path, &d.details.sha256) {
            eprintln!("{} {} checksum failed: {}", style("✗").red(), style(&d.name).magenta(), e);
            failed.push(d.name.clone());
            continue;
        }

        let result = install_from_downloaded(&d.details, d.artifact_type, &d.download_path).await;
        match result {
            Ok(installed_cask) => {
                let state = CaskState::new()?;
                state.add(installed_cask).await?;
                println!(
                    "{} {} (cask) {}",
                    style("✓").green().bold(),
                    style(&d.name).magenta(),
                    style(&d.details.version).dim()
                );
                installed_count += 1;
            }
            Err(e) => {
                eprintln!("{} {} failed: {}", style("✗").red(), style(&d.name).magenta(), e);
                failed.push(d.name.clone());
            }
        }
    }

    let elapsed = start.elapsed();
    if failed.is_empty() {
        println!(
            "\n{} {} installed [{}ms]",
            installed_count,
            if installed_count == 1 { "cask" } else { "casks" },
            elapsed.as_millis()
        );
        Ok(())
    } else {
        println!(
            "\n{}/{} casks installed ({} failed) [{}ms]",
            installed_count,
            downloaded.len(),
            failed.len(),
            elapsed.as_millis()
        );
        Err(WaxError::InstallError(format!(
            "Some casks failed: {}",
            failed.join(", ")
        )))
    }
}

/// Install a cask from an already-downloaded file (skips download).
async fn install_from_downloaded(
    cask: &crate::api::CaskDetails,
    artifact_type: &'static str,
    download_path: &std::path::Path,
) -> Result<InstalledCask> {
    let installer = CaskInstaller::new();
    let display_name = cask.name.first().unwrap_or(&cask.token);

    let mut binary_paths: Vec<String> = Vec::new();
    let mut installed_app_name: Option<String> = None;

    match artifact_type {
        "dmg" | "zip" => {
            let app_name = if let Some(artifacts) = &cask.artifacts {
                extract_app_name(artifacts).unwrap_or_else(|| format!("{}.app", display_name))
            } else {
                format!("{}.app", display_name)
            };
            if artifact_type == "dmg" {
                installer.install_dmg(download_path, &app_name).await?;
            } else {
                installer.install_zip(download_path, &app_name).await?;
            }
            installed_app_name = Some(app_name);
        }
        "pkg" => installer.install_pkg(download_path).await?,
        "tar.gz" => {
            let binary_path = installer
                .install_tarball(download_path, &cask.token)
                .await?;
            binary_paths.push(binary_path.display().to_string());
        }
        _ => {
            return Err(WaxError::InstallError(format!(
                "Unsupported artifact type: {}",
                artifact_type
            )));
        }
    }

    Ok(InstalledCask {
        name: cask.token.clone(),
        version: cask.version.clone(),
        install_date: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        artifact_type: Some(artifact_type.to_string()),
        binary_paths: if binary_paths.is_empty() { None } else { Some(binary_paths) },
        app_name: installed_app_name,
    })
}
