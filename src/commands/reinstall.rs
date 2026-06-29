use crate::cache::Cache;
use crate::cask::{CaskState, InstalledCask};
use crate::commands::{install, uninstall};
use crate::error::{Result, WaxError};
use crate::install::{InstallMode, InstallState, InstalledPackage};
use crate::signal::{clear_active_multi, clear_current_op, set_active_multi, set_current_op};
use crate::ui::{PROGRESS_BAR_CHARS, PROGRESS_BAR_TEMPLATE, SPINNER_TICK_CHARS};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::time::Instant;

struct ReinstallSignalGuard;

impl Drop for ReinstallSignalGuard {
    fn drop(&mut self) {
        clear_current_op();
        clear_active_multi();
    }
}

fn resolve_packages<T, U>(
    packages: &[String],
    cask: bool,
    all: bool,
    installed: &HashMap<String, T>,
    installed_casks: &HashMap<String, U>,
) -> Result<Vec<String>> {
    let resolved: Vec<String> = if all {
        let mut names: Vec<String> = if cask {
            installed_casks.keys().cloned().collect()
        } else {
            let mut names: Vec<String> = installed.keys().cloned().collect();
            names.extend(installed_casks.keys().cloned());
            names
        };
        names.sort();
        names.dedup();
        names
    } else {
        if packages.is_empty() {
            return Err(WaxError::InvalidInput(
                "Specify package name(s) or use --all to reinstall everything".to_string(),
            ));
        }
        packages.to_vec()
    };
    Ok(resolved)
}

fn check_missing_packages<T, U>(
    resolved: &[String],
    cask: bool,
    installed: &HashMap<String, T>,
    installed_casks: &HashMap<String, U>,
) -> Result<()> {
    let missing: Vec<&str> = resolved
        .iter()
        .map(String::as_str)
        .filter(|name| {
            if cask {
                !installed_casks.contains_key(*name)
            } else {
                !installed.contains_key(*name) && !installed_casks.contains_key(*name)
            }
        })
        .collect();
    if !missing.is_empty() {
        return Err(WaxError::NotInstalled(missing.join(", ")));
    }
    Ok(())
}

async fn reinstall_package(
    cache: &Cache,
    name: &String,
    i: usize,
    total: usize,
    cask: bool,
    installed: &HashMap<String, InstalledPackage>,
    installed_casks: &HashMap<String, InstalledCask>,
    multi: &MultiProgress,
) -> Result<()> {
    // Determine if this package is a cask (either by explicit flag or by being in cask state)
    let is_cask = cask || installed_casks.contains_key(name.as_str());

    let install_mode = installed.get(name.as_str()).map(|p| p.install_mode);
    let (user_flag, global_flag) = match install_mode {
        Some(InstallMode::User) => (true, false),
        Some(InstallMode::Global) => (false, true),
        None => (false, false),
    };

    let prefix = if total > 1 {
        format!("[{}/{}] ", i + 1, total)
    } else {
        String::new()
    };

    // Spinner for uninstall phase (inserted above the overall bar)
    let spinner = multi.insert_from_back(1, ProgressBar::new_spinner());
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars(SPINNER_TICK_CHARS),
    );
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));
    let is_installed =
        installed.contains_key(name.as_str()) || installed_casks.contains_key(name.as_str());

    if is_installed {
        set_current_op(format!("removing {}", name));
        spinner.set_message(format!("{}removing {}...", prefix, style(name).magenta()));
        uninstall::uninstall_quiet(cache, name, is_cask).await?;
        spinner.finish_and_clear();
    } else {
        spinner.set_message(format!("{}installing {}...", prefix, style(name).magenta()));
        spinner.finish_and_clear();
    }

    let pkg_start = Instant::now();
    if is_cask {
        set_current_op(format!("installing {}", name));
        install::install_impl(
            cache,
            std::slice::from_ref(name),
            install::InstallArgs {
                dry_run: false,
                ask: false,
                cask: true,
                user: user_flag,
                global: global_flag,
                build_from_source: false,
                head: false,
                run_scripts: true,
                quiet: true,
                force_reinstall: true,
                external_pb: None,
            },
        )
        .await?;
    } else {
        // Formula reinstall keeps the outer package bar because the formula
        // install path renders into the provided progress bar.
        let pb = multi.insert_from_back(1, ProgressBar::new(0));
        pb.set_style(
            ProgressStyle::default_bar()
                .template(&format!("{}{}", prefix, PROGRESS_BAR_TEMPLATE))
                .unwrap()
                .progress_chars(PROGRESS_BAR_CHARS),
        );
        pb.set_message(style(name).magenta().to_string());

        set_current_op(format!("downloading {}", name));
        install::install_impl(
            cache,
            std::slice::from_ref(name),
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
                force_reinstall: true,
                external_pb: Some(&pb),
            },
        )
        .await?;
        pb.finish_and_clear();
    }
    println!(
        "{} {}{}@{}{}",
        style("✓").green().bold(),
        prefix,
        style(name).magenta(),
        style(
            installed
                .get(name.as_str())
                .map(|p| p.version.as_str())
                .unwrap_or("latest")
        )
        .dim(),
        style(crate::ui::elapsed_suffix(pkg_start.elapsed())).dim(),
    );
    Ok(())
}

pub async fn reinstall(cache: &Cache, packages: &[String], cask: bool, all: bool) -> Result<()> {
    let state = InstallState::new()?;
    state.sync_from_cellar().await.ok();
    let installed = state.load().await?;

    let cask_state = CaskState::new()?;
    let installed_casks = cask_state.load().await?;

    let resolved = resolve_packages(packages, cask, all, &installed, &installed_casks)?;
    check_missing_packages(&resolved, cask, &installed, &installed_casks)?;

    let total = resolved.len();
    let start = Instant::now();
    let multi = MultiProgress::new();
    set_active_multi(multi.clone());
    let _signal_guard = ReinstallSignalGuard;

    if total > 1 {
        println!("reinstalling {} packages\n", style(total).bold());
    }

    for (i, name) in resolved.iter().enumerate() {
        reinstall_package(
            cache,
            name,
            i,
            total,
            cask,
            &installed,
            &installed_casks,
            &multi,
        )
        .await?;
    }

    println!(
        "\n{} {} reinstalled{}",
        style(total).bold(),
        if total == 1 { "package" } else { "packages" },
        crate::ui::elapsed_suffix(start.elapsed())
    );

    Ok(())
}
