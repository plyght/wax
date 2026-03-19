use crate::cache::Cache;
use crate::commands::{install, uninstall};
use crate::error::{Result, WaxError};
use crate::install::{InstallMode, InstallState};
use crate::signal::{clear_active_multi, clear_current_op, set_active_multi, set_current_op};
use crate::ui::{PROGRESS_BAR_CHARS, PROGRESS_BAR_TEMPLATE};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::time::Instant;

pub async fn reinstall(cache: &Cache, packages: &[String], cask: bool, all: bool) -> Result<()> {
    let state = InstallState::new()?;
    state.sync_from_cellar().await.ok();
    let installed = state.load().await?;

    let resolved: Vec<String> = if all {
        let mut names: Vec<String> = installed.keys().cloned().collect();
        names.sort();
        names
    } else {
        if packages.is_empty() {
            return Err(WaxError::InvalidInput(
                "Specify package name(s) or use --all to reinstall everything".to_string(),
            ));
        }
        packages.to_vec()
    };

    let total = resolved.len();
    let start = Instant::now();
    let multi = MultiProgress::new();
    set_active_multi(multi.clone());

    // Overall progress bar for multi-package reinstalls, anchored to the bottom
    let overall_pb = if total > 1 {
        println!(
            "reinstalling {} packages\n",
            style(total).bold()
        );
        let pb = multi.insert_from_back(0, ProgressBar::new(total as u64));
        pb.set_style(
            ProgressStyle::default_bar()
                .template("  overall  {bar:40.white/dim} {pos}/{len}  eta {eta}")
                .unwrap()
                .progress_chars("█▓▒░ "),
        );
        Some(pb)
    } else {
        None
    };

    for (i, name) in resolved.iter().enumerate() {
        let install_mode = installed.get(name.as_str()).map(|p| p.install_mode);
        let (user_flag, global_flag) = match install_mode {
            Some(InstallMode::User) => (true, false),
            Some(InstallMode::Global) => (false, true),
            None => (false, false),
        };

        let prefix = if total > 1 {
            format!("[{}/{}] ", i + 1, total)
        } else {
            String::new()
        };

        // Spinner for uninstall phase (inserted above the overall bar)
        let spinner = multi.insert_from_back(1, ProgressBar::new_spinner());
        spinner.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        spinner.enable_steady_tick(std::time::Duration::from_millis(80));
        set_current_op(format!("removing {}", name));
        spinner.set_message(format!(
            "{}removing {}...",
            prefix,
            style(name).magenta()
        ));

        if installed.contains_key(name.as_str()) {
            uninstall::uninstall_quiet(cache, name, cask).await?;
        }
        spinner.finish_and_clear();

        // Progress bar for download/install phase (inserted above the overall bar)
        let pb = multi.insert_from_back(1, ProgressBar::new(0));
        pb.set_style(
            ProgressStyle::default_bar()
                .template(&format!(
                    "{}{}",
                    prefix,
                    PROGRESS_BAR_TEMPLATE
                ))
                .unwrap()
                .progress_chars(PROGRESS_BAR_CHARS),
        );
        pb.set_message(style(name).magenta().to_string());

        let pkg_start = Instant::now();
        set_current_op(format!("downloading {}", name));
        install::install_quiet_with_progress(
            cache,
            std::slice::from_ref(name),
            cask,
            user_flag,
            global_flag,
            &pb,
        )
        .await?;
        pb.finish_and_clear();
        if let Some(ref opb) = overall_pb {
            opb.inc(1);
        }

        println!(
            "{} {}{}@{}  {}",
            style("✓").green().bold(),
            prefix,
            style(name).magenta(),
            style(
                installed
                    .get(name.as_str())
                    .map(|p| p.version.as_str())
                    .unwrap_or("latest")
            )
            .dim(),
            style(format!("[{}ms]", pkg_start.elapsed().as_millis())).dim(),
        );
    }

    if let Some(pb) = overall_pb {
        pb.finish_and_clear();
    }
    clear_current_op();
    clear_active_multi();

    println!(
        "\n{} {} reinstalled [{}ms]",
        style(total).bold(),
        if total == 1 { "package" } else { "packages" },
        start.elapsed().as_millis()
    );

    Ok(())
}
