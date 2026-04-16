use crate::cache::Cache;
use crate::error::Result;
use crate::install::InstallState;
use console::style;
use std::collections::{HashMap, HashSet};

pub async fn leaves(cache: &Cache) -> Result<()> {
    let state = InstallState::new()?;
    state.sync_from_cellar().await.ok();
    let installed = state.load().await?;

    if installed.is_empty() {
        println!("no packages installed");
        return Ok(());
    }

    let installed_names: HashSet<String> = installed.keys().cloned().collect();

    // Collect all packages that are depended on by other installed packages
    let formulae = cache.load_all_formulae().await?;
    let formula_index: HashMap<_, _> = formulae.iter().map(|f| (f.name.as_str(), f)).collect();
    let mut depended_on: HashSet<String> = HashSet::new();

    for name in &installed_names {
        if let Some(formula) = formula_index.get(name.as_str()) {
            if let Some(deps) = &formula.dependencies {
                for dep in deps {
                    depended_on.insert(dep.clone());
                }
            }
        }
    }

    let mut leaves: Vec<&str> = installed_names
        .iter()
        .filter(|name| !depended_on.contains(*name))
        .map(|s| s.as_str())
        .collect();

    leaves.sort_unstable();

    if leaves.is_empty() {
        println!("no leaf packages (all packages are dependencies of others)");
    } else {
        for name in leaves {
            if let Some(pkg) = installed.get(name) {
                println!(
                    "{}  {}",
                    style(name).magenta(),
                    style(format!("@{}", pkg.version)).dim()
                );
            }
        }
    }

    Ok(())
}
