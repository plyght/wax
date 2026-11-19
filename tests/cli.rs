//! Integration tests for the `wax` CLI binary.
//!
//! These tests compile and run the real binary so they exercise the full
//! command dispatch path.  Network-dependent tests are gated behind the
//! `INTEGRATION` env var so they don't run in CI without connectivity.

use std::path::Path;
use std::process::Command;

fn wax() -> Command {
    // Use the debug binary built by `cargo test --test cli`.
    let bin = env!("CARGO_BIN_EXE_wax");
    Command::new(bin)
}

fn wax_with_home(home: &Path) -> Command {
    let mut cmd = wax();
    cmd.env("HOME", home);
    #[cfg(windows)]
    cmd.env("USERPROFILE", home);
    cmd
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod provenance {
    use super::*;
    use serde_json::{json, Value};
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::PathBuf;
    use std::process::Output;

    const PACKAGE: &str = "wax-fixture-origin";

    struct Home {
        _temp: tempfile::TempDir,
        root: PathBuf,
    }

    impl Home {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            for dir in [".wax/cache", ".local/bin", ".cargo/bin", ".bun/bin"] {
                fs::create_dir_all(root.join(dir)).unwrap();
            }
            fs::write(root.join(".wax/cache/formulae.json"), "[]").unwrap();
            fs::write(root.join(".wax/cache/casks.json"), "[]").unwrap();
            Self { _temp: temp, root }
        }

        fn path(&self, path: &str) -> PathBuf {
            self.root.join(path)
        }

        fn command(&self) -> Command {
            let mut cmd = wax_with_home(&self.root);
            cmd.env("WAX_CACHE_DIR", self.path(".wax/cache"))
                .env("CARGO_HOME", self.path(".cargo"))
                .env("BUN_INSTALL", self.path(".bun"))
                .env("PATH", "/usr/bin:/bin")
                .env_remove("HOMEBREW_PREFIX")
                .env_remove("WAX_TEST_CELLAR");
            cmd
        }

        fn run(&self, args: &[&str]) -> Value {
            let out = self.command().args(args).output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            serde_json::from_slice(&out.stdout).unwrap()
        }

        fn replace(&self, path: &Path, dry_run: bool) -> Output {
            let mut cmd = self.command();
            cmd.args(["provenance", "replace"])
                .arg(path)
                .args(["--with", PACKAGE]);
            if dry_run {
                cmd.arg("--dry-run");
            }
            cmd.output().unwrap()
        }

        fn restore(&self, path: &Path, dry_run: bool) -> Output {
            let mut cmd = self.command();
            cmd.args(["provenance", "restore"]).arg(path);
            if dry_run {
                cmd.arg("--dry-run");
            }
            cmd.output().unwrap()
        }

        fn package(&self, version: &str) -> PathBuf {
            let prefix = self.path(".local/wax");
            let keg = prefix.join("Cellar").join(PACKAGE).join(version);
            executable(&keg.join("bin/tool"), version.as_bytes());
            fs::create_dir_all(prefix.join("opt")).unwrap();
            let opt = prefix.join("opt").join(PACKAGE);
            if fs::symlink_metadata(&opt).is_ok() {
                fs::remove_file(&opt).unwrap();
            }
            symlink(&keg, &opt).unwrap();
            fs::write(
                self.path(".wax/installed.json"),
                serde_json::to_vec(&json!({PACKAGE: {
                    "name": PACKAGE, "version": version, "platform": "fixture",
                    "install_date": 0, "install_mode": "user"
                }}))
                .unwrap(),
            )
            .unwrap();
            keg
        }

        fn journal(&self) -> PathBuf {
            fs::read_dir(self.path(".wax/provenance"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.extension().is_some_and(|e| e == "json"))
                .unwrap()
        }
    }

    fn executable(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn success(out: &Output) {
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn inspect_preserves_path_order_duplicates_receipts_and_raw_symlinks_without_execution() {
        let h = Home::new();
        let payload = h.path(".local/share/fixture/standalone/1.2.3/tool");
        let marker = h.path("executed");
        executable(
            &payload,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()).as_bytes(),
        );
        let entry = h.path(".local/bin/tool");
        symlink("../share/fixture/standalone/1.2.3/tool", &entry).unwrap();
        executable(&h.path(".cargo/bin/tool"), b"cargo fixture");
        fs::write(h.path(".cargo/.crates2.json"), r#"{"installs":{"fixture 4.5.6 (registry+https://example.invalid)":{"bins":["tool"]}}}"#).unwrap();
        let keg = h.package("2.0.0");
        fs::create_dir_all(h.path(".local/wax/bin")).unwrap();
        symlink(keg.join("bin/tool"), h.path(".local/wax/bin/tool")).unwrap();
        let paths = std::env::join_paths([
            h.path(".local/bin"),
            h.path(".local/wax/bin"),
            h.path(".cargo/bin"),
            h.path(".local/bin"),
        ])
        .unwrap();
        let out = h
            .command()
            .args(["provenance", "inspect", "tool", "--json"])
            .env("PATH", paths)
            .output()
            .unwrap();
        success(&out);
        let hits: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 4);
        for (i, manager) in ["standalone", "wax", "cargo", "standalone"]
            .iter()
            .enumerate()
        {
            assert_eq!(hits[i]["path_index"], i);
            assert_eq!(hits[i]["active"], i == 0);
            assert_eq!(hits[i]["origin"]["manager"], *manager);
        }
        assert_eq!(hits[0]["executable"], payload.to_str().unwrap());
        assert_eq!(
            hits[0]["symlinks"][0]["target"],
            "../share/fixture/standalone/1.2.3/tool"
        );
        assert_eq!(hits[2]["origin"]["version"], "4.5.6");
        assert_eq!(
            hits[2]["origin"]["receipt"],
            h.path(".cargo/.crates2.json").to_str().unwrap()
        );
        assert!(!String::from_utf8_lossy(&out.stdout).contains("example.invalid"));
        assert!(!marker.exists());
        assert!(!h.path(".wax/provenance").exists());
        fs::write(h.path(".cargo/.crates2.json"), "broken JSON").unwrap();
        fs::write(
            h.path(".cargo/.crates.toml"),
            "[v1]\n\"fixture 7.8.9 (registry+private)\" = [\"tool\"]\n",
        )
        .unwrap();
        let fallback = h.run(&[
            "provenance",
            "inspect",
            h.path(".cargo/bin/tool").to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(fallback[0]["origin"]["version"], "7.8.9");
        symlink(h.path(".cargo/bin/tool"), h.path(".local/bin/alias")).unwrap();
        let alias = h.run(&[
            "provenance",
            "inspect",
            h.path(".local/bin/alias").to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(alias[0]["origin"]["package"], "fixture");
    }

    #[test]
    fn inspect_distinguishes_broken_cycles_nonexecutables_and_ambiguous_node_origins() {
        let h = Home::new();
        symlink("missing", h.path(".local/bin/broken")).unwrap();
        symlink("cycle", h.path(".local/bin/cycle")).unwrap();
        fs::write(h.path(".local/bin/plain"), "not executable").unwrap();
        for (name, status) in [
            ("broken", "broken"),
            ("cycle", "cycle"),
            ("plain", "non_executable"),
        ] {
            let hits = h.run(&[
                "provenance",
                "inspect",
                h.path(&format!(".local/bin/{name}")).to_str().unwrap(),
                "--json",
            ]);
            assert_eq!(hits[0]["status"], status);
            assert_eq!(hits[0]["active"], false);
        }
        fs::create_dir_all(h.path("broken-bin")).unwrap();
        fs::create_dir_all(h.path("plain-bin")).unwrap();
        symlink("missing", h.path("broken-bin/tool")).unwrap();
        fs::write(h.path("plain-bin/tool"), "not executable").unwrap();
        executable(&h.path(".local/bin/tool"), b"fixture");
        let paths = std::env::join_paths([
            h.path("broken-bin"),
            h.path("plain-bin"),
            h.path(".local/bin"),
        ])
        .unwrap();
        let out = h
            .command()
            .args(["provenance", "inspect", "tool", "--json"])
            .env("PATH", paths)
            .output()
            .unwrap();
        success(&out);
        let hits: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(hits[0]["active"], false);
        assert_eq!(hits[1]["active"], false);
        assert_eq!(hits[2]["active"], true);
        fs::remove_file(h.path(".local/bin/tool")).unwrap();
        for (base, manager) in [
            ("node", "node"),
            (".bun/install/global", "bun"),
            ("pnpm/.pnpm/fixture", "pnpm"),
        ] {
            let root = h.path(base).join("node_modules/fixture");
            executable(&root.join("cli.js"), b"node fixture");
            fs::write(
                root.join("package.json"),
                r#"{"name":"fixture","version":"3.2.1","bin":{"tool":"cli.js"}}"#,
            )
            .unwrap();
            let entry = h.path(".local/bin/tool");
            symlink(root.join("cli.js"), &entry).unwrap();
            let hit = h.run(&["provenance", "inspect", entry.to_str().unwrap(), "--json"]);
            assert_eq!(hit[0]["origin"]["manager"], manager);
            assert_eq!(hit[0]["origin"]["version"], "3.2.1");
            if manager == "node" {
                fs::write(
                    root.parent().unwrap().join(".package-lock.json"),
                    r#"{"packages":{"node_modules/fixture":{"version":"3.2.1"}}}"#,
                )
                .unwrap();
                let hit = h.run(&["provenance", "inspect", entry.to_str().unwrap(), "--json"]);
                assert_eq!(hit[0]["origin"]["manager"], "npm");
                fs::write(
                    root.join("package.json"),
                    r#"{"name":"fixture","version":"3.2.1","bin":{"other":"cli.js"}}"#,
                )
                .unwrap();
                let mismatch = h.run(&["provenance", "inspect", entry.to_str().unwrap(), "--json"]);
                assert!(mismatch[0]["origin"]["package"].is_null());
            }
            fs::remove_file(entry).unwrap();
        }
    }

    #[test]
    fn record_adopts_observations_without_package_ownership_or_entrypoint_changes() {
        let h = Home::new();
        let entry = h.path(".cargo/bin/tool");
        executable(&entry, b"foreign tool");
        let record = h.run(&["provenance", "record", entry.to_str().unwrap()]);
        assert_eq!(record["observations"][0]["origin"]["manager"], "cargo");
        assert!(record["replacement"].is_null());
        assert_eq!(fs::read(entry).unwrap(), b"foreign tool");
        assert!(!h.path(".wax/installed.json").exists());
        assert!(h.journal().exists());
    }

    #[test]
    fn replacement_preserves_file_link_or_absence_follows_opt_and_restores_idempotently() {
        for kind in ["file", "symlink", "missing"] {
            let h = Home::new();
            h.package("1.0.0");
            let entry = h.path(".local/bin/tool");
            match kind {
                "file" => executable(&entry, b"original tool"),
                "symlink" => {
                    executable(&h.path(".local/original"), b"original tool");
                    symlink("../original", &entry).unwrap();
                }
                _ => {}
            }
            let state = fs::read(h.path(".wax/installed.json")).unwrap();
            success(&h.replace(&entry, true));
            assert!(!h.path(".wax/provenance").exists());
            assert_eq!(fs::read(h.path(".wax/installed.json")).unwrap(), state);
            let out = h.replace(&entry, false);
            success(&out);
            let record: Value = serde_json::from_slice(&out.stdout).unwrap();
            let backup = PathBuf::from(record["replacement"]["backup"].as_str().unwrap());
            assert_eq!(fs::read(&entry).unwrap(), b"1.0.0");
            assert_eq!(
                fs::read_link(&entry).unwrap(),
                h.path(&format!(".local/wax/opt/{PACKAGE}/bin/tool"))
            );
            success(&h.replace(&entry, false));
            h.package("2.0.0");
            assert_eq!(fs::read(&entry).unwrap(), b"2.0.0");
            let journal = fs::read(h.journal()).unwrap();
            success(&h.restore(&entry, true));
            assert_eq!(fs::read(h.journal()).unwrap(), journal);
            assert_eq!(fs::read(&entry).unwrap(), b"2.0.0");
            success(&h.restore(&entry, false));
            success(&h.restore(&entry, false));
            if kind == "missing" {
                assert!(fs::symlink_metadata(&entry).is_err());
            } else {
                assert_eq!(fs::read(&entry).unwrap(), b"original tool");
                assert_eq!(
                    fs::symlink_metadata(&entry).unwrap().is_symlink(),
                    kind == "symlink"
                );
                if kind == "symlink" {
                    assert_eq!(fs::read_link(&entry).unwrap(), Path::new("../original"));
                }
            }
            assert!(fs::symlink_metadata(backup).is_err());
        }
    }

    #[test]
    fn replacement_refuses_manager_entries_unsafe_parents_and_mismatched_wax_targets() {
        let h = Home::new();
        h.package("1.0.0");
        for path in [".cargo/bin/tool", ".bun/bin/tool", ".local/wax/bin/tool"] {
            let entry = h.path(path);
            executable(&entry, b"manager tool");
            assert!(!h.replace(&entry, false).status.success());
            assert_eq!(fs::read(entry).unwrap(), b"manager tool");
        }
        let entry = h.path(".local/bin/tool");
        symlink(h.path(".cargo/bin/tool"), &entry).unwrap();
        assert!(!h.replace(&entry, false).status.success());
        assert_eq!(fs::read_link(&entry).unwrap(), h.path(".cargo/bin/tool"));
        fs::remove_file(&entry).unwrap();
        fs::remove_file(h.path(".cargo/bin/tool")).unwrap();
        symlink("../../.cargo/bin/tool", &entry).unwrap();
        let broken = h.run(&["provenance", "inspect", entry.to_str().unwrap(), "--json"]);
        assert_eq!(broken[0]["origin"]["manager"], "cargo");
        assert!(!h.replace(&entry, false).status.success());
        assert_eq!(
            fs::read_link(&entry).unwrap(),
            Path::new("../../.cargo/bin/tool")
        );
        fs::remove_file(&entry).unwrap();
        executable(&entry, b"original tool");
        fs::set_permissions(h.path(".local/bin"), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!h.replace(&entry, false).status.success());
        fs::set_permissions(h.path(".local/bin"), fs::Permissions::from_mode(0o755)).unwrap();
        let opt = h.path(&format!(".local/wax/opt/{PACKAGE}"));
        fs::remove_file(&opt).unwrap();
        symlink(h.path(".cargo"), &opt).unwrap();
        assert!(!h.replace(&entry, false).status.success());
        assert_eq!(fs::read(entry).unwrap(), b"original tool");
    }

    #[test]
    fn restore_preserves_intervening_changes_and_uninstall_requires_restore() {
        let h = Home::new();
        let keg = h.package("1.0.0");
        let entry = h.path(".local/bin/tool");
        executable(&entry, b"original tool");
        let out = h.replace(&entry, false);
        success(&out);
        let record: Value = serde_json::from_slice(&out.stdout).unwrap();
        let backup = PathBuf::from(record["replacement"]["backup"].as_str().unwrap());
        let out = h
            .command()
            .args(["uninstall", PACKAGE, "--yes"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("Restore provenance entrypoint"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(keg.join("bin/tool").exists());
        fs::remove_file(&entry).unwrap();
        executable(&entry, b"intervening user tool");
        assert!(!h.restore(&entry, false).status.success());
        assert_eq!(fs::read(&entry).unwrap(), b"intervening user tool");
        assert_eq!(fs::read(&backup).unwrap(), b"original tool");
        fs::remove_file(&entry).unwrap();
        executable(&backup, b"changed backup");
        assert!(!h.restore(&entry, false).status.success());
        assert_eq!(fs::read(&backup).unwrap(), b"changed backup");
        executable(&backup, b"original tool");
        success(&h.restore(&entry, false));
        let out = h
            .command()
            .args(["uninstall", PACKAGE, "--yes"])
            .output()
            .unwrap();
        success(&out);
        assert!(!keg.exists());
        assert_eq!(fs::read(entry).unwrap(), b"original tool");
    }

    #[test]
    fn restore_recovers_interrupted_replacement_and_completed_restore() {
        for phase in ["prepared", "restoring"] {
            let h = Home::new();
            h.package("1.0.0");
            let entry = h.path(".local/bin/tool");
            executable(&entry, b"original tool");
            success(&h.replace(&entry, false));
            let journal = h.journal();
            let mut record: Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
            record["replacement"]["phase"] = json!(phase);
            fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
            let backup = PathBuf::from(record["replacement"]["backup"].as_str().unwrap());
            let holding = backup.with_file_name(
                backup
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace("-backup", "-managed"),
            );
            if phase == "restoring" {
                fs::rename(&entry, &holding).unwrap();
                fs::rename(&backup, &entry).unwrap();
            } else {
                fs::remove_file(&entry).unwrap();
            }
            success(&h.restore(&entry, false));
            assert_eq!(fs::read(entry).unwrap(), b"original tool");
            assert!(fs::symlink_metadata(holding).is_err());
        }
    }

    #[test]
    fn concurrent_provenance_operations_preserve_entrypoints_and_package_payloads() {
        use std::os::fd::AsRawFd;
        let h = Home::new();
        let keg = h.package("1.0.0");
        let entry = h.path(".local/bin/tool");
        executable(&entry, b"original tool");
        fs::create_dir_all(h.path(".wax/provenance")).unwrap();
        let lock = fs::File::create(h.path(".wax/provenance/.lock")).unwrap();
        // SAFETY: this file owns the descriptor until it is dropped below.
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        let out = h.replace(&entry, false);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("Another Wax provenance operation"));
        let out = h
            .command()
            .args(["uninstall", PACKAGE, "--yes"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("Another Wax provenance operation"));
        assert_eq!(fs::read(&entry).unwrap(), b"original tool");
        assert_eq!(fs::read(keg.join("bin/tool")).unwrap(), b"1.0.0");
        drop(lock);
        success(&h.replace(&entry, false));
        success(&h.restore(&entry, false));
        assert_eq!(fs::read(entry).unwrap(), b"original tool");
    }

    #[test]
    fn formula_link_preserves_foreign_executables_and_relinks_its_own_versions() {
        for kind in ["file", "symlink"] {
            let h = Home::new();
            h.package("1.0.0");
            let entry = h.path(".local/wax/bin/tool");
            executable(&h.path("foreign"), b"foreign tool");
            fs::create_dir_all(entry.parent().unwrap()).unwrap();
            if kind == "file" {
                executable(&entry, b"foreign tool");
            } else {
                symlink(h.path("foreign"), &entry).unwrap();
            }
            let out = h.command().args(["link", PACKAGE]).output().unwrap();
            assert!(!out.status.success());
            assert_eq!(fs::read(&entry).unwrap(), b"foreign tool");
            assert_eq!(
                fs::symlink_metadata(&entry).unwrap().is_symlink(),
                kind == "symlink"
            );
            fs::remove_file(&entry).unwrap();
            success(&h.command().args(["link", PACKAGE]).output().unwrap());
            h.package("2.0.0");
            success(&h.command().args(["link", PACKAGE]).output().unwrap());
            assert_eq!(fs::read(entry).unwrap(), b"2.0.0");
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn cask_uninstall_requires_the_recorded_path_and_bundle_identity() {
    use std::fs;
    let home = tempfile::tempdir().unwrap();
    let canonical_home = home.path().canonicalize().unwrap();
    let root = canonical_home.as_path();
    let cache = root.join(".wax/cache");
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("casks.json"), "[]").unwrap();
    let state = root.join(".wax/installed_casks.json");
    let token = "wax-fixture-paper-desktop";
    let recorded = root.join("Custom/Paper.app");
    let neighbor = root.join("Applications/Paper.app");
    let bundle = |p: &Path, id: &str| {
        fs::create_dir_all(p.join("Contents")).unwrap();
        fs::write(p.join("Contents/Info.plist"), format!("<plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>{id}</string></dict></plist>")).unwrap();
    };
    let receipt = serde_json::json!({(token): {"name":token,"version":"1","install_date":0,"artifact_type":"dmg","app_name":"Paper.app","apps":[{"path":recorded,"identifier":"test.paper"}]}});
    let reset = || fs::write(&state, receipt.to_string()).unwrap();
    let remove = || {
        wax_with_home(root)
            .env("WAX_CACHE_DIR", &cache)
            .env("WAX_APPDIR", neighbor.parent().unwrap())
            .args(["uninstall", "--cask", token])
            .output()
            .unwrap()
    };
    reset();
    bundle(&neighbor, "test.paper");
    bundle(&recorded, "other.paper");
    let result = remove();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("identity changed"));
    assert!(recorded.is_dir() && neighbor.is_dir());
    let renamed = root.join("Custom/Renamed.app");
    bundle(&recorded, "test.paper");
    fs::rename(&recorded, &renamed).unwrap();
    assert!(remove().status.success());
    assert!(renamed.is_dir() && neighbor.is_dir());
    reset();
    bundle(&recorded, "test.paper");
    let mut ambiguous = receipt.clone();
    ambiguous["wax-fixture-paper-beta"] = receipt[token].clone();
    fs::write(&state, ambiguous.to_string()).unwrap();
    let result = remove();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Multiple casks claim"));
    assert!(recorded.is_dir());
    let mut legacy = receipt.clone();
    legacy[token].as_object_mut().unwrap().remove("apps");
    legacy[token]["installed_paths"] = serde_json::json!([recorded]);
    fs::write(&state, legacy.to_string()).unwrap();
    assert!(!remove().status.success());
    assert!(recorded.is_dir());
    reset();
    assert!(remove().status.success());
    assert!(!recorded.exists());
    assert!(renamed.is_dir() && neighbor.is_dir());
}

#[cfg(target_os = "macos")]
mod cask_receipt_regressions {
    use super::*;
    use serde_json::{json, Value};
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::process::Output;

    const TOKEN: &str = "wax-fixture-receipts";

    struct Fixture {
        _home: tempfile::TempDir,
        root: PathBuf,
        app: PathBuf,
        state: PathBuf,
        receipts: Value,
    }

    impl Fixture {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let root = home.path().canonicalize().unwrap();
            fs::create_dir_all(root.join(".wax/cache")).unwrap();
            fs::write(root.join(".wax/cache/casks.json"), "[]").unwrap();
            let app = root.join("Custom/Recorded.app");
            Self::bundle(&app);
            Self {
                state: root.join(".wax/installed_casks.json"),
                receipts: json!({(TOKEN): {
                    "name": TOKEN, "version": "1", "install_date": 0,
                    "artifact_type": "dmg", "app_name": "Recorded.app",
                    "binary_paths": null, "installed_paths": [],
                    "apps": [{"path": app, "identifier": "test.recorded"}]
                }}),
                _home: home,
                root,
                app,
            }
        }

        fn bundle(path: &Path) {
            fs::create_dir_all(path.join("Contents")).unwrap();
            fs::write(path.join("Contents/Info.plist"), "<plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>test.recorded</string></dict></plist>").unwrap();
            fs::write(path.join("Contents/payload"), b"keep this payload").unwrap();
        }

        fn remove(&self, args: &[&str]) -> Output {
            fs::write(&self.state, self.receipts.to_string()).unwrap();
            wax_with_home(&self.root)
                .current_dir(&self.root)
                .env("WAX_CACHE_DIR", self.root.join(".wax/cache"))
                .args(["uninstall", "--cask", TOKEN])
                .args(args)
                .output()
                .unwrap()
        }

        fn assert_receipt_retained(&self) {
            let after: Value = serde_json::from_slice(&fs::read(&self.state).unwrap()).unwrap();
            assert_eq!(after[TOKEN], self.receipts[TOKEN]);
        }
    }

    #[test]
    fn unsafe_app_receipts_preserve_bundles_and_ownership() {
        for case in [
            "symlink",
            "ancestor-symlink",
            "missing-id",
            "relative",
            "extension",
        ] {
            let mut f = Fixture::new();
            let path = match case {
                "symlink" => {
                    let link = f.root.join("Alias.app");
                    symlink(&f.app, &link).unwrap();
                    link
                }
                "ancestor-symlink" => {
                    let link = f.root.join("Alias");
                    symlink(f.app.parent().unwrap(), &link).unwrap();
                    link.join("Recorded.app")
                }
                "missing-id" => {
                    fs::write(
                        f.app.join("Contents/Info.plist"),
                        "<plist version=\"1.0\"><dict/></plist>",
                    )
                    .unwrap();
                    f.app.clone()
                }
                "relative" => PathBuf::from("Custom/Recorded.app"),
                "extension" => {
                    let path = f.root.join("Custom/Recorded.bundle");
                    Fixture::bundle(&path);
                    path
                }
                _ => unreachable!(),
            };
            f.receipts[TOKEN]["apps"][0]["path"] = json!(path);
            let result = f.remove(&[]);
            assert!(!result.status.success(), "accepted {case}");
            assert!(
                String::from_utf8_lossy(&result.stderr).contains("identity changed"),
                "{case}: {:?}",
                result
            );
            assert_eq!(
                fs::read(f.app.join("Contents/payload")).unwrap(),
                b"keep this payload",
                "{case}"
            );
            assert!(f.root.join(&path).exists(), "removed {case}");
            f.assert_receipt_retained();
        }
    }

    #[test]
    fn all_app_receipts_are_checked_before_any_deletion() {
        let mut f = Fixture::new();
        let second = f.root.join("Custom/Second.app");
        Fixture::bundle(&second);
        f.receipts[TOKEN]["apps"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "path": second, "identifier": "test.expected-other-id"
            }));
        let auxiliary = f.root.join("auxiliary-file");
        fs::write(&auxiliary, b"do not delete before validation").unwrap();
        f.receipts[TOKEN]["installed_paths"] = json!([auxiliary]);
        let result = f.remove(&[]);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("identity changed"));
        assert!(f.app.is_dir() && second.is_dir() && auxiliary.is_file());
        f.assert_receipt_retained();

        f.receipts[TOKEN]["apps"][1]["identifier"] = json!("test.recorded");
        let result = f.remove(&[]);
        assert!(result.status.success(), "{:?}", result);
        assert!(!f.app.exists() && !second.exists() && !auxiliary.exists());
    }

    #[test]
    fn case_aliased_claims_cannot_delete_another_casks_bundle() {
        for legacy in [false, true] {
            let mut f = Fixture::new();
            let alias = f.app.with_file_name("recorded.app");
            if !alias.exists() {
                eprintln!("case-alias scenario requires a case-insensitive volume");
                return;
            }
            let mut other = f.receipts[TOKEN].clone();
            other["name"] = json!("wax-fixture-other-owner");
            if legacy {
                other["apps"] = json!([]);
                other["installed_paths"] = json!([alias]);
            } else {
                other["apps"][0]["path"] = json!(alias);
            }
            f.receipts["wax-fixture-other-owner"] = other;
            let result = f.remove(&[]);
            assert!(
                !result.status.success(),
                "deleted shared bundle (legacy={legacy})"
            );
            assert!(String::from_utf8_lossy(&result.stderr).contains("Multiple casks claim"));
            assert_eq!(
                fs::read(f.app.join("Contents/payload")).unwrap(),
                b"keep this payload"
            );
            f.assert_receipt_retained();

            let separate = f.root.join("Other/Recorded.app");
            Fixture::bundle(&separate);
            if legacy {
                f.receipts["wax-fixture-other-owner"]["installed_paths"] = json!([separate]);
            } else {
                f.receipts["wax-fixture-other-owner"]["apps"][0]["path"] = json!(separate);
            }
            let result = f.remove(&[]);
            assert!(result.status.success(), "{:?}", result);
            assert!(!f.app.exists());
            assert_eq!(
                fs::read(separate.join("Contents/payload")).unwrap(),
                b"keep this payload"
            );
        }
    }

    #[test]
    fn dry_run_preserves_apps_auxiliary_files_and_receipt_contents() {
        let mut f = Fixture::new();
        let auxiliary = f.root.join("auxiliary-file");
        fs::write(&auxiliary, b"keep this file").unwrap();
        f.receipts[TOKEN]["installed_paths"] = json!([auxiliary]);
        let result = f.remove(&["--dry-run"]);
        assert!(result.status.success(), "{:?}", result);
        assert_eq!(
            fs::read(f.app.join("Contents/payload")).unwrap(),
            b"keep this payload"
        );
        assert_eq!(fs::read(auxiliary).unwrap(), b"keep this file");
        f.assert_receipt_retained();
    }

    #[test]
    fn legacy_binary_only_receipts_still_uninstall_without_touching_apps() {
        let mut f = Fixture::new();
        let binary = f.root.join("legacy-tool");
        fs::write(&binary, b"fixture binary").unwrap();
        let receipt = f.receipts[TOKEN].as_object_mut().unwrap();
        receipt.remove("apps");
        receipt.insert("app_name".into(), Value::Null);
        receipt.insert("artifact_type".into(), json!("binary"));
        receipt.insert("binary_paths".into(), json!([binary]));
        let result = f.remove(&[]);
        assert!(result.status.success(), "{:?}", result);
        assert!(!binary.exists());
        assert_eq!(
            fs::read(f.app.join("Contents/payload")).unwrap(),
            b"keep this payload"
        );
        let after: Value = serde_json::from_slice(&fs::read(&f.state).unwrap()).unwrap();
        assert!(after.get(TOKEN).is_none());
    }
}

#[cfg(windows)]
fn write_windows_manifest(home: &Path, ecosystem: &str, id: &str, version: &str) {
    let root = home.join(".local").join("wax");
    let staging = root
        .join(format!("{ecosystem}-apps"))
        .join(id)
        .join(version);
    std::fs::create_dir_all(&staging).unwrap();
    let manifest_dir = root.join("windows").join("manifests");
    std::fs::create_dir_all(&manifest_dir).unwrap();

    let ecosystem_enum = match ecosystem {
        "scoop" => "Scoop",
        "winget" => "Winget",
        "choco" => "Chocolatey",
        _ => panic!("unknown ecosystem {ecosystem}"),
    };
    let staging_json = staging.to_string_lossy().replace('\\', "/");
    let manifest = format!(
        r#"{{
  "ecosystem": "{ecosystem_enum}",
  "id": "{id}",
  "version": "{version}",
  "source": "https://example.invalid/{id}.zip",
  "staging_dir": "{staging_json}",
  "bin_links": [],
  "files": [],
  "install_kind": "Portable",
  "installed_at": 0
}}"#
    );
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
    std::fs::write(
        manifest_dir.join(format!("{ecosystem}-{safe_id}.json")),
        manifest,
    )
    .unwrap();
}

// ── basic smoke tests ────────────────────────────────────────────────────────

#[test]
fn version_flag_exits_zero() {
    let out = wax().arg("--version").output().expect("failed to run wax");
    assert!(out.status.success(), "exit code: {:?}", out.status.code());
}

#[test]
fn version_output_contains_version_string() {
    let out = wax().arg("--version").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.contains("wax"),
        "expected 'wax' in output, got: {combined}"
    );
}

#[test]
fn help_flag_exits_zero() {
    let out = wax().arg("--help").output().unwrap();
    assert!(out.status.success());
}

#[test]
fn help_output_contains_subcommands() {
    let out = wax().arg("--help").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    for cmd in &[
        "install",
        "search",
        "update",
        "self-update",
        "list",
        "info",
        "upgrade",
        "uninstall",
    ] {
        assert!(
            stdout.contains(cmd),
            "help output missing subcommand '{cmd}': {stdout}"
        );
    }
}

#[test]
fn subcommand_help_exits_zero() {
    for sub in &[
        "install",
        "search",
        "self-update",
        "info",
        "list",
        "upgrade",
        "uninstall",
        "tap",
    ] {
        let out = wax().args([sub, "--help"]).output().unwrap();
        assert!(
            out.status.success(),
            "wax {sub} --help failed: {:?}",
            out.status.code()
        );
    }
}

#[test]
fn doctor_help_mentions_full_flag() {
    let out = wax().args(["doctor", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--full"), "{stdout}");
}

#[test]
fn install_help_mentions_no_script_flag() {
    let out = wax().args(["install", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--no-script"), "{stdout}");
}

#[test]
fn update_help_mentions_self_nightly_shorts() {
    let out = wax().args(["update", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("-s"), "{stdout}");
    assert!(stdout.contains("-n"), "{stdout}");
}

#[test]
fn upgrade_help_mentions_self_nightly_shorts() {
    let out = wax().args(["upgrade", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("-s"), "{stdout}");
    assert!(stdout.contains("-n"), "{stdout}");
    assert!(stdout.contains("--clean"), "{stdout}");
}

#[test]
fn self_update_help_mentions_stable_and_nightly_flags() {
    let out = wax().args(["self-update", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--nightly"), "{stdout}");
    assert!(stdout.contains("--force"), "{stdout}");
    assert!(stdout.contains("--clean"), "{stdout}");
}

fn has_timing_line(stdout: &str) -> bool {
    stdout.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with('[') && trimmed.ends_with("ms]")
    })
}

#[test]
fn time_to_action_flag_prints_elapsed_footer() {
    let tmp = tempfile::tempdir().unwrap();
    let out = wax_with_home(tmp.path())
        .env("CI", "1")
        .env("WAX_CACHE_DIR", tmp.path().join("cache"))
        .args(["--time-to-action", "list"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "wax --time-to-action list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(has_timing_line(&stdout), "{stdout}");
}

#[test]
fn time_to_action_aliases_print_elapsed_footer() {
    for alias in ["--tta", "--time"] {
        let tmp = tempfile::tempdir().unwrap();
        let out = wax_with_home(tmp.path())
            .env("CI", "1")
            .env("WAX_CACHE_DIR", tmp.path().join("cache"))
            .args([alias, "list"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "wax {alias} list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(has_timing_line(&stdout), "{stdout}");
    }
}

#[test]
fn list_without_time_flag_omits_elapsed_footer() {
    let tmp = tempfile::tempdir().unwrap();
    let out = wax_with_home(tmp.path())
        .env("CI", "1")
        .env("WAX_CACHE_DIR", tmp.path().join("cache"))
        .args(["list"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "wax list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!has_timing_line(&stdout), "{stdout}");
}

#[test]
fn upgrade_batches_cask_force_reinstalls() {
    let source = std::fs::read_to_string("src/commands/upgrade.rs").unwrap();
    assert!(
        source.contains("&cask_names") && source.contains("force_reinstall: true"),
        "upgrade should pass all outdated casks into one force reinstall pipeline"
    );
}

#[test]
fn upgrade_runs_formulae_and_casks_in_parallel() {
    let source = std::fs::read_to_string("src/commands/upgrade.rs").unwrap();
    assert!(
        source.contains("tokio::join!(formula_stats, cask_fut)"),
        "upgrade should run formula and cask progress in parallel using tokio::join!"
    );
}

#[test]
fn cask_pipeline_concurrency_is_fifteen() {
    let source = std::fs::read_to_string("src/commands/install.rs").unwrap();
    assert!(
        source.contains("const CASK_PIPELINE_CONCURRENCY: usize = 15;"),
        "cask pipeline should keep up to 15 casks active"
    );
}

#[test]
fn upgrade_does_not_preplan_dependent_reinstalls() {
    let source = std::fs::read_to_string("src/commands/upgrade.rs").unwrap();
    assert!(
        !source.contains("dependents_to_reinstall"),
        "upgrade should not automatically reinstall reverse dependencies"
    );
}

#[test]
fn single_formula_upgrade_does_not_reinstall_dependents() {
    let source = std::fs::read_to_string("src/commands/upgrade.rs").unwrap();
    assert!(
        !source.contains("reinstall_dependents"),
        "single formula upgrade should leave healthy dependents untouched"
    );
}

// ── list / tap list work offline ─────────────────────────────────────────────

#[test]
fn list_exits_zero() {
    // `wax list` works without a populated cache (just shows an empty list).
    let tmp = tempfile::tempdir().unwrap();
    let out = wax_with_home(tmp.path())
        .env("CI", "1")
        .env("WAX_CACHE_DIR", tmp.path().join("cache"))
        .arg("list")
        .output()
        .unwrap();
    // Either success or a clean "no packages" message; not a crash.
    assert!(
        out.status.success(),
        "wax list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn list_with_query_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let out = wax_with_home(tmp.path())
        .env("CI", "1")
        .env("WAX_CACHE_DIR", tmp.path().join("cache"))
        .args(["list", "rust"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "wax list rust failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Hermetic Cellar layout via `WAX_TEST_CELLAR` (see `commands/list.rs`).
#[cfg(not(windows))]
#[test]
fn list_plain_shows_test_cellar_formulae() {
    let tmp = tempfile::tempdir().unwrap();
    let cellar = tmp.path().join("Cellar");
    std::fs::create_dir_all(cellar.join("wax-a-listtest/1.0.0")).unwrap();
    std::fs::create_dir_all(cellar.join("wax-b-listtest/2.0.0")).unwrap();
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();

    let out = wax_with_home(tmp.path())
        .env("CI", "1")
        .env("WAX_CACHE_DIR", &cache)
        .env("WAX_TEST_CELLAR", &cellar)
        .arg("list")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("wax-a-listtest"),
        "expected formula a in output: {stdout}"
    );
    assert!(
        stdout.contains("wax-b-listtest"),
        "expected formula b in output: {stdout}"
    );
}

#[cfg(not(windows))]
#[test]
fn list_plain_filter_excludes_non_matching() {
    let tmp = tempfile::tempdir().unwrap();
    let cellar = tmp.path().join("Cellar");
    std::fs::create_dir_all(cellar.join("wax-a-listtest/1.0.0")).unwrap();
    std::fs::create_dir_all(cellar.join("wax-b-listtest/2.0.0")).unwrap();
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();

    let out = wax_with_home(tmp.path())
        .env("CI", "1")
        .env("WAX_CACHE_DIR", &cache)
        .env("WAX_TEST_CELLAR", &cellar)
        .args(["list", "wax-b"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("wax-b-listtest"),
        "expected filtered formula: {stdout}"
    );
    assert!(
        !stdout.contains("wax-a-listtest"),
        "did not expect excluded formula: {stdout}"
    );
}

#[cfg(not(windows))]
#[test]
fn list_plain_no_match_reports_query() {
    let tmp = tempfile::tempdir().unwrap();
    let cellar = tmp.path().join("Cellar");
    std::fs::create_dir_all(cellar.join("only-wax-pkg/1.0")).unwrap();
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();

    let needle = "zzz-nope-match";
    let out = wax_with_home(tmp.path())
        .env("CI", "1")
        .env("WAX_CACHE_DIR", &cache)
        .env("WAX_TEST_CELLAR", &cellar)
        .args(["list", needle])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no installed packages match"), "{stdout}");
    assert!(stdout.contains(needle), "{stdout}");
}

#[cfg(windows)]
#[test]
fn list_plain_shows_windows_manifests() {
    let tmp = tempfile::tempdir().unwrap();
    write_windows_manifest(tmp.path(), "winget", "wax-a-listtest", "1.0.0");
    write_windows_manifest(tmp.path(), "scoop", "wax-b-listtest", "2.0.0");
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();

    let out = wax_with_home(tmp.path())
        .env("CI", "1")
        .env("WAX_CACHE_DIR", &cache)
        .arg("list")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("winget/wax-a-listtest"),
        "expected winget manifest in output: {stdout}"
    );
    assert!(
        stdout.contains("scoop/wax-b-listtest"),
        "expected scoop manifest in output: {stdout}"
    );
}

#[cfg(windows)]
#[test]
fn list_plain_filter_excludes_non_matching_windows() {
    let tmp = tempfile::tempdir().unwrap();
    write_windows_manifest(tmp.path(), "winget", "wax-a-listtest", "1.0.0");
    write_windows_manifest(tmp.path(), "scoop", "wax-b-listtest", "2.0.0");
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();

    let out = wax_with_home(tmp.path())
        .env("CI", "1")
        .env("WAX_CACHE_DIR", &cache)
        .args(["list", "wax-b"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("scoop/wax-b-listtest"),
        "expected filtered manifest: {stdout}"
    );
    assert!(
        !stdout.contains("wax-a-listtest"),
        "did not expect excluded manifest: {stdout}"
    );
}

#[cfg(windows)]
#[test]
fn list_plain_no_match_reports_query_windows() {
    let tmp = tempfile::tempdir().unwrap();
    write_windows_manifest(tmp.path(), "winget", "only-wax-pkg", "1.0");
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();

    let needle = "zzz-nope-match";
    let out = wax_with_home(tmp.path())
        .env("CI", "1")
        .env("WAX_CACHE_DIR", &cache)
        .args(["list", needle])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no installed packages match"), "{stdout}");
    assert!(stdout.contains(needle), "{stdout}");
}

#[cfg(target_os = "macos")]
#[test]
fn cask_command_fails_when_no_requested_cask_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("formulae.json"), "[]").unwrap();
    std::fs::write(cache.join("casks.json"), "[]").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        cache.join("metadata.json"),
        format!(
            r#"{{"last_updated":{now},"formula_count":0,"cask_count":0,"formulae_etag":null,"formulae_last_modified":null,"casks_etag":null,"casks_last_modified":null}}"#
        ),
    )
    .unwrap();

    let out = wax_with_home(tmp.path())
        .env("WAX_CACHE_DIR", &cache)
        .env("CI", "1")
        .args(["cask", "--dry-run", "missing-cask"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "unexpected success: {stderr}");
    assert!(stderr.contains("cask not found"), "{stderr}");
}

#[cfg(target_os = "macos")]
#[test]
fn cask_command_indexes_a_newly_tapped_legacy_untrusted_cask() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    let tap = tmp.path().join("tinycast-tap");
    std::fs::create_dir_all(tap.join("Casks")).unwrap();
    std::fs::write(
        tap.join("Casks/tinycast.rb"),
        r#"cask "tinycast" do
  version "1.0.0"
  sha256 :no_check
  url "https://example.invalid/tinycast.zip"
  name "Tinycast"
  desc "Test cask"
  homepage "https://example.invalid/"
  app "Tinycast.app"
end
"#,
    )
    .unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("formulae.json"), "[]").unwrap();
    std::fs::write(cache.join("casks.json"), "[]").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        cache.join("metadata.json"),
        format!(
            r#"{{"last_updated":{now},"formula_count":0,"cask_count":0,"formulae_etag":null,"formulae_last_modified":null,"casks_etag":null,"casks_last_modified":null}}"#
        ),
    )
    .unwrap();
    std::fs::create_dir_all(home.join(".wax")).unwrap();
    std::fs::write(
        home.join(".wax/taps.json"),
        serde_json::json!({
            "abue-ammar/tinycast": {
                "full_name": "abue-ammar/tinycast",
                "path": tap,
                "trusted": false
            }
        })
        .to_string(),
    )
    .unwrap();

    let out = wax_with_home(&home)
        .env("WAX_CACHE_DIR", &cache)
        .env("CI", "1")
        .args(["cask", "--dry-run", "abue-ammar/tinycast/tinycast"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        !stderr.contains("cask not found"),
        "unexpected cask lookup failure: {stderr}"
    );
}

#[cfg(not(windows))]
#[test]
fn tap_list_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let out = wax_with_home(tmp.path())
        .env("WAX_CACHE_DIR", tmp.path().join("cache"))
        .arg("tap")
        .arg("list")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "wax tap list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(windows)]
#[test]
fn tap_list_rejected_on_windows() {
    let tmp = tempfile::tempdir().unwrap();
    let out = wax_with_home(tmp.path())
        .env("WAX_CACHE_DIR", tmp.path().join("cache"))
        .args(["tap", "list"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not available on Windows"), "{stderr}");
}

// ── invalid input should not panic ───────────────────────────────────────────

#[test]
fn install_no_args_does_not_panic() {
    let out = wax().arg("install").output().unwrap();
    // Should not panic or abort.
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Must not produce a Rust panic message.
    assert!(
        !stderr.contains("thread 'main' panicked"),
        "wax panicked: {stderr}"
    );
}

#[test]
fn search_no_args_does_not_panic() {
    let out = wax().arg("search").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("thread 'main' panicked"), "{stderr}");
}

#[test]
#[cfg(not(windows))]
fn search_windows_prefix_reports_platform_without_touching_the_network() {
    for prefix in ["scoop", "winget", "choco", "chocolatey"] {
        let home = tempfile::tempdir().unwrap();
        let out = wax_with_home(home.path())
            .arg("search")
            .arg(format!("{prefix}/ripgrep"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "search {prefix}/ failed: {stdout}");
        assert!(
            stdout.contains("only available on Windows"),
            "search {prefix}/ said: {stdout}"
        );
    }
}

#[test]
#[cfg(not(windows))]
fn search_empty_query_short_circuits() {
    let home = tempfile::tempdir().unwrap();
    let out = wax_with_home(home.path())
        .arg("search")
        .arg("   ")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "empty search failed: {stdout}");
    assert!(stdout.contains("empty search query"), "said: {stdout}");
}

#[test]
fn unknown_subcommand_exits_nonzero() {
    let out = wax()
        .arg("definitely-not-a-real-subcommand")
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[cfg(not(windows))]
#[test]
fn reinstall_missing_package_exits_nonzero_without_installing() {
    let tmp = tempfile::tempdir().unwrap();
    let out = wax_with_home(tmp.path())
        .env("CI", "1")
        .args(["reinstall", "definitely-no-such-package"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("definitely-no-such-package is not installed"),
        "{stderr}"
    );
}

#[cfg(windows)]
#[test]
fn reinstall_rejected_on_windows() {
    let tmp = tempfile::tempdir().unwrap();
    let out = wax_with_home(tmp.path())
        .env("CI", "1")
        .args(["reinstall", "definitely-no-such-package"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not available on Windows"), "{stderr}");
}

// ── network integration tests (skipped unless INTEGRATION=1) ─────────────────

fn integration_enabled() -> bool {
    std::env::var("INTEGRATION").unwrap_or_default() == "1"
}

#[test]
fn search_tree_finds_results() {
    if !integration_enabled() {
        return;
    }
    let out = wax().args(["search", "tree"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("tree"), "expected 'tree' in search results");
}

#[test]
fn info_tree_shows_details() {
    if !integration_enabled() {
        return;
    }
    let out = wax().args(["info", "tree"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("tree"));
}

#[test]
fn update_fetches_index() {
    if !integration_enabled() {
        return;
    }
    let cache_dir = tempfile::tempdir().unwrap();
    let out = wax()
        .env("WAX_CACHE_DIR", cache_dir.path())
        .arg("update")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "wax update failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Cache should now exist.
    assert!(cache_dir.path().join("formulae.json").exists());
    assert!(cache_dir.path().join("casks.json").exists());
}
