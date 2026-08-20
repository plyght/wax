//! Chocolatey community gallery: HTML search + `.nupkg` download (ZIP).
//! Only packages that ship portable `.exe` files under `tools/` without requiring
//! `chocolateyinstall.ps1` downloads are supported for wax-managed install.

use crate::error::{Result, WaxError};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[cfg(target_os = "windows")]
use crate::bottle::BottleDownloader;
#[cfg(target_os = "windows")]
use crate::package_spec::Ecosystem;
#[cfg(target_os = "windows")]
use crate::scoop;
#[cfg(target_os = "windows")]
use crate::windows_state::{self, WindowsPackageManifest};
#[cfg(target_os = "windows")]
use indicatif::{ProgressBar, ProgressStyle};
#[cfg(target_os = "windows")]
use tempfile::TempDir;
#[cfg(target_os = "windows")]
use tracing::debug;

static SEARCH_RE: OnceLock<Regex> = OnceLock::new();

/// Search chocolatey.org web UI; returns package ids (lowercase) matching the query.
pub async fn search_package_ids(query: &str, limit: usize) -> Result<Vec<String>> {
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    let url = reqwest::Url::parse_with_params(
        "https://community.chocolatey.org/packages",
        &[("q", query)],
    )
    .map_err(|e| WaxError::InvalidInput(format!("invalid chocolatey search URL: {e}")))?;
    let html = crate::http_client::default_client()
        .get(url)
        .send()
        .await?
        .text()
        .await?;
    let re = SEARCH_RE.get_or_init(|| {
        Regex::new(r##"href="/packages/([^"#?]+)"##).expect("Invalid regex in chocolatey search")
    });
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(&html) {
        let id = cap[1].to_string();
        if seen.insert(id.clone()) {
            out.push(id);
        }
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

#[cfg(target_os = "windows")]
pub async fn package_exists(id: &str) -> bool {
    let url = format!("https://community.chocolatey.org/api/v2/package/{}", id);
    // Chocolatey v2 feed returns 501 for HEAD; GET redirects to the .nupkg on success.
    match crate::http_client::default_client().get(&url).send().await {
        Ok(r) => r.status().is_success() || r.status().is_redirection(),
        Err(_) => false,
    }
}

/// Install latest `.nupkg` if it contains at least one `tools/*.exe` and no mandatory script-only layout.
#[cfg(not(target_os = "windows"))]
pub async fn install_portable_tools(_id: &str) -> Result<()> {
    Err(WaxError::PlatformNotSupported(
        "Chocolatey-backed portable install is only supported on Windows".into(),
    ))
}

/// Install latest `.nupkg` if it contains at least one `tools/*.exe` and no mandatory script-only layout.
#[cfg(target_os = "windows")]
pub async fn install_portable_tools(id: &str) -> Result<()> {
    let nupkg_url = format!("https://community.chocolatey.org/api/v2/package/{}", id);
    debug!("Chocolatey nupkg {}", nupkg_url);

    let tmp = TempDir::new()?;
    let nupkg_path = tmp.path().join("pkg.nupkg");

    download_nupkg(id, &nupkg_url, &nupkg_path).await?;

    let extract_root = tmp.path().join("nupkg");
    std::fs::create_dir_all(&extract_root)?;
    scoop::extract_zip_file(&nupkg_path, &extract_root)?;

    let (tools_dir, exes) = get_tools_and_exes(&extract_root)?;

    stage_and_link_tools(id, nupkg_url, &tools_dir, &exes)?;

    Ok(())
}

#[cfg(target_os = "windows")]
fn stage_and_link_tools(
    id: &str,
    nupkg_url: String,
    tools_dir: &Path,
    exes: &[PathBuf],
) -> Result<()> {
    let bin_dir = windows_state::wax_bin_dir()?;
    std::fs::create_dir_all(&bin_dir)?;

    let staging = windows_state::wax_windows_root()?
        .join("choco-apps")
        .join(id);
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    crate::ui::copy_dir_all(tools_dir, &staging)?;

    let mut copy_actions = Vec::new();
    for src in exes {
        let file_name = src
            .file_name()
            .ok_or_else(|| WaxError::InstallError("invalid exe path".into()))?;
        let dest = bin_dir.join(file_name);
        copy_actions.push((src.clone(), dest));
    }
    let bin_links: Vec<PathBuf> = copy_actions.iter().map(|(_, dest)| dest.clone()).collect();
    windows_state::validate_bin_links_available(Ecosystem::Chocolatey, id, &bin_links)?;

    for (src, dest) in copy_actions {
        if dest.exists() {
            let _ = std::fs::remove_file(&dest);
        }
        std::fs::copy(src, &dest)?;
    }

    let mut files = windows_state::collect_files(&staging)?;
    files.extend(bin_links.iter().cloned());
    WindowsPackageManifest::new(
        Ecosystem::Chocolatey,
        id,
        "latest",
        nupkg_url,
        staging,
        bin_links,
        files,
    )
    .save()?;

    println!(
        "Installed {} from Chocolatey .nupkg (tools/*.exe) — binaries under:\n  {}",
        id,
        bin_dir.display()
    );

    Ok(())
}

#[cfg(target_os = "windows")]
async fn download_nupkg(id: &str, nupkg_url: &str, nupkg_path: &Path) -> Result<()> {
    let dl = BottleDownloader::new();
    let size = dl.probe_size(nupkg_url).await;
    let conns =
        BottleDownloader::num_connections(size, BottleDownloader::MAX_CONNECTIONS_PER_DOWNLOAD);
    let pb = ProgressBar::new(0);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.cyan} {msg} [{bar:30.cyan/blue}] {bytes}/{total_bytes}")
            .unwrap()
            .progress_chars("=>-"),
    );
    pb.set_message(id.to_string());

    dl.download(nupkg_url, nupkg_path, Some(&pb), conns, None)
        .await?;
    pb.finish_and_clear();
    Ok(())
}

fn get_tools_and_exes(extract_root: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    let tools_dir = extract_root.join("tools");
    if !tools_dir.is_dir() {
        return Err(WaxError::InstallError(
            "Chocolatey package has no tools/ directory in .nupkg (wax cannot run install scripts)"
                .into(),
        ));
    }

    let mut exes: Vec<PathBuf> = Vec::new();
    collect_exe_files(&tools_dir, &mut exes, 0, 4)?;

    if exes.is_empty() {
        let script_only = tools_dir.join("chocolateyinstall.ps1").is_file();
        let msg = if script_only {
            "Chocolatey package only ships chocolateyinstall.ps1 (no portable tools/*.exe). Try scoop/ or winget/ for the same app, or install with choco.exe"
        } else {
            "No suitable portable .exe under tools/ (wax does not run Chocolatey install scripts)"
        };
        return Err(WaxError::InstallError(msg.into()));
    }

    Ok((tools_dir, exes))
}

fn collect_exe_files(dir: &Path, out: &mut Vec<PathBuf>, depth: u32, max_depth: u32) -> Result<()> {
    if depth > max_depth {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        let ty = entry.file_type()?;
        if ty.is_dir() {
            collect_exe_files(&p, out, depth + 1, max_depth)?;
        } else if p
            .extension()
            .map(|e| e.eq_ignore_ascii_case("exe"))
            .unwrap_or(false)
        {
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let nl = name.to_lowercase();
            if nl.contains("uninstall") || nl.contains("chocolatey") || nl.starts_with("unins") {
                continue;
            }
            out.push(p);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    #[test]
    fn missing_tools_dir_is_an_install_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let err = get_tools_and_exes(tmp.path()).unwrap_err();
        assert!(
            err.to_string().contains("no tools/ directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn script_only_package_reports_the_scoop_winget_hint() {
        let tmp = tempfile::TempDir::new().unwrap();
        touch(&tmp.path().join("tools").join("chocolateyinstall.ps1"));
        let err = get_tools_and_exes(tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("chocolateyinstall.ps1"), "unexpected: {msg}");
        assert!(msg.contains("scoop/"), "unexpected: {msg}");
    }

    #[test]
    fn tools_dir_without_exes_reports_no_portable_exe() {
        let tmp = tempfile::TempDir::new().unwrap();
        touch(&tmp.path().join("tools").join("readme.txt"));
        let err = get_tools_and_exes(tmp.path()).unwrap_err();
        assert!(
            err.to_string().contains("No suitable portable .exe"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn portable_exes_are_collected_recursively() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tools = tmp.path().join("tools");
        touch(&tools.join("app.exe"));
        touch(&tools.join("nested").join("helper.EXE"));
        let (dir, mut exes) = get_tools_and_exes(tmp.path()).unwrap();
        assert_eq!(dir, tools);
        exes.sort();
        let names: Vec<String> = exes
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["app.exe", "helper.EXE"]);
    }

    #[test]
    fn uninstaller_and_chocolatey_shims_are_skipped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tools = tmp.path().join("tools");
        touch(&tools.join("app.exe"));
        touch(&tools.join("Uninstall.exe"));
        touch(&tools.join("unins000.exe"));
        touch(&tools.join("chocolateyProxy.exe"));
        let (_, mut exes) = get_tools_and_exes(tmp.path()).unwrap();
        exes.sort();
        let names: Vec<String> = exes
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["app.exe"]);
    }

    #[test]
    fn recursion_stops_past_the_depth_limit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let deep = tmp.path().join("a").join("b").join("c");
        touch(&deep.join("too-deep.exe"));
        let mut out = Vec::new();
        collect_exe_files(tmp.path(), &mut out, 0, 1).unwrap();
        assert!(out.is_empty());

        let mut out = Vec::new();
        collect_exe_files(tmp.path(), &mut out, 0, 4).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn missing_directory_surfaces_an_io_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut out = Vec::new();
        assert!(collect_exe_files(&tmp.path().join("nope"), &mut out, 0, 4).is_err());
    }

    #[tokio::test]
    async fn blank_query_never_hits_the_network() {
        assert!(search_package_ids("", 10).await.unwrap().is_empty());
        assert!(search_package_ids("   ", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn portable_install_is_windows_only() {
        let err = install_portable_tools("git").await.unwrap_err();
        assert!(
            matches!(err, WaxError::PlatformNotSupported(_)),
            "unexpected error: {err}"
        );
    }
}
