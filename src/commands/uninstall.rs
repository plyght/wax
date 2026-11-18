#[cfg(not(target_os = "windows"))]
use crate::adopt::AdoptOptions;
use crate::adopt::{self, PackageKind};
use crate::cache::Cache;
use crate::cask::CaskState;
use crate::error::{Result, WaxError};
use crate::install::{remove_symlinks, InstallState};
use crate::lockfile::Lockfile;
use crate::signal::{clear_current_op, set_current_op};
use crate::ui::SPINNER_TICK_CHARS;
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use inquire::Confirm;
use std::path::Path;
use std::time::Instant;

#[cfg(target_os = "windows")]
use crate::windows_state::{self, WindowsPackageManifest};

static ZAP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_zap(enabled: bool) {
    ZAP.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub async fn uninstall(
    cache: &Cache,
    formulae: &[String],
    dry_run: bool,
    cask: bool,
    yes: bool,
    all: bool,
) -> Result<()> {
    let names: Vec<String> = if all {
        #[cfg(target_os = "windows")]
        {
            let mut names: Vec<String> = windows_state::list_manifests()?
                .into_iter()
                .map(|m| format!("{}/{}", m.ecosystem.label(), m.id))
                .collect();
            names.sort();
            names
        }
        #[cfg(not(target_os = "windows"))]
        {
            let snapshot =
                adopt::sync_installed_state(cache, AdoptOptions::formulae_only()).await?;
            let mut names: Vec<String> = snapshot.formulae.keys().cloned().collect();
            names.sort();
            names
        }
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

    if total > 1 {
        println!("uninstalling {} packages\n", style(total).bold());
    }

    for (i, name) in names.iter().enumerate() {
        let prefix = if total > 1 {
            format!("[{}/{}] ", i + 1, total)
        } else {
            String::new()
        };
        uninstall_impl(cache, name, dry_run, cask, yes, false, &prefix).await?;
    }
    clear_current_op();

    if total > 1 && !dry_run {
        println!(
            "\n{} {} removed{}",
            style(total).bold(),
            if total == 1 { "package" } else { "packages" },
            crate::ui::elapsed_suffix(start.elapsed())
        );
    }

    Ok(())
}

pub async fn uninstall_quiet(cache: &Cache, formula_name: &str, cask: bool) -> Result<()> {
    uninstall_impl(cache, formula_name, false, cask, true, true, "").await
}

#[cfg_attr(target_os = "windows", allow(unused_variables, unreachable_code))]
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

    #[cfg(target_os = "windows")]
    {
        if cask {
            return Err(crate::error::homebrew_unavailable());
        }
        if let Some(manifest) = windows_state::find_manifest(formula_name)? {
            return uninstall_windows_package(&manifest, dry_run, start, quiet, prefix).await;
        }
        return Err(WaxError::NotInstalled(formula_name.to_string()));
    }

    let resolved = adopt::resolve_package(cache, formula_name, cask).await?;
    if resolved.kind == PackageKind::Cask {
        return uninstall_cask(cache, &resolved.name, dry_run, start, quiet).await;
    }

    let package = resolved
        .formula
        .ok_or_else(|| WaxError::NotInstalled(formula_name.to_string()))?;
    let state = InstallState::new()?;
    let installed_packages = state.load().await?;

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

    uninstall_package_direct(
        &resolved.name,
        &package,
        state,
        dry_run,
        start,
        quiet,
        prefix,
    )
    .await
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

    if let Some(ref pb) = spinner {
        pb.set_message(format!(
            "{}removing {} {}",
            prefix,
            style(formula_name).magenta(),
            style("unlinking...").dim()
        ));
    }
    remove_symlinks(
        formula_name,
        &package.version,
        &cellar,
        false, /* dry_run */
        install_mode,
    )
    .await?;

    if let Some(ref pb) = spinner {
        pb.set_message(format!(
            "{}removing {} {}",
            prefix,
            style(formula_name).magenta(),
            style("deleting files...").dim()
        ));
    }
    let formula_dir = cellar.join(formula_name);
    if formula_dir.exists() {
        tokio::fs::remove_dir_all(&formula_dir).await.map_err(|e| {
            crate::error::WaxError::InstallError(format!(
                "Failed to remove formula directory {}: {}",
                formula_dir.display(),
                e
            ))
        })?;
    }

    state.remove(formula_name).await?;

    let lockfile_path = Lockfile::default_path();
    if lockfile_path.exists() {
        let _guard = crate::install::state_lock().lock().await;
        if let Ok(mut lockfile) = Lockfile::load(&lockfile_path).await {
            lockfile.remove_package(formula_name).await;
            let _ = lockfile.save(&lockfile_path).await;
        }
    }

    if let Some(pb) = spinner {
        pb.finish_and_clear();
    }

    if !quiet {
        println!(
            "{} {}{}{}{}",
            style("✗").red().bold(),
            prefix,
            style(formula_name).magenta(),
            style(format!("@{}", package.version)).dim(),
            style(crate::ui::elapsed_suffix(start.elapsed())).dim(),
        );
    }

    Ok(())
}

async fn run_uninstall_hook(details: &crate::api::CaskDetails, hook: &str, quiet: bool) {
    let declared = details.artifacts.iter().flatten().any(|artifact| {
        matches!(artifact, crate::api::CaskArtifact::Other(serde_json::Value::Object(map)) if map.contains_key(hook))
    });
    let (Some(rb_path), true) = (&details.rb_path, declared) else {
        return;
    };
    let Some(ruby) = crate::ruby_eval::find_ruby() else {
        return;
    };
    let staged = CaskState::caskroom_dir()
        .join(&details.token)
        .join(&details.version);
    match crate::ruby_eval::run_hook(&ruby, rb_path, hook, &staged).await {
        Ok(log) if !quiet => {
            for line in log.lines().filter(|l| !l.trim().is_empty()) {
                println!("  {line}");
            }
        }
        Ok(_) => {}
        Err(e) if !quiet => eprintln!("{} {}", style("warning:").yellow(), e),
        Err(_) => {}
    }
}

async fn uninstall_cask(
    cache: &Cache,
    cask_name: &str,
    dry_run: bool,
    start: std::time::Instant,
    quiet: bool,
) -> Result<()> {
    let state = CaskState::new()?;
    let installed_casks = state.load().await?;

    let short_name = adopt::short_package_name(cask_name);

    let cask = installed_casks
        .get(cask_name)
        .or_else(|| installed_casks.get(short_name))
        .cloned()
        .ok_or_else(|| WaxError::NotInstalled(cask_name.to_string()))?;

    if dry_run {
        if !quiet {
            println!("- {} (cask)", cask_name);
            let elapsed = start.elapsed();
            println!(
                "\ndry run - no changes made{}",
                crate::ui::elapsed_suffix(elapsed)
            );
        }
        return Ok(());
    }

    let details = match cache.load_all_casks().await {
        Ok(casks)
            if !casks
                .iter()
                .any(|c| c.token == cask_name || c.full_token == cask_name) =>
        {
            None
        }
        Ok(casks) => cache
            .fetch_cask_details_from_index(&casks, cask_name)
            .await
            .ok(),
        Err(_) => None,
    };
    let artifacts = details.as_ref().and_then(|d| d.artifacts.as_deref());
    #[cfg(target_os = "macos")]
    {
        if cask.apps.is_empty()
            && (cask.app_name.is_some()
                || cask.artifact_type.is_none()
                || cask.artifact_type.as_deref() == Some("app")
                || artifacts
                    .iter()
                    .flat_map(|a| a.iter())
                    .any(|a| matches!(a, crate::api::CaskArtifact::App { .. })))
        {
            return Err(WaxError::InvalidInput(format!("No verified app receipt for {cask_name}; use the original installer or remove it manually")));
        }
        for app in &cask.apps {
            if installed_casks.iter().any(|(other, record)| {
                other != cask_name
                    && (record.apps.iter().any(|r| r.path == app.path)
                        || record
                            .installed_paths
                            .iter()
                            .any(|p| Path::new(p) == app.path))
            }) {
                return Err(WaxError::InvalidInput(format!(
                    "Multiple casks claim {}; leaving it untouched",
                    app.path.display()
                )));
            }
            app.present().await?;
        }
    }
    if cask
        .installed_paths
        .iter()
        .any(|p| Path::new(p).extension().is_some_and(|ext| ext == "app"))
    {
        return Err(WaxError::InvalidInput(format!(
            "Unverified legacy app path for {cask_name}; remove it manually"
        )));
    }
    let directives = artifacts
        .map(crate::cask_uninstall::parse)
        .unwrap_or_default();
    let zap = artifacts
        .filter(|_| ZAP.load(std::sync::atomic::Ordering::Relaxed))
        .map(crate::cask_uninstall::parse_zap)
        .unwrap_or_default();
    if let Some(details) = &details {
        run_uninstall_hook(details, "uninstall_preflight", quiet).await;
    }
    if !directives.is_empty() || !zap.is_empty() {
        let warnings = tokio::task::spawn_blocking(move || {
            let mut warnings = crate::cask_uninstall::run(&directives);
            warnings.extend(crate::cask_uninstall::run(&zap));
            warnings
        })
        .await
        .unwrap_or_default();
        if !quiet {
            for warning in warnings {
                eprintln!("{} {}", style("warning:").yellow(), warning);
            }
        }
    }

    #[cfg(target_os = "macos")]
    for app in &cask.apps {
        if app.present().await? {
            tokio::fs::remove_dir_all(&app.path).await?;
        } else if !quiet {
            eprintln!(
                "{} no longer exists; not searching for another copy",
                app.path.display()
            );
        }
    }

    // Older binary-only receipts predate installed_paths.
    if matches!(cask.artifact_type.as_deref(), Some("tar.gz" | "binary")) {
        for path in cask.binary_paths.iter().flatten() {
            if Path::new(path).is_file() || Path::new(path).is_symlink() {
                tokio::fs::remove_file(path).await?;
            }
        }
    }

    // Remove all tracked installed paths
    if let Some(details) = &details {
        run_uninstall_hook(details, "uninstall_postflight", quiet).await;
    }
    for path_str in &cask.installed_paths {
        let path = Path::new(path_str);
        if path.exists() {
            let meta = tokio::fs::symlink_metadata(path).await;
            if let Ok(m) = meta {
                if m.is_dir() && !m.file_type().is_symlink() {
                    let _ = tokio::fs::remove_dir_all(path).await;
                } else {
                    let _ = tokio::fs::remove_file(path).await;
                }
            }
        }
    }

    let mut caskrooms = vec![CaskState::global_caskroom_dir()];
    caskrooms.extend(CaskState::user_caskroom_dir().ok());
    for caskroom in caskrooms {
        let rack = caskroom.join(short_name);
        let version_dir = rack.join(&cask.version);
        if version_dir.exists() {
            let _ = tokio::fs::remove_dir_all(&version_dir).await;
        }
        let only_metadata = std::fs::read_dir(&rack).is_ok_and(|entries| {
            entries
                .flatten()
                .all(|e| e.file_name().to_string_lossy() == ".metadata")
        });
        if only_metadata {
            let _ = tokio::fs::remove_dir_all(&rack).await;
        }
    }

    state.remove(cask_name).await?;

    let lockfile_path = Lockfile::default_path();
    if lockfile_path.exists() {
        let _guard = crate::install::state_lock().lock().await;
        if let Ok(mut lockfile) = Lockfile::load(&lockfile_path).await {
            lockfile.remove_cask(cask_name).await;
            let _ = lockfile.save(&lockfile_path).await;
        }
    }

    if !quiet {
        println!(
            "{} {}{}{}",
            style("✓").green().bold(),
            style(cask_name).magenta(),
            style(format!("@{} (cask)", cask.version)).dim(),
            style(crate::ui::elapsed_suffix(start.elapsed())).dim(),
        );
    }

    Ok(())
}

#[cfg(target_os = "windows")]
async fn uninstall_windows_package(
    manifest: &WindowsPackageManifest,
    dry_run: bool,
    _start: std::time::Instant,
    quiet: bool,
    prefix: &str,
) -> Result<()> {
    let qualified = format!("{}/{}", manifest.ecosystem.label(), manifest.id);

    if dry_run {
        if !quiet {
            println!(
                "{}would remove {}@{} (windows)",
                prefix,
                style(&qualified).magenta(),
                style(&manifest.version).dim()
            );
        }
        return Ok(());
    }

    // Run native uninstaller first if present
    if let Some(native) = &manifest.native_uninstall {
        if !quiet {
            println!(
                "{}uninstalling {} via native uninstaller...",
                prefix,
                style(&qualified).magenta()
            );
        }
        let _ = std::process::Command::new(&native.command)
            .args(&native.args)
            .status();
    }

    windows_state::remove_manifest(manifest, false)?;

    if !quiet {
        println!(
            "{} {}{}{}",
            style("✗").red().bold(),
            prefix,
            style(&qualified).magenta(),
            style(format!("@{} (windows)", manifest.version)).dim(),
        );
    }

    Ok(())
}
