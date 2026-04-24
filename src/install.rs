use crate::bottle::{detect_platform, homebrew_prefix, run_command_with_timeout};
use crate::error::{Result, WaxError};
use crate::sudo;
use crate::ui::dirs;
use crate::version::sort_versions;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, instrument};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallMode {
    User,
    Global,
}

impl InstallMode {
    pub fn detect() -> Self {
        let prefix = homebrew_prefix();

        let cellar = prefix.join("Cellar");
        if (cellar.exists() || prefix.exists()) && is_writable(&prefix) {
            return InstallMode::Global;
        }

        InstallMode::User
    }

    pub fn from_flags(user: bool, global: bool) -> Result<Option<Self>> {
        match (user, global) {
            (true, true) => Err(WaxError::InstallError(
                "Cannot specify both --user and --global".to_string(),
            )),
            (true, false) => Ok(Some(InstallMode::User)),
            (false, true) => Ok(Some(InstallMode::Global)),
            (false, false) => Ok(None),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if *self == InstallMode::Global {
            let prefix = homebrew_prefix();
            if !is_writable(&prefix) {
                return Err(WaxError::InstallError(format!(
                    "Cannot write to {}. This usually means:\n  \
                     - You don't have permission (try: sudo wax install or wax install --user)\n  \
                     - The directory doesn't exist (Homebrew may not be installed)\n\n  \
                     For per-user installation: wax install --user",
                    prefix.display()
                )));
            }
        }
        Ok(())
    }

    pub fn prefix(&self) -> Result<PathBuf> {
        match self {
            InstallMode::User => Ok(dirs::home_dir()?.join(".local").join("wax")),
            InstallMode::Global => Ok(homebrew_prefix()),
        }
    }

    pub fn cellar_path(&self) -> Result<PathBuf> {
        Ok(self.prefix()?.join("Cellar"))
    }
}

fn is_writable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use nix::unistd::{getgid, getuid};
        use std::os::unix::fs::MetadataExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mode = metadata.mode();
            let uid = getuid();

            if uid.is_root() {
                return true;
            }

            let uid_raw = uid.as_raw();

            // Owner write
            if metadata.uid() == uid_raw {
                return mode & 0o200 != 0;
            }

            // Check primary and supplementary groups
            if mode & 0o020 != 0 {
                let file_gid = metadata.gid();
                let primary_gid = getgid().as_raw();
                if file_gid == primary_gid {
                    return true;
                }
                // Check supplementary groups using libc (nix getgroups is not available on macOS).
                // SAFETY: getgroups with a zero-length buffer and null pointer is a valid probe.
                let ngroups = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
                if ngroups > 0 {
                    let mut groups = vec![0u32; ngroups as usize];
                    // SAFETY: groups vector is correctly sized and its pointer is valid for writes.
                    let n = unsafe { libc::getgroups(ngroups, groups.as_mut_ptr()) };
                    if n > 0 && groups[..n as usize].contains(&file_gid) {
                        return true;
                    }
                }
            }

            // Other write
            return mode & 0o002 != 0;
        }
    }

    // Fallback: actually try to create a file (also used on non-unix)
    let test_file = path.join(".wax_write_test");
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&test_file);

    if result.is_ok() {
        let _ = std::fs::remove_file(&test_file);
        true
    } else {
        false
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub install_date: i64,
    #[serde(default = "default_install_mode")]
    pub install_mode: InstallMode,
    #[serde(default)]
    pub from_source: bool,
    #[serde(default)]
    pub bottle_rebuild: u32,
    #[serde(default)]
    pub bottle_sha256: Option<String>,
    #[serde(default)]
    pub pinned: bool,
}

fn default_install_mode() -> InstallMode {
    InstallMode::Global
}

pub struct InstallState {
    state_path: PathBuf,
}

impl InstallState {
    pub fn new() -> Result<Self> {
        let state_path = dirs::wax_dir()?.join("installed.json");
        Ok(Self { state_path })
    }

    pub async fn load(&self) -> Result<HashMap<String, InstalledPackage>> {
        match fs::read_to_string(&self.state_path).await {
            Ok(json) => {
                let packages: HashMap<String, InstalledPackage> = serde_json::from_str(&json)?;
                Ok(packages)
            }
            Err(_) => Ok(HashMap::new()),
        }
    }

    pub async fn save(&self, packages: &HashMap<String, InstalledPackage>) -> Result<()> {
        let parent = self
            .state_path
            .parent()
            .ok_or_else(|| WaxError::CacheError("Cannot determine parent directory".into()))?;
        fs::create_dir_all(parent).await?;

        let json = serde_json::to_string_pretty(packages)?;
        fs::write(&self.state_path, json).await?;
        Ok(())
    }

    pub async fn add(&self, package: InstalledPackage) -> Result<()> {
        let mut packages = self.load().await?;
        packages.insert(package.name.clone(), package);
        self.save(&packages).await?;
        Ok(())
    }

    pub async fn remove(&self, name: &str) -> Result<()> {
        let mut packages = self.load().await?;
        packages.remove(name);
        self.save(&packages).await?;
        Ok(())
    }

    pub async fn set_pinned(&self, name: &str, pinned: bool) -> Result<()> {
        let mut packages = self.load().await?;
        if let Some(pkg) = packages.get_mut(name) {
            pkg.pinned = pinned;
            self.save(&packages).await?;
        }
        Ok(())
    }

    pub async fn load_formulae_from_cache(&self) -> Result<Vec<crate::api::Formula>> {
        let cache = crate::cache::Cache::new()?;
        cache.load_all_formulae().await
    }

    fn detect_install_mode(&self, cellar: &Path) -> InstallMode {
        if cellar.starts_with("/opt/homebrew") || cellar.starts_with("/usr/local") {
            InstallMode::Global
        } else {
            InstallMode::User
        }
    }

    pub async fn sync_from_cellar(&self) -> Result<()> {
        let mut packages = self.load().await?;

        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        let mut candidates = match os {
            "macos" => match arch {
                "aarch64" => vec![PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")],
                _ => vec![PathBuf::from("/usr/local"), PathBuf::from("/opt/homebrew")],
            },
            "linux" => vec![
                PathBuf::from("/home/linuxbrew/.linuxbrew"),
                PathBuf::from("/usr/local"),
            ],
            _ => vec![PathBuf::from("/usr/local")],
        };

        if let Some(prefix_str) = run_command_with_timeout("brew", &["--prefix"], 2) {
            candidates.push(PathBuf::from(prefix_str.trim()));
        }

        // De-duplicate candidates
        let mut seen = std::collections::HashSet::new();
        candidates.retain(|p| seen.insert(p.clone()));

        for path in candidates {
            let cellar = path.join("Cellar");
            if cellar.exists() {
                self.scan_cellar_and_update(&cellar, &mut packages).await?;
            }
        }

        if let Ok(home) = dirs::home_dir() {
            let wax_user_cellar = home.join(".local/wax/Cellar");
            if wax_user_cellar.exists() {
                self.scan_cellar_and_update(&wax_user_cellar, &mut packages)
                    .await?;
            }
        }

        self.save(&packages).await?;
        Ok(())
    }

    async fn scan_cellar_and_update(
        &self,
        cellar: &Path,
        packages: &mut HashMap<String, InstalledPackage>,
    ) -> Result<()> {
        let mut entries = tokio::fs::read_dir(cellar).await?;

        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                let package_name = entry.file_name().to_string_lossy().to_string();

                let mut versions = Vec::new();
                let mut version_entries = tokio::fs::read_dir(entry.path()).await?;
                while let Some(version_entry) = version_entries.next_entry().await? {
                    if version_entry.file_type().await?.is_dir() {
                        versions.push(version_entry.file_name().to_string_lossy().to_string());
                    }
                }

                if !versions.is_empty() {
                    sort_versions(&mut versions);
                    let version = versions.last().unwrap().clone();

                    if let Some(existing) = packages.get_mut(&package_name) {
                        existing.version = version;
                    } else {
                        packages.insert(
                            package_name.clone(),
                            InstalledPackage {
                                name: package_name,
                                version,
                                platform: detect_platform(),
                                install_date: 0,
                                install_mode: self.detect_install_mode(cellar),
                                from_source: false,
                                bottle_rebuild: 0,
                                bottle_sha256: None,
                                pinned: false,
                            },
                        );
                    }
                }
            }
        }

        Ok(())
    }
}

impl Default for InstallState {
    fn default() -> Self {
        Self::new().expect("Failed to initialize install state")
    }
}

#[instrument(skip(cellar_path))]
pub async fn create_symlinks(
    formula_name: &str,
    version: &str,
    cellar_path: &Path,
    dry_run: bool,
    install_mode: InstallMode,
) -> Result<Vec<PathBuf>> {
    debug!(
        "Creating symlinks for {} {} (dry_run={}, mode={:?})",
        formula_name, version, dry_run, install_mode
    );

    let formula_path = cellar_path.join(formula_name).join(version);
    if !formula_path.exists() {
        return Err(WaxError::InstallError(format!(
            "Formula path does not exist: {}",
            formula_path.display()
        )));
    }
    let formula_path = dunce::canonicalize(&formula_path).unwrap_or(formula_path);

    let prefix = install_mode.prefix()?;

    let mut created_links = Vec::new();

    let link_dirs = vec![
        ("bin", prefix.join("bin")),
        ("lib", prefix.join("lib")),
        ("include", prefix.join("include")),
        ("share", prefix.join("share")),
        ("etc", prefix.join("etc")),
        ("sbin", prefix.join("sbin")),
    ];

    for (subdir, target_dir) in link_dirs {
        let source_dir = formula_path.join(subdir);

        if !source_dir.exists() {
            continue;
        }

        if !dry_run {
            fs::create_dir_all(&target_dir)
                .await
                .or_else(|_| sudo::sudo_mkdir(&target_dir))?;
        }

        link_directory_recursive(
            &source_dir,
            &target_dir,
            &formula_path,
            dry_run,
            &mut created_links,
        )
        .await?;
    }

    let opt_dir = prefix.join("opt");
    if !dry_run {
        fs::create_dir_all(&opt_dir)
            .await
            .or_else(|_| sudo::sudo_mkdir(&opt_dir))?;
    }
    let opt_link = opt_dir.join(formula_name);
    if !dry_run && opt_link.symlink_metadata().is_ok() {
        if opt_link.is_dir() && !opt_link.is_symlink() {
            fs::remove_dir_all(&opt_link)
                .await
                .or_else(|_| sudo::sudo_remove(&opt_link).map(|_| ()))?;
        } else {
            fs::remove_file(&opt_link)
                .await
                .or_else(|_| sudo::sudo_remove(&opt_link).map(|_| ()))?;
        }
    }
    if !dry_run {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&formula_path, &opt_link)
                .or_else(|_| sudo::sudo_symlink(&formula_path, &opt_link).map(|_| ()))?;
        }
        created_links.push(opt_link);
    }

    debug!("Created {} symlinks", created_links.len());
    Ok(created_links)
}

fn link_directory_recursive<'a>(
    source_dir: &'a Path,
    target_dir: &'a Path,
    formula_base: &'a Path,
    dry_run: bool,
    created_links: &'a mut Vec<PathBuf>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let mut entries = fs::read_dir(source_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_name = entry.file_name();
            let source_path = entry.path();
            let target_path = target_dir.join(&file_name);
            let source_meta = entry.metadata().await?;

            // Safety check: ensure source is actually inside the formula path
            if !source_path.starts_with(formula_base) {
                debug!(
                    "Skipping symlink for path outside formula: {:?}",
                    source_path
                );
                continue;
            }

            if source_meta.is_dir() {
                if let Ok(target_meta) = fs::symlink_metadata(&target_path).await {
                    if target_meta.is_dir() && !target_meta.is_symlink() {
                        link_directory_recursive(
                            &source_path,
                            &target_path,
                            formula_base,
                            dry_run,
                            created_links,
                        )
                        .await?;
                        continue;
                    }
                    if !dry_run {
                        debug!("Removing existing symlink/file at {:?}", target_path);
                        fs::remove_file(&target_path)
                            .await
                            .or_else(|_| sudo::sudo_remove(&target_path).map(|_| ()))?;
                    }
                }

                if !dry_run {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::symlink;
                        symlink(&source_path, &target_path).or_else(|_| {
                            sudo::sudo_symlink(&source_path, &target_path).map(|_| ())
                        })?;
                    }
                    #[cfg(not(unix))]
                    {
                        return Err(WaxError::PlatformNotSupported(
                            "Symlinks not supported on this platform".to_string(),
                        ));
                    }
                }
                created_links.push(target_path);
            } else {
                if target_path.symlink_metadata().is_ok() {
                    if !dry_run {
                        debug!("Removing existing symlink/file at {:?}", target_path);
                        fs::remove_file(&target_path)
                            .await
                            .or_else(|_| sudo::sudo_remove(&target_path).map(|_| ()))?;
                    } else {
                        debug!("Symlink target already exists: {:?}", target_path);
                        continue;
                    }
                }

                if !dry_run {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::symlink;
                        symlink(&source_path, &target_path).or_else(|_| {
                            sudo::sudo_symlink(&source_path, &target_path).map(|_| ())
                        })?;
                    }
                    #[cfg(not(unix))]
                    {
                        return Err(WaxError::PlatformNotSupported(
                            "Symlinks not supported on this platform".to_string(),
                        ));
                    }
                }
                created_links.push(target_path);
            }
        }
        Ok(())
    })
}

#[instrument(skip(cellar_path))]
pub async fn remove_symlinks(
    formula_name: &str,
    version: &str,
    cellar_path: &Path,
    dry_run: bool,
    install_mode: InstallMode,
) -> Result<Vec<PathBuf>> {
    debug!(
        "Removing symlinks for {} {} (dry_run={}, mode={:?})",
        formula_name, version, dry_run, install_mode
    );

    let formula_path = cellar_path.join(formula_name).join(version);
    let formula_path = dunce::canonicalize(&formula_path).unwrap_or(formula_path);
    let prefix = install_mode.prefix()?;

    let mut removed_links = Vec::new();

    let link_dirs = vec![
        ("bin", prefix.join("bin")),
        ("lib", prefix.join("lib")),
        ("include", prefix.join("include")),
        ("share", prefix.join("share")),
        ("etc", prefix.join("etc")),
        ("sbin", prefix.join("sbin")),
    ];

    for (subdir, target_dir) in link_dirs {
        let source_dir = formula_path.join(subdir);

        unlink_directory_recursive(
            &source_dir,
            &target_dir,
            &formula_path,
            dry_run,
            &mut removed_links,
        )
        .await?;
    }

    let opt_link = prefix.join("opt").join(formula_name);
    #[cfg(unix)]
    {
        if let Ok(metadata) = fs::symlink_metadata(&opt_link).await {
            if metadata.is_symlink() {
                if let Ok(link_target) = fs::read_link(&opt_link).await {
                    let link_target = dunce::canonicalize(&link_target).unwrap_or(link_target);
                    if link_target.starts_with(&formula_path) {
                        if !dry_run {
                            fs::remove_file(&opt_link)
                                .await
                                .or_else(|_| sudo::sudo_remove(&opt_link).map(|_| ()))?;
                        }
                        removed_links.push(opt_link);
                    }
                }
            }
        }
    }

    debug!("Removed {} symlinks", removed_links.len());
    Ok(removed_links)
}

fn unlink_directory_recursive<'a>(
    source_dir: &'a Path,
    target_dir: &'a Path,
    formula_path: &'a Path,
    dry_run: bool,
    removed_links: &'a mut Vec<PathBuf>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let mut entries = match fs::read_dir(source_dir).await {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };

        while let Some(entry) = entries.next_entry().await? {
            let file_name = entry.file_name();
            let source_path = entry.path();
            let target_path = target_dir.join(&file_name);

            let target_meta = match fs::symlink_metadata(&target_path).await {
                Ok(m) => m,
                Err(_) => continue,
            };

            #[cfg(unix)]
            {
                if target_meta.is_symlink() {
                    if let Ok(link_target) = fs::read_link(&target_path).await {
                        let link_target = dunce::canonicalize(&link_target).unwrap_or(link_target);
                        if link_target.starts_with(formula_path) {
                            if !dry_run {
                                fs::remove_file(&target_path)
                                    .await
                                    .or_else(|_| sudo::sudo_remove(&target_path).map(|_| ()))?;
                            }
                            removed_links.push(target_path);
                        }
                    }
                } else if target_meta.is_dir() && source_path.is_dir() {
                    unlink_directory_recursive(
                        &source_path,
                        &target_path,
                        formula_path,
                        dry_run,
                        removed_links,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    })
}
