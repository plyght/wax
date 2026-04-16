use crate::error::{Result, WaxError};
use crate::ui::create_spinner;
use crate::version::WAX_VERSION as CURRENT_VERSION;
use console::style;
use inquire::Confirm;
use sha2::{Digest, Sha256};
use std::io::IsTerminal;
use tracing::{debug, info, instrument};

const GITHUB_REPO: &str = "semitechnological/wax";
const GITHUB_REPO_URL: &str = "https://github.com/semitechnological/wax";

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

/// Detect the asset name for the current platform/arch.
fn asset_name() -> Result<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        other => {
            return Err(WaxError::SelfUpdateError(format!(
                "Unsupported OS for self-update: {other}"
            )))
        }
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => {
            return Err(WaxError::SelfUpdateError(format!(
                "Unsupported architecture for self-update: {other}"
            )))
        }
    };
    Ok(format!("wax-{os}-{arch}"))
}

async fn fetch_latest_release_tag(client: &reqwest::Client) -> Result<String> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let resp = client
        .get(&url)
        .header("User-Agent", "wax-self-update")
        .send()
        .await
        .map_err(|e| WaxError::SelfUpdateError(format!("GitHub API request failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(WaxError::SelfUpdateError(format!(
            "GitHub API returned {}",
            resp.status()
        )));
    }

    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }

    let release: Release = resp.json().await.map_err(|e| {
        WaxError::SelfUpdateError(format!("Failed to parse GitHub API response: {e}"))
    })?;

    Ok(release.tag_name)
}

async fn download_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    debug!("Downloading {url}");
    let resp = client
        .get(url)
        .header("User-Agent", "wax-self-update")
        .send()
        .await
        .map_err(|e| WaxError::SelfUpdateError(format!("Download failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(WaxError::SelfUpdateError(format!(
            "HTTP {} downloading {url}",
            resp.status()
        )));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to read response: {e}")))?;
    Ok(bytes.to_vec())
}

/// Atomically replace the running binary.
///
/// We write to a `.tmp` file beside the destination, then rename — which is
/// atomic on the same filesystem. The old binary is moved aside first so the
/// rename cannot leave the directory without a working `wax` even if the
/// process is interrupted.
fn install_binary(bytes: &[u8]) -> Result<()> {
    let current_exe = std::env::current_exe().map_err(|e| {
        WaxError::SelfUpdateError(format!("Cannot determine current binary path: {e}"))
    })?;

    // Resolve symlinks so we write to the real file.
    let dest = dunce::canonicalize(&current_exe).unwrap_or(current_exe);
    let tmp = dest.with_extension("tmp");

    // Write new binary to a temp file in the same directory.
    std::fs::write(&tmp, bytes)
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to write temporary binary: {e}")))?;

    // Make it executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| WaxError::SelfUpdateError(format!("Failed to set permissions: {e}")))?;
    }

    // Atomic rename — replaces the destination.
    std::fs::rename(&tmp, &dest)
        .map_err(|e| WaxError::SelfUpdateError(format!("Failed to replace binary: {e}")))?;

    debug!("Installed new binary to {:?}", dest);
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
        Channel::Stable => update_from_release(force).await,
        Channel::Nightly => update_from_source(force, nightly_cleanup).await,
    }
}

async fn update_from_release(force: bool) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| WaxError::SelfUpdateError(format!("HTTP client error: {e}")))?;

    let spinner = create_spinner("Checking for updates…");
    let latest_tag = fetch_latest_release_tag(&client).await?;
    spinner.finish_and_clear();

    let latest_version = latest_tag.trim_start_matches('v');

    println!(
        "  {} {}",
        style("current:").dim(),
        style(CURRENT_VERSION).cyan()
    );
    println!(
        "  {} {}",
        style("latest: ").dim(),
        style(latest_version).cyan()
    );

    if !is_newer(CURRENT_VERSION, latest_version) && !force {
        println!("{} already up to date", style("✓").green());
        println!(
            "  {} use {} to reinstall anyway",
            style("hint:").dim(),
            style("-f / --force").yellow()
        );
        return Ok(());
    }

    let asset = asset_name()?;
    let base = format!("https://github.com/{GITHUB_REPO}/releases/download/{latest_tag}");

    let download_spinner = create_spinner(&format!("Downloading wax {latest_version}…"));

    // Download binary and optional checksum in parallel.
    let binary_url = format!("{base}/{asset}");
    let sha_url = format!("{base}/{asset}.sha256");
    let (binary_result, sha_result) = tokio::join!(
        download_bytes(&client, &binary_url),
        download_bytes(&client, &sha_url),
    );

    let binary = binary_result?;

    // Verify checksum when available (releases ≥ v0.13.3).
    if let Ok(sha_bytes) = sha_result {
        let expected = String::from_utf8_lossy(&sha_bytes).trim().to_string();
        if !expected.is_empty() {
            let actual = format!("{:x}", Sha256::digest(&binary));
            if actual != expected {
                download_spinner.finish_and_clear();
                return Err(WaxError::SelfUpdateError(format!(
                    "SHA256 mismatch — download may be corrupted\n  expected: {expected}\n  actual:   {actual}"
                )));
            }
            debug!("Checksum verified: {actual}");
        }
    } else {
        debug!("No checksum file for {latest_tag} — skipping verification");
    }

    download_spinner.finish_and_clear();

    let install_spinner = create_spinner("Installing…");
    install_binary(&binary)?;
    install_spinner.finish_and_clear();

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
            if name.starts_with("wax-") && path.is_dir() {
                if std::fs::remove_dir_all(&path).is_ok() {
                    removed += 1;
                }
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

    #[test]
    fn asset_name_returns_valid_string() {
        let name = asset_name().unwrap();
        assert!(name.starts_with("wax-"));
        assert!(name.contains('-'));
    }
}
