use crate::api::Formula;
use crate::cache::Cache;
use crate::cask::CaskState;
use crate::error::{Result, WaxError};
use crate::install::InstallState;

use console::style;
use std::collections::HashSet;
use tracing::instrument;

fn tap_slug_from_qualified_name(qualified: &str) -> Option<String> {
    let parts: Vec<&str> = qualified.split('/').collect();
    if parts.len() < 3 {
        return None;
    }
    if parts[0] == "homebrew" && matches!(parts[1], "core" | "cask" | "services") {
        return None;
    }
    Some(format!("{}/{}", parts[0], parts[1]))
}

#[instrument(skip(cache))]
pub async fn info(cache: &Cache, name: &str, cask: bool) -> Result<()> {
    cache.ensure_fresh().await?;

    if cask {
        return info_cask(cache, name).await;
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
            return info_cask(cache, name).await;
        }

        return Err(WaxError::FormulaNotFound(format!(
            "{} (not found as formula or cask)",
            name
        )));
    }

    let formula = formulae
        .iter()
        .find(|f| f.name == name || f.full_name == name)
        .ok_or_else(|| WaxError::FormulaNotFound(name.to_string()))?;

    info_formula(formula, name, &formulae).await
}

async fn info_formula(formula: &Formula, name: &str, formulae: &[Formula]) -> Result<()> {
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

    if let Some(ref tap) = tap_slug_from_qualified_name(&formula.full_name) {
        println!("{} {}", style("tap:").dim(), style(tap).cyan());
    }

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

    // Show "why installed" section if the package is installed locally
    let state = InstallState::new()?;
    let installed_packages = state.load().await?;
    let installed_pkg = installed_packages
        .get(name)
        .or_else(|| installed_packages.get(formula.full_name.as_str()))
        .or_else(|| installed_packages.get(&formula.name));
    if let Some(pkg) = installed_pkg {
        println!();
        let installed_names: HashSet<String> = installed_packages.keys().cloned().collect();
        let dependents: Vec<&str> = formulae
            .iter()
            .filter(|f| {
                installed_names.contains(&f.name)
                    && f.dependencies
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|d| d == name)
            })
            .map(|f| f.name.as_str())
            .collect();

        if dependents.is_empty() {
            println!("{} installed explicitly", style("installed:").dim());
        } else {
            println!("{} required by:", style("installed:").dim());
            for dep in &dependents {
                println!("  {} {}", style("←").dim(), style(dep).cyan());
            }
        }

        if pkg.from_source {
            println!("  built from source");
        }
        if pkg.pinned {
            println!("  {}", style("pinned").yellow());
        }

        let cellar_path = pkg.install_mode.cellar_path()?;
        let package_path = cellar_path.join(&pkg.name).join(&pkg.version);
        println!();
        println!("{} {}", style("path:").dim(), package_path.display());
    }

    Ok(())
}

#[instrument(skip(cache))]
async fn info_cask(cache: &Cache, name: &str) -> Result<()> {
    cache.ensure_fresh().await?;

    let casks = cache.load_casks().await?;

    let cask_summary = casks
        .iter()
        .find(|c| c.token == name || c.full_token == name)
        .ok_or_else(|| WaxError::CaskNotFound(name.to_string()))?;

    let cask = cache.fetch_cask_details(name).await?;

    let display_name = cask.name.first().unwrap_or(&cask.token);

    let state = CaskState::new()?;
    let installed_casks = state.load().await?;
    let installed_version = installed_casks
        .get(name)
        .or_else(|| installed_casks.get(cask_summary.full_token.as_str()))
        .or_else(|| installed_casks.get(&cask_summary.token))
        .map(|i| &i.version);

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

    if let Some(ref tap) = tap_slug_from_qualified_name(&cask_summary.full_token) {
        println!("{} {}", style("tap:").dim(), style(tap).cyan());
    }

    if let Some(desc) = &cask.desc {
        println!("{}", desc);
    }

    println!();
    println!("{}", &cask.homepage);

    println!();
    println!("{}", &cask.url);

    if let Some(artifacts) = &cask.artifacts {
        let artifact_types: Vec<&'static str> = artifacts
            .iter()
            .map(|a| a.as_str())
            .collect();

        if !artifact_types.is_empty() {
            println!();
            println!("{}:", style("artifacts").dim());
            for artifact_type in artifact_types {
                println!("  {}", artifact_type);
            }
        }

        if installed_version.is_some() {
            let user_caskroom = CaskState::user_caskroom_dir()?;
            let global_caskroom = CaskState::caskroom_dir();
            let cask_path = if user_caskroom.join(name).exists() {
                user_caskroom.join(name)
            } else {
                global_caskroom.join(name)
            };
            println!();
            println!("{} {}", style("path:").dim(), cask_path.display());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::tap_slug_from_qualified_name;

    #[test]
    fn tap_slug_from_user_tap_formula() {
        assert_eq!(
            tap_slug_from_qualified_name("user/repo/pkg"),
            Some("user/repo".to_string())
        );
    }

    #[test]
    fn tap_slug_from_homebrew_core_is_none() {
        assert_eq!(
            tap_slug_from_qualified_name("homebrew/core/openssl@3"),
            None
        );
    }

    #[test]
    fn tap_slug_from_short_name_is_none() {
        assert_eq!(tap_slug_from_qualified_name("tree"), None);
    }
}
