use crate::api::{Cask, Formula};
use crate::error::{Result, WaxError};
use crate::tap::TapManager;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use tracing::{debug, info, instrument};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMetadata {
    pub last_updated: i64,
    pub formula_count: usize,
    pub cask_count: usize,
    pub formulae_etag: Option<String>,
    pub formulae_last_modified: Option<String>,
    pub casks_etag: Option<String>,
    pub casks_last_modified: Option<String>,
}

pub struct Cache {
    cache_dir: PathBuf,
}

impl Cache {
    pub fn new() -> Result<Self> {
        let cache_dir = if let Some(base_dirs) = directories::BaseDirs::new() {
            base_dirs.cache_dir().join("wax")
        } else {
            dirs::home_dir()
                .ok_or_else(|| WaxError::CacheError("Cannot determine home directory".into()))?
                .join(".wax")
                .join("cache")
        };

        Ok(Self { cache_dir })
    }

    pub async fn ensure_cache_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.cache_dir).await?;
        Ok(())
    }

    fn formulae_path(&self) -> PathBuf {
        self.cache_dir.join("formulae.json")
    }

    fn casks_path(&self) -> PathBuf {
        self.cache_dir.join("casks.json")
    }

    fn metadata_path(&self) -> PathBuf {
        self.cache_dir.join("metadata.json")
    }

    fn taps_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("taps")
    }

    fn tap_cache_path(&self, tap_name: &str) -> PathBuf {
        self.taps_cache_dir()
            .join(format!("{}.json", tap_name.replace('/', "-")))
    }

    pub fn is_initialized(&self) -> bool {
        self.formulae_path().exists() && self.casks_path().exists()
    }

    #[instrument(skip(self, formulae))]
    pub async fn save_formulae(&self, formulae: &[Formula]) -> Result<()> {
        self.ensure_cache_dir().await?;
        let json = serde_json::to_string_pretty(formulae)?;
        fs::write(self.formulae_path(), json).await?;
        info!("Saved {} formulae to cache", formulae.len());
        Ok(())
    }

    #[instrument(skip(self, casks))]
    pub async fn save_casks(&self, casks: &[Cask]) -> Result<()> {
        self.ensure_cache_dir().await?;
        let json = serde_json::to_string_pretty(casks)?;
        fs::write(self.casks_path(), json).await?;
        info!("Saved {} casks to cache", casks.len());
        Ok(())
    }

    pub async fn save_metadata(&self, metadata: &CacheMetadata) -> Result<()> {
        self.ensure_cache_dir().await?;
        let json = serde_json::to_string_pretty(metadata)?;
        fs::write(self.metadata_path(), json).await?;
        Ok(())
    }

    pub async fn load_formulae(&self) -> Result<Vec<Formula>> {
        let json = fs::read_to_string(self.formulae_path()).await?;
        let formulae = serde_json::from_str(&json)?;
        Ok(formulae)
    }

    pub async fn load_casks(&self) -> Result<Vec<Cask>> {
        let json = fs::read_to_string(self.casks_path()).await?;
        let casks = serde_json::from_str(&json)?;
        Ok(casks)
    }

    pub async fn load_metadata(&self) -> Result<Option<CacheMetadata>> {
        if !self.metadata_path().exists() {
            return Ok(None);
        }
        let json = fs::read_to_string(self.metadata_path()).await?;
        let metadata = serde_json::from_str(&json)?;
        Ok(Some(metadata))
    }

    pub async fn load_all_formulae(&self) -> Result<Vec<Formula>> {
        let mut all = self.load_formulae().await?;

        let mut tap_manager = TapManager::new()?;
        tap_manager.load().await?;

        for tap in tap_manager.list_taps() {
            let tap_cache_path = self.tap_cache_path(&tap.full_name);

            let tap_formulae = if tap_cache_path.exists() {
                debug!(
                    "Loading tap formulae from cache: {}",
                    tap_cache_path.display()
                );
                let json = fs::read_to_string(&tap_cache_path).await?;
                serde_json::from_str(&json)?
            } else {
                debug!("Loading tap formulae from filesystem: {}", tap.full_name);
                let formulae = tap_manager.load_formulae_from_tap(tap).await?;

                fs::create_dir_all(self.taps_cache_dir()).await?;
                let json = serde_json::to_string_pretty(&formulae)?;
                fs::write(&tap_cache_path, json).await?;

                formulae
            };

            all.extend(tap_formulae);
        }

        Ok(all)
    }

    pub async fn invalidate_tap_cache(&self, tap_name: &str) -> Result<()> {
        let tap_cache_path = self.tap_cache_path(tap_name);
        if tap_cache_path.exists() {
            fs::remove_file(&tap_cache_path).await?;
            info!("Invalidated tap cache: {}", tap_name);
        }
        Ok(())
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self::new().expect("Failed to initialize cache")
    }
}

mod dirs {
    use std::path::PathBuf;

    pub fn home_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}
