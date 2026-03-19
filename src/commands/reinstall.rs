use crate::cache::Cache;
use crate::commands::{install, uninstall};
use crate::error::{Result, WaxError};
use crate::install::{InstallMode, InstallState};
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

    if total > 1 {
        println!(
            "reinstalling {} packages\n",
            style(total).bold()
        );
    }

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

        // Spinner for uninstall phase
        let spinner = multi.add(ProgressBar::new_spinner());
        spinner.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        spinner.enable_steady_tick(std::time::Duration::from_millis(80));
        spinner.set_message(format!(
            "{}removing {}...",
            prefix,
            style(name).magenta()
        ));

        if installed.contains_key(name.as_str()) {
            uninstall::uninstall_quiet(cache, name, cask).await?;
        }
        spinner.finish_and_clear();

        // Progress bar for download/install phase
        let pb = multi.add(ProgressBar::new(0));
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

    println!(
        "\n{} {} reinstalled [{}ms]",
        style(total).bold(),
        if total == 1 { "package" } else { "packages" },
        start.elapsed().as_millis()
    );

    Ok(())
}
