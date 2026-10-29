use crate::advisory::{
    self, AdvisoryStore, Freshness, OpenAdvisory, PackageReport, ScanReport, Severity,
};
use crate::cache::Cache;
use crate::error::{Result, WaxError};
use crate::install::{InstallMode, InstalledPackage};
use crate::ui::elapsed_suffix;
use console::style;
use serde::Serialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::instrument;

const SUMMARY_WIDTH: usize = 72;

pub(crate) fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 {
        one
    } else {
        many
    }
}

fn severity_label(severity: Option<Severity>) -> console::StyledObject<String> {
    let label = format!(
        "{:<8}",
        severity.map_or("unknown", Severity::as_str).to_uppercase()
    );
    match severity {
        Some(Severity::Critical) => style(label).red().bold(),
        Some(Severity::High) => style(label).red(),
        Some(Severity::Medium) => style(label).yellow(),
        Some(Severity::Low) | None => style(label).dim(),
    }
}

fn truncate(text: &str, width: usize) -> String {
    match text.char_indices().nth(width) {
        Some((idx, _)) => format!("{}…", text[..idx].trim_end()),
        None => text.to_string(),
    }
}

fn print_advisory(open: &OpenAdvisory, package_fixable: bool) {
    let advisory = &open.advisory;
    let mut line = format!(
        "    {} {}",
        severity_label(advisory.severity),
        style(advisory.display_id()).bold()
    );
    if let Some(summary) = advisory.summary.as_deref() {
        line.push_str(&format!("  {}", truncate(summary, SUMMARY_WIDTH)));
    }
    if let Some(resource) = advisory.resource.as_deref() {
        line.push_str(&format!(" {}", style(format!("(via {resource})")).dim()));
    }
    if let Some(confidence) = advisory.confidence.as_deref().filter(|c| *c != "high") {
        line.push_str(&format!(
            " {}",
            style(format!("[{confidence} confidence]")).dim()
        ));
    }
    // The package header already says "no fix available" when nothing is fixed.
    if package_fixable && open.fixed_in.is_none() {
        line.push_str(&format!(" {}", style("· no fix yet").dim()));
    }
    println!("{line}");
}

fn print_package(pkg: &PackageReport) {
    let target = match pkg.upgrade_target() {
        Some(version) => format!("→ {}", style(version).green()),
        None => style("no fix available").dim().to_string(),
    };
    println!(
        "\n  {} {}  {}",
        style(&pkg.name).magenta().bold(),
        style(&pkg.version).dim(),
        target
    );
    let fixable = pkg.upgrade_target().is_some();
    for open in &pkg.open {
        print_advisory(open, fixable);
    }
}

fn print_report(report: &ScanReport, advisory_count: u64, elapsed: Duration) {
    let vulnerable: Vec<&PackageReport> = report.vulnerable().collect();
    let checked = format!(
        "{} {} checked against {} advisories",
        style(report.checked).cyan(),
        plural(report.checked, "formula", "formulae"),
        style(advisory_count).cyan()
    );

    if vulnerable.is_empty() {
        println!(
            "{} no known vulnerabilities · {}{}",
            style("✓").green(),
            checked,
            elapsed_suffix(elapsed)
        );
    } else {
        let open = report.open_count();
        println!(
            "{} {} vulnerable {} · {} {}",
            style("✗").red().bold(),
            vulnerable.len(),
            plural(vulnerable.len(), "package", "packages"),
            open,
            plural(open, "advisory", "advisories")
        );
        for pkg in &vulnerable {
            print_package(pkg);
        }
    }

    let patched: usize = report.packages.iter().map(|p| p.patched.len()).sum();
    if patched > 0 {
        println!(
            "\n{} {} {} resolved by Homebrew patches",
            style("✓").green(),
            patched,
            plural(patched, "advisory", "advisories")
        );
    }
    if !report.skipped.is_empty() {
        println!(
            "{} skipped HEAD {}: {}",
            style("?").dim(),
            plural(report.skipped.len(), "install", "installs"),
            report.skipped.join(", ")
        );
    }

    if !vulnerable.is_empty() {
        let fixable: Vec<&str> = vulnerable
            .iter()
            .filter(|p| p.upgrade_target().is_some())
            .map(|p| p.name.as_str())
            .collect();
        if !fixable.is_empty() {
            println!(
                "\n{} {}",
                style("→").cyan(),
                style(format!("wax upgrade --vulnerable {}", fixable.join(" "))).bold()
            );
        }
        println!("{}{}", checked, elapsed_suffix(elapsed));
    }
}

#[derive(Serialize)]
struct JsonReport<'a> {
    checked: usize,
    advisory_count: u64,
    vulnerable: Vec<JsonPackage<'a>>,
    patched: Vec<JsonPatched<'a>>,
    skipped: &'a [String],
}

#[derive(Serialize)]
struct JsonPackage<'a> {
    name: &'a str,
    version: &'a str,
    upgrade_to: Option<&'a str>,
    advisories: Vec<JsonAdvisory<'a>>,
}

#[derive(Serialize)]
struct JsonAdvisory<'a> {
    id: &'a str,
    upstream: &'a [String],
    summary: Option<&'a str>,
    severity: Option<&'static str>,
    fix: Option<&'a str>,
    fixed_in: Option<&'a str>,
    resource: Option<&'a str>,
    confidence: Option<&'a str>,
}

#[derive(Serialize)]
struct JsonPatched<'a> {
    name: &'a str,
    version: &'a str,
    advisories: Vec<&'a str>,
}

fn json_report(report: &ScanReport, advisory_count: u64) -> JsonReport<'_> {
    JsonReport {
        checked: report.checked,
        advisory_count,
        vulnerable: report
            .vulnerable()
            .map(|pkg| JsonPackage {
                name: &pkg.name,
                version: &pkg.version,
                upgrade_to: pkg.upgrade_target(),
                advisories: pkg
                    .open
                    .iter()
                    .map(|open| JsonAdvisory {
                        id: &open.advisory.id,
                        upstream: &open.advisory.upstream,
                        summary: open.advisory.summary.as_deref(),
                        severity: open.advisory.severity.map(Severity::as_str),
                        fix: open.advisory.fix.as_deref(),
                        fixed_in: open.fixed_in.as_deref(),
                        resource: open.advisory.resource.as_deref(),
                        confidence: open.advisory.confidence.as_deref(),
                    })
                    .collect(),
            })
            .collect(),
        patched: report
            .packages
            .iter()
            .filter(|p| !p.patched.is_empty())
            .map(|pkg| JsonPatched {
                name: &pkg.name,
                version: &pkg.version,
                advisories: pkg.patched.iter().map(|a| a.id.as_str()).collect(),
            })
            .collect(),
        skipped: &report.skipped,
    }
}

/// `(name, version)` for the named installed formulae, or all of them, limited
/// to `scope` when given.
pub(crate) fn select_formulae<'a>(
    installed: &'a HashMap<String, InstalledPackage>,
    names: &[String],
    scope: Option<InstallMode>,
) -> Result<Vec<(&'a str, &'a str)>> {
    let mut selected: Vec<(&str, &str)> = if names.is_empty() {
        installed
            .iter()
            .filter(|(_, pkg)| scope.is_none_or(|mode| pkg.install_mode == mode))
            .map(|(name, pkg)| (name.as_str(), pkg.version.as_str()))
            .collect()
    } else {
        names
            .iter()
            .map(|name| {
                installed
                    .get_key_value(name)
                    .map(|(name, pkg)| (name.as_str(), pkg.version.as_str()))
                    .ok_or_else(|| WaxError::NotInstalled(name.clone()))
            })
            .collect::<Result<_>>()?
    };
    selected.sort_unstable();
    selected.dedup();
    Ok(selected)
}

/// Check installed formulae against the Homebrew advisory database. Exits
/// non-zero when any open advisory remains, so it can gate CI.
#[instrument(skip(cache))]
pub async fn vulns(
    cache: &Cache,
    formulae: &[String],
    min_severity: Option<Severity>,
    json: bool,
) -> Result<()> {
    let start = Instant::now();
    let store = AdvisoryStore::new(cache);
    let (installed, db) = tokio::join!(
        crate::adopt::sync_formulae(),
        store.load(Freshness::IfStale, !json)
    );
    let installed = installed?;
    let db = db?;

    let selected = select_formulae(&installed, formulae, None)?;

    let mut report = advisory::scan(&db, selected)?;
    if let Some(min) = min_severity {
        for pkg in &mut report.packages {
            pkg.retain_min_severity(min);
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json_report(&report, db.advisory_count()))?
        );
    } else {
        print_report(&report, db.advisory_count(), start.elapsed());
    }

    match report.open_count() {
        0 => Ok(()),
        open => Err(WaxError::VulnerabilitiesFound(open)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("héllo wörld", 5), "héllo…");
        assert_eq!(truncate("trailing space here", 9), "trailing…");
    }
}
