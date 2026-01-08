use crate::api::ApiClient;
use crate::cache::{Cache, CacheMetadata};
use crate::error::Result;
use crate::ui::{create_spinner, print_success};
use tracing::instrument;

#[instrument(skip(api_client, cache))]
pub async fn update(api_client: &ApiClient, cache: &Cache) -> Result<()> {
    let spinner = create_spinner("Updating formula index...");

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
    let status = if formulae_fetch.not_modified && casks_fetch.not_modified {
        "Already up-to-date"
    } else if formulae_fetch.not_modified {
        "Updated casks only"
    } else if casks_fetch.not_modified {
        "Updated formulae only"
    } else {
        "Updated"
    };

    print_success(&format!(
        "{} - {} formulae and {} casks in {:.2}s",
        status,
        formula_count,
        cask_count,
        elapsed.as_secs_f64()
    ));

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
