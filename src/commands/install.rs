use crate::api::Formula;
use crate::bottle::{detect_platform, BottleDownloader};
use crate::builder::Builder;
use crate::cache::Cache;
use crate::cask::{detect_artifact_type, CaskInstaller, CaskState, InstalledCask};
use crate::deps::resolve_dependencies;
use crate::error::{Result, WaxError};
use crate::formula_parser::FormulaParser;
use crate::install::{create_symlinks, InstallMode, InstallState, InstalledPackage};
use crate::tap::TapManager;
use crate::ui::print_success;
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use sha2::Digest;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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

    create_symlinks(&formula.name, version, cellar, false, install_mode).await?;

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
    };
    state.add(package).await?;

    spinner.finish_with_message(format!("Built and installed {}", formula.name));
    println!();
    println!(
        "+ {}@{} {}",
        style(&formula.name).white(),
        style(version).dim(),
        style("(source)").dim()
    );

    Ok(())
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
    if package_names.is_empty() {
        return Err(WaxError::InvalidInput("No packages specified".to_string()));
    }

    if cask {
        return install_multiple_casks(cache, package_names, dry_run).await;
    }

    let install_mode = match InstallMode::from_flags(user, global)? {
        Some(mode) => mode,
        None => {
            let detected = InstallMode::detect();
            if detected == InstallMode::User {
                println!(
                    "{} No write access to system directory, defaulting to per-user installation",
                    style("ℹ").blue().bold()
                );
                println!(
                    "  Install location: {}",
                    style(detected.prefix().display()).cyan()
                );
                println!(
                    "  Binaries will be in: {}",
                    style(detected.bin_path().display()).cyan()
                );
                println!(
                    "  Add to PATH: export PATH=\"{}:$PATH\"\n",
                    detected.bin_path().display()
                );
            }
            detected
        }
    };

    install_mode.validate()?;

    let start = std::time::Instant::now();

    let mut tap_manager = TapManager::new()?;
    tap_manager.load().await?;

    let formulae = cache.load_all_formulae().await?;
    let state = InstallState::new()?;
    let installed_packages = state.load().await?;
    let installed: HashSet<String> = installed_packages.keys().cloned().collect();

    let mut all_to_install = Vec::new();
    let mut already_installed = Vec::new();
    let mut errors = Vec::new();

    for package_name in package_names {
        if installed.contains(package_name) {
            already_installed.push(package_name.clone());
            continue;
        }

        let formula = if package_name.contains('/') {
            formulae
                .iter()
                .find(|f| &f.full_name == package_name || &f.name == package_name)
        } else {
            formulae.iter().find(|f| &f.name == package_name)
        };

        let formula = match formula {
            Some(f) => f,
            None => {
                let casks = cache.load_casks().await?;
                let cask_exists = casks
                    .iter()
                    .any(|c| &c.token == package_name || &c.full_token == package_name);

                if cask_exists {
                    return install_cask(cache, package_name, dry_run).await;
                }

                errors.push((
                    package_name.clone(),
                    "Not found as formula or cask. If using a custom tap, install it with: wax tap add user/repo".to_string(),
                ));
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

    if !already_installed.is_empty() {
        println!();
        for pkg in &already_installed {
            println!("✓ {} is already installed", pkg);
        }
    }

    if !errors.is_empty() {
        println!();
        for (pkg, err) in &errors {
            eprintln!("✗ {}: {}", pkg, err);
        }
        if all_to_install.is_empty() {
            return Err(WaxError::InstallError(
                "Cannot install any packages (all failed validation)".to_string(),
            ));
        }
    }

    if all_to_install.is_empty() {
        return Ok(());
    }

    let package_list = package_names
        .iter()
        .filter(|p| !already_installed.contains(p) && !errors.iter().any(|(e, _)| e == *p))
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    if all_to_install.len() > package_names.len() {
        println!();
        println!(
            "installing {} + {} {}",
            package_list,
            all_to_install.len() - package_names.len(),
            if all_to_install.len() - package_names.len() == 1 {
                "dependency"
            } else {
                "dependencies"
            }
        );
    }

    if dry_run {
        println!();
        println!("dry run - no changes made");
        return Ok(());
    }

    let platform = detect_platform();
    debug!("Detected platform: {}", platform);

    let cellar = install_mode.cellar_path();

    let multi = MultiProgress::new();
    let downloader = Arc::new(BottleDownloader::new());

    let packages_to_install: Vec<_> = all_to_install
        .iter()
        .map(|name| {
            formulae
                .iter()
                .find(|f| &f.name == name)
                .ok_or_else(|| WaxError::FormulaNotFound(name.clone()))
        })
        .collect::<Result<_>>()?;

    let semaphore = Arc::new(Semaphore::new(8));
    let mut tasks = Vec::new();

    let temp_dir = Arc::new(TempDir::new()?);

    for pkg in packages_to_install {
        let has_bottle = pkg
            .bottle
            .as_ref()
            .and_then(|b| b.stable.as_ref())
            .and_then(|s| s.files.get(&platform).or_else(|| s.files.get("all")))
            .is_some();

        if !has_bottle || build_from_source {
            if build_from_source && has_bottle {
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

        let downloader = Arc::clone(&downloader);
        let semaphore = Arc::clone(&semaphore);
        let temp_dir = Arc::clone(&temp_dir);

        let pb = multi.add(ProgressBar::new(0));
        let style = ProgressStyle::default_bar()
            .template("{msg} {bar:40.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec}")
            .unwrap()
            .progress_chars("█▓▒░ ");
        pb.set_style(style);
        pb.set_message(name.clone());

        let task = tokio::spawn(async move {
            let _permit = semaphore.acquire().await.unwrap();

            let tarball_path = temp_dir.path().join(format!("{}-{}.tar.gz", name, version));

            downloader.download(&url, &tarball_path, Some(&pb)).await?;
            pb.finish_and_clear();

            BottleDownloader::verify_checksum(&tarball_path, &sha256)?;

            let extract_dir = temp_dir.path().join(&name);
            BottleDownloader::extract(&tarball_path, &extract_dir)?;

            Ok::<_, WaxError>((name, version, extract_dir))
        });

        tasks.push(task);
    }

    let results = futures::future::join_all(tasks).await;

    let mut extracted_packages = Vec::new();
    let mut failed_packages = Vec::new();

    for result in results {
        match result {
            Ok(Ok(data)) => extracted_packages.push(data),
            Ok(Err(e)) => {
                failed_packages.push(format!("{}", e));
            }
            Err(e) => {
                failed_packages.push(format!("Task error: {}", e));
            }
        }
    }

    if !failed_packages.is_empty() {
        println!();
        for err in &failed_packages {
            eprintln!("✗ {}", err);
        }
        if extracted_packages.is_empty() {
            return Err(WaxError::InstallError(
                "All package downloads failed".to_string(),
            ));
        }
    }

    println!();
    for (name, version, extract_dir) in extracted_packages {
        let formula_cellar = cellar.join(&name).join(&version);
        tokio::fs::create_dir_all(&formula_cellar).await?;

        let actual_content_dir = extract_dir.join(&name).join(&version);
        if actual_content_dir.exists() {
            copy_dir_all(&actual_content_dir, &formula_cellar)?;
        } else {
            copy_dir_all(&extract_dir, &formula_cellar)?;
        }

        create_symlinks(&name, &version, &cellar, false, install_mode).await?;

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
        };
        state.add(package).await?;

        println!("+ {}@{}", style(&name).white(), style(&version).dim());
    }

    let elapsed = start.elapsed();

    println!();
    println!(
        "{} {} installed [{:.2}ms]",
        package_names.len(),
        if package_names.len() == 1 {
            "package"
        } else {
            "packages"
        },
        elapsed.as_millis()
    );

    Ok(())
}

fn copy_dir_all(src: &PathBuf, dst: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if ty.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[instrument(skip(cache))]
async fn install_cask(cache: &Cache, cask_name: &str, dry_run: bool) -> Result<()> {
    let start = std::time::Instant::now();

    let casks = cache.load_casks().await?;
    let _cask_summary = casks
        .iter()
        .find(|c| c.token == cask_name)
        .ok_or_else(|| WaxError::CaskNotFound(cask_name.to_string()))?;

    let state = CaskState::new()?;
    let installed_casks = state.load().await?;

    if installed_casks.contains_key(cask_name) {
        println!();
        println!("✓ {} is already installed", cask_name);
        return Ok(());
    }

    let api_client = crate::api::ApiClient::new();
    let cask = api_client.fetch_cask_details(cask_name).await?;

    let display_name = cask.name.first().unwrap_or(&cask.token);

    if dry_run {
        println!();
        println!("dry run - no changes made");
        return Ok(());
    }

    let artifact_type = detect_artifact_type(&cask.url).ok_or_else(|| {
        WaxError::InstallError(format!("Unsupported artifact type for URL: {}", cask.url))
    })?;

    let temp_dir = TempDir::new()?;
    let download_path = temp_dir
        .path()
        .join(format!("{}.{}", cask_name, artifact_type));

    let pb = ProgressBar::new(0);
    let pb_style = ProgressStyle::default_bar()
        .template("{prefix:.bold} {bar:40.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec}")
        .unwrap()
        .progress_chars("█▓▒░ ");
    pb.set_style(pb_style);
    pb.set_prefix(format!("[>] {}", display_name));

    let installer = CaskInstaller::new();
    installer
        .download_cask(&cask.url, &download_path, Some(&pb))
        .await?;
    pb.set_prefix(format!("[✓] {}", display_name));
    pb.finish_and_clear();

    CaskInstaller::verify_checksum(&download_path, &cask.sha256)?;

    let mut installed_binaries: Vec<String> = Vec::new();
    let mut binary_paths: Vec<String> = Vec::new();

    match artifact_type {
        "dmg" | "zip" => {
            let app_name = if let Some(artifacts) = &cask.artifacts {
                extract_app_name(artifacts).unwrap_or_else(|| format!("{}.app", display_name))
            } else {
                format!("{}.app", display_name)
            };

            if artifact_type == "dmg" {
                installer.install_dmg(&download_path, &app_name).await?
            } else {
                installer.install_zip(&download_path, &app_name).await?
            }
        }
        "pkg" => installer.install_pkg(&download_path).await?,
        "tar.gz" => {
            let binary_name = cask_name;
            let binary_path = installer
                .install_tarball(&download_path, binary_name)
                .await?;
            installed_binaries.push(binary_name.to_string());
            binary_paths.push(binary_path.display().to_string());
        }
        _ => {
            return Err(WaxError::InstallError(format!(
                "Unsupported artifact type: {}",
                artifact_type
            )))
        }
    }

    let installed_cask = InstalledCask {
        name: cask_name.to_string(),
        version: cask.version.clone(),
        install_date: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        artifact_type: Some(artifact_type.to_string()),
        binary_paths: if binary_paths.is_empty() {
            None
        } else {
            Some(binary_paths)
        },
    };
    state.add(installed_cask).await?;

    let elapsed = start.elapsed();

    println!();
    if !installed_binaries.is_empty() {
        println!(
            "+ {}@{} {}",
            console::style(cask_name).white(),
            console::style(&cask.version).dim(),
            console::style("(cask)").dim()
        );
        println!("  with binaries:");
        for binary in installed_binaries {
            println!("  - {}", binary);
        }
    } else {
        println!(
            "+ {}@{} {}",
            console::style(cask_name).white(),
            console::style(&cask.version).dim(),
            console::style("(cask)").dim()
        );
    }

    println!();
    println!("1 cask installed [{:.2}ms]", elapsed.as_millis());

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

#[instrument(skip(cache))]
async fn install_multiple_casks(cache: &Cache, cask_names: &[String], dry_run: bool) -> Result<()> {
    if cask_names.len() == 1 {
        return install_cask(cache, &cask_names[0], dry_run).await;
    }

    let start = std::time::Instant::now();
    let casks = cache.load_casks().await?;
    let state = CaskState::new()?;
    let installed_casks = state.load().await?;

    let mut to_install = Vec::new();
    let mut already_installed = Vec::new();
    let mut errors = Vec::new();

    for cask_name in cask_names {
        if installed_casks.contains_key(cask_name) {
            already_installed.push(cask_name.clone());
            continue;
        }

        if casks.iter().any(|c| &c.token == cask_name) {
            to_install.push(cask_name.clone());
        } else {
            errors.push((cask_name.clone(), "Cask not found".to_string()));
        }
    }

    if !already_installed.is_empty() {
        println!(
            "{} Already installed: {}",
            style("ℹ").blue().bold(),
            already_installed.join(", ")
        );
    }

    if !errors.is_empty() {
        for (cask, err) in &errors {
            eprintln!("{} {}: {}", style("✗").red().bold(), cask, err);
        }
    }

    if to_install.is_empty() {
        if errors.is_empty() {
            println!("{} Nothing to install", style("✓").green().bold());
        }
        return if errors.is_empty() {
            Ok(())
        } else {
            Err(WaxError::CaskNotFound(
                "No valid casks to install".to_string(),
            ))
        };
    }

    println!(
        "{} Installing {} {}",
        style("→").cyan().bold(),
        to_install.len(),
        if to_install.len() == 1 {
            "cask"
        } else {
            "casks"
        }
    );
    println!("  Casks: {}", to_install.join(", "));

    if dry_run {
        println!("\n{} Dry run - no changes made", style("✓").green().bold());
        return Ok(());
    }

    let mut installed_count = 0;
    let mut failed = Vec::new();

    for cask_name in &to_install {
        match install_cask(cache, cask_name, false).await {
            Ok(_) => installed_count += 1,
            Err(e) => {
                eprintln!(
                    "{} Failed to install {}: {}",
                    style("✗").red().bold(),
                    cask_name,
                    e
                );
                failed.push(cask_name.clone());
            }
        }
    }

    let elapsed = start.elapsed();

    if failed.is_empty() {
        print_success(&format!(
            "Installed {} {} in {:.1}s",
            installed_count,
            if installed_count == 1 {
                "cask"
            } else {
                "casks"
            },
            elapsed.as_secs_f64()
        ));
        Ok(())
    } else {
        println!(
            "{} Installed {}/{} casks in {:.1}s ({} failed)",
            style("⚠").yellow().bold(),
            installed_count,
            to_install.len(),
            elapsed.as_secs_f64(),
            failed.len()
        );
        Err(WaxError::InstallError(format!(
            "Some casks failed: {}",
            failed.join(", ")
        )))
    }
}
