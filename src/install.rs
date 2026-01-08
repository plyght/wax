use crate::bottle::homebrew_prefix;
use crate::error::{Result, WaxError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, instrument};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub install_date: i64,
}

pub struct InstallState {
    state_path: PathBuf,
}

impl InstallState {
    pub fn new() -> Result<Self> {
        let state_path = dirs::home_dir()
            .ok_or_else(|| WaxError::CacheError("Cannot determine home directory".into()))?
            .join(".wax")
            .join("installed.json");

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
) -> Result<Vec<PathBuf>> {
    debug!(
        "Creating symlinks for {} {} (dry_run={})",
        formula_name, version, dry_run
    );

    let formula_path = cellar_path.join(formula_name).join(version);
    let prefix = homebrew_prefix();

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
                    return Err(WaxError::InstallError(
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
) -> Result<Vec<PathBuf>> {
    debug!(
        "Removing symlinks for {} {} (dry_run={})",
        formula_name, version, dry_run
    );

    let formula_path = cellar_path.join(formula_name).join(version);
    let prefix = homebrew_prefix();

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

mod dirs {
    use std::path::PathBuf;

    pub fn home_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}
