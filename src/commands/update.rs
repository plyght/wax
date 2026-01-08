use crate::api::ApiClient;
use crate::cache::{Cache, CacheMetadata};
use crate::error::Result;
use crate::ui::{create_spinner, print_success};
use tracing::instrument;

#[instrument(skip(api_client, cache))]
pub async fn update(api_client: &ApiClient, cache: &Cache) -> Result<()> {
    let spinner = create_spinner("Updating formula index...");

    let start = std::time::Instant::now();

    let (formulae_result, casks_result) =
        tokio::join!(api_client.fetch_formulae(), api_client.fetch_casks());

    let formulae = formulae_result?;
    let casks = casks_result?;

    cache.save_formulae(&formulae).await?;
    cache.save_casks(&casks).await?;

    let metadata = CacheMetadata {
        last_updated: chrono::Utc::now().timestamp(),
        formula_count: formulae.len(),
        cask_count: casks.len(),
    };
    cache.save_metadata(&metadata).await?;

    spinner.finish_and_clear();

    let elapsed = start.elapsed();
    print_success(&format!(
        "Updated {} formulae and {} casks in {:.1}s",
        formulae.len(),
        casks.len(),
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
