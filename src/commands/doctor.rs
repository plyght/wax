use crate::api::ApiClient;
use crate::bottle::{detect_platform, homebrew_prefix, run_command_with_timeout};
use crate::cache::Cache;
use crate::cask::CaskState;
use crate::error::Result;
use crate::install::{InstallMode, InstallState};
use console::style;
use std::path::Path;

struct DiagResult {
    passed: usize,
    warned: usize,
    failed: usize,
    fixed: usize,
    fix: bool,
}

impl DiagResult {
    fn new(fix: bool) -> Self {
        Self {
            passed: 0,
            warned: 0,
            failed: 0,
            fixed: 0,
            fix,
        }
    }

    fn pass(&mut self, msg: &str) {
        self.passed += 1;
        println!("  {} {}", style("✓").green(), msg);
    }

    fn warn(&mut self, msg: &str) {
        self.warned += 1;
        println!("  {} {}", style("!").yellow(), msg);
    }

    fn fail(&mut self, msg: &str) {
        self.failed += 1;
        println!("  {} {}", style("✗").red(), msg);
    }

    fn fixed(&mut self, msg: &str) {
        self.fixed += 1;
        println!("  {} {}", style("⚡").cyan(), msg);
    }
}

pub async fn doctor(cache: &Cache, fix: bool) -> Result<()> {
    let mut d = DiagResult::new(fix);

    if fix {
        println!("{}", style("wax doctor --fix").bold());
    } else {
        println!("{}", style("wax doctor").bold());
    }
    println!();

    check_platform(&mut d);
    check_prefix(&mut d);
    check_cellar(&mut d).await;
    check_symlink_dirs(&mut d).await;
    check_cache(cache, &mut d).await;
    check_install_state(&mut d).await;
    check_cask_state(&mut d).await;
    check_broken_symlinks(&mut d).await;
    check_state_consistency(&mut d).await;
    check_tools(&mut d);

    println!();
    let mut parts = vec![format!("{} passed", style(d.passed).green())];
    if d.warned > 0 {
        parts.push(format!("{} warnings", style(d.warned).yellow()));
    }
    if d.failed > 0 {
        parts.push(format!("{} errors", style(d.failed).red()));
    }
    if d.fixed > 0 {
        parts.push(format!("{} fixed", style(d.fixed).cyan()));
    }
    println!("{}: {}", style("result").bold(), parts.join(", "));

    if !fix && (d.warned > 0 || d.failed > 0) {
        println!(
            "\n  {} run {} to auto-fix issues",
            style("hint:").dim(),
            style("wax doctor --fix").yellow()
        );
    }

    Ok(())
}

fn check_platform(d: &mut DiagResult) {
    let platform = detect_platform();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    if platform == "unknown" {
        d.fail(&format!("unsupported platform: {}-{}", os, arch));
    } else {
        d.pass(&format!("platform: {} ({}-{})", platform, os, arch));
    }
}

fn check_prefix(d: &mut DiagResult) {
    let prefix = homebrew_prefix();

    if prefix.exists() {
        d.pass(&format!("prefix exists: {}", prefix.display()));
    } else if d.fix {
        match std::fs::create_dir_all(&prefix) {
            Ok(_) => d.fixed(&format!("created prefix: {}", prefix.display())),
            Err(e) => d.fail(&format!(
                "cannot create prefix {}: {} (try with sudo)",
                prefix.display(),
                e
            )),
        }
    } else {
        d.fail(&format!("prefix missing: {}", prefix.display()));
        return;
    }

    if is_writable(&prefix) {
        d.pass(&format!("prefix writable: {}", prefix.display()));
    } else {
        d.warn(&format!(
            "prefix not writable: {} (use --user or sudo)",
            prefix.display()
        ));
    }
}

async fn check_cellar(d: &mut DiagResult) {
    let global_mode = InstallMode::Global;
    if let Ok(cellar) = global_mode.cellar_path() {
        if cellar.exists() {
            let count = std::fs::read_dir(&cellar)
                .map(|entries| entries.filter_map(|e| e.ok()).count())
                .unwrap_or(0);
            d.pass(&format!(
                "cellar: {} ({} packages)",
                cellar.display(),
                count
            ));
        } else if d.fix {
            match std::fs::create_dir_all(&cellar) {
                Ok(_) => d.fixed(&format!("created cellar: {}", cellar.display())),
                Err(e) => d.warn(&format!("cannot create cellar: {}", e)),
            }
        } else {
            d.warn(&format!("cellar missing: {}", cellar.display()));
        }
    }

    let user_mode = InstallMode::User;
    if let Ok(cellar) = user_mode.cellar_path() {
        if cellar.exists() {
            let count = std::fs::read_dir(&cellar)
                .map(|entries| entries.filter_map(|e| e.ok()).count())
                .unwrap_or(0);
            d.pass(&format!(
                "user cellar: {} ({} packages)",
                cellar.display(),
                count
            ));
        }
    }
}

async fn check_symlink_dirs(d: &mut DiagResult) {
    let prefix = homebrew_prefix();
    let dirs = ["bin", "lib", "include", "share", "opt"];

    for dir in &dirs {
        let path = prefix.join(dir);
        if path.exists() {
            continue;
        }
        if d.fix {
            match std::fs::create_dir_all(&path) {
                Ok(_) => d.fixed(&format!("created {}", path.display())),
                Err(e) => d.warn(&format!("cannot create {}: {}", path.display(), e)),
            }
        } else {
            d.warn(&format!("{} directory missing: {}", dir, path.display()));
        }
    }

    let bin_dir = prefix.join("bin");
    if bin_dir.exists() {
        if let Ok(path_var) = std::env::var("PATH") {
            let bin_str = bin_dir.to_string_lossy();
            if path_var.split(':').any(|p| p == bin_str.as_ref()) {
                d.pass(&format!("{} is in PATH", bin_dir.display()));
            } else {
                d.warn(&format!(
                    "{} is not in PATH — add it to your shell profile",
                    bin_dir.display()
                ));
            }
        }
    }
}

async fn check_cache(cache: &Cache, d: &mut DiagResult) {
    match cache.load_metadata().await {
        Ok(Some(meta)) => {
            d.pass(&format!(
                "cache: {} formulae, {} casks",
                meta.formula_count, meta.cask_count
            ));

            let age_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                - meta.last_updated;

            let age_hours = age_secs / 3600;
            if age_hours > 168 {
                if d.fix {
                    d.warn(&format!("cache is {} days old — refreshing...", age_hours / 24));
                    let api_client = ApiClient::new();
                    match super::update::update(&api_client, cache).await {
                        Ok(_) => d.fixed("cache refreshed"),
                        Err(e) => d.fail(&format!("cache refresh failed: {}", e)),
                    }
                } else {
                    d.warn(&format!(
                        "cache is {} days old — run `wax update`",
                        age_hours / 24
                    ));
                }
            } else {
                d.pass(&format!(
                    "cache age: {}h (updated recently)",
                    age_hours.max(0)
                ));
            }
        }
        Ok(None) => {
            if d.fix {
                d.warn("cache not initialized — refreshing...");
                let api_client = ApiClient::new();
                match super::update::update(&api_client, cache).await {
                    Ok(_) => d.fixed("cache initialized"),
                    Err(e) => d.fail(&format!("cache init failed: {}", e)),
                }
            } else {
                d.fail("cache not initialized — run `wax update`");
            }
        }
        Err(e) => {
            d.fail(&format!("cache error: {}", e));
        }
    }
}

async fn check_install_state(d: &mut DiagResult) {
    match InstallState::new() {
        Ok(state) => match state.load().await {
            Ok(packages) => {
                d.pass(&format!("install state: {} packages tracked", packages.len()));
            }
            Err(e) => {
                if d.fix {
                    d.warn(&format!("install state corrupt: {}", e));
                    match state.save(&std::collections::HashMap::new()).await {
                        Ok(_) => {
                            match state.sync_from_cellar().await {
                                Ok(_) => d.fixed("install state rebuilt from cellar"),
                                Err(_) => d.fixed("install state reset to empty"),
                            }
                        }
                        Err(e2) => d.fail(&format!("cannot reset install state: {}", e2)),
                    }
                } else {
                    d.fail(&format!("install state corrupt: {}", e));
                }
            }
        },
        Err(e) => {
            d.fail(&format!("install state unavailable: {}", e));
        }
    }
}

async fn check_cask_state(d: &mut DiagResult) {
    match CaskState::new() {
        Ok(state) => match state.load().await {
            Ok(casks) => {
                if !casks.is_empty() {
                    d.pass(&format!("cask state: {} casks tracked", casks.len()));
                }
            }
            Err(e) => {
                if d.fix {
                    d.warn(&format!("cask state corrupt: {}", e));
                    match state.save(&std::collections::HashMap::new()).await {
                        Ok(_) => d.fixed("cask state reset"),
                        Err(e2) => d.fail(&format!("cannot reset cask state: {}", e2)),
                    }
                } else {
                    d.fail(&format!("cask state corrupt: {}", e));
                }
            }
        },
        Err(e) => {
            d.fail(&format!("cask state unavailable: {}", e));
        }
    }
}

async fn check_broken_symlinks(d: &mut DiagResult) {
    let prefix = homebrew_prefix();
    let link_dirs = ["bin", "lib", "sbin", "include", "share", "opt"];

    let mut total_broken = 0;
    let mut total_removed = 0;

    for dir_name in &link_dirs {
        let dir = prefix.join(dir_name);
        if !dir.exists() {
            continue;
        }

        let broken = collect_broken_symlinks_recursive(&dir);

        if broken.is_empty() {
            continue;
        }

        for path in &broken {
            total_broken += 1;
            let rel = path.strip_prefix(&prefix).unwrap_or(path);

            if d.fix {
                match std::fs::remove_file(path) {
                    Ok(_) => {
                        total_removed += 1;
                        if total_removed <= 10 {
                            d.fixed(&format!("removed broken symlink: {}", rel.display()));
                        }
                    }
                    Err(e) => {
                        d.fail(&format!(
                            "cannot remove {}: {}",
                            rel.display(),
                            e
                        ));
                    }
                }
            } else if total_broken <= 5 {
                d.fail(&format!("broken symlink: {}", rel.display()));
            }
        }
    }

    if total_broken == 0 {
        d.pass("no broken symlinks");
    } else if d.fix {
        if total_removed > 10 {
            d.fixed(&format!(
                "... and {} more broken symlinks removed",
                total_removed - 10
            ));
        }
    } else if total_broken > 5 {
        d.fail(&format!(
            "... and {} more broken symlinks",
            total_broken - 5
        ));
    }
}

fn collect_broken_symlinks_recursive(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut broken = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return broken,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.is_symlink() {
                if std::fs::metadata(&path).is_err() {
                    broken.push(path);
                }
            } else if meta.is_dir() {
                broken.extend(collect_broken_symlinks_recursive(&path));
            }
        }
    }
    broken
}

async fn check_state_consistency(d: &mut DiagResult) {
    let state = match InstallState::new() {
        Ok(s) => s,
        Err(_) => return,
    };

    let mut packages = match state.load().await {
        Ok(p) => p,
        Err(_) => return,
    };

    let mut missing_names: Vec<String> = Vec::new();
    let mut orphaned_names: Vec<(String, InstallMode)> = Vec::new();

    for (name, pkg) in &packages {
        if let Ok(cellar) = pkg.install_mode.cellar_path() {
            let pkg_dir = cellar.join(name);
            if !pkg_dir.exists() {
                missing_names.push(name.clone());
            }
        }
    }

    for mode in &[InstallMode::Global, InstallMode::User] {
        if let Ok(cellar) = mode.cellar_path() {
            if !cellar.exists() {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(&cellar) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !packages.contains_key(&name) {
                        orphaned_names.push((name, *mode));
                    }
                }
            }
        }
    }

    if !missing_names.is_empty() {
        if d.fix {
            for name in &missing_names {
                packages.remove(name);
                d.fixed(&format!(
                    "removed stale tracking entry: {}",
                    style(name).magenta()
                ));
            }
            if let Err(e) = state.save(&packages).await {
                d.fail(&format!("cannot save state: {}", e));
            }
        } else {
            for (i, name) in missing_names.iter().enumerate() {
                if i < 3 {
                    d.warn(&format!(
                        "tracked but missing from cellar: {}",
                        style(name).magenta()
                    ));
                }
            }
            if missing_names.len() > 3 {
                d.warn(&format!(
                    "... and {} more missing packages",
                    missing_names.len() - 3
                ));
            }
        }
    }

    if !orphaned_names.is_empty() {
        if d.fix {
            d.warn("syncing untracked cellar packages into state...");
            match state.sync_from_cellar().await {
                Ok(_) => d.fixed(&format!(
                    "registered {} untracked packages",
                    orphaned_names.len()
                )),
                Err(e) => d.fail(&format!("cellar sync failed: {}", e)),
            }
        } else {
            for (i, (name, _)) in orphaned_names.iter().enumerate() {
                if i < 3 {
                    d.warn(&format!(
                        "in cellar but untracked: {}",
                        style(name).magenta()
                    ));
                }
            }
            if orphaned_names.len() > 3 {
                d.warn(&format!(
                    "... and {} more untracked packages",
                    orphaned_names.len() - 3
                ));
            }
        }
    }

    if missing_names.is_empty() && orphaned_names.is_empty() {
        d.pass("install state consistent with cellar");
    }
}

fn check_tools(d: &mut DiagResult) {
    let tools: &[(&str, &[&str], &str)] = &[
        ("curl", &["--version"], "required for downloads"),
        ("git", &["--version"], "required for taps"),
    ];

    for (tool, args, purpose) in tools {
        if run_command_with_timeout(tool, args, 2).is_some() {
            d.pass(&format!("{} installed ({})", tool, purpose));
        } else {
            d.warn(&format!("{} not found ({})", tool, purpose));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if run_command_with_timeout("xcode-select", &["-p"], 2).is_some() {
            d.pass("xcode command line tools installed");
        } else {
            d.warn("xcode command line tools not installed — run `xcode-select --install`");
        }
    }

    if run_command_with_timeout("brew", &["--version"], 2).is_some() {
        d.pass("homebrew installed");
    } else {
        d.warn("homebrew not found (wax works standalone, but some features benefit from it)");
    }
}

fn is_writable(path: &Path) -> bool {
    let test_file = path.join(".wax_doctor_test");
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
