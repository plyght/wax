use super::*;
use crate::install::HOME_MUTEX;

fn artifacts(json: &str) -> Vec<CaskArtifact> {
    serde_json::from_str(json).unwrap()
}

#[test]
fn parses_homebrew_api_uninstall_stanzas() {
    let d = parse(&artifacts(
        r#"[
            {"app": ["Foo.app"]},
            {"uninstall": [{
                "quit": "com.example.foo",
                "launchctl": ["com.example.helper", "com.example.agent"],
                "pkgutil": "com.example.foo.*",
                "signal": [["TERM", "com.example.foo"]],
                "script": {"executable": "/Library/Foo/uninstall.sh", "args": ["--all"], "sudo": true},
                "delete": ["/Library/Foo", "~/Library/Foo"],
                "rmdir": "/Library/Foo Parent"
            }]},
            {"zap": [{"trash": "~/Library/Caches/Foo"}]}
        ]"#,
    ));
    assert_eq!(d.quit, vec!["com.example.foo"]);
    assert_eq!(d.launchctl.len(), 2);
    assert_eq!(d.pkgutil, vec!["com.example.foo.*"]);
    assert_eq!(d.signal, vec![("TERM".into(), "com.example.foo".into())]);
    assert_eq!(
        d.script,
        vec![Script {
            executable: "/Library/Foo/uninstall.sh".into(),
            args: vec!["--all".into()],
            sudo: true
        }]
    );
    assert_eq!(d.delete, vec!["/Library/Foo", "~/Library/Foo"]);
    assert!(d.trash.is_empty(), "zap is not part of uninstall");
}

#[test]
fn protects_system_and_home_roots() {
    let _guard = HOME_MUTEX.lock().unwrap();
    std::env::set_var("HOME", "/Users/fixture");
    for path in [
        "/",
        "/Applications",
        "/Library",
        "/usr",
        "/Users",
        "/Users/fixture",
        "/Users/fixture/Library",
        "/Users/fixture/Library/Application Support",
        "/Users/fixture/Documents",
    ] {
        assert!(is_protected(Path::new(path)), "{path}");
    }
    for path in [
        "/Library/Foo",
        "/Users/fixture/Library/Application Support/Foo",
        "/Applications/Foo.app",
    ] {
        assert!(!is_protected(Path::new(path)), "{path}");
    }
    assert_eq!(expand("relative/path"), None);
    assert_eq!(expand("/Library/../etc"), None);
}

#[test]
fn delete_trash_and_rmdir_with_globs() {
    let _guard = HOME_MUTEX.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());
    let base = home.path().join("Library/Foo Stuff");
    std::fs::create_dir_all(base.join("cache-1")).unwrap();
    std::fs::create_dir_all(base.join("cache-2")).unwrap();
    std::fs::write(base.join("keep.txt"), "x").unwrap();
    std::fs::write(home.path().join("Library/foo.plist"), "x").unwrap();
    std::fs::create_dir_all(home.path().join("Library/EmptyFoo")).unwrap();

    let d = Directives {
        delete: vec![format!("{}/cache-*", base.display())],
        trash: vec!["~/Library/foo.plist".into()],
        rmdir: vec!["~/Library/EmptyFoo".into(), format!("{}", base.display())],
        ..Directives::default()
    };
    let warnings = run(&d);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(!base.join("cache-1").exists());
    assert!(!base.join("cache-2").exists());
    assert!(
        base.join("keep.txt").exists(),
        "non-empty dir must survive rmdir"
    );
    assert!(!home.path().join("Library/foo.plist").exists());
    assert!(home.path().join(".Trash/foo.plist").exists());
    assert!(!home.path().join("Library/EmptyFoo").exists());

    let protected = Directives {
        delete: vec!["~/Library".into()],
        ..Directives::default()
    };
    assert_eq!(run(&protected).len(), 1);
    assert!(home.path().join("Library").exists());
}
