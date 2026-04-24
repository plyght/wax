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
        Channel::Stable => update_from_crates(force).await,
        Channel::Nightly => update_from_source(force, nightly_cleanup).await,
    }
}

pub async fn available_stable_update() -> Result<Option<String>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| WaxError::SelfUpdateError(format!("HTTP client error: {e}")))?;

    let latest_version = fetch_latest_crate_version(&client).await?;

    if is_newer(CURRENT_VERSION, &latest_version) {
        Ok(Some(latest_version))
    } else {
        Ok(None)
    }
}

async fn update_from_crates(force: bool) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| WaxError::SelfUpdateError(format!("HTTP client error: {e}")))?;

    let spinner = create_spinner("Checking for updates…");
    let latest_version = fetch_latest_crate_version(&client).await?;
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

    let mut args = vec!["install", "waxpkg", "--bin", "wax"];
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

fn cleanup_nightly_artifacts() -> Result<usize> {
    let home = crate::ui::dirs::home_dir()?;
    let mut removed = 0usize;

    let roots = [
        home.join(".cargo/git/checkouts"),
        home.join(".cargo/git/db"),
    ];
    for root in roots {
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("wax-") && path.is_dir() && std::fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
        }
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
        let removed = cleanup_nightly_artifacts()?;
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
