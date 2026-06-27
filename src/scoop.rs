//! Install portable Windows packages from Scoop-style JSON manifests using wax's
//! HTTP downloader (multipart when beneficial) and local extraction — without
//! invoking Scoop's PowerShell installer. Supports zip/tar.gz archives and
//! direct `.exe` downloads (including Scoop's `#/rename.exe` URL fragments).
//!
//! Chocolatey `.nupkg` packages are not supported here: most run
//! `chocolateyinstall.ps1` to compute download URLs and drive MSI/EXE setups.
//! Use `scoop-install` for zip/tar.gz-based portable apps, or run `choco.exe`
//! separately for full Chocolatey semantics.

use crate::bottle::BottleDownloader;
use crate::error::{Result, WaxError};
use crate::package_spec::Ecosystem;
use crate::ui::copy_dir_all;
use crate::windows_state::{self, WindowsPackageManifest};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use tempfile::TempDir;
use tracing::debug;

pub const DEFAULT_BUCKET_BASE: &str =
    "https://raw.githubusercontent.com/ScoopInstaller/Main/master/bucket";

#[derive(Debug, Clone)]
pub struct ResolvedScoopPackage {
    pub version: String,
    pub download_url: String,
    /// Scoop `#/filename.exe` fragment — save the downloaded artifact under this name.
    pub save_as: Option<String>,
    pub sha256: String,
    pub extract_dir: Option<String>,
    /// Paths relative to the extraction root (after optional extract_dir), using OS separators.
    pub bin_paths: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct ArchEntry {
    url: String,
    hash: String,
    extract_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ScoopManifest {
    version: String,
    url: Option<String>,
    hash: Option<String>,
    architecture: Option<std::collections::HashMap<String, ArchEntry>>,
    bin: Option<Value>,
    pre_install: Option<Value>,
    post_install: Option<Value>,
    installer: Option<Value>,
}

fn scoop_arch_key() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "64bit",
        "aarch64" => "arm64",
        "x86" => "32bit",
        _ => "64bit",
    }
}

fn parse_scoop_download_url(url_raw: &str) -> (String, Option<String>) {
    let (base, fragment) = match url_raw.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (url_raw, None),
    };
    let save_as = fragment.and_then(|f| {
        let name = f.strip_prefix('/').unwrap_or(f);
        if name.is_empty() {
            None
        } else {
            Some(name.replace('\\', "/"))
        }
    });
    (base.to_string(), save_as)
}

fn unsupported_script_fields(m: &ScoopManifest) -> Option<&'static str> {
    if let Some(v) = &m.pre_install {
        if !value_is_empty_or_comment_only(v) {
            return Some("pre_install");
        }
    }
    if let Some(v) = &m.post_install {
        if !value_is_empty_or_comment_only(v) {
            return Some("post_install");
        }
    }
    if m.installer.is_some() {
        return Some("installer");
    }
    None
}

fn value_is_empty_or_comment_only(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(a) => a.iter().all(|x| match x {
            Value::String(s) => {
                let t = s.trim();
                t.is_empty() || t.starts_with('#')
            }
            _ => false,
        }),
        Value::String(s) => {
            let t = s.trim();
            t.is_empty() || t.starts_with('#')
        }
        _ => false,
    }
}

fn normalize_scoop_hash(raw: &str) -> String {
    let t = raw.trim();
    if let Some(rest) = t.strip_prefix("sha256:") {
        return rest.trim().to_ascii_lowercase();
    }
    if let Some(rest) = t.strip_prefix("SHA256:") {
        return rest.trim().to_ascii_lowercase();
    }
    t.to_ascii_lowercase()
}

fn parse_bin_paths(bin: &Option<Value>) -> Result<Vec<PathBuf>> {
    let Some(bin) = bin else {
        return Err(WaxError::InstallError(
            "Scoop manifest has no bin field (wax cannot guess executables)".into(),
        ));
    };

    let mut out = Vec::new();
    match bin {
        Value::String(s) => out.push(PathBuf::from(s.replace('\\', "/"))),
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::String(s) => out.push(PathBuf::from(s.replace('\\', "/"))),
                    Value::Array(pair) => {
                        if let Some(Value::String(p)) = pair.first() {
                            out.push(PathBuf::from(p.replace('\\', "/")));
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {
            return Err(WaxError::ParseError(
                "Scoop manifest bin field has unsupported shape".into(),
            ));
        }
    }

    if out.is_empty() {
        return Err(WaxError::InstallError(
            "Scoop manifest bin field resolved to no executables".into(),
        ));
    }
    Ok(out)
}

fn join_under_root(root: &Path, rel: &Path) -> Result<PathBuf> {
    for c in rel.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(WaxError::InstallError(format!(
                    "Unsafe path in manifest: {}",
                    rel.display()
                )));
            }
        }
    }
    Ok(root.join(rel))
}

/// Parse a manifest JSON string and resolve URLs for the current architecture.
pub fn resolve_manifest_json(raw: &str) -> Result<ResolvedScoopPackage> {
    let m: ScoopManifest = serde_json::from_str(raw).map_err(WaxError::JsonError)?;

    if let Some(field) = unsupported_script_fields(&m) {
        return Err(WaxError::InstallError(format!(
            "This Scoop manifest uses `{field}` scripts; wax only supports portable zip/tar.gz installs without PowerShell hooks. Try another package or install with Scoop itself."
        )));
    }

    let arch = scoop_arch_key();
    let (url_raw, hash_raw, extract_dir) = if let Some(map) = &m.architecture {
        let entry = map.get(arch).ok_or_else(|| {
            WaxError::InstallError(format!(
                "Scoop manifest has no {arch} architecture entry for this host"
            ))
        })?;
        (
            entry.url.clone(),
            entry.hash.clone(),
            entry.extract_dir.clone(),
        )
    } else {
        let url = m.url.ok_or_else(|| {
            WaxError::InstallError("Scoop manifest has no url (and no architecture block)".into())
        })?;
        let hash = m.hash.ok_or_else(|| {
            WaxError::InstallError("Scoop manifest has no hash (and no architecture block)".into())
        })?;
        (url, hash, None)
    };

    let (download_url, save_as) = parse_scoop_download_url(&url_raw);
    let sha256 = normalize_scoop_hash(&hash_raw);
    if sha256.len() != 64 || !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(WaxError::InstallError(format!(
            "Expected 64-char sha256 hex from manifest, got {sha256:?}"
        )));
    }

    let bin_paths = parse_bin_paths(&m.bin)?;

    Ok(ResolvedScoopPackage {
        version: m.version,
        download_url,
        save_as,
        sha256,
        extract_dir,
        bin_paths,
    })
}

/// True if a Scoop JSON manifest exists for this package name (HEAD request).
#[cfg(target_os = "windows")]
pub async fn scoop_manifest_exists(bucket_base: &str, package: &str) -> bool {
    let base = bucket_base.trim_end_matches('/');
    let url = format!("{base}/{}.json", package);
    let client = crate::http_client::default_client();
    match client.head(&url).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

async fn fetch_manifest_text(bucket_base: &str, package: &str) -> Result<String> {
    let base = bucket_base.trim_end_matches('/');
    let url = format!("{base}/{}.json", package);
    debug!("Fetching Scoop manifest {}", url);
    let client = crate::http_client::default_client();
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(WaxError::InstallError(format!(
            "Failed to fetch manifest {}: HTTP {}",
            url,
            resp.status()
        )));
    }
    Ok(resp.text().await?)
}

fn download_kind_from_url(url: &str) -> Result<&'static str> {
    let lower = url.to_ascii_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);
    if path.ends_with(".zip") {
        return Ok("zip");
    }
    if path.contains(".tar.gz") || path.ends_with(".tgz") {
        return Ok("tar.gz");
    }
    if path.ends_with(".exe") {
        return Ok("exe");
    }
    Err(WaxError::InstallError(format!(
        "Unsupported download type for wax scoop-install (need .zip, .tar.gz/.tgz, or .exe URL): {url}"
    )))
}

fn exe_filename(resolved: &ResolvedScoopPackage) -> Result<String> {
    if let Some(name) = &resolved.save_as {
        return Ok(name.clone());
    }
    let path = resolved
        .download_url
        .split('?')
        .next()
        .unwrap_or(&resolved.download_url);
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            WaxError::InstallError(format!(
                "Cannot determine executable filename from {}",
                resolved.download_url
            ))
        })
}

pub(crate) fn extract_zip_file(zip_path: &Path, dest_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| WaxError::InstallError(e.to_string()))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| WaxError::InstallError(e.to_string()))?;
        let rel = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => continue,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out_path = dest_dir.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(p) = out_path.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut outfile = std::fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut outfile)?;
        }
    }
    Ok(())
}

fn extract_tar_gz(tarball: &Path, dest_dir: &Path) -> Result<()> {
    BottleDownloader::extract(tarball, dest_dir)
}

/// Download manifest from `bucket_base`, then download & extract the artifact.
pub async fn install_from_bucket(package: &str, bucket_base: Option<&str>) -> Result<()> {
    if !cfg!(target_os = "windows") {
        return Err(WaxError::PlatformNotSupported(
            "scoop-install is only supported on Windows (portable .exe layout)".into(),
        ));
    }

    let bucket = bucket_base.unwrap_or(DEFAULT_BUCKET_BASE);
    let text = fetch_manifest_text(bucket, package).await?;
    let resolved = resolve_manifest_json(&text)?;

    let kind = download_kind_from_url(&resolved.download_url)?;
    debug!(
        "Scoop package {} @ {} kind={} url={}",
        package, resolved.version, kind, resolved.download_url
    );

    let version_dir = resolved_staging_dir(package, &resolved.version)?;
    if version_dir.exists() {
        std::fs::remove_dir_all(&version_dir)?;
    }
    std::fs::create_dir_all(version_dir.parent().unwrap())?;

    let tmp = TempDir::new()?;
    let ext = match kind {
        "zip" => "zip",
        "tar.gz" => "tar.gz",
        "exe" => "exe",
        _ => "dat",
    };
    let download_path = tmp.path().join(format!("download.{ext}"));

    let dl = BottleDownloader::new();
    let size = dl.probe_size(&resolved.download_url).await;
    let conns =
        BottleDownloader::num_connections(size, BottleDownloader::MAX_CONNECTIONS_PER_DOWNLOAD);
    let pb = ProgressBar::new(0);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.cyan} {msg} [{bar:30.cyan/blue}] {bytes}/{total_bytes}")
            .unwrap()
            .progress_chars("=>-"),
    );
    pb.set_message(format!("{} {}", package, resolved.version));

    dl.download(
        &resolved.download_url,
        &download_path,
        Some(&pb),
        conns,
        None,
    )
    .await?;
    pb.finish_and_clear();

    crate::digest::verify_sha256_file(&download_path, &resolved.sha256)?;

    match kind {
        "zip" | "tar.gz" => {
            let extract_root = tmp.path().join("extract");
            std::fs::create_dir_all(&extract_root)?;
            match kind {
                "zip" => extract_zip_file(&download_path, &extract_root)?,
                "tar.gz" => extract_tar_gz(&download_path, &extract_root)?,
                _ => unreachable!(),
            }

            let source_tree = match &resolved.extract_dir {
                Some(d) => extract_root.join(d.replace('\\', "/")),
                None => extract_root.clone(),
            };
            if !source_tree.exists() {
                return Err(WaxError::InstallError(format!(
                    "Extracted files missing expected extract_dir {:?}",
                    resolved.extract_dir
                )));
            }

            copy_dir_all(&source_tree, &version_dir)?;
        }
        "exe" => {
            std::fs::create_dir_all(&version_dir)?;
            let dest = version_dir.join(exe_filename(&resolved)?);
            std::fs::copy(&download_path, &dest)?;
        }
        _ => unreachable!(),
    }

    let bin_dir = windows_state::wax_bin_dir()?;
    std::fs::create_dir_all(&bin_dir)?;

    let mut copy_actions = Vec::new();
    for rel in &resolved.bin_paths {
        let src = join_under_root(&version_dir, rel)?;
        if !src.exists() {
            return Err(WaxError::InstallError(format!(
                "Expected binary missing after extract: {}",
                src.display()
            )));
        }
        let file_name = src
            .file_name()
            .ok_or_else(|| WaxError::InstallError("Invalid bin path".into()))?;
        let dest = bin_dir.join(file_name);
        copy_actions.push((src, dest));
    }
    let bin_links: Vec<PathBuf> = copy_actions.iter().map(|(_, dest)| dest.clone()).collect();
    windows_state::validate_bin_links_available(Ecosystem::Scoop, package, &bin_links)?;

    for (src, dest) in copy_actions {
        if dest.exists() {
            std::fs::remove_file(&dest)?;
        }
        std::fs::copy(&src, &dest)?;
    }

    let mut files = windows_state::collect_files(&version_dir)?;
    files.extend(bin_links.iter().cloned());
    WindowsPackageManifest::new(
        Ecosystem::Scoop,
        package,
        resolved.version.clone(),
        resolved.download_url.clone(),
        version_dir.clone(),
        bin_links,
        files,
    )
    .save()?;

    println!(
        "Installed {} {} (Scoop manifest) — add to PATH if needed:\n  {}",
        package,
        resolved.version,
        bin_dir.display()
    );

    Ok(())
}

fn wax_user_root() -> Result<PathBuf> {
    windows_state::wax_windows_root()
}

fn resolved_staging_dir(package: &str, version: &str) -> Result<PathBuf> {
    Ok(wax_user_root()?
        .join("scoop-apps")
        .join(package)
        .join(version))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RG_MANIFEST: &str = include_str!("../tests/fixtures/scoop_ripgrep.json");
    const AGENT_BROWSER_MANIFEST: &str = include_str!("../tests/fixtures/scoop_agent_browser.json");

    #[test]
    fn resolve_ripgrep_main_manifest() {
        let r = resolve_manifest_json(RG_MANIFEST).unwrap();
        assert_eq!(r.version, "15.1.0");
        assert!(r.download_url.ends_with(".zip"));
        assert_eq!(r.sha256.len(), 64);
        assert!(r.extract_dir.as_ref().unwrap().contains("ripgrep"));
        assert_eq!(r.bin_paths.len(), 1);
        assert!(r.bin_paths[0].to_string_lossy().contains("rg"));
        assert!(r.save_as.is_none());
    }

    #[test]
    fn resolve_agent_browser_exe_manifest() {
        let r = resolve_manifest_json(AGENT_BROWSER_MANIFEST).unwrap();
        assert_eq!(r.version, "0.31.1");
        assert!(r.download_url.ends_with(".exe"));
        assert!(!r.download_url.contains('#'));
        assert_eq!(r.save_as.as_deref(), Some("agent-browser.exe"));
        assert_eq!(r.sha256.len(), 64);
        assert!(r.extract_dir.is_none());
        assert_eq!(r.bin_paths.len(), 1);
        assert_eq!(r.bin_paths[0].to_string_lossy(), "agent-browser.exe");
        assert_eq!(download_kind_from_url(&r.download_url).unwrap(), "exe");
        assert_eq!(exe_filename(&r).unwrap(), "agent-browser.exe");
    }

    #[test]
    fn parse_scoop_download_url_strips_fragment_and_save_as() {
        let (url, save_as) = parse_scoop_download_url(
            "https://example.invalid/pkg-win32-x64.exe#/agent-browser.exe",
        );
        assert_eq!(url, "https://example.invalid/pkg-win32-x64.exe");
        assert_eq!(save_as.as_deref(), Some("agent-browser.exe"));
    }
}
