use crate::digest::{sha256_digest_hex, sha256_file};
use crate::error::{validate_package_name, Result, WaxError};
use crate::install::InstallState;
use crate::provenance::{self, Manager, Observation};
use crate::ui::dirs;
use clap::Subcommand;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum Action {
    /// Inspect executable origins and PATH precedence without executing them.
    Inspect {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Save an observation, without claiming another manager's package.
    Record { entrypoint: PathBuf },
    /// Select an installed Wax formula through a backed-up user entrypoint.
    Replace {
        entrypoint: PathBuf,
        #[arg(long = "with")]
        package: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Restore the entrypoint preserved by a replacement.
    Restore {
        entrypoint: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EntryIdentity {
    Missing,
    File { sha256: String },
    Symlink { target: PathBuf },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    Active,
    Restoring,
    Restored,
}

#[derive(Debug, Serialize, Deserialize)]
struct Replacement {
    package: String,
    target: PathBuf,
    backup: PathBuf,
    before: EntryIdentity,
    phase: Phase,
}

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    schema_version: u32,
    entrypoint: PathBuf,
    observations: Vec<Observation>,
    path_candidates: Vec<Observation>,
    replacement: Option<Replacement>,
}

fn conflict(message: impl Into<String>) -> WaxError {
    WaxError::InvalidInput(message.into())
}

fn registry() -> Result<PathBuf> {
    Ok(dirs::wax_dir()?.join("provenance"))
}

fn key(path: &Path) -> String {
    sha256_digest_hex(path.as_os_str().as_encoded_bytes())
}

fn record_path(path: &Path) -> Result<PathBuf> {
    Ok(registry()?.join(format!("{}.json", key(path))))
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    path.with_file_name(format!(".wax-{}-{suffix}", key(path)))
}

fn load(path: &Path) -> Result<Record> {
    match fs::read(record_path(path)?) {
        Ok(bytes) => {
            let record: Record = serde_json::from_slice(&bytes)?;
            if record.schema_version != 1 || record.entrypoint != path {
                return Err(conflict("Unsupported or mismatched provenance record"));
            }
            if record
                .replacement
                .as_ref()
                .is_some_and(|r| r.backup != sibling(path, "backup"))
            {
                return Err(conflict("Invalid provenance backup path"));
            }
            Ok(record)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Record {
            schema_version: 1,
            entrypoint: path.to_path_buf(),
            observations: Vec::new(),
            path_candidates: Vec::new(),
            replacement: None,
        }),
        Err(e) => Err(e.into()),
    }
}

fn save(record: &Record) -> Result<()> {
    let root = registry()?;
    let mut temp = tempfile::NamedTempFile::new_in(&root)?;
    temp.write_all(&serde_json::to_vec_pretty(record)?)?;
    temp.as_file().sync_all()?;
    temp.persist(record_path(&record.entrypoint)?)
        .map_err(|e| e.error)?;
    File::open(root)?.sync_all()?;
    Ok(())
}

pub struct RegistryLock {
    _file: File,
}

pub(crate) fn lock() -> Result<RegistryLock> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        fs::create_dir_all(registry()?)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(registry()?.join(".lock"))?;
        // SAFETY: the open file owns this descriptor for the lock's lifetime.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(conflict("Another Wax provenance operation is running"));
        }
        Ok(RegistryLock { _file: file })
    }
    #[cfg(not(unix))]
    Err(WaxError::PlatformNotSupported(
        "Provenance recording/replacement currently requires Unix".into(),
    ))
}

fn identity(path: &Path) -> Result<EntryIdentity> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() => Ok(EntryIdentity::Symlink {
            target: fs::read_link(path)?,
        }),
        Ok(meta) if meta.is_file() => Ok(EntryIdentity::File {
            sha256: sha256_file(path)?,
        }),
        Ok(_) => Err(conflict("Entrypoint must be a regular file or symlink")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(EntryIdentity::Missing),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn validate_entrypoint(path: &Path) -> Result<()> {
    validate_user_path(path)?;
    identity(path)?;
    Ok(())
}

pub(crate) fn validate_user_path(path: &Path) -> Result<()> {
    let home = fs::canonicalize(dirs::home_dir()?)?;
    let parent = path.parent().ok_or_else(|| conflict("Missing parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| conflict("Missing executable name"))?;
    if !path.is_absolute()
        || !path.starts_with(&home)
        || fs::canonicalize(parent)?.join(name) != path
    {
        return Err(conflict(
            "Use an absolute entrypoint under HOME with no symlinked parents",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(parent)?;
        if meta.uid() != nix::unistd::getuid().as_raw() || meta.mode() & 0o022 != 0 {
            return Err(conflict(
                "Entrypoint parent must be user-owned and not group/world-writable",
            ));
        }
        if let Ok(meta) = fs::symlink_metadata(path) {
            if meta.uid() != nix::unistd::getuid().as_raw() {
                return Err(conflict("Entrypoint is not owned by the current user"));
            }
        }
    }
    Ok(())
}

pub(crate) fn move_without_overwrite(from: &Path, to: &Path) -> Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from =
            CString::new(from.as_os_str().as_bytes()).map_err(|_| conflict("NUL in path"))?;
        let to = CString::new(to.as_os_str().as_bytes()).map_err(|_| conflict("NUL in path"))?;
        // SAFETY: both C strings live through the call; no-overwrite preserves intervening files.
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        // SAFETY: both C strings live through the call, as above.
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (from, to);
        Err(WaxError::PlatformNotSupported(
            "Safe replacement currently requires macOS or Linux".into(),
        ))
    }
}

async fn replace(path: &Path, package: &str, dry_run: bool) -> Result<Record> {
    validate_entrypoint(path)?;
    validate_package_name(package)?;
    let _lock = if dry_run { None } else { Some(lock()?) };
    let mut record = load(path)?;
    if let Some(r) = &record.replacement {
        if r.phase == Phase::Active
            && r.package == package
            && identity(path)?
                == (EntryIdentity::Symlink {
                    target: r.target.clone(),
                })
        {
            return Ok(record);
        }
        if r.phase != Phase::Restored {
            return Err(conflict(
                "Restore the previous replacement before replacing again",
            ));
        }
    }
    let observed = provenance::observe(path).await?;
    let home = dirs::home_dir()?;
    let cargo = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cargo"));
    let bun = std::env::var_os("BUN_INSTALL")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".bun"));
    let reserved = [
        cargo,
        bun,
        home.join(".local/wax"),
        home.join(".local/share/pnpm"),
    ];
    if reserved.iter().any(|root| path.starts_with(root))
        || !matches!(
            observed.origin.manager,
            Manager::Standalone | Manager::Unknown
        )
        || !matches!(observed.status.as_str(), "ok" | "missing" | "broken")
    {
        return Err(conflict(
            "Manager-owned entrypoint: preserve it and select a separate user shim instead",
        ));
    }
    let packages = InstallState::new()?.load().await?;
    let package = crate::adopt::short_package_name(package);
    let installed = packages
        .get(package)
        .ok_or_else(|| WaxError::NotInstalled(package.into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| conflict("Missing executable name"))?;
    let prefix = installed.install_mode.prefix()?;
    let target = prefix.join("opt").join(package).join("bin").join(name);
    let expected = prefix
        .join("Cellar")
        .join(package)
        .join(&installed.version)
        .join("bin")
        .join(name);
    if fs::canonicalize(&target)? != fs::canonicalize(&expected)? {
        return Err(conflict(
            "Wax opt link does not match the installed package receipt",
        ));
    }
    if provenance::observe(&target).await?.status != "ok" {
        return Err(conflict("Wax target is not executable"));
    }
    let before = identity(path)?;
    let backup = sibling(path, "backup");
    if fs::symlink_metadata(&backup).is_ok()
        || fs::symlink_metadata(sibling(path, "managed")).is_ok()
    {
        return Err(conflict(
            "A preserved backup already exists; restore/recover it first",
        ));
    }
    record.observations.push(observed);
    record.path_candidates = provenance::inspect(&name.to_string_lossy()).await?;
    record.replacement = Some(Replacement {
        package: package.into(),
        target: target.clone(),
        backup: backup.clone(),
        before: before.clone(),
        phase: Phase::Prepared,
    });
    if dry_run {
        return Ok(record);
    }
    save(&record)?;
    if identity(path)? != before {
        return Err(conflict(
            "Entrypoint changed while planning; nothing was replaced",
        ));
    }
    if before != EntryIdentity::Missing {
        move_without_overwrite(path, &backup)?;
        if identity(&backup)? != before {
            move_without_overwrite(&backup, path)?;
            return Err(conflict(
                "Entrypoint changed during backup; replacement refused",
            ));
        }
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, path)?;
    if let Some(r) = &mut record.replacement {
        r.phase = Phase::Active;
    }
    save(&record)?;
    Ok(record)
}

fn restore(path: &Path, dry_run: bool) -> Result<Record> {
    validate_entrypoint(path)?;
    let _lock = if dry_run { None } else { Some(lock()?) };
    let mut record = load(path)?;
    let r = record
        .replacement
        .as_ref()
        .ok_or_else(|| conflict("No recorded replacement"))?;
    if r.phase == Phase::Restored {
        return Ok(record);
    }
    let current = identity(path)?;
    let managed = EntryIdentity::Symlink {
        target: r.target.clone(),
    };
    let backup = r.backup.clone();
    let holding = sibling(path, "managed");
    let before = r.before.clone();
    let held = identity(&holding)?;
    if held != EntryIdentity::Missing && held != managed {
        return Err(conflict(
            "Restore holding path changed; leaving files untouched",
        ));
    }
    if identity(&backup)? == EntryIdentity::Missing && current == r.before {
        if !dry_run {
            if held == managed {
                fs::remove_file(&holding)?;
            }
            if let Some(r) = &mut record.replacement {
                r.phase = Phase::Restored;
            }
            save(&record)?;
        }
        return Ok(record);
    }
    if current != managed && current != EntryIdentity::Missing {
        return Err(conflict(
            "Entrypoint changed since replacement; leaving it and the backup untouched",
        ));
    }
    if identity(&backup)? != r.before {
        return Err(conflict(
            "Backup changed; refusing to restore it automatically",
        ));
    }
    if dry_run {
        return Ok(record);
    }
    if let Some(r) = &mut record.replacement {
        r.phase = Phase::Restoring;
    }
    save(&record)?;
    if current == managed {
        move_without_overwrite(path, &holding)?;
        if identity(&holding)? != managed {
            move_without_overwrite(&holding, path)?;
            return Err(conflict(
                "Entrypoint changed during restore; leaving files preserved",
            ));
        }
    }
    if before != EntryIdentity::Missing {
        move_without_overwrite(&backup, path)?;
    }
    if identity(&holding)? == managed {
        fs::remove_file(&holding)?;
    }
    if let Some(r) = &mut record.replacement {
        r.phase = Phase::Restored;
    }
    save(&record)?;
    Ok(record)
}

/// Hold the registry lock through uninstall so a replacement cannot be added mid-removal.
pub fn guard_uninstall(package: &str) -> Result<RegistryLock> {
    let lock = lock()?;
    for entry in fs::read_dir(registry()?)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "json") {
            let record: Record = serde_json::from_slice(&fs::read(path)?)?;
            if record
                .replacement
                .as_ref()
                .is_some_and(|r| r.package == package && r.phase != Phase::Restored)
            {
                return Err(conflict(format!(
                    "Restore provenance entrypoint {} before uninstalling {package}",
                    record.entrypoint.display()
                )));
            }
        }
    }
    Ok(lock)
}

pub async fn run(action: Action) -> Result<()> {
    if !cfg!(any(target_os = "macos", target_os = "linux"))
        && !matches!(&action, Action::Inspect { .. })
    {
        return Err(WaxError::PlatformNotSupported(
            "Provenance recording/replacement currently requires macOS or Linux".into(),
        ));
    }
    match action {
        Action::Inspect { name, json } => {
            let hits = if Path::new(&name).is_absolute() {
                vec![provenance::observe(Path::new(&name)).await?]
            } else {
                provenance::inspect(&name).await?
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
            } else {
                for hit in hits {
                    println!(
                        "{} {:?} {} -> {} ({:?}; {}; version={})",
                        if hit.active { "active" } else { "candidate" },
                        hit.path_index,
                        hit.entrypoint.display(),
                        hit.executable
                            .as_deref()
                            .unwrap_or(Path::new("<unresolved>"))
                            .display(),
                        hit.origin.manager,
                        hit.origin.evidence,
                        hit.origin.version.as_deref().unwrap_or("unknown")
                    );
                }
            }
        }
        Action::Record { entrypoint } => {
            if !entrypoint.is_absolute() {
                return Err(conflict("Record requires an absolute entrypoint"));
            }
            let _lock = lock()?;
            let mut record = load(&entrypoint)?;
            record
                .observations
                .push(provenance::observe(&entrypoint).await?);
            record.path_candidates = provenance::inspect(
                &entrypoint
                    .file_name()
                    .ok_or_else(|| conflict("Missing executable name"))?
                    .to_string_lossy(),
            )
            .await?;
            save(&record)?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Action::Replace {
            entrypoint,
            package,
            dry_run,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&replace(&entrypoint, &package, dry_run).await?)?
        ),
        Action::Restore {
            entrypoint,
            dry_run,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&restore(&entrypoint, dry_run)?)?
        ),
    }
    Ok(())
}
