use crate::cache::Cache;
use crate::cask::CaskState;
use crate::error::{Result, WaxError};
use crate::install::{remove_symlinks, InstallState};
use crate::signal::{clear_current_op, set_current_op};
use crate::ui::{OVERALL_PROGRESS_TEMPLATE, PROGRESS_BAR_CHARS, SPINNER_TICK_CHARS};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use inquire::Confirm;
use std::time::Instant;

pub async fn uninstall(
    cache: &Cache,
    formulae: &[String],
    dry_run: bool,
    cask: bool,
    yes: bool,
    all: bool,
) -> Result<()> {
    let names: Vec<String> = if all {
        let state = InstallState::new()?;
        state.sync_from_cellar().await.ok();
        let installed = state.load().await?;
        let mut names: Vec<String> = installed.keys().cloned().collect();
        names.sort();
        names
    } else {
        if formulae.is_empty() {
            return Err(WaxError::InvalidInput(
                "Specify package name(s) or use --all to uninstall everything".to_string(),
            ));
        }
        for name in formulae {
            crate::error::validate_package_name(name)?;
        }
        formulae.to_vec()
    };

    let total = names.len();
    let start = Instant::now();

    let overall_pb = if total > 1 {
        println!("uninstalling {} packages\n", style(total).bold());
        let pb = ProgressBar::new(total as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template(OVERALL_PROGRESS_TEMPLATE)
                .unwrap()
                .progress_chars(PROGRESS_BAR_CHARS),
        );
        Some(pb)
    } else {
        None
    };

    for (i, name) in names.iter().enumerate() {
        let prefix = if total > 1 {
            format!("[{}/{}] ", i + 1, total)
        } else {
            String::new()
        };
        uninstall_impl(cache, name, dry_run, cask, yes, false, &prefix).await?;
        if let Some(ref pb) = overall_pb {
            pb.inc(1);
        }
    }

    if let Some(pb) = overall_pb {
        pb.finish_and_clear();
    }
    clear_current_op();

    if total > 1 && !dry_run {
        println!(
            "\n{} {} removed [{}ms]",
            style(total).bold(),
            if total == 1 { "package" } else { "packages" },
            start.elapsed().as_millis()
        );
    }

    Ok(())
}

pub async fn uninstall_quiet(cache: &Cache, formula_name: &str, cask: bool) -> Result<()> {
    uninstall_impl(cache, formula_name, false, cask, true, true, "").await
}

async fn uninstall_impl(
    cache: &Cache,
    formula_name: &str,
    dry_run: bool,
    cask: bool,
    yes: bool,
    quiet: bool,
    prefix: &str,
) -> Result<()> {
    let start = std::time::Instant::now();

    if cask {
        return uninstall_cask(cache, formula_name, dry_run, start, quiet).await;
    }

    let state = InstallState::new()?;
    let installed_packages = state.load().await?;

    let package = if let Some(pkg) = installed_packages.get(formula_name) {
        pkg.clone()
    } else {
        let cask_state = CaskState::new()?;
        let installed_casks = cask_state.load().await?;

        if installed_casks.contains_key(formula_name) {
            return uninstall_cask(cache, formula_name, dry_run, start, quiet).await;
        }

        state.sync_from_cellar().await?;
        let updated_packages = state.load().await?;

        updated_packages
            .get(formula_name)
            .cloned()
            .ok_or_else(|| WaxError::NotInstalled(formula_name.to_string()))?
    };

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

    if !dependents.is_empty() && !quiet {
        println!("{} is a dependency of:", style(formula_name).magenta());
        for dep in &dependents {
            println!("  - {}", dep);
        }

        if !dry_run && !yes {
            let confirm = Confirm::new("Continue with uninstall?")
                .with_default(false)
                .prompt();

            match confirm {
                Ok(true) => {}
                Ok(false) => {
                    println!("uninstall cancelled");
                    return Ok(());
                }
                Err(_) => return Ok(()),
            }
        }
    }

    uninstall_package_direct(formula_name, &package, state, dry_run, start, quiet, prefix).await
}

async fn uninstall_package_direct(
    formula_name: &str,
    package: &crate::install::InstalledPackage,
    state: InstallState,
    dry_run: bool,
    start: std::time::Instant,
    quiet: bool,
    prefix: &str,
) -> Result<()> {
    if dry_run {
        if !quiet {
            println!(
                "{}would remove {}@{}",
                prefix,
                style(formula_name).magenta(),
                style(&package.version).dim()
            );
        }
        return Ok(());
    }

    set_current_op(format!("removing {}", formula_name));

    let spinner = if !quiet {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.red} {msg}")
                .unwrap()
                .tick_chars(SPINNER_TICK_CHARS),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(80));
        pb.set_message(format!(
            "{}removing {}@{}...",
            prefix,
            style(formula_name).magenta(),
            style(&package.version).dim()
        ));
        Some(pb)
    } else {
        None
    };

    let install_mode = package.install_mode;
    let cellar = install_mode.cellar_path()?;

    remove_symlinks(
        formula_name,
        &package.version,
        &cellar,
        false, /* dry_run */
        install_mode,
    )
    .await?;

    let formula_dir = cellar.join(formula_name);
    if formula_dir.exists() {
        tokio::fs::remove_dir_all(&formula_dir).await?;
    }

    state.remove(formula_name).await?;

    if let Some(pb) = spinner {
        pb.finish_and_clear();
    }

    if !quiet {
        println!(
            "{} {}{}{} {}",
            style("✗").red().bold(),
            prefix,
            style(formula_name).magenta(),
            style(format!("@{}", package.version)).dim(),
            style(format!("[{}ms]", start.elapsed().as_millis())).dim(),
        );
    }

    Ok(())
}

async fn uninstall_cask(
    _cache: &Cache,
    cask_name: &str,
    dry_run: bool,
    start: std::time::Instant,
    quiet: bool,
) -> Result<()> {
    let state = CaskState::new()?;
    let installed_casks = state.load().await?;

    let cask = installed_casks
        .get(cask_name)
        .ok_or_else(|| WaxError::NotInstalled(cask_name.to_string()))?;

    if dry_run {
        if !quiet {
            println!("- {} (cask)", cask_name);
            let elapsed = start.elapsed();
            println!("\ndry run - no changes made [{}ms]", elapsed.as_millis());
        }
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
            if !quiet {
                println!(
                    "PKG uninstallation not fully supported - you may need to manually remove files"
                );
            }
        }
        _ => {
            #[cfg(target_os = "macos")]
            {
                let app_name = cask
                    .app_name
                    .clone()
                    .unwrap_or_else(|| format!("{}.app", cask_name));
                let app_path = std::path::PathBuf::from("/Applications").join(&app_name);

                if app_path.exists() {
                    tokio::fs::remove_dir_all(&app_path).await?;
                }
            }

            #[cfg(not(target_os = "macos"))]
            {
                return Err(WaxError::PlatformNotSupported(
                    "Cask uninstallation is only supported on macOS".to_string(),
                ));
            }
        }
    }

    state.remove(cask_name).await?;

    if !quiet {
        println!(
            "{} {}{}  {}",
            style("✗").red().bold(),
            style(cask_name).magenta(),
            style(format!("@{} (cask)", cask.version)).dim(),
            style(format!("[{}ms]", start.elapsed().as_millis())).dim(),
        );
    }

    Ok(())
}
