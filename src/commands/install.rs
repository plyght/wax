use crate::bottle::{cellar_path, detect_platform, BottleDownloader};
use crate::cache::Cache;
use crate::cask::{detect_artifact_type, CaskInstaller, CaskState, InstalledCask};
use crate::deps::resolve_dependencies;
use crate::error::{Result, WaxError};
use crate::install::{create_symlinks, InstallState, InstalledPackage};
use crate::ui::print_success;
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Semaphore;
use tracing::{debug, instrument};

#[instrument(skip(cache))]
pub async fn install(cache: &Cache, formula_name: &str, dry_run: bool, cask: bool) -> Result<()> {
    if cask {
        return install_cask(cache, formula_name, dry_run).await;
    }

    let start = std::time::Instant::now();

    let formulae = cache.load_formulae().await?;
    let formula = formulae
        .iter()
        .find(|f| f.name == formula_name)
        .ok_or_else(|| WaxError::FormulaNotFound(formula_name.to_string()))?;

    let state = InstallState::new()?;
    let installed_packages = state.load().await?;
    let installed: HashSet<String> = installed_packages.keys().cloned().collect();

    if installed.contains(formula_name) {
        println!(
            "{} {} is already installed",
            style("ℹ").blue().bold(),
            formula_name
        );
        return Ok(());
    }

    let to_install = resolve_dependencies(formula, &formulae, &installed)?;

    if to_install.is_empty() {
        println!(
            "{} All dependencies satisfied, installing {}",
            style("ℹ").blue().bold(),
            formula_name
        );
    } else {
        println!(
            "{} Installing {} with {} {}",
            style("→").cyan().bold(),
            formula_name,
            to_install.len(),
            if to_install.len() == 1 {
                "dependency"
            } else {
                "dependencies"
            }
        );
        println!("  Packages: {}", to_install.join(", "));
    }

    if dry_run {
        println!("\n{} Dry run - no changes made", style("✓").green().bold());
        return Ok(());
    }

    let platform = detect_platform();
    debug!("Detected platform: {}", platform);

    let multi = MultiProgress::new();
    let downloader = Arc::new(BottleDownloader::new());

    let packages_to_install: Vec<_> = to_install
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
            .template("{prefix:.bold} {bar:40.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec}")
            .unwrap()
            .progress_chars("█▓▒░ ");
        pb.set_style(style);
        pb.set_prefix(format!("[>] {}", name));

        let task = tokio::spawn(async move {
            let _permit = semaphore.acquire().await.unwrap();

            let tarball_path = temp_dir.path().join(format!("{}-{}.tar.gz", name, version));

            downloader.download(&url, &tarball_path, Some(&pb)).await?;
            pb.set_prefix(format!("[✓] {}", name));
            pb.finish();

            BottleDownloader::verify_checksum(&tarball_path, &sha256)?;

            let extract_dir = temp_dir.path().join(&name);
            BottleDownloader::extract(&tarball_path, &extract_dir)?;

            Ok::<_, WaxError>((name, version, extract_dir))
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

    let cellar = cellar_path();

    for (name, version, extract_dir) in extracted_packages {
        let formula_cellar = cellar.join(&name).join(&version);
        tokio::fs::create_dir_all(&formula_cellar).await?;

        let actual_content_dir = extract_dir.join(&name).join(&version);
        if actual_content_dir.exists() {
            copy_dir_all(&actual_content_dir, &formula_cellar)?;
        } else {
            copy_dir_all(&extract_dir, &formula_cellar)?;
        }

        create_symlinks(&name, &version, &cellar, false).await?;

        let package = InstalledPackage {
            name: name.clone(),
            version: version.clone(),
            platform: platform.clone(),
            install_date: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        };
        state.add(package).await?;

        println!("{} Installed {}", style("✓").green().bold(), name);
    }

    let elapsed = start.elapsed();
    print_success(&format!(
        "Installed {} in {:.1}s",
        formula_name,
        elapsed.as_secs_f64()
    ));

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
        println!(
            "{} {} is already installed",
            style("ℹ").blue().bold(),
            cask_name
        );
        return Ok(());
    }

    let api_client = crate::api::ApiClient::new();
    let cask = api_client.fetch_cask_details(cask_name).await?;

    let display_name = cask.name.first().unwrap_or(&cask.token);
    println!(
        "{} Installing {} {}",
        style("→").cyan().bold(),
        display_name,
        cask.version
    );

    if dry_run {
        println!("\n{} Dry run - no changes made", style("✓").green().bold());
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
    let style = ProgressStyle::default_bar()
        .template("{prefix:.bold} {bar:40.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec}")
        .unwrap()
        .progress_chars("█▓▒░ ");
    pb.set_style(style);
    pb.set_prefix(format!("[>] {}", display_name));

    let installer = CaskInstaller::new();
    installer
        .download_cask(&cask.url, &download_path, Some(&pb))
        .await?;
    pb.set_prefix(format!("[✓] {}", display_name));
    pb.finish();

    CaskInstaller::verify_checksum(&download_path, &cask.sha256)?;

    let app_name = if let Some(artifacts) = &cask.artifacts {
        extract_app_name(artifacts).unwrap_or_else(|| format!("{}.app", display_name))
    } else {
        format!("{}.app", display_name)
    };

    match artifact_type {
        "dmg" => installer.install_dmg(&download_path, &app_name).await?,
        "pkg" => installer.install_pkg(&download_path).await?,
        "zip" => installer.install_zip(&download_path, &app_name).await?,
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
    };
    state.add(installed_cask).await?;

    let elapsed = start.elapsed();
    print_success(&format!(
        "Installed {} in {:.1}s",
        display_name,
        elapsed.as_secs_f64()
    ));

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
