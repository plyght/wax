use crate::bottle::homebrew_prefix;
use crate::cache::Cache;
use crate::cask::CaskState;
use crate::commands::upgrade::{get_outdated_packages, upgrade as run_upgrade};
use crate::discovery::discover_manually_installed_casks;
use crate::error::{Result, WaxError};
use crate::install::InstallState;
use console::style;
use inquire::{Confirm, Select};
use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use tracing::instrument;

/// When set (tests only), treat this path as the Cellar root (`<Cellar>/<formula>/<version>/`)
/// and do not merge in casks from the system, so `wax list` output is deterministic.
const WAX_TEST_CELLAR_ENV: &str = "WAX_TEST_CELLAR";

#[derive(Clone)]
struct InstalledRow {
    name: String,
    line: String,
    is_cask: bool,
}

impl std::fmt::Display for InstalledRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.line)
    }
}

async fn collect_installed_rows(cache: &Cache) -> Result<Vec<InstalledRow>> {
    let test_cellar = std::env::var_os(WAX_TEST_CELLAR_ENV);

    let (cellar_path, skip_casks) = if let Some(ref raw) = test_cellar {
        (PathBuf::from(raw), true)
    } else {
        let candidates = [
            homebrew_prefix().join("Cellar"),
            crate::ui::dirs::home_dir()
                .unwrap_or_else(|_| homebrew_prefix())
                .join(".local/wax/Cellar"),
        ];
        let cellar_path = candidates
            .iter()
            .find(|p| p.exists())
            .cloned()
            .unwrap_or_else(|| homebrew_prefix().join("Cellar"));
        (cellar_path, false)
    };

    let cask_state = CaskState::new()?;
    let installed_casks: HashMap<_, _> = if skip_casks {
        HashMap::new()
    } else {
        cask_state.load().await?
    };

    // External cask discovery is handled by sync/lock commands
    // which save discovered casks to CaskState for persistence.
    // List here only shows what's in CaskState.

    let install_state = InstallState::new()?;
    let installed_packages = install_state.load().await?;

    let mut rows = Vec::new();

    if cellar_path.exists() {
        let mut entries = tokio::fs::read_dir(&cellar_path).await?;

        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                let package_name = entry.file_name().to_string_lossy().to_string();

                let mut versions = Vec::new();
                let mut version_entries = tokio::fs::read_dir(entry.path()).await?;
                while let Some(version_entry) = version_entries.next_entry().await? {
                    if version_entry.file_type().await?.is_dir() {
                        versions.push(version_entry.file_name().to_string_lossy().to_string());
                    }
                }

                let from_source = installed_packages
                    .get(&package_name)
                    .map(|p| p.from_source)
                    .unwrap_or(false);

                let version_str = versions.join(", ");
                let line = if from_source {
                    format!(
                        "{} {} {}",
                        style(&package_name).magenta(),
                        style(&version_str).dim(),
                        style("(source)").yellow()
                    )
                } else {
                    format!(
                        "{} {}",
                        style(&package_name).magenta(),
                        style(&version_str).dim()
                    )
                };

                rows.push(InstalledRow {
                    name: package_name,
                    line,
                    is_cask: false,
                });
            }
        }
    }

    let mut cask_list: Vec<_> = installed_casks.iter().collect();
    cask_list.sort_by_key(|(name, _)| *name);

    for (cask_name, cask) in cask_list {
        let line = format!(
            "{} {} {}",
            style(cask_name.as_str()).magenta(),
            style(&cask.version).dim(),
            style("(cask)").yellow()
        );
        rows.push(InstalledRow {
            name: cask_name.clone(),
            line,
            is_cask: true,
        });
    }

    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

fn matches_query(row: &InstalledRow, query: &str) -> bool {
    let q = query.to_lowercase();
    if q.is_empty() {
        return true;
    }
    row.name.to_lowercase().contains(&q) || row.line.to_lowercase().contains(&q)
}

fn print_table(rows: &[InstalledRow]) {
    if rows.is_empty() {
        return;
    }
    println!();
    for row in rows {
        println!("{}", row.line);
    }
}

fn summarize_counts(rows: &[InstalledRow]) -> (usize, usize) {
    let fc = rows.iter().filter(|r| !r.is_cask).count();
    let cc = rows.iter().filter(|r| r.is_cask).count();
    (fc, cc)
}

fn print_summary(total: usize, formula_count: usize, cask_count: usize) {
    let parts: Vec<String> = [
        if formula_count == 0 {
            None
        } else {
            Some(format!(
                "{} {}",
                formula_count,
                if formula_count == 1 {
                    "formula"
                } else {
                    "formulae"
                }
            ))
        },
        if cask_count == 0 {
            None
        } else {
            Some(format!(
                "{} {}",
                cask_count,
                if cask_count == 1 { "cask" } else { "casks" }
            ))
        },
    ]
    .into_iter()
    .flatten()
    .collect();

    println!(
        "\n{} {} installed ({})",
        style(total).cyan(),
        if total == 1 { "package" } else { "packages" },
        parts.join(", ")
    );
}

fn map_inquire_err(e: inquire::error::InquireError) -> WaxError {
    WaxError::InvalidInput(e.to_string())
}

async fn offer_upgrade_for_selection(cache: &Cache, choice: &InstalledRow) -> Result<()> {
    cache.ensure_fresh().await?;

    let state = InstallState::new()?;
    let installed_packages = state.load().await?;
    if let Some(pkg) = installed_packages.get(&choice.name) {
        if pkg.pinned {
            println!(
                "{} is pinned — run `wax unpin {}` before upgrading.",
                style(&choice.name).magenta(),
                choice.name
            );
            return Ok(());
        }
    }

    let outdated = get_outdated_packages(cache).await?;
    let Some(pkg) = outdated.iter().find(|p| p.name == choice.name) else {
        println!(
            "{} is already on the latest version.",
            style(&choice.name).magenta()
        );
        return Ok(());
    };

    let cask_note = if pkg.is_cask {
        format!(" {}", style("(cask)").yellow())
    } else {
        String::new()
    };

    let prompt = format!(
        "Upgrade {}{} from {} → {}?",
        choice.name, cask_note, pkg.installed_version, pkg.latest_version
    );

    let should_upgrade = Confirm::new(prompt.as_str())
        .with_default(true)
        .prompt_skippable()
        .map_err(map_inquire_err)?
        .unwrap_or(false);

    if should_upgrade {
        run_upgrade(cache, &[choice.name.clone()], false).await?;
        println!(
            "\n{} {}",
            style("✓").green(),
            style(format!("{} upgraded", choice.name)).magenta()
        );
    }

    Ok(())
}

async fn run_interactive_list(cache: &Cache, initial_query: Option<String>) -> Result<()> {
    let mut first_prompt = true;

    loop {
        let rows = collect_installed_rows(cache).await?;
        if rows.is_empty() {
            println!("no packages installed");
            return Ok(());
        }

        let page = std::cmp::min(12, rows.len()).max(1);
        let mut select = Select::new(
            "Installed packages — type to filter, ↑↓ move, Enter to select, Esc to exit",
            rows,
        )
        .with_page_size(page)
        .with_help_message(
            "Choose a package, then confirm to upgrade to the latest version when an update exists",
        );

        if first_prompt {
            if let Some(ref q) = initial_query {
                if !q.is_empty() {
                    select = select.with_starting_filter_input(q);
                }
            }
            first_prompt = false;
        }

        let choice = match select.prompt_skippable() {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => return Err(map_inquire_err(e)),
        };

        offer_upgrade_for_selection(cache, &choice).await?;

        let again = Confirm::new("Select another package?")
            .with_default(false)
            .prompt_skippable()
            .map_err(map_inquire_err)?
            .unwrap_or(false);
        if !again {
            break;
        }
    }

    Ok(())
}

#[instrument(skip(cache))]
pub async fn list(cache: &Cache, query: Option<String>) -> Result<()> {
    let rows = collect_installed_rows(cache).await?;

    if rows.is_empty() {
        println!("no packages installed");
        return Ok(());
    }

    let use_ui =
        io::stdin().is_terminal() && io::stdout().is_terminal() && std::env::var_os("CI").is_none();

    if use_ui {
        return run_interactive_list(cache, query).await;
    }

    let q_str = query.as_deref().unwrap_or("");
    let filtered: Vec<_> = rows
        .iter()
        .filter(|r| matches_query(r, q_str))
        .cloned()
        .collect();

    if filtered.is_empty() {
        println!("no installed packages match '{q_str}'");
        return Ok(());
    }

    print_table(&filtered);
    let (fc, cc) = summarize_counts(&filtered);
    print_summary(filtered.len(), fc, cc);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::matches_query;
    use super::InstalledRow;

    fn row(name: &str, line: &str) -> InstalledRow {
        InstalledRow {
            name: name.to_string(),
            line: line.to_string(),
            is_cask: false,
        }
    }

    #[test]
    fn matches_query_empty_string_matches_all() {
        let r = row("tree", "tree 2.0");
        assert!(matches_query(&r, ""));
    }

    #[test]
    fn matches_query_name_substring() {
        let r = row("ripgrep", "ripgrep 14");
        assert!(matches_query(&r, "rip"));
        assert!(!matches_query(&r, "zzz"));
    }

    #[test]
    fn matches_query_is_case_insensitive() {
        let r = row("Foo-Bar", "foo-bar 1");
        assert!(matches_query(&r, "FOO"));
        assert!(matches_query(&r, "bar"));
    }

    #[test]
    fn matches_query_matches_line_text() {
        let r = row("x", "x 1 (source) something");
        assert!(matches_query(&r, "source"));
    }
}
