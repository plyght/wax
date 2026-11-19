//! Receipt-backed adapters for moving user-installed tools without removing runtimes.

use crate::commands::provenance::validate_entrypoint;
use crate::error::{Result, WaxError};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

mod node;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Manager {
    Cargo,
    Npm,
    Bun,
}

impl Manager {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Bun => "bun",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Package {
    pub manager: Manager,
    pub name: String,
    pub version: String,
    pub root: PathBuf,
    pub bins: BTreeMap<String, PathBuf>,
    pub payloads: Vec<PathBuf>,
    pub metadata: Vec<PathBuf>,
    pub backend: PathBuf,
}

fn invalid(message: impl Into<String>) -> WaxError {
    WaxError::InvalidInput(message.into())
}

fn supported(_manager: Manager) -> Result<()> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(WaxError::PlatformNotSupported(
            "Tool migration requires macOS or Linux".into(),
        ));
    }
    Ok(())
}

fn runtime(name: &str) -> bool {
    matches!(
        name,
        "cargo"
            | "rustc"
            | "rustdoc"
            | "rustup"
            | "rustfmt"
            | "cargo-fmt"
            | "cargo-clippy"
            | "clippy-driver"
            | "rust-analyzer"
            | "rust-gdb"
            | "rust-gdbgui"
            | "rust-lldb"
            | "node"
            | "nodejs"
            | "npm"
            | "npx"
            | "bun"
            | "bunx"
            | "corepack"
            | "yarn"
            | "yarnpkg"
            | "pnpm"
            | "pnpx"
            | "deno"
    )
}

fn cargo_name(name: &str) -> Result<()> {
    crate::error::validate_package_name(name)?;
    if !name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || runtime(name)
    {
        return Err(invalid(format!("Not a migratable Cargo package: {name}")));
    }
    Ok(())
}

fn exact_version(version: &str) -> Result<()> {
    crate::error::validate_version(version)?;
    // Cargo accepts version requirements; migration must accept only a single SemVer.
    let pattern = regex::Regex::new(
        r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-([0-9A-Za-z-]+\.)*[0-9A-Za-z-]+)?(\+([0-9A-Za-z-]+\.)*[0-9A-Za-z-]+)?$",
    )
    .map_err(|e| invalid(e.to_string()))?;
    if !pattern.is_match(version) {
        return Err(invalid("Cargo migration requires an exact SemVer version"));
    }
    let without_build = version.split('+').next().unwrap_or_default();
    if let Some((_, pre)) = without_build.split_once('-') {
        for identifier in pre.split('.') {
            if identifier.bytes().all(|b| b.is_ascii_digit())
                && identifier.len() > 1
                && identifier.starts_with('0')
            {
                return Err(invalid("Invalid numeric SemVer prerelease identifier"));
            }
        }
    }
    Ok(())
}

fn safe_file(path: &Path, executable: bool) -> Result<()> {
    validate_entrypoint(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err(invalid(format!(
            "Expected a regular file: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o6022 != 0 || (executable && meta.mode() & 0o111 == 0) {
            return Err(invalid(format!(
                "Unsafe file permissions: {}",
                path.display()
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = executable;
    Ok(())
}

fn safe_root(root: &Path) -> Result<()> {
    if !root.is_absolute() || fs::canonicalize(root)? != root || !root.is_dir() {
        return Err(invalid("Use an absolute canonical manager root"));
    }
    validate_entrypoint(&root.join(".wax-root-check"))?;
    let home = fs::canonicalize(crate::ui::dirs::home_dir()?)?;
    for ancestor in root.ancestors().take_while(|p| p.starts_with(&home)) {
        validate_entrypoint(&ancestor.join(".wax-root-check"))?;
    }
    #[cfg(unix)]
    if nix::unistd::geteuid().is_root() || nix::unistd::getuid() != nix::unistd::geteuid() {
        return Err(invalid(
            "Tool migration must not run with elevated privileges",
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct CargoInstall {
    bins: Vec<String>,
}

#[derive(Deserialize)]
struct CargoJson {
    #[serde(deserialize_with = "unique_installs")]
    installs: BTreeMap<String, CargoInstall>,
}

fn unique_installs<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, CargoInstall>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = BTreeMap<String, CargoInstall>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("unique Cargo receipt identities")
        }
        fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
        where
            M: serde::de::MapAccess<'de>,
        {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, CargoInstall>()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("Duplicate Cargo receipt identity"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Visitor)
}

#[derive(Deserialize)]
struct CargoToml {
    v1: BTreeMap<String, Vec<String>>,
}

type Receipts = BTreeMap<String, BTreeSet<String>>;

fn normalize(receipts: impl IntoIterator<Item = (String, Vec<String>)>) -> Result<Receipts> {
    let mut normalized = BTreeMap::new();
    let mut owners = BTreeSet::new();
    for (identity, bins) in receipts {
        cargo_identity(&identity)?;
        if bins.is_empty() {
            return Err(invalid("Cargo receipt has no binaries"));
        }
        let mut names = BTreeSet::new();
        for bin in bins {
            crate::error::validate_package_name(&bin)?;
            if bin.contains('/') || !names.insert(bin.clone()) || !owners.insert(bin) {
                return Err(invalid("Cargo binary has duplicate or ambiguous ownership"));
            }
        }
        normalized.insert(identity, names);
    }
    Ok(normalized)
}

fn cargo_identity(identity: &str) -> Result<(&str, &str, &str)> {
    let (package, source) = identity
        .split_once(" (")
        .ok_or_else(|| invalid("Malformed Cargo receipt identity"))?;
    let source = source
        .strip_suffix(')')
        .filter(|s| !s.is_empty() && !s.contains(['(', ')', '\n', '\r']))
        .ok_or_else(|| invalid("Malformed Cargo receipt source"))?;
    let (name, version) = package
        .split_once(' ')
        .ok_or_else(|| invalid("Missing Cargo receipt version"))?;
    // Unrelated toolchain receipts may be present; only selected packages are barred below.
    crate::error::validate_package_name(name)?;
    if name.contains('/') || name.starts_with('-') {
        return Err(invalid("Malformed Cargo receipt package"));
    }
    exact_version(version)?;
    Ok((name, version, source))
}

fn cargo_receipts(root: &Path) -> Result<Receipts> {
    let json_path = root.join(".crates2.json");
    let toml_path = root.join(".crates.toml");
    let json = match fs::symlink_metadata(&json_path) {
        Ok(_) => {
            safe_file(&json_path, false)?;
            let receipt: CargoJson = serde_json::from_slice(&fs::read(&json_path)?)?;
            Some(normalize(
                receipt
                    .installs
                    .into_iter()
                    .map(|(key, value)| (key, value.bins)),
            )?)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let toml = match fs::symlink_metadata(&toml_path) {
        Ok(_) => {
            safe_file(&toml_path, false)?;
            let receipt: CargoToml = toml::from_str(&fs::read_to_string(&toml_path)?)?;
            Some(normalize(receipt.v1)?)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    match (json, toml) {
        (Some(a), Some(b)) if a != b => Err(invalid("Cargo receipts disagree")),
        (Some(a), _) | (_, Some(a)) => Ok(a),
        _ => Err(invalid("No Cargo installation receipt found")),
    }
}

fn cargo_backend() -> Result<PathBuf> {
    let paths = std::env::var_os("PATH").unwrap_or_default();
    let mut rejected = Vec::new();
    for directory in std::env::split_paths(&paths).filter(|p| p.is_absolute()) {
        let path = directory.join("cargo");
        if path.is_file() {
            match validate_backend(&path) {
                // Keep the cargo filename: a rustup proxy dispatches by argv[0].
                Ok(()) => return Ok(path),
                Err(error) => rejected.push(format!("{}: {error}", path.display())),
            }
        }
    }
    Err(invalid(format!(
        "No safe absolute Cargo backend found on PATH: {}",
        rejected.join("; ")
    )))
}

fn validate_backend(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.file_name().is_none_or(|n| n != "cargo") {
        return Err(invalid("Expected a captured absolute Cargo backend"));
    }
    let meta = fs::metadata(path)?;
    if !meta.is_file() {
        return Err(invalid("Cargo backend is not a regular executable"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let resolved = fs::canonicalize(path)?;
        for ancestor in path.ancestors().skip(1).chain(resolved.ancestors().skip(1)) {
            let parent = fs::metadata(ancestor)?;
            // Sticky directories protect trusted-owned children, including in /tmp.
            if (parent.mode() & 0o022 != 0 && parent.mode() & 0o1000 == 0)
                || (parent.uid() != 0 && parent.uid() != nix::unistd::getuid().as_raw())
            {
                return Err(invalid(format!(
                    "Unsafe Cargo backend parent {} (mode {:o}, uid {})",
                    ancestor.display(),
                    parent.mode() & 0o7777,
                    parent.uid()
                )));
            }
        }
        if meta.mode() & 0o6022 != 0
            || meta.mode() & 0o111 == 0
            || (meta.uid() != 0 && meta.uid() != nix::unistd::getuid().as_raw())
        {
            return Err(invalid("Unsafe Cargo backend permissions"));
        }
    }
    Ok(())
}

pub fn discover(manager: Manager, root: &Path, entry: &Path) -> Result<Package> {
    supported(manager)?;
    match manager {
        Manager::Cargo => discover_cargo(root, entry, cargo_backend()?),
        Manager::Npm | Manager::Bun => node::discover(manager, root, entry),
    }
}

fn discover_cargo(root: &Path, entry: &Path, backend: PathBuf) -> Result<Package> {
    safe_root(root)?;
    safe_file(entry, true)?;
    if entry.parent() != Some(root.join("bin").as_path()) {
        return Err(invalid("Cargo entrypoint must be directly inside ROOT/bin"));
    }
    let selected = entry
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| invalid("Non-UTF-8 Cargo executable name"))?;
    let receipts = cargo_receipts(root)?;
    let (identity, names) = receipts
        .iter()
        .find(|(_, names)| names.contains(selected))
        .ok_or_else(|| invalid("Entrypoint is not claimed by a Cargo receipt"))?;
    let (name, version, source) = cargo_identity(identity)?;
    cargo_name(name)?;
    if receipts
        .keys()
        .filter(|key| cargo_identity(key).is_ok_and(|(n, _, _)| n == name))
        .count()
        != 1
    {
        return Err(invalid("Multiple Cargo receipts claim this package"));
    }
    if !matches!(
        source,
        "registry+https://github.com/rust-lang/crates.io-index"
            | "registry+https://index.crates.io/"
    ) && !source
        .strip_prefix("path+file://")
        .is_some_and(|p| p.starts_with('/'))
    {
        return Err(invalid(
            "Only crates.io and path Cargo installs can be migrated",
        ));
    }
    let mut bins = BTreeMap::new();
    for bin in names {
        if runtime(bin) {
            return Err(invalid(
                "Refusing to migrate a runtime or toolchain executable",
            ));
        }
        let path = root.join("bin").join(bin);
        safe_file(&path, true)?;
        bins.insert(bin.clone(), path);
    }
    Ok(Package {
        manager: Manager::Cargo,
        name: name.to_owned(),
        version: version.to_owned(),
        root: root.to_path_buf(),
        payloads: bins.values().cloned().collect(),
        bins,
        metadata: vec![root.join(".crates2.json"), root.join(".crates.toml")],
        backend,
    })
}

fn revalidate(package: &Package) -> Result<()> {
    supported(package.manager)?;
    validate_backend(&package.backend)?;
    let entry = package
        .bins
        .values()
        .next()
        .ok_or_else(|| invalid("Package has no binaries"))?;
    let current = discover_cargo(&package.root, entry, package.backend.clone())?;
    if current.name != package.name
        || current.version != package.version
        || current.bins != package.bins
        || current.metadata != package.metadata
        || current.payloads != package.payloads
    {
        return Err(invalid("Cargo installation changed since discovery"));
    }
    validate_backend(&package.backend)
}

pub fn import(package: &Package, destination: &Path) -> Result<()> {
    if package.manager != Manager::Cargo {
        return node::import(package, destination);
    }
    revalidate(package)?;
    safe_root(destination)?;
    if fs::read_dir(destination)?.next().is_some() {
        return Err(invalid("Import destination must be an empty staging root"));
    }
    let bin_dir = destination.join("bin");
    fs::create_dir(&bin_dir)?;
    for (name, source) in &package.bins {
        let mut input = fs::File::open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(bin_dir.join(name))?;
        std::io::copy(&mut input, &mut output)?;
        output.set_permissions(input.metadata()?.permissions())?;
        output.sync_all()?;
    }
    Ok(())
}

fn run(command: &mut Command) -> Result<()> {
    // The existing timeout helper only supports brew/sw_vers and does not stop its child.
    let output = command.output()?;
    if !output.status.success() {
        return Err(WaxError::InstallError(format!(
            "Cargo exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

pub fn uninstall(package: &Package) -> Result<()> {
    if package.manager != Manager::Cargo {
        return node::uninstall(package);
    }
    revalidate(package)?;
    run(Command::new(&package.backend)
        .arg("uninstall")
        .arg("--root")
        .arg(&package.root)
        .arg(&package.name))
}

pub fn install(
    manager: Manager,
    name: &str,
    version: &str,
    artifact: Option<&Path>,
    destination: &Path,
) -> Result<Package> {
    supported(manager)?;
    if manager != Manager::Cargo {
        return node::install(manager, name, version, artifact, destination);
    }
    cargo_name(name)?;
    exact_version(version)?;
    safe_root(destination)?;
    if fs::read_dir(destination)?.next().is_some() {
        return Err(invalid("Install destination must be an empty private root"));
    }
    let backend = cargo_backend()?;
    let mut command = Command::new(&backend);
    command.arg("install");
    if let Some(path) = artifact {
        if !path.is_absolute() || !path.is_dir() {
            return Err(invalid(
                "Cargo artifact must be an existing absolute directory",
            ));
        }
        let manifest: toml::Value = toml::from_str(&fs::read_to_string(path.join("Cargo.toml"))?)?;
        if manifest
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
            != Some(name)
            || manifest
                .get("package")
                .and_then(|p| p.get("version"))
                .and_then(toml::Value::as_str)
                != Some(version)
        {
            return Err(invalid(
                "Cargo artifact manifest name/version does not match the requested package",
            ));
        }
        command.arg("--path").arg(path).arg("--offline");
    } else {
        command
            .arg(name)
            .arg("--version")
            .arg(format!("={version}"));
    }
    command.arg("--root").arg(destination).arg("--locked");
    run(&mut command)?;
    let receipts = cargo_receipts(destination)?;
    let (_, bins) = receipts
        .iter()
        .find(|(key, _)| cargo_identity(key).is_ok_and(|(n, v, _)| n == name && v == version))
        .ok_or_else(|| invalid("Cargo did not install the requested exact package/version"))?;
    let bin = bins
        .first()
        .ok_or_else(|| invalid("Cargo installed no binaries"))?;
    let package = discover_cargo(destination, &destination.join("bin").join(bin), backend)?;
    if package.name != name || package.version != version || receipts.len() != 1 {
        return Err(invalid("Cargo installed an unexpected package/version"));
    }
    Ok(package)
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    const ID: &str = "fixture-tool 1.2.3 (registry+https://github.com/rust-lang/crates.io-index)";

    fn fixture() -> (crate::install::TestHome, tempfile::TempDir) {
        let home = crate::install::TestHome::new();
        let dir = tempfile::tempdir_in(home.path()).unwrap();
        fs::create_dir(dir.path().join("bin")).unwrap();
        for name in ["bin/fixture-tool", "cargo"] {
            let path = dir.path().join(name);
            fs::write(&path, b"fixture executable bytes").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        write_receipts(dir.path(), ID, &["fixture-tool"]);
        (home, dir)
    }

    fn write_receipts(root: &Path, identity: &str, bins: &[&str]) {
        let installs = BTreeMap::from([(identity, serde_json::json!({"bins": bins}))]);
        fs::write(
            root.join(".crates2.json"),
            serde_json::to_vec(&serde_json::json!({"installs": installs})).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join(".crates.toml"),
            toml::to_string(&BTreeMap::from([(
                "v1",
                BTreeMap::from([(identity, bins)]),
            )]))
            .unwrap(),
        )
        .unwrap();
    }

    fn package(root: &Path) -> Result<Package> {
        discover_cargo(root, &root.join("bin/fixture-tool"), root.join("cargo"))
    }

    #[test]
    fn discovers_agreeing_receipts_and_snapshots_missing_receipt_paths() {
        let (_home, dir) = fixture();
        fs::remove_file(dir.path().join(".crates.toml")).unwrap();
        let found = package(dir.path()).unwrap();
        assert_eq!(
            found.metadata,
            vec![
                dir.path().join(".crates2.json"),
                dir.path().join(".crates.toml")
            ]
        );
        assert_eq!(found.payloads, vec![dir.path().join("bin/fixture-tool")]);
    }

    #[test]
    fn rejects_receipt_version_conflicts() {
        let (_home, dir) = fixture();
        let path = dir.path().join(".crates.toml");
        let text = fs::read_to_string(&path).unwrap().replace("1.2.3", "1.2.4");
        fs::write(path, text).unwrap();
        assert!(package(dir.path()).is_err());
    }

    #[test]
    fn rejects_symlinked_cargo_binaries() {
        let (_home, dir) = fixture();
        let entry = dir.path().join("bin/fixture-tool");
        fs::remove_file(&entry).unwrap();
        symlink(dir.path().join("cargo"), entry).unwrap();
        assert!(package(dir.path()).is_err());
    }

    #[test]
    fn rejects_runtime_binary_in_selected_package() {
        let (_home, dir) = fixture();
        write_receipts(dir.path(), ID, &["fixture-tool", "rustc"]);
        assert!(package(dir.path()).is_err());
    }

    #[test]
    fn rejects_multiple_receipt_claims() {
        let receipts = vec![
            (ID.to_owned(), vec!["shared".into()]),
            (
                "other 1.2.3 (path+file:///fixture)".into(),
                vec!["shared".into()],
            ),
        ];
        assert!(normalize(receipts).is_err());
    }

    #[test]
    fn rejects_duplicate_json_package_keys() {
        let text = format!(r#"{{"installs":{{"{ID}":{{"bins":["a"]}},"{ID}":{{"bins":["b"]}}}}}}"#);
        assert!(serde_json::from_str::<CargoJson>(&text).is_err());
    }

    #[test]
    fn rejects_git_source_but_accepts_path_install() {
        let (_home, dir) = fixture();
        write_receipts(
            dir.path(),
            "fixture-tool 1.2.3 (git+https://example.invalid/fixture)",
            &["fixture-tool"],
        );
        assert!(package(dir.path()).is_err());
        write_receipts(
            dir.path(),
            "fixture-tool 1.2.3 (path+file:///fixture)",
            &["fixture-tool"],
        );
        assert!(package(dir.path()).is_ok());
    }

    #[test]
    fn imports_only_claimed_binaries_preserving_bytes_and_mode() {
        let (_home, dir) = fixture();
        let found = package(dir.path()).unwrap();
        let destination = tempfile::tempdir_in(dir.path()).unwrap();
        import(&found, destination.path()).unwrap();
        let copied = destination.path().join("bin/fixture-tool");
        assert_eq!(
            crate::digest::sha256_file(&copied).unwrap(),
            crate::digest::sha256_file(&found.payloads[0]).unwrap()
        );
        assert_eq!(
            fs::metadata(copied).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!destination.path().join("cargo").exists());
    }

    #[test]
    fn backend_accepts_sticky_parents_but_rejects_unprotected_writable_paths() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared");
        let private = shared.join("private");
        fs::create_dir_all(&private).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).unwrap();
        let backend = private.join("cargo");
        fs::write(&backend, "fixture").unwrap();
        fs::set_permissions(&backend, fs::Permissions::from_mode(0o755)).unwrap();
        validate_backend(&backend).unwrap();

        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(validate_backend(&backend).is_err());
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).unwrap();
        fs::set_permissions(&backend, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(validate_backend(&backend).is_err());
    }

    #[test]
    fn rejects_version_requirements_and_option_injection() {
        for version in ["^1.2.3", "=1.2.3", "--force", "1.2", "1.2.3-01"] {
            assert!(exact_version(version).is_err(), "{version}");
        }
        for name in ["--force", "../tool", "cargo", "node", "name;id"] {
            assert!(cargo_name(name).is_err(), "{name}");
        }
    }
}
