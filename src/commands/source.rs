use crate::cache::Cache;
use crate::error::{Result, WaxError};
use console::style;
use std::collections::HashMap;
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn source(cache: &Cache, formula_name: &str) -> Result<()> {
    cache.ensure_fresh().await?;

    let formulae = cache.load_all_formulae().await?;
    let casks = cache.load_casks().await?;
    let formula_index: HashMap<_, _> = formulae
        .iter()
        .map(|f| (f.name.as_str(), f))
        .chain(formulae.iter().map(|f| (f.full_name.as_str(), f)))
        .collect();
    let cask_index: HashMap<_, _> = casks
        .iter()
        .map(|c| (c.token.as_str(), c))
        .chain(casks.iter().map(|c| (c.full_token.as_str(), c)))
        .collect();

    if let Some(formula) = formula_index.get(formula_name) {
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

    if let Some(cask) = cask_index.get(formula_name) {
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
