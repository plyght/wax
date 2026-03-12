use crate::cache::Cache;
use crate::error::{Result, WaxError};
use console::style;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::instrument;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Waxfile {
    #[serde(default)]
    pub tap: Vec<String>,
    #[serde(default)]
    pub brew: Vec<BundleEntry>,
    #[serde(default)]
    pub cask: Vec<BundleEntry>,
    #[serde(default)]
    pub cargo: Vec<BundleEntry>,
    #[serde(default)]
    pub uv: Vec<BundleEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BundleEntry {
    Simple(String),
    Detailed {
        name: String,
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        args: Option<Vec<String>>,
    },
}

impl BundleEntry {
    pub fn name(&self) -> &str {
        match self {
            BundleEntry::Simple(s) => s,
            BundleEntry::Detailed { name, .. } => name,
        }
    }

    pub fn version(&self) -> Option<&str> {
        match self {
            BundleEntry::Simple(_) => None,
            BundleEntry::Detailed { version, .. } => version.as_deref(),
        }
    }

    pub fn args(&self) -> Option<&[String]> {
        match self {
            BundleEntry::Simple(_) => None,
            BundleEntry::Detailed { args, .. } => args.as_deref(),
        }
    }
}

fn find_waxfile() -> Result<PathBuf> {
    let candidates = ["Waxfile", "Waxfile.toml", "waxfile", "waxfile.toml"];
    for name in &candidates {
        let path = PathBuf::from(name);
        if path.exists() {
            return Ok(path);
        }
    }
    Err(WaxError::BundleError(
        "No Waxfile found. Create a Waxfile.toml in your project root.".to_string(),
    ))
}

pub fn parse_waxfile(path: &Path) -> Result<Waxfile> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| WaxError::BundleError(format!("Cannot read {}: {}", path.display(), e)))?;
    let waxfile: Waxfile = toml::from_str(&content)?;
    Ok(waxfile)
}

#[instrument(skip(cache))]
pub async fn bundle(cache: &Cache, waxfile_path: Option<&str>, dry_run: bool) -> Result<()> {
    let start = std::time::Instant::now();

    let path = match waxfile_path {
        Some(p) => PathBuf::from(p),
        None => find_waxfile()?,
    };

    println!(
        "{} {}",
        style("wax bundle").bold(),
        style(path.display()).dim()
    );

    let waxfile = parse_waxfile(&path)?;

    let tap_count = waxfile.tap.len();
    let brew_count = waxfile.brew.len();
    let cask_count = waxfile.cask.len();
    let cargo_count = waxfile.cargo.len();
    let uv_count = waxfile.uv.len();
    let total = tap_count + brew_count + cask_count + cargo_count + uv_count;

    if total == 0 {
        println!("  {} Waxfile is empty", style("!").yellow());
        return Ok(());
    }

    println!(
        "  {} taps, {} formulae, {} casks, {} cargo, {} uv",
        style(tap_count).cyan(),
        style(brew_count).cyan(),
        style(cask_count).cyan(),
        style(cargo_count).cyan(),
        style(uv_count).cyan()
    );

    if dry_run {
        print_dry_run(&waxfile);
        return Ok(());
    }

    let mut success = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;

    for tap in &waxfile.tap {
        println!();
        println!("  {} tap {}", style("→").cyan(), style(tap).magenta());
        match add_tap(tap).await {
            Ok(true) => success += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                eprintln!(
                    "  {} tap {} failed: {}",
                    style("✗").red(),
                    style(tap).magenta(),
                    e
                );
                failed += 1;
            }
        }
    }

    if !waxfile.brew.is_empty() {
        let names: Vec<String> = waxfile.brew.iter().map(|e| e.name().to_string()).collect();
        println!();
        println!(
            "  {} installing {} formulae",
            style("→").cyan(),
            names.len()
        );
        match crate::commands::install::install(cache, &names, false, false, false, false, false)
            .await
        {
            Ok(()) => success += names.len(),
            Err(e) => {
                eprintln!("  {} brew install failed: {}", style("✗").red(), e);
                failed += names.len();
            }
        }
    }

    if !waxfile.cask.is_empty() {
        let names: Vec<String> = waxfile.cask.iter().map(|e| e.name().to_string()).collect();
        println!();
        println!("  {} installing {} casks", style("→").cyan(), names.len());
        match crate::commands::install::install(cache, &names, false, true, false, false, false)
            .await
        {
            Ok(()) => success += names.len(),
            Err(e) => {
                eprintln!("  {} cask install failed: {}", style("✗").red(), e);
                failed += names.len();
            }
        }
    }

    if !waxfile.cargo.is_empty() {
        println!();
        for entry in &waxfile.cargo {
            let name = entry.name();
            print!(
                "  {} cargo install {}",
                style("→").cyan(),
                style(name).magenta()
            );

            if is_cargo_installed(name).await {
                println!(" {}", style("(already installed)").dim());
                skipped += 1;
                continue;
            }
            println!();

            match cargo_install(entry).await {
                Ok(()) => {
                    println!("  {} cargo {}", style("✓").green(), style(name).magenta());
                    success += 1;
                }
                Err(e) => {
                    eprintln!(
                        "  {} cargo {} failed: {}",
                        style("✗").red(),
                        style(name).magenta(),
                        e
                    );
                    failed += 1;
                }
            }
        }
    }

    if !waxfile.uv.is_empty() {
        println!();
        for entry in &waxfile.uv {
            let name = entry.name();
            print!(
                "  {} uv tool install {}",
                style("→").cyan(),
                style(name).magenta()
            );

            if is_uv_tool_installed(name).await {
                println!(" {}", style("(already installed)").dim());
                skipped += 1;
                continue;
            }
            println!();

            match uv_tool_install(entry).await {
                Ok(()) => {
                    println!("  {} uv {}", style("✓").green(), style(name).magenta());
                    success += 1;
                }
                Err(e) => {
                    eprintln!(
                        "  {} uv {} failed: {}",
                        style("✗").red(),
                        style(name).magenta(),
                        e
                    );
                    failed += 1;
                }
            }
        }
    }

    let elapsed = start.elapsed();
    println!();
    if failed == 0 {
        println!(
            "{} installed, {} skipped [{}ms]",
            style(success).green(),
            style(skipped).dim(),
            elapsed.as_millis()
        );
    } else {
        println!(
            "{} installed, {} failed, {} skipped [{}ms]",
            style(success).green(),
            style(failed).red(),
            style(skipped).dim(),
            elapsed.as_millis()
        );
    }

    Ok(())
}

#[instrument(skip(_cache))]
pub async fn bundle_dump(_cache: &Cache) -> Result<()> {
    let state = crate::install::InstallState::new()?;
    let installed = state.load().await?;
    let cask_state = crate::cask::CaskState::new()?;
    let installed_casks = cask_state.load().await?;

    let mut waxfile = String::new();

    if !installed.is_empty() {
        waxfile.push_str("brew = [\n");
        let mut names: Vec<_> = installed.keys().collect();
        names.sort();
        for name in names {
            waxfile.push_str(&format!("  \"{}\",\n", name));
        }
        waxfile.push_str("]\n\n");
    }

    if !installed_casks.is_empty() {
        waxfile.push_str("cask = [\n");
        let mut names: Vec<_> = installed_casks.keys().collect();
        names.sort();
        for name in names {
            waxfile.push_str(&format!("  \"{}\",\n", name));
        }
        waxfile.push_str("]\n");
    }

    print!("{}", waxfile);
    Ok(())
}

fn print_dry_run(waxfile: &Waxfile) {
    println!();
    for tap in &waxfile.tap {
        println!("  tap {}", style(tap).magenta());
    }
    for entry in &waxfile.brew {
        println!("  brew {}", style(entry.name()).magenta());
    }
    for entry in &waxfile.cask {
        println!(
            "  cask {} {}",
            style(entry.name()).magenta(),
            style("(cask)").yellow()
        );
    }
    for entry in &waxfile.cargo {
        println!("  cargo {}", style(entry.name()).magenta());
    }
    for entry in &waxfile.uv {
        println!("  uv {}", style(entry.name()).magenta());
    }
    println!("\n{}", style("dry run - no changes made").dim());
}

async fn add_tap(tap: &str) -> Result<bool> {
    let mut tap_manager = crate::tap::TapManager::new()?;
    tap_manager.load().await?;
    if tap_manager.has_tap(tap).await {
        return Ok(false);
    }

    let tap_parts: Vec<&str> = tap.split('/').collect();
    if tap_parts.len() < 2 {
        return Err(WaxError::BundleError(format!(
            "Invalid tap format: {}",
            tap
        )));
    }

    tap_manager.add_tap(tap).await?;
    Ok(true)
}

async fn is_cargo_installed(name: &str) -> bool {
    let output = Command::new("cargo")
        .args(["install", "--list"])
        .output()
        .await;

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout
                .lines()
                .any(|line| !line.starts_with(' ') && line.starts_with(name))
        }
        Err(_) => false,
    }
}

async fn cargo_install(entry: &BundleEntry) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.arg("install");

    let name = entry.name();
    cmd.arg(name);

    if let Some(version) = entry.version() {
        cmd.args(["--version", version]);
    }

    if let Some(args) = entry.args() {
        cmd.args(args);
    }

    let status = cmd
        .status()
        .await
        .map_err(|e| WaxError::BundleError(format!("cargo not found: {}", e)))?;

    if !status.success() {
        return Err(WaxError::BundleError(format!(
            "cargo install {} failed with exit code {}",
            name,
            status.code().unwrap_or(-1)
        )));
    }

    Ok(())
}

async fn is_uv_tool_installed(name: &str) -> bool {
    let output = Command::new("uv").args(["tool", "list"]).output().await;

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.lines().any(|line| line.starts_with(name))
        }
        Err(_) => false,
    }
}

async fn uv_tool_install(entry: &BundleEntry) -> Result<()> {
    let mut cmd = Command::new("uv");
    cmd.args(["tool", "install"]);

    let name = entry.name();

    if let Some(version) = entry.version() {
        cmd.arg(format!("{}=={}", name, version));
    } else {
        cmd.arg(name);
    }

    if let Some(args) = entry.args() {
        cmd.args(args);
    }

    let status = cmd
        .status()
        .await
        .map_err(|e| WaxError::BundleError(format!("uv not found: {}", e)))?;

    if !status.success() {
        return Err(WaxError::BundleError(format!(
            "uv tool install {} failed with exit code {}",
            name,
            status.code().unwrap_or(-1)
        )));
    }

    Ok(())
}
