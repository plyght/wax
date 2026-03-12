use crate::cache::Cache;
use crate::error::{Result, WaxError};
use console::style;
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn source(cache: &Cache, formula_name: &str) -> Result<()> {
    cache.ensure_fresh().await?;

    let formulae = cache.load_all_formulae().await?;
    let casks = cache.load_casks().await?;

    if let Some(formula) = formulae
        .iter()
        .find(|f| f.name == formula_name || f.full_name == formula_name)
    {
        let homepage = &formula.homepage;
        println!(
            "{} → {}",
            style(formula_name).magenta(),
            style(homepage).cyan().underlined()
        );

        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(homepage).spawn();
        }

        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(homepage).spawn();
        }

        return Ok(());
    }

    if let Some(cask) = casks
        .iter()
        .find(|c| c.token == formula_name || c.full_token == formula_name)
    {
        let homepage = &cask.homepage;
        println!(
            "{} {} → {}",
            style(formula_name).magenta(),
            style("(cask)").yellow(),
            style(homepage).cyan().underlined()
        );

        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(homepage).spawn();
        }

        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(homepage).spawn();
        }

        return Ok(());
    }

    Err(WaxError::FormulaNotFound(formula_name.to_string()))
}
