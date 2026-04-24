use crate::bottle::{homebrew_prefix, BottleDownloader, DownloadTotals};
use crate::error::{Result, WaxError};
use crate::ui::dirs;
use indicatif::ProgressBar;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tracing::{debug, info, instrument};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledCask {
    pub name: String,
    pub version: String,
    pub install_date: i64,
    #[serde(default)]
    pub artifact_type: Option<String>,
    #[serde(default)]
    pub binary_paths: Option<Vec<String>>,
    #[serde(default)]
    pub app_name: Option<String>,
}

pub struct CaskState {
    // Keep a path to legacy state for migration/fallback if needed, but primarily use Caskroom
    legacy_state_path: PathBuf,
}

fn temp_path_for(path: &Path) -> PathBuf {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("installed_casks.json");
    path.with_file_name(format!(".{}.{}.{}.tmp", file_name, pid, nanos))
}

fn normalize_existing_prefix(path: &Path) -> PathBuf {
    if let Ok(normalized) = dunce::canonicalize(path) {
        return normalized;
    }

    let mut suffix = PathBuf::new();
    let mut current = path;
    while let Some(parent) = current.parent() {
        if let Some(name) = current.file_name() {
            suffix = Path::new(name).join(suffix);
        }
        if let Ok(normalized_parent) = dunce::canonicalize(parent) {
            return normalized_parent.join(suffix);
        }
        current = parent;
    }

    path.to_path_buf()
}

impl CaskState {
    pub fn new() -> Result<Self> {
        let legacy_state_path = dirs::wax_dir()?.join("installed_casks.json");
        Ok(Self { legacy_state_path })
    }

    pub fn caskroom_dir() -> PathBuf {
        homebrew_prefix().join("Caskroom")
    }

    pub fn user_caskroom_dir() -> Result<PathBuf> {
        Ok(dirs::home_dir()?
            .join(".local")
            .join("wax")
            .join("Caskroom"))
    }

    pub async fn load(&self) -> Result<HashMap<String, InstalledCask>> {
        let mut casks = HashMap::new();

        // Load only from legacy state file - NOT from Caskroom directories
        // This ensures we only show casks that were explicitly tracked
        if self.legacy_state_path.exists() {
            if let Ok(json) = fs::read_to_string(&self.legacy_state_path).await {
                if let Ok(legacy_casks) =
                    serde_json::from_str::<HashMap<String, InstalledCask>>(&json)
                {
                    casks.extend(legacy_casks);
                }
            }
        }

        Ok(casks)
    }

    #[allow(dead_code)]
    async fn scan_cask_version_dir(&self, cask_path: &Path) -> Result<(String, i64)> {
        let mut version = "unknown".to_string();
        let mut install_date = 0;

        let mut ver_entries = tokio::fs::read_dir(cask_path).await?;
        while let Some(ver_entry) = ver_entries.next_entry().await? {
            let ver_name = ver_entry.file_name().to_string_lossy().to_string();
            if ver_name.starts_with('.') {
                continue;
            }

            let t = ver_entry.file_type().await?;
            if t.is_dir() {
                version = ver_name;
                if let Ok(metadata) = ver_entry.metadata().await {
                    if let Ok(modified) = metadata.modified() {
                        if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                            install_date = duration.as_secs() as i64;
                        }
                    }
                }
                break;
            }
        }
        Ok((version, install_date))
    }

    pub async fn save(&self, casks: &HashMap<String, InstalledCask>) -> Result<()> {
        let parent = self
            .legacy_state_path
            .parent()
            .ok_or_else(|| WaxError::CacheError("Cannot determine parent directory".into()))?;
        fs::create_dir_all(parent).await?;

        let json = serde_json::to_string_pretty(casks)?;
        let temp_path = temp_path_for(&self.legacy_state_path);
        fs::write(&temp_path, json).await?;
        fs::rename(&temp_path, &self.legacy_state_path)
            .await
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&temp_path);
            })?;
        Ok(())
    }

    pub async fn add(&self, cask: InstalledCask) -> Result<()> {
        let mut casks = self.load().await?;

        // Also create Caskroom structure
        let caskroom = Self::caskroom_dir();
        let cask_dir = caskroom.join(&cask.name);
        let version_dir = cask_dir.join(&cask.version);
        fs::create_dir_all(&version_dir).await?;

        // Try to create symlinks inside version_dir based on app_name or binary_paths
        if let Some(app_name) = &cask.app_name {
            let app_path = PathBuf::from("/Applications").join(app_name);
            let link_path = version_dir.join(app_name);
            if app_path.exists() && !link_path.exists() {
                #[cfg(unix)]
                if let Err(e) = tokio::fs::symlink(&app_path, &link_path).await {
                    tracing::warn!(
                        "Failed to create Caskroom symlink {:?} -> {:?}: {}",
                        link_path,
                        app_path,
                        e
                    );
                }
            }
        }

        casks.insert(cask.name.clone(), cask);
        self.save(&casks).await?;
        Ok(())
    }

    pub async fn remove(&self, name: &str) -> Result<()> {
        let mut casks = self.load().await?;

        let caskroom = Self::caskroom_dir();
        let cask_dir = caskroom.join(name);
        if cask_dir.exists() {
            let _ = fs::remove_dir_all(&cask_dir).await;
        }

        if let Ok(user_dir) = Self::user_caskroom_dir() {
            let user_cask_dir = user_dir.join(name);
            if user_cask_dir.exists() {
                let _ = fs::remove_dir_all(&user_cask_dir).await;
            }
        }

        casks.remove(name);
        self.save(&casks).await?;
        Ok(())
    }
}

async fn installed_cask_version_dir(cask: &InstalledCask) -> Result<Option<PathBuf>> {
    let mut candidates = vec![CaskState::caskroom_dir()
        .join(&cask.name)
        .join(&cask.version)];
    if let Ok(user_dir) = CaskState::user_caskroom_dir() {
        candidates.push(user_dir.join(&cask.name).join(&cask.version));
    }

    for candidate in candidates {
        if candidate.exists() {
            return Ok(Some(candidate));
        }
    }

    Ok(None)
}

async fn replace_path_with_link(source: &Path, dest: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(dest).await {
        let file_type = metadata.file_type();
        if file_type.is_symlink() || file_type.is_file() {
            fs::remove_file(dest).await.ok();
        } else if file_type.is_dir() {
            fs::remove_dir_all(dest).await.ok();
        }
    }

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).await?;
    }

    #[cfg(unix)]
    {
        tokio::fs::symlink(source, dest).await?;
    }
    #[cfg(not(unix))]
    {
        if source.is_dir() {
            crate::ui::copy_dir_all(source, dest)?;
        } else {
            fs::copy(source, dest).await?;
        }
    }

    Ok(())
}

async fn remove_path_if_present(path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path).await {
        let file_type = metadata.file_type();
        if file_type.is_dir() && !file_type.is_symlink() {
            fs::remove_dir_all(path).await.ok();
        } else {
            fs::remove_file(path).await.ok();
        }
    }

    Ok(())
}

pub async fn relink_installed_cask(cask: &InstalledCask) -> Result<Vec<PathBuf>> {
    let mut links = Vec::new();
    let Some(version_dir) = installed_cask_version_dir(cask).await? else {
        return Ok(links);
    };

    if let Some(app_name) = &cask.app_name {
        #[cfg(target_os = "macos")]
        {
            let app_path = PathBuf::from("/Applications").join(app_name);
            let link_path = version_dir.join(app_name);
            if app_path.exists() {
                replace_path_with_link(&app_path, &link_path).await?;
                links.push(link_path);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let link_path = version_dir.join(app_name);
            if link_path.exists() {
                links.push(link_path);
            }
        }
    }

    if let Some(binary_paths) = &cask.binary_paths {
        for binary_path in binary_paths {
            let dest = PathBuf::from(binary_path);
            let Some(name) = dest.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let source = version_dir.join(name);
            if source.exists() {
                replace_path_with_link(&source, &dest).await?;
                links.push(dest);
            }
        }
    }

    Ok(links)
}

pub async fn unlink_installed_cask(cask: &InstalledCask) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    let version_dir = installed_cask_version_dir(cask).await?;

    if let Some(app_name) = &cask.app_name {
        if let Some(version_dir) = &version_dir {
            let link_path = version_dir.join(app_name);
            if link_path.exists() {
                remove_path_if_present(&link_path).await?;
                removed.push(link_path);
            }
        }
    }

    if let Some(binary_paths) = &cask.binary_paths {
        for binary_path in binary_paths {
            let dest = PathBuf::from(binary_path);
            if dest.exists() {
                remove_path_if_present(&dest).await?;
                removed.push(dest);
            }
        }
    }

    Ok(removed)
}

impl Default for CaskState {
    fn default() -> Self {
        Self::new().expect("Failed to initialize cask state")
    }
}

pub struct StagingContext {
    pub staging_root: PathBuf,
    mount_point: Option<PathBuf>,
    _temp_dir: Option<tempfile::TempDir>,
}

pub struct RollbackContext {
    installed_paths: Vec<PathBuf>,
    committed: bool,
}

impl RollbackContext {
    pub fn new() -> Self {
        Self {
            installed_paths: Vec::new(),
            committed: false,
        }
    }

    pub fn add(&mut self, path: PathBuf) {
        self.installed_paths.push(path);
    }

    pub fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for RollbackContext {
    fn drop(&mut self) {
        if !self.committed && !self.installed_paths.is_empty() {
            crate::signal::println_through_active_multi(format!(
                "  ⚠️  rolling back {} partially installed artifact(s)...",
                self.installed_paths.len()
            ));
            for path in &self.installed_paths {
                if path.exists() {
                    if path.is_dir() {
                        let _ = std::fs::remove_dir_all(path);
                    } else {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        }
    }
}

/// Maximum allowed size for a cask staging directory after extraction (5 GB).
const MAX_STAGING_SIZE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MAX_STAGING_DIRS_VISITED: usize = 100_000;
const MAX_STAGING_ENTRIES_VISITED: usize = 1_000_000;

fn dir_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut dirs_to_visit = vec![path.to_path_buf()];
    let mut dirs_visited = 0usize;
    let mut entries_visited = 0usize;

    while let Some(dir) = dirs_to_visit.pop() {
        dirs_visited += 1;
        if dirs_visited > MAX_STAGING_DIRS_VISITED {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "staging directory scan exceeded directory traversal limit",
            ));
        }

        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            entries_visited += 1;
            if entries_visited > MAX_STAGING_ENTRIES_VISITED {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "staging directory scan exceeded entry traversal limit",
                ));
            }

            let entry_path = entry.path();
            let metadata = std::fs::symlink_metadata(&entry_path)?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                dirs_to_visit.push(entry_path);
            } else if file_type.is_file() {
                total = total.saturating_add(metadata.len());
                if total > MAX_STAGING_SIZE_BYTES {
                    return Ok(total);
                }
            }
        }
    }

    Ok(total)
}

impl StagingContext {
    /// Returns the permanent on-disk directory for this cask version.
    /// For DMG installs the staging root is a temporary mount point; the parent
    /// is the actual version directory that survives after the image is detached.
    pub fn permanent_dir(&self) -> PathBuf {
        match &self.mount_point {
            Some(mp) => mp
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| self.staging_root.clone()),
            None => self.staging_root.clone(),
        }
    }

    pub fn is_mounted(&self) -> bool {
        self.mount_point.is_some()
    }

    pub async fn new_in_dir(
        download_path: &Path,
        artifact_type: &str,
        url: &str,
        target_dir: PathBuf,
    ) -> Result<Self> {
        tokio::fs::create_dir_all(&target_dir).await?;
        Self::new_internal(download_path, artifact_type, url, target_dir, None).await
    }

    async fn new_internal(
        download_path: &Path,
        artifact_type: &str,
        url: &str,
        staging_root: PathBuf,
        temp_dir: Option<tempfile::TempDir>,
    ) -> Result<Self> {
        let mut mount_point = None;

        match artifact_type {
            "dmg" => {
                let mp = staging_root.join("mount");
                tokio::fs::create_dir_all(&mp).await?;

                let attach_output = tokio::process::Command::new("hdiutil")
                    .arg("attach")
                    .arg("-nobrowse")
                    .arg("-quiet")
                    .arg("-mountpoint")
                    .arg(&mp)
                    .arg(download_path)
                    .output()
                    .await?;

                if attach_output.status.success() {
                    mount_point = Some(mp);
                } else {
                    // Some casks use extensionless endpoints that are actually ZIP files.
                    // If DMG mounting fails, try ZIP extraction as a fallback.
                    let unzip_output = tokio::process::Command::new("unzip")
                        .arg("-q")
                        .arg("-o")
                        .arg(download_path)
                        .arg("-d")
                        .arg(&staging_root)
                        .output()
                        .await?;

                    if !unzip_output.status.success() {
                        return Err(WaxError::InstallError(format!(
                            "Failed to mount DMG and fallback unzip failed: {} | {}",
                            String::from_utf8_lossy(&attach_output.stderr),
                            String::from_utf8_lossy(&unzip_output.stderr)
                        )));
                    }
                }
            }
            "zip" => {
                let unzip_output = tokio::process::Command::new("unzip")
                    .arg("-q")
                    .arg("-o")
                    .arg(download_path)
                    .arg("-d")
                    .arg(&staging_root)
                    .output()
                    .await?;

                if !unzip_output.status.success() {
                    return Err(WaxError::InstallError(format!(
                        "Failed to extract ZIP: {}",
                        String::from_utf8_lossy(&unzip_output.stderr)
                    )));
                }
            }
            "tar.gz" | "tar" | "tgz" | "tar.bz2" | "tbz" | "tar.xz" | "txz" => {
                let tar_output = tokio::process::Command::new("tar")
                    .arg("-xf")
                    .arg(download_path)
                    .arg("-C")
                    .arg(&staging_root)
                    .output()
                    .await?;

                if !tar_output.status.success() {
                    return Err(WaxError::InstallError(format!(
                        "Failed to extract tarball: {}",
                        String::from_utf8_lossy(&tar_output.stderr)
                    )));
                }
            }
            _ => {
                // For "pkg" or "binary", copy the file to the staging root, attempting to use its original name
                let original_filename = url
                    .split('?')
                    .next()
                    .unwrap_or(url)
                    .split('/')
                    .next_back()
                    .unwrap_or_else(|| {
                        download_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("download")
                    });

                let decoded_filename = urlencoding::decode(original_filename)
                    .unwrap_or(std::borrow::Cow::Borrowed(original_filename));

                let filename = decoded_filename.as_ref();
                if filename.contains("..") || filename.starts_with("/") || filename.contains("\0") {
                    return Err(WaxError::InstallError(format!(
                        "Filename contains unsafe characters: {}",
                        filename
                    )));
                }

                let dest = staging_root.join(filename);
                tokio::fs::copy(download_path, &dest).await?;
            }
        }

        let actual_staging_root = if let Some(ref mp) = mount_point {
            mp.clone()
        } else {
            staging_root
        };

        // Guard against zip-bomb / archive-bomb resource exhaustion.
        let staging_root_for_size = actual_staging_root.clone();
        match tokio::task::spawn_blocking(move || dir_size(&staging_root_for_size))
            .await
            .map_err(|e| {
                WaxError::InstallError(format!("Failed to scan staging directory: {}", e))
            })? {
            Ok(size) if size > MAX_STAGING_SIZE_BYTES => {
                return Err(WaxError::InstallError(format!(
                    "Extracted cask staging directory exceeds size limit ({} > {} bytes)",
                    size, MAX_STAGING_SIZE_BYTES
                )));
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("Unable to compute staging directory size: {}", e);
            }
        }

        Ok(Self {
            staging_root: actual_staging_root,
            mount_point,
            _temp_dir: temp_dir,
        })
    }
}

impl Drop for StagingContext {
    fn drop(&mut self) {
        if let Some(ref mp) = self.mount_point {
            let _ = std::process::Command::new("hdiutil")
                .arg("detach")
                .arg(mp)
                .arg("-quiet")
                .status();
        }
    }
}

pub struct CaskInstaller {
    downloader: BottleDownloader,
}

impl CaskInstaller {
    pub fn new() -> Self {
        Self {
            downloader: BottleDownloader::new(),
        }
    }

    pub fn applications_dir() -> Result<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            Ok(PathBuf::from("/Applications"))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(dirs::home_dir()?.join("Applications"))
        }
    }

    pub async fn detect_writable_bin_dir() -> Result<PathBuf> {
        let candidates = vec![
            crate::bottle::homebrew_prefix().join("bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/opt/homebrew/bin"),
        ];

        for candidate in candidates {
            if candidate.exists() && Self::is_dir_writable(&candidate).await {
                debug!("Using writable bin directory: {:?}", candidate);
                return Ok(candidate);
            }
        }

        let local_bin = Self::user_bin_dir()?;
        tokio::fs::create_dir_all(&local_bin).await?;
        debug!("Using fallback bin directory: {:?}", local_bin);
        Ok(local_bin)
    }

    fn user_bin_dir() -> Result<PathBuf> {
        Ok(dirs::home_dir()?.join(".local").join("wax").join("bin"))
    }

    async fn is_dir_writable(path: &Path) -> bool {
        let test_file = path.join(".wax_write_test");
        match tokio::fs::File::create(&test_file).await {
            Ok(_) => {
                let _ = tokio::fs::remove_file(&test_file).await;
                true
            }
            Err(_) => false,
        }
    }

    fn resolve_source_path(&self, staging: &StagingContext, source_rel: &str) -> PathBuf {
        let prefix = crate::bottle::homebrew_prefix()
            .to_string_lossy()
            .to_string();
        let staging_str = staging.staging_root.to_str().unwrap_or("");
        let path = source_rel
            .replace("$HOMEBREW_PREFIX", &prefix)
            .replace("#{HOMEBREW_PREFIX}", &prefix)
            .replace("$APPDIR", staging_str);

        let p = Path::new(&path);
        let resolved = if p.is_absolute() {
            p.to_path_buf()
        } else {
            staging.staging_root.join(&path)
        };

        if p.components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            tracing::warn!(
                "Rejecting source path with parent-directory traversal: {} (resolved: {:?})",
                source_rel,
                resolved
            );
            return staging.staging_root.join(
                Path::new(source_rel)
                    .file_name()
                    .unwrap_or(std::ffi::OsStr::new("unknown")),
            );
        }

        // For absolute paths, only allow known-safe directories.
        if p.is_absolute() {
            let allowed_prefixes: Vec<PathBuf> = vec![
                crate::bottle::homebrew_prefix(),
                staging.staging_root.clone(),
                #[cfg(target_os = "macos")]
                PathBuf::from("/Applications"),
                #[cfg(not(target_os = "macos"))]
                dirs::home_dir()
                    .unwrap_or_else(|_| PathBuf::from("/tmp"))
                    .join("Applications"),
            ];
            let normalized_resolved = normalize_existing_prefix(&resolved);
            let is_allowed = allowed_prefixes.iter().any(|allowed| {
                let normalized_allowed = normalize_existing_prefix(allowed);
                normalized_resolved.starts_with(&normalized_allowed)
            });
            if !is_allowed {
                tracing::warn!(
                    "Rejecting absolute source path outside safe directories: {} (resolved: {:?})",
                    source_rel,
                    resolved
                );
                return staging.staging_root.join(
                    Path::new(source_rel)
                        .file_name()
                        .unwrap_or(std::ffi::OsStr::new("unknown")),
                );
            }
            return resolved;
        }

        // For relative paths, normalize and ensure it stays inside staging_root.
        let mut normalized = PathBuf::new();
        for component in resolved.components() {
            match component {
                std::path::Component::ParentDir => {
                    if !normalized.pop() {
                        tracing::warn!(
                            "Rejecting source path that escapes staging root: {} (resolved: {:?})",
                            source_rel,
                            resolved
                        );
                        return staging.staging_root.join(
                            Path::new(source_rel)
                                .file_name()
                                .unwrap_or(std::ffi::OsStr::new("unknown")),
                        );
                    }
                }
                std::path::Component::CurDir => {}
                other => normalized.push(other),
            }
        }

        if !normalized.starts_with(&staging.staging_root) {
            tracing::warn!(
                "Rejecting source path that escapes staging root: {} (normalized: {:?})",
                source_rel,
                normalized
            );
            return staging.staging_root.join(
                Path::new(source_rel)
                    .file_name()
                    .unwrap_or(std::ffi::OsStr::new("unknown")),
            );
        }

        normalized
    }

    /// Probe a URL via HEAD request to detect artifact type from response headers.
    /// Falls back to a ranged GET if HEAD is not supported (e.g. 405).
    /// Returns None if type cannot be determined.
    pub async fn probe_artifact_type(&self, url: &str) -> Option<&'static str> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .ok()?;

        let response = match client.head(url).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => {
                // HEAD rejected — fall back to a tiny ranged GET.
                client
                    .get(url)
                    .header(reqwest::header::RANGE, "bytes=0-0")
                    .send()
                    .await
                    .ok()?
            }
        };
        let final_url = response.url().to_string();

        // Check final URL after redirects
        if let Some(t) = detect_artifact_type(&final_url) {
            return Some(t);
        }

        // Check Content-Disposition header
        if let Some(disposition) = response
            .headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
        {
            if let Some(t) = detect_artifact_type_from_disposition(disposition) {
                return Some(t);
            }
        }

        // Check Content-Type header
        if let Some(ct) = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
        {
            if let Some(t) = detect_artifact_type_from_content_type(ct) {
                return Some(t);
            }
        }

        None
    }

    #[instrument(skip(self, progress, totals))]
    pub async fn download_cask(
        &self,
        url: &str,
        dest_path: &Path,
        progress: Option<&ProgressBar>,
        totals: Option<&DownloadTotals>,
    ) -> Result<()> {
        debug!("Downloading cask from {}", url);
        self.downloader
            .download(
                url,
                dest_path,
                progress,
                BottleDownloader::GLOBAL_CONNECTION_POOL,
                totals,
            )
            .await
    }

    pub fn verify_checksum(path: &Path, expected_sha256: &str) -> Result<()> {
        // Homebrew uses "no_check" to skip checksum verification
        if expected_sha256 == "no_check" {
            debug!("Skipping checksum verification (no_check) for {:?}", path);
            return Ok(());
        }

        debug!("Verifying checksum for {:?}", path);

        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 8192];

        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }

        let hash = format!("{:x}", hasher.finalize());

        if hash != expected_sha256 {
            return Err(WaxError::ChecksumMismatch {
                expected: expected_sha256.to_string(),
                actual: hash,
            });
        }

        debug!("Checksum verified: {}", hash);
        Ok(())
    }

    /// Rejects paths that contain parent-directory traversal components.
    fn reject_traversal(path: &Path) -> Result<()> {
        if path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(WaxError::InstallError(format!(
                "Path contains directory traversal: {}",
                path.display()
            )));
        }
        Ok(())
    }

    #[instrument(skip(self, _staging, _rollback))]
    pub async fn install_app(
        &self,
        _staging: &StagingContext,
        _rollback: &mut RollbackContext,
        source_rel: &str,
    ) -> Result<()> {
        #[cfg(not(target_os = "macos"))]
        {
            debug!("Skipping .app bundle install on non-macOS: {}", source_rel);
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        {
            let source = self.resolve_source_path(_staging, source_rel);
            let app_name = Path::new(source_rel)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    WaxError::InstallError(format!("Invalid app source: {}", source_rel))
                })?;

            info!("Installing app: {}", app_name);

            if !source.exists() {
                return Err(WaxError::InstallError(format!(
                    "App source does not exist: {:?}",
                    source
                )));
            }

            let app_dest = Self::applications_dir()?.join(app_name);

            // Remove existing app bundle before copying (upgrade path)
            if app_dest.exists() {
                tokio::fs::remove_dir_all(&app_dest).await?;
            }

            _rollback.add(app_dest.clone());

            let cp_output = tokio::process::Command::new("cp")
                .arg("-R")
                .arg(&source)
                .arg(&app_dest)
                .output()
                .await?;

            if !cp_output.status.success() {
                return Err(WaxError::InstallError(format!(
                    "Failed to copy app: {}",
                    String::from_utf8_lossy(&cp_output.stderr)
                )));
            }

            Ok(())
        }
    }

    #[instrument(skip(self, _staging, _rollback))]
    pub async fn install_pkg(
        &self,
        _staging: &StagingContext,
        _rollback: &mut RollbackContext,
        source_rel: &str,
    ) -> Result<()> {
        #[cfg(not(target_os = "macos"))]
        return Err(WaxError::PlatformNotSupported(
            "PKG installers are macOS-only".to_string(),
        ));
        #[cfg(target_os = "macos")]
        {
            let source = self.resolve_source_path(_staging, source_rel);
            info!("Installing PKG: {:?}", source);

            if !source.exists() {
                return Err(WaxError::InstallError(format!(
                    "PKG source does not exist: {:?}",
                    source
                )));
            }

            // Warn before prompting so the user knows why credentials are needed.
            if let Some(m) = crate::signal::clone_active_multi() {
                let _ = m.println("\n⚠️  PKG installer requires administrator privileges");
            } else {
                println!("\n⚠️  PKG installer requires administrator privileges");
            }

            // Acquire sudo credentials interactively before spawning the installer.
            // acquire_sudo() prompts with Touch ID / password and caches the ticket.
            tokio::task::spawn_blocking(crate::sudo::acquire_sudo)
                .await
                .map_err(|e| WaxError::InstallError(e.to_string()))??;

            let install_output = tokio::process::Command::new("sudo")
                .arg("installer")
                .arg("-pkg")
                .arg(&source)
                .arg("-target")
                .arg("/")
                .output()
                .await?;

            if !install_output.status.success() {
                return Err(WaxError::InstallError(format!(
                    "Failed to install PKG: {}",
                    String::from_utf8_lossy(&install_output.stderr)
                )));
            }

            info!("Successfully installed PKG");
            Ok(())
        }
    }

    #[instrument(skip(self, staging, rollback))]
    pub async fn install_binary(
        &self,
        staging: &StagingContext,
        rollback: &mut RollbackContext,
        source_rel: &str,
        target_name: Option<&str>,
        cask_name: Option<&str>,
    ) -> Result<Option<PathBuf>> {
        let source = self.resolve_source_path(staging, source_rel);
        let name = target_name.unwrap_or_else(|| {
            Path::new(source_rel)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(source_rel)
        });

        Self::reject_traversal(Path::new(name))?;

        info!("Installing binary: {} from {:?}", name, source);

        if !source.exists() {
            if let Some(cask) = cask_name {
                debug!(
                    "Binary missing, attempting to fetch and extract preflight shimscript for {}",
                    cask
                );
                if let Ok(ruby_content) =
                    crate::formula_parser::FormulaParser::fetch_cask_rb(cask).await
                {
                    if let Some(script_content) =
                        crate::formula_parser::FormulaParser::extract_shimscript(&ruby_content)
                    {
                        // Write the script to the expected source location
                        if let Some(parent) = source.parent() {
                            tokio::fs::create_dir_all(parent).await.ok();
                        }
                        if tokio::fs::write(&source, script_content).await.is_ok() {
                            crate::signal::println_through_active_multi(format!(
                                "  {} generated wrapper script via preflight",
                                console::style("✓").green()
                            ));
                        }
                    }
                }
            }
        }

        if !source.exists() {
            crate::signal::println_through_active_multi(
                "  ⚠️  skipping binary: source not found (possibly requires preflight script)",
            );
            return Ok(None);
        }

        let bin_dest_dir = Self::detect_writable_bin_dir().await?;
        let binary_dest_path = bin_dest_dir.join(name);

        if let Ok(metadata) = tokio::fs::symlink_metadata(&binary_dest_path).await {
            let file_type = metadata.file_type();
            if file_type.is_symlink() || file_type.is_file() {
                tokio::fs::remove_file(&binary_dest_path).await.ok();
            } else if file_type.is_dir() {
                tokio::fs::remove_dir_all(&binary_dest_path).await.ok();
            }
        }

        rollback.add(binary_dest_path.clone());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            // For DMG casks the staging root is a mounted image that gets detached
            // after staging drops. Copy the binary to the permanent version_dir first
            // so the symlink target survives past unmount.
            let link_target = if staging.is_mounted() {
                let perm_dir = staging.permanent_dir();
                tokio::fs::create_dir_all(&perm_dir).await?;
                let dest = perm_dir.join(name);
                tokio::fs::copy(&source, &dest).await?;
                dest
            } else {
                source
            };

            tokio::fs::symlink(&link_target, &binary_dest_path).await?;

            let mut perms = tokio::fs::metadata(&link_target).await?.permissions();
            perms.set_mode(0o755);
            tokio::fs::set_permissions(&link_target, perms).await?;
        }
        #[cfg(not(unix))]
        {
            tokio::fs::copy(&source, &binary_dest_path).await?;
        }

        info!(
            "Successfully installed {} to {}",
            name,
            bin_dest_dir.display()
        );

        Ok(Some(binary_dest_path))
    }

    #[instrument(skip(self, staging, rollback))]
    pub async fn install_font(
        &self,
        staging: &StagingContext,
        rollback: &mut RollbackContext,
        source_rel: &str,
    ) -> Result<()> {
        let source = self.resolve_source_path(staging, source_rel);
        let font_name = Path::new(source_rel)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                WaxError::InstallError(format!("Invalid font source: {}", source_rel))
            })?;

        #[cfg(target_os = "macos")]
        let user_fonts = dirs::home_dir()?.join("Library/Fonts");
        #[cfg(not(target_os = "macos"))]
        let user_fonts = dirs::home_dir()?.join(".local/share/fonts");
        tokio::fs::create_dir_all(&user_fonts).await?;
        let dest = user_fonts.join(font_name);

        if dest.exists() {
            tokio::fs::remove_file(&dest).await.ok();
        }

        rollback.add(dest.clone());

        tokio::fs::copy(&source, &dest).await?;
        Ok(())
    }

    #[instrument(skip(self, staging, rollback))]
    pub async fn install_manpage(
        &self,
        staging: &StagingContext,
        rollback: &mut RollbackContext,
        source_rel: &str,
    ) -> Result<()> {
        let source = self.resolve_source_path(staging, source_rel);
        let man_name = Path::new(source_rel)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                WaxError::InstallError(format!("Invalid manpage source: {}", source_rel))
            })?;

        let man_prefix = crate::bottle::homebrew_prefix().join("share/man");
        // Determine man section (e.g. man1, man8) from extension
        let section = Path::new(man_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("man1");
        let dest_dir = man_prefix.join(format!("man{}", section));
        tokio::fs::create_dir_all(&dest_dir).await?;
        let dest = dest_dir.join(man_name);

        if dest.exists() {
            tokio::fs::remove_file(&dest).await.ok();
        }

        rollback.add(dest.clone());

        tokio::fs::copy(&source, &dest).await?;
        Ok(())
    }

    #[instrument(skip(self, staging, rollback))]
    pub async fn install_artifact(
        &self,
        staging: &StagingContext,
        rollback: &mut RollbackContext,
        source_rel: &str,
        target_path: &str,
    ) -> Result<()> {
        let source = self.resolve_source_path(staging, source_rel);
        let dest = PathBuf::from(target_path);
        Self::reject_traversal(&dest)?;

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        if dest.exists() {
            if dest.is_dir() {
                tokio::fs::remove_dir_all(&dest).await?;
            } else {
                tokio::fs::remove_file(&dest).await?;
            }
        }

        rollback.add(dest.clone());

        let cp_output = tokio::process::Command::new("cp")
            .arg("-R")
            .arg(&source)
            .arg(&dest)
            .output()
            .await?;

        if !cp_output.status.success() {
            return Err(WaxError::InstallError(format!(
                "Failed to copy artifact: {}",
                String::from_utf8_lossy(&cp_output.stderr)
            )));
        }

        Ok(())
    }

    pub async fn install_generic_directory(
        &self,
        staging: &StagingContext,
        rollback: &mut RollbackContext,
        source_rel: &str,
        dest_parent: &Path,
    ) -> Result<()> {
        let source = self.resolve_source_path(staging, source_rel);
        let name = Path::new(source_rel)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| WaxError::InstallError(format!("Invalid source: {}", source_rel)))?;

        tokio::fs::create_dir_all(dest_parent).await?;
        let dest = dest_parent.join(name);

        if dest.exists() {
            let meta = tokio::fs::symlink_metadata(&dest).await?;
            if meta.is_dir() {
                tokio::fs::remove_dir_all(&dest).await?;
            } else {
                tokio::fs::remove_file(&dest).await?;
            }
        }

        rollback.add(dest.clone());

        let cp_output = tokio::process::Command::new("cp")
            .arg("-R")
            .arg(&source)
            .arg(&dest)
            .output()
            .await?;

        if !cp_output.status.success() {
            return Err(WaxError::InstallError(format!(
                "Failed to copy to {:?}: {}",
                dest_parent,
                String::from_utf8_lossy(&cp_output.stderr)
            )));
        }

        Ok(())
    }

    #[instrument(skip(self, staging, rollback))]
    pub async fn install_completion(
        &self,
        staging: &StagingContext,
        rollback: &mut RollbackContext,
        source_rel: &str,
        shell: &str,
        token: &str,
        target_name: Option<&str>,
    ) -> Result<()> {
        let source = self.resolve_source_path(staging, source_rel);

        if !source.exists() {
            debug!("Completion source not found at {:?}, skipping", source);
            return Ok(());
        }

        let prefix = crate::bottle::homebrew_prefix();
        let dest_dir = match shell {
            "bash" => prefix.join("etc/bash_completion.d"),
            "zsh" => prefix.join("share/zsh/site-functions"),
            "fish" => prefix.join("share/fish/vendor_completions.d"),
            _ => {
                return Err(WaxError::InstallError(format!(
                    "Unsupported shell: {}",
                    shell
                )));
            }
        };

        tokio::fs::create_dir_all(&dest_dir).await?;
        let filename = target_name.unwrap_or_else(|| {
            Path::new(source_rel)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(token)
        });

        Self::reject_traversal(Path::new(filename))?;

        let dest = dest_dir.join(filename);

        if let Ok(metadata) = tokio::fs::symlink_metadata(&dest).await {
            let file_type = metadata.file_type();
            if file_type.is_symlink() || file_type.is_file() {
                tokio::fs::remove_file(&dest).await.ok();
            } else if file_type.is_dir() {
                tokio::fs::remove_dir_all(&dest).await.ok();
            }
        }

        rollback.add(dest.clone());

        #[cfg(unix)]
        {
            tokio::fs::symlink(&source, &dest).await?;
        }
        #[cfg(not(unix))]
        {
            if source.is_dir() {
                crate::ui::copy_dir_all(&source, &dest)?;
            } else {
                tokio::fs::copy(&source, &dest).await?;
            }
        }

        Ok(())
    }
}

impl Default for CaskInstaller {
    fn default() -> Self {
        Self::new()
    }
}

pub fn detect_artifact_type(url: &str) -> Option<&'static str> {
    let path = url.split('?').next().unwrap_or(url);
    let path = path.split('#').next().unwrap_or(path);

    if path.ends_with(".dmg") {
        Some("dmg")
    } else if path.ends_with(".pkg") {
        Some("pkg")
    } else if path.ends_with(".zip") {
        Some("zip")
    } else if path.ends_with(".tar.gz")
        || path.ends_with(".tgz")
        || path.ends_with(".tar.bz2")
        || path.ends_with(".tbz")
        || path.ends_with(".tar.xz")
        || path.ends_with(".txz")
    {
        Some("tar.gz")
    } else {
        None
    }
}

pub fn detect_artifact_type_from_content_type(content_type: &str) -> Option<&'static str> {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    match ct {
        "application/x-apple-diskimage" => Some("dmg"),
        "application/octet-stream" => Some("binary"),
        "application/zip" | "application/x-zip-compressed" => Some("zip"),
        "application/x-tar" | "application/gzip" | "application/x-gzip" => Some("tar.gz"),
        "application/x-pkg" | "application/vnd.apple.installer+xml" => Some("pkg"),
        _ => None,
    }
}

pub fn detect_artifact_type_from_disposition(disposition: &str) -> Option<&'static str> {
    // Look for filename= in Content-Disposition header
    for part in disposition.split(';') {
        let part = part.trim();
        let value = if let Some(v) = part.strip_prefix("filename*=") {
            // RFC 5987 encoded, e.g. UTF-8''Raycast-1.0.dmg
            v.splitn(3, '\'').nth(2).unwrap_or(v).to_string()
        } else if let Some(v) = part.strip_prefix("filename=") {
            v.trim_matches('"').to_string()
        } else {
            continue;
        };
        if let Some(t) = detect_artifact_type(&value) {
            return Some(t);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_resolve_source_path() {
        let installer = CaskInstaller::new();
        let temp = tempdir().unwrap();
        let staging_root = temp.path().to_path_buf();

        let staging = StagingContext {
            staging_root: staging_root.clone(),
            mount_point: None,
            _temp_dir: Some(temp),
        };

        let prefix = crate::bottle::homebrew_prefix()
            .to_string_lossy()
            .to_string();

        // Test $HOMEBREW_PREFIX
        let res = installer.resolve_source_path(&staging, "$HOMEBREW_PREFIX/bin/foo");
        assert_eq!(res, PathBuf::from(format!("{}/bin/foo", prefix)));

        // Test #{HOMEBREW_PREFIX}
        let res = installer.resolve_source_path(&staging, "#{HOMEBREW_PREFIX}/bin/bar");
        assert_eq!(res, PathBuf::from(format!("{}/bin/bar", prefix)));

        // Test $APPDIR
        let res = installer.resolve_source_path(&staging, "$APPDIR/Contents/MacOS/qux");
        assert_eq!(res, staging_root.join("Contents/MacOS/qux"));

        // Test absolute path outside safe directories — should fall back to staging root
        let res = installer.resolve_source_path(&staging, "/usr/bin/true");
        assert_eq!(res, staging_root.join("true"));

        // Test relative path
        let res = installer.resolve_source_path(&staging, "relative/path");
        assert_eq!(res, staging_root.join("relative/path"));

        // Test path traversal is rejected
        let res = installer.resolve_source_path(&staging, "../../etc/passwd");
        assert_eq!(res, staging_root.join("passwd"));

        // Test absolute path within homebrew prefix is allowed
        let res = installer.resolve_source_path(&staging, &format!("{}/bin/brew", prefix));
        assert_eq!(res, PathBuf::from(format!("{}/bin/brew", prefix)));

        // Test absolute path within staging root is allowed
        let abs_in_staging = staging_root.join("foo/bar");
        let res = installer.resolve_source_path(&staging, abs_in_staging.to_str().unwrap());
        assert_eq!(res, abs_in_staging);
    }

    #[test]
    fn user_bin_dir_matches_documented_path() {
        let user_bin_dir = CaskInstaller::user_bin_dir().unwrap();
        assert_eq!(
            user_bin_dir,
            dirs::home_dir().unwrap().join(".local/wax/bin")
        );
    }
}
