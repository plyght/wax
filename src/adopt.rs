//! Shared adoption of software installed outside Wax.
//!
//! Commands previously re-implemented Cellar sync, caskroom sync, and
//! `/Applications` discovery with inconsistent merge rules. This module is
//! the single resolve + adopt layer every lifecycle command should use.

use crate::cache::Cache;
use crate::cask::{CaskState, InstalledCask};
#[cfg(target_os = "linux")]
use crate::discovery::discover_linux_system_packages;
use crate::error::{Result, WaxError};
use crate::install::{InstallState, InstalledPackage};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    Formula,
    Cask,
}

#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub name: String,
    pub kind: PackageKind,
    pub formula: Option<InstalledPackage>,
}

#[derive(Debug, Clone, Copy)]
pub struct AdoptOptions {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub persist: bool,
    pub formulae: bool,
    pub casks: bool,
}

impl Default for AdoptOptions {
    fn default() -> Self {
        Self {
            persist: true,
            formulae: true,
            casks: true,
        }
    }
}

impl AdoptOptions {
    #[cfg(not(target_os = "windows"))]
    pub fn formulae_only() -> Self {
        Self {
            persist: true,
            formulae: true,
            casks: false,
        }
    }

    pub fn casks_only() -> Self {
        Self {
            persist: true,
            formulae: false,
            casks: true,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct InstalledSnapshot {
    pub formulae: HashMap<String, InstalledPackage>,
    pub casks: HashMap<String, InstalledCask>,
    pub caskroom_synced_names: HashSet<String>,
}

pub fn short_package_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Sync formula installs from Cellar without requiring a package cache.
pub async fn sync_formulae() -> Result<HashMap<String, InstalledPackage>> {
    let state = InstallState::new()?;
    if let Err(e) = state.sync_from_cellar().await {
        tracing::warn!("sync_from_cellar failed: {e}");
    }
    state.load().await
}

#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
pub async fn sync_installed_state(
    cache: &Cache,
    options: AdoptOptions,
) -> Result<InstalledSnapshot> {
    let mut snapshot = InstalledSnapshot::default();

    if options.formulae {
        let state = InstallState::new()?;
        if let Err(e) = state.sync_from_cellar().await {
            tracing::warn!("sync_from_cellar failed: {e}");
        }
        #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
        let mut formulae = state.load().await?;

        #[cfg(target_os = "linux")]
        {
            let all_formulae = cache.load_all_formulae().await?;
            for (name, package) in discover_linux_system_packages(&all_formulae).await? {
                formulae.entry(name).or_insert(package);
            }
            if options.persist {
                state.save(&formulae).await?;
            }
        }

        snapshot.formulae = formulae;
    }

    if options.casks {
        let cask_state = CaskState::new()?;
        let caskroom_synced_names = match cask_state.sync_from_caskrooms().await {
            Ok(synced) => synced,
            Err(e) => {
                tracing::warn!("sync_from_caskrooms failed: {e}");
                Default::default()
            }
        };
        #[allow(unused_mut)]
        let mut casks = cask_state.load().await?;

        #[cfg(target_os = "macos")]
        {
            // Older Wax versions persisted guesses about unmanaged apps here.
            casks.retain(|name, cask| {
                cask.artifact_type.as_deref() != Some("app") || caskroom_synced_names.contains(name)
            });
        }

        snapshot.casks = casks;
        snapshot.caskroom_synced_names = caskroom_synced_names;
    }

    Ok(snapshot)
}

/// Resolve a package name to formula or cask, adopting unmanaged installs first.
///
/// When `force_cask` is true, only the cask path is considered. Otherwise formulae
/// win on name collision; if neither state has the name, discovery may still
/// surface a manually installed app as a cask.
pub async fn resolve_package(
    cache: &Cache,
    name: &str,
    force_cask: bool,
) -> Result<ResolvedPackage> {
    let short = short_package_name(name);

    if force_cask {
        let snapshot = sync_installed_state(cache, AdoptOptions::casks_only()).await?;
        return lookup_cask(cache, &snapshot.casks, name, short).await;
    }

    let snapshot = sync_installed_state(cache, AdoptOptions::default()).await?;

    if let Some(pkg) = snapshot
        .formulae
        .get(name)
        .or_else(|| snapshot.formulae.get(short))
    {
        return Ok(ResolvedPackage {
            name: if snapshot.formulae.contains_key(name) {
                name.to_string()
            } else {
                short.to_string()
            },
            kind: PackageKind::Formula,
            formula: Some(pkg.clone()),
        });
    }

    lookup_cask(cache, &snapshot.casks, name, short).await
}

async fn lookup_cask(
    _cache: &Cache,
    casks: &HashMap<String, InstalledCask>,
    name: &str,
    short: &str,
) -> Result<ResolvedPackage> {
    if casks.contains_key(name) || casks.contains_key(short) {
        return Ok(ResolvedPackage {
            name: if casks.contains_key(name) {
                name.to_string()
            } else {
                short.to_string()
            },
            kind: PackageKind::Cask,
            formula: None,
        });
    }

    #[cfg(target_os = "macos")]
    if let Ok(details) = _cache.fetch_cask_details(name).await {
        for artifact in details.artifacts.into_iter().flatten() {
            if let crate::api::CaskArtifact::App { app } = artifact {
                if let Some(target) = crate::cask::app_artifact_name(&app) {
                    for root in crate::discovery::macos_application_roots() {
                        let path = root.join(target);
                        if path.is_dir() {
                            return Err(manual_app_not_installed_error(
                                name,
                                &path.display().to_string(),
                            ));
                        }
                    }
                }
            }
        }
    }

    Err(WaxError::NotInstalled(name.to_string()))
}

#[cfg(target_os = "macos")]
fn manual_app_not_installed_error(requested_name: &str, app_name: &str) -> WaxError {
    WaxError::NotInstalled(format!(
        "{requested_name}\n  Found {app_name}, but it is not managed by Wax. Wax will not remove it automatically; remove it in Finder or with that app's supported uninstaller."
    ))
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::manual_app_not_installed_error;
    use super::short_package_name;

    #[cfg(target_os = "macos")]
    #[test]
    fn manual_app_suggestion_keeps_removal_explicit() {
        let error = manual_app_not_installed_error("raycast@beta", "Raycast Beta.app");
        assert!(error.to_string().contains("Raycast Beta.app"));
        assert!(error.to_string().contains("not managed by Wax"));
        assert!(error.to_string().contains("not remove it automatically"));
    }

    #[test]
    fn short_package_name_uses_last_segment() {
        assert_eq!(short_package_name("undivisible/tap/vro"), "vro");
        assert_eq!(short_package_name("vro"), "vro");
    }
}
