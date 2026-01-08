use crate::api::{Cask, Formula};
use crate::error::{Result, WaxError};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use tracing::{info, instrument};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMetadata {
    pub last_updated: i64,
    pub formula_count: usize,
    pub cask_count: usize,
}

pub struct Cache {
    cache_dir: PathBuf,
}

impl Cache {
    pub fn new() -> Result<Self> {
        let cache_dir = dirs::home_dir()
            .ok_or_else(|| WaxError::CacheError("Cannot determine home directory".into()))?
            .join(".wax")
            .join("cache");

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
        match fs::read_to_string(self.metadata_path()).await {
            Ok(json) => {
                let metadata = serde_json::from_str(&json)?;
                Ok(Some(metadata))
            }
            Err(_) => Ok(None),
        }
    }

    pub async fn is_stale(&self, max_age_secs: i64) -> bool {
        match self.load_metadata().await {
            Ok(Some(metadata)) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;
                now - metadata.last_updated > max_age_secs
            }
            _ => true,
        }
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
