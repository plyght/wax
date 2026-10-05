use crate::api::Formula;
use crate::error::{Result, WaxError};
use crate::formula_parser::FormulaParser;
use crate::install::{
    create_opt_link, create_symlinks, InstallMode, InstallState, InstalledPackage,
};
use crate::ruby_eval::{self, FormulaInstall};
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tracing::debug;

async fn single_root(dir: &Path) -> Result<PathBuf> {
    let mut entries = tokio::fs::read_dir(dir).await?;
    let mut only = None;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if only.is_some() {
            return Ok(dir.to_path_buf());
        }
        only = Some(entry);
    }
    match only {
        Some(entry) if entry.file_type().await?.is_dir() => Ok(entry.path()),
        _ => Ok(dir.to_path_buf()),
    }
}

async fn keg_has_files(keg: &Path) -> bool {
    let Ok(mut entries) = tokio::fs::read_dir(keg).await else {
        return false;
    };
    matches!(entries.next_entry().await, Ok(Some(_)))
}

pub(super) struct Installed {
    pub version: String,
}

pub(super) async fn install(
    formula: &Formula,
    ruby_content: &str,
    cellar: &Path,
    install_mode: InstallMode,
    state: &InstallState,
    platform: &str,
) -> Result<Installed> {
    let ruby = ruby_eval::find_ruby()
        .ok_or_else(|| WaxError::BuildError("no Ruby interpreter available".into()))?;
    let tmp = TempDir::new()?;
    let rb_path = match &formula.rb_path {
        Some(path) => path.clone(),
        None => {
            let path = tmp.path().join(format!("{}.rb", formula.name));
            tokio::fs::write(&path, ruby_content).await?;
            path
        }
    };

    let meta = ruby_eval::eval_formula_meta(&ruby, &rb_path).await?;
    if meta.patches {
        return Err(WaxError::BuildError(format!(
            "{} applies patches, which the Ruby installer does not support yet",
            formula.name
        )));
    }
    if meta.using.as_deref().is_some_and(|u| u != "nounzip") {
        return Err(WaxError::BuildError(format!(
            "{} downloads with `using: :{}`, which is not supported",
            formula.name,
            meta.using.as_deref().unwrap_or_default()
        )));
    }
    let url = meta
        .url
        .ok_or_else(|| WaxError::BuildError(format!("{} has no stable url", formula.name)))?;
    let sha256 = meta
        .sha256
        .ok_or_else(|| WaxError::BuildError(format!("{} has no sha256", formula.name)))?;
    let version = meta
        .version
        .unwrap_or_else(|| FormulaParser::extract_version_from_url(&url));
    crate::error::validate_version(&version)?;

    let response = crate::http_client::download().get(&url).send().await?;
    if !response.status().is_success() {
        return Err(WaxError::BuildError(format!(
            "Failed to download {}: HTTP {}",
            url,
            response.status()
        )));
    }
    let bytes = response.bytes().await?;
    let actual = crate::digest::sha256_digest_hex(&bytes);
    if actual != sha256 {
        return Err(WaxError::ChecksumMismatch {
            expected: sha256,
            actual,
        });
    }

    let stage = tmp.path().join("stage");
    tokio::fs::create_dir_all(&stage).await?;
    let extracted =
        super::stage_binary_release_download(&bytes, &url, &formula.name, &stage).await?;
    let buildpath = single_root(&extracted).await?;

    let keg = cellar.join(&formula.name).join(&version);
    if tokio::fs::symlink_metadata(&keg).await.is_ok() {
        tokio::fs::remove_dir_all(&keg).await?;
    }
    tokio::fs::create_dir_all(&keg).await?;

    let result = ruby_eval::run_formula_install(
        &ruby,
        &rb_path,
        FormulaInstall {
            name: &formula.name,
            version: &version,
            buildpath: &buildpath,
            prefix: &keg,
            path_prefix: &install_mode.prefix()?,
        },
    )
    .await;
    let log = match result {
        Ok(log) if keg_has_files(&keg).await => log,
        Ok(_) => {
            let _ = tokio::fs::remove_dir_all(&keg).await;
            return Err(WaxError::BuildError(format!(
                "{} install produced no files",
                formula.name
            )));
        }
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&keg).await;
            return Err(e);
        }
    };
    debug!("ruby install log for {}:\n{}", formula.name, log);

    if meta.keg_only {
        create_opt_link(&formula.name, &version, cellar, install_mode).await?;
    } else {
        create_symlinks(&formula.name, &version, cellar, false, install_mode).await?;
    }
    if let Err(e) = ruby_eval::run_formula_post_install(
        &ruby,
        &rb_path,
        FormulaInstall {
            name: &formula.name,
            version: &version,
            buildpath: &keg,
            prefix: &keg,
            path_prefix: &install_mode.prefix()?,
        },
    )
    .await
    {
        crate::signal::println_through_active_multi(format!(
            "warning: the post-install step for {} did not complete: {}",
            formula.name, e
        ));
    }
    state
        .add(InstalledPackage {
            name: formula.name.clone(),
            version: version.clone(),
            platform: platform.to_string(),
            install_date: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            install_mode,
            from_source: true,
            bottle_rebuild: 0,
            bottle_sha256: None,
            pinned: false,
        })
        .await?;
    Ok(Installed { version })
}
