use crate::cache::Cache;
use crate::cask::CaskState;
use crate::error::{Result, WaxError};
use crate::install::{remove_symlinks, InstallState};
use console::style;
use inquire::Confirm;
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn uninstall(cache: &Cache, formula_name: &str, dry_run: bool, cask: bool) -> Result<()> {
    let start = std::time::Instant::now();

    println!("wax remove v{}\n", env!("CARGO_PKG_VERSION"));

    if cask {
        return uninstall_cask(cache, formula_name, dry_run, start).await;
    }

    let state = InstallState::new()?;
    let installed_packages = state.load().await?;

    let package_opt = installed_packages.get(formula_name);

    if package_opt.is_none() {
        let cask_state = CaskState::new()?;
        let installed_casks = cask_state.load().await?;

        if installed_casks.contains_key(formula_name) {
            return uninstall_cask(cache, formula_name, dry_run, start).await;
        }

        return Err(WaxError::NotInstalled(formula_name.to_string()));
    }

    let package = package_opt.unwrap();

    let formulae = cache.load_formulae().await?;
    let dependents: Vec<String> = formulae
        .iter()
        .filter(|f| {
            if let Some(deps) = &f.dependencies {
                if deps.contains(&formula_name.to_string()) {
                    return installed_packages.contains_key(&f.name);
                }
            }
            false
        })
        .map(|f| f.name.clone())
        .collect();

    if !dependents.is_empty() {
        println!(
            "{} {} is a dependency of:",
            style("⚠").yellow().bold(),
            formula_name
        );
        for dep in &dependents {
            println!("  - {}", dep);
        }

        if !dry_run {
            let confirm = Confirm::new("Continue with uninstall?")
                .with_default(false)
                .prompt();

            match confirm {
                Ok(true) => {}
                Ok(false) => {
                    println!("Uninstall cancelled");
                    return Ok(());
                }
                Err(_) => return Ok(()),
            }
        }
    }

    if dry_run {
        println!("- {} (dry run)", formula_name);
        let elapsed = start.elapsed();
        println!("\n[{:.2}ms] (dry run)", elapsed.as_secs_f64() * 1000.0);
        return Ok(());
    }

    let install_mode = package.install_mode;
    let cellar = install_mode.cellar_path();
    remove_symlinks(formula_name, &package.version, &cellar, false, install_mode).await?;

    let formula_dir = cellar.join(formula_name);
    if formula_dir.exists() {
        tokio::fs::remove_dir_all(&formula_dir).await?;
    }

    state.remove(formula_name).await?;

    println!(
        "- {}@{}",
        style(formula_name).white(),
        style(&package.version).dim()
    );

    let elapsed = start.elapsed();
    println!(
        "\n1 package removed [{:.2}ms]",
        elapsed.as_secs_f64() * 1000.0
    );

    Ok(())
}

async fn uninstall_cask(
    _cache: &Cache,
    cask_name: &str,
    dry_run: bool,
    start: std::time::Instant,
) -> Result<()> {
    let state = CaskState::new()?;
    let installed_casks = state.load().await?;

    let cask = installed_casks
        .get(cask_name)
        .ok_or_else(|| WaxError::NotInstalled(cask_name.to_string()))?;

    if dry_run {
        println!("- {} (cask) (dry run)", cask_name);
        let elapsed = start.elapsed();
        println!("\n[{:.2}ms] (dry run)", elapsed.as_secs_f64() * 1000.0);
        return Ok(());
    }

    let artifact_type = cask.artifact_type.as_deref().unwrap_or("dmg");

    match artifact_type {
        "tar.gz" => {
            if let Some(binary_paths) = &cask.binary_paths {
                for binary_path in binary_paths {
                    let path = std::path::PathBuf::from(binary_path);
                    if path.exists() {
                        tokio::fs::remove_file(&path).await?;
                    }
                }
            }
        }
        "pkg" => {
            println!(
                "{} PKG uninstallation not fully supported - you may need to manually remove files",
                style("⚠").yellow().bold()
            );
        }
        _ => {
            #[cfg(target_os = "macos")]
            let app_path =
                std::path::PathBuf::from("/Applications").join(format!("{}.app", cask_name));

            #[cfg(not(target_os = "macos"))]
            return Err(WaxError::PlatformNotSupported(
                "Cask uninstallation is only supported on macOS".to_string(),
            ));

            if app_path.exists() {
                tokio::fs::remove_dir_all(&app_path).await?;
            }
        }
    }

    state.remove(cask_name).await?;

    println!(
        "- {}@{} {}",
        style(cask_name).white(),
        style(&cask.version).dim(),
        style("(cask)").dim()
    );

    let elapsed = start.elapsed();
    println!(
        "\n1 package removed [{:.2}ms]",
        elapsed.as_secs_f64() * 1000.0
    );

    Ok(())
}
