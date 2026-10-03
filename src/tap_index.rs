//! Incremental indexes for custom taps. The Homebrew API remains the source of
//! core/cask metadata; this only caches the Ruby parser's existing contract.
use crate::api::{Cask, Formula};
use crate::digest::sha256_digest_hex;
use crate::error::{Result, WaxError};
use crate::formula_parser::FormulaParser;
use crate::tap::{Tap, TapKind, TapManager};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio::fs;

// Bump whenever the Ruby parser or serialized index contract changes.
const INDEX_VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum FileKind {
    Formula,
    Cask,
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    kind: FileKind,
    sha256: String,
    formula: Option<Formula>,
    cask: Option<Cask>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct TapIndex {
    version: u32,
    tap_name: String,
    tap_path: PathBuf,
    indexed_commit: Option<String>,
    entries: BTreeMap<PathBuf, Entry>,
}

struct Input {
    kind: FileKind,
    content: String,
    sha256: String,
}

pub(crate) struct TapIndexStore {
    index_path: PathBuf,
    lock_path: PathBuf,
}

impl TapIndexStore {
    pub(crate) fn new(cache_dir: &Path, tap_name: &str) -> Self {
        // Unlike replacing '/' with '-', this cannot alias two tap names.
        let key = sha256_digest_hex(tap_name);
        Self {
            index_path: cache_dir.join("taps/index-v1").join(format!("{key}.json")),
            // Keep lock inodes outside directories that cache invalidation removes.
            lock_path: cache_dir.join("tap-locks").join(format!("{key}.lock")),
        }
    }

    pub(crate) async fn lock(&self) -> Result<std::fs::File> {
        let path = self.lock_path.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path)?;
            file.lock()?;
            Ok(file)
        })
        .await
        .map_err(|e| WaxError::CacheError(format!("tap lock task: {e}")))?
    }

    pub(crate) async fn invalidate(&self) -> Result<()> {
        let _lock = self.lock().await?;
        match fs::remove_file(&self.index_path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) async fn load(&self, tap: &Tap) -> Result<Option<TapIndex>> {
        match fs::read(&self.index_path).await {
            Ok(bytes) => Ok(serde_json::from_slice::<TapIndex>(&bytes)
                .ok()
                .filter(|index| {
                    index.version == INDEX_VERSION
                        && index.tap_name == tap.full_name
                        && index.tap_path == tap.path
                })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // Caller holds the same lock over any git fetch/reset and this refresh.
    pub(crate) async fn refresh(&self, tap: &Tap) -> Result<TapIndex> {
        let previous = self.load(tap).await?;

        // Other programs can edit local taps without our lock. Publish only a
        // stable view, retrying boundedly if the source changes while indexing.
        for _ in 0..3 {
            let commit = indexed_commit(tap).await?;
            let inputs = read_inputs(tap).await?;
            let mut entries = BTreeMap::new();
            let mut changed = previous.is_none();
            for (path, input) in &inputs {
                let old = previous.as_ref().and_then(|index| index.entries.get(path));
                let entry = match old {
                    Some(entry) if entry.kind == input.kind && entry.sha256 == input.sha256 => {
                        entry.clone()
                    }
                    _ => {
                        changed = true;
                        parse_entry(tap, path, input)
                    }
                };
                entries.insert(path.clone(), entry);
            }
            if inputs_signature(&inputs) != inputs_signature(&read_inputs(tap).await?)
                || commit != indexed_commit(tap).await?
            {
                continue;
            }
            changed |= previous.as_ref().is_some_and(|index| {
                index.indexed_commit != commit || index.entries.len() != entries.len()
            });
            let index = TapIndex {
                version: INDEX_VERSION,
                tap_name: tap.full_name.clone(),
                tap_path: tap.path.clone(),
                indexed_commit: commit,
                entries,
            };
            if changed {
                self.save(&index).await?;
            }
            return Ok(index);
        }
        Err(WaxError::CacheError(format!(
            "Tap {} changed while indexing; previous cache retained",
            tap.full_name
        )))
    }

    async fn save(&self, index: &TapIndex) -> Result<()> {
        let bytes = serde_json::to_vec(index)?;
        let path = self.index_path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let parent = path.parent().unwrap();
            std::fs::create_dir_all(parent)?;
            let mut staged = tempfile::NamedTempFile::new_in(parent)?;
            staged.write_all(&bytes)?;
            staged.as_file().sync_all()?;
            staged.persist(path).map_err(|e| e.error)?;
            Ok(())
        })
        .await
        .map_err(|e| WaxError::CacheError(format!("tap index task: {e}")))?
    }
}

impl TapIndex {
    pub(crate) fn signature(&self) -> Result<String> {
        Ok(sha256_digest_hex(serde_json::to_vec(self)?))
    }

    pub(crate) fn formulae(&self) -> Vec<Formula> {
        self.entries
            .iter()
            .filter_map(|(path, entry)| {
                entry.formula.clone().map(|mut formula| {
                    formula.rb_path = Some(path.clone());
                    formula
                })
            })
            .collect()
    }

    pub(crate) fn casks(&self) -> Vec<Cask> {
        self.entries
            .iter()
            .filter_map(|(path, entry)| {
                entry.cask.clone().map(|mut cask| {
                    cask.rb_path = Some(path.clone());
                    cask
                })
            })
            .collect()
    }
}

async fn indexed_commit(tap: &Tap) -> Result<Option<String>> {
    if !matches!(tap.kind, TapKind::GitHub { .. } | TapKind::Git { .. }) {
        return Ok(None);
    }
    let output = tokio::process::Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(&tap.path)
        .output()
        .await?;
    if !output.status.success() {
        return Err(WaxError::TapError(format!(
            "Cannot read HEAD for tap {}: {}",
            tap.full_name,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

async fn read_inputs(tap: &Tap) -> Result<BTreeMap<PathBuf, Input>> {
    if !tap.path.exists() {
        return Err(WaxError::TapError(format!(
            "Tap path does not exist: {}",
            tap.path.display()
        )));
    }
    let mut paths = Vec::new();
    if let TapKind::LocalFile { path } = &tap.kind {
        paths.push((path.clone(), FileKind::Formula));
    } else {
        // Match the current loaders: immediate .rb children only. In particular,
        // do not pretend this is a recursive index for official core/cask taps.
        for (dir, kind) in [
            (tap.formula_dir(), FileKind::Formula),
            (tap.cask_dir(), FileKind::Cask),
        ] {
            let mut children = match fs::read_dir(dir).await {
                Ok(children) => children,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            while let Some(child) = children.next_entry().await? {
                let path = child.path();
                if path.extension().and_then(|s| s.to_str()) == Some("rb") {
                    paths.push((path, kind));
                }
            }
        }
    }
    let mut inputs = BTreeMap::new();
    for (path, kind) in paths {
        let content = fs::read_to_string(&path).await?;
        let sha256 = sha256_digest_hex(&content);
        inputs.insert(
            path,
            Input {
                kind,
                content,
                sha256,
            },
        );
    }
    Ok(inputs)
}

fn inputs_signature(inputs: &BTreeMap<PathBuf, Input>) -> Vec<(&Path, FileKind, &str)> {
    inputs
        .iter()
        .map(|(path, input)| (path.as_path(), input.kind, input.sha256.as_str()))
        .collect()
}

fn parse_entry(tap: &Tap, path: &Path, input: &Input) -> Entry {
    let is_cask = FormulaParser::is_homebrew_cask_rb(&input.content);
    let formula = if input.kind == FileKind::Formula
        && (!is_cask || matches!(tap.kind, TapKind::LocalFile { .. }))
    {
        TapManager::parse_formula_content(path, &tap.full_name, &input.content).ok()
    } else {
        None
    };
    let cask = if input.kind == FileKind::Cask && is_cask {
        let token = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        FormulaParser::parse_ruby_cask(token, &tap.full_name, &input.content).ok()
    } else {
        None
    };
    Entry {
        kind: input.kind,
        sha256: input.sha256.clone(),
        formula,
        cask,
    }
}

#[cfg(test)]
mod tests;
