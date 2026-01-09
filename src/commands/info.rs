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
        println!("initializing package index (first time only)...");
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

    let installed_suffix = if let Some(installed) = &formula.installed {
        if !installed.is_empty() {
            let installed_versions: Vec<_> = installed.iter().map(|i| i.version.as_str()).collect();
            if installed_versions.len() == 1 {
                if installed_versions[0] == formula.versions.stable {
                    " · installed".to_string()
                } else {
                    format!(" · installed ({})", installed_versions[0])
                }
            } else {
                format!(" · installed ({})", installed_versions.join(", "))
            }
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    println!();
    println!(
        "{} · {}{}",
        style(&formula.name).magenta(),
        style(&formula.versions.stable).dim(),
        style(&installed_suffix).dim()
    );

    if let Some(desc) = &formula.desc {
        println!("{}", desc);
    }

    println!();
    println!("{}", &formula.homepage);

    if let Some(deps) = &formula.dependencies {
        if !deps.is_empty() {
            println!();
            println!("{}", style("dependencies:").dim());
            for dep in deps {
                println!("  {}", dep);
            }
        }
    }

    if let Some(build_deps) = &formula.build_dependencies {
        if !build_deps.is_empty() {
            println!();
            println!("{}", style("build dependencies:").dim());
            for dep in build_deps {
                println!("  {}", dep);
            }
        }
    }

    if !formula.versions.bottle {
        println!();
        println!("no precompiled bottle available (will build from source)");
    }

    Ok(())
}

#[instrument(skip(api_client, cache))]
async fn info_cask(api_client: &ApiClient, cache: &Cache, name: &str) -> Result<()> {
    if !cache.is_initialized() {
        println!("initializing package index (first time only)...");
        update::update(api_client, cache).await?;
    }

    let casks = cache.load_casks().await?;

    let _cask_summary = casks
        .iter()
        .find(|c| c.token == name || c.full_token == name)
        .ok_or_else(|| WaxError::CaskNotFound(name.to_string()))?;

    let cask = api_client.fetch_cask_details(name).await?;

    let display_name = cask.name.first().unwrap_or(&cask.token);

    let state = CaskState::new()?;
    let installed_casks = state.load().await?;
    let installed_version = installed_casks.get(name).map(|i| &i.version);

    let installed_suffix = if let Some(installed_ver) = installed_version {
        if installed_ver == &cask.version {
            " · installed".to_string()
        } else {
            format!(" · installed ({})", installed_ver)
        }
    } else {
        String::new()
    };

    println!();
    println!(
        "{} · {} {}{}",
        style(display_name).magenta(),
        style(&cask.version).dim(),
        style("(cask)").yellow(),
        style(installed_suffix).dim()
    );

    if let Some(desc) = &cask.desc {
        println!("{}", desc);
    }

    println!();
    println!("{}", &cask.homepage);

    println!();
    println!("{}", &cask.url);

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
            println!();
            println!("{}:", style("artifacts").dim());
            for artifact_type in artifact_types {
                println!("  {}", artifact_type);
            }
        }
    }

    Ok(())
}
