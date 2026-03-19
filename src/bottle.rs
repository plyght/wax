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
            .gzip(false)
            .brotli(false)
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

        debug!(
            "Download response: status={}, content-type={:?}, content-encoding={:?}",
            response.status(),
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            response
                .headers()
                .get("content-encoding")
                .and_then(|v| v.to_str().ok()),
        );

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(WaxError::InstallError(format!(
                "Download failed with HTTP {}: {}",
                status,
                body.chars().take(200).collect::<String>()
            )));
        }

        let total_size = response.content_length().unwrap_or(0);

        if let Some(pb) = progress {
            pb.set_length(total_size);
        }

        let mut file = tokio::fs::File::create(dest_path).await?;
        let mut downloaded = 0u64;
        let mut stream = response.bytes_stream();

        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            if crate::signal::is_shutdown_requested() {
                // Drop the incomplete file before returning so temp dir is clean
                drop(file);
                let _ = tokio::fs::remove_file(dest_path).await;
                return Err(crate::error::WaxError::Interrupted);
            }
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

        let canonical_dest = dunce::canonicalize(dest_dir)?;

        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();

            if path.is_absolute()
                || path
                    .components()
                    .any(|c| c == std::path::Component::ParentDir)
            {
                return Err(WaxError::InstallError(format!(
                    "Tar entry contains unsafe path: {}",
                    path.display()
                )));
            }

            match entry.header().entry_type() {
                t if t.is_symlink() || t.is_hard_link() => {
                    return Err(WaxError::InstallError(format!(
                        "Tar entry contains unsupported link type: {}",
                        path.display()
                    )));
                }
                _ => {}
            }

            entry.unpack_in(&canonical_dest)?;
        }

        debug!("Extraction complete");
        Ok(())
    }

    pub fn relocate_bottle(dir: &Path, prefix: &str) -> Result<()> {
        let placeholders = ["@@HOMEBREW_PREFIX@@", "@@HOMEBREW_CELLAR@@"];
        let cellar = format!("{}/Cellar", prefix);

        Self::relocate_dir(dir, &placeholders, prefix, &cellar)
    }

    fn relocate_dir(dir: &Path, placeholders: &[&str], prefix: &str, cellar: &str) -> Result<()> {
        let entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();

        for entry in entries {
            let path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_dir() {
                Self::relocate_dir(&path, placeholders, prefix, cellar)?;
            } else if file_type.is_file() {
                Self::relocate_file(&path, placeholders, prefix, cellar)?;
            }
        }
        Ok(())
    }

    fn relocate_file(path: &Path, placeholders: &[&str], prefix: &str, cellar: &str) -> Result<()> {
        let content = match std::fs::read(path) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };

        if content.len() >= 4 && &content[0..4] == b"\x7fELF" {
            return Self::relocate_elf(path, prefix, cellar);
        }

        // Detect Mach-O binaries (macOS): 32-bit, 64-bit, and fat/universal
        if is_mach_o(&content) {
            return Self::relocate_macho(path, prefix, cellar);
        }

        let mut content = content;
        let metadata = std::fs::metadata(path)?;
        let original_permissions = metadata.permissions();
        let mut perms = original_permissions.clone();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(perms.mode() | 0o200);
            std::fs::set_permissions(path, perms)?;
        }

        let mut modified = false;
        for placeholder in placeholders {
            let replacement = if *placeholder == "@@HOMEBREW_CELLAR@@" {
                cellar.as_bytes()
            } else {
                prefix.as_bytes()
            };

            let placeholder_bytes = placeholder.as_bytes();
            if replacement.len() > placeholder_bytes.len() {
                debug!(
                    "Skipping relocation: replacement ({} bytes) longer than placeholder ({} bytes) in {:?}",
                    replacement.len(),
                    placeholder_bytes.len(),
                    path
                );
                continue;
            }
            let mut i = 0;
            while i + placeholder_bytes.len() <= content.len() {
                if &content[i..i + placeholder_bytes.len()] == placeholder_bytes {
                    let pad_len = placeholder_bytes.len() - replacement.len();
                    content.splice(
                        i..i + placeholder_bytes.len(),
                        replacement
                            .iter()
                            .copied()
                            .chain(std::iter::repeat_n(0, pad_len)),
                    );
                    modified = true;
                    i += placeholder_bytes.len();
                } else {
                    i += 1;
                }
            }
        }

        if modified {
            std::fs::write(path, &content)?;
            #[cfg(unix)]
            {
                std::fs::set_permissions(path, original_permissions)?;
            }
            debug!("Relocated: {:?}", path);
        }
        Ok(())
    }

    fn relocate_elf(path: &Path, prefix: &str, cellar: &str) -> Result<()> {
        use std::process::Command;

        let patchelf = which_patchelf();
        if patchelf.is_none() {
            debug!("patchelf not found, skipping ELF relocation for {:?}", path);
            return Ok(());
        }
        let patchelf = patchelf.unwrap();

        let metadata = std::fs::metadata(path)?;
        let original_permissions = metadata.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = original_permissions.clone();
            perms.set_mode(perms.mode() | 0o200);
            std::fs::set_permissions(path, perms)?;
        }

        let interpreter = format!("{}/lib/ld.so", prefix);
        if Path::new(&interpreter).exists() {
            let output = Command::new(&patchelf)
                .args([
                    "--set-interpreter",
                    &interpreter,
                    path.to_str().unwrap_or_default(),
                ])
                .output();
            if let Ok(out) = output {
                if !out.status.success() {
                    debug!(
                        "patchelf set-interpreter failed: {:?}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
            }
        }

        if let Ok(output) = Command::new(&patchelf)
            .args(["--print-rpath", path.to_str().unwrap_or_default()])
            .output()
        {
            if output.status.success() {
                let rpath = String::from_utf8_lossy(&output.stdout);
                let new_rpath = rpath
                    .replace("@@HOMEBREW_PREFIX@@", prefix)
                    .replace("@@HOMEBREW_CELLAR@@", cellar);
                if new_rpath != rpath.as_ref() {
                    let _ = Command::new(&patchelf)
                        .args([
                            "--set-rpath",
                            new_rpath.trim(),
                            path.to_str().unwrap_or_default(),
                        ])
                        .output();
                    debug!("Relocated ELF rpath: {:?}", path);
                }
            }
        }

        #[cfg(unix)]
        {
            std::fs::set_permissions(path, original_permissions)?;
        }

        Ok(())
    }

    fn relocate_macho(path: &Path, prefix: &str, cellar: &str) -> Result<()> {
        use std::process::Command;

        #[cfg(unix)]
        let _perm_guard = {
            use std::os::unix::fs::PermissionsExt;
            struct PermissionGuard {
                path: std::path::PathBuf,
                original_mode: u32,
                changed: bool,
            }
            impl PermissionGuard {
                fn new(path: &Path) -> Option<Self> {
                    if let Ok(metadata) = std::fs::metadata(path) {
                        let perms = metadata.permissions();
                        let mode = perms.mode();
                        if mode & 0o200 == 0 {
                            let mut new_perms = perms;
                            new_perms.set_mode(mode | 0o200);
                            if std::fs::set_permissions(path, new_perms).is_ok() {
                                return Some(Self {
                                    path: path.to_path_buf(),
                                    original_mode: mode,
                                    changed: true,
                                });
                            }
                            return None;
                        }
                        Some(Self {
                            path: path.to_path_buf(),
                            original_mode: mode,
                            changed: false,
                        })
                    } else {
                        None
                    }
                }
            }
            impl Drop for PermissionGuard {
                fn drop(&mut self) {
                    if !self.changed {
                        return;
                    }
                    if let Ok(metadata) = std::fs::metadata(&self.path) {
                        let mut perms = metadata.permissions();
                        perms.set_mode(self.original_mode);
                        let _ = std::fs::set_permissions(&self.path, perms);
                    }
                }
            }
            PermissionGuard::new(path)
        };

        let path_str = match path.to_str() {
            Some(s) => s,
            None => {
                debug!("Skipping Mach-O relocation: non-UTF-8 path {:?}", path);
                return Ok(());
            }
        };

        let mut modified = false;

        // Fix the binary's own install name (relevant for dylibs)
        if let Ok(output) = Command::new("otool").args(["-D", path_str]).output() {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                let mut lines = text.lines();
                lines.next(); // skip header line
                if let Some(install_name) = lines.next() {
                    let install_name = install_name.trim();
                    let new_name = install_name
                        .replace("@@HOMEBREW_CELLAR@@", cellar)
                        .replace("@@HOMEBREW_PREFIX@@", prefix);
                    if new_name != install_name {
                        let _ = Command::new("install_name_tool")
                            .args(["-id", &new_name, path_str])
                            .output();
                        modified = true;
                        debug!("Relocated Mach-O install name: {:?}", path);
                    }
                }
            }
        }

        // Fix all referenced dylib paths (LC_LOAD_DYLIB)
        if let Ok(output) = Command::new("otool").args(["-L", path_str]).output() {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines().skip(1) {
                    let line = line.trim();
                    // Format: "\t/path/to/lib (compatibility version X, current version Y)"
                    let lib_path = if let Some(end) = line.find(" (") {
                        &line[..end]
                    } else {
                        continue;
                    };

                    if !lib_path.contains("@@HOMEBREW_CELLAR@@")
                        && !lib_path.contains("@@HOMEBREW_PREFIX@@")
                    {
                        continue;
                    }

                    let new_path = lib_path
                        .replace("@@HOMEBREW_CELLAR@@", cellar)
                        .replace("@@HOMEBREW_PREFIX@@", prefix);

                    let result = Command::new("install_name_tool")
                        .args(["-change", lib_path, &new_path, path_str])
                        .output();

                    if let Ok(out) = result {
                        if !out.status.success() {
                            debug!(
                                "install_name_tool -change failed for {:?}: {}",
                                path,
                                String::from_utf8_lossy(&out.stderr)
                            );
                        } else {
                            debug!(
                                "Relocated Mach-O dep {} -> {} in {:?}",
                                lib_path, new_path, path
                            );
                            modified = true;
                        }
                    }
                }
            }
        }

        // Fix RPATH entries (LC_RPATH) — e.g. @@HOMEBREW_PREFIX@@/lib
        if let Ok(output) = Command::new("otool").args(["-l", path_str]).output() {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                // Parse "path <value> (offset N)" lines inside LC_RPATH sections
                let mut in_rpath = false;
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("cmd LC_RPATH") || trimmed == "cmd LC_RPATH" {
                        in_rpath = true;
                        continue;
                    }
                    if trimmed.starts_with("cmd ") {
                        in_rpath = false;
                    }
                    if in_rpath && trimmed.starts_with("path ") {
                        let rpath = if let Some(end) = trimmed.find(" (offset") {
                            &trimmed["path ".len()..end]
                        } else {
                            &trimmed["path ".len()..]
                        };
                        if rpath.contains("@@HOMEBREW_CELLAR@@")
                            || rpath.contains("@@HOMEBREW_PREFIX@@")
                        {
                            let new_rpath = rpath
                                .replace("@@HOMEBREW_CELLAR@@", cellar)
                                .replace("@@HOMEBREW_PREFIX@@", prefix);
                            let result = Command::new("install_name_tool")
                                .args(["-rpath", rpath, &new_rpath, path_str])
                                .output();
                            if let Ok(out) = result {
                                if out.status.success() {
                                    debug!(
                                        "Relocated rpath {} -> {} in {:?}",
                                        rpath, new_rpath, path
                                    );
                                    modified = true;
                                } else {
                                    debug!(
                                        "install_name_tool -rpath failed for {:?}: {}",
                                        path,
                                        String::from_utf8_lossy(&out.stderr)
                                    );
                                }
                            }
                        }
                        in_rpath = false; // each LC_RPATH has one path
                    }
                }
            }
        }

        // Re-sign with an ad-hoc signature after any modification.
        // install_name_tool invalidates the code signature on Apple Silicon,
        // and macOS kills modified unsigned binaries with SIGKILL.
        if modified {
            let _ = Command::new("codesign")
                .args(["--force", "--sign", "-", path_str])
                .output();
            debug!("Re-signed Mach-O: {:?}", path);
        }

        Ok(())
    }
}

/// Returns true if the first 4 bytes match any Mach-O magic number.
pub fn is_mach_o(data: &[u8]) -> bool {
    data.len() >= 4
        && matches!(
            &data[0..4],
            b"\xCE\xFA\xED\xFE" | b"\xCF\xFA\xED\xFE" | b"\xBE\xBA\xFE\xCA" | b"\xCA\xFE\xBA\xBE"
        )
}

fn which_patchelf() -> Option<String> {
    for path in [
        "/home/linuxbrew/.linuxbrew/bin/patchelf",
        "/usr/bin/patchelf",
        "/usr/local/bin/patchelf",
        "patchelf",
    ] {
        if let Ok(output) = std::process::Command::new(path).arg("--version").output() {
            if output.status.success() {
                return Some(path.to_string());
            }
        }
    }
    None
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
        ("macos", arch) => {
            let prefix = if arch == "aarch64" { "arm64_" } else { "" };
            let codename = macos_codename();
            format!("{}{}", prefix, codename)
        }
        ("linux", "x86_64") => "x86_64_linux".to_string(),
        ("linux", "aarch64" | "arm") => "arm64_linux".to_string(),
        _ => "unknown".to_string(),
    }
}

fn macos_codename() -> &'static str {
    let version = macos_version();
    match version.as_str() {
        "16" | "26" => "tahoe",
        "15" => "sequoia",
        "14" => "sonoma",
        "13" => "ventura",
        "12" => "monterey",
        v => {
            if let Ok(major) = v.parse::<u32>() {
                if major > 26 {
                    "tahoe"
                } else {
                    "sequoia"
                }
            } else {
                "sequoia"
            }
        }
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
