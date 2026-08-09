use crate::error::{Result, WaxError};
use crate::ui::create_spinner;
use crate::version::WAX_VERSION as CURRENT_VERSION;
use console::style;
use inquire::Confirm;
use std::io::IsTerminal;
use tracing::{info, instrument};

const GITHUB_REPO_URL: &str = "https://github.com/plyght/wax";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Channel {
    Stable,
    Nightly,
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Channel::Stable => write!(f, "stable"),
            Channel::Nightly => write!(f, "nightly"),
        }
    }
}

fn parse_version(version: &str) -> Option<(u32, u32, u32)> {
    let v = version.trim_start_matches('v');
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() >= 3 {
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        let patch = parts[2].split('-').next()?.parse().ok()?;
        Some((major, minor, patch))
    } else {
        None
    }
}

fn is_newer(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

async fn fetch_latest_crate_version(client: &reqwest::Client) -> Result<String> {
    let resp = client
        .get("https://crates.io/api/v1/crates/waxpkg")
        .header("User-Agent", "wax-self-update")
        .send()
        .await
        .map_err(|e| WaxError::SelfUpdateError(format!("crates.io API request failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(WaxError::SelfUpdateError(format!(
            "crates.io API returned {}",
            resp.status()
        )));
    }

    #[derive(serde::Deserialize)]
    struct CrateInfo {
        #[serde(rename = "crate")]
        krate: CrateVersion,
    }

    #[derive(serde::Deserialize)]
    struct CrateVersion {
        max_stable_version: String,
    }

    let info: CrateInfo = resp.json().await.map_err(|e| {
        WaxError::SelfUpdateError(format!("Failed to parse crates.io API response: {e}"))
    })?;

    Ok(info.krate.max_stable_version)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InstallMethod {
    Homebrew,
    Wax,
    Script,
    Cargo,
    Custom,
}

pub async fn detect_install_method() -> InstallMethod {
    let exe_path = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => return InstallMethod::Custom,
    };

    let path_str = exe_path.to_string_lossy();

    // Check if it's installed inside a Cellar (Homebrew or Wax cellar)
    if path_str.contains("/Cellar/wax/") || path_str.contains("/Cellar/waxpkg/") {
        if let Ok(state) = crate::install::InstallState::new() {
            if let Ok(installed) = state.load().await {
                if installed.contains_key("wax") || installed.contains_key("waxpkg") {
                    return InstallMethod::Wax;
                }
            }
        }
        return InstallMethod::Homebrew;
    }

    // Check if it's inside ~/.local/bin/
    if let Ok(home) = crate::ui::dirs::home_dir() {
        let local_bin = home.join(".local").join("bin");
        if exe_path.starts_with(&local_bin) {
            return InstallMethod::Script;
        }
    }

    // Check if it's inside ~/.cargo/bin/
    if let Ok(home) = crate::ui::dirs::home_dir() {
        let cargo_bin = home.join(".cargo").join("bin");
        if exe_path.starts_with(&cargo_bin) {
            return InstallMethod::Cargo;
        }
    }

    InstallMethod::Custom
}

async fn update_from_brew() -> Result<()> {
    println!(
        "  {} {}",
        style("detected:").dim(),
        style("Homebrew installation").cyan()
    );
    println!(
        "  {} running {} (live output below)",
        style("upgrade:").dim(),
        style("brew upgrade wax").yellow()
    );

    let status = std::process::Command::new("brew")
        .args(["upgrade", "wax"])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to run brew: {e}")))?;

    if !status.success() {
        return Err(WaxError::SelfUpdateError("brew upgrade failed".to_string()));
    }

    Ok(())
}

async fn update_from_wax() -> Result<()> {
    println!(
        "  {} {}",
        style("detected:").dim(),
        style("Wax installation").cyan()
    );
    println!(
        "  {} running {} (live output below)",
        style("upgrade:").dim(),
        style("wax install wax --force").yellow()
    );

    let exe_path = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("wax"));

    let status = std::process::Command::new(exe_path)
        .args(["install", "wax", "--force"])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to run wax install: {e}")))?;

    if !status.success() {
        return Err(WaxError::SelfUpdateError("wax install failed".to_string()));
    }

    Ok(())
}

#[derive(serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

async fn fetch_release_metadata(client: &reqwest::Client) -> Result<Release> {
    let spinner = create_spinner("Checking for updates…");

    let resp = client
        .get("https://api.github.com/repos/plyght/wax/releases/latest")
        .header("User-Agent", "wax-self-update")
        .send()
        .await
        .map_err(|e| WaxError::SelfUpdateError(format!("GitHub API request failed: {e}")))?;

    if !resp.status().is_success() {
        spinner.finish_and_clear();
        return Err(WaxError::SelfUpdateError(format!(
            "GitHub API returned {}",
            resp.status()
        )));
    }

    let release: Release = resp.json().await.map_err(|e| {
        spinner.finish_and_clear();
        WaxError::SelfUpdateError(format!("Failed to parse GitHub API response: {e}"))
    })?;

    spinner.finish_and_clear();

    Ok(release)
}

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

async fn download_asset(client: &reqwest::Client, asset: &Asset) -> Result<Vec<u8>> {
    println!(
        "  {} downloading {}...",
        style("download:").dim(),
        style(&asset.name).yellow()
    );

    let download_resp = client
        .get(&asset.browser_download_url)
        .send()
        .await
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to download asset: {e}")))?;

    if !download_resp.status().is_success() {
        return Err(WaxError::SelfUpdateError(format!(
            "Failed to download asset: HTTP {}",
            download_resp.status()
        )));
    }

    let bytes = download_resp
        .bytes()
        .await
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to read asset bytes: {e}")))?;

    Ok(bytes.to_vec())
}

fn install_binary(bytes: &[u8]) -> Result<()> {
    let current_exe = std::env::current_exe().map_err(|e| {
        WaxError::SelfUpdateError(format!("Failed to resolve current exe path: {e}"))
    })?;

    let exe_dir = current_exe.parent().ok_or_else(|| {
        WaxError::SelfUpdateError("Current exe has no parent directory".to_string())
    })?;

    let temp_exe = exe_dir.join(".wax-update-tmp");

    std::fs::write(&temp_exe, bytes)
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to write temporary binary: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp_exe, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| WaxError::SelfUpdateError(format!("Failed to set permissions: {e}")))?;
    }

    std::fs::rename(&temp_exe, &current_exe)
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to overwrite executable: {e}")))?;

    Ok(())
}

async fn update_from_releases(force: bool) -> Result<()> {
    println!(
        "  {} {}",
        style("detected:").dim(),
        style("Script / binary release installation").cyan()
    );

    let client = crate::http_client::api();
    let release = fetch_release_metadata(client).await?;

    let latest_version = release.tag_name.trim_start_matches('v').to_string();

    println!(
        "  {} {}",
        style("current:").dim(),
        style(CURRENT_VERSION).cyan()
    );
    println!(
        "  {} {}",
        style("latest: ").dim(),
        style(&latest_version).cyan()
    );

    if !is_newer(CURRENT_VERSION, &latest_version) && !force {
        println!("{} already up to date", style("✓").green());
        return Ok(());
    }

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let target_asset_name = match os {
        "macos" => "wax-macos-x64",
        "linux" => match arch {
            "aarch64" => "wax-linux-arm64",
            _ => "wax-linux-x64",
        },
        _ => {
            return Err(WaxError::SelfUpdateError(format!(
                "Unsupported platform for binary release update: {os}-{arch}"
            )))
        }
    };

    let asset = release
        .assets
        .iter()
        .find(|a| a.name == target_asset_name)
        .ok_or_else(|| {
            WaxError::SelfUpdateError(format!(
                "Could not find release asset '{}' for {}-{}",
                target_asset_name, os, arch
            ))
        })?;

    let bytes = download_asset(client, asset).await?;
    install_binary(&bytes)?;

    println!(
        "{} updated to {}",
        style("✓").green(),
        style(format!("v{latest_version}")).cyan()
    );

    Ok(())
}

#[instrument]
pub async fn self_update(
    channel: Channel,
    force: bool,
    nightly_cleanup: Option<bool>,
) -> Result<()> {
    info!(
        "Self-update initiated: channel={channel}, force={force}, nightly_cleanup={:?}",
        nightly_cleanup
    );

    match channel {
        Channel::Stable => {
            let method = detect_install_method().await;
            match method {
                InstallMethod::Homebrew => update_from_brew().await,
                InstallMethod::Wax => update_from_wax().await,
                InstallMethod::Script => update_from_releases(force).await,
                InstallMethod::Cargo | InstallMethod::Custom => update_from_crates(force).await,
            }
        }
        Channel::Nightly => update_from_source(force, nightly_cleanup).await,
    }
}

pub async fn available_stable_update() -> Result<Option<String>> {
    let client = crate::http_client::api();
    let latest_version = fetch_latest_crate_version(client).await?;

    if is_newer(CURRENT_VERSION, &latest_version) {
        Ok(Some(latest_version))
    } else {
        Ok(None)
    }
}

async fn update_from_crates(force: bool) -> Result<()> {
    let client = crate::http_client::api();
    let spinner = create_spinner("Checking for updates…");
    let latest_version = fetch_latest_crate_version(client).await?;
    spinner.finish_and_clear();

    println!(
        "  {} {}",
        style("current:").dim(),
        style(CURRENT_VERSION).cyan()
    );
    println!(
        "  {} {}",
        style("latest: ").dim(),
        style(&latest_version).cyan()
    );

    if !is_newer(CURRENT_VERSION, &latest_version) && !force {
        println!("{} already up to date", style("✓").green());
        println!(
            "  {} use {} to reinstall anyway",
            style("hint:").dim(),
            style("-f / --force").yellow()
        );
        return Ok(());
    }

    println!(
        "  {} running {} (live output below)",
        style("install:").dim(),
        style("cargo install waxpkg --bin wax --force").yellow()
    );

    let mut args = vec!["install", "waxpkg", "--bin", "wax", "--locked"];
    if force || is_newer(CURRENT_VERSION, &latest_version) {
        args.push("--force");
    }

    let status = std::process::Command::new("cargo")
        .args(&args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to run cargo: {e}")))?;

    if !status.success() {
        return Err(WaxError::SelfUpdateError(
            "cargo install failed".to_string(),
        ));
    }

    println!(
        "{} updated to {}",
        style("✓").green(),
        style(format!("v{latest_version}")).cyan()
    );

    Ok(())
}

async fn cleanup_nightly_artifacts() -> Result<usize> {
    let home = crate::ui::dirs::home_dir()?;
    let mut removed = 0usize;

    let roots = [
        home.join(".cargo/git/checkouts"),
        home.join(".cargo/git/db"),
    ];

    let mut join_set = tokio::task::JoinSet::new();

    for root in roots {
        if let Ok(mut entries) = tokio::fs::read_dir(&root).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();

                if name.starts_with("wax-") {
                    join_set.spawn(async move {
                        if let Ok(file_type) = entry.file_type().await {
                            if file_type.is_dir() && tokio::fs::remove_dir_all(&path).await.is_ok()
                            {
                                return 1usize;
                            }
                        }
                        0usize
                    });
                }
            }
        }
    }

    while let Some(res) = join_set.join_next().await {
        removed += res.unwrap_or(0);
    }

    Ok(removed)
}

fn should_cleanup_nightly(nightly_cleanup: Option<bool>) -> Result<bool> {
    match nightly_cleanup {
        Some(value) => Ok(value),
        None => {
            if !std::io::stdin().is_terminal() {
                println!(
                    "  {} use {} or {} to control nightly cache cleanup",
                    style("hint:").dim(),
                    style("--clean").yellow(),
                    style("--no-clean").yellow()
                );
                return Ok(false);
            }
            Confirm::new("Clean Cargo git cache for wax nightly sources?")
                .with_default(false)
                .prompt()
                .map_err(|e| WaxError::SelfUpdateError(format!("Failed to read prompt input: {e}")))
        }
    }
}

async fn update_from_source(force: bool, nightly_cleanup: Option<bool>) -> Result<()> {
    println!(
        "  {} {}",
        style("current:").dim(),
        style(CURRENT_VERSION).cyan()
    );
    println!(
        "  {} {}",
        style("channel:").dim(),
        style("nightly (GitHub HEAD)").yellow()
    );

    let mut args = vec!["install", "--git", GITHUB_REPO_URL, "--bin", "wax"];
    if force {
        args.push("--force");
    }

    println!(
        "  {} running {} (live output below)",
        style("build:").dim(),
        style(format!("cargo {}", args.join(" "))).yellow()
    );

    let status = std::process::Command::new("cargo")
        .args(&args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to run cargo: {e}")))?;

    if !status.success() {
        return Err(WaxError::SelfUpdateError(
            "cargo install failed".to_string(),
        ));
    }

    if should_cleanup_nightly(nightly_cleanup)? {
        let removed = cleanup_nightly_artifacts().await?;
        println!(
            "{} cleaned {} nightly cache entr{}",
            style("✓").green(),
            removed,
            if removed == 1 { "y" } else { "ies" }
        );
    }

    println!("{} installed nightly build from HEAD", style("✓").green());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_with_v_prefix() {
        assert_eq!(parse_version("v0.13.3"), Some((0, 13, 3)));
    }

    #[test]
    fn parse_version_without_prefix() {
        assert_eq!(parse_version("0.13.3"), Some((0, 13, 3)));
    }

    #[test]
    fn nightly_update_uses_release_repository() {
        assert_eq!(GITHUB_REPO_URL, "https://github.com/plyght/wax");
    }

    #[test]
    fn parse_version_prerelease_ignored() {
        assert_eq!(parse_version("1.2.3-beta.1"), Some((1, 2, 3)));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not-a-version"), None);
        assert_eq!(parse_version("1.2"), None);
    }

    #[test]
    fn is_newer_detects_upgrade() {
        assert!(is_newer("0.13.2", "0.13.3"));
        assert!(is_newer("0.12.9", "0.13.0"));
        assert!(is_newer("1.0.0", "2.0.0"));
    }

    #[test]
    fn is_newer_same_or_older() {
        assert!(!is_newer("0.13.3", "0.13.3"));
        assert!(!is_newer("0.13.3", "0.13.2"));
    }
}
