use crate::api::{CaskArtifact, Formula};
use crate::bottle::{detect_platform, BottleDownloader, DownloadTotals};
use crate::builder::Builder;
use crate::cache::Cache;
use crate::cask::{
    detect_artifact_type, CaskInstaller, CaskState, InstalledCask, RollbackContext, StagingContext,
};
use crate::commands::version_install;
use crate::deps::resolve_dependencies;
use crate::discovery::discover_manually_installed_casks;
use crate::error::{Result, WaxError};
use crate::formula_parser::{BuildSystem, FormulaParser};
use crate::install::{create_symlinks, InstallMode, InstallState, InstalledPackage};
use crate::signal::{check_cancelled, clear_active_multi, set_active_multi, CriticalSection};
use crate::system_pm::SystemPm;
use crate::tap::TapManager;
use crate::ui::{
    copy_dir_all, dirs, PROGRESS_BAR_CHARS, PROGRESS_BAR_PREFIX_TEMPLATE, PROGRESS_BAR_TEMPLATE,
};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use sha2::Digest;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
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

    // Use the local tap .rb file if available; otherwise fetch from homebrew-core.
    let ruby_content = if let Some(rb_path) = &formula.rb_path {
        tokio::fs::read_to_string(rb_path).await.map_err(|e| {
            crate::error::WaxError::BuildError(format!(
                "Failed to read formula file {}: {}",
                rb_path.display(),
                e
            ))
        })?
    } else {
        FormulaParser::fetch_formula_rb(&formula.name).await?
    };

    spinner.set_message("Parsing formula...");
    let parsed_formula = FormulaParser::parse_ruby_formula(&formula.name, &ruby_content)?;

    // Binary-release formula: `bin.install` entries with no build system.
    // Download the platform-appropriate pre-built tarball and copy the named files.
    if !parsed_formula.bin_installs.is_empty()
        && parsed_formula.build_system == BuildSystem::Unknown
    {
        let (dl_url, dl_sha) =
            FormulaParser::extract_platform_source(&ruby_content).ok_or_else(|| {
                WaxError::BuildError(format!(
                    "Formula '{}' has no pre-built binary for this platform (os={}, arch={})",
                    formula.name,
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                ))
            })?;

        spinner.set_message(format!("Downloading {}…", formula.name));
        let client = reqwest::Client::new();
        let response = client.get(&dl_url).send().await?;
        if !response.status().is_success() {
            return Err(WaxError::BuildError(format!(
                "Failed to download binary: HTTP {}",
                response.status()
            )));
        }
        let bytes = response.bytes().await?;
        let actual_sha = format!("{:x}", sha2::Sha256::digest(&bytes));
        if actual_sha != dl_sha {
            return Err(WaxError::ChecksumMismatch {
                expected: dl_sha,
                actual: actual_sha,
            });
        }

        // Extract tarball.
        let temp_dir = TempDir::new()?;
        let extract_dir =
            stage_binary_release_download(bytes.as_ref(), &dl_url, &formula.name, temp_dir.path())
                .await?;

        // Find the single extracted subdirectory, or use extract_dir itself.
        let src_dir = std::fs::read_dir(&extract_dir)
            .ok()
            .and_then(|mut rd| {
                let entries: Vec<_> = rd.by_ref().filter_map(|e| e.ok()).collect();
                if entries.len() == 1 {
                    let e = &entries[0];
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        return Some(e.path());
                    }
                }
                None
            })
            .unwrap_or_else(|| extract_dir.clone());

        // Copy bin_install targets into install_prefix/bin/.
        let install_prefix = temp_dir.path().join("install");
        let bin_dir = install_prefix.join("bin");
        tokio::fs::create_dir_all(&bin_dir).await?;
        let mut copied_bins = 0usize;
        let mut missing_bins = Vec::new();
        for target in &parsed_formula.bin_install_targets {
            let dest_path = Path::new(&target.destination);
            if dest_path.is_absolute() || target.destination.split('/').any(|part| part == "..") {
                return Err(WaxError::BuildError(format!(
                    "Formula '{}' has invalid bin.install destination '{}'",
                    formula.name, target.destination
                )));
            }

            let src = resolve_bin_install_source(&src_dir, &target.source).await?;
            if src.exists() {
                let dst = bin_dir.join(&target.destination);
                if let Some(parent) = dst.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::copy(&src, &dst).await?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = tokio::fs::metadata(&dst).await?.permissions();
                    perms.set_mode(perms.mode() | 0o111);
                    tokio::fs::set_permissions(&dst, perms).await?;
                }
                copied_bins += 1;
            } else {
                if !target.optional {
                    missing_bins.push(target.source.clone());
                }
            }
        }
        if !missing_bins.is_empty() {
            return Err(WaxError::BuildError(format!(
                "Formula '{}' is broken: bin.install target(s) not found after extracting {}: {}",
                formula.name,
                dl_url,
                missing_bins.join(", ")
            )));
        }
        if copied_bins == 0 {
            return Err(WaxError::BuildError(format!(
                "Formula '{}' is broken: no bin.install targets were installed",
                formula.name
            )));
        }

        spinner.set_message("Installing to Cellar...");
        let version = &parsed_formula.source.version;
        let formula_cellar = cellar.join(&formula.name).join(version);
        tokio::fs::create_dir_all(&formula_cellar).await?;
        copy_dir_all(&install_prefix, &formula_cellar)?;
        create_symlinks(&formula.name, version, cellar, false, install_mode).await?;

        let package = InstalledPackage {
            name: formula.name.clone(),
            version: version.clone(),
            platform: platform.to_string(),
            install_date: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            install_mode,
            from_source: false,
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
            style("(binary)").yellow()
        );
        return Ok(());
    }

    spinner.set_message("Building from source (this may take several minutes)...".to_string());

    if parsed_formula.source.url.is_empty() {
        spinner.finish_and_clear();
        return Err(WaxError::BuildError(format!(
            "Formula '{}' has no stable source URL; install it with --head",
            formula.name
        )));
    }

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
            .unwrap_or_default()
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

async fn resolve_bin_install_source(root: &Path, source: &str) -> Result<std::path::PathBuf> {
    if !source.contains('*') {
        return Ok(root.join(source));
    }
    let source_path = Path::new(source);
    if source_path.components().count() != 1 {
        return Err(WaxError::BuildError(format!(
            "Unsupported bin.install glob '{}'",
            source
        )));
    }
    let Some((prefix, suffix)) = source.split_once('*') else {
        return Ok(root.join(source));
    };
    let mut matches = Vec::new();
    let mut entries = tokio::fs::read_dir(root).await?;
    while let Some(entry) = entries.next_entry().await? {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if name.starts_with(prefix) && name.ends_with(suffix) {
            matches.push(entry.path());
        }
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Ok(root.join(source)),
        _ => Err(WaxError::BuildError(format!(
            "Formula bin.install glob '{}' matched multiple files",
            source
        ))),
    }
}

async fn stage_binary_release_download(
    bytes: &[u8],
    dl_url: &str,
    formula_name: &str,
    temp_dir: &Path,
) -> Result<PathBuf> {
    let extract_dir = temp_dir.join("extracted");
    tokio::fs::create_dir_all(&extract_dir).await?;

    if binary_release_url_is_archive(dl_url) {
        let archive_path = temp_dir.join(binary_release_download_filename(dl_url, formula_name));
        tokio::fs::write(&archive_path, bytes).await?;

        let tar_output = tokio::process::Command::new("tar")
            .arg("xf")
            .arg(&archive_path)
            .arg("-C")
            .arg(&extract_dir)
            .output()
            .await?;
        if !tar_output.status.success() {
            return Err(WaxError::BuildError(format!(
                "Failed to extract tarball: {}",
                String::from_utf8_lossy(&tar_output.stderr)
            )));
        }
    } else {
        tokio::fs::write(
            extract_dir.join(binary_release_download_filename(dl_url, formula_name)),
            bytes,
        )
        .await?;
    }

    Ok(extract_dir)
}

fn binary_release_url_is_archive(url: &str) -> bool {
    let path = url
        .split('?')
        .next()
        .unwrap_or(url)
        .split('#')
        .next()
        .unwrap_or(url);
    path.ends_with(".tar.gz")
        || path.ends_with(".tgz")
        || path.ends_with(".tar.bz2")
        || path.ends_with(".tbz")
        || path.ends_with(".tar.xz")
        || path.ends_with(".txz")
        || path.ends_with(".tar")
}

fn binary_release_download_filename(url: &str, formula_name: &str) -> String {
    let path = url
        .split('?')
        .next()
        .unwrap_or(url)
        .split('#')
        .next()
        .unwrap_or(url);
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(formula_name)
        .to_string()
}

/// Clone and build from a formula's HEAD git URL.
async fn install_from_head_task(
    formula: Formula,
    cellar: &Path,
    install_mode: InstallMode,
    state: &InstallState,
    platform: &str,
) -> Result<()> {
    info!("Installing {} from HEAD", formula.name);

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {prefix:.bold} {msg}")
            .unwrap(),
    );
    spinner.set_prefix("[>]".to_string());
    spinner.set_message(format!("Fetching formula for {}...", formula.name));
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));

    let ruby_content = if let Some(rb_path) = &formula.rb_path {
        tokio::fs::read_to_string(rb_path).await.map_err(|e| {
            crate::error::WaxError::BuildError(format!(
                "Failed to read formula file {}: {}",
                rb_path.display(),
                e
            ))
        })?
    } else {
        FormulaParser::fetch_formula_rb(&formula.name).await?
    };

    spinner.set_message("Parsing formula...");
    let parsed_formula = FormulaParser::parse_ruby_formula(&formula.name, &ruby_content)?;

    let Some(head_url) = parsed_formula.head_url.as_deref() else {
        spinner.finish_and_clear();
        eprintln!(
            "  {} '{}' has no HEAD URL — installing stable release instead",
            console::style("note:").yellow(),
            formula.name
        );
        return install_from_source_task(formula, cellar, install_mode, state, platform).await;
    };

    let temp_dir = TempDir::new()?;
    let clone_dir = temp_dir.path().join("head-src");

    spinner.set_message(format!("Cloning HEAD from {}...", head_url));

    let clone_output = tokio::process::Command::new("git")
        .args(["clone", "--depth=1", head_url])
        .arg(&clone_dir)
        .output()
        .await?;

    if !clone_output.status.success() {
        let stderr = String::from_utf8_lossy(&clone_output.stderr);
        return Err(crate::error::WaxError::BuildError(format!(
            "Failed to clone HEAD: {}",
            stderr
        )));
    }

    // Determine a version string from the commit SHA.
    let sha_output = tokio::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&clone_dir)
        .output()
        .await?;

    let sha = if sha_output.status.success() {
        String::from_utf8_lossy(&sha_output.stdout)
            .trim()
            .to_string()
    } else {
        "HEAD".to_string()
    };

    let version = format!("HEAD-{}", sha);

    spinner.set_message("Building from HEAD (this may take several minutes)...");

    let install_prefix = temp_dir.path().join("install");
    tokio::fs::create_dir_all(&install_prefix).await?;

    let builder = crate::builder::Builder::new();
    builder
        .build_from_directory(&parsed_formula, &clone_dir, &install_prefix, Some(&spinner))
        .await?;

    spinner.set_message("Installing to Cellar...");

    let formula_cellar = cellar.join(&formula.name).join(&version);
    tokio::fs::create_dir_all(&formula_cellar).await?;

    copy_dir_all(&install_prefix, &formula_cellar)?;

    create_symlinks(
        &formula.name,
        &version,
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
            .unwrap_or_default()
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
        style(&version).dim(),
        style("(HEAD)").yellow()
    );

    Ok(())
}

struct InstallArgs<'a> {
    dry_run: bool,
    cask: bool,
    user: bool,
    global: bool,
    build_from_source: bool,
    head: bool,
    run_scripts: bool,
    quiet: bool,
    force_reinstall: bool,
    external_pb: Option<&'a ProgressBar>,
}

#[instrument(skip(cache))]
#[allow(clippy::too_many_arguments)]
pub async fn install(
    cache: &Cache,
    package_names: &[String],
    dry_run: bool,
    cask: bool,
    user: bool,
    global: bool,
    build_from_source: bool,
    head: bool,
    run_scripts: bool,
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
            head,
            run_scripts,
            quiet: false,
            force_reinstall: false,
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
            head: false,
            run_scripts: true,
            quiet: true,
            force_reinstall: false,
            external_pb: None,
        },
    )
    .await
}

pub async fn install_quiet_force(
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
            head: false,
            run_scripts: true,
            quiet: true,
            force_reinstall: true,
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
    force_reinstall: bool,
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
            head: false,
            run_scripts: true,
            quiet: true,
            force_reinstall,
            external_pb: Some(pb),
        },
    )
    .await
}

fn tap_name_from_qualified_package(package_name: &str) -> Option<String> {
    let mut parts = package_name.split('/');
    let user = parts.next()?;
    let repo = parts.next()?;
    let formula = parts.next()?;

    if user.is_empty() || repo.is_empty() || formula.is_empty() {
        return None;
    }

    Some(format!("{}/{}", user, repo))
}

fn hint_user_prefix_path_if_needed(install_mode: InstallMode, quiet: bool) {
    if quiet || install_mode != InstallMode::User {
        return;
    }
    let Ok(prefix) = install_mode.prefix() else {
        return;
    };
    let bin_dir = prefix.join("bin");
    if !bin_dir.exists() {
        return;
    }
    let Ok(path_var) = std::env::var("PATH") else {
        return;
    };
    let bin_str = bin_dir.to_string_lossy();
    if path_var.split(':').any(|p| p == bin_str.as_ref()) {
        return;
    }
    println!();
    println!("{}", style("Installed programs are linked under:").yellow());
    println!("  {}", bin_dir.display());
    println!(
        "{}",
        style("Add this to your shell profile if a command is not found:").dim()
    );
    println!("  export PATH=\"{}:$PATH\"", bin_dir.display());
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
        head,
        run_scripts,
        quiet,
        force_reinstall,
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
        return install_casks(cache, package_names, dry_run, quiet, force_reinstall).await;
    }

    let install_mode = match InstallMode::from_flags(user, global)? {
        Some(mode) => mode,
        None => InstallMode::detect(),
    };

    install_mode.validate()?;

    let mut tap_manager = TapManager::new()?;
    tap_manager.load().await?;

    if !dry_run {
        let mut tapped = HashSet::new();
        for package_name in package_names {
            if let Some(tap_name) = tap_name_from_qualified_package(package_name) {
                if tapped.contains(&tap_name) {
                    continue;
                }
                if tap_manager.has_tap(&tap_name).await {
                    if !quiet {
                        println!("updating tap {}", style(&tap_name).cyan());
                    }
                    tap_manager.update_tap(&tap_name).await?;
                } else {
                    if !quiet {
                        println!("tapping {}", style(&tap_name).cyan());
                    }
                    tap_manager.add_tap(&tap_name).await?;
                }
                cache.invalidate_tap_cache(&tap_name).await?;
                tapped.insert(tap_name);
            }
        }
    }

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
    let mut all_to_install_set = HashSet::new();
    let mut already_installed = Vec::new();
    let mut errors = Vec::new();
    let mut detected_casks: Vec<String> = Vec::new();
    let mut user_direct_formula_names: HashSet<String> = HashSet::new();

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
                user_direct_formula_names.insert(formula.name.clone());
                for dep in deps {
                    if all_to_install_set.insert(dep.clone()) {
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

    check_already_installed_formula_linkages(&already_installed, &installed_packages)?;

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

    let cask_task = if detected_casks.is_empty() {
        None
    } else {
        let cask_names = detected_casks.clone();
        Some(tokio::spawn(async move {
            let local_cache = Cache::new()?;
            install_casks(&local_cache, &cask_names, dry_run, quiet, false).await
        }))
    };

    if all_to_install.is_empty() {
        if let Some(task) = cask_task {
            task.await
                .map_err(|e| WaxError::InstallError(format!("cask task failed: {}", e)))??;
        }
        hint_user_prefix_path_if_needed(install_mode, quiet);
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
    let owns_formula_multi = crate::signal::clone_active_multi().is_none();
    if owns_formula_multi {
        set_active_multi(multi.clone());
    }

    let packages_to_install: Vec<_> = all_to_install
        .iter()
        .map(|name| {
            by_name
                .get(name.as_str())
                .copied()
                .ok_or_else(|| WaxError::FormulaNotFound(name.clone()))
        })
        .collect::<Result<_>>()?;

    let formula_bottle_count = packages_to_install
        .iter()
        .filter(|pkg| {
            !(head || build_from_source)
                && pkg
                    .bottle
                    .as_ref()
                    .and_then(|b| b.stable.as_ref())
                    .and_then(|s| s.file_for_platform(&platform))
                    .is_some()
        })
        .count();

    let user_direct_formula_count = user_direct_formula_names.len();

    // "All downloads" only for multiple *user-requested* formulae with multiple bottle
    // downloads. One requested formula (plus deps), or a single bottle, stays per-row only
    // — same idea as one cask, and keeps `wax install one_formula one_cask` uncluttered.
    let formula_pipeline_totals = if quiet
        || external_pb.is_some()
        || user_direct_formula_count <= 1
        || formula_bottle_count <= 1
    {
        None
    } else {
        Some(DownloadTotals::default())
    };
    let hide_formula_overall = Arc::new(AtomicBool::new(false));
    let formula_net_phase_done = Arc::new(AtomicUsize::new(0));
    let formula_overall_poller = if let Some(totals) = formula_pipeline_totals.as_ref() {
        let overall_pb = multi.insert(0, ProgressBar::new(0));
        overall_pb.set_style(
            ProgressStyle::default_bar()
                .template(PROGRESS_BAR_TEMPLATE)
                .unwrap()
                .progress_chars(PROGRESS_BAR_CHARS),
        );
        overall_pb.set_message("All downloads");
        let totals_w = totals.clone();
        let overall_w = overall_pb.clone();
        let hide_w = Arc::clone(&hide_formula_overall);
        Some(tokio::spawn(async move {
            loop {
                if hide_w.load(Ordering::Relaxed) {
                    overall_w.finish_and_clear();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                if hide_w.load(Ordering::Relaxed) {
                    overall_w.finish_and_clear();
                    return;
                }
                let pos = totals_w.downloaded.load(Ordering::Relaxed);
                let len = totals_w.expected.load(Ordering::Relaxed);
                let cap = len.max(pos).max(1);
                overall_w.set_length(cap);
                overall_w.set_position(pos);
            }
        }))
    } else {
        None
    };

    let downloader = Arc::new(BottleDownloader::new());

    // Collect (name, url) for every package that has a bottle on this platform.
    let bottle_urls: Vec<(String, String)> = packages_to_install
        .iter()
        .filter(|_pkg| !build_from_source)
        .filter_map(|pkg| {
            let f = pkg.bottle.as_ref()?.stable.as_ref()?;
            let file = f.file_for_platform(&platform)?;
            Some((pkg.name.clone(), file.url.clone()))
        })
        .collect();

    // Probe all bottle URLs concurrently to get file sizes, then allocate
    // connections proportionally by size from the global pool.
    // Run multiple formula pipelines concurrently for parallel downloads.
    let concurrent_limit = 8;
    let connections_map: std::collections::HashMap<String, usize> = {
        use std::sync::Arc;
        let dl = Arc::clone(&downloader);
        let probe_tasks: Vec<_> = bottle_urls
            .iter()
            .map(|(name, url)| {
                let dl = Arc::clone(&dl);
                let url = url.clone();
                let name = name.clone();
                tokio::spawn(async move { (name, dl.probe_size(&url).await) })
            })
            .collect();

        let mut sizes: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for task in probe_tasks {
            if let Ok((name, size)) = task.await {
                sizes.insert(name, size);
            }
        }

        let total_size: u64 = sizes.values().sum();
        let pool = BottleDownloader::GLOBAL_CONNECTION_POOL;
        let n = bottle_urls.len().max(1);
        let mut allocs: Vec<(String, usize, f64)> = sizes
            .iter()
            .map(|(name, &size)| {
                if total_size == 0 {
                    let base = pool / n;
                    (name.clone(), base.max(1), 0.0)
                } else {
                    let exact = pool as f64 * size as f64 / total_size as f64;
                    let base = (exact.floor() as usize).max(1);
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

    let semaphore = Arc::new(Semaphore::new(concurrent_limit));
    let mut tasks = JoinSet::new();

    let temp_dir = Arc::new(TempDir::new()?);

    for pkg in packages_to_install {
        let has_bottle = pkg
            .bottle
            .as_ref()
            .and_then(|b| b.stable.as_ref())
            .and_then(|s| s.file_for_platform(&platform))
            .is_some();

        if head {
            check_cancelled()?;
            if !quiet {
                println!();
                println!("installing {} from HEAD", pkg.name);
            }
            install_from_head_task(pkg.clone(), &cellar, install_mode, &state, &platform).await?;
            continue;
        }

        if !has_bottle || build_from_source {
            check_cancelled()?;

            if build_from_source && has_bottle && !quiet {
                println!();
                println!("building {} from source", pkg.name);
            }

            install_from_source_task(pkg.clone(), &cellar, install_mode, &state, &platform).await?;
            continue;
        }

        let bottle_info = pkg
            .bottle
            .as_ref()
            .and_then(|b| b.stable.as_ref())
            .ok_or_else(|| {
                WaxError::BottleNotAvailable(format!("{} (no bottle info)", pkg.name))
            })?;

        let bottle_file = bottle_info.file_for_platform(&platform).ok_or_else(|| {
            WaxError::BottleNotAvailable(format!("{} for platform {}", pkg.name, platform))
        })?;

        let url = bottle_file.url.clone();
        let sha256 = bottle_file.sha256.clone();
        let name = pkg.name.clone();
        let version = pkg.versions.stable.clone();
        let rebuild = pkg.bottle_rebuild();

        let pkg_connections = connections_map.get(&name).copied().unwrap_or(1);

        if let Some(ext_pb) = external_pb {
            let tarball_path = temp_dir.path().join(format!("{}-{}.tar.gz", name, version));

            downloader
                .download(&url, &tarball_path, Some(ext_pb), pkg_connections, None)
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
                run_scripts,
                None,
                Some(ext_pb.clone()),
            )
            .await?;
            continue;
        }

        let downloader = Arc::clone(&downloader);
        let semaphore = Arc::clone(&semaphore);
        let temp_dir = Arc::clone(&temp_dir);
        let conns = pkg_connections;
        let pipe_totals = formula_pipeline_totals.clone();
        let net_done_f = Arc::clone(&formula_net_phase_done);
        let hide_f = Arc::clone(&hide_formula_overall);
        let n_bottle_formula = formula_bottle_count;

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

        tasks.spawn(async move {
            let permit = semaphore
                .acquire()
                .await
                .map_err(|e| WaxError::InstallError(format!("download semaphore closed: {e}")))?;
            // Don't even start if already cancelled
            crate::signal::check_cancelled()?;
            crate::signal::set_current_op(format!("downloading {}", name));

            let tarball_path = temp_dir.path().join(format!("{}-{}.tar.gz", name, version));

            let dl = downloader
                .download(&url, &tarball_path, Some(&pb), conns, pipe_totals.as_ref())
                .await;
            pb.finish_and_clear();

            // Release the download permit before extraction so the next package
            // can start downloading immediately rather than waiting for CPU-bound work.
            drop(permit);

            if pipe_totals.is_some() {
                note_aggregate_download_row_done(&net_done_f, n_bottle_formula, &hide_f);
            }

            dl?;

            BottleDownloader::verify_checksum(&tarball_path, &sha256)?;

            let extract_dir = temp_dir.path().join(&name);
            BottleDownloader::extract(&tarball_path, &extract_dir)?;

            Ok::<_, WaxError>((name, version, extract_dir, sha256, rebuild))
        });
    }

    // Collect results; abort remaining tasks immediately on cancellation.
    // Install each extracted bottle as soon as it becomes available.
    let mut failed_packages = Vec::new();
    let mut cancelled = false;

    while let Some(handle) = tasks.join_next().await {
        if cancelled || crate::signal::is_shutdown_requested() {
            tasks.abort_all();
            cancelled = true;
            continue;
        }
        match handle {
            Ok(Ok((name, version, extract_dir, bottle_sha, bottle_rebuild))) => {
                let spinner = if quiet {
                    ProgressBar::hidden()
                } else {
                    let pb = multi.add(ProgressBar::new_spinner());
                    pb.set_style(
                        ProgressStyle::default_spinner()
                            .template("{spinner:.cyan} {msg}")
                            .unwrap()
                            .tick_chars(crate::ui::SPINNER_TICK_CHARS),
                    );
                    pb.enable_steady_tick(std::time::Duration::from_millis(80));
                    pb
                };
                match install_extracted_bottle(
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
                    run_scripts,
                    None,
                    Some(spinner.clone()),
                )
                .await
                {
                    Ok(()) => {
                        spinner.finish_and_clear();
                        if !quiet {
                            println!("+ {}@{}", style(&name).magenta(), style(&version).dim());
                        }
                    }
                    Err(e) => {
                        spinner.finish_and_clear();
                        failed_packages.push(format!("{}", e));
                    }
                }
            }
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

    hide_formula_overall.store(true, Ordering::SeqCst);
    if let Some(poller) = formula_overall_poller {
        let _ = poller.await;
    }

    if cancelled {
        return Err(WaxError::Interrupted);
    }

    if !failed_packages.is_empty() && !quiet {
        for err in &failed_packages {
            eprintln!("{}", err);
        }
        if all_to_install.len() == failed_packages.len() {
            return Err(WaxError::InstallError(
                "All package downloads failed".to_string(),
            ));
        }
    }

    check_cancelled()?;
    if owns_formula_multi {
        clear_active_multi();
    }
    drop(multi);

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
    if let Some(task) = cask_task {
        task.await
            .map_err(|e| WaxError::InstallError(format!("cask task failed: {}", e)))??;
    }
    hint_user_prefix_path_if_needed(install_mode, quiet);
    Ok(())
}

fn infer_artifact_type_from_cask_artifacts(
    details: &crate::api::CaskDetails,
) -> Option<&'static str> {
    let artifacts = details.artifacts.as_ref()?;

    if artifacts
        .iter()
        .any(|a| matches!(a, crate::api::CaskArtifact::Pkg { .. }))
    {
        return Some("pkg");
    }

    if artifacts
        .iter()
        .any(|a| matches!(a, crate::api::CaskArtifact::Binary { .. }))
    {
        return Some("binary");
    }

    // Many app-distributing casks use extensionless endpoints; default to DMG
    // on macOS so we can proceed and let staging logic handle extraction.
    if cfg!(target_os = "macos")
        && artifacts.iter().any(|a| {
            matches!(
                a,
                crate::api::CaskArtifact::App { .. }
                    | crate::api::CaskArtifact::Suite { .. }
                    | crate::api::CaskArtifact::Font { .. }
                    | crate::api::CaskArtifact::Manpage { .. }
                    | crate::api::CaskArtifact::Artifact { .. }
            )
        })
    {
        return Some("dmg");
    }

    None
}

fn check_already_installed_formula_linkages(
    packages: &[String],
    installed_packages: &HashMap<String, InstalledPackage>,
) -> Result<Vec<PathBuf>> {
    check_already_installed_formula_linkages_with_cellar(packages, installed_packages, |mode| {
        mode.cellar_path()
    })
}

fn check_already_installed_formula_linkages_with_cellar<F>(
    packages: &[String],
    installed_packages: &HashMap<String, InstalledPackage>,
    mut cellar_for_mode: F,
) -> Result<Vec<PathBuf>>
where
    F: FnMut(InstallMode) -> Result<PathBuf>,
{
    let mut checked = Vec::new();

    for package in packages {
        let Some(installed_package) = installed_packages.get(package) else {
            continue;
        };

        let version_dir = cellar_for_mode(installed_package.install_mode)?
            .join(&installed_package.name)
            .join(&installed_package.version);

        if !version_dir.exists() {
            continue;
        }

        BottleDownloader::validate_runtime(&version_dir).map_err(|err| {
            WaxError::InstallError(format!(
                "{} is already installed, but runtime linkage check failed: {}. Run wax reinstall {}",
                package, err, package
            ))
        })?;
        checked.push(version_dir);
    }

    Ok(checked)
}

/// Pick the cellar version directory inside an extracted bottle (`name/<version>/...`).
/// Uses exact `stable` or `stable_*` rebuild suffixes only (avoids `1.1` matching `1.10`).
fn cellar_version_from_bottle_layout(name_dir: &Path, stable: &str, bottle_rebuild: u32) -> String {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(name_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if !entry.path().is_dir() {
                continue;
            }
            let entry_name = entry.file_name().to_string_lossy().to_string();
            if entry_name == stable || entry_name.starts_with(&format!("{stable}_")) {
                candidates.push(entry_name);
            }
        }
    }
    if !candidates.is_empty() {
        crate::version::sort_versions(&mut candidates);
        return candidates.pop().expect("non-empty");
    }
    if bottle_rebuild > 0 {
        format!("{stable}_{bottle_rebuild}")
    } else {
        stable.to_string()
    }
}

#[allow(clippy::too_many_arguments)]
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
    run_scripts: bool,
    multi: Option<&MultiProgress>,
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
                    let line = format!("  {} {}", style(name).magenta(), style($msg).dim());
                    if let Some(ref m) = multi {
                        let _ = m.println(&line);
                    } else {
                        println!("{}", line);
                    }
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
        cellar_version_from_bottle_layout(&name_dir, version, bottle_rebuild)
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
            .or_else(|_| crate::sudo::sudo_remove(&formula_cellar).map(|_| ()))
            .map_err(|e| {
                WaxError::InstallError(format!(
                    "Failed to clean old version at {}: {}",
                    formula_cellar.display(),
                    e
                ))
            })?;
    }
    tokio::fs::create_dir_all(&formula_cellar)
        .await
        .or_else(|_| crate::sudo::sudo_mkdir(&formula_cellar))
        .map_err(|e| {
            WaxError::InstallError(format!(
                "Failed to create cellar directory {}: {}",
                formula_cellar.display(),
                e
            ))
        })?;

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

    if run_scripts && state.load().await?.contains_key(name) {
        // Auto-run postinstall if possible
        if let Ok(formulae) = state.load_formulae_from_cache().await {
            if let Some(f) = formulae
                .iter()
                .find(|f| f.name == name || f.full_name == name)
            {
                if f.post_install_defined {
                    let _ = postinstall_impl(name, install_mode, true).await;
                }
            }
        }
    }

    let package = InstalledPackage {
        name: name.to_string(),
        version: cellar_version.clone(),
        platform: platform.to_string(),
        install_date: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
        install_mode,
        from_source: false,
        bottle_rebuild,
        bottle_sha256: Some(bottle_sha),
        pinned: false,
    };
    state.add(package).await?;

    if !quiet && existing_pb.is_none() {
        println!(
            "+ {}@{}",
            style(name).magenta(),
            style(&cellar_version).dim()
        );
    }

    Ok(())
}

/// Per-cask install pipeline failure (download, verify, disk, or install).
enum CaskPipelineFail {
    Download { name: String, err: WaxError },
    Checksum { name: String, err: WaxError },
    Install { name: String, err: WaxError },
}

fn reuse_download_bar_as_install_spinner(pb: &ProgressBar, prefix: &str) {
    pb.disable_steady_tick();
    pb.reset();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {prefix:.bold} {wide_msg}")
            .unwrap()
            .tick_chars(crate::ui::SPINNER_TICK_CHARS),
    );
    pb.set_prefix(prefix.to_string());
    pb.set_message(String::new());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
}

/// Clears one `MultiProgress` row when dropped (after verify + install for that cask).
struct FinishProgressLine<'a>(&'a ProgressBar);

impl Drop for FinishProgressLine<'_> {
    fn drop(&mut self) {
        self.0.finish_and_clear();
    }
}

/// One increment per package when its download attempt finishes (ok or fail). When all have
/// reached that point, `hide_overall` tells the aggregate bar poller to exit and clear.
fn note_aggregate_download_row_done(done: &AtomicUsize, total: usize, hide_overall: &AtomicBool) {
    if total == 0 {
        return;
    }
    let c = done.fetch_add(1, Ordering::SeqCst) + 1;
    if c == total {
        hide_overall.store(true, Ordering::SeqCst);
    }
}

#[instrument(skip(cache))]
async fn install_casks(
    cache: &Cache,
    cask_names: &[String],
    dry_run: bool,
    quiet: bool,
    force_reinstall: bool,
) -> Result<()> {
    let start = std::time::Instant::now();

    // Reuse the globally active MultiProgress if one is running (e.g. upgrade),
    // so download bars appear inside the existing render layer instead of a
    // competing one that causes terminal tearing.
    let multi: Arc<MultiProgress> =
        Arc::new(crate::signal::clone_active_multi().unwrap_or_default());

    let casks = cache.load_casks().await?;
    let _state = CaskState::new()?;
    let mut installed_casks = _state.load().await?;

    if cfg!(target_os = "macos") {
        for (name, cask) in discover_manually_installed_casks(&casks).await? {
            installed_casks.entry(name).or_insert(cask);
        }
    }

    let mut to_install = Vec::new(); // macOS: full CaskInstaller path
    let mut linux_cask_installs = Vec::new(); // Linux: snap → flatpak → native PM
    let mut already_installed = Vec::new();

    for cask_name in cask_names {
        if installed_casks.contains_key(cask_name) && !force_reinstall {
            already_installed.push(cask_name.clone());
        } else if cfg!(target_os = "macos") {
            if casks
                .iter()
                .any(|c| &c.token == cask_name || &c.full_token == cask_name)
            {
                to_install.push(cask_name.clone());
            } else {
                eprintln!("{}: cask not found", style(cask_name).magenta());
            }
        } else {
            // On Linux, Homebrew cask artifacts are macOS-only.
            // Route all cask requests through snap/flatpak/native PM instead.
            linux_cask_installs.push(cask_name.clone());
        }
    }

    if !already_installed.is_empty() {
        for name in &already_installed {
            let _ = multi.println(format!("{} is already installed", style(name).magenta()));
        }
    }

    if to_install.is_empty() && linux_cask_installs.is_empty() {
        return Ok(());
    }

    if dry_run {
        let _ = multi.println("dry run - no changes made");
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
                let _permit = sem.acquire().await.map_err(|e| {
                    WaxError::InstallError(format!("cask detail semaphore closed: {e}"))
                })?;
                let details = api.fetch_cask_details(&name).await?;
                let artifact_type = if let Some(t) = detect_artifact_type(&details.url) {
                    t
                } else if let Some(t) = inst.probe_artifact_type(&details.url).await {
                    t
                } else if details
                    .artifacts
                    .as_ref()
                    .map(|a| {
                        a.iter()
                            .any(|art| matches!(art, crate::api::CaskArtifact::Binary { .. }))
                    })
                    .unwrap_or(false)
                {
                    "binary"
                } else if let Some(t) = infer_artifact_type_from_cask_artifacts(&details) {
                    t
                } else {
                    return Err(WaxError::InstallError(format!(
                        "Unsupported artifact type for URL: {}",
                        details.url
                    )));
                };
                Ok::<_, WaxError>((name, details, artifact_type.to_string()))
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

    if resolved.is_empty() && linux_cask_installs.is_empty() {
        return Err(WaxError::InstallError(
            "No casks could be resolved".to_string(),
        ));
    }

    // --- Phase 2: per-cask pipelines (download → verify → install) with bounded overlap ---
    // While some casks are still downloading, others may already be installing. State persistence
    // is serialized so concurrent installs do not corrupt the cask JSON.
    const CASK_PIPELINE_CONCURRENCY: usize = 15;

    // Register our MultiProgress for nested cask helpers (preflight, etc.) only once we know
    // we are past early returns; standalone installs own the global slot until phase 2 ends.
    let owns_multi_globals = crate::signal::clone_active_multi().is_none();
    if owns_multi_globals {
        crate::signal::set_active_multi((*multi).clone());
    }

    let cask_count = resolved.len();

    // Aggregate download progress on the top row; per-cask rows sit below and switch to
    // install spinners in place (avoids fighting an overall bar at the bottom).
    // Skip the overall "All downloads" row for formula bottles to clean up UI.
    let pipeline_totals: Option<DownloadTotals> = None;

    let hide_overall_downloads = Arc::new(AtomicBool::new(false));
    let network_phase_done = Arc::new(AtomicUsize::new(0));

    let overall_poller = if let Some(totals) = pipeline_totals.as_ref() {
        if cask_count == 0 {
            None
        } else {
            let overall_pb = multi.insert(0, ProgressBar::new(0));
            overall_pb.set_style(
                ProgressStyle::default_bar()
                    .template(PROGRESS_BAR_TEMPLATE)
                    .unwrap()
                    .progress_chars(PROGRESS_BAR_CHARS),
            );
            overall_pb.set_message("All downloads");
            let totals_w = totals.clone();
            let overall_w = overall_pb.clone();
            let hide_w = Arc::clone(&hide_overall_downloads);
            let poller = tokio::spawn(async move {
                loop {
                    if hide_w.load(Ordering::Relaxed) {
                        overall_w.finish_and_clear();
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    if hide_w.load(Ordering::Relaxed) {
                        overall_w.finish_and_clear();
                        return;
                    }
                    let pos = totals_w.downloaded.load(Ordering::Relaxed);
                    let len = totals_w.expected.load(Ordering::Relaxed);
                    let cap = len.max(pos).max(1);
                    overall_w.set_length(cap);
                    overall_w.set_position(pos);
                }
            });
            Some(poller)
        }
    } else {
        None
    };

    // One JoinSet task per cask so work runs on the runtime thread pool (true overlap of
    // I/O and CPU-heavy install steps). A semaphore caps how many pipelines run at once.
    let pipeline_sem = Arc::new(Semaphore::new(CASK_PIPELINE_CONCURRENCY));
    let mut pipeline_tasks = JoinSet::new();

    for (name, details, artifact_type) in resolved {
        let multi = Arc::clone(&multi);
        let installer = Arc::clone(&installer);
        let dl_totals = pipeline_totals.clone();
        let pipeline_sem = Arc::clone(&pipeline_sem);
        let hide_dl = Arc::clone(&hide_overall_downloads);
        let net_done = Arc::clone(&network_phase_done);
        pipeline_tasks.spawn(async move {
            let _permit = pipeline_sem
                .acquire()
                .await
                .map_err(|_| CaskPipelineFail::Download {
                    name: name.clone(),
                    err: WaxError::InstallError("download worker cancelled".into()),
                })?;

            if let Err(e) = check_cancelled() {
                return Err(CaskPipelineFail::Download { name, err: e });
            }

            let temp_dir = TempDir::new().map_err(|e| CaskPipelineFail::Download {
                name: name.clone(),
                err: e.into(),
            })?;
            let download_path =
                temp_dir
                    .path()
                    .join(format!("{}.{}", name, artifact_type.as_str()));
            let pb = multi.insert_from_back(1, ProgressBar::new(0));
            pb.set_style(
                ProgressStyle::default_bar()
                    .template(PROGRESS_BAR_PREFIX_TEMPLATE)
                    .unwrap()
                    .progress_chars(PROGRESS_BAR_CHARS),
            );
            pb.set_prefix(name.clone());
            if let Err(e) = installer
                .download_cask(&details.url, &download_path, Some(&pb), dl_totals.as_ref())
                .await
            {
                pb.finish_and_clear();
                note_aggregate_download_row_done(&net_done, cask_count, &hide_dl);
                return Err(CaskPipelineFail::Download { name, err: e });
            }

            reuse_download_bar_as_install_spinner(&pb, details.token.as_str());
            pb.set_message(format!("{}", style("verifying checksum…").dim()));

            if let Err(e) = check_cancelled() {
                pb.finish_and_clear();
                note_aggregate_download_row_done(&net_done, cask_count, &hide_dl);
                return Err(CaskPipelineFail::Download { name, err: e });
            }

            let installed_cask = {
                let _line_done = FinishProgressLine(&pb);
                if let Err(e) = CaskInstaller::verify_checksum(&download_path, &details.sha256) {
                    note_aggregate_download_row_done(&net_done, cask_count, &hide_dl);
                    return Err(CaskPipelineFail::Checksum { name, err: e });
                }
                note_aggregate_download_row_done(&net_done, cask_count, &hide_dl);
                install_from_downloaded(&details, artifact_type.as_str(), &download_path, &pb).await
            };

            match installed_cask {
                Ok(installed_cask) => {
                    if !quiet {
                        let _ = multi.println(format!(
                            "{} {} (cask) {}",
                            style("✓").green().bold(),
                            style(&name).magenta(),
                            style(&details.version).dim()
                        ));
                    }
                    Ok((name, installed_cask, details))
                }
                Err(e) => Err(CaskPipelineFail::Install { name, err: e }),
            }
        });
    }

    let mut pipeline_outcomes = Vec::new();
    let mut successful_casks: Vec<(String, InstalledCask, crate::api::CaskDetails)> = Vec::new();
    while let Some(join_res) = pipeline_tasks.join_next().await {
        match join_res {
            Ok(Ok((name, installed_cask, details))) => {
                successful_casks.push((name, installed_cask, details));
            }
            Ok(Err(e)) => pipeline_outcomes.push(Err(e)),
            Err(e) => eprintln!("{} task error: {}", style("✗").red(), e),
        }
    }

    // Serialize cask state updates to avoid file corruption from concurrent writes.
    if !successful_casks.is_empty() {
        let cask_state = CaskState::new().map_err(|e| WaxError::InstallError(e.to_string()))?;
        for (name, installed_cask, details) in successful_casks {
            if let Err(e) = cask_state
                .add_with_details(installed_cask, Some(&details))
                .await
            {
                pipeline_outcomes.push(Err(CaskPipelineFail::Install { name, err: e }));
            } else {
                pipeline_outcomes.push(Ok(()));
            }
        }
    }

    // Ensure the aggregate bar task always stops (e.g. join error before a pipeline counted).
    hide_overall_downloads.store(true, Ordering::SeqCst);
    if let Some(poller) = overall_poller {
        let _ = poller.await;
    }

    check_cancelled()?;

    let mut installed_count = 0;
    let mut failed = Vec::new();
    for outcome in pipeline_outcomes {
        match outcome {
            Ok(()) => installed_count += 1,
            Err(CaskPipelineFail::Download { name, err }) => {
                eprintln!(
                    "{} {} download failed: {}",
                    style("✗").red(),
                    style(&name).magenta(),
                    err
                );
                failed.push(name);
            }
            Err(CaskPipelineFail::Checksum { name, err }) => {
                eprintln!(
                    "{} {} checksum failed: {}",
                    style("✗").red(),
                    style(&name).magenta(),
                    err
                );
                failed.push(name);
            }
            Err(CaskPipelineFail::Install { name, err }) => {
                eprintln!(
                    "{} {} failed: {}",
                    style("✗").red(),
                    style(&name).magenta(),
                    err
                );
                failed.push(name);
            }
        }
    }

    if owns_multi_globals {
        crate::signal::clear_active_multi();
    }

    // Drop multi before summary to keep output stable.
    drop(multi);

    if !linux_cask_installs.is_empty() {
        let pm = SystemPm::detect().await.ok_or_else(|| {
            WaxError::InstallError(
                "No supported package manager found for Linux cask install".to_string(),
            )
        })?;

        for name in &linux_cask_installs {
            match pm.install_cask(name).await {
                Ok(()) => {
                    if !quiet {
                        println!(
                            "{} {} installed",
                            style("✓").green().bold(),
                            style(name).magenta(),
                        );
                    }
                    installed_count += 1;
                }
                Err(e) => {
                    eprintln!(
                        "{} {} failed: {}",
                        style("✗").red(),
                        style(name).magenta(),
                        e
                    );
                    failed.push(name.clone());
                }
            }
        }
    }

    let elapsed = start.elapsed();
    if failed.is_empty() {
        if !quiet {
            println!(
                "\n{} {} installed [{}ms]",
                installed_count,
                if installed_count == 1 {
                    "cask"
                } else {
                    "casks"
                },
                elapsed.as_millis()
            );
        }
        Ok(())
    } else {
        if !quiet {
            println!(
                "\n{}/{} casks installed ({} failed) [{}ms]",
                installed_count,
                installed_count + failed.len(),
                failed.len(),
                elapsed.as_millis()
            );
        }
        Err(WaxError::InstallError(format!(
            "Some casks failed: {}",
            failed.join(", ")
        )))
    }
}

pub async fn postinstall(
    _cache: &Cache,
    package_names: &[String],
    user: bool,
    global: bool,
) -> Result<()> {
    let install_mode = match InstallMode::from_flags(user, global)? {
        Some(mode) => mode,
        None => InstallMode::detect(),
    };

    for name in package_names {
        postinstall_impl(name, install_mode, false).await?;
    }

    Ok(())
}

async fn postinstall_impl(name: &str, _install_mode: InstallMode, quiet: bool) -> Result<()> {
    if !quiet {
        println!(
            "  {} {}",
            style(name).magenta(),
            style("running postinstall...").dim()
        );
    }

    // Try to run Homebrew's postinstall if brew is installed
    if let Some(brew_path) = find_in_path("brew") {
        let mut cmd = tokio::process::Command::new(&brew_path);
        cmd.arg("postinstall").arg(name);

        // We might need to set HOMEBREW_PREFIX or similar if wax's prefix is different
        // but for now let's assume standard prefix
        match cmd.status().await {
            Ok(status) if status.success() => return Ok(()),
            _ => {
                if !quiet {
                    debug!("'brew postinstall' failed or was not relevant for {}", name);
                }
            }
        }
    }

    // Fallback: Acknowledge that native post-install is a gap in parity
    if !quiet {
        debug!("Postinstall for {} is defined but native execution is not yet supported in wax without Homebrew.", name);
    }

    Ok(())
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
}

/// Install a cask from an already-downloaded file (skips download).
/// `line` must already be switched to an install spinner (see `reuse_download_bar_as_install_spinner`).
async fn install_from_downloaded(
    cask: &crate::api::CaskDetails,
    artifact_type: &str,
    download_path: &std::path::Path,
    line: &ProgressBar,
) -> Result<InstalledCask> {
    let installer = CaskInstaller::new();

    macro_rules! step {
        ($msg:expr) => {
            line.set_message(format!("{}", style($msg).dim()));
        };
    }

    step!("staging...");
    let cask_dir = CaskState::caskroom_dir().join(&cask.token);
    let version_dir = cask_dir.join(&cask.version);

    // Clean up if version_dir already exists to ensure a fresh extraction
    if version_dir.exists() {
        tokio::fs::remove_dir_all(&version_dir).await?;
    }

    let staging =
        StagingContext::new_in_dir(download_path, artifact_type, &cask.url, version_dir.clone())
            .await?;
    let mut rollback = RollbackContext::new();

    // Ensure we rollback the version_dir if installation fails
    rollback.add(version_dir.clone());

    let mut binary_paths: Vec<String> = Vec::new();
    let mut installed_app_name: Option<String> = None;

    if let Some(artifacts) = &cask.artifacts {
        for artifact in artifacts {
            match artifact {
                CaskArtifact::App { app } => {
                    if let Some(source) = app.first().and_then(|v| v.as_str()) {
                        step!(format!("installing app: {}", source));
                        installer
                            .install_app(&staging, &mut rollback, source)
                            .await?;
                        installed_app_name = Some(source.to_string());
                    }
                }
                CaskArtifact::Pkg { pkg } => {
                    if let Some(source) = pkg.first().and_then(|v| v.as_str()) {
                        step!(format!("installing pkg: {}", source));
                        installer
                            .install_pkg(&staging, &mut rollback, source)
                            .await?;
                    }
                }
                CaskArtifact::Binary { binary } => {
                    if let Some(source) = binary.first().and_then(|v| v.as_str()) {
                        let target = if binary.len() > 1 {
                            binary
                                .get(1)
                                .and_then(|v| v.as_object())
                                .and_then(|obj| obj.get("target"))
                                .and_then(|v| v.as_str())
                        } else {
                            None
                        };
                        step!(format!("installing binary: {}", source));
                        if let Some(path) = installer
                            .install_binary(
                                &staging,
                                &mut rollback,
                                source,
                                target,
                                Some(&cask.token),
                            )
                            .await?
                        {
                            binary_paths.push(path.display().to_string());
                        }
                    }
                }
                CaskArtifact::Font { font } => {
                    if let Some(source) = font.first().and_then(|v| v.as_str()) {
                        step!(format!("installing font: {}", source));
                        installer
                            .install_font(&staging, &mut rollback, source)
                            .await?;
                    }
                }
                CaskArtifact::Manpage { manpage } => {
                    if let Some(source) = manpage.first().and_then(|v| v.as_str()) {
                        step!(format!("installing manpage: {}", source));
                        installer
                            .install_manpage(&staging, &mut rollback, source)
                            .await?;
                    }
                }
                CaskArtifact::Artifact { artifact } => {
                    if let (Some(source), Some(target)) = (
                        artifact.first().and_then(|v| v.as_str()),
                        artifact
                            .get(1)
                            .and_then(|v| v.as_object())
                            .and_then(|o| o.get("target"))
                            .and_then(|v| v.as_str()),
                    ) {
                        step!(format!("installing artifact: {} to {}", source, target));
                        installer
                            .install_artifact(&staging, &mut rollback, source, target)
                            .await?;
                    }
                }
                CaskArtifact::Dictionary { dictionary } => {
                    if let Some(source) = dictionary.first().and_then(|v| v.as_str()) {
                        step!(format!("installing dictionary: {}", source));
                        installer
                            .install_generic_directory(
                                &staging,
                                &mut rollback,
                                source,
                                &dirs::home_dir()?.join("Library/Dictionaries"),
                            )
                            .await?;
                    }
                }
                CaskArtifact::Colorpicker { colorpicker } => {
                    if let Some(source) = colorpicker.first().and_then(|v| v.as_str()) {
                        step!(format!("installing colorpicker: {}", source));
                        installer
                            .install_generic_directory(
                                &staging,
                                &mut rollback,
                                source,
                                &dirs::home_dir()?.join("Library/ColorPickers"),
                            )
                            .await?;
                    }
                }
                CaskArtifact::Prefpane { prefpane } => {
                    if let Some(source) = prefpane.first().and_then(|v| v.as_str()) {
                        step!(format!("installing prefpane: {}", source));
                        installer
                            .install_generic_directory(
                                &staging,
                                &mut rollback,
                                source,
                                &dirs::home_dir()?.join("Library/PreferencePanes"),
                            )
                            .await?;
                    }
                }
                CaskArtifact::Qlplugin { qlplugin } => {
                    if let Some(source) = qlplugin.first().and_then(|v| v.as_str()) {
                        step!(format!("installing qlplugin: {}", source));
                        installer
                            .install_generic_directory(
                                &staging,
                                &mut rollback,
                                source,
                                &dirs::home_dir()?.join("Library/QuickLook"),
                            )
                            .await?;
                    }
                }
                CaskArtifact::ScreenSaver { screen_saver } => {
                    if let Some(source) = screen_saver.first().and_then(|v| v.as_str()) {
                        step!(format!("installing screen saver: {}", source));
                        installer
                            .install_generic_directory(
                                &staging,
                                &mut rollback,
                                source,
                                &dirs::home_dir()?.join("Library/Screen Savers"),
                            )
                            .await?;
                    }
                }
                CaskArtifact::Service { service } => {
                    if let Some(source) = service.first().and_then(|v| v.as_str()) {
                        step!(format!("installing service: {}", source));
                        installer
                            .install_generic_directory(
                                &staging,
                                &mut rollback,
                                source,
                                &dirs::home_dir()?.join("Library/Services"),
                            )
                            .await?;
                    }
                }
                CaskArtifact::Suite { suite } => {
                    if let Some(source) = suite.first().and_then(|v| v.as_str()) {
                        step!(format!("installing suite: {}", source));
                        installer
                            .install_generic_directory(
                                &staging,
                                &mut rollback,
                                source,
                                &CaskInstaller::applications_dir()?,
                            )
                            .await?;
                    }
                }
                CaskArtifact::BashCompletion { bash_completion } => {
                    if let Some(source) = bash_completion.first().and_then(|v| v.as_str()) {
                        let target = bash_completion
                            .get(1)
                            .and_then(|v| v.as_object())
                            .and_then(|o| o.get("target"))
                            .and_then(|v| v.as_str());
                        step!(format!("installing bash completion: {}", source));
                        installer
                            .install_completion(
                                &staging,
                                &mut rollback,
                                source,
                                "bash",
                                &cask.token,
                                target,
                            )
                            .await?;
                    }
                }
                CaskArtifact::ZshCompletion { zsh_completion } => {
                    if let Some(source) = zsh_completion.first().and_then(|v| v.as_str()) {
                        let target = zsh_completion
                            .get(1)
                            .and_then(|v| v.as_object())
                            .and_then(|o| o.get("target"))
                            .and_then(|v| v.as_str());
                        step!(format!("installing zsh completion: {}", source));
                        installer
                            .install_completion(
                                &staging,
                                &mut rollback,
                                source,
                                "zsh",
                                &cask.token,
                                target,
                            )
                            .await?;
                    }
                }
                CaskArtifact::FishCompletion { fish_completion } => {
                    if let Some(source) = fish_completion.first().and_then(|v| v.as_str()) {
                        let target = fish_completion
                            .get(1)
                            .and_then(|v| v.as_object())
                            .and_then(|o| o.get("target"))
                            .and_then(|v| v.as_str());
                        step!(format!("installing fish completion: {}", source));
                        installer
                            .install_completion(
                                &staging,
                                &mut rollback,
                                source,
                                "fish",
                                &cask.token,
                                target,
                            )
                            .await?;
                    }
                }
                CaskArtifact::Preflight {
                    preflight: Some(script),
                } => {
                    step!("skipping preflight script (not supported yet)");
                    debug!("Preflight script: {}", script);
                }
                CaskArtifact::Preflight { preflight: None } => {}
                CaskArtifact::Postflight {
                    postflight: Some(script),
                } => {
                    step!("skipping postflight script (not supported yet)");
                    debug!("Postflight script: {}", script);
                }
                CaskArtifact::Postflight { postflight: None } => {}
                _ => {}
            }
        }
    } else {
        // Fallback if no artifacts are explicitly defined (try to guess .app)
        if artifact_type == "dmg" || artifact_type == "zip" {
            let mut entries = tokio::fs::read_dir(&staging.staging_root).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("app") {
                    let Some(app_name) = path.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    step!(format!("installing guessed app: {}", app_name));
                    installer
                        .install_app(&staging, &mut rollback, app_name)
                        .await?;
                    installed_app_name = Some(app_name.to_string());
                    break;
                }
            }
        }
    }

    step!("registering...");
    rollback.commit();

    Ok(InstalledCask {
        name: cask.token.clone(),
        version: cask.version.clone(),
        install_date: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
        artifact_type: Some(artifact_type.to_string()),
        binary_paths: if binary_paths.is_empty() {
            None
        } else {
            Some(binary_paths)
        },
        app_name: installed_app_name,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        check_already_installed_formula_linkages_with_cellar, stage_binary_release_download,
        tap_name_from_qualified_package,
    };
    use crate::install::{InstallMode, InstalledPackage};
    use std::collections::HashMap;

    #[test]
    fn tap_name_from_qualified_package_uses_first_two_segments() {
        assert_eq!(
            tap_name_from_qualified_package("user/tap/package"),
            Some("user/tap".to_string())
        );
    }

    #[test]
    fn tap_name_from_qualified_package_rejects_unqualified_names() {
        assert_eq!(tap_name_from_qualified_package("package"), None);
        assert_eq!(tap_name_from_qualified_package("user/tap"), None);
    }

    #[test]
    fn already_installed_linkage_check_uses_recorded_install_location() {
        let tmp = tempfile::tempdir().unwrap();

        let cellar = tmp.path().join("Cellar");
        let version_dir = cellar.join("ripgrep/14.1.1");
        std::fs::create_dir_all(&version_dir).unwrap();

        let mut installed = HashMap::new();
        installed.insert(
            "ripgrep".to_string(),
            InstalledPackage {
                name: "ripgrep".to_string(),
                version: "14.1.1".to_string(),
                platform: "x86_64_linux".to_string(),
                install_date: 0,
                install_mode: InstallMode::User,
                from_source: false,
                bottle_rebuild: 0,
                bottle_sha256: None,
                pinned: false,
            },
        );

        let checked = check_already_installed_formula_linkages_with_cellar(
            &["ripgrep".to_string()],
            &installed,
            |mode| {
                assert_eq!(mode, InstallMode::User);
                Ok(cellar.clone())
            },
        )
        .unwrap();

        assert_eq!(checked, vec![version_dir]);
    }

    #[tokio::test]
    async fn binary_release_staging_keeps_direct_executable_downloads() {
        let tmp = tempfile::tempdir().unwrap();

        let src_dir = stage_binary_release_download(
            b"#!/bin/sh\n",
            "https://static.ampcode.com/cli/1.0.0/amp-darwin-arm64",
            "ampcode",
            tmp.path(),
        )
        .await
        .unwrap();

        let staged = src_dir.join("amp-darwin-arm64");
        assert_eq!(std::fs::read(staged).unwrap(), b"#!/bin/sh\n");
    }
}
