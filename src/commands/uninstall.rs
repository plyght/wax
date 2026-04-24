use crate::cache::Cache;
use crate::cask::CaskState;
use crate::discovery::discover_manually_installed_casks;
use crate::error::{Result, WaxError};
use crate::install::{remove_symlinks, InstallState};
use crate::lockfile::Lockfile;
use crate::signal::{clear_current_op, set_current_op};
use crate::ui::dirs;
use crate::ui::SPINNER_TICK_CHARS;
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use inquire::Confirm;
use std::path::Path;
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

async fn resolve_cask_app_name(
    cask_name: &str,
    version: &str,
    stored_app_name: Option<&str>,
) -> String {
    if let Some(name) = stored_app_name {
        let basename = std::path::Path::new(name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(name);
        return if basename.ends_with(".app") {
            basename.to_string()
        } else {
            format!("{}.app", basename)
        };
    }

    if let Some(app_name) = find_app_in_caskroom(cask_name, version) {
        return app_name;
    }

    if let Ok(details) = crate::api::ApiClient::new()
        .fetch_cask_details(cask_name)
        .await
    {
        if let Some(artifacts) = details.artifacts {
            for artifact in artifacts {
                if let crate::api::CaskArtifact::App { app } = artifact {
                    if let Some(source) = app.first().and_then(|v| v.as_str()) {
                        let basename = std::path::Path::new(source)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(source);
                        return if basename.ends_with(".app") {
                            basename.to_string()
                        } else {
                            format!("{}.app", basename)
                        };
                    }
                }
            }
        }
    }

    format!("{}.app", cask_name)
}

async fn uninstall_cask(
    cache: &Cache,
    cask_name: &str,
    dry_run: bool,
    start: std::time::Instant,
    quiet: bool,
) -> Result<()> {
    let state = CaskState::new()?;
    let mut installed_casks = state.load().await?;

    // If cask not found, try discovering manually installed apps
    if !installed_casks.contains_key(cask_name) {
        let casks = cache.load_casks().await?;
        if let Ok(discovered) = discover_manually_installed_casks(&casks).await {
            for (name, cask) in discovered {
                installed_casks.entry(name).or_insert(cask);
            }
        }
    }

    // Last resort: check /Applications for a matching .app bundle
    if !installed_casks.contains_key(cask_name) {
        let app_name = resolve_cask_app_name(cask_name, "unknown", None).await;
        let app_candidates = [
            std::path::PathBuf::from("/Applications").join(&app_name),
            dirs::home_dir()
                .map(|h| h.join("Applications").join(&app_name))
                .unwrap_or_default(),
        ];
        for app_path in app_candidates {
            if app_path.exists() {
                let version = read_app_version_from_plist(&app_path)
                    .await
                    .unwrap_or_else(|| "unknown".to_string());
                installed_casks.insert(
                    cask_name.to_string(),
                    crate::cask::InstalledCask {
                        name: cask_name.to_string(),
                        version,
                        install_date: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs() as i64,
                        artifact_type: Some("app".to_string()),
                        binary_paths: None,
                        app_name: Some(
                            app_path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                        ),
                    },
                );
                break;
            }
        }
    }

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
        "tar.gz" | "binary" => {
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
            let app_basename =
                resolve_cask_app_name(cask_name, &cask.version, cask.app_name.as_deref()).await;

            // On macOS: check /Applications, then ~/Applications.
            // On Linux: check ~/Applications only (no system /Applications).
            #[cfg(target_os = "macos")]
            let candidates: Vec<std::path::PathBuf> = vec![
                std::path::PathBuf::from("/Applications").join(&app_basename),
                dirs::home_dir()
                    .map(|h| h.join("Applications").join(&app_basename))
                    .unwrap_or_default(),
            ];
            #[cfg(not(target_os = "macos"))]
            let candidates: Vec<std::path::PathBuf> = vec![dirs::home_dir()
                .map(|h| h.join("Applications").join(&app_basename))
                .unwrap_or_default()];

            let mut removed = false;
            for app_path in &candidates {
                if app_path.exists() {
                    #[cfg(target_os = "macos")]
                    if tokio::fs::remove_dir_all(app_path).await.is_err() {
                        // Fall back to sudo for system-installed apps.
                        crate::sudo::sudo_remove(app_path)?;
                        removed = true;
                        break;
                    }
                    #[cfg(not(target_os = "macos"))]
                    tokio::fs::remove_dir_all(app_path).await?;
                    removed = true;
                    break;
                }
            }

            if !removed && !quiet {
                eprintln!(
                    "warning: could not find {} in Applications — \
                    you may need to remove it manually",
                    app_basename
                );
            }
        }
    }

    state.remove(cask_name).await?;

    let lockfile_path = Lockfile::default_path();
    if lockfile_path.exists() {
        if let Ok(mut lockfile) = Lockfile::load(&lockfile_path).await {
            lockfile.remove_cask(cask_name).await;
            let _ = lockfile.save(&lockfile_path).await;
        }
    }

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

fn find_app_in_caskroom(cask_name: &str, version: &str) -> Option<String> {
    let caskroom = CaskState::caskroom_dir();
    let version_dir = caskroom.join(cask_name).join(version);
    if !version_dir.exists() {
        return None;
    }

    if let Ok(entries) = std::fs::read_dir(&version_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("app") {
                return path.file_name().map(|n| n.to_string_lossy().into_owned());
            }
        }
    }
    None
}

async fn read_app_version_from_plist(path: &Path) -> Option<String> {
    let plist = path.join("Contents/Info.plist");
    if !plist.exists() {
        return None;
    }

    let output = tokio::process::Command::new("plutil")
        .arg("-extract")
        .arg("CFBundleShortVersionString")
        .arg("raw")
        .arg("-o")
        .arg("-")
        .arg(&plist)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_app_in_caskroom_nonexistent() {
        let result = find_app_in_caskroom("nonexistent", "1.0.0");
        assert_eq!(result, None);
    }
}
