//! `wax upgrade --vulnerable` and `wax outdated --vulnerable`.
//!
//! `brew upgrade --vulnerable` takes every outdated formula with an open
//! finding, and its help warns the new version may not fix them. Here each
//! candidate's available version is checked against the same advisories
//! first, so upgrades that would not clear anything are reported, not run.

use super::{upgrade_outdated, OutdatedPackage};
use crate::adopt;
use crate::advisory::{
    self, Advisory, AdvisoryDb, AdvisoryStore, Freshness, PackageReport, RangeState, ScanReport,
    Severity,
};
use crate::cache::Cache;
use crate::commands::vulns::{plural, select_formulae};
use crate::error::{Result, WaxError};
use crate::install::{InstallMode, InstalledPackage};
use crate::ui::{confirm_prompt, elapsed_suffix};
use crate::version::is_same_or_newer;
use console::style;
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    /// Upgrading to `target` clears `cleared` of the open advisories.
    Upgrade { target: String, cleared: usize },
    /// Would clear `cleared` advisories, but the formula is pinned.
    Pinned { target: String, cleared: usize },
    /// An upgrade exists, but `target` is still affected by every advisory.
    StillAffected { target: String },
    /// Already on the newest version Homebrew ships.
    NoUpgrade,
}

struct PlanEntry {
    report: PackageReport,
    install_mode: InstallMode,
    action: Action,
}

fn still_affected(advisory: &Advisory, version: &str) -> bool {
    matches!(
        advisory.range_status(version),
        None | Some(RangeState::Affected { .. })
    )
}

fn classify(report: &PackageReport, pinned: bool, latest: Option<&str>) -> Action {
    let Some(target) = latest.filter(|latest| !is_same_or_newer(&report.version, latest)) else {
        return Action::NoUpgrade;
    };
    let cleared = report
        .open
        .iter()
        .filter(|open| !still_affected(&open.advisory, target))
        .count();
    let target = target.to_string();
    match (cleared, pinned) {
        (0, _) => Action::StillAffected { target },
        (_, true) => Action::Pinned { target, cleared },
        (_, false) => Action::Upgrade { target, cleared },
    }
}

fn scan_installed(
    db: &AdvisoryDb,
    installed: &HashMap<String, InstalledPackage>,
    packages: &[String],
    min_severity: Option<Severity>,
    scope: Option<InstallMode>,
) -> Result<ScanReport> {
    let mut report = advisory::scan(db, select_formulae(installed, packages, scope)?)?;
    if let Some(min) = min_severity {
        for pkg in &mut report.packages {
            pkg.retain_min_severity(min);
        }
    }
    Ok(report)
}

/// Refresh both indexes, scan, and decide what to do with each vulnerable
/// formula. Advisories are always revalidated: a 304 is cheap and security
/// decisions should not run on day-old data.
async fn prepare(
    cache: &Cache,
    packages: &[String],
    min_severity: Option<Severity>,
    scope: Option<InstallMode>,
) -> Result<(AdvisoryDb, Vec<PlanEntry>)> {
    let store = AdvisoryStore::new(cache);
    let first_fetch = !store.is_present();
    let (fresh, db, installed) = tokio::join!(
        cache.ensure_fresh(),
        store.load(Freshness::Revalidate, first_fetch),
        adopt::sync_formulae()
    );
    fresh?;
    let db = db?;
    let installed = installed?;

    let report = scan_installed(&db, &installed, packages, min_severity, scope)?;
    let formulae = cache.load_all_formulae().await?;
    let formula_index: HashMap<_, _> = formulae.iter().map(|f| (f.name.as_str(), f)).collect();

    let entries = report
        .packages
        .into_iter()
        .filter(|pkg| !pkg.open.is_empty())
        .filter_map(|report| {
            let installed = installed.get(&report.name)?;
            let latest = formula_index
                .get(report.name.as_str())
                .map(|f| f.full_version());
            Some(PlanEntry {
                action: classify(&report, installed.pinned, latest.as_deref()),
                install_mode: installed.install_mode,
                report,
            })
        })
        .collect();
    Ok((db, entries))
}

fn advisory_count(report: &PackageReport) -> String {
    let open = report.open.len();
    format!(
        "{open} {} ({})",
        plural(open, "advisory", "advisories"),
        report.severity_summary()
    )
}

fn print_plan(entries: &[PlanEntry]) {
    let (upgrades, skipped): (Vec<&PlanEntry>, Vec<&PlanEntry>) = entries
        .iter()
        .partition(|e| matches!(e.action, Action::Upgrade { .. }));

    println!("\n{} security upgrade plan", style("→").cyan().bold());
    if upgrades.is_empty() {
        println!(
            "  {}",
            style("no available upgrade clears an open advisory").dim()
        );
    }
    for entry in &upgrades {
        let Action::Upgrade { target, cleared } = &entry.action else {
            continue;
        };
        let open = entry.report.open.len();
        let fixes = if *cleared == open {
            format!("fixes {}", advisory_count(&entry.report))
        } else {
            format!(
                "fixes {cleared} of {} · {} stay open",
                advisory_count(&entry.report),
                open - cleared
            )
        };
        println!(
            "  {} {} {} {}  {}",
            style("↻").cyan(),
            style(&entry.report.name).magenta(),
            style(&entry.report.version).dim(),
            style(format!("→ {target}")).green(),
            style(fixes).dim()
        );
    }

    if skipped.is_empty() {
        return;
    }
    println!("\n  {}", style("not upgraded:").dim());
    for entry in &skipped {
        let reason = match &entry.action {
            Action::Upgrade { .. } => continue,
            Action::Pinned { target, cleared } => format!(
                "pinned · {target} fixes {cleared} (`wax unpin {}`)",
                entry.report.name
            ),
            Action::StillAffected { target } => format!("{target} is still affected"),
            Action::NoUpgrade => match entry.report.upgrade_target() {
                Some(fixed) => format!("fixed in {fixed}, not yet in Homebrew"),
                None => "no fix available yet".to_string(),
            },
        };
        println!(
            "  {} {} {}  {}  {}",
            style("·").dim(),
            style(&entry.report.name).yellow(),
            style(&entry.report.version).dim(),
            style(advisory_count(&entry.report)).dim(),
            reason
        );
    }
}

fn print_clean(start: Instant) {
    println!(
        "{} no open advisories in installed formulae{}",
        style("✓").green(),
        elapsed_suffix(start.elapsed())
    );
}

/// Exit status mirrors `wax vulns`: non-zero while advisories stay open.
fn finish(report_open: usize, vulnerable_packages: usize) -> Result<()> {
    if report_open == 0 {
        return Ok(());
    }
    println!(
        "{} {} {} still open in {} {} · run `wax vulns` for details",
        style("!").yellow(),
        report_open,
        plural(report_open, "advisory", "advisories"),
        vulnerable_packages,
        plural(vulnerable_packages, "package", "packages")
    );
    Err(WaxError::VulnerabilitiesFound(report_open))
}

pub async fn upgrade_vulnerable(
    cache: &Cache,
    packages: &[String],
    min_severity: Option<Severity>,
    dry_run: bool,
    ask: bool,
    scope: Option<InstallMode>,
) -> Result<()> {
    let start = Instant::now();
    let (db, entries) = prepare(cache, packages, min_severity, scope).await?;
    if entries.is_empty() {
        print_clean(start);
        return Ok(());
    }

    print_plan(&entries);
    let open_before: usize = entries.iter().map(|e| e.report.open.len()).sum();
    let to_upgrade: Vec<OutdatedPackage> = entries
        .iter()
        .filter_map(|entry| match &entry.action {
            Action::Upgrade { target, .. } => Some(OutdatedPackage {
                name: entry.report.name.clone(),
                installed_version: entry.report.version.clone(),
                latest_version: target.clone(),
                is_cask: false,
                install_mode: Some(entry.install_mode),
            }),
            _ => None,
        })
        .collect();

    if dry_run {
        println!("\n{}", style("dry run - no changes made").dim());
        return Ok(());
    }
    if to_upgrade.is_empty() {
        println!();
        return finish(open_before, entries.len());
    }
    if ask && !confirm_prompt("Proceed with upgrade?")? {
        println!("{} upgrade cancelled", style("✗").red());
        return Ok(());
    }

    println!();
    upgrade_outdated(cache, to_upgrade, false, false, start).await?;

    let installed = adopt::sync_formulae().await?;
    let after = scan_installed(&db, &installed, packages, min_severity, scope)?;
    let open_after = after.open_count();
    let cleared = open_before.saturating_sub(open_after);
    if cleared > 0 {
        println!(
            "{} cleared {} {}",
            style("✓").green(),
            cleared,
            plural(cleared, "advisory", "advisories")
        );
    }
    finish(open_after, after.vulnerable().count())
}

pub async fn outdated_vulnerable(
    cache: &Cache,
    min_severity: Option<Severity>,
    scope: Option<InstallMode>,
) -> Result<()> {
    let start = Instant::now();
    let (_, entries) = prepare(cache, &[], min_severity, scope).await?;
    if entries.is_empty() {
        print_clean(start);
        return Ok(());
    }

    print_plan(&entries);
    let upgradable = entries
        .iter()
        .filter(|e| matches!(e.action, Action::Upgrade { .. }))
        .count();
    println!(
        "\n{} {} can be upgraded to clear advisories{}",
        style(upgradable).cyan(),
        plural(upgradable, "package", "packages"),
        elapsed_suffix(start.elapsed())
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(name: &str, version: &str) -> PackageReport {
        let db = advisory::test_db();
        advisory::scan(&db, [(name, version)])
            .unwrap()
            .packages
            .into_iter()
            .next()
            .expect("fixture formula is vulnerable at this version")
    }

    fn upgrade(target: &str, cleared: usize) -> Action {
        Action::Upgrade {
            target: target.to_string(),
            cleared,
        }
    }

    fn still_affected_at(target: &str) -> Action {
        Action::StillAffected {
            target: target.to_string(),
        }
    }

    #[test]
    fn upgrade_only_when_target_clears_an_advisory() {
        let hermes = report("hermes-agent", "0.22.0");
        assert_eq!(
            classify(&hermes, false, Some("0.22.4")),
            upgrade("0.22.4", 1)
        );
        assert_eq!(
            classify(&hermes, false, Some("0.22.1")),
            still_affected_at("0.22.1")
        );
    }

    #[test]
    fn newer_version_can_still_be_affected() {
        // hermes-agent's version_scheme jump lands in a second affected range.
        let hermes = report("hermes-agent", "0.22.0");
        assert_eq!(
            classify(&hermes, false, Some("2026.5.1")),
            still_affected_at("2026.5.1")
        );
        let abcde = report("abcde", "2.9.3_1");
        assert_eq!(
            classify(&abcde, false, Some("2.9.4")),
            still_affected_at("2.9.4")
        );
    }

    #[test]
    fn no_upgrade_when_latest_is_installed_or_unknown() {
        let hermes = report("hermes-agent", "0.22.0");
        assert_eq!(classify(&hermes, false, Some("0.22.0")), Action::NoUpgrade);
        assert_eq!(classify(&hermes, false, None), Action::NoUpgrade);
    }

    #[test]
    fn pinned_formulae_are_not_upgraded() {
        let hermes = report("hermes-agent", "0.22.0");
        assert_eq!(
            classify(&hermes, true, Some("0.22.4")),
            Action::Pinned {
                target: "0.22.4".to_string(),
                cleared: 1
            }
        );
        // A pin is irrelevant when the upgrade would not help anyway.
        assert_eq!(
            classify(&hermes, true, Some("0.22.1")),
            still_affected_at("0.22.1")
        );
    }
}
