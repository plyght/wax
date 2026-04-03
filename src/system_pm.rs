//! Native OS package manager integration.
//!
//! Detects whichever system package manager is present and provides a unified
//! interface for install, upgrade, and listing operations.  This lets wax act
//! as a single entry point for both Homebrew-formula packages and OS-level
//! packages (apt, dnf, pacman, apk, zypper, emerge, yum, xbps-install, nix).

use crate::error::{Result, WaxError};
use console::style;
use tokio::process::Command;
use tracing::debug;

/// A detected system package manager.
#[derive(Debug, Clone, PartialEq)]
pub enum SystemPm {
    Apt,
    Dnf,
    Pacman,
    Apk,
    Zypper,
    Emerge,
    Yum,
    Xbps,
    Nix,
}

impl SystemPm {
    /// Human-readable name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Apt => "apt",
            Self::Dnf => "dnf",
            Self::Pacman => "pacman",
            Self::Apk => "apk",
            Self::Zypper => "zypper",
            Self::Emerge => "emerge",
            Self::Yum => "yum",
            Self::Xbps => "xbps-install",
            Self::Nix => "nix-env",
        }
    }

    /// Detect the first available system package manager on the current host.
    /// Returns `None` on macOS or when no supported PM is found.
    pub async fn detect() -> Option<Self> {
        #[cfg(target_os = "macos")]
        return None;

        let candidates: &[(&str, Self)] = &[
            ("apt-get", Self::Apt),
            ("dnf", Self::Dnf),
            ("pacman", Self::Pacman),
            ("apk", Self::Apk),
            ("zypper", Self::Zypper),
            ("emerge", Self::Emerge),
            ("yum", Self::Yum),
            ("xbps-install", Self::Xbps),
            ("nix-env", Self::Nix),
        ];

        for (bin, pm) in candidates {
            if which(bin).await {
                debug!("Detected system package manager: {}", bin);
                return Some(pm.clone());
            }
        }
        None
    }

    /// Upgrade all packages managed by this PM.
    /// Streams output directly to the terminal (many upgrade commands are
    /// interactive / produce a lot of output).
    pub async fn upgrade_all(&self) -> Result<()> {
        // For apt we need to do "update" then "upgrade" as two steps.
        match self {
            Self::Apt => {
                run_visible("sudo", &["apt-get", "update", "-q"]).await?;
                run_visible("sudo", &["apt-get", "upgrade", "-y"]).await?;
            }
            Self::Dnf => {
                run_visible("sudo", &["dnf", "upgrade", "--refresh", "-y"]).await?;
            }
            Self::Pacman => {
                run_visible("sudo", &["pacman", "-Syu", "--noconfirm"]).await?;
            }
            Self::Apk => {
                run_visible("sudo", &["apk", "upgrade"]).await?;
            }
            Self::Zypper => {
                run_visible("sudo", &["zypper", "refresh"]).await?;
                run_visible("sudo", &["zypper", "update", "-y"]).await?;
            }
            Self::Emerge => {
                run_visible("sudo", &["emerge", "--sync"]).await?;
                run_visible(
                    "sudo",
                    &["emerge", "--update", "--deep", "--newuse", "@world"],
                )
                .await?;
            }
            Self::Yum => {
                run_visible("sudo", &["yum", "update", "-y"]).await?;
            }
            Self::Xbps => {
                run_visible("sudo", &["xbps-install", "-Su"]).await?;
            }
            Self::Nix => {
                run_visible("nix-channel", &["--update"]).await?;
                run_visible("nix-env", &["-u", "*"]).await?;
            }
        }
        Ok(())
    }

    /// Install one or more packages via the system PM.
    pub async fn install(&self, packages: &[String]) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let pkg_args: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();

        match self {
            Self::Apt => {
                let mut args = vec!["apt-get", "install", "-y"];
                args.extend_from_slice(&pkg_args);
                run_visible("sudo", &args).await?;
            }
            Self::Dnf => {
                let mut args = vec!["dnf", "install", "-y"];
                args.extend_from_slice(&pkg_args);
                run_visible("sudo", &args).await?;
            }
            Self::Pacman => {
                let mut args = vec!["pacman", "-S", "--noconfirm"];
                args.extend_from_slice(&pkg_args);
                run_visible("sudo", &args).await?;
            }
            Self::Apk => {
                let mut args = vec!["apk", "add"];
                args.extend_from_slice(&pkg_args);
                run_visible("sudo", &args).await?;
            }
            Self::Zypper => {
                let mut args = vec!["zypper", "install", "-y"];
                args.extend_from_slice(&pkg_args);
                run_visible("sudo", &args).await?;
            }
            Self::Emerge => {
                let mut args: Vec<&str> = vec!["emerge"];
                args.extend_from_slice(&pkg_args);
                run_visible("sudo", &args).await?;
            }
            Self::Yum => {
                let mut args = vec!["yum", "install", "-y"];
                args.extend_from_slice(&pkg_args);
                run_visible("sudo", &args).await?;
            }
            Self::Xbps => {
                let mut args = vec!["xbps-install", "-S"];
                args.extend_from_slice(&pkg_args);
                run_visible("sudo", &args).await?;
            }
            Self::Nix => {
                let mut args = vec!["-i"];
                args.extend_from_slice(&pkg_args);
                run_visible("nix-env", &args).await?;
            }
        }
        Ok(())
    }

    /// Install a cask (GUI app) on Linux by trying snap → flatpak → native PM.
    ///
    /// On Linux, Homebrew cask artifacts are macOS-only, so we route cask
    /// installs through the Linux app distribution layers instead.
    pub async fn install_cask(&self, cask_name: &str) -> Result<()> {
        // 1. Try snap — no extra repo setup needed on Ubuntu/Debian/derivatives.
        if which("snap").await {
            // Some snaps need --classic confinement; try both.
            for args in &[
                vec!["install", cask_name],
                vec!["install", "--classic", cask_name],
            ] {
                let ok = tokio::process::Command::new("snap")
                    .args(args.as_slice())
                    .status()
                    .await
                    .map(|s| s.success())
                    .unwrap_or(false);
                if ok {
                    return Ok(());
                }
            }
        }

        // 2. Try flatpak via Flathub — good GUI app coverage on Fedora/etc.
        if which("flatpak").await {
            // Ensure Flathub remote exists (harmless if already present).
            let _ = tokio::process::Command::new("flatpak")
                .args([
                    "remote-add",
                    "--if-not-exists",
                    "flathub",
                    "https://dl.flathub.org/repo/flathub.flatpakrepo",
                ])
                .output()
                .await;

            let ok = tokio::process::Command::new("flatpak")
                .args(["install", "-y", "flathub", cask_name])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return Ok(());
            }
        }

        // 3. Fall back to native package manager (apt, dnf, pacman, etc.).
        self.install(&[cask_name.to_string()]).await
    }
}

/// Check if a binary exists on PATH.
async fn which(bin: &str) -> bool {
    Command::new("which")
        .arg(bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run a command, inheriting stdin/stdout/stderr so the user sees all output
/// and can interact (e.g. sudo password prompt).
async fn run_visible(program: &str, args: &[&str]) -> Result<()> {
    println!(
        "  {} {} {}",
        style("→").cyan(),
        style(program).dim(),
        args.join(" ")
    );

    let status = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .await
        .map_err(|e| WaxError::InstallError(format!("Failed to run {}: {}", program, e)))?;

    if !status.success() {
        return Err(WaxError::InstallError(format!(
            "{} exited with status {}",
            program,
            status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}
