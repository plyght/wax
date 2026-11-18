//! Best-effort package discovery for items installed outside Wax.
//!
//! Wax keeps its own install state, but users can also install software
//! manually or through other package managers. These helpers scan platform-
//! specific locations and merge any matches back into Wax’s installed-package
//! view so lockfiles, sync, and status commands stay accurate.

#[cfg(target_os = "linux")]
use crate::api::Formula;
#[cfg_attr(not(target_os = "linux"), allow(unused_imports))]
use crate::bottle::detect_platform;
#[cfg(target_os = "linux")]
use crate::error::Result;
#[cfg_attr(not(target_os = "linux"), allow(unused_imports))]
use crate::install::{InstallMode, InstalledPackage};
#[cfg(target_os = "macos")]
use crate::ui::dirs;
#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg_attr(target_os = "windows", allow(unused_imports))]
use tokio::process::Command;
#[cfg(target_os = "linux")]
use tracing::info;

#[cfg(target_os = "linux")]
pub async fn discover_linux_system_packages(
    formulae: &[Formula],
) -> Result<HashMap<String, InstalledPackage>> {
    // Normalize package-manager names so dpkg/rpm entries can be matched
    // back to the canonical Homebrew formula name.
    let token_index = build_formula_token_index(formulae);
    let mut discovered = HashMap::new();

    for (name, version) in read_linux_package_inventory().await? {
        let Some(formula_name) = token_index.get(&normalize_package_token(&name)).cloned() else {
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

#[cfg(target_os = "linux")]
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

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn normalize_package_token(value: &str) -> String {
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

#[cfg(target_os = "macos")]
pub(crate) fn macos_application_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Ok(home) = dirs::home_dir() {
        roots.push(home.join("Applications"));
    }
    roots
}

#[cfg(target_os = "macos")]
pub(crate) async fn read_info_plist_string(path: &Path, key: &str) -> Option<String> {
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn parse_line_list(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect()
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn system_time_to_unix_seconds(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() as i64)
}

#[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    fn parses_space_inventory_lines() {
        let input = b"pacman 6.1.0-3
pacman:amd64 6.1.0-3
";
        let parsed = parse_space_inventory_lines(input, true);
        assert_eq!(parsed[0], ("pacman".to_string(), "6.1.0-3".to_string()));
        assert_eq!(parsed[1], ("pacman".to_string(), "6.1.0-3".to_string()));
    }

    #[test]
    #[cfg(target_os = "linux")]
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
