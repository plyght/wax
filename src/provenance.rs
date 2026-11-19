//! Read-only executable observations. Receipt associations do not verify file bytes.
use crate::error::{Result, WaxError};
use crate::install::{InstallMode, InstallState};
use crate::ui::dirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Manager {
    Wax,
    Cargo,
    Bun,
    Npm,
    Pnpm,
    Node,
    Standalone,
    Conflicting,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Origin {
    pub manager: Manager,
    pub package: Option<String>,
    pub version: Option<String>,
    pub receipt: Option<PathBuf>,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkHop {
    pub path: PathBuf,
    pub target: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Observation {
    pub entrypoint: PathBuf,
    pub symlinks: Vec<LinkHop>,
    pub executable: Option<PathBuf>,
    pub executable_sha256: Option<String>,
    pub origin: Origin,
    pub status: String,
    pub path_index: Option<usize>,
    pub active: bool,
}

fn unknown(evidence: &str) -> Origin {
    Origin {
        manager: Manager::Unknown,
        package: None,
        version: None,
        receipt: None,
        evidence: evidence.into(),
    }
}

fn absolute(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn parts(path: &Path) -> VecDeque<OsString> {
    path.components()
        .map(|c| c.as_os_str().to_owned())
        .collect()
}

fn resolve(path: &Path, hops: &mut Vec<LinkHop>) -> std::result::Result<PathBuf, String> {
    let mut pending = parts(path);
    let mut current = PathBuf::new();
    let mut seen = HashSet::new();
    while let Some(part) = pending.pop_front() {
        match Path::new(&part).components().next() {
            Some(Component::ParentDir) => {
                current.pop();
                continue;
            }
            Some(Component::CurDir) => continue,
            _ => current.push(&part),
        }
        let meta = fs::symlink_metadata(&current).map_err(|e| -> String {
            match e.kind() {
                std::io::ErrorKind::NotFound if hops.is_empty() => "missing".into(),
                std::io::ErrorKind::NotFound => "broken".into(),
                std::io::ErrorKind::PermissionDenied => "unreadable".into(),
                _ => "resolution_error".into(),
            }
        })?;
        if meta.file_type().is_symlink() {
            if !seen.insert((current.clone(), pending.clone())) {
                return Err("cycle".into());
            }
            if hops.len() == 40 {
                return Err("hop_limit".into());
            }
            let target = fs::read_link(&current).map_err(|_| "unreadable".to_string())?;
            hops.push(LinkHop {
                path: current.clone(),
                target: target.clone(),
            });
            if target.is_absolute() {
                current.clear();
            } else {
                current.pop();
            }
            let mut next = parts(&target);
            next.append(&mut pending);
            pending = next;
        }
    }
    fs::canonicalize(current).map_err(|_| "resolution_error".into())
}

fn executable(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        path.extension().is_some_and(|s| {
            matches!(
                s.to_string_lossy().to_ascii_lowercase().as_str(),
                "exe" | "com" | "cmd" | "bat"
            )
        })
    }
}

fn json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn cargo_origin(entry: &Path, resolved: Option<&Path>, root: &Path) -> Option<Origin> {
    let bin = root.join("bin");
    let canonical_bin = fs::canonicalize(&bin).ok();
    let in_bin = |p: &Path| {
        p.parent() == Some(bin.as_path())
            || canonical_bin.as_ref().is_some_and(|bin| {
                p.parent()
                    .is_some_and(|parent| fs::canonicalize(parent).is_ok_and(|p| p == *bin))
            })
    };
    let candidate = [Some(entry), resolved]
        .into_iter()
        .flatten()
        .find(|p| in_bin(p))?;
    let name = candidate.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let mut origin = unknown("Cargo bin layout; receipts missing, unreadable, malformed or unmatched; file bytes not verified");
    origin.manager = Manager::Cargo;
    let receipt = root.join(".crates2.json");
    let mut matches = Vec::new();
    if let Some(value) = json(&receipt) {
        if let Some(installs) = value.get("installs").and_then(Value::as_object) {
            for (key, row) in installs {
                if row
                    .get("bins")
                    .and_then(Value::as_array)
                    .is_some_and(|bins| bins.iter().any(|b| b.as_str() == Some(name)))
                {
                    matches.push((key.clone(), receipt.clone()));
                }
            }
        }
    }
    if matches.is_empty() {
        let receipt = root.join(".crates.toml");
        if let Some(table) = fs::read_to_string(&receipt)
            .ok()
            .and_then(|s| toml::from_str::<toml::Value>(&s).ok())
        {
            if let Some(installs) = table.get("v1").and_then(toml::Value::as_table) {
                for (key, bins) in installs {
                    if bins
                        .as_array()
                        .is_some_and(|bins| bins.iter().any(|b| b.as_str() == Some(name)))
                    {
                        matches.push((key.clone(), receipt.clone()));
                    }
                }
            }
        }
    }
    if matches.len() == 1 {
        let (key, receipt) = &matches[0];
        let mut fields = key.split_whitespace();
        if let (Some(package), Some(version)) = (fields.next(), fields.next()) {
            if package
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                && version.starts_with(|c: char| c.is_ascii_digit())
                && version
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
            {
                origin.package = Some(package.into());
                origin.version = Some(version.into());
                origin.receipt = Some(receipt.clone());
                origin.evidence = "Cargo receipt associates bin name with package/version; source omitted; file bytes not verified".into();
            }
        }
    } else if matches.len() > 1 {
        origin.evidence =
            "Cargo bin layout; conflicting receipt associations; file bytes not verified".into();
    }
    Some(origin)
}

fn node_origin(entry: &Path, resolved: &Path, bun: Option<&Path>) -> Option<Origin> {
    for root in resolved.ancestors().skip(1) {
        if !root.components().any(|c| c.as_os_str() == "node_modules") {
            continue;
        }
        let receipt = root.join("package.json");
        let Some(value) = json(&receipt) else {
            continue;
        };
        let name = value.get("name").and_then(Value::as_str);
        let Some(bin) = value.get("bin") else {
            continue;
        };
        let mappings: Vec<(&str, &str)> = if let Some(bin) = bin.as_str() {
            name.map(|n| vec![(n.rsplit('/').next().unwrap_or(n), bin)])
                .unwrap_or_default()
        } else {
            bin.as_object()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| Some((k.as_str(), v.as_str()?)))
                        .collect()
                })
                .unwrap_or_default()
        };
        if !mappings.iter().any(|(n, b)| {
            entry.file_name().is_some_and(|s| s == *n)
                && fs::canonicalize(root.join(b)).is_ok_and(|p| p == resolved)
        }) {
            continue;
        }
        let global = bun.map(|b| b.join("install/global"));
        // BUN_INSTALL is the installation root, whose global packages live in install/global.
        let bun_layout = global.as_ref().is_some_and(|g| root.starts_with(g));
        let pnpm = root.components().any(|c| c.as_os_str() == ".pnpm");
        let lock = global.as_ref().map(|g| g.join("bun.lock"));
        let npm_receipt = root
            .ancestors()
            .filter(|p| p.file_name().is_some_and(|n| n == "node_modules"))
            .find_map(|modules| {
                let lock = modules.join(".package-lock.json");
                let lock_value = json(&lock)?;
                let key = format!(
                    "node_modules/{}",
                    root.strip_prefix(modules).ok()?.to_str()?
                );
                let row = lock_value.get("packages")?.get(&key)?;
                (row.get("version")?.as_str()? == value.get("version")?.as_str()?).then_some(lock)
            });
        let manager = if bun_layout {
            Manager::Bun
        } else if pnpm {
            Manager::Pnpm
        } else if npm_receipt.is_some() {
            Manager::Npm
        } else {
            Manager::Node
        };
        return Some(Origin {
            manager,
            package: name.map(str::to_owned),
            version: value.get("version").and_then(Value::as_str).map(str::to_owned),
            receipt: Some(npm_receipt.clone().unwrap_or(receipt)),
            evidence: format!("package.json bin mapping matches resolved file; manager {} evidence; Bun text lock {} (not parsed); file bytes not verified",
                if bun_layout || pnpm { "layout" } else if npm_receipt.is_some() { "npm hidden lock package/version" } else { "ambiguous, npm not asserted" },
                if lock.as_ref().is_some_and(|p| p.is_file()) { "present" } else { "absent" }),
        });
    }
    let layout = [Some(entry), Some(resolved)]
        .into_iter()
        .flatten()
        .find_map(|path| {
            if bun.is_some_and(|b| {
                path.starts_with(b.join("bin")) || path.starts_with(b.join("install/global"))
            }) {
                Some(Manager::Bun)
            } else if path.components().any(|c| c.as_os_str() == ".pnpm") {
                Some(Manager::Pnpm)
            } else if path.components().any(|c| c.as_os_str() == "node_modules") {
                Some(Manager::Node)
            } else {
                None
            }
        });
    layout.map(|manager| Origin { manager, ..unknown("Reserved manager layout only; package receipt missing, unreadable, malformed or bin mapping unmatched; file bytes not verified") })
}

async fn origin(entry: &Path, resolved: Option<&Path>, hops: &[LinkHop]) -> Origin {
    let home = dirs::home_dir().ok();
    let cargo = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".cargo")))
        .and_then(|p| absolute(&p).ok());
    let bun = std::env::var_os("BUN_INSTALL")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".bun")))
        .and_then(|p| absolute(&p).ok());
    let external = cargo
        .as_deref()
        .and_then(|r| cargo_origin(entry, resolved, r))
        .or_else(|| node_origin(entry, resolved.unwrap_or(entry), bun.as_deref()))
        .or_else(|| {
            // A broken shim into a manager's reserved directory is still not unmanaged.
            hops.iter().rev().find_map(|hop| {
                let target = if hop.target.is_absolute() {
                    hop.target.clone()
                } else {
                    hop.path.parent()?.join(&hop.target)
                };
                cargo
                    .as_deref()
                    .and_then(|r| cargo_origin(&target, None, r))
                    .or_else(|| node_origin(&target, &target, bun.as_deref()))
            })
        });
    let mut wax = Vec::new();
    let mut state_note = "Wax state missing or unmatched";
    if let Ok(state) = InstallState::new() {
        match state.load().await {
            Ok(packages) => {
                for package in packages.values() {
                    // InstallMode::prefix calls brew; observation must never execute it.
                    let roots = match package.install_mode {
                        InstallMode::User => home
                            .iter()
                            .map(|h| h.join(".local/wax/Cellar"))
                            .collect::<Vec<_>>(),
                        InstallMode::Global => {
                            let mut roots = vec![
                                PathBuf::from("/opt/homebrew/Cellar"),
                                PathBuf::from("/usr/local/Cellar"),
                                PathBuf::from("/home/linuxbrew/.linuxbrew/Cellar"),
                            ];
                            roots.extend(home.iter().map(|h| h.join(".linuxbrew/Cellar")));
                            roots.extend(
                                std::env::var_os("HOMEBREW_PREFIX")
                                    .map(|p| PathBuf::from(p).join("Cellar")),
                            );
                            roots
                        }
                    };
                    let mut roots_seen = HashSet::new();
                    for root in roots.into_iter().filter_map(|p| fs::canonicalize(p).ok()) {
                        if !roots_seen.insert(root.clone()) {
                            continue;
                        }
                        let Some(relative) = resolved.and_then(|r| r.strip_prefix(&root).ok())
                        else {
                            continue;
                        };
                        let mut components = relative.components();
                        if components
                            .next()
                            .is_some_and(|c| c.as_os_str() == package.name.as_str())
                            && components
                                .next()
                                .is_some_and(|c| c.as_os_str() == package.version.as_str())
                        {
                            wax.push(Origin {
                            manager: Manager::Wax,
                            package: Some(package.name.clone()),
                            version: Some(package.version.clone()),
                            receipt: dirs::wax_dir().ok().map(|p| p.join("installed.json")),
                            evidence: "installed.json and actual Cellar package/version agree; file bytes not verified".into(),
                        });
                        }
                    }
                }
            }
            Err(_) => state_note = "Wax state unreadable or malformed",
        }
    }
    if wax.len() > 1 || (!wax.is_empty() && external.is_some()) {
        return Origin {
            manager: Manager::Conflicting,
            ..unknown("Conflicting manager evidence; Wax ownership not asserted")
        };
    }
    if let Some(wax) = wax.pop() {
        return wax;
    }
    if let Some(external) = external {
        return external;
    }
    if let Some(path) = resolved {
        let components: Vec<_> = path.components().collect();
        if let Some(index) = components
            .iter()
            .position(|c| c.as_os_str() == "standalone")
        {
            if let Some(version) = components.get(index + 1) {
                return Origin {
                    manager: Manager::Standalone,
                    package: index.checked_sub(1).map(|i| components[i].as_os_str().to_string_lossy().into_owned()),
                    version: Some(version.as_os_str().to_string_lossy().into_owned()),
                    receipt: None,
                    evidence: "Standalone directory layout only; version is a layout hint, not trusted executable output; no receipt verified".into(),
                };
            }
        }
    }
    unknown(&format!("{state_note}; no matching manager receipt; Node/Bun receipts may be missing, unreadable, malformed or unmatched"))
}

pub async fn observe(path: &Path) -> Result<Observation> {
    let entrypoint = absolute(path)?;
    let mut symlinks = Vec::new();
    let (resolved, mut status) = match resolve(&entrypoint, &mut symlinks) {
        Ok(path) => {
            let status = if executable(&path) {
                "ok"
            } else {
                "non_executable"
            };
            (Some(path), status.to_string())
        }
        Err(status) => (None, status),
    };
    let executable_sha256 = if status == "ok" {
        match resolved
            .as_deref()
            .map(crate::digest::sha256_file)
            .transpose()
        {
            Ok(hash) => hash,
            Err(_) => {
                status = "hash_unreadable".into();
                None
            }
        }
    } else {
        None
    };
    let origin = origin(&entrypoint, resolved.as_deref(), &symlinks).await;
    Ok(Observation {
        entrypoint,
        symlinks,
        executable: resolved,
        executable_sha256,
        origin,
        status,
        path_index: None,
        active: false,
    })
}

pub async fn inspect(name: &str) -> Result<Vec<Observation>> {
    if name.is_empty()
        || Path::new(name).components().count() != 1
        || !matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(WaxError::InstallError(
            "Expected an executable name, not a path".into(),
        ));
    }
    let mut observations = Vec::new();
    let mut selected = false;
    for (index, directory) in
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).enumerate()
    {
        let path = absolute(&directory.join(name))?;
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            _ => {}
        }
        let mut observation = observe(&path).await?;
        observation.path_index = Some(index);
        observation.active =
            !selected && matches!(observation.status.as_str(), "ok" | "hash_unreadable");
        selected |= observation.active;
        observations.push(observation);
    }
    Ok(observations)
}
