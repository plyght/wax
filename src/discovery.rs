//! Best-effort package discovery for items installed outside Wax.
//!
//! Wax keeps its own install state, but users can also install software
//! manually or through other package managers. These helpers scan platform-
//! specific locations and merge any matches back into Wax’s installed-package
//! view so lockfiles, sync, and status commands stay accurate.

use crate::api::{Cask, Formula};
#[cfg_attr(not(target_os = "linux"), allow(unused_imports))]
use crate::bottle::detect_platform;
use crate::cask::InstalledCask;
use crate::error::Result;
#[cfg_attr(not(target_os = "linux"), allow(unused_imports))]
use crate::install::{InstallMode, InstalledPackage};
use crate::ui::dirs;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tracing::{debug, info};

#[allow(dead_code)]
pub async fn discover_manually_installed_casks(
    casks: &[Cask],
) -> Result<HashMap<String, InstalledCask>> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = casks;
        Ok(HashMap::new())
    }

    #[cfg(target_os = "macos")]
    {
        // Match application bundles against every known cask token/name alias.
        let token_index = build_cask_token_index(casks);
        let mut discovered = HashMap::new();

        // Scan the standard application roots so manually installed apps are
        // visible to Wax even when they were not installed through brew.
        for root in macos_application_roots() {
            if !root.exists() {
                continue;
            }

            let mut entries = match tokio::fs::read_dir(&root).await {
                Ok(entries) => entries,
                Err(err) => {
                    debug!("Skipping {:?}: {}", root, err);
                    continue;
                }
            };

            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let file_name = entry.file_name().to_string_lossy().to_string();

                if !path.is_dir() && !path.is_symlink() {
                    continue;
                }
                if !file_name.ends_with(".app") {
                    continue;
                }
                if file_name.starts_with('.') {
                    continue;
                }

                let bundle_name = read_app_bundle_name(&path)
                    .await
                    .unwrap_or_else(|| file_name.trim_end_matches(".app").to_string());

                let token = resolve_cask_token(&token_index, &bundle_name)
                    .or_else(|| resolve_cask_token(&token_index, &file_name));

                let Some(token) = token else {
                    continue;
                };

                let version = read_app_bundle_version(&path)
                    .await
                    .unwrap_or_else(|| "unknown".to_string());
                let install_date = entry
                    .metadata()
                    .await
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(system_time_to_unix_seconds)
                    .unwrap_or_else(unix_seconds_now);

                discovered
                    .entry(token.clone())
                    .or_insert_with(|| InstalledCask {
                        name: token,
                        version,
                        install_date,
                        artifact_type: Some("app".to_string()),
                        binary_paths: None,
                        app_name: Some(bundle_name),
                    });
            }
        }

        if !discovered.is_empty() {
            info!(
                "Discovered {} cask(s) from manual installs in application roots",
                discovered.len()
            );
        }

        Ok(discovered)
    }
}

#[allow(dead_code)]
#[allow(clippy::needless_return)]
pub async fn discover_linux_system_packages(
    formulae: &[Formula],
) -> Result<HashMap<String, InstalledPackage>> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = formulae;
        return Ok(HashMap::new());
    }

    #[cfg(target_os = "linux")]
    {
        // Normalize package-manager names so dpkg/rpm entries can be matched
        // back to the canonical Homebrew formula name.
        let token_index = build_formula_token_index(formulae);
        let mut discovered = HashMap::new();

        for (name, version) in read_linux_package_inventory().await? {
            let Some(formula_name) = token_index.get(&normalize_package_token(&name)).cloned()
            else {
                continue;
            };

            discovered
                .entry(formula_name.clone())
                .or_insert_with(|| InstalledPackage {
                    name: formula_name,
                    version,
                    platform: detect_platform(),
                    install_date: unix_seconds_now(),
                    install_mode: InstallMode::Global,
                    from_source: false,
                    bottle_rebuild: 0,
                    bottle_sha256: None,
                    pinned: false,
                });
        }

        if !discovered.is_empty() {
            info!(
                "Discovered {} Linux package(s) from dpkg/rpm inventories",
                discovered.len()
            );
        }

        Ok(discovered)
    }
}

#[allow(dead_code)]
fn build_cask_token_index(casks: &[Cask]) -> HashMap<String, String> {
    let mut index = HashMap::new();

    for cask in casks {
        for alias in cask_tokens(cask) {
            index
                .entry(normalize_package_token(&alias))
                .or_insert_with(|| cask.token.clone());
        }
    }

    index
}

#[allow(dead_code)]
fn build_formula_token_index(formulae: &[Formula]) -> HashMap<String, String> {
    let mut index = HashMap::new();

    for formula in formulae {
        index
            .entry(normalize_package_token(&formula.name))
            .or_insert_with(|| formula.name.clone());
        index
            .entry(normalize_package_token(&formula.full_name))
            .or_insert_with(|| formula.name.clone());
    }

    index
}

#[allow(dead_code)]
fn cask_tokens(cask: &Cask) -> Vec<String> {
    let mut aliases = vec![cask.token.clone(), cask.full_token.clone()];
    aliases.extend(cask.name.clone());
    aliases
}

#[allow(dead_code)]
fn resolve_cask_token(token_index: &HashMap<String, String>, value: &str) -> Option<String> {
    let normalized = normalize_package_token(value);
    if let Some(token) = token_index.get(&normalized) {
        return Some(token.clone());
    }

    let stripped = value.trim_end_matches(".app");
    let normalized_stripped = normalize_package_token(stripped);
    token_index.get(&normalized_stripped).cloned()
}

#[allow(dead_code)]
fn normalize_package_token(value: &str) -> String {
    let value = value
        .replace(".app", "")
        .replace("_", "-")
        .replace('/', "-")
        .to_lowercase();

    let mut out = String::new();
    let mut prev_dash = false;

    for ch in value.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch)
        } else {
            Some('-')
        };

        if let Some(mapped) = mapped {
            if mapped == '-' {
                if !prev_dash && !out.is_empty() {
                    out.push(mapped);
                }
                prev_dash = true;
            } else {
                out.push(mapped);
                prev_dash = false;
            }
        }
    }

    out.trim_matches('-').to_string()
}

fn macos_application_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Ok(home) = dirs::home_dir() {
        roots.push(home.join("Applications"));
    }
    roots
}

#[allow(dead_code)]
async fn read_app_bundle_name(path: &Path) -> Option<String> {
    if let Some(name) = read_info_plist_string(path, "CFBundleDisplayName").await {
        return Some(name);
    }
    if let Some(name) = read_info_plist_string(path, "CFBundleName").await {
        return Some(name);
    }

    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

#[allow(dead_code)]
async fn read_app_bundle_version(path: &Path) -> Option<String> {
    if let Some(version) = read_info_plist_string(path, "CFBundleShortVersionString").await {
        Some(version)
    } else {
        read_info_plist_string(path, "CFBundleVersion").await
    }
}

async fn read_info_plist_string(path: &Path, key: &str) -> Option<String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        let _ = key;
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        let plist = path.join("Contents/Info.plist");
        if !plist.exists() {
            return None;
        }

        let output = Command::new("plutil")
            .arg("-extract")
            .arg(key)
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
}

#[allow(dead_code)]
async fn read_linux_package_inventory() -> Result<Vec<(String, String)>> {
    let mut inventories = Vec::new();

    if let Some(pkgs) = query_dpkg_inventory().await? {
        inventories.extend(pkgs);
    }

    if let Some(pkgs) = query_pacman_inventory().await? {
        inventories.extend(pkgs);
    }

    if let Some(pkgs) = query_apk_inventory().await? {
        inventories.extend(pkgs);
    }

    if let Some(pkgs) = query_rpm_inventory().await? {
        inventories.extend(pkgs);
    }

    Ok(inventories)
}

#[allow(dead_code)]
async fn query_dpkg_inventory() -> Result<Option<Vec<(String, String)>>> {
    let output = Command::new("dpkg-query")
        .arg("-W")
        .arg("-f=${binary:Package}\t${Version}\n")
        .output()
        .await;

    let Ok(output) = output else {
        return Ok(None);
    };

    if !output.status.success() {
        return Ok(None);
    }

    Ok(Some(parse_tab_inventory_lines(&output.stdout, true)))
}

#[allow(dead_code)]
async fn query_pacman_inventory() -> Result<Option<Vec<(String, String)>>> {
    let output = Command::new("pacman").arg("-Q").output().await;

    let Ok(output) = output else {
        return Ok(None);
    };

    if !output.status.success() {
        return Ok(None);
    }

    Ok(Some(parse_space_inventory_lines(&output.stdout, false)))
}

#[allow(dead_code)]
async fn query_apk_inventory() -> Result<Option<Vec<(String, String)>>> {
    let names_output = Command::new("apk").arg("info").arg("-e").output().await;

    let Ok(names_output) = names_output else {
        return Ok(None);
    };

    if !names_output.status.success() {
        return Ok(None);
    }

    let package_names = parse_line_list(&names_output.stdout);
    if package_names.is_empty() {
        return Ok(None);
    }

    let details_output = Command::new("apk").arg("info").arg("-v").output().await;

    let Ok(details_output) = details_output else {
        return Ok(None);
    };

    if !details_output.status.success() {
        return Ok(None);
    }

    Ok(Some(parse_apk_inventory_lines(
        &details_output.stdout,
        &package_names,
    )))
}

#[allow(dead_code)]
async fn query_rpm_inventory() -> Result<Option<Vec<(String, String)>>> {
    let output = Command::new("rpm")
        .arg("-qa")
        .arg("--qf")
        .arg("%{NAME}\t%{VERSION}-%{RELEASE}\n")
        .output()
        .await;

    let Ok(output) = output else {
        return Ok(None);
    };

    if !output.status.success() {
        return Ok(None);
    }

    Ok(Some(parse_tab_inventory_lines(&output.stdout, false)))
}

#[allow(dead_code)]
fn parse_line_list(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect()
}

#[allow(dead_code)]
fn parse_space_inventory_lines(stdout: &[u8], strip_arch_suffix: bool) -> Vec<(String, String)> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next()?;
            let version = parts.next()?;
            let name = if strip_arch_suffix {
                name.split_once(':').map(|(base, _)| base).unwrap_or(name)
            } else {
                name
            };
            if name.is_empty() || version.is_empty() {
                None
            } else {
                Some((name.to_string(), version.to_string()))
            }
        })
        .collect()
}

#[allow(dead_code)]
fn parse_tab_inventory_lines(stdout: &[u8], strip_arch_suffix: bool) -> Vec<(String, String)> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| {
            let (name, version) = line.split_once('\t')?;
            let name = if strip_arch_suffix {
                name.split_once(':').map(|(base, _)| base).unwrap_or(name)
            } else {
                name
            };
            let name = name.trim();
            let version = version.trim();
            if name.is_empty() || version.is_empty() {
                None
            } else {
                Some((name.to_string(), version.to_string()))
            }
        })
        .collect()
}

#[allow(dead_code)]
fn parse_apk_inventory_lines(stdout: &[u8], package_names: &[String]) -> Vec<(String, String)> {
    let mut names = package_names.to_vec();
    names.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }

            let package_name = names.iter().find(|name| {
                line.starts_with(name.as_str()) && line.as_bytes().get(name.len()) == Some(&b'-')
            })?;

            let version = line[package_name.len() + 1..]
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim();

            if version.is_empty() {
                None
            } else {
                Some((package_name.clone(), version.to_string()))
            }
        })
        .collect()
}
fn system_time_to_unix_seconds(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() as i64)
}

fn unix_seconds_now() -> i64 {
    system_time_to_unix_seconds(SystemTime::now()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_common_app_names() {
        assert_eq!(
            normalize_package_token("Google Chrome.app"),
            "google-chrome"
        );
        assert_eq!(
            normalize_package_token("Visual Studio Code"),
            "visual-studio-code"
        );
        assert_eq!(normalize_package_token("Docker Desktop"), "docker-desktop");
    }

    #[test]
    fn matches_cask_aliases() {
        let cask = Cask {
            token: "google-chrome".to_string(),
            full_token: "homebrew/cask/google-chrome".to_string(),
            name: vec!["Google Chrome".to_string()],
            desc: None,
            homepage: "https://www.google.com/chrome/".to_string(),
            version: "1.0".to_string(),
            deprecated: false,
            disabled: false,
        };
        let index = build_cask_token_index(&[cask]);
        assert_eq!(
            resolve_cask_token(&index, "Google Chrome.app"),
            Some("google-chrome".to_string())
        );
        assert_eq!(
            resolve_cask_token(&index, "Google Chrome"),
            Some("google-chrome".to_string())
        );
    }

    #[test]
    fn parses_tab_inventory_lines() {
        let input = b"vim	2:9.1.0000-1
chromium:amd64	125.0.6422.141-1
";
        let parsed = parse_tab_inventory_lines(input, true);
        assert_eq!(parsed[0], ("vim".to_string(), "2:9.1.0000-1".to_string()));
        assert_eq!(
            parsed[1],
            ("chromium".to_string(), "125.0.6422.141-1".to_string())
        );
    }

    #[test]
    fn parses_space_inventory_lines() {
        let input = b"pacman 6.1.0-3
pacman:amd64 6.1.0-3
";
        let parsed = parse_space_inventory_lines(input, true);
        assert_eq!(parsed[0], ("pacman".to_string(), "6.1.0-3".to_string()));
        assert_eq!(parsed[1], ("pacman".to_string(), "6.1.0-3".to_string()));
    }

    #[test]
    fn parses_apk_inventory_lines_with_longest_prefix_match() {
        let names = vec![
            "foo".to_string(),
            "foo-bar".to_string(),
            "busybox".to_string(),
        ];
        let input = b"foo-bar-1.2.3-r0 BusyBox package
busybox-1.36.1-r2 busybox utilities
";
        let parsed = parse_apk_inventory_lines(input, &names);
        assert_eq!(parsed[0], ("foo-bar".to_string(), "1.2.3-r0".to_string()));
        assert_eq!(parsed[1], ("busybox".to_string(), "1.36.1-r2".to_string()));
    }
}
