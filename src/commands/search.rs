use crate::api::ApiClient;
use crate::cache::Cache;
use crate::commands::update;
use crate::error::Result;
use crate::ui::print_info;
use tracing::instrument;

#[instrument(skip(api_client, cache))]
pub async fn search(api_client: &ApiClient, cache: &Cache, query: &str) -> Result<()> {
    if !cache.is_initialized() {
        println!("Initializing package index (first time only)...");
        update::update(api_client, cache).await?;
    }

    let formulae = cache.load_formulae().await?;
    let casks = cache.load_casks().await?;

    let query_lower = query.to_lowercase();

    let mut formula_matches: Vec<_> = formulae
        .iter()
        .filter(|f| {
            f.name.to_lowercase().contains(&query_lower)
                || f.desc
                    .as_ref()
                    .map(|d| d.to_lowercase().contains(&query_lower))
                    .unwrap_or(false)
        })
        .collect();

    let mut cask_matches: Vec<_> = casks
        .iter()
        .filter(|c| {
            c.token.to_lowercase().contains(&query_lower)
                || c.name
                    .iter()
                    .any(|n| n.to_lowercase().contains(&query_lower))
                || c.desc
                    .as_ref()
                    .map(|d| d.to_lowercase().contains(&query_lower))
                    .unwrap_or(false)
        })
        .collect();

    formula_matches.sort_by_key(|f| &f.name);
    cask_matches.sort_by_key(|c| &c.token);

    let formula_matches = &formula_matches[..formula_matches.len().min(20)];
    let cask_matches = &cask_matches[..cask_matches.len().min(20)];

    if !formula_matches.is_empty() {
        println!("\n==> Formulae");
        for formula in formula_matches {
            let desc = formula.desc.as_deref().unwrap_or("No description");
            println!("{:<30} {}", formula.name, desc);
        }
    }

    if !cask_matches.is_empty() {
        println!("\n==> Casks");
        for cask in cask_matches {
            let desc = cask.desc.as_deref().unwrap_or("No description");
            println!("{:<30} {}", cask.token, desc);
        }
    }

    if formula_matches.is_empty() && cask_matches.is_empty() {
        print_info(&format!("No formulae or casks matching '{}'", query));
    }

    Ok(())
}
