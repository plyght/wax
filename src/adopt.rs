//! Shared adoption of software installed outside Wax.
//!
//! Commands previously re-implemented Cellar sync, caskroom sync, and
//! `/Applications` discovery with inconsistent merge rules. This module is
//! the single resolve + adopt layer every lifecycle command should use.

use crate::cache::Cache;
use crate::cask::{CaskState, InstalledCask};
use crate::discovery::{
    discover_linux_system_packages, discover_manually_installed_casks, normalize_package_token,
};
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
    #[allow(dead_code)]
    pub cask: Option<InstalledCask>,
}

#[derive(Debug, Clone, Copy)]
pub struct AdoptOptions {
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

pub fn merge_discovered_casks(
    installed_casks: &mut HashMap<String, InstalledCask>,
    discovered_casks: HashMap<String, InstalledCask>,
    caskroom_synced_names: &HashSet<String>,
) {
    for (name, discovered) in discovered_casks {
        if let Some(app_key) = manual_app_key(&discovered) {
            let stale_names = installed_casks
                .iter()
                .filter_map(|(installed_name, installed)| {
                    if installed_name == &name || caskroom_synced_names.contains(installed_name) {
                        return None;
                    }
                    (manual_app_key(installed).as_deref() == Some(app_key.as_str()))
                        .then(|| installed_name.clone())
                })
                .collect::<Vec<_>>();
            for stale_name in stale_names {
                installed_casks.remove(&stale_name);
            }
        }

        installed_casks
            .entry(name.clone())
            .and_modify(|installed| {
                if !caskroom_synced_names.contains(&name) && discovered.version != "unknown" {
                    installed.version = discovered.version.clone();
                }
                if !caskroom_synced_names.contains(&name) && discovered.install_date > 0 {
                    installed.install_date = discovered.install_date;
                }
                if installed.artifact_type.is_none() {
                    installed.artifact_type = discovered.artifact_type.clone();
                }
                if installed.binary_paths.is_none() {
                    installed.binary_paths = discovered.binary_paths.clone();
                }
                if installed.app_name.is_none() {
                    installed.app_name = discovered.app_name.clone();
                }
            })
            .or_insert(discovered);
    }
}

fn manual_app_key(cask: &InstalledCask) -> Option<String> {
    if cask.artifact_type.as_deref() != Some("app") {
        return None;
    }

    cask.app_name
        .as_deref()
        .map(normalize_package_token)
        .filter(|name| !name.is_empty())
}

/// Sync formula installs from Cellar without requiring a package cache.
pub async fn sync_formulae() -> Result<HashMap<String, InstalledPackage>> {
    let state = InstallState::new()?;
    let _ = state.sync_from_cellar().await;
    state.load().await
}

pub async fn sync_installed_state(
    cache: &Cache,
    options: AdoptOptions,
) -> Result<InstalledSnapshot> {
    let mut snapshot = InstalledSnapshot::default();

    if options.formulae {
        let state = InstallState::new()?;
        state.sync_from_cellar().await.ok();
        let mut formulae = state.load().await?;

        if cfg!(target_os = "linux") {
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
        let caskroom_synced_names = cask_state.sync_from_caskrooms().await.unwrap_or_default();
        let mut casks = cask_state.load().await?;

        if cfg!(target_os = "macos") {
            let all_casks = cache.load_all_casks().await?;
            let discovered = discover_manually_installed_casks(&all_casks).await?;
            merge_discovered_casks(&mut casks, discovered, &caskroom_synced_names);
            if options.persist {
                cask_state.save(&casks).await?;
            }
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
        return lookup_cask(&snapshot.casks, name, short);
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
            cask: None,
        });
    }

    lookup_cask(&snapshot.casks, name, short)
}

fn lookup_cask(
    casks: &HashMap<String, InstalledCask>,
    name: &str,
    short: &str,
) -> Result<ResolvedPackage> {
    if let Some(cask) = casks.get(name).or_else(|| casks.get(short)) {
        return Ok(ResolvedPackage {
            name: if casks.contains_key(name) {
                name.to_string()
            } else {
                short.to_string()
            },
            kind: PackageKind::Cask,
            formula: None,
            cask: Some(cask.clone()),
        });
    }

    Err(WaxError::NotInstalled(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{merge_discovered_casks, short_package_name};
    use crate::cask::InstalledCask;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn short_package_name_uses_last_segment() {
        assert_eq!(short_package_name("undivisible/tap/vro"), "vro");
        assert_eq!(short_package_name("vro"), "vro");
    }

    #[test]
    fn merge_discovered_casks_updates_existing_versions() {
        let mut installed = HashMap::from([(
            "example-cask".to_string(),
            InstalledCask {
                name: "example-cask".to_string(),
                version: "1.0.0".to_string(),
                install_date: 1,
                artifact_type: Some("dmg".to_string()),
                binary_paths: None,
                app_name: Some("Example.app".to_string()),
                installed_paths: Vec::new(),
            },
        )]);
        let discovered = HashMap::from([(
            "example-cask".to_string(),
            InstalledCask {
                name: "example-cask".to_string(),
                version: "2.0.0".to_string(),
                install_date: 2,
                artifact_type: Some("app".to_string()),
                binary_paths: None,
                app_name: Some("Example".to_string()),
                installed_paths: Vec::new(),
            },
        )]);

        merge_discovered_casks(&mut installed, discovered, &HashSet::new());

        let cask = installed.get("example-cask").unwrap();
        assert_eq!(cask.version, "2.0.0");
        assert_eq!(cask.install_date, 2);
        assert_eq!(cask.artifact_type.as_deref(), Some("dmg"));
        assert_eq!(cask.app_name.as_deref(), Some("Example.app"));
    }

    #[test]
    fn merge_discovered_casks_preserves_caskroom_synced_versions() {
        let mut installed = HashMap::from([(
            "example-cask".to_string(),
            InstalledCask {
                name: "example-cask".to_string(),
                version: "2.0.0".to_string(),
                install_date: 2,
                artifact_type: Some("dmg".to_string()),
                binary_paths: None,
                app_name: Some("Example.app".to_string()),
                installed_paths: Vec::new(),
            },
        )]);
        let discovered = HashMap::from([(
            "example-cask".to_string(),
            InstalledCask {
                name: "example-cask".to_string(),
                version: "1.0.0".to_string(),
                install_date: 1,
                artifact_type: Some("app".to_string()),
                binary_paths: None,
                app_name: Some("Example".to_string()),
                installed_paths: Vec::new(),
            },
        )]);

        merge_discovered_casks(
            &mut installed,
            discovered,
            &HashSet::from(["example-cask".to_string()]),
        );

        let cask = installed.get("example-cask").unwrap();
        assert_eq!(cask.version, "2.0.0");
        assert_eq!(cask.install_date, 2);
    }

    #[test]
    fn merge_discovered_casks_replaces_stale_manual_app_token() {
        let mut installed = HashMap::from([(
            "example".to_string(),
            InstalledCask {
                name: "example".to_string(),
                version: "1.0.0".to_string(),
                install_date: 1,
                artifact_type: Some("app".to_string()),
                binary_paths: None,
                app_name: Some("Example".to_string()),
                installed_paths: Vec::new(),
            },
        )]);
        let discovered = HashMap::from([(
            "vendor-example".to_string(),
            InstalledCask {
                name: "vendor-example".to_string(),
                version: "2.0.0".to_string(),
                install_date: 2,
                artifact_type: Some("app".to_string()),
                binary_paths: None,
                app_name: Some("Example.app".to_string()),
                installed_paths: Vec::new(),
            },
        )]);

        merge_discovered_casks(&mut installed, discovered, &HashSet::new());

        assert!(!installed.contains_key("example"));
        assert_eq!(installed.get("vendor-example").unwrap().version, "2.0.0");
    }

    #[test]
    fn merge_discovered_casks_keeps_caskroom_synced_same_app_token() {
        let mut installed = HashMap::from([(
            "example".to_string(),
            InstalledCask {
                name: "example".to_string(),
                version: "1.0.0".to_string(),
                install_date: 1,
                artifact_type: Some("app".to_string()),
                binary_paths: None,
                app_name: Some("Example".to_string()),
                installed_paths: Vec::new(),
            },
        )]);
        let discovered = HashMap::from([(
            "vendor-example".to_string(),
            InstalledCask {
                name: "vendor-example".to_string(),
                version: "2.0.0".to_string(),
                install_date: 2,
                artifact_type: Some("app".to_string()),
                binary_paths: None,
                app_name: Some("Example".to_string()),
                installed_paths: Vec::new(),
            },
        )]);

        merge_discovered_casks(
            &mut installed,
            discovered,
            &HashSet::from(["example".to_string()]),
        );

        assert!(installed.contains_key("example"));
        assert!(installed.contains_key("vendor-example"));
    }
}
