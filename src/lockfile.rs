use crate::error::{Result, WaxError};
use crate::install::InstallState;
use crate::cask::CaskState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, instrument};

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

        debug!("Loaded {} packages and {} casks from lockfile", lockfile.packages.len(), lockfile.casks.len());
        Ok(lockfile)
    }

    pub fn default_path() -> PathBuf {
        crate::ui::dirs::wax_dir()
            .unwrap_or_else(|_| PathBuf::from(".wax"))
            .join("wax.lock")
    }
}

impl Default for Lockfile {
    fn default() -> Self {
        Self::new()
    }
}
