use crate::bottle::BottleDownloader;
use crate::error::{Result, WaxError};
use indicatif::ProgressBar;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, info, instrument};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledCask {
    pub name: String,
    pub version: String,
    pub install_date: i64,
}

pub struct CaskState {
    state_path: PathBuf,
}

impl CaskState {
    pub fn new() -> Result<Self> {
        let state_path = if let Some(base_dirs) = directories::BaseDirs::new() {
            base_dirs.data_local_dir().join("wax").join("installed_casks.json")
        } else {
            dirs::home_dir()
                .ok_or_else(|| WaxError::CacheError("Cannot determine home directory".into()))?
                .join(".wax")
                .join("installed_casks.json")
        };

        Ok(Self { state_path })
    }

    pub async fn load(&self) -> Result<HashMap<String, InstalledCask>> {
        match fs::read_to_string(&self.state_path).await {
            Ok(json) => {
                let casks: HashMap<String, InstalledCask> = serde_json::from_str(&json)?;
                Ok(casks)
            }
            Err(_) => Ok(HashMap::new()),
        }
    }

    pub async fn save(&self, casks: &HashMap<String, InstalledCask>) -> Result<()> {
        let parent = self
            .state_path
            .parent()
            .ok_or_else(|| WaxError::CacheError("Cannot determine parent directory".into()))?;
        fs::create_dir_all(parent).await?;

        let json = serde_json::to_string_pretty(casks)?;
        fs::write(&self.state_path, json).await?;
        Ok(())
    }

    pub async fn add(&self, cask: InstalledCask) -> Result<()> {
        let mut casks = self.load().await?;
        casks.insert(cask.name.clone(), cask);
        self.save(&casks).await?;
        Ok(())
    }

    pub async fn remove(&self, name: &str) -> Result<()> {
        let mut casks = self.load().await?;
        casks.remove(name);
        self.save(&casks).await?;
        Ok(())
    }
}

impl Default for CaskState {
    fn default() -> Self {
        Self::new().expect("Failed to initialize cask state")
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

    fn check_platform_support() -> Result<()> {
        #[cfg(not(target_os = "macos"))]
        {
            return Err(WaxError::PlatformNotSupported(
                "Cask installation is only supported on macOS. Use formulae for Linux packages.".to_string()
            ));
        }
        #[cfg(target_os = "macos")]
        {
            Ok(())
        }
    }

    fn applications_dir() -> Result<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            Ok(PathBuf::from("/Applications"))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(WaxError::PlatformNotSupported(
                "Applications directory concept is macOS-specific".to_string()
            ))
        }
    }

    #[instrument(skip(self, progress))]
    pub async fn download_cask(
        &self,
        url: &str,
        dest_path: &Path,
        progress: Option<&ProgressBar>,
    ) -> Result<()> {
        debug!("Downloading cask from {}", url);
        self.downloader.download(url, dest_path, progress).await
    }

    pub fn verify_checksum(path: &Path, expected_sha256: &str) -> Result<()> {
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

    #[instrument(skip(self))]
    pub async fn install_dmg(&self, dmg_path: &Path, app_name: &str) -> Result<()> {
        Self::check_platform_support()?;
        info!("Installing DMG: {:?}", dmg_path);

        let mount_point = PathBuf::from("/Volumes").join(format!("wax-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&mount_point).await?;

        let attach_output = tokio::process::Command::new("hdiutil")
            .arg("attach")
            .arg("-nobrowse")
            .arg("-quiet")
            .arg("-mountpoint")
            .arg(&mount_point)
            .arg(dmg_path)
            .output()
            .await?;

        if !attach_output.status.success() {
            return Err(WaxError::InstallError(format!(
                "Failed to mount DMG: {}",
                String::from_utf8_lossy(&attach_output.stderr)
            )));
        }

        let app_source = mount_point.join(app_name);
        if !app_source.exists() {
            let mut found_app = None;
            let mut entries = tokio::fs::read_dir(&mount_point).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("app") {
                    found_app = Some(path);
                    break;
                }
            }

            if let Some(found) = found_app {
                self.copy_app(&found, app_name).await?;
            } else {
                let _ = self.unmount_dmg(&mount_point).await;
                return Err(WaxError::InstallError(format!(
                    "Could not find {} in DMG",
                    app_name
                )));
            }
        } else {
            self.copy_app(&app_source, app_name).await?;
        }

        self.unmount_dmg(&mount_point).await?;

        tokio::fs::remove_dir(&mount_point).await.ok();

        info!("Successfully installed {}", app_name);
        Ok(())
    }

    async fn copy_app(&self, source: &Path, app_name: &str) -> Result<()> {
        let app_dest = Self::applications_dir()?.join(app_name);

        if app_dest.exists() {
            return Err(WaxError::InstallError(format!(
                "{} already exists in Applications directory",
                app_name
            )));
        }

        let cp_output = tokio::process::Command::new("cp")
            .arg("-R")
            .arg(source)
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

    async fn unmount_dmg(&self, mount_point: &Path) -> Result<()> {
        debug!("Unmounting DMG at {:?}", mount_point);

        let detach_output = tokio::process::Command::new("hdiutil")
            .arg("detach")
            .arg(mount_point)
            .arg("-quiet")
            .output()
            .await?;

        if !detach_output.status.success() {
            debug!(
                "Warning: Failed to unmount DMG: {}",
                String::from_utf8_lossy(&detach_output.stderr)
            );
        }

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn install_pkg(&self, pkg_path: &Path) -> Result<()> {
        Self::check_platform_support()?;
        info!("Installing PKG: {:?}", pkg_path);

        println!("\n⚠️  PKG installer requires administrator privileges");

        let install_output = tokio::process::Command::new("sudo")
            .arg("installer")
            .arg("-pkg")
            .arg(pkg_path)
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

    #[instrument(skip(self))]
    pub async fn install_zip(&self, zip_path: &Path, app_name: &str) -> Result<()> {
        Self::check_platform_support()?;
        info!("Installing ZIP: {:?}", zip_path);

        let temp_dir = tempfile::tempdir()?;

        let unzip_output = tokio::process::Command::new("unzip")
            .arg("-q")
            .arg(zip_path)
            .arg("-d")
            .arg(temp_dir.path())
            .output()
            .await?;

        if !unzip_output.status.success() {
            return Err(WaxError::InstallError(format!(
                "Failed to extract ZIP: {}",
                String::from_utf8_lossy(&unzip_output.stderr)
            )));
        }

        let app_source = temp_dir.path().join(app_name);
        if !app_source.exists() {
            let mut found_app = None;
            let mut entries = tokio::fs::read_dir(temp_dir.path()).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("app") {
                    found_app = Some(path);
                    break;
                }
            }

            if let Some(found) = found_app {
                self.copy_app(&found, app_name).await?;
            } else {
                return Err(WaxError::InstallError(format!(
                    "Could not find {} in ZIP",
                    app_name
                )));
            }
        } else {
            self.copy_app(&app_source, app_name).await?;
        }

        info!("Successfully installed {}", app_name);
        Ok(())
    }
}

impl Default for CaskInstaller {
    fn default() -> Self {
        Self::new()
    }
}

pub fn detect_artifact_type(url: &str) -> Option<&str> {
    if url.ends_with(".dmg") {
        Some("dmg")
    } else if url.ends_with(".pkg") {
        Some("pkg")
    } else if url.ends_with(".zip") {
        Some("zip")
    } else {
        None
    }
}

mod dirs {
    use std::path::PathBuf;

    pub fn home_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

mod uuid {
    pub struct Uuid;

    impl Uuid {
        pub fn new_v4() -> String {
            use std::time::{SystemTime, UNIX_EPOCH};
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            format!("{:x}", now)
        }
    }
}
