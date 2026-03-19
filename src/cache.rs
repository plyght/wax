use crate::api::{ApiClient, Cask, Formula};
use crate::error::Result;
use crate::tap::TapManager;
use crate::ui::{create_spinner, dirs};
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
        let cache_dir = dirs::wax_cache_dir()?;
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

    const STALE_THRESHOLD_SECS: i64 = 3600;

    pub fn is_initialized(&self) -> bool {
        self.formulae_path().exists() && self.casks_path().exists()
    }

    pub async fn ensure_fresh(&self) -> Result<()> {
        if !self.is_initialized() {
            self.auto_init().await?;
            return Ok(());
        }

        let metadata = self.load_metadata().await?;
        let is_stale = match &metadata {
            Some(m) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;
                (now - m.last_updated) > Self::STALE_THRESHOLD_SECS
            }
            None => true,
        };

        if is_stale {
            let spinner = create_spinner("Refreshing index…");
            let api_client = ApiClient::new();

            let (formulae_etag, formulae_last_modified) = metadata
                .as_ref()
                .map(|m| {
                    (
                        m.formulae_etag.as_deref(),
                        m.formulae_last_modified.as_deref(),
                    )
                })
                .unwrap_or((None, None));

            let (casks_etag, casks_last_modified) = metadata
                .as_ref()
                .map(|m| (m.casks_etag.as_deref(), m.casks_last_modified.as_deref()))
                .unwrap_or((None, None));

            let (formulae_result, casks_result) = tokio::join!(
                api_client.fetch_formulae_conditional(formulae_etag, formulae_last_modified),
                api_client.fetch_casks_conditional(casks_etag, casks_last_modified)
            );

            let formulae_fetch = formulae_result?;
            let casks_fetch = casks_result?;

            let formula_count = if let Some(data) = &formulae_fetch.data {
                self.save_formulae(data).await?;
                data.len()
            } else {
                metadata.as_ref().map(|m| m.formula_count).unwrap_or(0)
            };

            let cask_count = if let Some(data) = &casks_fetch.data {
                self.save_casks(data).await?;
                data.len()
            } else {
                metadata.as_ref().map(|m| m.cask_count).unwrap_or(0)
            };

            let new_metadata = CacheMetadata {
                last_updated: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64,
                formula_count,
                cask_count,
                formulae_etag: formulae_fetch
                    .etag
                    .or_else(|| metadata.as_ref().and_then(|m| m.formulae_etag.clone())),
                formulae_last_modified: formulae_fetch.last_modified.or_else(|| {
                    metadata
                        .as_ref()
                        .and_then(|m| m.formulae_last_modified.clone())
                }),
                casks_etag: casks_fetch
                    .etag
                    .or_else(|| metadata.as_ref().and_then(|m| m.casks_etag.clone())),
                casks_last_modified: casks_fetch.last_modified.or_else(|| {
                    metadata
                        .as_ref()
                        .and_then(|m| m.casks_last_modified.clone())
                }),
            };
            self.save_metadata(&new_metadata).await?;

            spinner.finish_and_clear();
        }
        Ok(())
    }

    #[instrument(skip(self, formulae))]
    pub async fn save_formulae(&self, formulae: &[Formula]) -> Result<()> {
        self.ensure_cache_dir().await?;
        let json = serde_json::to_string(formulae)?;
        fs::write(self.formulae_path(), json).await?;
        info!("Saved {} formulae to cache", formulae.len());
        Ok(())
    }

    #[instrument(skip(self, casks))]
    pub async fn save_casks(&self, casks: &[Cask]) -> Result<()> {
        self.ensure_cache_dir().await?;
        let json = serde_json::to_string(casks)?;
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
        let path = self.formulae_path();
        if !path.exists() {
            self.auto_init().await?;
        }
        let json = fs::read_to_string(path).await?;
        let formulae = serde_json::from_str(&json)?;
        Ok(formulae)
    }

    pub async fn load_casks(&self) -> Result<Vec<Cask>> {
        let path = self.casks_path();
        if !path.exists() {
            self.auto_init().await?;
        }
        let json = fs::read_to_string(path).await?;
        let casks = serde_json::from_str(&json)?;
        Ok(casks)
    }

    async fn auto_init(&self) -> Result<()> {
        let spinner = create_spinner("Fetching package index…");

        let api_client = ApiClient::new();

        let (formulae_result, casks_result) = tokio::join!(
            api_client.fetch_formulae_conditional(None, None),
            api_client.fetch_casks_conditional(None, None)
        );

        let formulae_fetch = formulae_result?;
        let casks_fetch = casks_result?;

        if let Some(formulae) = formulae_fetch.data {
            self.save_formulae(&formulae).await?;
        }

        if let Some(casks) = casks_fetch.data {
            self.save_casks(&casks).await?;
        }

        let metadata = CacheMetadata {
            last_updated: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
            formula_count: 0,
            cask_count: 0,
            formulae_etag: formulae_fetch.etag,
            formulae_last_modified: formulae_fetch.last_modified,
            casks_etag: casks_fetch.etag,
            casks_last_modified: casks_fetch.last_modified,
        };
        self.save_metadata(&metadata).await?;

        spinner.finish_and_clear();
        Ok(())
    }

    pub async fn load_metadata(&self) -> Result<Option<CacheMetadata>> {
        if !self.metadata_path().exists() {
            return Ok(None);
        }
        let json = fs::read_to_string(self.metadata_path()).await?;
        let metadata = serde_json::from_str(&json)?;
        Ok(Some(metadata))
    }

    pub async fn invalidate_tap_cache(&self, tap_name: &str) -> Result<()> {
        let path = self.tap_cache_path(tap_name);
        if path.exists() {
            fs::remove_file(&path).await?;
            debug!("Invalidated tap cache for {}", tap_name);
        }
        Ok(())
    }

    pub async fn invalidate_all_tap_caches(&self) -> Result<()> {
        let taps_dir = self.taps_cache_dir();
        if taps_dir.exists() {
            fs::remove_dir_all(&taps_dir).await?;
            debug!("Invalidated all tap caches");
        }
        Ok(())
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
}

impl Default for Cache {
    fn default() -> Self {
        Self::new().expect("Failed to initialize cache")
    }
}
