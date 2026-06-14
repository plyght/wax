use crate::error::{Result, WaxError};
use crate::package_spec::Ecosystem;
use crate::ui::dirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowsPackageManifest {
    pub ecosystem: Ecosystem,
    pub id: String,
    pub version: String,
    pub source: String,
    pub staging_dir: PathBuf,
    pub bin_links: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
    #[serde(default)]
    pub install_kind: WindowsInstallKind,
    #[serde(default)]
    pub native_uninstall: Option<WindowsNativeUninstall>,
    pub installed_at: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowsInstallKind {
    #[default]
    Portable,
    Native,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WindowsNativeUninstall {
    pub command: String,
    pub args: Vec<String>,
}

pub fn wax_windows_root() -> Result<PathBuf> {
    Ok(dirs::home_dir()?.join(".local").join("wax"))
}

pub fn wax_bin_dir() -> Result<PathBuf> {
    Ok(wax_windows_root()?.join("bin"))
}

fn manifest_dir() -> Result<PathBuf> {
    Ok(wax_windows_root()?.join("windows").join("manifests"))
}

fn manifest_path(ecosystem: Ecosystem, id: &str) -> Result<PathBuf> {
    let safe_id: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    Ok(manifest_dir()?.join(format!("{}-{}.json", ecosystem.label(), safe_id)))
}

fn path_is_under_root(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

pub fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_files_inner(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files_inner(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_files_inner(&path, files)?;
        } else if file_type.is_file() || file_type.is_symlink() {
            files.push(path);
        }
    }
    Ok(())
}

impl WindowsPackageManifest {
    pub fn new(
        ecosystem: Ecosystem,
        id: impl Into<String>,
        version: impl Into<String>,
        source: impl Into<String>,
        staging_dir: PathBuf,
        bin_links: Vec<PathBuf>,
        files: Vec<PathBuf>,
    ) -> Self {
        Self {
            ecosystem,
            id: id.into(),
            version: version.into(),
            source: source.into(),
            staging_dir,
            bin_links,
            files,
            install_kind: WindowsInstallKind::Portable,
            native_uninstall: None,
            installed_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        }
    }

    pub fn with_native_uninstall(mut self, uninstall: WindowsNativeUninstall) -> Self {
        self.install_kind = WindowsInstallKind::Native;
        self.native_uninstall = Some(uninstall);
        self
    }

    pub fn save(&self) -> Result<()> {
        let path = manifest_path(self.ecosystem, &self.id)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(self).map_err(WaxError::JsonError)?;
        std::fs::write(path, raw)?;
        Ok(())
    }
}

pub fn load_manifest(ecosystem: Ecosystem, id: &str) -> Result<Option<WindowsPackageManifest>> {
    let path = manifest_path(ecosystem, id)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)?;
    Ok(Some(
        serde_json::from_str(&raw).map_err(WaxError::JsonError)?,
    ))
}

pub fn list_manifests() -> Result<Vec<WindowsPackageManifest>> {
    let dir = manifest_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut manifests: Vec<WindowsPackageManifest> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|s| s.to_str()) == Some("json")
        {
            let raw = std::fs::read_to_string(entry.path())?;
            manifests.push(serde_json::from_str(&raw).map_err(WaxError::JsonError)?);
        }
    }
    manifests.sort_by(|a, b| {
        a.id.to_ascii_lowercase()
            .cmp(&b.id.to_ascii_lowercase())
            .then_with(|| a.ecosystem.cmp(&b.ecosystem))
    });
    Ok(manifests)
}

pub fn find_manifest(raw: &str) -> Result<Option<WindowsPackageManifest>> {
    let spec = crate::package_spec::parse_package_spec(raw);
    if let Some(ecosystem @ (Ecosystem::Scoop | Ecosystem::Winget | Ecosystem::Chocolatey)) =
        spec.force
    {
        return load_manifest(ecosystem, &spec.name);
    }

    let matches: Vec<_> = list_manifests()?
        .into_iter()
        .filter(|m| m.id.eq_ignore_ascii_case(raw))
        .collect();
    if matches.len() > 1 {
        return Err(WaxError::InvalidInput(format!(
            "multiple Windows packages match '{raw}'; use scoop/{raw}, winget/{raw}, or choco/{raw}"
        )));
    }
    Ok(matches.into_iter().next())
}

pub fn validate_bin_links_available(
    ecosystem: Ecosystem,
    id: &str,
    links: &[PathBuf],
) -> Result<()> {
    for manifest in list_manifests()? {
        if manifest.ecosystem == ecosystem && manifest.id.eq_ignore_ascii_case(id) {
            continue;
        }
        for link in links {
            if manifest.bin_links.iter().any(|existing| existing == link) {
                return Err(WaxError::InstallError(format!(
                    "binary link {} is already owned by {}/{}",
                    link.display(),
                    manifest.ecosystem.label(),
                    manifest.id
                )));
            }
        }
    }
    Ok(())
}

pub fn remove_manifest(manifest: &WindowsPackageManifest, dry_run: bool) -> Result<Vec<PathBuf>> {
    let root = wax_windows_root()?;
    let mut removed = Vec::new();
    for path in manifest.bin_links.iter().chain(manifest.files.iter()) {
        if !path_is_under_root(path, &root) {
            return Err(WaxError::InstallError(format!(
                "refusing to remove path outside wax root: {}",
                path.display()
            )));
        }
        if path.exists() || path.is_symlink() {
            removed.push(path.clone());
            if !dry_run {
                std::fs::remove_file(path)?;
            }
        }
    }

    if path_is_under_root(&manifest.staging_dir, &root) && manifest.staging_dir.exists() {
        removed.push(manifest.staging_dir.clone());
        if !dry_run {
            std::fs::remove_dir_all(&manifest.staging_dir)?;
        }
    }

    let path = manifest_path(manifest.ecosystem, &manifest.id)?;
    if path.exists() {
        removed.push(path.clone());
        if !dry_run {
            std::fs::remove_file(path)?;
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn collect_files_recurses_and_sorts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/tool.exe"), b"tool").unwrap();
        std::fs::write(root.join("readme.txt"), b"readme").unwrap();

        let files = collect_files(root).unwrap();
        let names: Vec<_> = files
            .iter()
            .map(|path| {
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        assert_eq!(names, vec!["bin/tool.exe", "readme.txt"]);
    }

    #[test]
    fn manifests_roundtrip_and_list_sorted() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        let one = WindowsPackageManifest::new(
            Ecosystem::Winget,
            "Zoo.Tool",
            "1.0.0",
            "https://example.invalid/zoo.zip",
            wax_windows_root()
                .unwrap()
                .join("winget-apps/Zoo.Tool/1.0.0"),
            Vec::new(),
            Vec::new(),
        );
        let two = WindowsPackageManifest::new(
            Ecosystem::Scoop,
            "alpha",
            "2.0.0",
            "https://example.invalid/alpha.zip",
            wax_windows_root().unwrap().join("scoop-apps/alpha/2.0.0"),
            Vec::new(),
            Vec::new(),
        );
        one.save().unwrap();
        two.save().unwrap();

        let loaded = load_manifest(Ecosystem::Winget, "Zoo.Tool")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.version, "1.0.0");
        let listed = list_manifests().unwrap();
        assert_eq!(listed[0].id, "alpha");
        assert_eq!(listed[1].id, "Zoo.Tool");
    }

    #[test]
    fn remove_manifest_deletes_only_owned_wax_paths() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        let root = wax_windows_root().unwrap();
        let staging = root.join("scoop-apps/tool/1.0.0");
        let bin = root.join("bin/tool.exe");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(staging.join("tool.exe"), b"tool").unwrap();
        std::fs::write(&bin, b"tool").unwrap();
        let manifest = WindowsPackageManifest::new(
            Ecosystem::Scoop,
            "tool",
            "1.0.0",
            "https://example.invalid/tool.zip",
            staging.clone(),
            vec![bin.clone()],
            vec![staging.join("tool.exe")],
        );
        manifest.save().unwrap();

        let removed = remove_manifest(&manifest, false).unwrap();
        assert!(removed.iter().any(|path| path == &bin));
        assert!(!bin.exists());
        assert!(!staging.exists());
        assert!(load_manifest(Ecosystem::Scoop, "tool").unwrap().is_none());
    }

    #[test]
    fn bin_link_collision_reports_existing_owner() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        let bin = wax_bin_dir().unwrap().join("tool.exe");
        let manifest = WindowsPackageManifest::new(
            Ecosystem::Scoop,
            "tool",
            "1.0.0",
            "https://example.invalid/tool.zip",
            wax_windows_root().unwrap().join("scoop-apps/tool/1.0.0"),
            vec![bin.clone()],
            Vec::new(),
        );
        manifest.save().unwrap();

        let err = validate_bin_links_available(Ecosystem::Winget, "Other.Tool", &[bin])
            .unwrap_err()
            .to_string();
        assert!(err.contains("already owned by scoop/tool"));
    }
}
