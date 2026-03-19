use crate::cache::Cache;
use crate::error::Result;
use crate::install::InstallState;
use console::style;
use std::collections::HashSet;

pub async fn uses(cache: &Cache, formula: &str, installed_only: bool) -> Result<()> {
    let formulae = cache.load_all_formulae().await?;

    let installed_names: HashSet<String> = if installed_only {
        let state = InstallState::new()?;
        state.sync_from_cellar().await.ok();
        state.load().await?.into_keys().collect()
    } else {
        HashSet::new()
    };

    let mut dependents: Vec<&str> = formulae
        .iter()
        .filter(|f| {
            if installed_only && !installed_names.contains(&f.name) {
                return false;
            }
            f.dependencies
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|d| d == formula)
        })
        .map(|f| f.name.as_str())
        .collect();

    dependents.sort_unstable();

    if dependents.is_empty() {
        if installed_only {
            println!(
                "no installed packages depend on {}",
                style(formula).magenta()
            );
        } else {
            println!("no packages depend on {}", style(formula).magenta());
        }
    } else {
        let scope = if installed_only { " (installed)" } else { "" };
        println!(
            "{}{} is used by {} package{}:",
            style(formula).magenta(),
            scope,
            dependents.len(),
            if dependents.len() == 1 { "" } else { "s" }
        );
        for name in dependents {
            println!("  {}", style(name).cyan());
        }
    }

    Ok(())
}
