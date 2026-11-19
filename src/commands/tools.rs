//! Package lifecycle ownership for explicitly migrated language-ecosystem tools.
use crate::error::{Result, WaxError};
use crate::tool_sources::Manager;
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum Action {
    /// Transfer a supported global package, preserving its original installation for restore.
    Migrate {
        entrypoint: PathBuf,
        #[arg(long = "from", value_enum)]
        manager: Manager,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    /// List migrated packages and their lifecycle state.
    List,
    /// Install an exact package version into Wax's private generation and select it.
    Upgrade {
        package: String,
        #[arg(long)]
        version: String,
        /// Local npm tarball or Cargo source directory; otherwise use the package registry.
        #[arg(long)]
        artifact: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Uninstall the Wax-managed tool, retaining its original-manager rollback snapshot.
    Remove {
        package: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Restore the original package artifacts and manager receipts if unchanged since migration.
    Restore {
        package: String,
        #[arg(long)]
        dry_run: bool,
    },
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unsupported() -> WaxError {
    WaxError::PlatformNotSupported("Tool migration currently requires macOS or Linux".into())
}

pub async fn run(action: Action) -> Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        unix::run(action)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = action;
        Err(unsupported())
    }
}

/// Recognize only explicit ecosystem-qualified migrated package identifiers.
pub fn qualified(name: &str) -> bool {
    ["cargo/", "npm/", "bun/"]
        .iter()
        .any(|p| name.starts_with(p))
}

pub async fn remove(package: &str, dry_run: bool) -> Result<()> {
    run(Action::Remove {
        package: package.into(),
        dry_run,
    })
    .await
}

pub async fn upgrade(package: &str, dry_run: bool) -> Result<()> {
    let (name, version) = package.rsplit_once('@').filter(|(_, v)| v.starts_with(|c: char| c.is_ascii_digit()))
        .ok_or_else(|| WaxError::InvalidInput("Specify an exact tool version: wax upgrade cargo/name@1.2.3 or wax tools upgrade ID --version VERSION".into()))?;
    run(Action::Upgrade {
        package: name.into(),
        version: version.into(),
        artifact: None,
        dry_run,
    })
    .await
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod unix {
    use super::*;
    use crate::commands::provenance::{
        lock, move_without_overwrite, validate_entrypoint, validate_user_path,
    };
    use crate::digest::{sha256_digest_hex, sha256_file};
    use crate::tool_sources::{self, Package};
    use crate::ui::dirs;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
    use std::fs::{self, File};
    use std::io::Write;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::Path;

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum Entry {
        File { sha256: String, mode: u32 },
        Directory { mode: u32 },
        Link { target: PathBuf },
    }
    type Tree = BTreeMap<PathBuf, Entry>;

    #[derive(Serialize, Deserialize)]
    struct Saved {
        path: PathBuf,
        before: Tree,
        after: Option<Tree>,
    }

    #[derive(Serialize, Deserialize)]
    struct Generation {
        path: PathBuf,
        files: Tree,
    }

    #[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    enum State {
        Prepared,
        Active,
        Removing,
        Removed,
        Restoring,
        Restored,
        Conflict,
    }

    #[derive(Serialize, Deserialize)]
    struct Record {
        schema_version: u32,
        id: String,
        source: Package,
        version: String,
        state: State,
        originals: Vec<Saved>,
        generations: Vec<Generation>,
    }

    fn conflict(message: impl Into<String>) -> WaxError {
        WaxError::InvalidInput(message.into())
    }
    fn registry() -> Result<PathBuf> {
        Ok(dirs::wax_dir()?.join("tools"))
    }
    fn root(id: &str) -> Result<PathBuf> {
        Ok(registry()?.join(sha256_digest_hex(id.as_bytes())))
    }
    fn bin() -> Result<PathBuf> {
        Ok(dirs::home_dir()?.join(".local/wax/bin"))
    }

    fn tree(path: &Path) -> Result<Tree> {
        let mut result = Tree::new();
        let mut pending = vec![PathBuf::new()];
        while let Some(relative) = pending.pop() {
            let path = if relative.as_os_str().is_empty() {
                path.to_path_buf()
            } else {
                path.join(&relative)
            };
            let meta = match fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(e)
                    if e.kind() == std::io::ErrorKind::NotFound
                        && relative.as_os_str().is_empty() =>
                {
                    return Ok(result)
                }
                Err(e) => return Err(e.into()),
            };
            let entry = if meta.is_symlink() {
                Entry::Link {
                    target: fs::read_link(path)?,
                }
            } else if meta.is_file() {
                Entry::File {
                    sha256: sha256_file(&path)?,
                    mode: meta.permissions().mode(),
                }
            } else if meta.is_dir() {
                for child in fs::read_dir(path)? {
                    pending.push(relative.join(child?.file_name()));
                }
                Entry::Directory {
                    mode: meta.permissions().mode(),
                }
            } else {
                return Err(conflict("Special files are not supported in tool packages"));
            };
            result.insert(relative, entry);
        }
        Ok(result)
    }

    fn copy(source: &Path, destination: &Path) -> Result<()> {
        let meta = fs::symlink_metadata(source)?;
        if meta.is_symlink() {
            symlink(fs::read_link(source)?, destination)?;
        } else if meta.is_file() {
            fs::copy(source, destination)?;
            File::open(destination)?.sync_all()?;
        } else if meta.is_dir() {
            fs::create_dir(destination)?;
            for item in fs::read_dir(source)? {
                let item = item?;
                copy(&item.path(), &destination.join(item.file_name()))?;
            }
            fs::set_permissions(destination, meta.permissions())?;
            File::open(destination)?.sync_all()?;
        } else {
            return Err(conflict("Cannot snapshot special files"));
        }
        Ok(())
    }

    fn delete(path: &Path) -> Result<()> {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(path)?,
            Ok(_) => fs::remove_file(path)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    fn save(record: &Record) -> Result<()> {
        let root = root(&record.id)?;
        let mut file = tempfile::NamedTempFile::new_in(&root)?;
        file.write_all(&serde_json::to_vec_pretty(record)?)?;
        file.as_file().sync_all()?;
        file.persist(root.join("record.json"))
            .map_err(|e| e.error)?;
        File::open(root)?.sync_all()?;
        Ok(())
    }

    fn load(id: &str) -> Result<Record> {
        let root = root(id)?;
        let record: Record = serde_json::from_slice(&fs::read(root.join("record.json"))?)?;
        validate_user_path(&record.source.root.join(".wax-root-check"))?;
        let mut paths = record.source.payloads.clone();
        paths.extend(record.source.bins.values().cloned());
        paths.extend(record.source.metadata.iter().cloned());
        paths.sort();
        paths.dedup();
        if record.schema_version != 1
            || record.id != id
            || format!("{}/{}", record.source.manager.label(), record.source.name) != id
            || record
                .generations
                .iter()
                .any(|g| g.path.parent() != Some(root.join("versions").as_path()))
            || record
                .originals
                .iter()
                .any(|s| !s.path.starts_with(&record.source.root))
            || paths.iter().ne(record.originals.iter().map(|s| &s.path))
            || paths
                .iter()
                .chain(record.generations.iter().map(|g| &g.path))
                .any(|p| {
                    p.components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                })
            || record.source.bins.keys().any(|n| {
                Path::new(n).components().count() != 1
                    || !matches!(
                        Path::new(n).components().next(),
                        Some(std::path::Component::Normal(_))
                    )
            })
        {
            return Err(conflict("Invalid tool ownership record"));
        }
        Ok(record)
    }

    fn target(id: &str, name: &str) -> Result<PathBuf> {
        Ok(root(id)?.join("current/bin").join(name))
    }

    fn check_links(record: &Record, allow_missing: bool) -> Result<()> {
        let current = root(&record.id)?.join("current");
        match fs::symlink_metadata(&current) {
            Ok(meta)
                if meta.is_symlink()
                    && record
                        .generations
                        .iter()
                        .any(|g| fs::read_link(&current).is_ok_and(|p| p == g.path)) => {}
            Err(e) if allow_missing && e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(conflict("Tool current pointer changed or is missing")),
        }
        for name in record.source.bins.keys() {
            let path = bin()?.join(name);
            validate_entrypoint(&path)?;
            let expected = Tree::from([(
                PathBuf::new(),
                Entry::Link {
                    target: target(&record.id, name)?,
                },
            )]);
            let actual = tree(&path)?;
            if actual != expected && !(allow_missing && actual.is_empty()) {
                return Err(conflict(format!(
                    "Tool entrypoint changed or conflicts: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    fn verify_generation(path: &Path, package: &Package) -> Result<Generation> {
        let canonical = fs::canonicalize(path)?;
        let files = tree(path)?;
        for (relative, item) in &files {
            if matches!(item, Entry::Link { .. })
                && !fs::canonicalize(path.join(relative))?.starts_with(&canonical)
            {
                return Err(conflict(
                    "Imported package has a symlink outside its owned generation",
                ));
            }
        }
        for (name, original) in &package.bins {
            let installed = path.join("bin").join(name);
            if sha256_file(&installed)? != sha256_file(original)? {
                return Err(conflict(
                    "Imported executable differs from the installed source artifact",
                ));
            }
        }
        Ok(Generation {
            path: path.to_path_buf(),
            files,
        })
    }

    fn select(record: &Record) -> Result<()> {
        check_links(record, record.state != State::Active)?;
        let generation = record
            .generations
            .last()
            .ok_or_else(|| conflict("Missing tool generation"))?;
        let root = root(&record.id)?;
        let current = root.join("current");
        if let Ok(meta) = fs::symlink_metadata(&current) {
            if !meta.is_symlink() {
                return Err(conflict("Tool current pointer is not a symlink"));
            }
        }
        let temporary = root.join("next");
        symlink(&generation.path, &temporary)?;
        fs::rename(temporary, &current)?;
        for name in record.source.bins.keys() {
            let path = bin()?.join(name);
            if fs::symlink_metadata(&path).is_err() {
                symlink(target(&record.id, name)?, path)?;
            }
        }
        File::open(root)?.sync_all()?;
        Ok(())
    }

    fn new_generation(id: &str) -> Result<tempfile::TempDir> {
        let versions = root(id)?.join("versions");
        fs::create_dir_all(&versions)?;
        Ok(tempfile::Builder::new()
            .prefix("generation-")
            .tempdir_in(versions)?)
    }

    fn holding(record: &Record, item: &Saved, i: usize, phase: &str) -> Result<PathBuf> {
        Ok(item.path.with_file_name(format!(
            ".wax-{}-{i}-{phase}",
            root(&record.id)?
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )))
    }

    fn switch_item(
        path: &Path,
        expected: &Tree,
        desired: &Tree,
        snapshot: &Path,
        holding: &Path,
    ) -> Result<()> {
        if tree(path)? == *desired {
            return Ok(());
        }
        let parent = path
            .parent()
            .ok_or_else(|| conflict("Artifact has no parent"))?;
        fs::create_dir_all(parent)?;
        validate_user_path(path)?;
        let current = tree(path)?;
        if current != *expected && !current.is_empty() {
            return Err(conflict(
                "Manager artifact changed during ownership transfer",
            ));
        }
        if !current.is_empty() {
            move_without_overwrite(path, holding)?;
            if tree(holding)? != *expected {
                let _ = move_without_overwrite(holding, path);
                return Err(conflict(
                    "Manager changed during transfer; preserved its files",
                ));
            }
        }
        if !desired.is_empty() {
            let stage = tempfile::Builder::new()
                .prefix(".wax-transfer-")
                .tempdir_in(parent)?;
            let staged = stage.path().join("artifact");
            copy(snapshot, &staged)?;
            if tree(&staged)? != *desired {
                return Err(conflict("Artifact snapshot verification failed"));
            }
            move_without_overwrite(&staged, path)?;
        }
        File::open(parent)?.sync_all()?;
        Ok(())
    }

    fn prepare_removal(record: &mut Record, sandbox: &Path) -> Result<()> {
        let source = &record.source;
        let rebase = |path: &Path| -> Result<PathBuf> {
            Ok(sandbox.join(
                path.strip_prefix(&source.root)
                    .map_err(|_| conflict("Manager artifact escapes its source root"))?,
            ))
        };
        // Native managers calculate receipt changes in a private projection, never the live root.
        // Bun may reinstall unrelated globals even during removal, so compare their content too.
        let modules = match source.manager {
            Manager::Cargo => None,
            Manager::Npm => Some(source.root.join("lib/node_modules")),
            Manager::Bun => Some(source.root.join("install/global/node_modules")),
        };
        if let Some(modules) = &modules {
            let destination = rebase(modules)?;
            fs::create_dir_all(
                destination
                    .parent()
                    .ok_or_else(|| conflict("Missing modules parent"))?,
            )?;
            copy(modules, &destination)?;
            let bin = source.root.join("bin");
            if bin.is_dir() {
                for entry in fs::read_dir(bin)? {
                    let entry = entry?;
                    if entry.file_type()?.is_symlink()
                        && fs::canonicalize(entry.path()).is_ok_and(|p| p.starts_with(modules))
                    {
                        let target = rebase(&entry.path())?;
                        fs::create_dir_all(
                            target
                                .parent()
                                .ok_or_else(|| conflict("Missing bin parent"))?,
                        )?;
                        copy(&entry.path(), &target)?;
                    }
                }
            }
        }
        for item in &record.originals {
            let path = rebase(&item.path)?;
            if !item.before.is_empty() && fs::symlink_metadata(&path).is_err() {
                fs::create_dir_all(
                    path.parent()
                        .ok_or_else(|| conflict("Missing sandbox parent"))?,
                )?;
                copy(&item.path, &path)?;
            }
        }
        // Copied global links must not let a native manager reach the original installation.
        for (relative, item) in tree(sandbox)? {
            if matches!(item, Entry::Link { .. }) {
                let path = sandbox.join(relative);
                let resolved = fs::canonicalize(&path)?;
                if resolved.starts_with(sandbox) {
                    continue;
                } else if resolved.starts_with(&source.root) {
                    fs::remove_file(&path)?;
                    symlink(rebase(&resolved)?, &path)?;
                    if !fs::canonicalize(&path)?.starts_with(sandbox) {
                        return Err(conflict("Rebased source link escapes its isolated root"));
                    }
                } else {
                    return Err(conflict(
                        "Source projection contains a symlink outside its isolated root",
                    ));
                }
            }
        }
        let before = tree(sandbox)?;
        let staged = Package {
            root: sandbox.to_path_buf(),
            bins: source
                .bins
                .iter()
                .map(|(n, p)| Ok((n.clone(), rebase(p)?)))
                .collect::<Result<_>>()?,
            payloads: source
                .payloads
                .iter()
                .map(|p| rebase(p))
                .collect::<Result<_>>()?,
            metadata: source
                .metadata
                .iter()
                .map(|p| rebase(p))
                .collect::<Result<_>>()?,
            ..source.clone()
        };
        tool_sources::uninstall(&staged)?;
        let after = tree(sandbox)?;
        let allowed = record
            .originals
            .iter()
            .map(|s| {
                s.path
                    .strip_prefix(&source.root)
                    .map(Path::to_path_buf)
                    .map_err(|_| conflict("Source snapshot escapes manager root"))
            })
            .collect::<Result<Vec<_>>>()?;
        for relative in before.keys() {
            if relative.as_os_str().is_empty() || allowed.iter().any(|p| relative.starts_with(p)) {
                continue;
            }
            // Managers may prune an emptied namespace directory. It is not removed from the source.
            if matches!(before.get(relative), Some(Entry::Directory { .. }))
                && !after.contains_key(relative)
                && allowed.iter().any(|p| p.starts_with(relative))
            {
                continue;
            }
            // New sandbox-only caches or regenerated unrelated shims are never copied to the source.
            if before.get(relative) != after.get(relative) {
                return Err(conflict(format!("Native removal changes unrelated package content at {} ({:?} → {:?}); migration refused before touching source", relative.display(), before.get(relative), after.get(relative))));
            }
        }
        if staged
            .bins
            .values()
            .any(|p| fs::symlink_metadata(p).is_ok())
        {
            return Err(conflict(
                "Native manager did not release the complete package bin set",
            ));
        }
        let post = root(&record.id)?.join("post");
        fs::create_dir(&post)?;
        for (i, item) in record.originals.iter_mut().enumerate() {
            let path = rebase(&item.path)?;
            let snapshot = tree(&path)?;
            if !snapshot.is_empty() {
                copy(&path, &post.join(i.to_string()))?;
                if tree(&post.join(i.to_string()))? != snapshot {
                    return Err(conflict("Post-manager snapshot verification failed"));
                }
            }
            item.after = Some(snapshot);
        }
        Ok(())
    }

    fn migrate(manager: Manager, source_root: &Path, entry: &Path, dry_run: bool) -> Result<()> {
        let package = tool_sources::discover(manager, source_root, entry)?;
        let id = format!("{}/{}", manager.label(), package.name);
        if source_root.starts_with(dirs::wax_dir()?)
            || source_root.starts_with(dirs::home_dir()?.join(".local/wax"))
        {
            return Err(conflict("Cannot migrate an already Wax-owned package root"));
        }
        let mut record = Record {
            schema_version: 1,
            id: id.clone(),
            version: package.version.clone(),
            source: package,
            state: State::Prepared,
            originals: Vec::new(),
            generations: Vec::new(),
        };
        // Preflight conflicts without making even the bin or registry directories for dry-run.
        for name in record.source.bins.keys() {
            if fs::symlink_metadata(bin()?.join(name)).is_ok() {
                return Err(conflict(format!("Wax bin already contains {name}")));
            }
        }
        if root(&id)?.exists() {
            return Err(conflict(
                "A tool record already exists; restore it before another migration",
            ));
        }
        if dry_run {
            println!("{}", serde_json::to_string_pretty(&record.source)?);
            return Ok(());
        }
        let _lock = lock()?;
        fs::create_dir_all(bin()?)?;
        // Re-read the source under the Wax lifecycle lock; foreign managers must not run concurrently.
        let refreshed = tool_sources::discover(manager, source_root, entry)?;
        if serde_json::to_vec(&refreshed)? != serde_json::to_vec(&record.source)? {
            return Err(conflict(
                "Source identity changed during migration planning",
            ));
        }
        fs::create_dir_all(registry()?)?;
        fs::create_dir(root(&id)?)?;
        let preparation = (|| -> Result<()> {
            let original_root = root(&id)?.join("original");
            fs::create_dir(&original_root)?;
            let mut paths = record.source.payloads.clone();
            paths.extend(record.source.bins.values().cloned());
            paths.extend(record.source.metadata.iter().cloned());
            paths.sort();
            paths.dedup();
            for (i, path) in paths.into_iter().enumerate() {
                let before = tree(&path)?;
                let backup = original_root.join(i.to_string());
                if !before.is_empty() {
                    copy(&path, &backup)?;
                    if tree(&backup)? != before {
                        return Err(conflict("Source snapshot verification failed"));
                    }
                }
                record.originals.push(Saved {
                    path,
                    before,
                    after: None,
                });
            }
            let generation = new_generation(&id)?;
            tool_sources::import(&record.source, generation.path())?;
            let mut imported = verify_generation(generation.path(), &record.source)?;
            imported.path = generation.keep();
            record.generations.push(imported);
            let removal = tempfile::Builder::new()
                .prefix("manager-plan-")
                .tempdir_in(root(&id)?)?;
            prepare_removal(&mut record, removal.path())?;
            save(&record)?;
            for item in &record.originals {
                if tree(&item.path)? != item.before {
                    return Err(conflict("Source changed before ownership transfer; preserved snapshots are available"));
                }
            }
            Ok(())
        })();
        if let Err(error) = preparation {
            // No source mutations occur during preparation; remove only our newly created staging tree.
            delete(&root(&id)?)?;
            return Err(error);
        }
        record.state = State::Conflict;
        save(&record)?;
        for (i, item) in record.originals.iter().enumerate() {
            let desired = item
                .after
                .as_ref()
                .ok_or_else(|| conflict("Missing removal plan"))?;
            switch_item(
                &item.path,
                &item.before,
                desired,
                &root(&id)?.join("post").join(i.to_string()),
                &holding(&record, item, i, "pre")?,
            )?;
        }
        if record
            .source
            .bins
            .values()
            .any(|p| fs::symlink_metadata(p).is_ok())
        {
            return Err(conflict(
                "Original manager did not release every package bin; use tools restore",
            ));
        }
        check_links(&record, true)?;
        select(&record)?;
        record.state = State::Active;
        save(&record)?;
        for (i, item) in record.originals.iter().enumerate() {
            let held = holding(&record, item, i, "pre")?;
            if tree(&held)? == item.before {
                delete(&held)?;
            }
        }
        println!("Migrated {}@{}; original artifacts and manager receipts retained for wax tools restore", record.id, record.version);
        Ok(())
    }

    fn upgrade(id: &str, version: &str, artifact: Option<&Path>, dry_run: bool) -> Result<()> {
        let _lock = if dry_run { None } else { Some(lock()?) };
        let mut record = load(id)?;
        if record.state != State::Active {
            return Err(conflict("Only active migrated tools can be upgraded"));
        }
        check_links(&record, false)?;
        if dry_run {
            println!("Would upgrade {id} from {} to {version}", record.version);
            return Ok(());
        }
        let generation = new_generation(id)?;
        let installed = tool_sources::install(
            record.source.manager,
            &record.source.name,
            version,
            artifact,
            generation.path(),
        )?;
        if installed.bins.keys().ne(record.source.bins.keys()) {
            return Err(conflict(
                "Upgrade changes the package bin set; explicit remigration is required",
            ));
        }
        let imported = verify_generation(generation.path(), &installed)?;
        for name in installed.bins.keys() {
            let installed_bin = generation.path().join("bin").join(name);
            if !installed_bin.is_file() {
                return Err(conflict("Upgrade bin is missing"));
            }
        }
        let path = generation.keep();
        record.generations.push(Generation {
            path,
            files: imported.files,
        });
        check_links(&record, false)?;
        record.version = installed.version;
        // Journal the new generation before selecting it; all prior generations remain recoverable.
        save(&record)?;
        select(&record)?;
        println!("Upgraded {id} to {}", record.version);
        Ok(())
    }

    fn deactivate(record: &Record) -> Result<()> {
        check_links(record, true)?;
        for name in record.source.bins.keys() {
            let path = bin()?.join(name);
            if fs::symlink_metadata(&path).is_ok() {
                fs::remove_file(path)?;
            }
        }
        let current = root(&record.id)?.join("current");
        if let Ok(meta) = fs::symlink_metadata(&current) {
            if !meta.is_symlink() {
                return Err(conflict("Tool current pointer changed"));
            }
            fs::remove_file(current)?;
        }
        Ok(())
    }

    fn clean_generations(record: &Record) -> Result<()> {
        for generation in &record.generations {
            let actual = tree(&generation.path)?;
            if !actual.is_empty() && actual != generation.files {
                return Err(conflict(
                    "Owned tool payload changed; preserving it instead of deleting",
                ));
            }
        }
        for generation in &record.generations {
            delete(&generation.path)?;
        }
        Ok(())
    }

    fn remove(id: &str, dry_run: bool) -> Result<()> {
        let _lock = if dry_run { None } else { Some(lock()?) };
        let mut record = load(id)?;
        if !matches!(
            record.state,
            State::Active | State::Removing | State::Removed
        ) {
            return Err(conflict(
                "Tool is not active; use tools restore to recover its original manager",
            ));
        }
        check_links(&record, true)?;
        for generation in &record.generations {
            let actual = tree(&generation.path)?;
            if !actual.is_empty() && actual != generation.files {
                return Err(conflict("Tool payload changed; refusing removal"));
            }
        }
        if dry_run {
            println!("Would remove {id}; its original-manager rollback snapshot will remain");
            return Ok(());
        }
        record.state = State::Removing;
        save(&record)?;
        deactivate(&record)?;
        clean_generations(&record)?;
        record.state = State::Removed;
        save(&record)?;
        println!("Removed {id}; wax tools restore {id} can restore its original installation");
        Ok(())
    }

    fn restore(id: &str, dry_run: bool) -> Result<()> {
        let _lock = if dry_run { None } else { Some(lock()?) };
        let mut record = load(id)?;
        if record.state == State::Restored {
            println!("Already restored {id}");
            return Ok(());
        }
        check_links(&record, true)?;
        let own_root = root(id)?;
        for (i, item) in record.originals.iter().enumerate() {
            if !item.before.is_empty()
                && tree(&own_root.join("original").join(i.to_string()))? != item.before
            {
                return Err(conflict("Original tool snapshot changed; refusing restore"));
            }
            let actual = tree(&item.path)?;
            let after = item.after.as_ref().ok_or_else(|| conflict("Migration was interrupted before its post-manager snapshot; preserved originals require manual recovery"))?;
            let post = tree(&holding(&record, item, i, "post")?)?;
            let pre = tree(&holding(&record, item, i, "pre")?)?;
            if actual != item.before
                && actual != *after
                && !(actual.is_empty() && (post == *after || pre == item.before))
            {
                return Err(conflict(format!(
                    "Original manager state changed at {}; refusing to overwrite it",
                    item.path.display()
                )));
            }
            if (!post.is_empty() && post != *after) || (!pre.is_empty() && pre != item.before) {
                return Err(conflict("Restore holding path changed"));
            }
        }
        if dry_run {
            println!("Would restore {id} and its original manager receipts");
            return Ok(());
        }
        record.state = State::Restoring;
        save(&record)?;
        for (i, item) in record.originals.iter().enumerate() {
            let after = item
                .after
                .as_ref()
                .ok_or_else(|| conflict("Missing removal plan"))?;
            switch_item(
                &item.path,
                after,
                &item.before,
                &own_root.join("original").join(i.to_string()),
                &holding(&record, item, i, "post")?,
            )?;
        }
        deactivate(&record)?;
        // Payload changes do not justify deleting user data; source restore already succeeded.
        clean_generations(&record)?;
        record.state = State::Restored;
        save(&record)?;
        for (i, item) in record.originals.iter().enumerate() {
            for (phase, expected) in [("pre", Some(&item.before)), ("post", item.after.as_ref())] {
                let held = holding(&record, item, i, phase)?;
                if Some(&tree(&held)?) == expected {
                    delete(&held)?;
                }
            }
        }
        println!("Restored {id} to its original manager with its original verified artifacts");
        Ok(())
    }

    pub(super) fn run(action: Action) -> Result<()> {
        match action {
            Action::Migrate {
                entrypoint,
                manager,
                root,
                dry_run,
            } => migrate(manager, &root, &entrypoint, dry_run),
            Action::Upgrade {
                package,
                version,
                artifact,
                dry_run,
            } => upgrade(&package, &version, artifact.as_deref(), dry_run),
            Action::Remove { package, dry_run } => remove(&package, dry_run),
            Action::Restore { package, dry_run } => restore(&package, dry_run),
            Action::List => {
                let root = registry()?;
                if root.exists() {
                    for item in fs::read_dir(root)? {
                        let path = item?.path().join("record.json");
                        if path.is_file() {
                            let record: serde_json::Value =
                                serde_json::from_slice(&fs::read(path)?)?;
                            println!(
                                "{} {} {}",
                                record["id"].as_str().unwrap_or("unknown"),
                                record["version"].as_str().unwrap_or("unknown"),
                                record["state"].as_str().unwrap_or("unknown")
                            );
                        }
                    }
                }
                Ok(())
            }
        }
    }
}
