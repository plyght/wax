use crate::bottle::{detect_platform, BottleDownloader};
use crate::cache::Cache;
use crate::error::{Result, WaxError};
use crate::install::{create_symlinks, InstallMode, InstallState, InstalledPackage};
use crate::signal::check_cancelled;
use crate::ui::{copy_dir_all, PROGRESS_BAR_CHARS, PROGRESS_BAR_TEMPLATE};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use tracing::instrument;

const GHCR_BASE: &str = "https://ghcr.io/v2/homebrew/core";

async fn get_ghcr_token(client: &reqwest::Client, formula_name: &str) -> Result<String> {
    let scope = format!("repository:homebrew/core/{}:pull", formula_name);
    let token_url = format!("https://ghcr.io/token?scope={}", scope);

    #[derive(serde::Deserialize)]
    struct TokenResponse {
        token: String,
    }

    let resp = client.get(&token_url).send().await?;
    let token_resp: TokenResponse = resp.json().await?;
    Ok(token_resp.token)
}

async fn list_available_versions(
    client: &reqwest::Client,
    formula_name: &str,
    token: &str,
) -> Result<Vec<String>> {
    let url = format!("{}/{}/tags/list", GHCR_BASE, formula_name);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(WaxError::VersionNotFound(format!(
            "Cannot list versions for {} (HTTP {})",
            formula_name,
            resp.status()
        )));
    }

    #[derive(serde::Deserialize)]
    struct TagList {
        tags: Vec<String>,
    }

    let tag_list: TagList = resp.json().await?;
    Ok(tag_list.tags)
}

async fn resolve_bottle_for_platform(
    client: &reqwest::Client,
    formula_name: &str,
    version: &str,
    platform: &str,
    token: &str,
) -> Result<(String, String)> {
    let manifest_url = format!("{}/{}/manifests/{}", GHCR_BASE, formula_name, version);

    let resp = client
        .get(&manifest_url)
        .header("Authorization", format!("Bearer {}", token))
        .header(
            "Accept",
            "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json",
        )
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(WaxError::VersionNotFound(format!(
            "No manifest found for {}@{} (HTTP {})",
            formula_name,
            version,
            resp.status()
        )));
    }

    let index: serde_json::Value = resp.json().await?;

    let manifests = index["manifests"].as_array().ok_or_else(|| {
        WaxError::VersionNotFound(format!(
            "Invalid image index for {}@{}",
            formula_name, version
        ))
    })?;

    let mut matched_digest: Option<String> = None;
    let mut available_platforms: Vec<String> = Vec::new();

    for manifest in manifests {
        let ref_name = manifest["annotations"]["org.opencontainers.image.ref.name"]
            .as_str()
            .unwrap_or("");

        let manifest_platform = ref_name
            .strip_prefix(&format!("{}.", version))
            .unwrap_or(ref_name);

        available_platforms.push(manifest_platform.to_string());

        if matched_digest.is_none() && manifest_platform == platform {
            matched_digest = Some(manifest["digest"].as_str().unwrap_or("").to_string());
        }
    }

    let platform_manifest_digest = matched_digest.ok_or_else(|| {
        WaxError::VersionNotFound(format!(
            "No bottle for {}@{} on {}.\nAvailable: {}",
            formula_name,
            version,
            platform,
            available_platforms.join(", ")
        ))
    })?;

    let layer_manifest_url = format!(
        "{}/{}/manifests/{}",
        GHCR_BASE, formula_name, platform_manifest_digest
    );

    let layer_resp = client
        .get(&layer_manifest_url)
        .header("Authorization", format!("Bearer {}", token))
        .header(
            "Accept",
            "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json",
        )
        .send()
        .await?;

    if !layer_resp.status().is_success() {
        return Err(WaxError::VersionNotFound(format!(
            "Cannot fetch platform manifest for {}@{} (HTTP {})",
            formula_name,
            version,
            layer_resp.status()
        )));
    }

    let layer_manifest: serde_json::Value = layer_resp.json().await?;

    let layers = layer_manifest["layers"].as_array().ok_or_else(|| {
        WaxError::VersionNotFound(format!(
            "Invalid layer manifest for {}@{}",
            formula_name, version
        ))
    })?;

    let layer = layers.first().ok_or_else(|| {
        WaxError::VersionNotFound(format!(
            "No layers in manifest for {}@{}",
            formula_name, version
        ))
    })?;

    let digest = layer["digest"].as_str().ok_or_else(|| {
        WaxError::VersionNotFound(format!(
            "No digest in layer manifest for {}@{}",
            formula_name, version
        ))
    })?;

    let sha256 = digest.strip_prefix("sha256:").unwrap_or(digest).to_string();
    let blob_url = format!("{}/{}/blobs/{}", GHCR_BASE, formula_name, digest);

    Ok((blob_url, sha256))
}

#[instrument(skip(cache))]
pub async fn version_install(
    cache: &Cache,
    formula_name: &str,
    version: &str,
    user: bool,
    global: bool,
) -> Result<()> {
    let start = std::time::Instant::now();

    cache.ensure_fresh().await?;

    let formulae = cache.load_all_formulae().await?;
    formulae
        .iter()
        .find(|f| f.name == formula_name || f.full_name == formula_name)
        .ok_or_else(|| WaxError::FormulaNotFound(formula_name.to_string()))?;

    let install_mode = match InstallMode::from_flags(user, global)? {
        Some(mode) => mode,
        None => InstallMode::detect(),
    };
    install_mode.validate()?;

    let state = InstallState::new()?;
    let platform = detect_platform();

    println!(
        "{} {}@{}",
        style("version-install").bold(),
        style(formula_name).magenta(),
        style(version).cyan()
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(WaxError::HttpError)?;

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));

    spinner.set_message("Authenticating with ghcr.io...");
    let token = get_ghcr_token(&client, formula_name).await?;

    spinner.set_message("Listing available versions...");
    let tags = list_available_versions(&client, formula_name, &token).await?;

    if !tags.contains(&version.to_string()) {
        spinner.finish_and_clear();

        let mut available = tags.clone();
        available.sort();
        available.reverse();
        let show: Vec<&String> = available.iter().take(15).collect();

        return Err(WaxError::VersionNotFound(format!(
            "Version {} not found for {}.\nAvailable versions: {}{}",
            version,
            formula_name,
            show.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            if available.len() > 15 {
                format!(" (+{} more)", available.len() - 15)
            } else {
                String::new()
            }
        )));
    }

    spinner.set_message(format!(
        "Resolving bottle for {}@{} ({})...",
        formula_name, version, platform
    ));

    let (blob_url, sha256) =
        resolve_bottle_for_platform(&client, formula_name, version, &platform, &token).await?;

    spinner.finish_and_clear();
    check_cancelled()?;

    let temp_dir = tempfile::TempDir::new()?;
    let tarball_path = temp_dir
        .path()
        .join(format!("{}-{}.tar.gz", formula_name, version));

    let downloader = BottleDownloader::new();
    let pb = ProgressBar::new(0);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(PROGRESS_BAR_TEMPLATE)
            .unwrap()
            .progress_chars(PROGRESS_BAR_CHARS),
    );
    pb.set_message(format!("{}@{}", formula_name, version));

    downloader
        .download(&blob_url, &tarball_path, Some(&pb))
        .await?;
    pb.finish_and_clear();

    BottleDownloader::verify_checksum(&tarball_path, &sha256)?;

    let extract_dir = temp_dir.path().join(formula_name);
    BottleDownloader::extract(&tarball_path, &extract_dir)?;

    let cellar = install_mode.cellar_path()?;
    let formula_cellar = cellar.join(formula_name).join(version);

    if formula_cellar.exists() {
        tokio::fs::remove_dir_all(&formula_cellar)
            .await
            .or_else(|_| crate::sudo::sudo_remove(&formula_cellar).map(|_| ()))?;
    }
    tokio::fs::create_dir_all(&formula_cellar)
        .await
        .or_else(|_| crate::sudo::sudo_mkdir(&formula_cellar))?;

    let actual_content_dir = extract_dir.join(formula_name).join(version);
    if actual_content_dir.exists() {
        copy_dir_all(&actual_content_dir, &formula_cellar)?;
    } else {
        let name_dir = extract_dir.join(formula_name);
        if name_dir.exists() {
            let mut found_version_dir = None;
            if let Ok(mut entries) = std::fs::read_dir(&name_dir) {
                while let Some(Ok(entry)) = entries.next() {
                    let entry_name = entry.file_name().to_string_lossy().to_string();
                    if entry_name.starts_with(version) && entry.path().is_dir() {
                        found_version_dir = Some(entry.path());
                        break;
                    }
                }
            }
            if let Some(version_dir) = found_version_dir {
                copy_dir_all(&version_dir, &formula_cellar)?;
            } else {
                copy_dir_all(&extract_dir, &formula_cellar)?;
            }
        } else {
            copy_dir_all(&extract_dir, &formula_cellar)?;
        }
    }

    if cfg!(target_os = "linux") {
        let prefix = install_mode.prefix()?;
        BottleDownloader::relocate_bottle(
            &formula_cellar,
            prefix.to_str().unwrap_or("/home/linuxbrew/.linuxbrew"),
        )?;
    }

    create_symlinks(formula_name, version, &cellar, false, install_mode).await?;

    let package = InstalledPackage {
        name: formula_name.to_string(),
        version: version.to_string(),
        platform: platform.clone(),
        install_date: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        install_mode,
        from_source: false,
        bottle_rebuild: 0,
        bottle_sha256: Some(sha256),
    };
    state.add(package).await?;

    let elapsed = start.elapsed();
    println!(
        "\n+ {}@{} [{}ms]",
        style(formula_name).magenta(),
        style(version).cyan(),
        elapsed.as_millis()
    );

    Ok(())
}
