use crate::api::ApiClient;
use crate::cache::Cache;
use crate::cask::CaskState;
use crate::commands::update;
use crate::error::Result;
use crate::install::InstallState;
use console::style;
use tracing::instrument;

fn calculate_match_score(name: &str, desc: Option<&str>, query: &str) -> Option<i32> {
    let query_lower = query.to_lowercase();
    let name_lower = name.to_lowercase();

    if name_lower == query_lower {
        return Some(1000);
    }

    if name_lower.starts_with(&query_lower) {
        return Some(900);
    }

    if name_lower.contains(&query_lower) {
        return Some(850);
    }

    let name_words: Vec<&str> = name_lower.split(|c: char| !c.is_alphanumeric()).collect();
    for word in &name_words {
        if *word == query_lower {
            return Some(800);
        }
    }

    for word in &name_words {
        if word.starts_with(&query_lower) {
            return Some(700);
        }
    }

    if let Some(description) = desc {
        let desc_lower = description.to_lowercase();
        let desc_words: Vec<&str> = desc_lower.split(|c: char| !c.is_alphanumeric()).collect();

        for word in desc_words {
            if word == query_lower {
                return Some(600);
            }
        }

        for word in &name_words {
            if word.contains(&query_lower) && word.len() < query_lower.len() * 3 {
                return Some(400);
            }
        }

        if desc_lower.contains(&query_lower) {
            return Some(300);
        }

        if query_lower.contains('-') {
            let query_with_spaces = query_lower.replace('-', " ");
            if desc_lower.contains(&query_with_spaces) {
                return Some(250);
            }
        }
    }

    None
}

#[instrument(skip(api_client, cache))]
pub async fn search(api_client: &ApiClient, cache: &Cache, query: &str) -> Result<()> {
    if !cache.is_initialized() {
        println!("initializing package index (first time only)...");
        update::update(api_client, cache).await?;
    }

    let formulae = cache.load_all_formulae().await?;
    let casks = cache.load_casks().await?;

    let state = InstallState::new()?;
    let installed_packages = state.load().await?;
    let cask_state = CaskState::new()?;
    let installed_casks = cask_state.load().await?;

    let core_formulae: Vec<_> = formulae
        .iter()
        .filter(|f| !f.full_name.contains('/') || f.full_name.starts_with("homebrew/"))
        .collect();

    let tap_formulae: Vec<_> = formulae
        .iter()
        .filter(|f| f.full_name.contains('/') && !f.full_name.starts_with("homebrew/"))
        .collect();

    let mut formula_matches: Vec<_> = core_formulae
        .iter()
        .filter_map(|f| {
            calculate_match_score(&f.name, f.desc.as_deref(), query).map(|score| (f, score))
        })
        .collect();

    let mut tap_matches: Vec<_> = tap_formulae
        .iter()
        .filter_map(|f| {
            let name_score = calculate_match_score(&f.name, f.desc.as_deref(), query);
            let full_name_score = calculate_match_score(&f.full_name, f.desc.as_deref(), query);
            name_score.or(full_name_score).map(|score| (f, score))
        })
        .collect();

    let mut cask_matches: Vec<_> = casks
        .iter()
        .filter_map(|c| {
            let token_score = calculate_match_score(&c.token, c.desc.as_deref(), query);
            let name_score = c
                .name
                .iter()
                .filter_map(|n| calculate_match_score(n, c.desc.as_deref(), query))
                .max();
            token_score.or(name_score).map(|score| (c, score))
        })
        .collect();

    formula_matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.name.cmp(&b.0.name)));
    tap_matches.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.full_name.cmp(&b.0.full_name))
    });
    cask_matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.token.cmp(&b.0.token)));

    let formula_matches: Vec<_> = formula_matches.iter().take(20).map(|(f, _)| f).collect();
    let tap_matches: Vec<_> = tap_matches.iter().take(10).map(|(f, _)| f).collect();
    let cask_matches: Vec<_> = cask_matches.iter().take(20).map(|(c, _)| c).collect();

    let total = formula_matches.len() + tap_matches.len() + cask_matches.len();

    if total == 0 {
        println!("no results for '{}'", query);
        return Ok(());
    }

    println!();
    for formula in &formula_matches {
        let desc = formula.desc.as_deref().unwrap_or("");
        let installed_suffix = if installed_packages.contains_key(&formula.name) {
            " · installed"
        } else {
            ""
        };
        if desc.is_empty() {
            println!(
                "{} · {}{}",
                style(&formula.name).magenta(),
                style(&formula.versions.stable).dim(),
                style(installed_suffix).dim()
            );
        } else {
            println!(
                "{} · {}{}",
                style(&formula.name).magenta(),
                style(&formula.versions.stable).dim(),
                style(installed_suffix).dim()
            );
            println!("  {}", desc);
        }
    }

    for formula in &tap_matches {
        let desc = formula.desc.as_deref().unwrap_or("");
        let installed_suffix = if installed_packages.contains_key(&formula.name) {
            " · installed"
        } else {
            ""
        };
        if desc.is_empty() {
            println!(
                "{} · {}{}",
                style(&formula.full_name).magenta(),
                style(&formula.versions.stable).dim(),
                style(installed_suffix).dim()
            );
        } else {
            println!(
                "{} · {}{}",
                style(&formula.full_name).magenta(),
                style(&formula.versions.stable).dim(),
                style(installed_suffix).dim()
            );
            println!("  {}", desc);
        }
    }

    for cask in &cask_matches {
        let desc = cask.desc.as_deref().unwrap_or("");
        let installed_suffix = if installed_casks.contains_key(&cask.token) {
            " · installed"
        } else {
            ""
        };
        if desc.is_empty() {
            println!(
                "{} {} · {}{}",
                style(&cask.token).magenta(),
                style("(cask)").yellow(),
                style(&cask.version).dim(),
                style(installed_suffix).dim()
            );
        } else {
            println!(
                "{} {} · {}{}",
                style(&cask.token).magenta(),
                style("(cask)").yellow(),
                style(&cask.version).dim(),
                style(installed_suffix).dim()
            );
            println!("  {}", desc);
        }
    }

    Ok(())
}
