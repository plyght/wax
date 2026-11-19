//! Conservative adapters for dependency-free npm and Bun global packages.
use super::{Manager, Package};
use crate::commands::provenance::validate_entrypoint;
use crate::error::{Result, WaxError};
use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

fn invalid(message: impl Into<String>) -> WaxError {
    WaxError::InvalidInput(message.into())
}

// Receipts authorize destructive operations, so duplicate keys must not be last-wins.
fn parse_json(bytes: &[u8]) -> Result<Value> {
    struct Unique(Value);
    impl<'de> Deserialize<'de> for Unique {
        fn deserialize<D: serde::Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Unique;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("JSON with unique object keys")
                }
                fn visit_map<M: serde::de::MapAccess<'de>>(
                    self,
                    mut map: M,
                ) -> std::result::Result<Unique, M::Error> {
                    let mut object = serde_json::Map::new();
                    while let Some((key, Unique(value))) = map.next_entry::<String, Unique>()? {
                        if object.insert(key, value).is_some() {
                            return Err(serde::de::Error::custom("Duplicate receipt key"));
                        }
                    }
                    Ok(Unique(Value::Object(object)))
                }
                fn visit_seq<S: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: S,
                ) -> std::result::Result<Unique, S::Error> {
                    let mut array = Vec::new();
                    while let Some(Unique(value)) = seq.next_element()? {
                        array.push(value);
                    }
                    Ok(Unique(Value::Array(array)))
                }
                fn visit_str<E: serde::de::Error>(
                    self,
                    value: &str,
                ) -> std::result::Result<Unique, E> {
                    Ok(Unique(Value::String(value.into())))
                }
                fn visit_bool<E: serde::de::Error>(
                    self,
                    value: bool,
                ) -> std::result::Result<Unique, E> {
                    Ok(Unique(Value::Bool(value)))
                }
                fn visit_i64<E: serde::de::Error>(
                    self,
                    value: i64,
                ) -> std::result::Result<Unique, E> {
                    Ok(Unique(Value::Number(value.into())))
                }
                fn visit_u64<E: serde::de::Error>(
                    self,
                    value: u64,
                ) -> std::result::Result<Unique, E> {
                    Ok(Unique(Value::Number(value.into())))
                }
                fn visit_f64<E: serde::de::Error>(
                    self,
                    value: f64,
                ) -> std::result::Result<Unique, E> {
                    serde_json::Number::from_f64(value)
                        .map(|n| Unique(Value::Number(n)))
                        .ok_or_else(|| E::custom("Invalid JSON number"))
                }
                fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                    Ok(Unique(Value::Null))
                }
            }
            deserializer.deserialize_any(Visitor)
        }
    }
    Ok(serde_json::from_slice::<Unique>(bytes)?.0)
}

fn runtime(name: &str) -> bool {
    matches!(
        name,
        "node"
            | "nodejs"
            | "npm"
            | "npx"
            | "bun"
            | "bunx"
            | "cargo"
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
            | "corepack"
            | "yarn"
            | "yarnpkg"
            | "pnpm"
            | "pnpx"
            | "deno"
    )
}

fn name(name: &str) -> Result<()> {
    let part = |s: &str| {
        !s.is_empty()
            && s.as_bytes()[0].is_ascii_alphanumeric()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    let valid = if let Some(scoped) = name.strip_prefix('@') {
        scoped
            .split_once('/')
            .is_some_and(|(scope, package)| part(scope) && part(package))
    } else {
        part(name)
    };
    if !valid || runtime(name.rsplit('/').next().unwrap_or(name)) {
        return Err(invalid(
            "Unsupported Node package or runtime/toolchain name",
        ));
    }
    Ok(())
}

fn version(version: &str) -> Result<()> {
    let pattern = regex::Regex::new(
        r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-([0-9A-Za-z-]+\.)*[0-9A-Za-z-]+)?(\+([0-9A-Za-z-]+\.)*[0-9A-Za-z-]+)?$",
    ).map_err(|e| invalid(e.to_string()))?;
    if !pattern.is_match(version) {
        return Err(invalid("An exact Node package SemVer is required"));
    }
    if let Some((_, pre)) = version
        .split('+')
        .next()
        .unwrap_or_default()
        .split_once('-')
    {
        if pre
            .split('.')
            .any(|p| p.len() > 1 && p.starts_with('0') && p.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(invalid("Invalid numeric SemVer prerelease"));
        }
    }
    Ok(())
}

fn safe_root(root: &Path) -> Result<()> {
    if !cfg!(unix) {
        return Err(WaxError::PlatformNotSupported(
            "Node tool migration requires Unix".into(),
        ));
    }
    if !root.is_absolute() || !root.is_dir() || fs::canonicalize(root)? != root {
        return Err(invalid("Use an absolute canonical Node manager root"));
    }
    let home = fs::canonicalize(crate::ui::dirs::home_dir()?)?;
    if !root.starts_with(&home) || root == home {
        return Err(invalid("Node manager root must be below HOME"));
    }
    for parent in root.ancestors().take_while(|p| p.starts_with(&home)) {
        validate_entrypoint(&parent.join(".wax-node-root-check"))?;
    }
    #[cfg(unix)]
    if nix::unistd::geteuid().is_root() || nix::unistd::getuid() != nix::unistd::geteuid() {
        return Err(invalid("Node tool migration cannot run elevated"));
    }
    Ok(())
}

fn safe_file(path: &Path) -> Result<()> {
    validate_entrypoint(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err(invalid(format!(
            "Expected regular file: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o6022 != 0 || meta.uid() != nix::unistd::getuid().as_raw() {
            return Err(invalid("Unsafe Node package file ownership or permissions"));
        }
    }
    Ok(())
}

fn relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(invalid(
            "Node package paths must be relative without traversal",
        ));
    }
    Ok(())
}

fn modules(manager: Manager, root: &Path) -> Result<PathBuf> {
    match manager {
        Manager::Npm => Ok(root.join("lib/node_modules")),
        Manager::Bun => Ok(root.join("install/global/node_modules")),
        _ => Err(invalid("Not a Node package manager")),
    }
}

fn bun_overrides(root: &Path) -> Result<()> {
    for (key, expected) in [
        ("BUN_INSTALL_GLOBAL_DIR", root.join("install/global")),
        ("BUN_INSTALL_BIN", root.join("bin")),
    ] {
        if std::env::var_os(key).is_some_and(|value| !value.is_empty() && value != expected) {
            return Err(invalid(format!(
                "{key} overrides the supported default Bun layout"
            )));
        }
    }
    Ok(())
}

fn manifest(value: &Value) -> Result<(String, String, BTreeMap<String, PathBuf>)> {
    let package = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Missing Node package name"))?;
    let release = value
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Missing Node package version"))?;
    name(package)?;
    version(release)?;
    for field in [
        "dependencies",
        "optionalDependencies",
        "peerDependencies",
        "bundledDependencies",
        "bundleDependencies",
        "workspaces",
    ] {
        if value.get(field).is_some() {
            return Err(invalid(format!(
                "Node package field {field} is not supported"
            )));
        }
    }
    if let Some(scripts) = value.get("scripts") {
        let scripts = scripts
            .as_object()
            .ok_or_else(|| invalid("Malformed package scripts"))?;
        for hook in [
            "preinstall",
            "install",
            "postinstall",
            "preprepare",
            "prepare",
            "postprepare",
            "preuninstall",
            "uninstall",
            "postuninstall",
            "prepublish",
        ] {
            if scripts.contains_key(hook) {
                return Err(invalid(format!(
                    "Node lifecycle hook {hook} requires an unsupported build"
                )));
            }
        }
    }
    if value
        .get("gypfile")
        .is_some_and(|v| v != &Value::Bool(false))
    {
        return Err(invalid("Native Node packages are not supported"));
    }
    let bin = value
        .get("bin")
        .ok_or_else(|| invalid("Node package has no bin mapping"))?;
    let mut bins = BTreeMap::new();
    if let Some(path) = bin.as_str() {
        bins.insert(
            package.rsplit('/').next().unwrap_or(package).to_owned(),
            PathBuf::from(path),
        );
    } else {
        for (bin, path) in bin
            .as_object()
            .ok_or_else(|| invalid("Malformed Node bin mapping"))?
        {
            bins.insert(
                bin.clone(),
                PathBuf::from(
                    path.as_str()
                        .ok_or_else(|| invalid("Non-string Node bin target"))?,
                ),
            );
        }
    }
    if bins.is_empty() {
        return Err(invalid("Node package has no executables"));
    }
    for (bin, target) in &bins {
        name(bin)?;
        if bin.contains('/') || runtime(bin) {
            return Err(invalid("Refusing runtime or invalid Node bin name"));
        }
        relative(target)?;
    }
    Ok((package.to_owned(), release.to_owned(), bins))
}

fn read_manifest(path: &Path) -> Result<Value> {
    safe_file(path)?;
    parse_json(&fs::read(path)?)
}

fn tree(root: &Path, current: &Path) -> Result<()> {
    validate_entrypoint(&current.join(".wax-node-tree-check"))?;
    for item in fs::read_dir(current)? {
        let path = item?.path();
        let meta = fs::symlink_metadata(&path)?;
        if path.extension().is_some_and(|e| e == "node")
            || path.file_name().is_some_and(|n| n == "binding.gyp")
            || path.file_name().is_some_and(|n| n == "node_modules")
        {
            return Err(invalid("Native Node addons are not supported"));
        }
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&path)?;
            if target.is_absolute() || !fs::canonicalize(&path)?.starts_with(root) {
                return Err(invalid("Node package contains an escaping symlink"));
            }
            let mut depth = current
                .strip_prefix(root)
                .map_err(|_| invalid("Node tree escaped package"))?
                .components()
                .count();
            for component in target.components() {
                match component {
                    Component::Normal(_) => depth += 1,
                    Component::CurDir => (),
                    Component::ParentDir if depth > 0 => depth -= 1,
                    _ => {
                        return Err(invalid(
                            "Node symlink traverses outside package before resolving",
                        ))
                    }
                }
            }
            validate_entrypoint(&path)?;
        } else if meta.is_dir() {
            tree(root, &path)?;
        } else {
            safe_file(&path)?;
        }
    }
    Ok(())
}

fn backend(manager: Manager) -> Result<PathBuf> {
    let executable = match manager {
        Manager::Npm => "npm",
        Manager::Bun => "bun",
        _ => return Err(invalid("Not a Node manager")),
    };
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|p| p.is_absolute())
    {
        let candidate = directory.join(executable);
        if candidate.is_file() && validate_backend(&candidate).is_ok() {
            return Ok(candidate);
        }
    }
    Err(invalid(format!(
        "No safe absolute {executable} backend on PATH"
    )))
}

fn validate_backend(path: &Path) -> Result<()> {
    if !path.is_absolute() || !path.is_file() {
        return Err(invalid("Expected absolute Node backend executable"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let resolved = fs::canonicalize(path)?;
        for file in [path, resolved.as_path()] {
            for ancestor in file.ancestors() {
                let meta = fs::metadata(ancestor)?;
                if meta.mode() & 0o6022 != 0
                    || (meta.uid() != 0 && meta.uid() != nix::unistd::getuid().as_raw())
                {
                    return Err(invalid("Unsafe Node backend ownership or permissions"));
                }
            }
        }
        if fs::metadata(path)?.mode() & 0o111 == 0 {
            return Err(invalid("Node backend is not executable"));
        }
    }
    Ok(())
}

fn command(backend: &Path, manager: Manager, root: &Path, scratch: &Path) -> Result<Command> {
    validate_backend(backend)?;
    let expected = match manager {
        Manager::Npm => "npm",
        Manager::Bun => "bun",
        _ => return Err(invalid("Not a Node manager")),
    };
    if backend.file_name().is_none_or(|n| n != expected) {
        return Err(invalid(
            "Captured Node backend does not match selected manager",
        ));
    }
    let home = scratch.join("home");
    fs::create_dir_all(&home)?;
    let npmrc = scratch.join("npmrc");
    let global_npmrc = scratch.join("global-npmrc");
    fs::write(&npmrc, b"")?;
    fs::write(&global_npmrc, b"")?;
    let mut cmd = Command::new(backend);
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("DO_NOT_TRACK", "1")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("NPM_CONFIG_USERCONFIG", &npmrc)
        .env("NPM_CONFIG_GLOBALCONFIG", &global_npmrc)
        .env("npm_config_cache", scratch.join("npm-cache"))
        .env("npm_config_ignore_scripts", "true")
        .env("npm_config_audit", "false")
        .env("npm_config_fund", "false")
        .env("npm_config_update_notifier", "false")
        .env("npm_config_offline", "true")
        .env("BUN_INSTALL", root)
        .env("BUN_INSTALL_GLOBAL_DIR", root.join("install/global"))
        .env("BUN_INSTALL_BIN", root.join("bin"))
        .env("BUN_INSTALL_CACHE_DIR", scratch.join("bun-cache"))
        .current_dir(scratch);
    if manager == Manager::Npm {
        cmd.env("npm_config_prefix", root);
    }
    Ok(cmd)
}

fn run(cmd: &mut Command) -> Result<Output> {
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(WaxError::InstallError(format!(
            "Node backend exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output)
}

// Bun text locks use JSON with trailing commas; remove only commas before ] or }.
fn text_lock(path: &Path) -> Result<Value> {
    safe_file(path)?;
    let bytes = fs::read(path)?;
    let mut clean = Vec::with_capacity(bytes.len());
    let (mut quoted, mut escaped) = (false, false);
    for (index, &byte) in bytes.iter().enumerate() {
        if !quoted
            && byte == b','
            && bytes[index + 1..]
                .iter()
                .find(|b| !b.is_ascii_whitespace())
                .is_some_and(|b| matches!(b, b']' | b'}'))
        {
            continue;
        }
        clean.push(byte);
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
        }
    }
    parse_json(&clean)
}

fn tar_manifest(path: &Path) -> Result<Value> {
    if !path.is_absolute() || path.extension().is_none_or(|e| e != "tgz") {
        return Err(invalid("Node artifact must be an absolute local .tgz"));
    }
    safe_file(path)?;
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(path)?));
    let mut manifest = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        relative(&path)?;
        if !path.starts_with("package")
            || !(entry.header().entry_type().is_file() || entry.header().entry_type().is_dir())
        {
            return Err(invalid(
                "Node artifact contains unsupported archive paths or links",
            ));
        }
        if path.extension().is_some_and(|e| e == "node")
            || path.file_name().is_some_and(|n| n == "binding.gyp")
        {
            return Err(invalid("Node artifact contains native addons"));
        }
        if path == Path::new("package/package.json") {
            if manifest.is_some() || entry.size() > 1024 * 1024 {
                return Err(invalid("Duplicate or oversized artifact manifest"));
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            manifest = Some(parse_json(&bytes)?);
        }
    }
    let value = manifest.ok_or_else(|| invalid("Node tarball has no package/package.json"))?;
    self::manifest(&value)?;
    Ok(value)
}

fn corroborate(
    manager: Manager,
    root: &Path,
    package: &str,
    release: &str,
    backend: &Path,
) -> Result<()> {
    if manager == Manager::Npm {
        let scratch = tempfile::tempdir()?;
        let output = run(command(backend, manager, root, scratch.path())?
            .args([
                "ls",
                "--global",
                "--depth=0",
                "--json",
                "--ignore-scripts",
                "--offline",
            ])
            .arg("--prefix")
            .arg(root))?;
        let receipt = parse_json(&output.stdout)?;
        if receipt
            .get("dependencies")
            .and_then(|v| v.get(package))
            .and_then(|v| v.get("version"))
            .and_then(Value::as_str)
            != Some(release)
        {
            return Err(invalid(
                "npm ls does not corroborate installed package/version",
            ));
        }
        return Ok(());
    }
    let global = root.join("install/global");
    let receipt = read_manifest(&global.join("package.json"))?;
    let spec = receipt
        .get("dependencies")
        .and_then(|v| v.get(package))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Bun global manifest does not claim package"))?;
    let lock = text_lock(&global.join("bun.lock"))?;
    if lock
        .get("workspaces")
        .and_then(|v| v.get(""))
        .and_then(|v| v.get("dependencies"))
        .and_then(|v| v.get(package))
        .and_then(Value::as_str)
        != Some(spec)
    {
        return Err(invalid("Bun lock and global manifest disagree"));
    }
    let row = lock
        .get("packages")
        .and_then(|v| v.get(package))
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("Bun lock has no selected package row"))?;
    let identity = row
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Malformed Bun lock package row"))?;
    let local = spec.strip_prefix("file:").unwrap_or(spec);
    if Path::new(local).is_absolute() {
        if identity != format!("{package}@{spec}") && identity != format!("{package}@{local}") {
            return Err(invalid(
                "Bun local artifact identity disagrees with global manifest",
            ));
        }
        let value = tar_manifest(Path::new(local))?;
        let actual = manifest(&value)?;
        let installed = manifest(&read_manifest(
            &modules(manager, root)?.join(package).join("package.json"),
        )?)?;
        if actual != installed || actual.0 != package || actual.1 != release {
            return Err(invalid(
                "Bun artifact manifest disagrees with installed name/version/bin mapping",
            ));
        }
        let integrity = row
            .last()
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Bun local artifact integrity missing"))?;
        // Compare the lock's SHA512 without introducing a base64 dependency.
        if integrity != sha512_integrity(Path::new(local))? {
            return Err(invalid("Bun local artifact integrity mismatch"));
        }
    } else {
        version(
            spec.strip_prefix('^')
                .or_else(|| spec.strip_prefix('~'))
                .unwrap_or(spec),
        )?;
        if identity != format!("{package}@{release}") {
            return Err(invalid(
                "Bun lock does not corroborate exact installed version",
            ));
        }
    }
    Ok(())
}

fn sha512_integrity(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha512};
    let mut file = File::open(path)?;
    let mut digest = Sha512::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::from("sha512-");
    for chunk in digest.finalize().chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        encoded.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        encoded.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    Ok(encoded)
}

pub(super) fn discover(manager: Manager, root: &Path, entry: &Path) -> Result<Package> {
    discover_with_backend(manager, root, entry, backend(manager)?, true)
}

fn discover_with_backend(
    manager: Manager,
    root: &Path,
    entry: &Path,
    backend: PathBuf,
    inherited: bool,
) -> Result<Package> {
    safe_root(root)?;
    validate_backend(&backend)?;
    if manager == Manager::Bun && inherited {
        bun_overrides(root)?;
    }
    if entry.parent() != Some(root.join("bin").as_path()) {
        return Err(invalid("Node entrypoint must be directly inside ROOT/bin"));
    }
    validate_entrypoint(entry)?;
    if !fs::symlink_metadata(entry)?.file_type().is_symlink() {
        return Err(invalid("Node global entrypoint must be a symlink"));
    }
    let modules = modules(manager, root)?;
    let resolved = fs::canonicalize(entry)?;
    let package_dir = resolved
        .ancestors()
        .find(|p| {
            p.parent() == Some(modules.as_path())
                || p.parent().and_then(Path::parent) == Some(modules.as_path())
                    && p.parent()
                        .and_then(Path::file_name)
                        .is_some_and(|n| n.to_string_lossy().starts_with('@'))
        })
        .ok_or_else(|| invalid("Node executable is outside the global package tree"))?;
    if fs::canonicalize(package_dir)? != package_dir {
        return Err(invalid(
            "Symlinked global package directories are unsupported",
        ));
    }
    let value = read_manifest(&package_dir.join("package.json"))?;
    let (name, version, mappings) = manifest(&value)?;
    if package_dir != modules.join(&name) {
        return Err(invalid(
            "Node manifest name does not match package directory",
        ));
    }
    tree(package_dir, package_dir)?;
    let mut bins = BTreeMap::new();
    let mut payloads = vec![package_dir.to_path_buf()];
    for (bin, relative) in mappings {
        let target = package_dir.join(relative);
        let canonical = fs::canonicalize(&target)?;
        if !canonical.starts_with(package_dir) {
            return Err(invalid("Node executable escapes package"));
        }
        safe_file(&canonical)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(&canonical)?.permissions().mode() & 0o111 == 0 {
                return Err(invalid("Node executable has no executable mode"));
            }
        }
        let outer = root.join("bin").join(&bin);
        validate_entrypoint(&outer)?;
        if !fs::symlink_metadata(&outer)?.file_type().is_symlink()
            || fs::canonicalize(&outer)? != canonical
        {
            return Err(invalid("Incomplete or mismatched global Node bin links"));
        }
        if manager == Manager::Bun {
            let inner = modules.join(".bin").join(&bin);
            if fs::symlink_metadata(&inner).is_ok() {
                validate_entrypoint(&inner)?;
                if !fs::symlink_metadata(&inner)?.file_type().is_symlink()
                    || fs::canonicalize(&inner)? != canonical
                {
                    return Err(invalid("Conflicting Bun internal bin link"));
                }
                payloads.push(inner);
            }
        }
        bins.insert(bin, outer);
    }
    if !bins.values().any(|p| p == entry) {
        return Err(invalid("Selected Node entrypoint is not in the manifest"));
    }
    // Extra outer links into the selected tree are not silently left behind.
    for item in fs::read_dir(root.join("bin"))? {
        let path = item?.path();
        if fs::canonicalize(&path).is_ok_and(|p| p.starts_with(package_dir))
            && !bins.values().any(|p| *p == path)
        {
            return Err(invalid(
                "Unreceipted global bin link into selected Node package",
            ));
        }
    }
    corroborate(manager, root, &name, &version, &backend)?;
    let metadata = if manager == Manager::Bun {
        ["package.json", "bun.lock", "bun.lockb"]
            .iter()
            .map(|n| root.join("install/global").join(n))
            .collect()
    } else {
        let mut paths = vec![modules.join(".package-lock.json")];
        for file in ["package.json", "package-lock.json"] {
            let path = root.join(file);
            if fs::symlink_metadata(&path).is_ok() {
                paths.push(path);
            }
        }
        paths
    };
    for path in &metadata {
        if fs::symlink_metadata(path).is_ok() {
            safe_file(path)?;
        }
    }
    Ok(Package {
        manager,
        name,
        version,
        root: root.to_path_buf(),
        bins,
        payloads,
        metadata,
        backend,
    })
}

fn revalidate(package: &Package) -> Result<Package> {
    let entry = package
        .bins
        .values()
        .next()
        .ok_or_else(|| invalid("Node package has no binaries"))?;
    let current = discover_with_backend(
        package.manager,
        &package.root,
        entry,
        package.backend.clone(),
        false,
    )?;
    if current.name != package.name
        || current.version != package.version
        || current.bins != package.bins
        || current.payloads != package.payloads
        || current.metadata != package.metadata
    {
        return Err(invalid("Node package changed since discovery"));
    }
    Ok(current)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let from = entry?.path();
        let to = destination.join(
            from.file_name()
                .ok_or_else(|| invalid("Missing Node tree filename"))?,
        );
        let meta = fs::symlink_metadata(&from)?;
        if meta.file_type().is_symlink() {
            link(&fs::read_link(&from)?, &to)?;
        } else if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            let mut input = File::open(&from)?;
            let mut output = OpenOptions::new().write(true).create_new(true).open(&to)?;
            std::io::copy(&mut input, &mut output)?;
            output.set_permissions(meta.permissions())?;
            output.sync_all()?;
        }
    }
    fs::set_permissions(destination, fs::metadata(source)?.permissions())?;
    Ok(())
}

fn link(target: &Path, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, path)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (target, path);
        Err(invalid("Node tool links require Unix"))
    }
}

fn empty_destination(destination: &Path) -> Result<()> {
    safe_root(destination)?;
    if fs::read_dir(destination)?.next().is_some() {
        return Err(invalid(
            "Node destination must be an empty private staging root",
        ));
    }
    Ok(())
}

pub(super) fn import(package: &Package, destination: &Path) -> Result<()> {
    revalidate(package)?;
    empty_destination(destination)?;
    if destination.starts_with(&package.root) {
        return Err(invalid(
            "Node import destination cannot be inside source root",
        ));
    }
    let source = modules(package.manager, &package.root)?.join(&package.name);
    let (_, _, bins) = manifest(&read_manifest(&source.join("package.json"))?)?;
    copy_tree(&source, &destination.join("package"))?;
    fs::create_dir(destination.join("bin"))?;
    for (name, target) in bins {
        link(
            &Path::new("../package").join(target),
            &destination.join("bin").join(name),
        )?;
    }
    Ok(())
}

/// The lifecycle engine calls this only for an isolated source projection.
pub(super) fn uninstall(package: &Package) -> Result<()> {
    revalidate(package)?;
    let scratch = tempfile::tempdir()?;
    let mut cmd = command(
        &package.backend,
        package.manager,
        &package.root,
        scratch.path(),
    )?;
    match package.manager {
        Manager::Npm => {
            cmd.args([
                "uninstall",
                "--global",
                "--ignore-scripts",
                "--offline",
                "--no-audit",
                "--no-fund",
            ])
            .arg("--prefix")
            .arg(&package.root);
        }
        Manager::Bun => {
            cmd.args(["remove", "--global", "--ignore-scripts"]);
        }
        _ => return Err(invalid("Not a Node manager")),
    }
    run(cmd.arg(&package.name))?;
    Ok(())
}

pub(super) fn install(
    manager: Manager,
    name: &str,
    version: &str,
    artifact: Option<&Path>,
    destination: &Path,
) -> Result<Package> {
    self::name(name)?;
    self::version(version)?;
    empty_destination(destination)?;
    let spec = if let Some(path) = artifact {
        let value = tar_manifest(path)?;
        let (actual_name, actual_version, _) = manifest(&value)?;
        if actual_name != name || actual_version != version {
            return Err(invalid("Node artifact name/version does not match request"));
        }
        path.as_os_str().to_owned()
    } else {
        format!("{name}@{version}").into()
    };
    let backend = backend(manager)?;
    let scratch = tempfile::tempdir()?;
    let mut cmd = command(&backend, manager, destination, scratch.path())?;
    match manager {
        Manager::Npm => {
            cmd.args([
                "install",
                "--global",
                "--ignore-scripts",
                "--offline",
                "--no-audit",
                "--no-fund",
            ])
            .arg("--prefix")
            .arg(destination);
        }
        Manager::Bun => {
            cmd.args(["add", "--global", "--ignore-scripts", "--exact"]);
        }
        _ => return Err(invalid("Not a Node manager")),
    }
    run(cmd.arg(spec))?;
    let directory = modules(manager, destination)?.join(name);
    let (actual_name, actual_version, bins) =
        manifest(&read_manifest(&directory.join("package.json"))?)?;
    if actual_name != name || actual_version != version {
        return Err(invalid("Node backend installed an unexpected name/version"));
    }
    let bin = bins
        .keys()
        .next()
        .ok_or_else(|| invalid("Node backend installed no bins"))?;
    discover_with_backend(
        manager,
        destination,
        &destination.join("bin").join(bin),
        backend,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        serde_json::json!({"name": "fixture-tool", "version": "1.2.3", "bin": {"fixture-tool": "cli.js"}})
    }

    #[test]
    fn accepts_complete_object_and_scoped_string_bin_mappings() {
        let (_, _, bins) = manifest(&fixture()).unwrap();
        assert_eq!(
            bins,
            BTreeMap::from([("fixture-tool".into(), PathBuf::from("cli.js"))])
        );
        let (_, _, scoped) = manifest(
            &serde_json::json!({"name": "@scope/tool", "version": "1.2.3", "bin": "bin/run.js"}),
        )
        .unwrap();
        assert_eq!(
            scoped,
            BTreeMap::from([("tool".into(), PathBuf::from("bin/run.js"))])
        );
    }

    #[test]
    fn rejects_dependencies_including_empty_declarations_and_lifecycle_hooks() {
        for field in [
            "dependencies",
            "optionalDependencies",
            "peerDependencies",
            "bundledDependencies",
            "bundleDependencies",
            "workspaces",
        ] {
            let mut value = fixture();
            value[field] = serde_json::json!({});
            assert!(manifest(&value).is_err(), "{field}");
        }
        for hook in [
            "preinstall",
            "install",
            "postinstall",
            "preprepare",
            "prepare",
            "postprepare",
            "preuninstall",
            "uninstall",
            "postuninstall",
            "prepublish",
        ] {
            let mut value = fixture();
            value["scripts"] = serde_json::json!({hook: "echo needs-build"});
            assert!(manifest(&value).is_err(), "{hook}");
        }
    }

    #[test]
    fn rejects_runtime_names_traversal_and_version_requirements() {
        for package in [
            "node",
            "npm",
            "npx",
            "bun",
            "bunx",
            "cargo",
            "rustc",
            "rustup",
            "@scope/node",
            "../tool",
            "--force",
        ] {
            assert!(name(package).is_err(), "{package}");
        }
        for release in ["^1.2.3", "=1.2.3", "1.2", "latest", "1.2.3-01", "--force"] {
            assert!(version(release).is_err(), "{release}");
        }
        for target in ["../outside.js", "/absolute.js", "bin/../../outside.js", ""] {
            let mut value = fixture();
            value["bin"]["fixture-tool"] = Value::String(target.into());
            assert!(manifest(&value).is_err(), "{target}");
        }
        let mut value = fixture();
        value["bin"] = serde_json::json!({"node": "cli.js"});
        assert!(manifest(&value).is_err());
    }

    #[test]
    fn rejects_duplicate_receipt_keys_at_every_depth() {
        for bytes in [
            br#"{"name":"a","name":"b"}"#.as_slice(),
            br#"{"bin":{"tool":"a","tool":"b"}}"#.as_slice(),
        ] {
            assert!(parse_json(bytes).is_err());
        }
        assert!(
            parse_json(br#"{"name":"tool","nested":[null,true,12,1.5,"escaped\"string"]}"#).is_ok()
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_native_addons_and_escape_links_but_accepts_internal_relative_links() {
        let home = fs::canonicalize(crate::ui::dirs::home_dir().unwrap()).unwrap();
        let temp = tempfile::tempdir_in(home).unwrap();
        let root = temp.path().join("package");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("cli.js"), b"fixture").unwrap();
        link(Path::new("cli.js"), &root.join("alias.js")).unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        link(Path::new("../cli.js"), &root.join("bin/alias.js")).unwrap();
        tree(&root, &root).unwrap();
        link(Path::new("../package/cli.js"), &root.join("reentry.js")).unwrap();
        assert!(tree(&root, &root).is_err());
        fs::remove_file(root.join("reentry.js")).unwrap();
        fs::write(root.join("addon.node"), b"native").unwrap();
        assert!(tree(&root, &root).is_err());
        fs::remove_file(root.join("addon.node")).unwrap();
        fs::write(temp.path().join("outside"), b"outside").unwrap();
        link(Path::new("../outside"), &root.join("escape")).unwrap();
        assert!(tree(&root, &root).is_err());
    }
}
