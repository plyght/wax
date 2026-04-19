use crate::cask::CaskState;
use crate::error::{Result, WaxError};
use crate::install::InstallState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, instrument, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockfilePackage {
    pub version: String,
    pub bottle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockfileCask {
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Lockfile {
    #[serde(default)]
    pub packages: HashMap<String, LockfilePackage>,
    #[serde(default)]
    pub casks: HashMap<String, LockfileCask>,
}

impl Lockfile {
    pub fn new() -> Self {
        Self {
            packages: HashMap::new(),
            casks: HashMap::new(),
        }
    }

    #[instrument]
    #[allow(dead_code)]
    pub async fn generate() -> Result<Self> {
        debug!("Generating lockfile from installed packages");

        let state = InstallState::new()?;
        let installed_packages = state.load().await?;

        let mut packages = HashMap::new();
        for (name, pkg) in installed_packages {
            packages.insert(
                name,
                LockfilePackage {
                    version: pkg.version,
                    bottle: pkg.platform,
                },
            );
        }

        let cask_state = CaskState::new()?;
        let installed_casks = cask_state.load().await?;

        let mut casks = HashMap::new();
        for (name, pkg) in installed_casks {
            casks.insert(
                name,
                LockfileCask {
                    version: pkg.version,
                },
            );
        }

        Ok(Self { packages, casks })
    }

    #[instrument(skip(self))]
    pub async fn save(&self, path: &Path) -> Result<()> {
        debug!("Saving lockfile to {:?}", path);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let toml_string = toml::to_string_pretty(&self)
            .map_err(|e| WaxError::LockfileError(format!("Failed to serialize lockfile: {}", e)))?;

        fs::write(path, toml_string).await?;

        debug!("Lockfile saved successfully");
        Ok(())
    }

    #[instrument]
    pub async fn load(path: &Path) -> Result<Self> {
        debug!("Loading lockfile from {:?}", path);

        if !path.exists() {
            return Err(WaxError::LockfileError(
                "Lockfile not found. Run 'wax lock' to generate one.".to_string(),
            ));
        }

        let contents = fs::read_to_string(path).await?;
        let lockfile: Lockfile = toml::from_str(&contents)
            .map_err(|e| WaxError::LockfileError(format!("Failed to parse lockfile: {}", e)))?;

        debug!(
            "Loaded {} packages and {} casks from lockfile",
            lockfile.packages.len(),
            lockfile.casks.len()
        );
        Ok(lockfile)
    }

    pub fn default_path() -> PathBuf {
        match crate::ui::dirs::wax_dir() {
            Ok(dir) => dir.join("wax.lock"),
            Err(e) => {
                warn!(
                    "Could not determine wax config directory: {}; using .wax/ fallback",
                    e
                );
                PathBuf::from(".wax").join("wax.lock")
            }
        }
    }

    pub async fn remove_cask(&mut self, name: &str) {
        self.casks.remove(name);
    }

    pub async fn remove_package(&mut self, name: &str) {
        self.packages.remove(name);
    }
}

impl Default for Lockfile {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_remove_cask() {
        let mut lockfile = Lockfile::new();
        lockfile.casks.insert(
            "brave-browser".to_string(),
            LockfileCask {
                version: "1.0.0".to_string(),
            },
        );
        lockfile.remove_cask("brave-browser").await;
        assert!(lockfile.casks.is_empty());
    }

    #[tokio::test]
    async fn test_remove_package() {
        let mut lockfile = Lockfile::new();
        lockfile.packages.insert(
            "nginx".to_string(),
            LockfilePackage {
                version: "1.25.0".to_string(),
                bottle: "all".to_string(),
            },
        );
        lockfile.remove_package("nginx").await;
        assert!(lockfile.packages.is_empty());
    }

    #[tokio::test]
    async fn test_remove_nonexistent() {
        let mut lockfile = Lockfile::new();
        lockfile.remove_cask("nonexistent").await;
        lockfile.remove_package("nonexistent").await;
        assert!(lockfile.casks.is_empty());
        assert!(lockfile.packages.is_empty());
    }
}
