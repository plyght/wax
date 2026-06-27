use crate::bottle::{detect_platform, homebrew_prefix};
use crate::cache::Cache;
use crate::error::Result;
use crate::install::{is_writable, InstallMode};
use crate::ui::dirs;
use console::style;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CACHE_STALE_SECS: i64 = 3600;

struct Summary {
    passed: usize,
    warned: usize,
    failed: usize,
    fixed: usize,
}

impl Summary {
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

fn path_in_path(path: &Path) -> bool {
    std::env::var("PATH")
        .ok()
        .is_some_and(|path_var| {
            let path_str = path.to_string_lossy();
            path_var.split(':').any(|p| p == path_str.as_ref())
        })
}

fn wax_bin_dirs() -> Vec<PathBuf> {
    let mut bins = vec![homebrew_prefix().join("bin")];
    if let Ok(home) = dirs::home_dir() {
        let user_bin = home.join(".local/wax/bin");
        if user_bin.exists() {
            bins.push(user_bin);
        }
    }
    bins
}

fn section(title: &str) {
    println!();
    println!("{}", style(title).bold());
}

fn check_platform(s: &mut Summary) {
    let platform = detect_platform();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    if platform == "unknown" {
        s.fail(&format!("unsupported platform: {os}-{arch}"));
    } else {
        s.pass(&format!("platform: {platform} ({os}-{arch})"));
    }
}

fn check_prefix(s: &mut Summary, fix: bool) {
    let prefix = homebrew_prefix();
    if prefix.exists() {
        s.pass(&format!("prefix exists: {}", prefix.display()));
    } else if fix {
        match std::fs::create_dir_all(&prefix) {
            Ok(()) => s.fixed(&format!("created prefix: {}", prefix.display())),
            Err(e) => {
                s.fail(&format!("cannot create prefix {}: {e}", prefix.display()));
                return;
            }
        }
    } else {
        s.fail(&format!("prefix missing: {}", prefix.display()));
        return;
    }

    if is_writable(&prefix) {
        s.pass(&format!("prefix writable: {}", prefix.display()));
    } else {
        s.warn(&format!(
            "prefix not writable: {} (use --user or sudo)",
            prefix.display()
        ));
    }
}

fn check_cellar(s: &mut Summary, fix: bool) {
    let Ok(cellar) = InstallMode::Global.cellar_path() else {
        return;
    };
    if cellar.exists() {
        let count = std::fs::read_dir(&cellar)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        e.path().is_dir() && !name.starts_with('.')
                    })
                    .count()
            })
            .unwrap_or(0);
        s.pass(&format!("cellar: {} ({count} packages)", cellar.display()));
    } else if fix {
        match std::fs::create_dir_all(&cellar) {
            Ok(()) => s.fixed(&format!("created cellar: {}", cellar.display())),
            Err(e) => s.warn(&format!("cannot create cellar: {e}")),
        }
    } else {
        s.warn(&format!("cellar missing: {}", cellar.display()));
    }
}

async fn refresh_cache(cache: &Cache, s: &mut Summary, ok_msg: &str) {
    match cache.ensure_fresh().await {
        Ok(()) => s.fixed(ok_msg),
        Err(e) => s.fail(&format!("cache refresh failed: {e}")),
    }
}

async fn check_cache(cache: &Cache, s: &mut Summary, fix: bool) {
    if !cache.is_initialized() {
        if fix {
            s.warn("cache not initialized — refreshing...");
            refresh_cache(cache, s, "cache initialized").await;
        } else {
            s.fail("cache not initialized — run `wax update`");
        }
        return;
    }

    match cache.load_metadata().await {
        Ok(Some(meta)) => {
            s.pass(&format!(
                "cache: {} formulae, {} casks",
                meta.formula_count, meta.cask_count
            ));
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let age_secs = now - meta.last_updated;
            let age_hours = age_secs / 3600;
            if age_secs > CACHE_STALE_SECS {
                if fix {
                    s.warn(&format!("cache is stale ({}h old) — refreshing...", age_hours));
                    refresh_cache(cache, s, "cache refreshed").await;
                } else {
                    s.warn(&format!("cache is stale ({}h old) — run `wax update`", age_hours));
                }
            } else {
                s.pass(&format!("cache age: {age_hours}h (fresh)"));
            }
        }
        Ok(None) => {
            if fix {
                s.warn("cache metadata missing — refreshing...");
                refresh_cache(cache, s, "cache refreshed").await;
            } else {
                s.fail("cache metadata missing — run `wax update`");
            }
        }
        Err(e) => s.fail(&format!("cache error: {e}")),
    }
}

async fn check_wax_update(s: &mut Summary) {
    match tokio::time::timeout(
        Duration::from_secs(30),
        crate::commands::self_update::available_stable_update(),
    )
    .await
    {
        Ok(Ok(Some(version))) => s.warn(&format!(
            "wax {} is available — run `wax update self`",
            style(format!("v{version}")).cyan()
        )),
        Ok(Ok(None)) => s.pass("wax is up to date"),
        Ok(Err(e)) => s.warn(&format!("could not check wax update: {e}")),
        Err(_) => s.warn("wax update check timed out"),
    }
}

fn check_path(s: &mut Summary) {
    let bins = wax_bin_dirs();
    let mut any_in_path = false;
    for bin_dir in &bins {
        if !bin_dir.exists() {
            continue;
        }
        if path_in_path(bin_dir) {
            s.pass(&format!("{} is in PATH", bin_dir.display()));
            any_in_path = true;
        } else {
            s.warn(&format!(
                "{} is not in PATH — add it to your shell profile",
                bin_dir.display()
            ));
        }
    }
    if !any_in_path && bins.iter().all(|b| !b.exists()) {
        s.warn("no wax bin directory found yet (install a package first)");
    }
}

fn check_writable_prefix(fix: bool) -> Result<()> {
    if !fix {
        return Ok(());
    }
    let prefix = homebrew_prefix();
    if prefix.exists() && !is_writable(&prefix) {
        return Err(crate::error::WaxError::InstallError(format!(
            "doctor --fix blocked\n  {} {} is not writable by {}\n\ntry:\n  run wax doctor --fix as the Homebrew-owning user\n  use wax install --user for per-user installs",
            style("→").cyan(),
            prefix.display(),
            std::env::var("USER").unwrap_or_else(|_| "this user".to_string())
        )));
    }
    Ok(())
}

fn print_summary(s: &Summary, start: Instant, fix: bool) {
    println!();
    let mut parts = vec![format!("{} passed", style(s.passed).green())];
    if s.warned > 0 {
        parts.push(format!("{} warnings", style(s.warned).yellow()));
    }
    if s.failed > 0 {
        parts.push(format!("{} errors", style(s.failed).red()));
    }
    if s.fixed > 0 {
        parts.push(format!("{} fixed", style(s.fixed).cyan()));
    }
    println!(
        "{}: {} {}",
        style("result").bold(),
        parts.join(", "),
        style(format!("({:.2}s)", start.elapsed().as_secs_f32())).dim()
    );
    if !fix && (s.warned > 0 || s.failed > 0) {
        println!(
            "{} run {} to auto-fix issues",
            style("hint:").dim(),
            style("wax doctor --fix").yellow()
        );
    }
}

pub async fn doctor(cache: &Cache, fix: bool, _full: bool) -> Result<()> {
    let start = Instant::now();
    check_writable_prefix(fix)?;

    println!(
        "{}",
        style(if fix {
            "running wax doctor --fix"
        } else {
            "running wax doctor"
        })
        .bold()
    );

    let mut s = Summary {
        passed: 0,
        warned: 0,
        failed: 0,
        fixed: 0,
    };

    section("platform");
    check_platform(&mut s);
    section("prefix");
    check_prefix(&mut s, fix);
    section("cellar");
    check_cellar(&mut s, fix);
    section("cache");
    check_cache(cache, &mut s, fix).await;
    section("wax update");
    check_wax_update(&mut s).await;
    section("path");
    check_path(&mut s);

    print_summary(&s, start, fix);
    Ok(())
}