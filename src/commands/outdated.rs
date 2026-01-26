use crate::cache::Cache;
use crate::commands::upgrade::get_outdated_packages;
use crate::error::Result;
use console::style;
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn outdated(cache: &Cache) -> Result<()> {
    let start = std::time::Instant::now();

    let outdated = get_outdated_packages(cache).await?;

    if outdated.is_empty() {
        println!("all packages are up to date");
        let elapsed = start.elapsed();
        println!("\n[{}ms] done", elapsed.as_millis());
        return Ok(());
    }

    println!();
    for pkg in &outdated {
        let cask_indicator = if pkg.is_cask {
            format!(" {}", style("(cask)").yellow())
        } else {
            String::new()
        };
        println!(
            "{}{} {} → {}",
            style(&pkg.name).magenta(),
            cask_indicator,
            style(&pkg.installed_version).dim(),
            style(&pkg.latest_version).green()
        );
    }

    let elapsed = start.elapsed();
    println!(
        "\n{} package{} can be upgraded [{}ms]",
        style(outdated.len()).cyan(),
        if outdated.len() == 1 { "" } else { "s" },
        elapsed.as_millis()
    );

    Ok(())
}
