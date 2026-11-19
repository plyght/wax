//! Genuine native installs followed by Wax ownership transfer, update, remove and restore.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOOL: &str = "wax-migration-fixture";
const NEIGHBOR: &str = "wax-migration-neighbor";

struct Home {
    _temp: tempfile::TempDir,
    root: PathBuf,
    rustup: PathBuf,
}

impl Home {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let rustup = std::env::var_os("RUSTUP_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".rustup"));
        fs::create_dir_all(root.join("manager")).unwrap();
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::write(root.join("npmrc"), "").unwrap();
        fs::write(root.join("global-npmrc"), "").unwrap();
        Self {
            _temp: temp,
            root,
            rustup,
        }
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(&self.root)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("DO_NOT_TRACK", "1")
            .env("HOME", &self.root)
            .env("RUSTUP_HOME", &self.rustup)
            .env("CARGO_HOME", self.root.join("cargo-cache"))
            .env("CARGO_TARGET_DIR", self.root.join("cargo-target"))
            .env("WAX_CACHE_DIR", self.root.join("wax-cache"))
            .env("NPM_CONFIG_USERCONFIG", self.root.join("npmrc"))
            .env("NPM_CONFIG_GLOBALCONFIG", self.root.join("global-npmrc"))
            .env("NPM_CONFIG_CACHE", self.root.join("cache/npm"))
            .env("NPM_CONFIG_UPDATE_NOTIFIER", "false")
            .env("NPM_CONFIG_AUDIT", "false")
            .env("NPM_CONFIG_FUND", "false")
            .env("BUN_INSTALL", self.root.join("manager"))
            .env(
                "BUN_INSTALL_GLOBAL_DIR",
                self.root.join("manager/install/global"),
            )
            .env("BUN_INSTALL_BIN", self.root.join("manager/bin"))
            .env("BUN_INSTALL_CACHE_DIR", self.root.join("cache/bun"))
            .env_remove("NPM_CONFIG_PREFIX")
            .env_remove("npm_config_prefix")
            .env_remove("WAX_TEST_CELLAR");
        cmd
    }

    fn wax(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_wax"))
            .args(args)
            .output()
            .unwrap()
    }

    fn bin(&self, name: &str) -> PathBuf {
        self.root.join("manager/bin").join(name)
    }
    fn owned_bin(&self) -> PathBuf {
        self.root.join(".local/wax/bin").join(TOOL)
    }

    fn migrate(&self, manager: &str, dry_run: bool) -> Output {
        let mut cmd = self.command(env!("CARGO_BIN_EXE_wax"));
        cmd.args(["tools", "migrate"])
            .arg(self.bin(TOOL))
            .args(["--from", manager, "--root"])
            .arg(self.root.join("manager"));
        if dry_run {
            cmd.arg("--dry-run");
        }
        cmd.output().unwrap()
    }

    fn record(&self) -> PathBuf {
        fs::read_dir(self.root.join(".wax/tools"))
            .unwrap()
            .map(|p| p.unwrap().path().join("record.json"))
            .find(|p| p.is_file())
            .unwrap()
    }
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_bin(h: &Home, path: &Path, expected: &str) {
    let out = h.command(path).output().unwrap();
    success(&out);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), expected);
}

fn crate_fixture(h: &Home, name: &str, version: &str) -> PathBuf {
    let root = h.root.join(format!("source-{name}-{version}"));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2021\"\n"),
    )
    .unwrap();
    fs::write(
        root.join("src/main.rs"),
        format!("fn main() {{ println!(\"{name}@{version}\"); }}\n"),
    )
    .unwrap();
    success(
        &h.command("cargo")
            .arg("generate-lockfile")
            .arg("--offline")
            .arg("--manifest-path")
            .arg(root.join("Cargo.toml"))
            .output()
            .unwrap(),
    );
    root
}

fn cargo_install(h: &Home, source: &Path) {
    success(
        &h.command("cargo")
            .args(["install", "--offline", "--locked", "--path"])
            .arg(source)
            .arg("--root")
            .arg(h.root.join("manager"))
            .output()
            .unwrap(),
    );
}

fn node_fixture(h: &Home, name: &str, version: &str, extra: &str) -> PathBuf {
    use flate2::{write::GzEncoder, Compression};
    let path = h.root.join(format!("{name}-{version}.tgz"));
    let gzip = GzEncoder::new(fs::File::create(&path).unwrap(), Compression::default());
    let mut tar = tar::Builder::new(gzip);
    let package = format!(
        r#"{{"name":"{name}","version":"{version}","bin":{{"{name}":"cli.js","{name}-alias":"cli.js"}}{extra}}}"#
    );
    let script = "#!/usr/bin/env node\nconsole.log(require('fs').readFileSync(require('path').join(__dirname, 'version.txt'), 'utf8'));\n";
    let resource = format!("{name}@{version}");
    for (name, contents, mode) in [
        ("package/package.json", package.as_bytes(), 0o644),
        ("package/cli.js", script.as_bytes(), 0o755),
        ("package/version.txt", resource.as_bytes(), 0o644),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        tar.append_data(&mut header, name, contents).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
    path
}

fn node_install(h: &Home, manager: &str, artifact: &Path) {
    let mut cmd = h.command(manager);
    if manager == "npm" {
        cmd.args([
            "install",
            "--global",
            "--offline",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--prefix",
        ])
        .arg(h.root.join("manager"));
    } else {
        cmd.args(["add", "--global", "--ignore-scripts"]);
    }
    success(&cmd.arg(artifact).output().unwrap());
}

fn exercise_lifecycle(h: &Home, manager: &str, update: &Path) {
    let id = format!("{manager}/{TOOL}");
    let original_link = if manager != "cargo" {
        let target = fs::canonicalize(h.bin(TOOL)).unwrap();
        fs::remove_file(h.bin(TOOL)).unwrap();
        std::os::unix::fs::symlink(&target, h.bin(TOOL)).unwrap();
        Some(target)
    } else {
        None
    };
    let original = fs::read(h.bin(TOOL)).unwrap();
    let neighbor = fs::read(h.bin(NEIGHBOR)).unwrap();
    let neighbor_inode = fs::metadata(h.bin(NEIGHBOR)).unwrap().ino();
    let receipts = if manager == "cargo" {
        [".crates.toml", ".crates2.json"]
            .iter()
            .map(|p| {
                let path = h.root.join("manager").join(p);
                (path.clone(), fs::read(path).unwrap())
            })
            .collect::<Vec<_>>()
    } else if manager == "bun" {
        ["package.json", "bun.lock"]
            .iter()
            .map(|p| {
                let path = h.root.join("manager/install/global").join(p);
                (path.clone(), fs::read(path).unwrap())
            })
            .collect()
    } else {
        Vec::new()
    };
    run_bin(h, &h.bin(TOOL), &format!("{TOOL}@1.2.3"));
    success(&h.migrate(manager, true));
    assert_eq!(fs::read(h.bin(TOOL)).unwrap(), original);
    for (path, bytes) in &receipts {
        assert_eq!(&fs::read(path).unwrap(), bytes);
    }
    assert!(!h.root.join(".wax/tools").exists());
    success(&h.migrate(manager, false));
    assert!(fs::symlink_metadata(h.bin(TOOL)).is_err());
    assert_eq!(fs::read(h.owned_bin()).unwrap(), original);
    run_bin(h, &h.owned_bin(), &format!("{TOOL}@1.2.3"));
    if manager != "cargo" {
        assert!(fs::symlink_metadata(h.bin(&format!("{TOOL}-alias"))).is_err());
        run_bin(
            h,
            &h.owned_bin().with_file_name(format!("{TOOL}-alias")),
            &format!("{TOOL}@1.2.3"),
        );
    }
    assert_eq!(fs::read(h.bin(NEIGHBOR)).unwrap(), neighbor);
    assert_eq!(fs::metadata(h.bin(NEIGHBOR)).unwrap().ino(), neighbor_inode);
    let listing = h.wax(&["tools", "list"]);
    success(&listing);
    assert!(String::from_utf8_lossy(&listing.stdout).contains(&format!("{id} 1.2.3 active")));
    let receipt = fs::read(h.record()).unwrap();
    success(&h.wax(&[
        "tools",
        "upgrade",
        &id,
        "--version",
        "2.3.4",
        "--artifact",
        update.to_str().unwrap(),
        "--dry-run",
    ]));
    assert_eq!(fs::read(h.record()).unwrap(), receipt);
    success(&h.wax(&[
        "tools",
        "upgrade",
        &id,
        "--version",
        "2.3.4",
        "--artifact",
        update.to_str().unwrap(),
    ]));
    run_bin(h, &h.owned_bin(), &format!("{TOOL}@2.3.4"));
    if manager != "cargo" {
        run_bin(
            h,
            &h.owned_bin().with_file_name(format!("{TOOL}-alias")),
            &format!("{TOOL}@2.3.4"),
        );
    }
    assert!(fs::symlink_metadata(h.bin(TOOL)).is_err());
    assert_eq!(fs::metadata(h.bin(NEIGHBOR)).unwrap().ino(), neighbor_inode);
    success(&h.wax(&["uninstall", &id, "--dry-run"]));
    run_bin(h, &h.owned_bin(), &format!("{TOOL}@2.3.4"));
    success(&h.wax(&["uninstall", &id]));
    assert!(fs::symlink_metadata(h.owned_bin()).is_err());
    if manager != "cargo" {
        assert!(
            fs::symlink_metadata(h.owned_bin().with_file_name(format!("{TOOL}-alias"))).is_err()
        );
    }
    success(&h.wax(&["tools", "restore", &id, "--dry-run"]));
    assert!(fs::symlink_metadata(h.bin(TOOL)).is_err());
    success(&h.wax(&["tools", "restore", &id]));
    success(&h.wax(&["tools", "restore", &id]));
    assert_eq!(fs::read(h.bin(TOOL)).unwrap(), original);
    if let Some(target) = original_link {
        assert_eq!(fs::read_link(h.bin(TOOL)).unwrap(), target);
    }
    for (path, bytes) in receipts {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert_eq!(fs::read(h.bin(NEIGHBOR)).unwrap(), neighbor);
    assert_eq!(fs::metadata(h.bin(NEIGHBOR)).unwrap().ino(), neighbor_inode);
    run_bin(h, &h.bin(TOOL), &format!("{TOOL}@1.2.3"));
    if manager != "cargo" {
        run_bin(
            h,
            &h.bin(&format!("{TOOL}-alias")),
            &format!("{TOOL}@1.2.3"),
        );
    }
    run_bin(h, &h.bin(NEIGHBOR), &format!("{NEIGHBOR}@4.5.6"));
    eprintln!("Verified genuine {manager} install → Wax migrate → upgrade → remove → original-manager restore");
}

#[test]
fn cargo_installed_package_transfers_lifecycle_and_restores_original_receipts() {
    let h = Home::new();
    cargo_install(&h, &crate_fixture(&h, TOOL, "1.2.3"));
    cargo_install(&h, &crate_fixture(&h, NEIGHBOR, "4.5.6"));
    let next = crate_fixture(&h, TOOL, "2.3.4");
    exercise_lifecycle(&h, "cargo", &next);
}

#[test]
#[ignore = "requires installed npm and Node; run explicitly for migration verification"]
fn npm_global_package_transfers_lifecycle_without_touching_unrelated_install() {
    let h = Home::new();
    node_install(&h, "npm", &node_fixture(&h, TOOL, "1.2.3", ""));
    node_install(&h, "npm", &node_fixture(&h, NEIGHBOR, "4.5.6", ""));
    let next = node_fixture(&h, TOOL, "2.3.4", "");
    exercise_lifecycle(&h, "npm", &next);
}

#[test]
#[ignore = "requires installed Bun and Node; run explicitly for migration verification"]
fn bun_global_package_transfers_lifecycle_without_native_collateral_reinstall() {
    let h = Home::new();
    node_install(&h, "bun", &node_fixture(&h, TOOL, "1.2.3", ""));
    node_install(&h, "bun", &node_fixture(&h, NEIGHBOR, "4.5.6", ""));
    let next = node_fixture(&h, TOOL, "2.3.4", "");
    exercise_lifecycle(&h, "bun", &next);
}

#[test]
fn cargo_receipt_conflicts_and_source_drift_preserve_existing_ownership() {
    let h = Home::new();
    cargo_install(&h, &crate_fixture(&h, TOOL, "1.2.3"));
    let receipt = h.root.join("manager/.crates2.json");
    let before = fs::read(&receipt).unwrap();
    fs::write(
        &receipt,
        String::from_utf8(before.clone())
            .unwrap()
            .replace("1.2.3", "9.9.9"),
    )
    .unwrap();
    assert!(!h.migrate("cargo", false).status.success());
    run_bin(&h, &h.bin(TOOL), &format!("{TOOL}@1.2.3"));
    assert!(!h.root.join(".wax/tools").exists());
    fs::write(&receipt, before).unwrap();
    success(&h.migrate("cargo", false));
    // Another genuine install changes shared manager receipts; restore must not overwrite it.
    cargo_install(&h, &crate_fixture(&h, NEIGHBOR, "4.5.6"));
    let changed = fs::read(&receipt).unwrap();
    let id = format!("cargo/{TOOL}");
    let refused = h.wax(&["tools", "restore", &id]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("manager state changed"));
    assert_eq!(fs::read(&receipt).unwrap(), changed);
    run_bin(&h, &h.bin(NEIGHBOR), &format!("{NEIGHBOR}@4.5.6"));
    run_bin(&h, &h.owned_bin(), &format!("{TOOL}@1.2.3"));
}

#[test]
fn cargo_migration_preserves_conflicts_and_recovers_interrupted_receipt_restore() {
    use std::os::unix::fs::symlink;
    let h = Home::new();
    cargo_install(&h, &crate_fixture(&h, TOOL, "1.2.3"));
    fs::create_dir_all(h.owned_bin().parent().unwrap()).unwrap();
    fs::write(h.owned_bin(), "unrelated user executable").unwrap();
    assert!(!h.migrate("cargo", false).status.success());
    assert_eq!(
        fs::read(h.owned_bin()).unwrap(),
        b"unrelated user executable"
    );
    run_bin(&h, &h.bin(TOOL), &format!("{TOOL}@1.2.3"));
    fs::remove_file(h.owned_bin()).unwrap();
    success(&h.migrate("cargo", false));
    let id = format!("cargo/{TOOL}");
    let original_record = fs::read(h.record()).unwrap();
    let next = crate_fixture(&h, TOOL, "2.3.4");
    // A mismatched artifact must leave the currently working generation and receipt alone.
    assert!(!h
        .wax(&[
            "tools",
            "upgrade",
            &id,
            "--version",
            "3.0.0",
            "--artifact",
            next.to_str().unwrap()
        ])
        .status
        .success());
    assert_eq!(fs::read(h.record()).unwrap(), original_record);
    run_bin(&h, &h.owned_bin(), &format!("{TOOL}@1.2.3"));

    let link = fs::read_link(h.owned_bin()).unwrap();
    fs::remove_file(h.owned_bin()).unwrap();
    fs::write(h.owned_bin(), "replacement user data").unwrap();
    assert!(!h.wax(&["tools", "remove", &id]).status.success());
    assert!(!h.wax(&["tools", "restore", &id]).status.success());
    assert_eq!(fs::read(h.owned_bin()).unwrap(), b"replacement user data");
    fs::remove_file(h.owned_bin()).unwrap();
    symlink(&link, h.owned_bin()).unwrap();

    let root = h.record().parent().unwrap().to_path_buf();
    let current = root.join("current");
    let selection = fs::read_link(&current).unwrap();
    fs::remove_file(&current).unwrap();
    symlink(&h.root, &current).unwrap();
    assert!(!h.wax(&["tools", "remove", &id]).status.success());
    assert_eq!(fs::read_link(&current).unwrap(), h.root);
    fs::remove_file(&current).unwrap();
    symlink(selection, &current).unwrap();

    // Model a crash after parking the post-migration receipt, before restoring its original.
    let mut record: serde_json::Value = serde_json::from_slice(&original_record).unwrap();
    let index = record["originals"]
        .as_array()
        .unwrap()
        .iter()
        .position(|p| p["path"].as_str().unwrap().ends_with(".crates2.json"))
        .unwrap();
    let receipt = h.root.join("manager/.crates2.json");
    let parked = receipt.with_file_name(format!(
        ".wax-{}-{index}-post",
        root.file_name().unwrap().to_str().unwrap()
    ));
    fs::rename(&receipt, &parked).unwrap();
    record["state"] = "restoring".into();
    fs::write(h.record(), serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    success(&h.wax(&["tools", "restore", &id]));
    assert!(!parked.exists());
    assert!(fs::symlink_metadata(h.owned_bin()).is_err());
    run_bin(&h, &h.bin(TOOL), &format!("{TOOL}@1.2.3"));
    let installed: serde_json::Value = serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    assert!(installed["installs"]
        .as_object()
        .unwrap()
        .keys()
        .any(|id| id.starts_with(&format!("{TOOL} 1.2.3 ("))));
}

#[test]
#[ignore = "requires installed npm and Node; run explicitly for migration verification"]
fn npm_unsupported_lifecycle_hooks_are_rejected_without_running_or_removing_package() {
    let h = Home::new();
    let tarball = node_fixture(
        &h,
        TOOL,
        "1.2.3",
        r#", "scripts":{"postinstall":"touch SHOULD_NOT_RUN"}"#,
    );
    node_install(&h, "npm", &tarball);
    let before = fs::read(h.bin(TOOL)).unwrap();
    assert!(!h.migrate("npm", false).status.success());
    assert_eq!(fs::read(h.bin(TOOL)).unwrap(), before);
    assert!(!h.root.join("SHOULD_NOT_RUN").exists());
    assert!(!h
        .root
        .join("manager/lib/node_modules")
        .join(TOOL)
        .join("SHOULD_NOT_RUN")
        .exists());
    assert!(!h.root.join(".wax/tools").exists());
}
