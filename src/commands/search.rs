use crate::cache::Cache;
use crate::cask::CaskState;
use crate::error::Result;
use crate::install::InstallState;
use console::style;
use std::collections::HashSet;
use tracing::instrument;

#[cfg(target_os = "windows")]
use crate::package_spec::Ecosystem;
#[cfg(target_os = "windows")]
use crate::remote_search::{collect_remote_hits, dedupe_remote_by_speed, print_remote_hits};

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

        for word in &desc_words {
            if *word == query_lower {
                return Some(600);
            }
        }

        for word in &desc_words {
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

#[instrument(skip(cache))]
pub async fn search(cache: &Cache, query: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let (eco_filter, q) = crate::package_spec::parse_search_query(query);
        let q = q.trim();
        if q.is_empty() {
            println!("empty search query");
            return Ok(());
        }

        cache.ensure_fresh().await?;
        let formulae = cache.load_all_formulae().await?;
        let casks = cache.load_casks().await?;

        let state = InstallState::new()?;
        let installed_packages = state.load().await?;
        let cask_state = CaskState::new()?;
        let installed_casks = cask_state.load().await?;

        let brew_catalog = eco_filter.is_none() || eco_filter == Some(Ecosystem::Brew);

        let core_formulae: Vec<_> = formulae
            .iter()
            .filter(|f| !f.full_name.contains('/') || f.full_name.starts_with("homebrew/"))
            .collect();

        let tap_formulae: Vec<_> = formulae
            .iter()
            .filter(|f| f.full_name.contains('/') && !f.full_name.starts_with("homebrew/"))
            .collect();

        let mut formula_matches: Vec<_> = Vec::new();
        let mut tap_matches: Vec<_> = Vec::new();
        let mut cask_matches: Vec<_> = Vec::new();

        if brew_catalog {
            formula_matches = core_formulae
                .iter()
                .filter_map(|f| {
                    calculate_match_score(&f.name, f.desc.as_deref(), q).map(|score| (f, score))
                })
                .collect();

            tap_matches = tap_formulae
                .iter()
                .filter_map(|f| {
                    let name_score = calculate_match_score(&f.name, f.desc.as_deref(), q);
                    let full_name_score =
                        calculate_match_score(&f.full_name, f.desc.as_deref(), q);
                    name_score.or(full_name_score).map(|score| (f, score))
                })
                .collect();

            cask_matches = casks
                .iter()
                .filter_map(|c| {
                    let token_score = calculate_match_score(&c.token, c.desc.as_deref(), q);
                    let name_score = c
                        .name
                        .iter()
                        .filter_map(|n| calculate_match_score(n, c.desc.as_deref(), q))
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
        }

        let formula_matches: Vec<_> =
            formula_matches.iter().take(20).map(|(f, _)| f).collect();
        let tap_matches: Vec<_> = tap_matches.iter().take(10).map(|(f, _)| f).collect();
        let cask_matches: Vec<_> = cask_matches.iter().take(20).map(|(c, _)| c).collect();

        let total = formula_matches.len() + tap_matches.len() + cask_matches.len();

        let brew_blocklist: HashSet<String> = formulae
            .iter()
            .map(|f| f.name.to_lowercase())
            .chain(casks.iter().map(|c| c.token.to_lowercase()))
            .collect();

        let include_scoop = eco_filter.map(|e| e == Ecosystem::Scoop).unwrap_or(true);
        let include_choco = eco_filter
            .map(|e| e == Ecosystem::Chocolatey)
            .unwrap_or(true);
        let include_winget = eco_filter.map(|e| e == Ecosystem::Winget).unwrap_or(true);

        let mut remote_hits =
            collect_remote_hits(cache, q, include_scoop, include_choco, include_winget).await?;
        remote_hits.retain(|h| !brew_blocklist.contains(&h.id.to_lowercase()));
        remote_hits = dedupe_remote_by_speed(remote_hits);

        if total == 0 && remote_hits.is_empty() {
            println!("no results for '{}'", query);
            return Ok(());
        }

        if !brew_catalog {
            println!(
                "{}",
                style(format!(
                    "Filtered to {} only (drop the prefix to search every catalogue)",
                    eco_filter.unwrap().label()
                ))
                .dim()
            );
        }

        if total > 0 {
            println!();
        }
        for formula in &formula_matches {
            let desc = formula.desc.as_deref().unwrap_or("");
            let installed_suffix = if installed_packages.contains_key(&formula.name) {
                " · installed"
            } else {
                ""
            };
            let status_label = if formula.disabled {
                format!(" {}", style("[disabled]").red())
            } else if formula.deprecated {
                format!(" {}", style("[deprecated]").yellow())
            } else {
                String::new()
            };
            println!(
                "{} · {}{}{}",
                style(&formula.name).magenta(),
                style(&formula.versions.stable).dim(),
                style(installed_suffix).dim(),
                status_label
            );
            if !desc.is_empty() {
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
            let status_label = if formula.disabled {
                format!(" {}", style("[disabled]").red())
            } else if formula.deprecated {
                format!(" {}", style("[deprecated]").yellow())
            } else {
                String::new()
            };
            println!(
                "{} · {}{}{}",
                style(&formula.full_name).magenta(),
                style(&formula.versions.stable).dim(),
                style(installed_suffix).dim(),
                status_label
            );
            if !desc.is_empty() {
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
            let status_label = if cask.disabled {
                format!(" {}", style("[disabled]").red())
            } else if cask.deprecated {
                format!(" {}", style("[deprecated]").yellow())
            } else {
                String::new()
            };
            println!(
                "{} {} · {}{}{}",
                style(&cask.token).magenta(),
                style("(cask)").yellow(),
                style(&cask.version).dim(),
                style(installed_suffix).dim(),
                status_label
            );
            if !desc.is_empty() {
                println!("  {}", desc);
            }
        }

        let mut parts = Vec::new();
        if !formula_matches.is_empty() {
            parts.push(format!(
                "{} {}",
                formula_matches.len(),
                if formula_matches.len() == 1 {
                    "formula"
                } else {
                    "formulae"
                }
            ));
        }
        if !tap_matches.is_empty() {
            parts.push(format!("{} from taps", tap_matches.len()));
        }
        if !cask_matches.is_empty() {
            parts.push(format!(
                "{} {}",
                cask_matches.len(),
                if cask_matches.len() == 1 {
                    "cask"
                } else {
                    "casks"
                }
            ));
        }
        if total > 0 {
            println!("\n{}", style(parts.join(", ")).dim());
        }

        print_remote_hits(&remote_hits);

        return Ok(());
    }

    #[cfg(not(target_os = "windows"))]
    {
        cache.ensure_fresh().await?;

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
                let full_name_score =
                    calculate_match_score(&f.full_name, f.desc.as_deref(), query);
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

        let formula_matches: Vec<_> =
            formula_matches.iter().take(20).map(|(f, _)| f).collect();
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
            let status_label = if formula.disabled {
                format!(" {}", style("[disabled]").red())
            } else if formula.deprecated {
                format!(" {}", style("[deprecated]").yellow())
            } else {
                String::new()
            };
            println!(
                "{} · {}{}{}",
                style(&formula.name).magenta(),
                style(&formula.versions.stable).dim(),
                style(installed_suffix).dim(),
                status_label
            );
            if !desc.is_empty() {
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
            let status_label = if formula.disabled {
                format!(" {}", style("[disabled]").red())
            } else if formula.deprecated {
                format!(" {}", style("[deprecated]").yellow())
            } else {
                String::new()
            };
            println!(
                "{} · {}{}{}",
                style(&formula.full_name).magenta(),
                style(&formula.versions.stable).dim(),
                style(installed_suffix).dim(),
                status_label
            );
            if !desc.is_empty() {
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
            let status_label = if cask.disabled {
                format!(" {}", style("[disabled]").red())
            } else if cask.deprecated {
                format!(" {}", style("[deprecated]").yellow())
            } else {
                String::new()
            };
            println!(
                "{} {} · {}{}{}",
                style(&cask.token).magenta(),
                style("(cask)").yellow(),
                style(&cask.version).dim(),
                style(installed_suffix).dim(),
                status_label
            );
            if !desc.is_empty() {
                println!("  {}", desc);
            }
        }

        let mut parts = Vec::new();
        if !formula_matches.is_empty() {
            parts.push(format!(
                "{} {}",
                formula_matches.len(),
                if formula_matches.len() == 1 {
                    "formula"
                } else {
                    "formulae"
                }
            ));
        }
        if !tap_matches.is_empty() {
            parts.push(format!("{} from taps", tap_matches.len()));
        }
        if !cask_matches.is_empty() {
            parts.push(format!(
                "{} {}",
                cask_matches.len(),
                if cask_matches.len() == 1 {
                    "cask"
                } else {
                    "casks"
                }
            ));
        }
        println!("\n{}", style(parts.join(", ")).dim());

        Ok(())
    }
}
