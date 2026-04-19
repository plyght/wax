use crate::cache::Cache;
use crate::error::{Result, WaxError};
use crate::install::InstallState;
use console::style;
use std::collections::{HashMap, HashSet};

pub async fn deps(cache: &Cache, formula: &str, tree: bool, installed: bool) -> Result<()> {
    let formulae = cache.load_all_formulae().await?;
    let formula_index: HashMap<_, _> = formulae
        .iter()
        .map(|f| (f.name.as_str(), f))
        .chain(formulae.iter().map(|f| (f.full_name.as_str(), f)))
        .collect();

    let target = formula_index
        .get(formula)
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
        print_dep_tree(&filtered, &formula_index, &mut HashSet::new(), "", true);
    } else {
        for dep in &filtered {
            println!("{}", style(dep).cyan());
        }
    }

    Ok(())
}

fn print_dep_tree(
    deps: &[&str],
    formula_index: &HashMap<&str, &crate::api::Formula>,
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

        if let Some(formula) = formula_index.get(*dep) {
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
                print_dep_tree(&child_deps, formula_index, seen, &new_prefix, is_last);
            }
        }
    }
}
