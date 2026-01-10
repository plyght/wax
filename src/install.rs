use crate::bottle::{homebrew_prefix, run_command_with_timeout};
use crate::error::{Result, WaxError};
use crate::ui::dirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, instrument};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallMode {
    User,
    Global,
}

impl InstallMode {
    pub fn detect() -> Self {
        let prefix = homebrew_prefix();

        let cellar = prefix.join("Cellar");
        if (cellar.exists() || prefix.exists()) && is_writable(&prefix) {
            return InstallMode::Global;
        }

        InstallMode::User
    }

    pub fn from_flags(user: bool, global: bool) -> Result<Option<Self>> {
        match (user, global) {
            (true, true) => Err(WaxError::InstallError(
                "Cannot specify both --user and --global".to_string(),
            )),
            (true, false) => Ok(Some(InstallMode::User)),
            (false, true) => Ok(Some(InstallMode::Global)),
            (false, false) => Ok(None),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if *self == InstallMode::Global {
            let prefix = homebrew_prefix();
            if !is_writable(&prefix) {
                return Err(WaxError::InstallError(format!(
                    "Cannot write to {}. This usually means:\n  \
                     - You don't have permission (try: sudo wax install or wax install --user)\n  \
                     - The directory doesn't exist (Homebrew may not be installed)\n\n  \
                     For per-user installation: wax install --user",
                    prefix.display()
                )));
            }
        }
        Ok(())
    }

    pub fn prefix(&self) -> Result<PathBuf> {
        match self {
            InstallMode::User => Ok(dirs::home_dir()?.join(".local").join("wax")),
            InstallMode::Global => Ok(homebrew_prefix()),
        }
    }

    pub fn cellar_path(&self) -> Result<PathBuf> {
        Ok(self.prefix()?.join("Cellar"))
    }
}

fn is_writable(path: &Path) -> bool {
    use std::fs::OpenOptions;

    let test_file = path.join(".wax_write_test");
    let result = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&test_file);

    if result.is_ok() {
        let _ = std::fs::remove_file(&test_file);
        true
    } else {
        false
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub install_date: i64,
    #[serde(default = "default_install_mode")]
    pub install_mode: InstallMode,
    #[serde(default)]
    pub from_source: bool,
}

fn default_install_mode() -> InstallMode {
    InstallMode::Global
}

pub struct InstallState {
    state_path: PathBuf,
}

impl InstallState {
    pub fn new() -> Result<Self> {
        let state_path = if let Some(base_dirs) = directories::BaseDirs::new() {
            base_dirs
                .data_local_dir()
                .join("wax")
                .join("installed.json")
        } else {
            dirs::home_dir()?.join(".wax").join("installed.json")
        };

        Ok(Self { state_path })
    }

    pub async fn load(&self) -> Result<HashMap<String, InstalledPackage>> {
        match fs::read_to_string(&self.state_path).await {
            Ok(json) => {
                let packages: HashMap<String, InstalledPackage> = serde_json::from_str(&json)?;
                Ok(packages)
            }
            Err(_) => Ok(HashMap::new()),
        }
    }

    pub async fn save(&self, packages: &HashMap<String, InstalledPackage>) -> Result<()> {
        let parent = self
            .state_path
            .parent()
            .ok_or_else(|| WaxError::CacheError("Cannot determine parent directory".into()))?;
        fs::create_dir_all(parent).await?;

        let json = serde_json::to_string_pretty(packages)?;
        fs::write(&self.state_path, json).await?;
        Ok(())
    }

    pub async fn add(&self, package: InstalledPackage) -> Result<()> {
        let mut packages = self.load().await?;
        packages.insert(package.name.clone(), package);
        self.save(&packages).await?;
        Ok(())
    }

    pub async fn remove(&self, name: &str) -> Result<()> {
        let mut packages = self.load().await?;
        packages.remove(name);
        self.save(&packages).await?;
        Ok(())
    }

    fn detect_install_mode(&self, cellar: &Path) -> InstallMode {
        if cellar.starts_with("/opt/homebrew") || cellar.starts_with("/usr/local") {
            InstallMode::Global
        } else {
            InstallMode::User
        }
    }

    pub async fn sync_from_cellar(&self) -> Result<()> {
        let mut packages = self.load().await?;

        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        let candidates = match os {
            "macos" => match arch {
                "aarch64" => vec![PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")],
                _ => vec![PathBuf::from("/usr/local"), PathBuf::from("/opt/homebrew")],
            },
            "linux" => vec![
                PathBuf::from("/home/linuxbrew/.linuxbrew"),
                PathBuf::from("/usr/local"),
            ],
            _ => vec![PathBuf::from("/usr/local")],
        };

        if let Some(prefix_str) = run_command_with_timeout("brew", &["--prefix"], 2) {
            let brew_prefix = PathBuf::from(prefix_str);
            let cellar = brew_prefix.join("Cellar");
            if cellar.exists() {
                self.scan_cellar_and_update(&cellar, &mut packages).await?;
            }
        }

        for path in candidates {
            let cellar = path.join("Cellar");
            if cellar.exists() {
                self.scan_cellar_and_update(&cellar, &mut packages).await?;
                break;
            }
        }

        if let Ok(home) = dirs::home_dir() {
            let wax_user_cellar = home.join(".local/wax/Cellar");
            if wax_user_cellar.exists() {
                self.scan_cellar_and_update(&wax_user_cellar, &mut packages)
                    .await?;
            }
        }

        self.save(&packages).await?;
        Ok(())
    }

    async fn scan_cellar_and_update(
        &self,
        cellar: &Path,
        packages: &mut HashMap<String, InstalledPackage>,
    ) -> Result<()> {
        let mut entries = tokio::fs::read_dir(cellar).await?;

        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                let package_name = entry.file_name().to_string_lossy().to_string();

                if packages.contains_key(&package_name) {
                    continue;
                }

                let mut versions = Vec::new();
                let mut version_entries = tokio::fs::read_dir(entry.path()).await?;
                while let Some(version_entry) = version_entries.next_entry().await? {
                    if version_entry.file_type().await?.is_dir() {
                        versions.push(version_entry.file_name().to_string_lossy().to_string());
                    }
                }

                if !versions.is_empty() {
                    versions.sort();
                    let version = versions.last().unwrap().clone();

                    packages.insert(
                        package_name.clone(),
                        InstalledPackage {
                            name: package_name,
                            version,
                            platform: format!(
                                "{}-{}",
                                std::env::consts::OS,
                                std::env::consts::ARCH
                            ),
                            install_date: 0,
                            install_mode: self.detect_install_mode(cellar),
                            from_source: false,
                        },
                    );
                }
            }
        }

        Ok(())
    }
}

impl Default for InstallState {
    fn default() -> Self {
        Self::new().expect("Failed to initialize install state")
    }
}

#[instrument(skip(cellar_path))]
pub async fn create_symlinks(
    formula_name: &str,
    version: &str,
    cellar_path: &Path,
    dry_run: bool,
    install_mode: InstallMode,
) -> Result<Vec<PathBuf>> {
    debug!(
        "Creating symlinks for {} {} (dry_run={}, mode={:?})",
        formula_name, version, dry_run, install_mode
    );

    let formula_path = cellar_path.join(formula_name).join(version);
    let prefix = install_mode.prefix()?;

    let mut created_links = Vec::new();

    let link_dirs = vec![
        ("bin", prefix.join("bin")),
        ("lib", prefix.join("lib")),
        ("include", prefix.join("include")),
        ("share", prefix.join("share")),
        ("etc", prefix.join("etc")),
        ("sbin", prefix.join("sbin")),
    ];

    for (subdir, target_dir) in link_dirs {
        let source_dir = formula_path.join(subdir);

        if !source_dir.exists() {
            continue;
        }

        if !dry_run {
            fs::create_dir_all(&target_dir).await?;
        }

        let mut entries = fs::read_dir(&source_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_name = entry.file_name();
            let source_path = entry.path();
            let target_path = target_dir.join(&file_name);

            if target_path.exists() {
                debug!("Symlink target already exists: {:?}", target_path);
                continue;
            }

            if !dry_run {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::symlink;
                    symlink(&source_path, &target_path)?;
                }
                #[cfg(not(unix))]
                {
                    return Err(WaxError::PlatformNotSupported(
                        "Symlinks not supported on this platform".to_string(),
                    ));
                }
            }

            created_links.push(target_path);
        }
    }

    debug!("Created {} symlinks", created_links.len());
    Ok(created_links)
}

#[instrument(skip(cellar_path))]
pub async fn remove_symlinks(
    formula_name: &str,
    version: &str,
    cellar_path: &Path,
    dry_run: bool,
    install_mode: InstallMode,
) -> Result<Vec<PathBuf>> {
    debug!(
        "Removing symlinks for {} {} (dry_run={}, mode={:?})",
        formula_name, version, dry_run, install_mode
    );

    let formula_path = cellar_path.join(formula_name).join(version);
    let prefix = install_mode.prefix()?;

    let mut removed_links = Vec::new();

    let link_dirs = vec![
        ("bin", prefix.join("bin")),
        ("lib", prefix.join("lib")),
        ("include", prefix.join("include")),
        ("share", prefix.join("share")),
        ("etc", prefix.join("etc")),
        ("sbin", prefix.join("sbin")),
    ];

    for (subdir, target_dir) in link_dirs {
        let source_dir = formula_path.join(subdir);

        if !source_dir.exists() {
            continue;
        }

        let mut entries = fs::read_dir(&source_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_name = entry.file_name();
            let target_path = target_dir.join(&file_name);

            if !target_path.exists() {
                continue;
            }

            #[cfg(unix)]
            {
                if let Ok(metadata) = fs::symlink_metadata(&target_path).await {
                    if metadata.is_symlink() {
                        if let Ok(link_target) = fs::read_link(&target_path).await {
                            if link_target.starts_with(&formula_path) {
                                if !dry_run {
                                    fs::remove_file(&target_path).await?;
                                }
                                removed_links.push(target_path);
                            }
                        }
                    }
                }
            }
        }
    }

    debug!("Removed {} symlinks", removed_links.len());
    Ok(removed_links)
}
