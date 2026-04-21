//! Integration tests for the `wax` CLI binary.
//!
//! These tests compile and run the real binary so they exercise the full
//! command dispatch path.  Network-dependent tests are gated behind the
//! `INTEGRATION` env var so they don't run in CI without connectivity.

use std::process::Command;

fn wax() -> Command {
    // Use the debug binary built by `cargo test --test cli`.
    let bin = env!("CARGO_BIN_EXE_wax");
    Command::new(bin)
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

// ── list / tap list work offline ─────────────────────────────────────────────

#[test]
fn list_exits_zero() {
    // `wax list` works without a populated cache (just shows an empty list).
    let tmp = tempfile::tempdir().unwrap();
    let out = wax()
        .env("WAX_CACHE_DIR", tmp.path())
        .env("CI", "1")
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
    let out = wax()
        .env("WAX_CACHE_DIR", tmp.path())
        .env("CI", "1")
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
#[test]
fn list_plain_shows_test_cellar_formulae() {
    let tmp = tempfile::tempdir().unwrap();
    let cellar = tmp.path().join("Cellar");
    std::fs::create_dir_all(cellar.join("wax-a-listtest/1.0.0")).unwrap();
    std::fs::create_dir_all(cellar.join("wax-b-listtest/2.0.0")).unwrap();
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();

    let out = wax()
        .env("WAX_CACHE_DIR", &cache)
        .env("WAX_TEST_CELLAR", &cellar)
        .env("CI", "1")
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

#[test]
fn list_plain_filter_excludes_non_matching() {
    let tmp = tempfile::tempdir().unwrap();
    let cellar = tmp.path().join("Cellar");
    std::fs::create_dir_all(cellar.join("wax-a-listtest/1.0.0")).unwrap();
    std::fs::create_dir_all(cellar.join("wax-b-listtest/2.0.0")).unwrap();
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();

    let out = wax()
        .env("WAX_CACHE_DIR", &cache)
        .env("WAX_TEST_CELLAR", &cellar)
        .env("CI", "1")
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

#[test]
fn list_plain_no_match_reports_query() {
    let tmp = tempfile::tempdir().unwrap();
    let cellar = tmp.path().join("Cellar");
    std::fs::create_dir_all(cellar.join("only-wax-pkg/1.0")).unwrap();
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();

    let needle = "zzz-nope-match";
    let out = wax()
        .env("WAX_CACHE_DIR", &cache)
        .env("WAX_TEST_CELLAR", &cellar)
        .env("CI", "1")
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

#[test]
fn tap_list_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let out = wax()
        .env("WAX_CACHE_DIR", tmp.path())
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

#[test]
fn hidden_refresh_state_command_exits_zero() {
    let out = wax().arg("__refresh_state").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

// ── invalid input should not panic ───────────────────────────────────────────

#[test]
fn install_no_args_does_not_panic() {
    let out = wax().arg("install").output().unwrap();
    // Should not panic or abort.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "wax install failed: {stderr}");
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
fn unknown_subcommand_exits_nonzero() {
    let out = wax()
        .arg("definitely-not-a-real-subcommand")
        .output()
        .unwrap();
    assert!(!out.status.success());
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
