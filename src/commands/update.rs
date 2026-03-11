use crate::api::ApiClient;
use crate::cache::{Cache, CacheMetadata};
use crate::error::Result;
use crate::tap::TapManager;
use crate::ui::create_spinner;
use console::style;
use tracing::instrument;

#[instrument(skip(api_client, cache))]
pub async fn update(api_client: &ApiClient, cache: &Cache) -> Result<()> {
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
        api_client.fetch_formulae_conditional(formulae_etag, formulae_last_modified),
        api_client.fetch_casks_conditional(casks_etag, casks_last_modified)
    );

    let formulae_fetch = formulae_result?;
    let casks_fetch = casks_result?;

    let (_formulae, formula_count) = if formulae_fetch.not_modified {
        let cached = cache.load_formulae().await?;
        let count = cached.len();
        (cached, count)
    } else if let Some(data) = formulae_fetch.data {
        let count = data.len();
        cache.save_formulae(&data).await?;
        (data, count)
    } else {
        let cached = cache.load_formulae().await?;
        let count = cached.len();
        (cached, count)
    };

    let (_casks, cask_count) = if casks_fetch.not_modified {
        let cached = cache.load_casks().await?;
        let count = cached.len();
        (cached, count)
    } else if let Some(data) = casks_fetch.data {
        let count = data.len();
        cache.save_casks(&data).await?;
        (data, count)
    } else {
        let cached = cache.load_casks().await?;
        let count = cached.len();
        (cached, count)
    };

    let mut tap_manager = TapManager::new()?;
    tap_manager.load().await?;
    let taps = tap_manager.list_taps().iter().map(|t| t.full_name.clone()).collect::<Vec<_>>();
    let tap_count = taps.len();

    if tap_count > 0 {
        cache.invalidate_all_tap_caches().await?;

        for tap_name in &taps {
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

    let new_metadata = CacheMetadata {
        last_updated: chrono::Utc::now().timestamp(),
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

    println!();
    if tap_count > 0 {
        println!(
            "{} {} · {} formulae, {} casks, {} {} [{}ms]",
            style("✓").green(),
            core_status,
            style(formula_count).cyan(),
            style(cask_count).cyan(),
            style(tap_count).cyan(),
            if tap_count == 1 { "tap" } else { "taps" },
            elapsed.as_millis()
        );
    } else {
        println!(
            "{} {} · {} formulae, {} casks [{}ms]",
            style("✓").green(),
            core_status,
            style(formula_count).cyan(),
            style(cask_count).cyan(),
            elapsed.as_millis()
        );
    }

    Ok(())
}

mod chrono {
    pub struct Utc;
    impl Utc {
        pub fn now() -> DateTime {
            DateTime
        }
    }
    pub struct DateTime;
    impl DateTime {
        pub fn timestamp(&self) -> i64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
        }
    }
}
