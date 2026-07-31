use crate::api::{Cask, CaskDetails, FetchResult, Formula, CASK_API_URL, FORMULA_API_URL};
use crate::error::{Result, WaxError};
use crate::tap::TapManager;
use crate::ui::dirs;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::fs;
use tracing::{debug, info, instrument};

/// Magic prefix for the bincode index sidecars so format/version mismatches
/// (e.g. from an older wax writing a different struct layout) fall back to JSON.
const INDEX_BIN_MAGIC: &[u8] = b"WAXBIN1\0";

fn encode_index<T: Serialize>(items: &[T]) -> Result<Vec<u8>> {
    let mut payload = INDEX_BIN_MAGIC.to_vec();
    bincode::serialize_into(&mut payload, items)
        .map_err(|e| WaxError::CacheError(format!("bincode encode: {e}")))?;
    Ok(payload)
}

fn decode_index<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<T> {
    if !payload.starts_with(INDEX_BIN_MAGIC) {
        return Err(WaxError::CacheError(
            "index cache format mismatch".to_string(),
        ));
    }
    bincode::deserialize(&payload[INDEX_BIN_MAGIC.len()..])
        .map_err(|e| WaxError::CacheError(format!("bincode decode: {e}")))
}

struct FormulaeIndexCache {
    signature: u64,
    formulae: Arc<Vec<Formula>>,
}

static FORMULAE_INDEX_CACHE: Mutex<Option<FormulaeIndexCache>> = Mutex::new(None);

struct CasksIndexCache {
    signature: u64,
    casks: Arc<Vec<Cask>>,
}

static CASKS_INDEX_CACHE: Mutex<Option<CasksIndexCache>> = Mutex::new(None);

fn clear_formulae_index_cache() {
    if let Ok(mut guard) = FORMULAE_INDEX_CACHE.lock() {
        *guard = None;
    }
}

fn clear_casks_index_cache() {
    if let Ok(mut guard) = CASKS_INDEX_CACHE.lock() {
        *guard = None;
    }
}

async fn formulae_index_signature(cache: &Cache, tap_names: &[String]) -> Result<u64> {
    let mut hasher = DefaultHasher::new();
    if let Ok(meta) = fs::metadata(cache.formulae_path()).await {
        if let Ok(mtime) = meta.modified() {
            mtime.hash(&mut hasher);
        }
    }
    for tap_name in tap_names {
        tap_name.hash(&mut hasher);
        let path = cache.tap_cache_path(tap_name);
        if let Ok(meta) = fs::metadata(&path).await {
            if let Ok(mtime) = meta.modified() {
                mtime.hash(&mut hasher);
            }
        }
    }
    Ok(hasher.finish())
}

async fn casks_index_signature(cache: &Cache, tap_names: &[String]) -> Result<u64> {
    let mut hasher = DefaultHasher::new();
    if let Ok(meta) = fs::metadata(cache.casks_path()).await {
        if let Ok(mtime) = meta.modified() {
            mtime.hash(&mut hasher);
        }
    }
    for tap_name in tap_names {
        tap_name.hash(&mut hasher);
        let path = cache.tap_casks_cache_path(tap_name);
        if let Ok(meta) = fs::metadata(&path).await {
            if let Ok(mtime) = meta.modified() {
                mtime.hash(&mut hasher);
            }
        }
    }
    Ok(hasher.finish())
}

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

#[derive(Clone)]
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

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn cache_dir_path(&self) -> &Path {
        &self.cache_dir
    }

    fn formulae_path(&self) -> PathBuf {
        self.cache_dir.join("formulae.json")
    }

    fn formulae_bin_path(&self) -> PathBuf {
        self.cache_dir.join("formulae.bin")
    }

    fn casks_path(&self) -> PathBuf {
        self.cache_dir.join("casks.json")
    }

    fn casks_bin_path(&self) -> PathBuf {
        self.cache_dir.join("casks.bin")
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

    fn tap_casks_cache_path(&self, tap_name: &str) -> PathBuf {
        self.taps_cache_dir()
            .join(format!("{}-casks.json", tap_name.replace('/', "-")))
    }

    const STALE_THRESHOLD_SECS: i64 = 3600;

    pub fn is_initialized(&self) -> bool {
        self.formulae_path().exists() && self.casks_path().exists()
    }

    fn index_progress_bars() -> (MultiProgress, ProgressBar, ProgressBar) {
        let multi = MultiProgress::new();
        let formulae_pb = multi.add(ProgressBar::new(0));
        let casks_pb = multi.add(ProgressBar::new(0));
        for (pb, label) in [(&formulae_pb, "formulae.json"), (&casks_pb, "casks.json")] {
            pb.set_message(label.to_string());
            pb.set_style(
                ProgressStyle::default_bar()
                    .template(
                        "{spinner:.green} {msg} {bar:40.cyan/blue} {bytes}/{total} {percent}%",
                    )
                    .unwrap(),
            );
            pb.enable_steady_tick(Duration::from_millis(80));
        }
        (multi, formulae_pb, casks_pb)
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
                    .unwrap_or_default()
                    .as_secs() as i64;
                (now - m.last_updated) > Self::STALE_THRESHOLD_SECS
            }
            None => true,
        };

        if is_stale {
            let (_multi, formulae_pb, casks_pb) = Self::index_progress_bars();

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
                self.fetch_formulae_conditional(
                    formulae_etag,
                    formulae_last_modified,
                    Some(&formulae_pb),
                ),
                self.fetch_casks_conditional(casks_etag, casks_last_modified, Some(&casks_pb))
            );
            formulae_pb.finish_and_clear();
            casks_pb.finish_and_clear();

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
                    .unwrap_or_default()
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
        }
        Ok(())
    }

    #[instrument(skip(self, formulae))]
    pub async fn save_formulae(&self, formulae: &[Formula]) -> Result<()> {
        self.ensure_cache_dir().await?;
        let json = serde_json::to_string(formulae)?;
        let tmp = self.formulae_path().with_extension("json.tmp");
        fs::write(&tmp, json).await?;
        fs::rename(&tmp, self.formulae_path()).await?;
        if let Ok(payload) = encode_index(formulae) {
            let _ = fs::write(self.formulae_bin_path(), payload).await;
        }
        clear_formulae_index_cache();
        info!("Saved {} formulae to cache", formulae.len());
        Ok(())
    }

    #[instrument(skip(self, casks))]
    pub async fn save_casks(&self, casks: &[Cask]) -> Result<()> {
        self.ensure_cache_dir().await?;
        let json = serde_json::to_string(casks)?;
        let tmp = self.casks_path().with_extension("json.tmp");
        fs::write(&tmp, json).await?;
        fs::rename(&tmp, self.casks_path()).await?;
        if let Ok(payload) = encode_index(casks) {
            let _ = fs::write(self.casks_bin_path(), payload).await;
        }
        info!("Saved {} casks to cache", casks.len());
        Ok(())
    }

    pub async fn save_metadata(&self, metadata: &CacheMetadata) -> Result<()> {
        self.ensure_cache_dir().await?;
        let json = serde_json::to_string_pretty(metadata)?;
        fs::write(self.metadata_path(), json).await?;
        Ok(())
    }

    /// Load an index, preferring the bincode sidecar when it is at least as
    /// fresh as the JSON (avoids re-parsing ~17MB of JSON every process).
    async fn load_index<T: serde::de::DeserializeOwned>(
        &self,
        json_path: &Path,
        bin_path: &Path,
    ) -> Result<Vec<T>> {
        if !json_path.exists() {
            self.auto_init().await?;
        }
        if let Ok(bin_meta) = fs::metadata(bin_path).await {
            let bin_mtime = bin_meta
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            if let Ok(json_meta) = fs::metadata(json_path).await {
                let json_mtime = json_meta
                    .modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                if bin_mtime >= json_mtime {
                    if let Ok(payload) = fs::read(bin_path).await {
                        if let Ok(items) = decode_index::<Vec<T>>(&payload) {
                            return Ok(items);
                        }
                    }
                }
            }
        }
        let json = fs::read_to_string(json_path).await?;
        Ok(serde_json::from_str(&json)?)
    }

    pub async fn load_formulae(&self) -> Result<Vec<Formula>> {
        self.load_index(&self.formulae_path(), &self.formulae_bin_path())
            .await
    }

    pub async fn load_casks(&self) -> Result<Vec<Cask>> {
        self.load_index(&self.casks_path(), &self.casks_bin_path())
            .await
    }

    async fn auto_init(&self) -> Result<()> {
        let (_multi, formulae_pb, casks_pb) = Self::index_progress_bars();

        let (formulae_result, casks_result) = tokio::join!(
            self.fetch_formulae_conditional(None, None, Some(&formulae_pb)),
            self.fetch_casks_conditional(None, None, Some(&casks_pb))
        );
        formulae_pb.finish_and_clear();
        casks_pb.finish_and_clear();

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
                .unwrap_or_default()
                .as_secs() as i64,
            formula_count: 0,
            cask_count: 0,
            formulae_etag: formulae_fetch.etag,
            formulae_last_modified: formulae_fetch.last_modified,
            casks_etag: casks_fetch.etag,
            casks_last_modified: casks_fetch.last_modified,
        };
        self.save_metadata(&metadata).await?;

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
        let casks_path = self.tap_casks_cache_path(tap_name);
        if casks_path.exists() {
            fs::remove_file(&casks_path).await?;
            debug!("Invalidated tap casks cache for {}", tap_name);
        }
        clear_formulae_index_cache();
        clear_casks_index_cache();
        Ok(())
    }

    pub async fn invalidate_all_tap_caches(&self) -> Result<()> {
        let taps_dir = self.taps_cache_dir();
        if taps_dir.exists() {
            fs::remove_dir_all(&taps_dir).await?;
            debug!("Invalidated all tap caches");
        }
        clear_formulae_index_cache();
        clear_casks_index_cache();
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn fetch_formulae_conditional(
        &self,
        etag: Option<&str>,
        last_modified: Option<&str>,
        progress: Option<&ProgressBar>,
    ) -> Result<FetchResult<Vec<Formula>>> {
        info!("Fetching formulae from API with conditional headers");
        let client = crate::http_client::api();
        let mut request = client.get(FORMULA_API_URL);

        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        if let Some(last_modified) = last_modified {
            request = request.header("If-Modified-Since", last_modified);
        }

        let mut response = request.send().await?;

        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            info!("Formulae not modified (304)");
            return Ok(FetchResult {
                data: None,
                etag: None,
                last_modified: None,
                not_modified: true,
            });
        }

        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let last_modified = response
            .headers()
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let total = response.content_length();
        if let Some(pb) = progress {
            pb.set_message("formulae.json");
            if let Some(len) = total {
                pb.set_length(len);
            }
        }

        let mut body = Vec::with_capacity(total.unwrap_or(0).min(512 * 1024 * 1024) as usize);
        while let Some(chunk) = response.chunk().await? {
            if let Some(pb) = progress {
                pb.inc(chunk.len() as u64);
            }
            body.extend_from_slice(&chunk);
        }
        let formulae: Vec<Formula> = serde_json::from_slice(&body)?;
        info!("Fetched {} formulae", formulae.len());

        Ok(FetchResult {
            data: Some(formulae),
            etag,
            last_modified,
            not_modified: false,
        })
    }

    #[instrument(skip(self))]
    pub async fn fetch_casks_conditional(
        &self,
        etag: Option<&str>,
        last_modified: Option<&str>,
        progress: Option<&ProgressBar>,
    ) -> Result<FetchResult<Vec<Cask>>> {
        info!("Fetching casks from API with conditional headers");
        let client = crate::http_client::api();
        let mut request = client.get(CASK_API_URL);

        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        if let Some(last_modified) = last_modified {
            request = request.header("If-Modified-Since", last_modified);
        }

        let mut response = request.send().await?;

        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            info!("Casks not modified (304)");
            return Ok(FetchResult {
                data: None,
                etag: None,
                last_modified: None,
                not_modified: true,
            });
        }

        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let last_modified = response
            .headers()
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let total = response.content_length();
        if let Some(pb) = progress {
            pb.set_message("casks.json");
            if let Some(len) = total {
                pb.set_length(len);
            }
        }

        let mut body = Vec::with_capacity(total.unwrap_or(0).min(512 * 1024 * 1024) as usize);
        while let Some(chunk) = response.chunk().await? {
            if let Some(pb) = progress {
                pb.inc(chunk.len() as u64);
            }
            body.extend_from_slice(&chunk);
        }
        let casks: Vec<Cask> = serde_json::from_slice(&body)?;
        info!("Fetched {} casks", casks.len());

        Ok(FetchResult {
            data: Some(casks),
            etag,
            last_modified,
            not_modified: false,
        })
    }

    fn cask_api_token(cask_name: &str) -> &str {
        let parts: Vec<&str> = cask_name.split('/').collect();
        if parts.len() >= 3 {
            parts[parts.len() - 1]
        } else {
            cask_name
        }
    }

    async fn fetch_cask_details_from_rb_path(rb_path: &Path) -> Result<CaskDetails> {
        let token = rb_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                crate::error::WaxError::ParseError("Invalid cask file path".to_string())
            })?;
        let content = fs::read_to_string(rb_path).await?;
        crate::formula_parser::FormulaParser::parse_ruby_cask_details(token, &content)
    }

    pub async fn fetch_cask_details_from_index(
        &self,
        casks: &[Cask],
        cask_name: &str,
    ) -> Result<CaskDetails> {
        crate::error::validate_package_name(cask_name)?;
        if let Some(summary) = casks
            .iter()
            .find(|c| c.token == cask_name || c.full_token == cask_name)
        {
            if let Some(ref rb_path) = summary.rb_path {
                info!("Loading cask details from tap: {}", rb_path.display());
                return Self::fetch_cask_details_from_rb_path(rb_path).await;
            }
        }

        let api_token = Self::cask_api_token(cask_name);
        info!("Fetching details for cask: {}", api_token);
        let client = crate::http_client::api();
        let url = format!("https://formulae.brew.sh/api/cask/{}.json", api_token);
        let response = client.get(&url).send().await?;
        let cask: CaskDetails = response.json().await?;
        info!("Fetched details for cask: {}", api_token);
        Ok(cask)
    }

    #[instrument(skip(self))]
    pub async fn fetch_cask_details(&self, cask_name: &str) -> Result<CaskDetails> {
        let casks = self.load_all_casks().await?;
        self.fetch_cask_details_from_index(&casks, cask_name).await
    }

    pub async fn load_all_casks(&self) -> Result<Vec<Cask>> {
        let mut tap_manager = TapManager::new()?;
        tap_manager.load().await?;

        let tap_names: Vec<String> = tap_manager
            .list_taps()
            .into_iter()
            .map(|tap| tap.full_name.clone())
            .collect();
        let signature = casks_index_signature(self, &tap_names).await?;
        if let Ok(guard) = CASKS_INDEX_CACHE.lock() {
            if let Some(cached) = guard.as_ref() {
                if cached.signature == signature {
                    debug!("Using in-process casks index cache");
                    return Ok((*cached.casks).clone());
                }
            }
        }

        let mut all = self.load_casks().await?;

        for tap in tap_manager.list_taps() {
            let tap_cache_path = self.tap_casks_cache_path(&tap.full_name);

            let tap_casks = if tap_cache_path.exists() {
                debug!("Loading tap casks from cache: {}", tap_cache_path.display());
                let cask_dir = tap.cask_dir();
                let json = fs::read_to_string(&tap_cache_path).await?;
                let mut casks: Vec<Cask> = serde_json::from_str(&json)?;
                for c in &mut casks {
                    let rb_file = cask_dir.join(format!("{}.rb", c.token));
                    if rb_file.exists() {
                        c.rb_path = Some(rb_file);
                    }
                }
                casks
            } else {
                debug!("Loading tap casks from filesystem: {}", tap.full_name);
                let casks = tap_manager.load_casks_from_tap(tap).await?;

                fs::create_dir_all(self.taps_cache_dir()).await?;
                let json = serde_json::to_string_pretty(&casks)?;
                fs::write(&tap_cache_path, json).await?;

                casks
            };

            all.extend(tap_casks);
        }

        if let Ok(mut guard) = CASKS_INDEX_CACHE.lock() {
            *guard = Some(CasksIndexCache {
                signature,
                casks: Arc::new(all.clone()),
            });
        }

        Ok(all)
    }

    pub async fn load_all_formulae(&self) -> Result<Vec<Formula>> {
        let mut tap_manager = TapManager::new()?;
        tap_manager.load().await?;

        let tap_names: Vec<String> = tap_manager
            .list_taps()
            .into_iter()
            .map(|tap| tap.full_name.clone())
            .collect();
        let signature = formulae_index_signature(self, &tap_names).await?;
        if let Ok(guard) = FORMULAE_INDEX_CACHE.lock() {
            if let Some(cached) = guard.as_ref() {
                if cached.signature == signature {
                    debug!("Using in-process formulae index cache");
                    return Ok((*cached.formulae).clone());
                }
            }
        }

        let mut all = self.load_formulae().await?;

        for tap in tap_manager.list_taps() {
            let tap_cache_path = self.tap_cache_path(&tap.full_name);

            let tap_formulae = if tap_cache_path.exists() {
                debug!(
                    "Loading tap formulae from cache: {}",
                    tap_cache_path.display()
                );
                let formula_dir = tap.formula_dir();
                let json = fs::read_to_string(&tap_cache_path).await?;
                let mut formulae: Vec<Formula> = serde_json::from_str(&json)?;
                // rb_path is skipped during serialisation — restore it from the filesystem.
                for f in &mut formulae {
                    let rb_file = formula_dir.join(format!("{}.rb", f.name));
                    if rb_file.exists() {
                        f.rb_path = Some(rb_file);
                    }
                }
                formulae
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

        if let Ok(mut guard) = FORMULAE_INDEX_CACHE.lock() {
            *guard = Some(FormulaeIndexCache {
                signature,
                formulae: Arc::new(all.clone()),
            });
        }

        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_bin_roundtrip() {
        let items = vec!["a".to_string(), "b".to_string()];
        let payload = encode_index(&items).unwrap();
        let decoded: Vec<String> = decode_index(&payload).unwrap();
        assert_eq!(decoded, items);
        assert!(decode_index::<Vec<String>>(b"nope").is_err());
        assert!(decode_index::<Vec<String>>(b"WAXBIN1\0garbage").is_err());
    }

    #[test]
    fn cache_metadata_serializes_roundtrip() {
        let meta = CacheMetadata {
            last_updated: 1_700_000_000,
            formula_count: 8100,
            cask_count: 7500,
            formulae_etag: Some("\"abc123\"".to_string()),
            formulae_last_modified: Some("Thu, 01 Jan 2026 00:00:00 GMT".to_string()),
            casks_etag: None,
            casks_last_modified: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let decoded: CacheMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.last_updated, meta.last_updated);
        assert_eq!(decoded.formula_count, meta.formula_count);
        assert_eq!(decoded.formulae_etag, meta.formulae_etag);
        assert_eq!(decoded.casks_etag, None);
    }

    #[test]
    fn unix_timestamp_is_positive() {
        // Sanity check: our timestamp helper produces a sane positive value.
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        // Must be > 2020-01-01 (Unix time 1577836800)
        assert!(ts > 1_577_836_800, "timestamp looks wrong: {ts}");
    }

    #[test]
    fn stale_threshold_constant_is_one_hour() {
        assert_eq!(Cache::STALE_THRESHOLD_SECS, 3600);
    }
}
