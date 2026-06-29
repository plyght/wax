use crate::cache::{Cache, CacheMetadata};
use crate::error::Result;
use crate::signal::check_cancelled;
use crate::tap::TapManager;
use crate::ui::create_spinner;
use console::style;
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn update(cache: &Cache) -> Result<()> {
    let spinner = create_spinner("Updating package index...");

    let start = std::time::Instant::now();

    let metadata = cache.load_metadata().await?;

    let (formulae_etag, formulae_last_modified) = metadata
        .as_ref()
        .map(|m| {
            (
                m.formulae_etag.as_deref(),
                m.formulae_last_modified.as_deref(),
            )
        })
        .unwrap_or((None, None));

    let (casks_etag, casks_last_modified) = metadata
        .as_ref()
        .map(|m| (m.casks_etag.as_deref(), m.casks_last_modified.as_deref()))
        .unwrap_or((None, None));

    let (formulae_result, casks_result) = tokio::join!(
        cache.fetch_formulae_conditional(formulae_etag, formulae_last_modified),
        cache.fetch_casks_conditional(casks_etag, casks_last_modified)
    );

    let mut formulae_fetch = formulae_result?;
    let mut casks_fetch = casks_result?;

    let formula_count = if formulae_fetch.not_modified {
        cache.load_formulae().await?.len()
    } else if let Some(data) = formulae_fetch.data.take() {
        let count = data.len();
        cache.save_formulae(&data).await?;
        count
    } else {
        cache.load_formulae().await?.len()
    };

    let cask_count = if casks_fetch.not_modified {
        cache.load_casks().await?.len()
    } else if let Some(data) = casks_fetch.data.take() {
        let count = data.len();
        cache.save_casks(&data).await?;
        count
    } else {
        cache.load_casks().await?.len()
    };

    let tap_count = update_taps(cache).await?;

    let new_metadata = CacheMetadata {
        last_updated: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
        formula_count,
        cask_count,
        formulae_etag: formulae_fetch
            .etag
            .or_else(|| metadata.as_ref().and_then(|m| m.formulae_etag.clone())),
        formulae_last_modified: formulae_fetch.last_modified.or_else(|| {
            metadata
                .as_ref()
                .and_then(|m| m.formulae_last_modified.clone())
        }),
        casks_etag: casks_fetch
            .etag
            .or_else(|| metadata.as_ref().and_then(|m| m.casks_etag.clone())),
        casks_last_modified: casks_fetch.last_modified.or_else(|| {
            metadata
                .as_ref()
                .and_then(|m| m.casks_last_modified.clone())
        }),
    };
    cache.save_metadata(&new_metadata).await?;

    spinner.finish_and_clear();

    let elapsed = start.elapsed();
    let core_status = if formulae_fetch.not_modified && casks_fetch.not_modified {
        "up to date"
    } else if formulae_fetch.not_modified {
        "updated casks"
    } else if casks_fetch.not_modified {
        "updated formulae"
    } else {
        "updated"
    };

    print_status(
        core_status,
        formula_count,
        cask_count,
        tap_count,
        elapsed,
    );

    Ok(())
}

async fn update_taps(cache: &Cache) -> Result<usize> {
    let mut tap_manager = TapManager::new()?;
    tap_manager.load().await?;
    let taps = tap_manager
        .list_taps()
        .iter()
        .map(|t| t.full_name.clone())
        .collect::<Vec<_>>();
    let tap_count = taps.len();

    if tap_count > 0 {
        cache.invalidate_all_tap_caches().await?;

        for tap_name in &taps {
            check_cancelled()?;
            if let Err(e) = tap_manager.update_tap(tap_name).await {
                eprintln!(
                    "  {} failed to update tap {}: {}",
                    style("!").yellow(),
                    style(tap_name).magenta(),
                    e
                );
            }
        }
    }

    Ok(tap_count)
}

fn print_status(
    core_status: &str,
    formula_count: usize,
    cask_count: usize,
    tap_count: usize,
    elapsed: std::time::Duration,
) {
    if tap_count > 0 {
        println!(
            "{} {} · {} formulae, {} casks, {} {}{}",
            style("✓").green(),
            core_status,
            style(formula_count).cyan(),
            style(cask_count).cyan(),
            style(tap_count).cyan(),
            if tap_count == 1 { "tap" } else { "taps" },
            crate::ui::elapsed_suffix(elapsed)
        );
    } else {
        println!(
            "{} {} · {} formulae, {} casks{}",
            style("✓").green(),
            core_status,
            style(formula_count).cyan(),
            style(cask_count).cyan(),
            crate::ui::elapsed_suffix(elapsed)
        );
    }
}
