use crate::bottle::{detect_platform, BottleDownloader};
use crate::cache::Cache;
use crate::error::{Result, WaxError};
use crate::install::{create_symlinks, InstallMode, InstallState, InstalledPackage};
use crate::lockfile::Lockfile;
use crate::ui::{copy_dir_all, PROGRESS_BAR_CHARS, PROGRESS_BAR_TEMPLATE};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
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

    if package_count == 0 {
        println!("no packages in lockfile");
        return Ok(());
    }

    let formulae = cache.load_formulae().await?;
    let state = InstallState::new()?;
    let installed_packages = state.load().await?;

    let current_platform = detect_platform();
    let mut packages_to_install = Vec::new();

    for (name, lock_pkg) in &lockfile.packages {
        let needs_install = match installed_packages.get(name) {
            Some(installed) => {
                installed.version != lock_pkg.version || installed.platform != lock_pkg.bottle
            }
            None => true,
        };

        if needs_install {
            packages_to_install.push((name.clone(), lock_pkg.clone()));
        } else {
            println!("{} is already installed", name);
        }
    }

    if packages_to_install.is_empty() {
        return Ok(());
    }

    let package_count = packages_to_install.len();

    let multi = MultiProgress::new();
    let downloader = Arc::new(BottleDownloader::new());
    let semaphore = Arc::new(Semaphore::new(8));
    let temp_dir = Arc::new(TempDir::new()?);
    let mut tasks = Vec::new();

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
            .ok_or_else(|| WaxError::BottleNotAvailable(format!("{} (no bottle info)", name)))?;

        let bottle_file = bottle_info
            .files
            .get(&lock_pkg.bottle)
            .or_else(|| bottle_info.files.get("all"))
            .ok_or_else(|| {
                WaxError::BottleNotAvailable(format!("{} for platform {}", name, lock_pkg.bottle))
            })?;

        let url = bottle_file.url.clone();
        let sha256 = bottle_file.sha256.clone();
        let version = lock_pkg.version.clone();
        let platform = lock_pkg.bottle.clone();

        let downloader = Arc::clone(&downloader);
        let semaphore = Arc::clone(&semaphore);
        let temp_dir = Arc::clone(&temp_dir);

        let pb = multi.add(ProgressBar::new(0));
        let style = ProgressStyle::default_bar()
            .template(PROGRESS_BAR_TEMPLATE)
            .unwrap()
            .progress_chars(PROGRESS_BAR_CHARS);
        pb.set_style(style);
        pb.set_message(name.clone());

        let name_clone = name.clone();
        let task = tokio::spawn(async move {
            let _permit = semaphore.acquire().await.unwrap();

            let tarball_path = temp_dir
                .path()
                .join(format!("{}-{}.tar.gz", name_clone, version));

            downloader.download(&url, &tarball_path, Some(&pb)).await?;
            pb.finish_and_clear();

            BottleDownloader::verify_checksum(&tarball_path, &sha256)?;

            let extract_dir = temp_dir.path().join(&name_clone);
            BottleDownloader::extract(&tarball_path, &extract_dir)?;

            Ok::<_, WaxError>((name_clone, version, platform, extract_dir))
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

    let cellar = install_mode.cellar_path();

    println!();
    for (name, version, platform, extract_dir) in extracted_packages {
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
        };
        state.add(package).await?;

        println!("+ {}", style(&name).magenta());
    }

    let elapsed = start.elapsed();

    println!();
    println!(
        "{} {} synced [{:.2}ms]",
        package_count,
        if package_count == 1 {
            "package"
        } else {
            "packages"
        },
        elapsed.as_millis()
    );

    Ok(())
}
