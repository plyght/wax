use crate::api::Formula;
use crate::tap::TapManager;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

fn split_qualified(name: &str) -> Option<(String, &str)> {
    let mut parts = name.splitn(3, '/');
    let (owner, repo, formula) = (parts.next()?, parts.next()?, parts.next()?);
    if owner.is_empty() || repo.is_empty() || formula.is_empty() || formula.contains('/') {
        return None;
    }
    let repo = repo.strip_prefix("homebrew-").unwrap_or(repo);
    Some((format!("{owner}/{repo}"), formula))
}

pub(super) fn missing_dependency_taps(
    formulae: &[Formula],
    roots: &[String],
    tap_manager: &TapManager,
) -> BTreeSet<String> {
    let by_name: HashMap<&str, &Formula> = formulae
        .iter()
        .flat_map(|f| [(f.full_name.as_str(), f), (f.name.as_str(), f)])
        .collect();
    let tapped: HashSet<&str> = tap_manager
        .list_taps()
        .into_iter()
        .map(|t| t.full_name.as_str())
        .collect();
    let mut missing = BTreeSet::new();
    let mut seen = HashSet::new();
    let mut queue: Vec<&str> = roots.iter().map(String::as_str).collect();
    while let Some(name) = queue.pop() {
        if !seen.insert(name) {
            continue;
        }
        let Some(formula) = by_name.get(name) else {
            if let Some((tap, _)) = split_qualified(name) {
                if !tapped.contains(tap.as_str()) && tap != "homebrew/core" {
                    missing.insert(tap);
                }
            }
            continue;
        };
        for dep in formula.dependencies.iter().flatten() {
            queue.push(dep.as_str());
        }
    }
    missing
}

fn read_migrations(tap_path: &Path) -> HashMap<String, String> {
    std::fs::read(tap_path.join("tap_migrations.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(super) fn migrated_name(
    name: &str,
    tap_manager: &TapManager,
    formula_exists: impl Fn(&str) -> bool,
) -> Option<String> {
    let (tap, formula) = split_qualified(name)?;
    if formula_exists(name) {
        return None;
    }
    let tap_path = tap_manager.get_tap(&tap).ok()?.path;
    let target = read_migrations(&tap_path).remove(formula)?;
    match target.as_str() {
        "homebrew/core" | "homebrew/cask" => Some(formula.to_string()),
        other if other.matches('/').count() == 1 => Some(format!("{other}/{formula}")),
        other if split_qualified(other).is_some() => Some(other.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tap::{Tap, TapKind};

    fn formula(full_name: &str, deps: &[&str]) -> Formula {
        let mut f: Formula = serde_json::from_value(serde_json::json!({
            "name": full_name.rsplit('/').next().unwrap(),
            "full_name": full_name,
            "homepage": "",
            "versions": {"stable": "1.0", "bottle": false},
        }))
        .unwrap();
        f.dependencies = Some(deps.iter().map(|d| d.to_string()).collect());
        f
    }

    fn manager_with(tap_name: &str, path: &Path) -> TapManager {
        TapManager::for_test(
            Tap {
                full_name: tap_name.into(),
                kind: TapKind::LocalDir {
                    path: path.to_path_buf(),
                },
                path: path.to_path_buf(),
            },
            path.join("taps.json"),
        )
    }

    #[test]
    fn finds_untapped_dependency_taps_transitively() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = manager_with("tursodatabase/tap", tmp.path());
        let formulae = vec![
            formula("tursodatabase/tap/turso", &["libsql/sqld/sqld", "ripgrep"]),
            formula("ripgrep", &["pcre2"]),
            formula("pcre2", &[]),
        ];
        let missing =
            missing_dependency_taps(&formulae, &["tursodatabase/tap/turso".into()], &manager);
        assert_eq!(missing.into_iter().collect::<Vec<_>>(), vec!["libsql/sqld"]);
    }

    #[test]
    fn follows_tap_migrations() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("tap_migrations.json"),
            r#"{"mongosh": "homebrew/core", "tool": "other/tap", "renamed": "other/tap/newname"}"#,
        )
        .unwrap();
        let manager = manager_with("mongodb/brew", tmp.path());
        let none = |_: &str| false;
        assert_eq!(
            migrated_name("mongodb/brew/mongosh", &manager, none).as_deref(),
            Some("mongosh")
        );
        assert_eq!(
            migrated_name("mongodb/homebrew-brew/tool", &manager, none).as_deref(),
            Some("other/tap/tool")
        );
        assert_eq!(
            migrated_name("mongodb/brew/renamed", &manager, none).as_deref(),
            Some("other/tap/newname")
        );
        assert_eq!(migrated_name("mongodb/brew/absent", &manager, none), None);
        assert_eq!(
            migrated_name("mongodb/brew/mongosh", &manager, |_| true),
            None
        );
    }
}
