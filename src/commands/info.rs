use crate::cache::Cache;
use crate::error::{Result, WaxError};
use console::style;
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn info(cache: &Cache, name: &str) -> Result<()> {
    let formulae = cache.load_formulae().await?;

    let formula = formulae
        .iter()
        .find(|f| f.name == name || f.full_name == name)
        .ok_or_else(|| WaxError::FormulaNotFound(name.to_string()))?;

    println!("{}: {}", style(&formula.name).bold().green(), formula.versions.stable);
    println!("{}", formula.desc.as_deref().unwrap_or("No description"));
    println!("{} {}", style("Homepage:").bold(), formula.homepage);
    
    if let Some(deps) = &formula.dependencies {
        if !deps.is_empty() {
            println!("{} {}", style("Dependencies:").bold(), deps.join(", "));
        }
    }

    if let Some(build_deps) = &formula.build_dependencies {
        if !build_deps.is_empty() {
            println!("{} {}", style("Build dependencies:").bold(), build_deps.join(", "));
        }
    }

    println!("{} {}", style("Bottle:").bold(), if formula.versions.bottle { "Yes" } else { "No" });

    if let Some(installed) = &formula.installed {
        if !installed.is_empty() {
            let versions: Vec<_> = installed.iter().map(|i| i.version.as_str()).collect();
            println!("{} {}", style("Installed:").bold().cyan(), versions.join(", "));
        }
    }

    Ok(())
}
