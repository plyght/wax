//! Unified search across Homebrew index (existing), Scoop Main bucket listing,
//! Chocolatey web search, and cached winget-pkgs manifest discovery.

use crate::cache::Cache;
use crate::chocolatey;
use crate::error::Result;
use crate::package_spec::Ecosystem;
use console::style;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tracing::debug;

const SCOOP_INDEX_CACHE: &str = "scoop_main_index.json";
const WINGET_INDEX_CACHE: &str = "winget_pkgs_index.json";
const SCOOP_INDEX_MAX_AGE_SECS: i64 = 86_400;
const WINGET_INDEX_MAX_AGE_SECS: i64 = 86_400;

#[derive(Serialize, Deserialize)]
struct ScoopIndexFile {
    fetched_unix: i64,
    names: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct WingetIndexFile {
    fetched_unix: i64,
    ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RemoteHit {
    pub ecosystem: Ecosystem,
    pub id: String,
    pub blurb: Option<String>,
    pub score: i32,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn match_score_token(name: &str, query: &str) -> Option<i32> {
    let q = query.to_lowercase();
    let n = name.to_lowercase();
    if n == q {
        return Some(1000);
    }
    if n.starts_with(&q) {
        return Some(900);
    }
    if n.contains(&q) {
        return Some(850);
    }
    None
}

async fn load_or_fetch_scoop_index(cache_dir: &std::path::Path) -> Result<Vec<String>> {
    let path: PathBuf = cache_dir.join(SCOOP_INDEX_CACHE);
    if let Ok(text) = tokio::fs::read_to_string(&path).await {
        if let Ok(idx) = serde_json::from_str::<ScoopIndexFile>(&text) {
            if now_unix() - idx.fetched_unix < SCOOP_INDEX_MAX_AGE_SECS {
                return Ok(idx.names);
            }
        }
    }

    debug!("Refreshing Scoop Main bucket index via GitHub API…");
    let client = crate::http_client::default_client();

    let resp = client
        .get("https://api.github.com/repos/ScoopInstaller/Main/git/trees/master?recursive=1")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(crate::error::WaxError::InstallError(format!(
            "Scoop index GitHub API: HTTP {}",
            resp.status()
        )));
    }
    let v: serde_json::Value = resp.json().await?;
    let tree = v
        .get("tree")
        .and_then(|t| t.as_array())
        .ok_or_else(|| crate::error::WaxError::ParseError("no tree array".into()))?;

    let mut names = Vec::new();
    for item in tree {
        let Some(path) = item.get("path").and_then(|p| p.as_str()) else {
            continue;
        };
        if let Some(rest) = path.strip_prefix("bucket/") {
            if let Some(stem) = rest.strip_suffix(".json") {
                if !stem.contains('/') {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort();
    names.dedup();

    let idx = ScoopIndexFile {
        fetched_unix: now_unix(),
        names: names.clone(),
    };
    if let Ok(encoded) = serde_json::to_string_pretty(&idx) {
        let _ = tokio::fs::write(&path, encoded).await;
    }

    Ok(names)
}

fn package_id_from_winget_manifest_path(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    let mpos = parts.iter().position(|p| *p == "manifests")?;
    if parts.len() < mpos + 4 {
        return None;
    }
    let tail = &parts[mpos + 2..];
    if tail.len() < 3 {
        return None;
    }
    let file = tail.last()?;
    if !file.ends_with(".installer.yaml") {
        return None;
    }
    let version_dir = tail.get(tail.len().saturating_sub(2))?;
    if !looks_like_version_folder(version_dir) {
        return None;
    }
    let id_parts = &tail[..tail.len() - 2];
    if id_parts.is_empty() {
        return None;
    }
    Some(id_parts.join("."))
}

fn looks_like_version_folder(s: &str) -> bool {
    let b = s.as_bytes();
    b.first().map(|c| c.is_ascii_digit()).unwrap_or(false)
}

async fn run_git(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command
        .output()
        .await
        .map_err(|e| crate::error::WaxError::InstallError(format!("git failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("exit status {}", output.status)
        } else {
            stderr
        };
        return Err(crate::error::WaxError::InstallError(format!(
            "git {} failed: {detail}",
            args.join(" ")
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn refresh_winget_index_from_git(cache_dir: &Path) -> Result<Vec<String>> {
    let repo_dir = cache_dir.join("winget-pkgs.git");
    let repo = repo_dir.to_string_lossy().to_string();
    if !repo_dir.join("HEAD").exists() {
        if let Some(parent) = repo_dir.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        debug!("Cloning winget-pkgs metadata into cache...");
        run_git(
            &[
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                "--depth=1",
                "https://github.com/microsoft/winget-pkgs.git",
                &repo,
            ],
            None,
        )
        .await?;
    } else {
        debug!("Refreshing cached winget-pkgs metadata...");
        run_git(&["fetch", "--depth=1", "origin", "master"], Some(&repo_dir)).await?;
    }

    let stdout = run_git(
        &["ls-tree", "-r", "--name-only", "origin/master", "manifests"],
        Some(&repo_dir),
    )
    .await?;
    let mut ids: Vec<String> = stdout
        .lines()
        .filter_map(package_id_from_winget_manifest_path)
        .collect();
    ids.sort_by_key(|id| id.to_ascii_lowercase());
    ids.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    Ok(ids)
}

async fn load_or_fetch_winget_index(cache_dir: &Path) -> Result<Vec<String>> {
    let path: PathBuf = cache_dir.join(WINGET_INDEX_CACHE);
    let stale = tokio::fs::read_to_string(&path)
        .await
        .ok()
        .and_then(|text| serde_json::from_str::<WingetIndexFile>(&text).ok());
    if let Some(idx) = &stale {
        if now_unix() - idx.fetched_unix < WINGET_INDEX_MAX_AGE_SECS {
            return Ok(idx.ids.clone());
        }
    }

    let ids = match refresh_winget_index_from_git(cache_dir).await {
        Ok(ids) => ids,
        Err(err) => {
            if let Some(idx) = stale {
                debug!("Using stale winget-pkgs index after refresh failed: {err}");
                return Ok(idx.ids);
            }
            return Err(err);
        }
    };

    let idx = WingetIndexFile {
        fetched_unix: now_unix(),
        ids: ids.clone(),
    };
    if let Ok(encoded) = serde_json::to_string_pretty(&idx) {
        let _ = tokio::fs::write(&path, encoded).await;
    }
    Ok(ids)
}

async fn search_winget_index(
    cache_dir: &std::path::Path,
    query: &str,
    limit: usize,
) -> Result<Vec<RemoteHit>> {
    let index = load_or_fetch_winget_index(cache_dir).await?;
    let mut out = Vec::new();
    for id in index {
        if let Some(score) = match_score_token(&id, query) {
            out.push(RemoteHit {
                ecosystem: Ecosystem::Winget,
                id,
                blurb: None,
                score,
            });
        }
    }
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    out.truncate(limit);
    Ok(out)
}

pub async fn collect_remote_hits(
    cache: &Cache,
    query: &str,
    include_scoop: bool,
    include_choco: bool,
    include_winget: bool,
) -> Result<Vec<RemoteHit>> {
    let mut hits = Vec::new();
    let q = query.trim();
    if q.is_empty() {
        return Ok(hits);
    }

    if include_scoop {
        let index = load_or_fetch_scoop_index(cache.cache_dir_path()).await?;
        for name in index {
            if let Some(s) = match_score_token(&name, q) {
                hits.push(RemoteHit {
                    ecosystem: Ecosystem::Scoop,
                    id: name.clone(),
                    blurb: None,
                    score: s,
                });
            }
        }
    }

    if include_choco {
        let ids = chocolatey::search_package_ids(q, 25).await?;
        for id in ids {
            let s = match_score_token(&id, q).unwrap_or(700);
            hits.push(RemoteHit {
                ecosystem: Ecosystem::Chocolatey,
                id,
                blurb: None,
                score: s,
            });
        }
    }

    if include_winget {
        hits.extend(search_winget_index(cache.cache_dir_path(), q, 15).await?);
    }

    Ok(hits)
}

/// Deduplicate by lowercase id; keep the hit from the fastest ecosystem (lowest speed_rank), then highest score.
pub fn dedupe_remote_by_speed(mut hits: Vec<RemoteHit>) -> Vec<RemoteHit> {
    let mut best: HashMap<String, RemoteHit> = HashMap::new();
    for h in hits.drain(..) {
        let key = h.id.to_lowercase();
        let replace = match best.get(&key) {
            None => true,
            Some(prev) => {
                let r_new = h.ecosystem.speed_rank();
                let r_old = prev.ecosystem.speed_rank();
                r_new < r_old || (r_new == r_old && h.score > prev.score)
            }
        };
        if replace {
            best.insert(key, h);
        }
    }
    let mut v: Vec<_> = best.into_values().collect();
    v.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.ecosystem.speed_rank().cmp(&b.ecosystem.speed_rank()))
            .then_with(|| a.id.cmp(&b.id))
    });
    v
}

pub fn print_remote_hits(hits: &[RemoteHit]) {
    if hits.is_empty() {
        return;
    }
    println!();
    println!("{}", style("Other catalogues (Windows-oriented)").bold());
    for h in hits {
        let tag = match h.ecosystem {
            Ecosystem::Scoop => style("scoop").cyan(),
            Ecosystem::Winget => style("winget").green(),
            Ecosystem::Chocolatey => style("choco").yellow(),
            Ecosystem::Brew => style("brew").magenta(),
        };
        let hint = format!("{}/{}", h.ecosystem.label(), h.id);
        println!(
            "{} {} · {}",
            tag,
            style(&h.id).magenta(),
            style(format!("wax install {hint}")).dim()
        );
        if let Some(b) = &h.blurb {
            println!("  {}", style(b).dim());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(eco: Ecosystem, id: &str, score: i32) -> RemoteHit {
        RemoteHit {
            ecosystem: eco,
            id: id.to_string(),
            blurb: None,
            score,
        }
    }

    #[test]
    fn dedupe_prefers_faster_ecosystem_when_ids_collide() {
        let hits = vec![
            hit(Ecosystem::Chocolatey, "git", 1000),
            hit(Ecosystem::Scoop, "git", 500),
        ];
        let d = dedupe_remote_by_speed(hits);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].ecosystem, Ecosystem::Scoop);
    }

    #[test]
    fn dedupe_same_ecosystem_keeps_higher_score() {
        let hits = vec![
            hit(Ecosystem::Scoop, "git", 500),
            hit(Ecosystem::Scoop, "git", 900),
        ];
        let d = dedupe_remote_by_speed(hits);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].score, 900);
    }

    #[test]
    fn dedupe_case_folds_ids() {
        let hits = vec![
            hit(Ecosystem::Chocolatey, "Foo", 900),
            hit(Ecosystem::Scoop, "foo", 800),
        ];
        let d = dedupe_remote_by_speed(hits);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].ecosystem, Ecosystem::Scoop);
    }

    #[test]
    fn dedupe_winget_beats_chocolatey_on_tie_id() {
        let hits = vec![
            hit(Ecosystem::Chocolatey, "pkg", 1000),
            hit(Ecosystem::Winget, "pkg", 1000),
        ];
        let d = dedupe_remote_by_speed(hits);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].ecosystem, Ecosystem::Winget);
    }

    #[test]
    fn winget_manifest_path_yields_package_id() {
        let path = "manifests/j/JesseDuffield/lazygit/0.55.1/JesseDuffield.lazygit.installer.yaml";
        assert_eq!(
            package_id_from_winget_manifest_path(path).as_deref(),
            Some("JesseDuffield.lazygit")
        );
    }

    #[test]
    fn winget_manifest_path_rejects_non_installer_yaml() {
        let path =
            "manifests/j/JesseDuffield/lazygit/0.55.1/JesseDuffield.lazygit.locale.en-US.yaml";
        assert_eq!(package_id_from_winget_manifest_path(path), None);
    }
}
