use crate::error::Result;
use crate::install::InstallState;
use crate::version::sort_versions;
use console::style;

pub async fn cleanup(dry_run: bool) -> Result<()> {
    let state = InstallState::new()?;
    state.sync_from_cellar().await.ok();
    let installed = state.load().await?;

    let mut total_freed: u64 = 0;
    let mut removed_count = 0;

    for pkg in installed.values() {
        let cellar = pkg.install_mode.cellar_path()?;
        let pkg_dir = cellar.join(&pkg.name);

        if !pkg_dir.exists() {
            continue;
        }

        let mut versions: Vec<String> = match std::fs::read_dir(&pkg_dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect(),
            Err(_) => continue,
        };

        if versions.len() <= 1 {
            continue;
        }

        sort_versions(&mut versions);
        let old_versions = &versions[..versions.len() - 1];

        for old_ver in old_versions {
            let old_path = pkg_dir.join(old_ver);
            let size = dir_size(&old_path);

            if dry_run {
                println!(
                    "  would remove {}@{} ({})",
                    style(&pkg.name).magenta(),
                    style(old_ver).dim(),
                    format_bytes(size)
                );
            } else {
                if let Err(e) = std::fs::remove_dir_all(&old_path) {
                    eprintln!(
                        "  {} failed to remove {}@{}: {}",
                        style("✗").red(),
                        style(&pkg.name).magenta(),
                        old_ver,
                        e
                    );
                    continue;
                }
                println!(
                    "  {} {}@{} ({})",
                    style("removed").green(),
                    style(&pkg.name).magenta(),
                    style(old_ver).dim(),
                    format_bytes(size)
                );
            }
            total_freed += size;
            removed_count += 1;
        }
    }

    if removed_count == 0 {
        println!("nothing to clean up");
    } else if dry_run {
        println!(
            "\nwould free {} across {} old version{}",
            format_bytes(total_freed),
            removed_count,
            if removed_count == 1 { "" } else { "s" }
        );
        println!("run without --dry-run to remove");
    } else {
        println!(
            "\nfreed {} ({} old version{} removed)",
            format_bytes(total_freed),
            removed_count,
            if removed_count == 1 { "" } else { "s" }
        );
    }

    Ok(())
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = std::fs::metadata(&p) {
                total += meta.len();
            }
        }
    }
    total
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}
