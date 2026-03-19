use crate::bottle::homebrew_prefix;
use crate::error::{validate_package_name, Result, WaxError};
use crate::install::InstallState;
use console::style;
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::instrument;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ServiceInfo {
    pub name: String,
    pub status: ServiceStatus,
    pub plist_path: Option<PathBuf>,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum ServiceStatus {
    Running,
    Stopped,
    Error,
    Unknown,
}

impl std::fmt::Display for ServiceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceStatus::Running => write!(f, "running"),
            ServiceStatus::Stopped => write!(f, "stopped"),
            ServiceStatus::Error => write!(f, "error"),
            ServiceStatus::Unknown => write!(f, "unknown"),
        }
    }
}

fn services_dir() -> PathBuf {
    let prefix = homebrew_prefix();
    prefix.join("opt")
}

fn launchctl_plist_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join("Library/LaunchAgents")
    } else {
        PathBuf::from("/Library/LaunchDaemons")
    }
}

fn find_service_plist(formula_name: &str) -> Option<PathBuf> {
    let opt_dir = services_dir().join(formula_name);

    let search_dirs = [
        opt_dir.join("homebrew.mxcl.".to_string() + formula_name + ".plist"),
        opt_dir.join(formula_name.to_string() + ".plist"),
    ];

    for path in &search_dirs {
        if path.exists() {
            return Some(path.clone());
        }
    }

    let plist_pattern = format!("homebrew.mxcl.{}.plist", formula_name);
    let alt_pattern = format!("{}.plist", formula_name);

    if let Ok(entries) = std::fs::read_dir(&opt_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == plist_pattern || name == alt_pattern || name.ends_with(".plist") {
                return Some(entry.path());
            }
        }
    }

    let cellar_opt = homebrew_prefix().join("Cellar").join(formula_name);
    if let Ok(versions) = std::fs::read_dir(&cellar_opt) {
        for version_entry in versions.flatten() {
            let version_dir = version_entry.path();
            let plist_in_cellar = version_dir.join(&plist_pattern);
            if plist_in_cellar.exists() {
                return Some(plist_in_cellar);
            }
            let alt_in_cellar = version_dir.join(&alt_pattern);
            if alt_in_cellar.exists() {
                return Some(alt_in_cellar);
            }
            if let Ok(files) = std::fs::read_dir(&version_dir) {
                for file in files.flatten() {
                    let fname = file.file_name().to_string_lossy().to_string();
                    if fname.ends_with(".plist") {
                        return Some(file.path());
                    }
                }
            }
        }
    }

    None
}

#[cfg(target_os = "linux")]
fn find_systemd_unit(formula_name: &str) -> Option<PathBuf> {
    let opt_dir = services_dir().join(formula_name);
    let unit_name = format!("homebrew.{}.service", formula_name);

    let path = opt_dir.join(&unit_name);
    if path.exists() {
        return Some(path);
    }

    let cellar_opt = homebrew_prefix().join("Cellar").join(formula_name);
    if let Ok(versions) = std::fs::read_dir(&cellar_opt) {
        for version_entry in versions.flatten() {
            let unit_in_cellar = version_entry.path().join(&unit_name);
            if unit_in_cellar.exists() {
                return Some(unit_in_cellar);
            }
        }
    }

    None
}

#[instrument]
pub async fn services_list() -> Result<()> {
    let state = InstallState::new()?;
    state.sync_from_cellar().await.ok();
    let installed = state.load().await?;

    let mut services = Vec::new();

    for name in installed.keys() {
        let plist = find_service_plist(name);
        if plist.is_some() {
            let status = get_service_status(name).await;
            let pid = get_service_pid(name).await;
            services.push(ServiceInfo {
                name: name.clone(),
                status,
                plist_path: plist,
                pid,
            });
        }

        #[cfg(target_os = "linux")]
        {
            if find_systemd_unit(name).is_some() {
                let status = get_service_status(name).await;
                services.push(ServiceInfo {
                    name: name.clone(),
                    status,
                    plist_path: None,
                    pid: None,
                });
            }
        }
    }

    if services.is_empty() {
        println!("no services found");
        return Ok(());
    }

    services.sort_by(|a, b| a.name.cmp(&b.name));

    println!();
    for svc in &services {
        let status_style = match svc.status {
            ServiceStatus::Running => style("running").green(),
            ServiceStatus::Stopped => style("stopped").dim(),
            ServiceStatus::Error => style("error").red(),
            ServiceStatus::Unknown => style("unknown").dim(),
        };

        let pid_str = svc.pid.map(|p| format!(" (pid {})", p)).unwrap_or_default();

        println!(
            "  {} {} {}",
            style(&svc.name).magenta(),
            status_style,
            style(&pid_str).dim()
        );
    }

    let running = services
        .iter()
        .filter(|s| s.status == ServiceStatus::Running)
        .count();
    let stopped = services.len() - running;
    println!(
        "\n{} services ({} running, {} stopped)",
        style(services.len()).cyan(),
        style(running).green(),
        style(stopped).dim()
    );

    Ok(())
}

#[instrument]
pub async fn services_start(formula_name: &str, nice: Option<i32>) -> Result<()> {
    validate_package_name(formula_name)?;
    let state = InstallState::new()?;
    let installed = state.load().await?;

    if !installed.contains_key(formula_name) {
        return Err(WaxError::NotInstalled(formula_name.to_string()));
    }

    #[cfg(target_os = "macos")]
    {
        let plist = find_service_plist(formula_name).ok_or_else(|| {
            WaxError::ServiceError(format!(
                "{} does not have a service definition (no plist found)",
                formula_name
            ))
        })?;

        let target_dir = launchctl_plist_dir();
        std::fs::create_dir_all(&target_dir)?;

        let plist_name = plist.file_name().unwrap().to_string_lossy().to_string();
        let target_plist = target_dir.join(&plist_name);

        let mut plist_content = std::fs::read_to_string(&plist)?;

        if let Some(priority) = nice {
            if !plist_content.contains("<key>Nice</key>") {
                let nice_entry = format!(
                    "\t<key>Nice</key>\n\t<integer>{}</integer>\n</dict>",
                    priority
                );
                plist_content = plist_content.replace("</dict>", &nice_entry);
            }
        }

        if !plist_content.contains("<key>ThrottleInterval</key>") {
            let throttle = "\t<key>ThrottleInterval</key>\n\t<integer>60</integer>\n</dict>";
            plist_content = plist_content.replace("</dict>", throttle);
        }

        std::fs::write(&target_plist, &plist_content)?;

        let _label = plist_name.trim_end_matches(".plist");
        let output = Command::new("launchctl")
            .args(["load", "-w"])
            .arg(&target_plist)
            .output()
            .await
            .map_err(|e| WaxError::ServiceError(format!("launchctl failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("already loaded") {
                return Err(WaxError::ServiceError(format!(
                    "launchctl load failed: {}",
                    stderr
                )));
            }
        }

        println!(
            "{} {} started",
            style("✓").green(),
            style(formula_name).magenta()
        );
    }

    #[cfg(target_os = "linux")]
    {
        let unit = find_systemd_unit(formula_name).ok_or_else(|| {
            WaxError::ServiceError(format!(
                "{} does not have a systemd service unit",
                formula_name
            ))
        })?;

        let systemd_user_dir =
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/systemd/user");
        std::fs::create_dir_all(&systemd_user_dir)?;

        let unit_name = unit.file_name().unwrap().to_string_lossy().to_string();
        let target_unit = systemd_user_dir.join(&unit_name);
        std::fs::copy(&unit, &target_unit)?;

        let output = Command::new("systemctl")
            .args(["--user", "enable", "--now", &unit_name])
            .output()
            .await
            .map_err(|e| WaxError::ServiceError(format!("systemctl failed: {}", e)))?;

        if !output.status.success() {
            return Err(WaxError::ServiceError(format!(
                "systemctl enable failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        println!(
            "{} {} started",
            style("✓").green(),
            style(formula_name).magenta()
        );
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        return Err(WaxError::ServiceError(
            "Service management not supported on this platform".to_string(),
        ));
    }

    Ok(())
}

#[instrument]
pub async fn services_stop(formula_name: &str) -> Result<()> {
    validate_package_name(formula_name)?;
    #[cfg(target_os = "macos")]
    {
        let plist_dir = launchctl_plist_dir();
        let plist_name = format!("homebrew.mxcl.{}.plist", formula_name);
        let plist_path = plist_dir.join(&plist_name);

        if !plist_path.exists() {
            let alt_name = format!("{}.plist", formula_name);
            let alt_path = plist_dir.join(&alt_name);
            if alt_path.exists() {
                return stop_launchctl(&alt_path, formula_name).await;
            }
            return Err(WaxError::ServiceError(format!(
                "{} service is not running",
                formula_name
            )));
        }

        return stop_launchctl(&plist_path, formula_name).await;
    }

    #[cfg(target_os = "linux")]
    {
        let unit_name = format!("homebrew.{}.service", formula_name);
        let output = Command::new("systemctl")
            .args(["--user", "disable", "--now", &unit_name])
            .output()
            .await
            .map_err(|e| WaxError::ServiceError(format!("systemctl failed: {}", e)))?;

        if !output.status.success() {
            return Err(WaxError::ServiceError(format!(
                "systemctl disable failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        println!(
            "{} {} stopped",
            style("✓").green(),
            style(formula_name).magenta()
        );
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Err(WaxError::ServiceError(
        "Service management not supported on this platform".to_string(),
    ))
}

#[cfg(target_os = "macos")]
async fn stop_launchctl(plist_path: &Path, formula_name: &str) -> Result<()> {
    let output = Command::new("launchctl")
        .args(["unload", "-w"])
        .arg(plist_path)
        .output()
        .await
        .map_err(|e| WaxError::ServiceError(format!("launchctl failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("Could not find specified service") {
            return Err(WaxError::ServiceError(format!(
                "launchctl unload failed: {}",
                stderr
            )));
        }
    }

    let _ = std::fs::remove_file(plist_path);

    println!(
        "{} {} stopped",
        style("✓").green(),
        style(formula_name).magenta()
    );
    Ok(())
}

#[instrument]
pub async fn services_restart(formula_name: &str, nice: Option<i32>) -> Result<()> {
    let _ = services_stop(formula_name).await;
    services_start(formula_name, nice).await
}

async fn get_service_status(formula_name: &str) -> ServiceStatus {
    #[cfg(target_os = "macos")]
    {
        let labels = [
            format!("homebrew.mxcl.{}", formula_name),
            formula_name.to_string(),
        ];

        for label in &labels {
            let output = Command::new("launchctl")
                .args(["list", label])
                .output()
                .await;

            if let Ok(out) = output {
                if out.status.success() {
                    return ServiceStatus::Running;
                }
            }
        }

        let plist_dir = launchctl_plist_dir();
        let plist = plist_dir.join(format!("homebrew.mxcl.{}.plist", formula_name));
        if plist.exists() {
            return ServiceStatus::Stopped;
        }

        ServiceStatus::Stopped
    }

    #[cfg(target_os = "linux")]
    {
        let unit = format!("homebrew.{}.service", formula_name);
        let output = Command::new("systemctl")
            .args(["--user", "is-active", &unit])
            .output()
            .await;

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if stdout.trim() == "active" {
                    ServiceStatus::Running
                } else {
                    ServiceStatus::Stopped
                }
            }
            Err(_) => ServiceStatus::Unknown,
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    ServiceStatus::Unknown
}

async fn get_service_pid(formula_name: &str) -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        let label = format!("homebrew.mxcl.{}", formula_name);
        let output = Command::new("launchctl")
            .args(["list", &label])
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let line = line.trim();
            if line.starts_with("\"PID\"") || line.contains("PID") {
                let parts: Vec<&str> = line.split(|c: char| !c.is_ascii_digit()).collect();
                for part in parts {
                    if let Ok(pid) = part.parse::<u32>() {
                        if pid > 0 {
                            return Some(pid);
                        }
                    }
                }
            }
        }

        None
    }

    #[cfg(target_os = "linux")]
    {
        let unit = format!("homebrew.{}.service", formula_name);
        let output = Command::new("systemctl")
            .args(["--user", "show", "--property=MainPID", &unit])
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let pid_str = stdout.trim().strip_prefix("MainPID=")?;
        let pid: u32 = pid_str.parse().ok()?;
        if pid > 0 {
            Some(pid)
        } else {
            None
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    None
}
