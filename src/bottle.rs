use crate::error::{Result, WaxError};
use flate2::read::GzDecoder;
use indicatif::ProgressBar;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tar::Archive;
use tokio::io::AsyncWriteExt;
use tracing::{debug, instrument};

pub struct BottleDownloader {
    client: reqwest::Client,
}

impl BottleDownloader {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }

    #[instrument(skip(self, progress))]
    pub async fn download(
        &self,
        url: &str,
        dest_path: &Path,
        progress: Option<&ProgressBar>,
    ) -> Result<()> {
        debug!("Downloading bottle from {}", url);

        let mut request = self.client.get(url);

        if url.contains("ghcr.io") {
            if let Ok(token) = self.get_ghcr_token(url).await {
                request = request.header("Authorization", format!("Bearer {}", token));
            }
        }

        let response = request.send().await?;
        let total_size = response.content_length().unwrap_or(0);

        if let Some(pb) = progress {
            pb.set_length(total_size);
        }

        let mut file = tokio::fs::File::create(dest_path).await?;
        let mut downloaded = 0u64;
        let mut stream = response.bytes_stream();

        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;

            if let Some(pb) = progress {
                pb.set_position(downloaded);
            }
        }

        file.flush().await?;
        debug!("Downloaded {} bytes to {:?}", downloaded, dest_path);
        Ok(())
    }

    async fn get_ghcr_token(&self, url: &str) -> Result<String> {
        let repo_path = self.extract_repo_path(url)?;
        let token_url = format!("https://ghcr.io/token?scope=repository:{}:pull", repo_path);

        #[derive(serde::Deserialize)]
        struct TokenResponse {
            token: String,
        }

        let response = self.client.get(&token_url).send().await?;
        let token_resp: TokenResponse = response.json().await?;
        Ok(token_resp.token)
    }

    fn extract_repo_path(&self, url: &str) -> Result<String> {
        if let Some(start) = url.find("/v2/") {
            if let Some(end) = url.find("/blobs/") {
                let repo = &url[start + 4..end];
                return Ok(repo.to_string());
            }
        }
        Err(WaxError::InstallError(format!(
            "Invalid GHCR URL format: {}",
            url
        )))
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

    pub fn extract(tarball_path: &Path, dest_dir: &Path) -> Result<()> {
        debug!("Extracting {:?} to {:?}", tarball_path, dest_dir);

        std::fs::create_dir_all(dest_dir)?;

        let file = std::fs::File::open(tarball_path)?;
        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);

        archive.unpack(dest_dir)?;

        debug!("Extraction complete");
        Ok(())
    }
}

impl Default for BottleDownloader {
    fn default() -> Self {
        Self::new()
    }
}

pub fn run_command_with_timeout(cmd: &str, args: &[&str], timeout_secs: u64) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    let cmd_str = cmd.to_string();
    let args_vec: Vec<String> = args.iter().map(|s| s.to_string()).collect();

    thread::spawn(move || {
        let output = Command::new(&cmd_str).args(&args_vec).output();
        let _ = tx.send(output);
    });

    match rx.recv_timeout(Duration::from_secs(timeout_secs)) {
        Ok(Ok(output)) if output.status.success() => String::from_utf8(output.stdout)
            .ok()
            .map(|s| s.trim().to_string()),
        _ => None,
    }
}

pub fn detect_platform() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    match (os, arch) {
        ("macos", "aarch64") => {
            let version_info = macos_version();
            match version_info.as_str() {
                "15" => "arm64_sequoia".to_string(),
                "14" => "arm64_sonoma".to_string(),
                "13" => "arm64_ventura".to_string(),
                "12" => "arm64_monterey".to_string(),
                _ => "arm64_sonoma".to_string(),
            }
        }
        ("macos", "x86_64") => {
            let version_info = macos_version();
            match version_info.as_str() {
                "15" => "sequoia".to_string(),
                "14" => "sonoma".to_string(),
                "13" => "ventura".to_string(),
                "12" => "monterey".to_string(),
                _ => "sonoma".to_string(),
            }
        }
        ("linux", "x86_64") => "x86_64_linux".to_string(),
        ("linux", "aarch64") => "aarch64_linux".to_string(),
        _ => "unknown".to_string(),
    }
}

fn macos_version() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Some(version) = run_command_with_timeout("sw_vers", &["-productVersion"], 1) {
            if let Some(major) = version.split('.').next() {
                return major.to_string();
            }
        }
        "14".to_string()
    }
    #[cfg(not(target_os = "macos"))]
    {
        "14".to_string()
    }
}

pub fn homebrew_prefix() -> PathBuf {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let standard_prefix = match os {
        "macos" => match arch {
            "aarch64" => PathBuf::from("/opt/homebrew"),
            _ => PathBuf::from("/usr/local"),
        },
        "linux" => {
            let linuxbrew = PathBuf::from("/home/linuxbrew/.linuxbrew");
            if linuxbrew.join("Cellar").exists() {
                linuxbrew
            } else {
                PathBuf::from("/usr/local")
            }
        }
        _ => PathBuf::from("/usr/local"),
    };

    if let Some(prefix_str) = run_command_with_timeout("brew", &["--prefix"], 2) {
        let brew_prefix = PathBuf::from(&prefix_str);
        if brew_prefix.join("Cellar").exists() {
            if brew_prefix != standard_prefix {
                debug!(
                    "Using custom Homebrew prefix from brew --prefix: {:?}",
                    brew_prefix
                );
            }
            return brew_prefix;
        }
    }

    standard_prefix
}
