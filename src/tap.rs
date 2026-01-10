use crate::api::Formula;
use crate::error::{Result, WaxError};
use crate::formula_parser::FormulaParser;
use crate::ui::dirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;
use tracing::{debug, info, instrument};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tap {
    pub user: String,
    pub repo: String,
    pub full_name: String,
    pub url: String,
    pub path: PathBuf,
}

impl Tap {
    pub fn new(user: &str, repo: &str) -> Result<Self> {
        let full_name = format!("{}/{}", user, repo);
        let url = format!("https://github.com/{}/homebrew-{}.git", user, repo);
        let path = Self::tap_directory()?
            .join(user)
            .join(format!("homebrew-{}", repo));

        Ok(Self {
            user: user.to_string(),
            repo: repo.to_string(),
            full_name,
            url,
            path,
        })
    }

    fn tap_directory() -> Result<PathBuf> {
        if let Some(base_dirs) = directories::BaseDirs::new() {
            Ok(base_dirs.data_local_dir().join("wax").join("taps"))
        } else {
            Ok(dirs::home_dir()?.join(".wax").join("taps"))
        }
    }

    pub fn formula_dir(&self) -> PathBuf {
        let formula_subdir = self.path.join("Formula");
        if formula_subdir.exists() {
            formula_subdir
        } else {
            self.path.clone()
        }
    }

    pub fn is_installed(&self) -> bool {
        self.path.exists()
    }
}

pub struct TapManager {
    taps: HashMap<String, Tap>,
    state_path: PathBuf,
}

impl TapManager {
    pub fn new() -> Result<Self> {
        let state_path = if let Some(base_dirs) = directories::BaseDirs::new() {
            base_dirs.data_local_dir().join("wax").join("taps.json")
        } else {
            dirs::home_dir()?.join(".wax").join("taps.json")
        };

        Ok(Self {
            taps: HashMap::new(),
            state_path,
        })
    }

    pub async fn load(&mut self) -> Result<()> {
        if !self.state_path.exists() {
            return Ok(());
        }

        let json = fs::read_to_string(&self.state_path).await?;
        self.taps = serde_json::from_str(&json)?;
        Ok(())
    }

    pub async fn save(&self) -> Result<()> {
        let parent = self
            .state_path
            .parent()
            .ok_or_else(|| WaxError::CacheError("Cannot determine parent directory".into()))?;
        fs::create_dir_all(parent).await?;

        let json = serde_json::to_string_pretty(&self.taps)?;
        fs::write(&self.state_path, json).await?;
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn add_tap(&mut self, user: &str, repo: &str) -> Result<()> {
        info!("Adding tap: {}/{}", user, repo);

        let tap = Tap::new(user, repo)?;

        if tap.is_installed() {
            return Err(WaxError::TapError(format!(
                "Tap {} is already installed",
                tap.full_name
            )));
        }

        fs::create_dir_all(tap.path.parent().unwrap()).await?;

        self.clone_tap(&tap).await?;

        self.taps.insert(tap.full_name.clone(), tap);
        self.save().await?;

        Ok(())
    }

    #[instrument(skip(self))]
    async fn clone_tap(&self, tap: &Tap) -> Result<()> {
        debug!("Cloning tap from {}", tap.url);

        let output = tokio::process::Command::new("git")
            .arg("clone")
            .arg("--depth=1")
            .arg(&tap.url)
            .arg(&tap.path)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(WaxError::TapError(format!(
                "Failed to clone tap: {}",
                stderr
            )));
        }

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn remove_tap(&mut self, user: &str, repo: &str) -> Result<()> {
        info!("Removing tap: {}/{}", user, repo);

        let full_name = format!("{}/{}", user, repo);
        let tap = self
            .taps
            .get(&full_name)
            .ok_or_else(|| WaxError::TapError(format!("Tap {} not found", full_name)))?;

        if tap.path.exists() {
            fs::remove_dir_all(&tap.path).await?;
        }

        self.taps.remove(&full_name);
        self.save().await?;

        Ok(())
    }

    pub fn list_taps(&self) -> Vec<&Tap> {
        self.taps.values().collect()
    }

    #[instrument(skip(self))]
    pub async fn update_tap(&mut self, user: &str, repo: &str) -> Result<()> {
        info!("Updating tap: {}/{}", user, repo);

        let full_name = format!("{}/{}", user, repo);
        let tap = self
            .taps
            .get(&full_name)
            .ok_or_else(|| WaxError::TapError(format!("Tap {} not found", full_name)))?;

        let output = tokio::process::Command::new("git")
            .arg("pull")
            .current_dir(&tap.path)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(WaxError::TapError(format!(
                "Failed to update tap: {}",
                stderr
            )));
        }

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn load_formulae_from_tap(&self, tap: &Tap) -> Result<Vec<Formula>> {
        debug!("Loading formulae from tap: {}", tap.full_name);

        let formula_dir = tap.formula_dir();
        if !formula_dir.exists() {
            return Ok(Vec::new());
        }

        let mut formulae = Vec::new();
        let mut entries = fs::read_dir(&formula_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("rb") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();

                let content = fs::read_to_string(&path).await?;

                match FormulaParser::parse_ruby_formula(&name, &content) {
                    Ok(parsed) => {
                        let formula = Formula {
                            name: parsed.name.clone(),
                            full_name: format!("{}/{}", tap.full_name, parsed.name),
                            desc: parsed.desc.clone(),
                            homepage: parsed.homepage.clone().unwrap_or_default(),
                            versions: crate::api::Versions {
                                stable: parsed.source.version.clone(),
                                bottle: false,
                            },
                            installed: None,
                            dependencies: Some(parsed.runtime_dependencies.clone()),
                            build_dependencies: Some(parsed.build_dependencies.clone()),
                            bottle: None,
                        };
                        formulae.push(formula);
                    }
                    Err(e) => {
                        debug!("Failed to parse formula {}: {}", name, e);
                    }
                }
            }
        }

        Ok(formulae)
    }
}

impl Default for TapManager {
    fn default() -> Self {
        Self::new().expect("Failed to initialize TapManager")
    }
}
