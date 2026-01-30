use crate::api::Formula;
use crate::error::{Result, WaxError};
use crate::formula_parser::FormulaParser;
use crate::ui::dirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, info, instrument};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TapKind {
    GitHub { user: String, repo: String },
    Git { url: String },
    LocalDir { path: PathBuf },
    LocalFile { path: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tap {
    pub full_name: String,
    pub kind: TapKind,
    pub path: PathBuf,
}

impl Tap {
    pub fn from_spec(spec: &str) -> Result<Self> {
        let expanded = shellexpand::tilde(spec).to_string();
        let path = Path::new(&expanded);

        if path.exists() {
            if path.is_file() {
                if path.extension().and_then(|s| s.to_str()) == Some("rb") {
                    return Self::new_local_file(path);
                } else {
                    return Err(WaxError::TapError(
                        "Local file must have .rb extension".to_string(),
                    ));
                }
            } else if path.is_dir() {
                return Self::new_local_dir(path);
            }
        }

        if expanded.starts_with("http://")
            || expanded.starts_with("https://")
            || expanded.starts_with("git@")
        {
            return Self::new_git(&expanded);
        }

        let parts: Vec<&str> = spec.split('/').collect();
        if parts.len() == 2 && !spec.contains('.') && !spec.starts_with('/') {
            return Self::new_github(parts[0], parts[1]);
        }

        Err(WaxError::TapError(format!(
            "Invalid tap specification: {}. Use 'user/repo', a Git URL, or a local path",
            spec
        )))
    }

    pub fn new_github(user: &str, repo: &str) -> Result<Self> {
        let full_name = format!("{}/{}", user, repo);
        let path = Self::tap_directory()?
            .join(user)
            .join(format!("homebrew-{}", repo));

        Ok(Self {
            full_name,
            kind: TapKind::GitHub {
                user: user.to_string(),
                repo: repo.to_string(),
            },
            path,
        })
    }

    pub fn new_git(url: &str) -> Result<Self> {
        let name = Self::extract_name_from_url(url);
        let path = Self::tap_directory()?.join("custom").join(&name);

        Ok(Self {
            full_name: format!("custom/{}", name),
            kind: TapKind::Git {
                url: url.to_string(),
            },
            path,
        })
    }

    pub fn new_local_dir(dir: &Path) -> Result<Self> {
        let canonicalized = dunce::canonicalize(dir)?;
        let name = canonicalized
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| WaxError::TapError("Invalid directory path".to_string()))?;

        Ok(Self {
            full_name: format!("local/{}", name),
            kind: TapKind::LocalDir {
                path: canonicalized.clone(),
            },
            path: canonicalized,
        })
    }

    pub fn new_local_file(file: &Path) -> Result<Self> {
        let canonicalized = dunce::canonicalize(file)?;
        let name = canonicalized
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| WaxError::TapError("Invalid file path".to_string()))?;

        Ok(Self {
            full_name: format!("local/{}", name),
            kind: TapKind::LocalFile {
                path: canonicalized.clone(),
            },
            path: canonicalized,
        })
    }

    fn extract_name_from_url(url: &str) -> String {
        let path_part = url
            .trim_end_matches(".git")
            .split('/')
            .next_back()
            .unwrap_or("custom-tap");
        path_part.to_string()
    }

    fn tap_directory() -> Result<PathBuf> {
        if let Some(base_dirs) = directories::BaseDirs::new() {
            Ok(base_dirs.data_local_dir().join("wax").join("taps"))
        } else {
            Ok(dirs::home_dir()?.join(".wax").join("taps"))
        }
    }

    pub fn formula_dir(&self) -> PathBuf {
        match &self.kind {
            TapKind::LocalFile { .. } => self.path.parent().unwrap_or(&self.path).to_path_buf(),
            _ => {
                let formula_subdir = self.path.join("Formula");
                if formula_subdir.exists() {
                    formula_subdir
                } else {
                    self.path.clone()
                }
            }
        }
    }

    pub fn is_installed(&self) -> bool {
        match &self.kind {
            TapKind::LocalDir { path } | TapKind::LocalFile { path } => path.exists(),
            _ => self.path.exists(),
        }
    }

    pub fn url(&self) -> Option<String> {
        match &self.kind {
            TapKind::GitHub { user, repo } => {
                Some(format!("https://github.com/{}/homebrew-{}.git", user, repo))
            }
            TapKind::Git { url } => Some(url.clone()),
            TapKind::LocalDir { path } => Some(format!("file://{}", path.display())),
            TapKind::LocalFile { path } => Some(format!("file://{}", path.display())),
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self.kind, TapKind::GitHub { .. } | TapKind::Git { .. })
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
    pub async fn add_tap(&mut self, spec: &str) -> Result<()> {
        info!("Adding tap: {}", spec);

        let tap = Tap::from_spec(spec)?;

        if self.taps.contains_key(&tap.full_name) {
            return Err(WaxError::TapError(format!(
                "Tap {} is already added",
                tap.full_name
            )));
        }

        match &tap.kind {
            TapKind::GitHub { .. } | TapKind::Git { .. } => {
                if tap.path.exists() {
                    return Err(WaxError::TapError(format!(
                        "Tap directory {} already exists",
                        tap.path.display()
                    )));
                }
                fs::create_dir_all(tap.path.parent().unwrap()).await?;
                self.clone_tap(&tap).await?;
            }
            TapKind::LocalDir { path } => {
                if !path.exists() {
                    return Err(WaxError::TapError(format!(
                        "Local directory does not exist: {}",
                        path.display()
                    )));
                }
            }
            TapKind::LocalFile { path } => {
                if !path.exists() {
                    return Err(WaxError::TapError(format!(
                        "Local file does not exist: {}",
                        path.display()
                    )));
                }
            }
        }

        self.taps.insert(tap.full_name.clone(), tap);
        self.save().await?;

        Ok(())
    }

    #[instrument(skip(self))]
    async fn clone_tap(&self, tap: &Tap) -> Result<()> {
        let url = tap.url().ok_or_else(|| {
            WaxError::TapError("Cannot clone tap without a valid URL".to_string())
        })?;
        debug!("Cloning tap from {}", url);

        let output = tokio::process::Command::new("git")
            .arg("clone")
            .arg("--depth=1")
            .arg(&url)
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
    pub async fn remove_tap(&mut self, spec: &str) -> Result<()> {
        info!("Removing tap: {}", spec);

        let tap_to_remove = Tap::from_spec(spec)?;
        let full_name = &tap_to_remove.full_name;

        let tap = self
            .taps
            .get(full_name)
            .ok_or_else(|| WaxError::TapError(format!("Tap {} not found", full_name)))?
            .clone();

        match &tap.kind {
            TapKind::GitHub { .. } | TapKind::Git { .. } => {
                if tap.path.exists() {
                    fs::remove_dir_all(&tap.path).await?;
                }
            }
            TapKind::LocalDir { .. } | TapKind::LocalFile { .. } => {
            }
        }

        self.taps.remove(full_name);
        self.save().await?;

        Ok(())
    }

    pub fn list_taps(&self) -> Vec<&Tap> {
        self.taps.values().collect()
    }

    pub async fn has_tap(&self, tap_name: &str) -> bool {
        self.taps.contains_key(tap_name)
    }

    #[instrument(skip(self))]
    pub async fn update_tap(&mut self, spec: &str) -> Result<()> {
        info!("Updating tap: {}", spec);

        let tap_to_update = Tap::from_spec(spec)?;
        let full_name = &tap_to_update.full_name;

        let tap = self
            .taps
            .get(full_name)
            .ok_or_else(|| WaxError::TapError(format!("Tap {} not found", full_name)))?;

        match &tap.kind {
            TapKind::GitHub { .. } | TapKind::Git { .. } => {
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
            }
            TapKind::LocalDir { .. } | TapKind::LocalFile { .. } => {
                return Err(WaxError::TapError(
                    "Cannot update local taps. They are managed externally.".to_string(),
                ));
            }
        }

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn load_formulae_from_tap(&self, tap: &Tap) -> Result<Vec<Formula>> {
        debug!("Loading formulae from tap: {}", tap.full_name);

        match &tap.kind {
            TapKind::LocalFile { path } => {
                if !path.exists() {
                    return Ok(Vec::new());
                }

                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();

                let content = fs::read_to_string(path).await?;

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
                        Ok(vec![formula])
                    }
                    Err(e) => {
                        debug!("Failed to parse formula {}: {}", name, e);
                        Ok(Vec::new())
                    }
                }
            }
            _ => {
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
    }
}

impl Default for TapManager {
    fn default() -> Self {
        Self::new().expect("Failed to initialize TapManager")
    }
}
