use crate::error::validate_package_name;
use crate::error::{Result, WaxError};
use crate::install::InstallState;
use console::style;

pub async fn pin(packages: &[String]) -> Result<()> {
    if packages.is_empty() {
        return Err(WaxError::InvalidInput("No packages specified".to_string()));
    }

    let state = InstallState::new()?;
    state.sync_from_cellar().await.ok();
    let installed = state.load().await?;

    for name in packages {
        validate_package_name(name)?;
        if !installed.contains_key(name.as_str()) {
            eprintln!(
                "{}: {} is not installed",
                style("warning").yellow(),
                style(name).magenta()
            );
            continue;
        }
        state.set_pinned(name, true).await?;
        let version = installed
            .get(name.as_str())
            .map(|p| p.version.as_str())
            .unwrap_or("?");
        println!(
            "{} {}@{} pinned",
            style("✓").green(),
            style(name).magenta(),
            style(version).dim()
        );
    }

    Ok(())
}

pub async fn unpin(packages: &[String]) -> Result<()> {
    if packages.is_empty() {
        return Err(WaxError::InvalidInput("No packages specified".to_string()));
    }

    let state = InstallState::new()?;
    state.sync_from_cellar().await.ok();
    let installed = state.load().await?;

    for name in packages {
        validate_package_name(name)?;
        if !installed.contains_key(name.as_str()) {
            eprintln!(
                "{}: {} is not installed",
                style("warning").yellow(),
                style(name).magenta()
            );
            continue;
        }
        state.set_pinned(name, false).await?;
        println!("{} {} unpinned", style("✓").green(), style(name).magenta());
    }

    Ok(())
}
