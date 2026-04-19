use crate::error::{Result, WaxError};
use flate2::read::GzDecoder;
use indicatif::ProgressBar;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;
use tar::Archive;
use tokio::io::AsyncWriteExt;
use tracing::{debug, instrument};

/// Tracks aggregate downloaded / expected bytes across concurrent downloads (e.g. multiple casks).
#[derive(Clone, Default)]
pub struct DownloadTotals {
    pub downloaded: Arc<AtomicU64>,
    pub expected: Arc<AtomicU64>,
}

pub struct BottleDownloader {
    client: reqwest::Client,
}

impl BottleDownloader {
    const TRANSIENT_RETRY_ATTEMPTS: usize = 3;

    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .gzip(false)
            .brotli(false)
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }

    // Minimum file size to bother splitting across multiple connections.
    const MULTIPART_THRESHOLD: u64 = 4 * 1024 * 1024; // 4 MB

    /// Global connection pool shared across all concurrent downloads.
    pub const GLOBAL_CONNECTION_POOL: usize = 32;

    /// Maximum connections a single download may use.
    pub const MAX_CONNECTIONS_PER_DOWNLOAD: usize = 8;

    /// Probe a URL to get its download size. Used before starting downloads to
    /// allocate connections proportionally across packages by file size.
    pub async fn probe_size(&self, url: &str) -> u64 {
        let auth_token: Option<String> = if url.contains("ghcr.io") {
            self.get_ghcr_token(url).await.ok()
        } else {
            None
        };
        self.probe_url(url, &auth_token)
            .await
            .map(|(_, size, _)| size)
            .unwrap_or(0)
    }

    /// Returns how many connections to use for a file of the given size,
    /// capped by `max_connections` (the caller's share of the global pool).
    pub fn num_connections(size: u64, max_connections: usize) -> usize {
        let ideal = match size {
            s if s < 10 * 1024 * 1024 => 4, // <10 MB → up to 4
            s if s < 50 * 1024 * 1024 => 6, // <50 MB → up to 6
            _ => 8,                         // ≥50 MB → up to 8
        };
        ideal.min(max_connections).max(1)
    }

    #[instrument(skip(self, progress, totals))]
    pub async fn download(
        &self,
        url: &str,
        dest_path: &Path,
        progress: Option<&ProgressBar>,
        max_connections: usize,
        totals: Option<&DownloadTotals>,
    ) -> Result<()> {
        debug!("Downloading from {}", url);

        // Fetch auth token once (GHCR only — needed for the first redirect).
        let auth_token: Option<String> = if url.contains("ghcr.io") {
            self.get_ghcr_token(url).await.ok()
        } else {
            None
        };

        // Probe with a tiny range request.  This also resolves any redirect chain
        // (e.g. GHCR → Azure CDN pre-signed URL) and tells us the final URL and
        // whether the server supports byte-range requests.
        let (cdn_url, total_size, accepts_ranges) = self
            .probe_url(url, &auth_token)
            .await
            .unwrap_or_else(|_| (url.to_string(), 0, false));

        if let Some(t) = totals {
            if total_size > 0 {
                t.expected.fetch_add(total_size, Ordering::Relaxed);
            }
        }

        debug!(
            "Download probe: size={} bytes, accepts_ranges={}, max_connections={}",
            total_size, accepts_ranges, max_connections
        );
        let totals_for_multipart = totals.cloned();
        if accepts_ranges && total_size >= Self::MULTIPART_THRESHOLD && max_connections > 1 {
            match self
                .download_multipart(
                    &cdn_url,
                    dest_path,
                    total_size,
                    progress,
                    max_connections,
                    totals_for_multipart,
                )
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) => tracing::info!(
                    "Multipart failed ({}), falling back to single-connection",
                    e
                ),
            }
        }

        self.download_single(url, dest_path, &auth_token, total_size, progress, totals)
            .await
    }

    /// Makes a HEAD probe following all redirects to discover the final CDN URL,
    /// total content length, and range support.  Falls back to a range-GET
    /// (bytes=0-0) if the HEAD request fails (e.g. 405 Method Not Allowed).
    async fn probe_url(
        &self,
        url: &str,
        auth_token: &Option<String>,
    ) -> Result<(String, u64, bool)> {
        // Try HEAD first — cheap and avoids downloading any body.
        let mut head_req = self.client.head(url);
        if let Some(ref tok) = auth_token {
            head_req = head_req.header("Authorization", format!("Bearer {}", tok));
        }

        let resp = match Self::send_with_retry(head_req, "HEAD probe").await {
            Ok(r) if r.status().is_success() || r.status().as_u16() == 206 => r,
            _ => {
                // HEAD rejected or failed — fall back to a tiny range GET.
                let mut get_req = self.client.get(url).header("Range", "bytes=0-0");
                if let Some(ref tok) = auth_token {
                    get_req = get_req.header("Authorization", format!("Bearer {}", tok));
                }
                let r = Self::send_with_retry(get_req, "range probe").await?;
                // If the server ignored the Range header and returned the full
                // body (200 instead of 206), abort early to avoid downloading
                // the entire file during a probe.
                if r.status().as_u16() == 200 {
                    let final_url = r.url().to_string();
                    let size = r.content_length().unwrap_or(0);
                    drop(r);
                    return Ok((final_url, size, false));
                }
                r
            }
        };

        let final_url = resp.url().to_string();
        let status = resp.status().as_u16();
        let accepts_ranges = status == 206
            || resp
                .headers()
                .get("accept-ranges")
                .and_then(|v| v.to_str().ok())
                .map(|v| v == "bytes")
                .unwrap_or(false);

        // Content-Range: bytes 0-0/TOTAL → parse total
        let total_size = resp
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split('/').next_back())
            .and_then(|s| s.parse::<u64>().ok())
            .or_else(|| resp.content_length())
            .unwrap_or(0);

        Ok((final_url, total_size, accepts_ranges))
    }

    async fn download_multipart(
        &self,
        url: &str,
        dest_path: &Path,
        total_size: u64,
        progress: Option<&ProgressBar>,
        max_connections: usize,
        totals: Option<DownloadTotals>,
    ) -> Result<()> {
        let n = Self::num_connections(total_size, max_connections);
        let chunk_size = total_size.div_ceil(n as u64);

        if let Some(pb) = progress {
            if total_size > 0 {
                pb.set_length(total_size);
            }
            // Append "[Nx]" badge to whichever field the caller used for the name.
            // Formula bars use set_message ({msg}); cask bars use set_prefix ({prefix}).
            if n > 1 {
                let msg = pb.message().to_string();
                if !msg.is_empty() {
                    pb.set_message(format!("{} [{}x]", msg, n));
                }
                let prefix = pb.prefix().to_string();
                if !prefix.is_empty() {
                    pb.set_prefix(format!("{} [{}x]", prefix, n));
                }
            }
        }

        // Pre-allocate the file so every chunk task can seek to its own offset
        // and write without holding the entire file in memory (aria2-style).
        {
            let f = std::fs::File::create(dest_path)?;
            f.set_len(total_size)?;
        }

        let downloaded_so_far = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let client = self.client.clone();
        let url = url.to_string();
        let dest_path_buf = dest_path.to_path_buf();

        let mut tasks = Vec::with_capacity(n);
        for i in 0..n {
            let start = i as u64 * chunk_size;
            let end = (start + chunk_size - 1).min(total_size - 1);

            let client = client.clone();
            let url = url.clone();
            let counter = Arc::clone(&downloaded_so_far);
            let dest = dest_path_buf.clone();
            let totals_chunk = totals.clone();

            tasks.push(tokio::spawn(async move {
                let response = client
                    .get(&url)
                    .header("Range", format!("bytes={}-{}", start, end))
                    .send()
                    .await
                    .map_err(WaxError::from)?;

                if response.status().as_u16() != 206 {
                    return Err(WaxError::InstallError(format!(
                        "Chunk {} got HTTP {} (not 206)",
                        i,
                        response.status()
                    )));
                }

                // Stream chunk bytes, counting progress, then write at the
                // correct file offset in a blocking thread.
                let mut data = Vec::with_capacity((end - start + 1) as usize);
                let mut stream = response.bytes_stream();
                use futures::StreamExt;
                while let Some(piece) = stream.next().await {
                    if crate::signal::is_shutdown_requested() {
                        return Err(WaxError::Interrupted);
                    }
                    let piece = piece.map_err(WaxError::from)?;
                    let n = piece.len() as u64;
                    counter.fetch_add(n, Ordering::Relaxed);
                    if let Some(ref t) = totals_chunk {
                        t.downloaded.fetch_add(n, Ordering::Relaxed);
                    }
                    data.extend_from_slice(&piece);
                }

                // Write directly to the correct byte offset — no in-memory assembly needed.
                tokio::task::spawn_blocking(move || {
                    use std::io::{Seek, SeekFrom, Write};
                    let mut f = std::fs::OpenOptions::new().write(true).open(&dest)?;
                    f.seek(SeekFrom::Start(start))?;
                    f.write_all(&data)?;
                    Ok::<(), std::io::Error>(())
                })
                .await
                .map_err(|e| WaxError::InstallError(format!("join error: {}", e)))??;

                Ok::<(), WaxError>(())
            }));
        }

        // Update progress bar at ~150ms intervals — smoother display, less jitter.
        let counter_poll = Arc::clone(&downloaded_so_far);
        let pb_poll = progress.cloned();
        let poll_handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(150)).await;
                if let Some(ref pb) = pb_poll {
                    pb.set_position(counter_poll.load(Ordering::Relaxed));
                }
            }
        });

        let mut err: Option<String> = None;
        for task in tasks {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    err = Some(e.to_string());
                    break;
                }
                Err(e) => {
                    err = Some(e.to_string());
                    break;
                }
            }
        }
        poll_handle.abort();

        if err.is_some() {
            if let Some(ref t) = totals {
                let partial = downloaded_so_far.load(Ordering::Relaxed);
                if partial > 0 {
                    t.downloaded.fetch_sub(partial, Ordering::Relaxed);
                }
            }
        }

        if let Some(e) = err {
            return Err(WaxError::InstallError(format!(
                "Multipart download failed: {}",
                e
            )));
        }

        if let Some(pb) = progress {
            pb.set_position(total_size);
        }
        tracing::info!(
            "Multipart complete: {} connections, {} bytes",
            n,
            total_size
        );
        Ok(())
    }

    async fn download_single(
        &self,
        url: &str,
        dest_path: &Path,
        auth_token: &Option<String>,
        content_length: u64,
        progress: Option<&ProgressBar>,
        totals: Option<&DownloadTotals>,
    ) -> Result<()> {
        let mut request = self.client.get(url);
        if let Some(ref tok) = auth_token {
            request = request.header("Authorization", format!("Bearer {}", tok));
        }

        let response = Self::send_with_retry(request, "download").await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(WaxError::InstallError(format!(
                "Download failed with HTTP {}: {}",
                status,
                body.chars().take(200).collect::<String>()
            )));
        }

        let total_size = response.content_length().unwrap_or(content_length);
        if let Some(pb) = progress {
            if total_size > 0 {
                pb.set_length(total_size);
            }
        }
        if let Some(t) = totals {
            if content_length == 0 && total_size > 0 {
                t.expected.fetch_add(total_size, Ordering::Relaxed);
            }
        }

        let mut file = tokio::fs::File::create(dest_path).await?;
        let mut downloaded = 0u64;
        let mut stream = response.bytes_stream();

        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            if crate::signal::is_shutdown_requested() {
                drop(file);
                let _ = tokio::fs::remove_file(dest_path).await;
                return Err(crate::error::WaxError::Interrupted);
            }
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            let n = chunk.len() as u64;
            downloaded += n;
            if let Some(pb) = progress {
                pb.set_position(downloaded);
            }
            if let Some(t) = totals {
                t.downloaded.fetch_add(n, Ordering::Relaxed);
            }
        }

        file.flush().await?;
        debug!("Single-connection download: {} bytes", downloaded);
        Ok(())
    }

    async fn send_with_retry(
        request: reqwest::RequestBuilder,
        op_name: &str,
    ) -> std::result::Result<reqwest::Response, reqwest::Error> {
        for attempt in 1..=Self::TRANSIENT_RETRY_ATTEMPTS {
            let Some(cloned) = request.try_clone() else {
                return request.send().await;
            };

            match cloned.send().await {
                Ok(resp) => {
                    if !Self::is_retryable_status(resp.status())
                        || attempt == Self::TRANSIENT_RETRY_ATTEMPTS
                    {
                        return Ok(resp);
                    }
                    let backoff_ms = 300 * attempt as u64;
                    tracing::debug!(
                        "{} got HTTP {}, retrying attempt {}/{} in {}ms",
                        op_name,
                        resp.status(),
                        attempt + 1,
                        Self::TRANSIENT_RETRY_ATTEMPTS,
                        backoff_ms
                    );
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
                Err(e) => {
                    if attempt == Self::TRANSIENT_RETRY_ATTEMPTS {
                        return Err(e);
                    }
                    let backoff_ms = 300 * attempt as u64;
                    tracing::debug!(
                        "{} network error ({}), retrying attempt {}/{} in {}ms",
                        op_name,
                        e,
                        attempt + 1,
                        Self::TRANSIENT_RETRY_ATTEMPTS,
                        backoff_ms
                    );
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
            }
        }

        request.send().await
    }

    fn is_retryable_status(status: reqwest::StatusCode) -> bool {
        status == reqwest::StatusCode::REQUEST_TIMEOUT
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error()
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

            let full_path = canonical_dest.join(&path);

            match entry.header().entry_type() {
                t if t.is_symlink() => {
                    #[cfg(unix)]
                    {
                        let link_name = entry.link_name()?.ok_or_else(|| {
                            WaxError::InstallError(format!(
                                "Symlink entry has no link name: {}",
                                path.display()
                            ))
                        })?;
                        // Validate symlink target: reject absolute paths and
                        // parent-dir traversals that could escape the dest.
                        let target = Path::new(&*link_name);
                        if target.is_absolute() {
                            return Err(WaxError::InstallError(format!(
                                "Symlink target is absolute (path traversal): {}",
                                link_name.display()
                            )));
                        }
                        // Resolve the symlink target relative to the entry's
                        // parent and ensure it stays within canonical_dest.
                        if let Some(parent) = full_path.parent() {
                            let resolved = parent.join(&*link_name);
                            // Normalize components manually, rejecting
                            // excessive ".." that would escape the root.
                            let mut normalized = PathBuf::new();
                            for component in resolved.components() {
                                match component {
                                    std::path::Component::CurDir => {}
                                    std::path::Component::ParentDir => {
                                        if !normalized.pop() {
                                            return Err(WaxError::InstallError(format!(
                                                "Symlink target escapes destination via parent traversal: {} -> {}",
                                                path.display(),
                                                link_name.display()
                                            )));
                                        }
                                    }
                                    _ => normalized.push(component),
                                }
                            }
                            if !normalized.starts_with(&canonical_dest) {
                                return Err(WaxError::InstallError(format!(
                                    "Symlink target escapes destination: {} -> {}",
                                    path.display(),
                                    link_name.display()
                                )));
                            }
                            std::fs::create_dir_all(parent)?;
                        }
                        if full_path.symlink_metadata().is_ok() {
                            std::fs::remove_file(&full_path)?;
                        }
                        std::os::unix::fs::symlink(&*link_name, &full_path)?;
                    }
                    #[cfg(not(unix))]
                    {
                        return Err(WaxError::InstallError(format!(
                            "Symlinks not supported on this platform: {}",
                            path.display()
                        )));
                    }
                }
                t if t.is_hard_link() => {
                    let link_name = entry.link_name()?.ok_or_else(|| {
                        WaxError::InstallError(format!(
                            "Hard link entry has no link name: {}",
                            path.display()
                        ))
                    })?;
                    let link_target = canonical_dest.join(&*link_name);
                    if !link_target.starts_with(&canonical_dest) {
                        return Err(WaxError::InstallError(format!(
                            "Hard link target escapes destination: {}",
                            link_name.display()
                        )));
                    }
                    if let Some(parent) = full_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::hard_link(&link_target, &full_path)?;
                }
                _ if entry.header().entry_type().is_dir() => {
                    std::fs::create_dir_all(&full_path)?;
                }
                _ => {
                    if let Some(parent) = full_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    entry.unpack(&full_path)?;
                }
            }
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
            let mut i = 0;
            while i + placeholder_bytes.len() <= content.len() {
                if &content[i..i + placeholder_bytes.len()] == placeholder_bytes {
                    content.splice(i..i + placeholder_bytes.len(), replacement.iter().copied());
                    modified = true;
                    i += replacement.len().max(placeholder_bytes.len());
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

        let Some(patchelf) = which_patchelf() else {
            debug!("patchelf not found, skipping ELF relocation for {:?}", path);
            return Ok(());
        };

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // ── num_connections ──────────────────────────────────────────────────────

    #[test]
    fn num_connections_tiny_file() {
        // <10 MB → ideally 4, but capped by max_connections
        assert_eq!(BottleDownloader::num_connections(1024, 8), 4);
    }

    #[test]
    fn num_connections_medium_file() {
        // 20 MB → ideally 6
        assert_eq!(BottleDownloader::num_connections(20 * 1024 * 1024, 8), 6);
    }

    #[test]
    fn num_connections_large_file() {
        // 60 MB → ideally 8
        assert_eq!(BottleDownloader::num_connections(60 * 1024 * 1024, 8), 8);
    }

    #[test]
    fn num_connections_caps_at_max() {
        // max_connections=2 caps even if ideal is higher
        assert_eq!(BottleDownloader::num_connections(60 * 1024 * 1024, 2), 2);
    }

    #[test]
    fn num_connections_minimum_one() {
        // max_connections=0 still returns at least 1
        assert_eq!(BottleDownloader::num_connections(1024, 0), 1);
    }

    // ── verify_checksum ──────────────────────────────────────────────────────

    #[test]
    fn verify_checksum_correct() {
        use sha2::{Digest, Sha256};
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello world").unwrap();
        let hash = format!("{:x}", Sha256::digest(b"hello world"));
        let result = BottleDownloader::verify_checksum(f.path(), &hash);
        assert!(result.is_ok(), "{:?}", result);
    }

    #[test]
    fn verify_checksum_mismatch_returns_error() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello world").unwrap();
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        let result = BottleDownloader::verify_checksum(f.path(), wrong);
        assert!(result.is_err(), "expected checksum mismatch error");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("mismatch") || msg.contains("Checksum"),
            "error message: {msg}"
        );
    }

    #[test]
    fn verify_checksum_missing_file_returns_error() {
        let path = std::path::Path::new("/tmp/wax-test-nonexistent-file-xyz-123.tar.gz");
        let result = BottleDownloader::verify_checksum(path, "abc123");
        assert!(result.is_err());
    }

    #[test]
    fn relocate_file_replaces_longer_text_paths() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"exec @@HOMEBREW_CELLAR@@/odin/bin/odin\n")
            .unwrap();

        BottleDownloader::relocate_file(
            f.path(),
            &["@@HOMEBREW_CELLAR@@", "@@HOMEBREW_PREFIX@@"],
            "/opt/homebrew",
            "/opt/homebrew/Cellar",
        )
        .unwrap();

        let contents = std::fs::read_to_string(f.path()).unwrap();
        assert!(contents.contains("/opt/homebrew/Cellar/odin/bin/odin"));
        assert!(!contents.contains("@@HOMEBREW_CELLAR@@"));
    }
}
