use crate::api::ApiClient;
#[cfg(target_os = "macos")]
use crate::bottle::BottleDownloader;
use crate::bottle::{detect_platform, homebrew_prefix, run_command_with_timeout};
use crate::cache::Cache;
use crate::cask::CaskState;
use crate::error::Result;
use crate::install::{create_symlinks, InstallMode, InstallState};
use console::style;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
#[cfg(target_os = "macos")]
use rayon::prelude::*;
use std::path::Path;
use std::time::{Duration, Instant};

struct DiagResult {
    passed: usize,
    warned: usize,
    failed: usize,
    fixed: usize,
    fix: bool,
    messages: Vec<String>,
}

impl DiagResult {
    fn new(fix: bool) -> Self {
        Self {
            passed: 0,
            warned: 0,
            failed: 0,
            fixed: 0,
            fix,
            messages: Vec::new(),
        }
    }

    fn pass(&mut self, msg: &str) {
        self.passed += 1;
        self.messages
            .push(format!("  {} {}", style("✓").green(), msg));
    }

    fn warn(&mut self, msg: &str) {
        self.warned += 1;
        self.messages
            .push(format!("  {} {}", style("!").yellow(), msg));
    }

    fn fail(&mut self, msg: &str) {
        self.failed += 1;
        self.messages
            .push(format!("  {} {}", style("✗").red(), msg));
    }

    fn fixed(&mut self, msg: &str) {
        self.fixed += 1;
        self.messages
            .push(format!("  {} {}", style("⚡").cyan(), msg));
    }

    fn add(&mut self, other: DiagResult) {
        self.passed += other.passed;
        self.warned += other.warned;
        self.failed += other.failed;
        self.fixed += other.fixed;
        self.messages.extend(other.messages);
    }
}

/// One diagnostic check: a display title and an async function that produces
/// the result.
struct Check {
    title: &'static str,
    run: BoxFuture<'static, DiagResult>,
}

fn summary_status(d: &DiagResult) -> (&'static str, console::Style) {
    if d.failed > 0 {
        ("fail", console::Style::new().red().bold())
    } else if d.warned > 0 {
        ("warn", console::Style::new().yellow().bold())
    } else {
        (" ok ", console::Style::new().green().bold())
    }
}

fn print_check_result(title: &str, result: &DiagResult, elapsed: Duration) {
    let (label, st) = summary_status(result);
    println!(
        "[{}] {:<22} {}",
        st.apply_to(label),
        style(title).bold(),
        style(format!("({})", format_elapsed(elapsed))).dim()
    );
    for msg in &result.messages {
        println!("  {}", msg);
    }
}

pub async fn doctor(cache: &Cache, fix: bool) -> Result<()> {
    let mut aggregate = DiagResult::new(fix);
    let cache = cache.clone();
    let start = Instant::now();

    if fix {
        println!("{}", style("running wax doctor --fix").bold());
    } else {
        println!("{}", style("running wax doctor").bold());
    }

    let cache_for_check = cache.clone();
    let checks: Vec<Check> = vec![
        Check {
            title: "platform",
            run: Box::pin(async move {
                tokio::task::spawn_blocking(move || check_platform(fix))
                    .await
                    .unwrap()
            }),
        },
        Check {
            title: "prefix",
            run: Box::pin(async move {
                tokio::task::spawn_blocking(move || check_prefix(fix))
                    .await
                    .unwrap()
            }),
        },
        Check {
            title: "cellar",
            run: Box::pin(async move { check_cellar(fix).await }),
        },
        Check {
            title: "symlink dirs",
            run: Box::pin(async move { check_symlink_dirs(fix).await }),
        },
        Check {
            title: "cache",
            run: Box::pin(async move { check_cache(&cache_for_check, fix).await }),
        },
        Check {
            title: "install state",
            run: Box::pin(async move { check_install_state(fix).await }),
        },
        Check {
            title: "cask state",
            run: Box::pin(async move { check_cask_state(fix).await }),
        },
        Check {
            title: "state consistency",
            run: Box::pin(async move { check_state_consistency(fix).await }),
        },
        Check {
            title: "broken symlinks",
            run: Box::pin(async move { check_broken_symlinks(fix).await }),
        },
        Check {
            title: "opt symlinks",
            run: Box::pin(async move { check_opt_symlinks(fix).await }),
        },
        Check {
            title: "unrelocated bottles",
            run: Box::pin(async move { check_unrelocated_bottles(fix).await }),
        },
        Check {
            title: "code signatures",
            run: Box::pin(async move { check_invalid_signatures(fix).await }),
        },
        Check {
            title: "tools",
            run: Box::pin(async move {
                tokio::task::spawn_blocking(move || check_tools(fix))
                    .await
                    .unwrap()
            }),
        },
        Check {
            title: "glibc",
            run: Box::pin(async move {
                tokio::task::spawn_blocking(move || check_glibc_version(fix))
                    .await
                    .unwrap()
            }),
        },
        Check {
            title: "gpu",
            run: Box::pin(async move {
                tokio::task::spawn_blocking(move || check_metal_toolchain(fix))
                    .await
                    .unwrap()
            }),
        },
    ];

    // One spinner per check, displayed in declaration order while all checks
    // run in parallel.
    let mp = MultiProgress::new();
    let spinner_style = ProgressStyle::default_spinner()
        .template("{spinner:.cyan} {msg}")
        .unwrap()
        .tick_chars(crate::ui::SPINNER_TICK_CHARS);

    let mut spinners: Vec<ProgressBar> = Vec::with_capacity(checks.len());
    for c in &checks {
        let pb = mp.add(ProgressBar::new_spinner());
        pb.set_style(spinner_style.clone());
        pb.set_message(format!("{} {}", style(c.title).bold(), style("…").dim()));
        pb.enable_steady_tick(Duration::from_millis(90));
        spinners.push(pb);
    }

    let mut fut: FuturesUnordered<BoxFuture<'static, (usize, DiagResult, Duration)>> =
        FuturesUnordered::new();
    let mut titles: Vec<&'static str> = Vec::with_capacity(checks.len());
    for (idx, c) in checks.into_iter().enumerate() {
        titles.push(c.title);
        let run = c.run;
        fut.push(Box::pin(async move {
            let t0 = Instant::now();
            let res = run.await;
            (idx, res, t0.elapsed())
        })
            as BoxFuture<'static, (usize, DiagResult, Duration)>);
    }

    let mut results: Vec<Option<(DiagResult, Duration)>> =
        (0..titles.len()).map(|_| None).collect();
    while let Some((idx, res, elapsed)) = fut.next().await {
        spinners[idx].finish_and_clear();
        results[idx] = Some((res, elapsed));
    }
    mp.clear().ok();

    for (idx, slot) in results.into_iter().enumerate() {
        let (res, elapsed) = slot.expect("every check must complete");
        print_check_result(titles[idx], &res, elapsed);
        aggregate.add(res);
    }

    println!();
    let mut parts = vec![format!("{} passed", style(aggregate.passed).green())];
    if aggregate.warned > 0 {
        parts.push(format!("{} warnings", style(aggregate.warned).yellow()));
    }
    if aggregate.failed > 0 {
        parts.push(format!("{} errors", style(aggregate.failed).red()));
    }
    if aggregate.fixed > 0 {
        parts.push(format!("{} fixed", style(aggregate.fixed).cyan()));
    }
    println!(
        "{}: {} {}",
        style("result").bold(),
        parts.join(", "),
        style(format!("({:.2}s)", start.elapsed().as_secs_f32())).dim()
    );

    if !fix && (aggregate.warned > 0 || aggregate.failed > 0) {
        println!(
            "{} run {} to auto-fix issues",
            style("hint:").dim(),
            style("wax doctor --fix").yellow()
        );
    }

    Ok(())
}

fn format_elapsed(elapsed: Duration) -> String {
    if elapsed.as_millis() < 10 {
        "<10ms".to_string()
    } else if elapsed.as_millis() < 1000 {
        format!("{}ms", elapsed.as_millis())
    } else {
        format!("{:.1}s", elapsed.as_secs_f32())
    }
}

fn check_platform(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
    let platform = detect_platform();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    if platform == "unknown" {
        d.fail(&format!("unsupported platform: {}-{}", os, arch));
    } else {
        d.pass(&format!("platform: {} ({}-{})", platform, os, arch));
    }
    d
}

fn check_prefix(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
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
        return d;
    }

    if is_writable(&prefix) {
        d.pass(&format!("prefix writable: {}", prefix.display()));
    } else {
        d.warn(&format!(
            "prefix not writable: {} (use --user or sudo)",
            prefix.display()
        ));
    }
    d
}

async fn check_cellar(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
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
    d
}

async fn check_symlink_dirs(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
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
    d
}

async fn check_cache(cache: &Cache, fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
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
                    d.warn(&format!(
                        "cache is {} days old — refreshing...",
                        age_hours / 24
                    ));
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
    d
}

async fn check_install_state(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
    match InstallState::new() {
        Ok(state) => match state.load().await {
            Ok(packages) => {
                d.pass(&format!(
                    "install state: {} packages tracked",
                    packages.len()
                ));
            }
            Err(e) => {
                if d.fix {
                    d.warn(&format!("install state corrupt: {}", e));
                    match state.save(&std::collections::HashMap::new()).await {
                        Ok(_) => match state.sync_from_cellar().await {
                            Ok(_) => d.fixed("install state rebuilt from cellar"),
                            Err(_) => d.fixed("install state reset to empty"),
                        },
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
    d
}

async fn check_cask_state(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
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
    d
}

async fn check_broken_symlinks(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
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
                        d.fail(&format!("cannot remove {}: {}", rel.display(), e));
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
    d
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

async fn check_opt_symlinks(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
    let mut missing_opt = Vec::new();
    let mut relinked = 0usize;

    for mode in &[InstallMode::Global, InstallMode::User] {
        let cellar = match mode.cellar_path() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !cellar.exists() {
            continue;
        }

        let prefix = match mode.prefix() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let opt_dir = prefix.join("opt");

        let entries = match std::fs::read_dir(&cellar) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();
            let opt_link = opt_dir.join(&name);

            // Check if opt symlink exists and is valid
            let needs_fix = if let Ok(meta) = std::fs::symlink_metadata(&opt_link) {
                if meta.is_symlink() {
                    // Symlink exists - check if target is valid
                    std::fs::metadata(&opt_link).is_err()
                } else {
                    false
                }
            } else {
                // opt symlink doesn't exist at all
                true
            };

            if needs_fix {
                missing_opt.push((name, entry.path(), *mode));
            }
        }
    }

    if missing_opt.is_empty() {
        d.pass("all cellar packages have opt/ symlinks");
    } else if d.fix {
        d.warn(&format!(
            "{} packages missing opt/ symlinks — relinking...",
            missing_opt.len()
        ));
        for (name, pkg_dir, mode) in &missing_opt {
            // Find the latest version directory
            let versions: Vec<String> = match std::fs::read_dir(pkg_dir) {
                Ok(entries) => entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect(),
                Err(_) => continue,
            };
            if versions.is_empty() {
                continue;
            }
            let mut sorted = versions;
            crate::version::sort_versions(&mut sorted);
            let version = sorted.last().unwrap().clone();

            let cellar = match mode.cellar_path() {
                Ok(c) => c,
                Err(_) => continue,
            };

            match create_symlinks(name, &version, &cellar, false, *mode).await {
                Ok(_) => {
                    relinked += 1;
                    if relinked <= 10 {
                        d.fixed(&format!("relinked {}@{}", name, version));
                    }
                }
                Err(e) => {
                    d.fail(&format!("failed to relink {}: {}", name, e));
                }
            }
        }
        if relinked > 10 {
            d.fixed(&format!("... and {} more packages relinked", relinked - 10));
        }
    } else {
        for (i, (name, _, _)) in missing_opt.iter().enumerate() {
            if i < 5 {
                d.fail(&format!("missing opt/ symlink: {}", style(name).magenta()));
            }
        }
        if missing_opt.len() > 5 {
            d.fail(&format!(
                "... and {} more missing opt/ symlinks",
                missing_opt.len() - 5
            ));
        }
    }
    d
}

async fn check_state_consistency(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
    let state = match InstallState::new() {
        Ok(s) => s,
        Err(_) => return d,
    };

    let mut packages = match state.load().await {
        Ok(p) => p,
        Err(_) => return d,
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
    d
}

fn check_glibc_version(fix: bool) -> DiagResult {
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut d = DiagResult::new(fix);
    #[cfg(target_os = "linux")]
    {
        if let Some(output) = run_command_with_timeout("ldd", &["--version"], 2) {
            let first_line = output.lines().next().unwrap_or("");
            if let Some(ver_str) = first_line.split_whitespace().last() {
                let parts: Vec<u32> = ver_str.split('.').filter_map(|p| p.parse().ok()).collect();
                if parts.len() >= 2 {
                    let (major, minor) = (parts[0], parts[1]);
                    if major == 2 && minor < 39 {
                        d.warn(&format!(
                            "glibc {}.{} detected — Homebrew 5.2.0 will require glibc 2.39+. \
                              Consider upgrading to Ubuntu 24.04 or equivalent.",
                            major, minor
                        ));
                    } else {
                        d.pass(&format!("glibc version: {}", ver_str));
                    }
                }
            }
        }
    }
    d
}

fn check_metal_toolchain(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
    #[cfg(target_os = "macos")]
    {
        if let Some(output) =
            run_command_with_timeout("system_profiler", &["SPDisplaysDataType"], 5)
        {
            let has_metal = output.contains("Metal Support") || output.contains("Metal Family");
            if has_metal {
                let metal_version = output
                    .lines()
                    .find(|l| l.contains("Metal Support") || l.contains("Metal Family"))
                    .map(|l| l.trim())
                    .unwrap_or("detected");
                d.pass(&format!("Metal: {}", metal_version));
            } else {
                d.warn("Metal GPU support not detected");
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let mut found_gpu = false;

        if let Some(output) = run_command_with_timeout("vulkaninfo", &["--summary"], 3) {
            if output.contains("apiVersion") || output.contains("Vulkan Instance") {
                let version = output
                    .lines()
                    .find(|l| l.contains("apiVersion"))
                    .map(|l| l.trim())
                    .unwrap_or("detected");
                d.pass(&format!("Vulkan: {}", version));
                found_gpu = true;
            }
        }

        if !found_gpu {
            if let Some(output) = run_command_with_timeout("glxinfo", &["-B"], 3) {
                if output.contains("OpenGL version") {
                    let version = output
                        .lines()
                        .find(|l| l.contains("OpenGL version"))
                        .map(|l| l.trim())
                        .unwrap_or("detected");
                    d.pass(&format!("GPU: {}", version));
                    found_gpu = true;
                }
            }
        }

        if !found_gpu {
            d.warn("no GPU toolchain detected (vulkaninfo/glxinfo not found)");
        }
    }
    d
}

async fn check_unrelocated_bottles(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
    #[cfg(target_os = "macos")]
    {
        let prefix = homebrew_prefix();
        let cellar = prefix.join("Cellar");
        if !cellar.exists() {
            d.pass("all bottles properly relocated (no @@HOMEBREW_*@@ placeholders)");
            return d;
        }

        let prefix_str = prefix.to_string_lossy().to_string();
        let entries = match std::fs::read_dir(&cellar) {
            Ok(e) => e,
            Err(_) => {
                d.pass("all bottles properly relocated (no @@HOMEBREW_*@@ placeholders)");
                return d;
            }
        };

        let unrelocated: Vec<(String, String, std::path::PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter_map(|pkg_entry| {
                let pkg_dir = pkg_entry.path();
                if !pkg_dir.is_dir() {
                    return None;
                }
                let name = pkg_entry.file_name().to_string_lossy().to_string();

                let mut versions: Vec<String> = match std::fs::read_dir(&pkg_dir) {
                    Ok(e) => e
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().is_dir())
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect(),
                    Err(_) => return None,
                };
                crate::version::sort_versions(&mut versions);

                let version = versions.last()?.clone();
                let ver_dir = pkg_dir.join(&version);
                Some((name, version, ver_dir))
            })
            .collect();

        let unrelocated: Vec<_> = unrelocated
            .into_par_iter()
            .filter(|(_, _, ver_dir)| has_unrelocated_bottle(ver_dir))
            .collect();

        if unrelocated.is_empty() {
            d.pass("all bottles properly relocated (no @@HOMEBREW_*@@ placeholders)");
            return d;
        }

        if d.fix {
            d.warn(&format!(
                "{} packages have unrelocated dylib paths — fixing with install_name_tool...",
                unrelocated.len()
            ));
            for (name, version, ver_dir) in &unrelocated {
                match BottleDownloader::relocate_bottle(ver_dir, &prefix_str) {
                    Ok(_) => d.fixed(&format!("relocated {}@{}", name, version)),
                    Err(e) => d.fail(&format!("failed to relocate {}@{}: {}", name, version, e)),
                }
            }
        } else {
            for (i, (name, version, _)) in unrelocated.iter().enumerate() {
                if i < 5 {
                    d.fail(&format!(
                        "unrelocated bottle: {}@{} (causes dyld Symbol not found errors)",
                        style(name).magenta(),
                        version
                    ));
                }
            }
            if unrelocated.len() > 5 {
                d.fail(&format!(
                    "... and {} more unrelocated bottles",
                    unrelocated.len() - 5
                ));
            }
            d.warn(&format!(
                "run {} to fix, or {} to reinstall affected packages",
                style("wax doctor --fix").yellow(),
                style("wax reinstall <name>").yellow()
            ));
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        d.pass("all bottles properly relocated (no @@HOMEBREW_*@@ placeholders)");
    }
    d
}

#[cfg(target_os = "macos")]
fn has_unrelocated_bottle(dir: &Path) -> bool {
    scan_dir_for_placeholders(dir)
}

#[cfg(target_os = "macos")]
fn scan_dir_for_placeholders(dir: &Path) -> bool {
    let entries: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(e) => e.filter_map(|e| e.ok().map(|entry| entry.path())).collect(),
        Err(_) => return false,
    };

    entries.par_iter().any(|path| {
        if path.is_dir() {
            scan_dir_for_placeholders(path)
        } else if path.is_file() {
            has_placeholders(path)
        } else {
            false
        }
    })
}

#[cfg(target_os = "macos")]
fn has_placeholders(path: &Path) -> bool {
    use std::io::Read;

    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };

    // First, check if it's a Mach-O file
    let mut header = [0u8; 4];
    if f.read(&mut header).unwrap_or(0) < 4 {
        return false;
    }

    let is_mach_o = crate::bottle::is_mach_o(&header);

    // If Mach-O, scan deeper as before
    if is_mach_o {
        drop(f);
        return is_mach_o_with_placeholders(path);
    }

    // For text files, scan for @@HOMEBREW_ in the first 64KB
    const MAX_SCAN: usize = 64 * 1024;
    let placeholder = b"@@HOMEBREW_";
    let mut scan_len = MAX_SCAN;
    if let Ok(meta) = std::fs::metadata(path) {
        scan_len = std::cmp::min(scan_len, meta.len() as usize);
    }
    if scan_len == 0 {
        return false;
    }

    drop(f);
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = vec![0u8; scan_len];
    let n = f.read(&mut buf).unwrap_or(0);
    buf[..n]
        .windows(placeholder.len())
        .any(|w| w == placeholder)
}

#[cfg(target_os = "macos")]
fn is_mach_o_with_placeholders(path: &Path) -> bool {
    use std::io::Read;

    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };

    // Read header: 4 (magic) + 4 (cputype) + 4 (cpusubtype) + 4 (filetype)
    //              + 4 (ncmds) + 4 (sizeofcmds) + 4 (flags) [+ 4 reserved for 64-bit]
    let mut header = [0u8; 32];
    if f.read(&mut header).unwrap_or(0) < 8 {
        return false;
    }

    if !crate::bottle::is_mach_o(&header) {
        return false;
    }

    // Parse sizeofcmds (bytes 20-23, little-endian) for 64-bit Mach-O.
    // For fat binaries we fall back to a generous 64KB scan of the file start.
    let placeholder = b"@@HOMEBREW_";
    let magic = &header[0..4];
    let is_fat = magic == b"\xBE\xBA\xFE\xCA" || magic == b"\xCA\xFE\xBA\xBE";

    let mut scan_len = if is_fat {
        65536usize
    } else {
        let sizeofcmds =
            u32::from_le_bytes([header[20], header[21], header[22], header[23]]) as usize;
        32 + sizeofcmds
    };

    const MAX_SCAN: usize = 4 * 1024 * 1024;
    if let Ok(meta) = std::fs::metadata(path) {
        scan_len = std::cmp::min(scan_len, meta.len() as usize);
    }
    scan_len = std::cmp::min(scan_len, MAX_SCAN);
    if scan_len == 0 {
        return false;
    }

    // Re-read from the start, limiting to scan_len bytes
    drop(f);
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = vec![0u8; scan_len];
    let n = f.read(&mut buf).unwrap_or(0);
    buf[..n]
        .windows(placeholder.len())
        .any(|w| w == placeholder)
}

async fn check_invalid_signatures(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
    #[cfg(target_os = "macos")]
    {
        use std::io::Read;
        use std::process::Command;

        let prefix = homebrew_prefix();
        let cellar = prefix.join("Cellar");
        if !cellar.exists() {
            d.pass("all Mach-O binaries have valid code signatures");
            return d;
        }

        let entries = match std::fs::read_dir(&cellar) {
            Ok(e) => e,
            Err(_) => {
                d.pass("all Mach-O binaries have valid code signatures");
                return d;
            }
        };

        let packages: Vec<(String, std::path::PathBuf, String)> = entries
            .filter_map(|e| e.ok())
            .filter_map(|pkg_entry| {
                let pkg_dir = pkg_entry.path();
                if !pkg_dir.is_dir() {
                    return None;
                }
                let name = pkg_entry.file_name().to_string_lossy().to_string();

                let mut versions: Vec<String> = match std::fs::read_dir(&pkg_dir) {
                    Ok(e) => e
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().is_dir())
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect(),
                    Err(_) => return None,
                };
                crate::version::sort_versions(&mut versions);
                let version = versions.last()?.clone();
                Some((name, pkg_dir, version))
            })
            .collect();

        let invalid: Vec<(String, String)> = packages
            .into_par_iter()
            .filter_map(|(name, pkg_dir, version)| {
                let ver_dir = pkg_dir.join(&version);
                let mut pkg_invalid = false;

                'outer: for subdir in &["bin", "lib"] {
                    let dir = ver_dir.join(subdir);
                    if !dir.is_dir() {
                        continue;
                    }
                    let Ok(dir_entries) = std::fs::read_dir(&dir) else {
                        continue;
                    };
                    for file in dir_entries.filter_map(|e| e.ok()).map(|entry| entry.path()) {
                        let Ok(meta) = std::fs::metadata(&file) else {
                            continue;
                        };
                        if !meta.is_file() {
                            continue;
                        }
                        let mut buf = [0u8; 4];
                        let Ok(mut f) = std::fs::File::open(&file) else {
                            continue;
                        };
                        if f.read(&mut buf).is_err() || !crate::bottle::is_mach_o(&buf) {
                            continue;
                        }
                        let Some(path_str) = file.to_str() else {
                            continue;
                        };
                        if let Ok(out) = Command::new("codesign").args(["-v", path_str]).output() {
                            let stderr = String::from_utf8_lossy(&out.stderr);
                            if stderr.contains("invalid signature")
                                || stderr.contains("code or signature have been modified")
                            {
                                pkg_invalid = true;
                                break 'outer;
                            }
                        }
                    }
                }

                pkg_invalid.then_some((name, version))
            })
            .collect();

        if invalid.is_empty() {
            d.pass("all Mach-O binaries have valid code signatures");
            return d;
        }

        if d.fix {
            d.warn(&format!(
                "{} packages have invalid code signatures — re-signing...",
                invalid.len()
            ));
            for (name, version) in &invalid {
                let ver_dir = cellar.join(name).join(version);
                let resigned = resign_macho_binaries(&ver_dir);
                if resigned > 0 {
                    d.fixed(&format!(
                        "re-signed {}@{} ({} binaries)",
                        name, version, resigned
                    ));
                } else {
                    d.fail(&format!("failed to re-sign {}@{}", name, version));
                }
            }
        } else {
            for (i, (name, version)) in invalid.iter().enumerate() {
                if i < 5 {
                    d.fail(&format!(
                        "invalid code signature: {}@{} (modified without re-signing — causes SIGKILL on Apple Silicon)",
                        style(name).magenta(),
                        version
                    ));
                }
            }
            if invalid.len() > 5 {
                d.fail(&format!(
                    "... and {} more with invalid signatures",
                    invalid.len() - 5
                ));
            }
            d.warn(&format!(
                "run {} to re-sign affected packages",
                style("wax doctor --fix").yellow()
            ));
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        d.pass("all Mach-O binaries have valid code signatures");
    }
    d
}

/// Re-sign all Mach-O binaries in bin/ and lib/ subdirs with an ad-hoc signature.
/// Returns the number of successfully re-signed files.
#[cfg(target_os = "macos")]
fn resign_macho_binaries(ver_dir: &Path) -> usize {
    use std::io::Read;
    use std::process::Command;

    let mut count = 0;
    for subdir in &["bin", "lib"] {
        let dir = ver_dir.join(subdir);
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let files: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok().map(|entry| entry.path()))
            .collect();
        count += files
            .into_par_iter()
            .filter(|file| {
                let Ok(meta) = std::fs::metadata(file) else {
                    return false;
                };
                if !meta.is_file() {
                    return false;
                }
                let mut buf = [0u8; 4];
                let Ok(mut f) = std::fs::File::open(file) else {
                    return false;
                };
                if f.read(&mut buf).is_err() || !crate::bottle::is_mach_o(&buf) {
                    return false;
                }
                let Some(path_str) = file.to_str() else {
                    return false;
                };
                Command::new("codesign")
                    .args(["--force", "--sign", "-", path_str])
                    .output()
                    .map(|out| out.status.success())
                    .unwrap_or(false)
            })
            .count();
    }
    count
}

fn check_tools(fix: bool) -> DiagResult {
    let mut d = DiagResult::new(fix);
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
    d
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
