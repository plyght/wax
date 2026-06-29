use crate::cache::Cache;
#[cfg(not(target_os = "windows"))]
use crate::cask::CaskState;
use crate::error::Result;
#[cfg(not(target_os = "windows"))]
use crate::install::InstallState;
use console::style;
use tracing::instrument;

#[cfg(target_os = "windows")]
use crate::package_spec::Ecosystem;
#[cfg(target_os = "windows")]
use crate::remote_search::{
    collect_remote_hits, dedupe_remote_by_speed, print_remote_hits, windows_search_plan,
};

#[instrument(skip(cache))]
pub async fn search(cache: &Cache, query: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        search_windows(cache, query).await
    }
    #[cfg(not(target_os = "windows"))]
    {
        search_unix(cache, query).await
    }
}

#[cfg(target_os = "windows")]
async fn search_windows(cache: &Cache, query: &str) -> Result<()> {
    let (eco_filter, q) = crate::package_spec::parse_search_query(query);
    crate::error::reject_brew_ecosystem(eco_filter)?;
    let q = q.trim();
    if q.is_empty() {
        println!("empty search query");
        return Ok(());
    }

    let plan = windows_search_plan(eco_filter);
    let remote_hits = if plan.include_scoop || plan.include_choco || plan.include_winget {
        let hits = collect_remote_hits(
            cache,
            q,
            plan.include_scoop,
            plan.include_choco,
            plan.include_winget,
        )
        .await?;
        dedupe_remote_by_speed(hits)
    } else {
        Vec::new()
    };

    if remote_hits.is_empty() {
        println!("no results for '{query}'");
        return Ok(());
    }

    if let Some(eco) = eco_filter {
        println!(
            "{}",
            style(format!(
                "Filtered to {} only (drop the prefix to search Scoop, winget, and Chocolatey)",
                eco.label()
            ))
            .dim()
        );
    }

    let remote_section = match eco_filter {
        Some(Ecosystem::Scoop) => "Scoop Main",
        Some(Ecosystem::Winget) => "winget-pkgs",
        Some(Ecosystem::Chocolatey) => "Chocolatey",
        _ => "Windows catalogues (scoop, winget, choco)",
    };
    print_remote_hits(&remote_hits, remote_section);

    Ok(())
}

#[cfg(not(target_os = "windows"))]
async fn search_unix(cache: &Cache, query: &str) -> Result<()> {
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
            crate::catalog_match::match_score(&f.name, f.desc.as_deref(), query)
                .map(|score| (f, score))
        })
        .collect();

    let mut tap_matches: Vec<_> = tap_formulae
        .iter()
        .filter_map(|f| {
            let name_score = crate::catalog_match::match_score(&f.name, f.desc.as_deref(), query);
            let full_name_score =
                crate::catalog_match::match_score(&f.full_name, f.desc.as_deref(), query);
            name_score.or(full_name_score).map(|score| (f, score))
        })
        .collect();

    let mut cask_matches: Vec<_> = casks
        .iter()
        .filter_map(|c| {
            let token_score = crate::catalog_match::match_score(&c.token, c.desc.as_deref(), query);
            let name_score = c
                .name
                .iter()
                .filter_map(|n| crate::catalog_match::match_score(n, c.desc.as_deref(), query))
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
        print_formula(
            formula,
            installed_packages.contains_key(&formula.name),
            &formula.name,
        );
    }

    for formula in &tap_matches {
        print_formula(
            formula,
            installed_packages.contains_key(&formula.name),
            &formula.full_name,
        );
    }

    for cask in &cask_matches {
        print_cask(cask, installed_casks.contains_key(&cask.token));
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

#[cfg(not(target_os = "windows"))]
fn print_formula(formula: &crate::api::Formula, is_installed: bool, display_name: &str) {
    let desc = formula.desc.as_deref().unwrap_or("");
    let installed_suffix = if is_installed { " · installed" } else { "" };
    let status_label = if formula.disabled {
        format!(" {}", style("[disabled]").red())
    } else if formula.deprecated {
        format!(" {}", style("[deprecated]").yellow())
    } else {
        String::new()
    };
    println!(
        "{} · {}{}{}",
        style(display_name).magenta(),
        style(&formula.versions.stable).dim(),
        style(installed_suffix).dim(),
        status_label
    );
    if !desc.is_empty() {
        println!("  {}", desc);
    }
}

#[cfg(not(target_os = "windows"))]
fn print_cask(cask: &crate::api::Cask, is_installed: bool) {
    let desc = cask.desc.as_deref().unwrap_or("");
    let installed_suffix = if is_installed { " · installed" } else { "" };
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
