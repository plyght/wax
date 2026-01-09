use crate::api::ApiClient;
use crate::cache::Cache;
use crate::cask::CaskState;
use crate::commands::update;
use crate::error::{Result, WaxError};
use console::style;
use tracing::instrument;

#[instrument(skip(api_client, cache))]
pub async fn info(api_client: &ApiClient, cache: &Cache, name: &str, cask: bool) -> Result<()> {
    if !cache.is_initialized() {
        println!("Initializing package index (first time only)...");
        update::update(api_client, cache).await?;
    }

    if cask {
        return info_cask(api_client, cache, name).await;
    }

    let formulae = cache.load_all_formulae().await?;
    let formula_exists = formulae
        .iter()
        .any(|f| f.name == name || f.full_name == name);

    if !formula_exists {
        let casks = cache.load_casks().await?;
        let cask_exists = casks
            .iter()
            .any(|c| c.token == name || c.full_token == name);

        if cask_exists {
            return info_cask(api_client, cache, name).await;
        }

        return Err(WaxError::FormulaNotFound(format!(
            "{} (not found as formula or cask)",
            name
        )));
    }

    let formula = formulae
        .iter()
        .find(|f| f.name == name || f.full_name == name)
        .unwrap();

    println!(
        "{}: {}",
        style(&formula.name).bold().green(),
        formula.versions.stable
    );
    println!("{}", formula.desc.as_deref().unwrap_or("No description"));
    println!("{} {}", style("Homepage:").bold(), formula.homepage);

    if let Some(deps) = &formula.dependencies {
        if !deps.is_empty() {
            println!("{} {}", style("Dependencies:").bold(), deps.join(", "));
        }
    }

    if let Some(build_deps) = &formula.build_dependencies {
        if !build_deps.is_empty() {
            println!(
                "{} {}",
                style("Build dependencies:").bold(),
                build_deps.join(", ")
            );
        }
    }

    println!(
        "{} {}",
        style("Bottle:").bold(),
        if formula.versions.bottle { "Yes" } else { "No" }
    );

    if let Some(installed) = &formula.installed {
        if !installed.is_empty() {
            let versions: Vec<_> = installed.iter().map(|i| i.version.as_str()).collect();
            println!(
                "{} {}",
                style("Installed:").bold().cyan(),
                versions.join(", ")
            );
        }
    }

    Ok(())
}

#[instrument(skip(api_client, cache))]
async fn info_cask(api_client: &ApiClient, cache: &Cache, name: &str) -> Result<()> {
    if !cache.is_initialized() {
        println!("Initializing package index (first time only)...");
        update::update(api_client, cache).await?;
    }

    let casks = cache.load_casks().await?;

    let _cask_summary = casks
        .iter()
        .find(|c| c.token == name || c.full_token == name)
        .ok_or_else(|| WaxError::CaskNotFound(name.to_string()))?;

    let cask = api_client.fetch_cask_details(name).await?;

    let display_name = cask.name.first().unwrap_or(&cask.token);

    println!(
        "{}: {} {}",
        style(display_name).bold().green(),
        cask.version,
        style("(cask)").dim()
    );
    println!("{}", cask.desc.as_deref().unwrap_or("No description"));
    println!("{} {}", style("Homepage:").bold(), cask.homepage);
    println!("{} {}", style("URL:").bold(), cask.url);

    if let Some(artifacts) = &cask.artifacts {
        let artifact_types: Vec<String> = artifacts
            .iter()
            .map(|a| match a {
                crate::api::CaskArtifact::App { .. } => "app".to_string(),
                crate::api::CaskArtifact::Pkg { .. } => "pkg".to_string(),
                crate::api::CaskArtifact::Binary { .. } => "binary".to_string(),
                crate::api::CaskArtifact::Uninstall { .. } => "uninstall".to_string(),
                crate::api::CaskArtifact::Preflight { .. } => "preflight".to_string(),
                crate::api::CaskArtifact::Other(_) => "other".to_string(),
            })
            .collect();

        if !artifact_types.is_empty() {
            println!(
                "{} {}",
                style("Artifacts:").bold(),
                artifact_types.join(", ")
            );
        }
    }

    let state = CaskState::new()?;
    let installed_casks = state.load().await?;

    if let Some(installed) = installed_casks.get(name) {
        println!(
            "{} {}",
            style("Installed:").bold().cyan(),
            installed.version
        );
    }

    Ok(())
}
