use crate::error::{Result, WaxError};
use crate::ui::create_spinner;
use console::style;
use serde::Deserialize;
use std::process::Command;
use tracing::{info, instrument};

const CRATE_NAME: &str = "waxpkg";
const GITHUB_REPO: &str = "https://github.com/plyght/wax";
const CRATES_IO_API: &str = "https://crates.io/api/v1/crates";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

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

#[derive(Debug, Deserialize)]
struct CratesIoResponse {
    #[serde(rename = "crate")]
    krate: CrateInfo,
}

#[derive(Debug, Deserialize)]
struct CrateInfo {
    max_stable_version: String,
}

async fn fetch_latest_stable_version() -> Result<String> {
    let client = reqwest::Client::builder()
        .user_agent("wax-package-manager")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to create HTTP client: {}", e)))?;

    let url = format!("{}/{}", CRATES_IO_API, CRATE_NAME);
    let response = client.get(&url).send().await.map_err(|e| {
        WaxError::SelfUpdateError(format!(
            "Failed to fetch version info from crates.io: {}",
            e
        ))
    })?;

    if !response.status().is_success() {
        return Err(WaxError::SelfUpdateError(format!(
            "crates.io returned status {}",
            response.status()
        )));
    }

    let crate_info: CratesIoResponse = response.json().await.map_err(|e| {
        WaxError::SelfUpdateError(format!("Failed to parse crates.io response: {}", e))
    })?;

    Ok(crate_info.krate.max_stable_version)
}

fn parse_version(version: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() >= 3 {
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        let patch = parts[2].split('-').next()?.parse().ok()?;
        Some((major, minor, patch))
    } else {
        None
    }
}

fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some((c_major, c_minor, c_patch)), Some((l_major, l_minor, l_patch))) => {
            (l_major, l_minor, l_patch) > (c_major, c_minor, c_patch)
        }
        _ => false,
    }
}

fn run_cargo_install(args: &[&str]) -> Result<()> {
    let output = Command::new("cargo")
        .args(args)
        .output()
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to run cargo: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(WaxError::SelfUpdateError(format!(
            "cargo install failed:\n{}",
            stderr
        )));
    }

    Ok(())
}

#[instrument]
pub async fn self_update(channel: Channel, force: bool) -> Result<()> {
    info!(
        "Self-update initiated: channel={}, force={}",
        channel, force
    );

    println!();

    match channel {
        Channel::Stable => update_stable(force).await,
        Channel::Nightly => update_nightly(force).await,
    }
}

async fn update_stable(force: bool) -> Result<()> {
    let spinner = create_spinner("Checking for updates...");

    let latest_version = fetch_latest_stable_version().await?;

    spinner.finish_and_clear();

    let update_available = is_newer_version(CURRENT_VERSION, &latest_version);

    println!(
        "  {} {}",
        style("Current version:").dim(),
        style(CURRENT_VERSION).cyan()
    );
    println!(
        "  {} {}",
        style("Latest version:").dim(),
        style(&latest_version).cyan()
    );
    println!();

    if !update_available && !force {
        println!(
            "{} Already on the latest stable version.",
            style("✓").green()
        );
        println!(
            "  {} Use {} to reinstall anyway.",
            style("hint:").dim(),
            style("-f/--force").yellow()
        );
        return Ok(());
    }

    if !update_available && force {
        println!(
            "{} Forcing reinstall of stable version...",
            style("→").yellow()
        );
    } else {
        println!("{} Updating to v{}...", style("→").green(), latest_version);
    }

    println!();

    let spinner = create_spinner("Installing from crates.io...");

    let args = if force {
        vec!["install", CRATE_NAME, "--force"]
    } else {
        vec!["install", CRATE_NAME]
    };

    run_cargo_install(&args)?;

    spinner.finish_and_clear();

    println!(
        "{} Successfully updated to v{}",
        style("✓").green(),
        latest_version
    );

    Ok(())
}

async fn update_nightly(force: bool) -> Result<()> {
    println!(
        "  {} {}",
        style("Current version:").dim(),
        style(CURRENT_VERSION).cyan()
    );
    println!(
        "  {} {}",
        style("Channel:").dim(),
        style("nightly (GitHub main)").yellow()
    );
    println!();

    if force {
        println!(
            "{} Installing latest nightly build from GitHub...",
            style("→").yellow()
        );
    } else {
        println!(
            "{} Updating to latest nightly build from GitHub...",
            style("→").green()
        );
    }

    println!();

    let spinner = create_spinner("Building from source (this may take a moment)...");

    let args = if force {
        vec!["install", "--git", GITHUB_REPO, "--force"]
    } else {
        vec!["install", "--git", GITHUB_REPO]
    };

    run_cargo_install(&args)?;

    spinner.finish_and_clear();

    println!(
        "{} Successfully installed latest nightly build",
        style("✓").green()
    );

    Ok(())
}
