use crate::error::Result;
use serde::{Deserialize, Serialize};
use tracing::{info, instrument};

const FORMULA_API_URL: &str = "https://formulae.brew.sh/api/formula.json";
const CASK_API_URL: &str = "https://formulae.brew.sh/api/cask.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Formula {
    pub name: String,
    pub full_name: String,
    pub desc: Option<String>,
    pub homepage: String,
    pub versions: Versions,
    pub installed: Option<Vec<InstalledVersion>>,
    pub dependencies: Option<Vec<String>>,
    pub build_dependencies: Option<Vec<String>>,
    pub bottle: Option<BottleInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BottleInfo {
    pub stable: Option<BottleStable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BottleStable {
    pub files: std::collections::HashMap<String, BottleFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BottleFile {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Versions {
    pub stable: String,
    pub bottle: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledVersion {
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cask {
    pub token: String,
    pub full_token: String,
    pub name: Vec<String>,
    pub desc: Option<String>,
    pub homepage: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaskDetails {
    pub token: String,
    pub name: Vec<String>,
    pub desc: Option<String>,
    pub homepage: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub artifacts: Option<Vec<CaskArtifact>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CaskArtifact {
    App { app: Vec<String> },
    Pkg { pkg: Vec<String> },
    Binary { binary: Vec<serde_json::Value> },
    Uninstall { uninstall: Vec<serde_json::Value> },
    Preflight { preflight: Option<String> },
    Other(serde_json::Value),
}

pub struct ApiClient {
    client: reqwest::Client,
}

#[derive(Debug)]
pub struct FetchResult<T> {
    pub data: Option<T>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub not_modified: bool,
}

impl ApiClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .gzip(true)
            .brotli(true)
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }

    #[instrument(skip(self))]
    pub async fn fetch_formulae_conditional(
        &self,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<FetchResult<Vec<Formula>>> {
        info!("Fetching formulae from API with conditional headers");
        let mut request = self.client.get(FORMULA_API_URL);

        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        if let Some(last_modified) = last_modified {
            request = request.header("If-Modified-Since", last_modified);
        }

        let response = request.send().await?;

        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            info!("Formulae not modified (304)");
            return Ok(FetchResult {
                data: None,
                etag: None,
                last_modified: None,
                not_modified: true,
            });
        }

        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let last_modified = response
            .headers()
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let body = response.bytes().await?;
        let formulae: Vec<Formula> = serde_json::from_slice(&body)?;
        info!("Fetched {} formulae", formulae.len());

        Ok(FetchResult {
            data: Some(formulae),
            etag,
            last_modified,
            not_modified: false,
        })
    }

    #[instrument(skip(self))]
    pub async fn fetch_casks_conditional(
        &self,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<FetchResult<Vec<Cask>>> {
        info!("Fetching casks from API with conditional headers");
        let mut request = self.client.get(CASK_API_URL);

        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        if let Some(last_modified) = last_modified {
            request = request.header("If-Modified-Since", last_modified);
        }

        let response = request.send().await?;

        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            info!("Casks not modified (304)");
            return Ok(FetchResult {
                data: None,
                etag: None,
                last_modified: None,
                not_modified: true,
            });
        }

        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let last_modified = response
            .headers()
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let body = response.bytes().await?;
        let casks: Vec<Cask> = serde_json::from_slice(&body)?;
        info!("Fetched {} casks", casks.len());

        Ok(FetchResult {
            data: Some(casks),
            etag,
            last_modified,
            not_modified: false,
        })
    }

    #[instrument(skip(self))]
    pub async fn fetch_cask_details(&self, cask_name: &str) -> Result<CaskDetails> {
        info!("Fetching details for cask: {}", cask_name);
        let url = format!("https://formulae.brew.sh/api/cask/{}.json", cask_name);
        let response = self.client.get(&url).send().await?;
        let cask: CaskDetails = response.json().await?;
        info!("Fetched details for cask: {}", cask_name);
        Ok(cask)
    }
}

impl Default for ApiClient {
    fn default() -> Self {
        Self::new()
    }
}
