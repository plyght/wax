use crate::cache::Cache;
use crate::error::Result;
use crate::install::InstallState;
use crate::version::is_same_or_newer;
use console::style;
use std::collections::HashMap;

pub async fn audit(cache: &Cache) -> Result<()> {
    let state = InstallState::new()?;
    state.sync_from_cellar().await.ok();
    let installed = state.load().await?;

    if installed.is_empty() {
        println!("no packages installed");
        return Ok(());
    }

    cache.ensure_fresh().await?;
    let formulae = cache.load_all_formulae().await?;
    let formula_index: HashMap<_, _> = formulae.iter().map(|f| (f.name.as_str(), f)).collect();

    let mut deprecated = Vec::new();
    let mut disabled = Vec::new();
    let mut outdated = Vec::new();
    let mut unknown = Vec::new();

    for (name, pkg) in &installed {
        match formula_index.get(name.as_str()) {
            Some(formula) => {
                if formula.disabled {
                    let reason = formula
                        .disable_reason
                        .as_deref()
                        .unwrap_or("no reason given");
                    disabled.push((name.as_str(), pkg.version.as_str(), reason));
                } else if formula.deprecated {
                    let reason = formula
                        .deprecation_reason
                        .as_deref()
                        .unwrap_or("no reason given");
                    deprecated.push((name.as_str(), pkg.version.as_str(), reason));
                }

                let latest = formula.full_version();
                if !is_same_or_newer(&pkg.version, &latest) {
                    outdated.push((name.as_str(), pkg.version.as_str(), latest));
                }
            }
            None => {
                unknown.push((name.as_str(), pkg.version.as_str()));
            }
        }
    }

    let total_issues = disabled.len() + deprecated.len();

    if total_issues == 0 && outdated.is_empty() && unknown.is_empty() {
        println!(
            "{} {} installed packages — no issues found",
            style("✓").green(),
            installed.len()
        );
        return Ok(());
    }

    if !disabled.is_empty() {
        println!(
            "\n{} {} disabled {}:",
            style("✗").red().bold(),
            disabled.len(),
            if disabled.len() == 1 {
                "package"
            } else {
                "packages"
            }
        );
        for (name, version, reason) in &disabled {
            println!(
                "  {} {}  {}",
                style(name).red(),
                style(format!("@{}", version)).dim(),
                style(reason).dim()
            );
        }
    }

    if !deprecated.is_empty() {
        println!(
            "\n{} {} deprecated {}:",
            style("!").yellow().bold(),
            deprecated.len(),
            if deprecated.len() == 1 {
                "package"
            } else {
                "packages"
            }
        );
        for (name, version, reason) in &deprecated {
            println!(
                "  {} {}  {}",
                style(name).yellow(),
                style(format!("@{}", version)).dim(),
                style(reason).dim()
            );
        }
    }

    if !outdated.is_empty() {
        println!(
            "\n{} {} outdated {}:",
            style("↑").cyan().bold(),
            outdated.len(),
            if outdated.len() == 1 {
                "package"
            } else {
                "packages"
            }
        );
        for (name, installed_ver, latest_ver) in &outdated {
            println!(
                "  {} {} → {}",
                style(name).cyan(),
                style(installed_ver).dim(),
                style(latest_ver).green()
            );
        }
    }

    if !unknown.is_empty() {
        println!(
            "\n{} {} {} not in any known tap:",
            style("?").dim().bold(),
            unknown.len(),
            if unknown.len() == 1 {
                "package"
            } else {
                "packages"
            }
        );
        for (name, version) in &unknown {
            println!(
                "  {} {}",
                style(name).dim(),
                style(format!("@{}", version)).dim()
            );
        }
    }

    println!("\n{} installed, {} issues", installed.len(), total_issues);

    Ok(())
}
