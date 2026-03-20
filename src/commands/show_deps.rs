use crate::cache::Cache;
use crate::error::{Result, WaxError};
use crate::install::InstallState;
use console::style;
use std::collections::HashSet;

pub async fn deps(cache: &Cache, formula: &str, tree: bool, installed: bool) -> Result<()> {
    let formulae = cache.load_all_formulae().await?;

    let target = formulae
        .iter()
        .find(|f| f.name == formula || f.full_name == formula)
        .ok_or_else(|| WaxError::FormulaNotFound(formula.to_string()))?;

    let installed_names: HashSet<String> = if installed {
        let state = InstallState::new()?;
        state.sync_from_cellar().await.ok();
        state.load().await?.into_keys().collect()
    } else {
        HashSet::new()
    };

    let deps = target.dependencies.as_deref().unwrap_or_default();
    let filtered: Vec<&str> = if installed {
        deps.iter()
            .filter(|d| installed_names.contains(*d))
            .map(|d| d.as_str())
            .collect()
    } else {
        deps.iter().map(|d| d.as_str()).collect()
    };

    if filtered.is_empty() {
        println!("{} has no dependencies", style(formula).magenta());
        return Ok(());
    }

    if tree {
        println!("{}", style(formula).magenta().bold());
        print_dep_tree(&filtered, &formulae, &mut HashSet::new(), "", true);
    } else {
        for dep in &filtered {
            println!("{}", style(dep).cyan());
        }
    }

    Ok(())
}

fn print_dep_tree(
    deps: &[&str],
    formulae: &[crate::api::Formula],
    seen: &mut HashSet<String>,
    prefix: &str,
    last_group: bool,
) {
    let _ = last_group;
    for (i, dep) in deps.iter().enumerate() {
        let is_last = i == deps.len() - 1;
        let connector = if is_last { "└─ " } else { "├─ " };
        let already_seen = seen.contains(*dep);

        print!("{}{}", prefix, connector);

        if already_seen {
            println!("{} {}", style(dep).cyan(), style("(already shown)").dim());
            continue;
        }

        println!("{}", style(dep).cyan());
        seen.insert(dep.to_string());

        if let Some(formula) = formulae.iter().find(|f| f.name == *dep) {
            let child_deps: Vec<&str> = formula
                .dependencies
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|d| d.as_str())
                .collect();

            if !child_deps.is_empty() {
                let extension = if is_last { "   " } else { "│  " };
                let new_prefix = format!("{}{}", prefix, extension);
                print_dep_tree(&child_deps, formulae, seen, &new_prefix, is_last);
            }
        }
    }
}
