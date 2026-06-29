//! Shared `reqwest` clients to avoid per-call TLS handshakes and builder overhead.

use crate::version::WAX_VERSION;
use std::sync::OnceLock;
use std::time::Duration;

static API_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
static DOWNLOAD_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
static DEFAULT_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn user_agent() -> String {
    format!("waxpkg/{WAX_VERSION} (https://github.com/plyght/wax)")
}

fn build_client(timeout: Duration, compress: bool) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(user_agent()).https_only(true);
    if compress {
        builder = builder.gzip(true).brotli(true);
    } else {
        builder = builder.gzip(false).brotli(false);
    }
    builder.build().expect("Failed to create HTTP client")
}

/// Homebrew JSON API: 30s timeout, compressed responses.
pub fn api() -> &'static reqwest::Client {
    API_CLIENT.get_or_init(|| build_client(Duration::from_secs(30), true))
}

/// Bottle/cask downloads: 5 minute timeout, raw bytes (no double decompression).
pub fn download() -> &'static reqwest::Client {
    DOWNLOAD_CLIENT.get_or_init(|| build_client(Duration::from_secs(300), false))
}

/// General-purpose client (GitHub, GHCR, ecosystem indexes): 60s, compressed.
pub fn default_client() -> &'static reqwest::Client {
    DEFAULT_CLIENT.get_or_init(|| build_client(Duration::from_secs(60), true))
}
