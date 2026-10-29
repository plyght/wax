//! Homebrew security advisories.
//!
//! Records come from Homebrew/advisory-database (OSV format, `Homebrew`
//! ecosystem), which formulae.brew.sh publishes as a single index keyed by
//! formula name. The ~57MB JSON index is compacted once per refresh into a
//! bincode file laid out as a sorted per-formula table plus blobs, so a scan
//! only decodes the records for installed formulae.
//!
//! Range evaluation and severity derivation mirror Homebrew's
//! `Library/Homebrew/vulns/{vulnerability,cvss,advisory_database}.rb`.

use crate::cache::Cache;
use crate::error::{Result, WaxError};
use crate::version::BrewVersion;
use console::style;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{info, instrument, warn};

pub const ADVISORY_API_URL: &str = "https://formulae.brew.sh/api/advisories.json";

const ADVISORY_BIN_MAGIC: &[u8] = b"WAXADV1\0";

/// Homebrew's own feed cache refreshes daily; `wax update` revalidates sooner.
const ADVISORY_STALE_SECS: i64 = 86_400;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    clap::ValueEnum,
)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }

    fn from_label(label: Option<&str>) -> Option<Self> {
        match label?.to_ascii_lowercase().as_str() {
            "critical" => Some(Severity::Critical),
            "high" => Some(Severity::High),
            "medium" | "moderate" => Some(Severity::Medium),
            "low" => Some(Severity::Low),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Interval {
    lower: Option<String>,
    upper: Option<String>,
    upper_inclusive: bool,
    fix: Option<String>,
}

/// A compacted `BREW-*` record: only what scanning and display need.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Advisory {
    pub id: String,
    pub upstream: Vec<String>,
    pub summary: Option<String>,
    pub severity: Option<Severity>,
    /// `ecosystem_specific.fix`: `"patch"` (Homebrew ships a patch) or `"bump"`.
    pub fix: Option<String>,
    /// Vendored resource the advisory concerns (e.g. a PyPI dependency).
    pub resource: Option<String>,
    pub confidence: Option<String>,
    intervals: Vec<Interval>,
    versions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeState {
    Affected { fixed_in: Option<String> },
    Fixed { fixed_in: Option<String> },
    NotApplicable,
}

impl Advisory {
    /// The identifier to show: the first upstream CVE, else the first
    /// upstream id, else the `BREW-*` id.
    pub fn display_id(&self) -> &str {
        self.upstream
            .iter()
            .find(|id| id.starts_with("CVE-"))
            .or_else(|| self.upstream.first())
            .unwrap_or(&self.id)
    }

    /// `None` when nothing in the record could be compared, which callers
    /// treat as affected (matching `brew`'s `status_for`).
    pub fn range_status(&self, version: &str) -> Option<RangeState> {
        let target_str = normalize_version(version);
        let target = BrewVersion::parse(target_str);
        let mut checked = false;
        let mut past_terminals: Vec<(&str, Option<&str>)> = Vec::new();

        for interval in &self.intervals {
            checked = true;
            let above = interval
                .lower
                .as_deref()
                .is_none_or(|lower| target >= BrewVersion::parse(lower));
            let below = match interval.upper.as_deref() {
                None => true,
                Some(upper) if interval.upper_inclusive => target <= BrewVersion::parse(upper),
                Some(upper) => target < BrewVersion::parse(upper),
            };
            if above && below {
                return Some(RangeState::Affected {
                    fixed_in: interval.fix.clone(),
                });
            }

            if let Some(upper) = interval.upper.as_deref() {
                let rel = target.cmp(&BrewVersion::parse(upper));
                let past = if interval.upper_inclusive {
                    rel == Ordering::Greater
                } else {
                    rel != Ordering::Less
                };
                if past {
                    past_terminals.push((upper, interval.fix.as_deref()));
                }
            }
        }

        if !self.versions.is_empty() {
            checked = true;
            if self
                .versions
                .iter()
                .any(|v| normalize_version(v) == target_str)
            {
                return Some(RangeState::Affected { fixed_in: None });
            }
        }

        if !checked {
            return None;
        }

        // Prefer a non-fix terminal on a tie so an inclusive `last_affected`
        // boundary is never reported as a fix version.
        let latest = past_terminals.iter().max_by(|a, b| {
            BrewVersion::parse(a.0)
                .cmp(&BrewVersion::parse(b.0))
                .then(a.1.is_none().cmp(&b.1.is_none()))
        });
        let Some(&(boundary, fixed_in)) = latest else {
            return Some(RangeState::NotApplicable);
        };
        let boundary_version = BrewVersion::parse(boundary);
        let shadowed = past_terminals
            .iter()
            .any(|(other, fix)| fix.is_none() && BrewVersion::parse(other) == boundary_version);
        Some(RangeState::Fixed {
            fixed_in: if shadowed {
                None
            } else {
                fixed_in.map(String::from)
            },
        })
    }
}

fn normalize_version(version: &str) -> &str {
    let bytes = version.as_bytes();
    if matches!(bytes.first(), Some(b'v' | b'V')) && !matches!(bytes.get(1), Some(b'v' | b'V')) {
        &version[1..]
    } else {
        version
    }
}

/// CVSS v3.0/v3.1 base score and qualitative rating. v2 and v4.0 vectors
/// return `None` so callers fall through to the next severity source.
mod cvss {
    use super::Severity;
    use std::collections::HashMap;

    fn av(v: &str) -> Option<f64> {
        Some(match v {
            "N" => 0.85,
            "A" => 0.62,
            "L" => 0.55,
            "P" => 0.2,
            _ => return None,
        })
    }

    fn ac(v: &str) -> Option<f64> {
        Some(match v {
            "L" => 0.77,
            "H" => 0.44,
            _ => return None,
        })
    }

    fn pr(v: &str, scope_changed: bool) -> Option<f64> {
        Some(match (v, scope_changed) {
            ("N", _) => 0.85,
            ("L", false) => 0.62,
            ("L", true) => 0.68,
            ("H", false) => 0.27,
            ("H", true) => 0.50,
            _ => return None,
        })
    }

    fn ui(v: &str) -> Option<f64> {
        Some(match v {
            "N" => 0.85,
            "R" => 0.62,
            _ => return None,
        })
    }

    fn cia(v: &str) -> Option<f64> {
        Some(match v {
            "N" => 0.0,
            "L" => 0.22,
            "H" => 0.56,
            _ => return None,
        })
    }

    /// CVSS v3.x "Roundup" (spec Appendix A).
    fn round_up(value: f64) -> f64 {
        let int = (value * 100_000.0).round() as i64;
        if int % 10_000 == 0 {
            int as f64 / 100_000.0
        } else {
            ((int / 10_000) + 1) as f64 / 10.0
        }
    }

    pub fn base_score(vector: &str) -> Option<f64> {
        let mut parts = vector.split('/');
        if !matches!(parts.next(), Some("CVSS:3.0" | "CVSS:3.1")) {
            return None;
        }
        let metrics: HashMap<&str, &str> = parts
            .map(|part| part.split_once(':').unwrap_or((part, "")))
            .collect();
        let get = |key: &str| metrics.get(key).copied();

        let scope_changed = match get("S")? {
            "C" => true,
            "U" => false,
            _ => return None,
        };
        let av = av(get("AV")?)?;
        let ac = ac(get("AC")?)?;
        let pr = pr(get("PR")?, scope_changed)?;
        let ui = ui(get("UI")?)?;
        let c = cia(get("C")?)?;
        let i = cia(get("I")?)?;
        let a = cia(get("A")?)?;

        let iss = 1.0 - ((1.0 - c) * (1.0 - i) * (1.0 - a));
        let impact = if scope_changed {
            7.52 * (iss - 0.029) - 3.25 * (iss - 0.02).powi(15)
        } else {
            6.42 * iss
        };
        if impact <= 0.0 {
            return Some(0.0);
        }

        let exploitability = 8.22 * av * ac * pr * ui;
        let mut raw = impact + exploitability;
        if scope_changed {
            raw *= 1.08;
        }
        Some(round_up(raw.min(10.0)))
    }

    pub fn severity(vector: &str) -> Option<Severity> {
        let score = base_score(vector)?;
        if score >= 9.0 {
            Some(Severity::Critical)
        } else if score >= 7.0 {
            Some(Severity::High)
        } else if score >= 4.0 {
            Some(Severity::Medium)
        } else if score > 0.0 {
            Some(Severity::Low)
        } else {
            None
        }
    }
}

// --- Raw OSV index (only the fields we keep; everything else is skipped) ---

#[derive(Deserialize)]
struct RawIndex {
    advisories: HashMap<String, Vec<RawRecord>>,
}

#[derive(Deserialize)]
struct RawRecord {
    id: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    details: Option<String>,
    #[serde(default)]
    upstream: Option<Vec<String>>,
    #[serde(default)]
    aliases: Option<Vec<String>>,
    #[serde(default)]
    withdrawn: Option<IgnoredAny>,
    #[serde(default)]
    severity: Option<Vec<RawSeverity>>,
    #[serde(default)]
    affected: Option<Vec<RawAffected>>,
    #[serde(default)]
    database_specific: Option<RawSpecific>,
}

#[derive(Deserialize)]
struct RawSeverity {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    score: String,
}

#[derive(Deserialize)]
struct RawAffected {
    #[serde(default)]
    package: Option<RawPackage>,
    #[serde(default)]
    ranges: Option<Vec<RawRange>>,
    #[serde(default)]
    versions: Option<Vec<String>>,
    #[serde(default)]
    ecosystem_specific: Option<RawSpecific>,
    #[serde(default)]
    database_specific: Option<RawSpecific>,
}

#[derive(Deserialize)]
struct RawPackage {
    #[serde(default)]
    ecosystem: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawRange {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    events: Option<Vec<RawEvent>>,
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(default)]
    introduced: Option<String>,
    #[serde(default)]
    fixed: Option<String>,
    #[serde(default)]
    last_affected: Option<String>,
    #[serde(default)]
    limit: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawSpecific {
    #[serde(default)]
    fix: Option<String>,
    #[serde(default)]
    resource: Option<String>,
    #[serde(default)]
    confidence: Option<String>,
    #[serde(default)]
    severity: Option<String>,
}

fn close_interval(
    out: &mut Vec<Interval>,
    lower: &mut Option<String>,
    upper: Option<&str>,
    upper_inclusive: bool,
    is_fix: bool,
) {
    let upper = upper.map(|u| normalize_version(u).to_string());
    out.push(Interval {
        lower: lower.take(),
        fix: if is_fix { upper.clone() } else { None },
        upper,
        upper_inclusive,
    });
}

/// OSV events → intervals: `introduced` inclusive (`"0"` is unbounded),
/// `fixed` and `limit` exclusive, `last_affected` inclusive.
fn push_intervals(events: &[RawEvent], out: &mut Vec<Interval>) {
    let start = out.len();
    let mut lower: Option<String> = None;

    for event in events {
        if let Some(introduced) = event.introduced.as_deref() {
            if lower.is_some() {
                close_interval(out, &mut lower, None, false, false);
            }
            let introduced = normalize_version(introduced);
            lower = (introduced != "0").then(|| introduced.to_string());
        } else if let Some(fixed) = event.fixed.as_deref() {
            close_interval(out, &mut lower, Some(fixed), false, true);
        } else if let Some(last) = event.last_affected.as_deref() {
            close_interval(out, &mut lower, Some(last), true, false);
        } else if let Some(limit) = event.limit.as_deref() {
            let upper = (limit != "*").then_some(limit);
            close_interval(out, &mut lower, upper, false, false);
        }
    }
    if lower.is_some() || out.len() == start {
        close_interval(out, &mut lower, None, false, false);
    }
}

fn compact(formula: &str, record: RawRecord) -> Option<Advisory> {
    if record.withdrawn.is_some() {
        return None;
    }
    let affected = record.affected.unwrap_or_default();
    let first_eco = affected.first().and_then(|a| a.ecosystem_specific.as_ref());

    let mut intervals = Vec::new();
    let mut versions = Vec::new();
    for entry in &affected {
        let matches = entry.package.as_ref().is_some_and(|p| {
            p.ecosystem.as_deref() == Some("Homebrew") && p.name.as_deref() == Some(formula)
        });
        if !matches {
            continue;
        }
        for range in entry.ranges.as_deref().unwrap_or_default() {
            if range.kind.as_deref() == Some("GIT") {
                continue;
            }
            push_intervals(range.events.as_deref().unwrap_or_default(), &mut intervals);
        }
        versions.extend(entry.versions.iter().flatten().cloned());
    }

    let mut cvss_entries: Vec<&RawSeverity> = record
        .severity
        .iter()
        .flatten()
        .filter(|s| cvss_priority(&s.kind) > 0)
        .collect();
    cvss_entries.sort_by_key(|s| std::cmp::Reverse(cvss_priority(&s.kind)));
    let severity = cvss_entries
        .iter()
        .find_map(|s| cvss::severity(&s.score))
        .or_else(|| {
            Severity::from_label(
                record
                    .database_specific
                    .as_ref()
                    .and_then(|d| d.severity.as_deref()),
            )
        })
        .or_else(|| {
            affected.iter().find_map(|a| {
                let eco = a
                    .ecosystem_specific
                    .as_ref()
                    .and_then(|s| s.severity.as_deref());
                let db = a
                    .database_specific
                    .as_ref()
                    .and_then(|s| s.severity.as_deref());
                Severity::from_label(eco).or_else(|| Severity::from_label(db))
            })
        });

    let upstream = record
        .upstream
        .filter(|u| !u.is_empty())
        .or(record.aliases)
        .unwrap_or_default();

    let summary = record
        .summary
        .filter(|s| !s.trim().is_empty())
        .or_else(|| record.details.as_deref().and_then(summary_from_details));

    Some(Advisory {
        id: record.id,
        upstream,
        summary,
        severity,
        fix: first_eco.and_then(|e| e.fix.clone()),
        resource: first_eco.and_then(|e| e.resource.clone()),
        confidence: record.database_specific.and_then(|d| d.confidence),
        intervals,
        versions,
    })
}

/// First prose line of `details`, for records without a `summary`.
fn summary_from_details(details: &str) -> Option<String> {
    let line = details
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;
    Some(match line.char_indices().nth(SUMMARY_MAX_CHARS) {
        Some((idx, _)) => format!("{}…", &line[..idx]),
        None => line.to_string(),
    })
}

const SUMMARY_MAX_CHARS: usize = 200;

fn cvss_priority(kind: &str) -> u8 {
    match kind {
        "CVSS_V4" => 4,
        "CVSS_V3" => 3,
        "CVSS_V2" => 2,
        _ => 0,
    }
}

/// Parse the formulae.brew.sh advisory index into compact per-formula records.
fn build_index(json: &[u8]) -> Result<BTreeMap<String, Vec<Advisory>>> {
    let raw: RawIndex = serde_json::from_slice(json)?;
    Ok(raw
        .advisories
        .into_iter()
        .filter_map(|(formula, records)| {
            let mut advisories: Vec<Advisory> = records
                .into_iter()
                .filter_map(|r| compact(&formula, r))
                .collect();
            if advisories.is_empty() {
                return None;
            }
            advisories.sort_by(|a, b| a.id.cmp(&b.id));
            Some((formula, advisories))
        })
        .collect())
}

// --- On-disk format: magic | bincode(header) | blobs ---

#[derive(Serialize, Deserialize)]
struct BinHeader {
    advisory_count: u64,
    /// Sorted by formula name: (name, blob start, blob end).
    entries: Vec<(String, u64, u64)>,
}

fn bincode_err(e: bincode::Error) -> WaxError {
    WaxError::CacheError(format!("advisory cache: {e}"))
}

fn encode(index: &BTreeMap<String, Vec<Advisory>>) -> Result<Vec<u8>> {
    let mut blobs = Vec::new();
    let mut entries = Vec::with_capacity(index.len());
    let mut advisory_count = 0u64;
    for (name, advisories) in index {
        let start = blobs.len() as u64;
        bincode::serialize_into(&mut blobs, advisories).map_err(bincode_err)?;
        entries.push((name.clone(), start, blobs.len() as u64));
        advisory_count += advisories.len() as u64;
    }
    let mut payload = ADVISORY_BIN_MAGIC.to_vec();
    bincode::serialize_into(
        &mut payload,
        &BinHeader {
            advisory_count,
            entries,
        },
    )
    .map_err(bincode_err)?;
    payload.extend_from_slice(&blobs);
    Ok(payload)
}

/// A loaded advisory index. Records are decoded lazily per formula.
pub struct AdvisoryDb {
    payload: Vec<u8>,
    header: BinHeader,
    blob_start: usize,
}

impl AdvisoryDb {
    fn from_bytes(payload: Vec<u8>) -> Result<Self> {
        let body = payload
            .strip_prefix(ADVISORY_BIN_MAGIC)
            .ok_or_else(|| WaxError::CacheError("advisory cache format mismatch".to_string()))?;
        let mut cursor = Cursor::new(body);
        let header: BinHeader = bincode::deserialize_from(&mut cursor).map_err(bincode_err)?;
        let blob_start = ADVISORY_BIN_MAGIC.len() + cursor.position() as usize;
        let blob_len = (payload.len() - blob_start) as u64;
        if header
            .entries
            .iter()
            .any(|(_, s, e)| s > e || *e > blob_len)
        {
            return Err(WaxError::CacheError(
                "advisory cache is truncated".to_string(),
            ));
        }
        Ok(Self {
            payload,
            header,
            blob_start,
        })
    }

    pub fn advisory_count(&self) -> u64 {
        self.header.advisory_count
    }

    pub fn records_for(&self, formula: &str) -> Result<Vec<Advisory>> {
        let Ok(idx) = self
            .header
            .entries
            .binary_search_by(|(name, _, _)| name.as_str().cmp(formula))
        else {
            return Ok(Vec::new());
        };
        let (_, start, end) = &self.header.entries[idx];
        let blob =
            &self.payload[self.blob_start + *start as usize..self.blob_start + *end as usize];
        bincode::deserialize(blob).map_err(bincode_err)
    }
}

// --- Scanning ---

#[derive(Debug, Clone)]
pub struct OpenAdvisory {
    pub advisory: Advisory,
    pub fixed_in: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PackageReport {
    pub name: String,
    pub version: String,
    /// Advisories whose range contains the installed version.
    pub open: Vec<OpenAdvisory>,
    /// Advisories this version is past because Homebrew ships a fix patch.
    pub patched: Vec<Advisory>,
}

impl PackageReport {
    pub fn max_severity(&self) -> Option<Severity> {
        self.open.iter().filter_map(|o| o.advisory.severity).max()
    }

    /// Highest `fixed_in` among open advisories: the version that clears them.
    pub fn upgrade_target(&self) -> Option<&str> {
        self.open
            .iter()
            .filter_map(|o| o.fixed_in.as_deref())
            .max_by(|a, b| BrewVersion::parse(a).cmp(&BrewVersion::parse(b)))
    }

    /// Open advisory counts by severity, highest first: "1 critical, 2 medium".
    pub fn severity_summary(&self) -> String {
        let mut counts: Vec<(Option<Severity>, usize)> = Vec::new();
        for open in &self.open {
            match counts
                .iter_mut()
                .find(|(s, _)| *s == open.advisory.severity)
            {
                Some((_, n)) => *n += 1,
                None => counts.push((open.advisory.severity, 1)),
            }
        }
        counts.sort_by_key(|(s, _)| severity_rank(*s));
        counts
            .iter()
            .map(|(s, n)| format!("{n} {}", s.map_or("unrated", Severity::as_str)))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Keep only open advisories at or above `min`; unrated ones are dropped,
    /// as `brew vulns --severity` does.
    pub fn retain_min_severity(&mut self, min: Severity) {
        self.open
            .retain(|o| o.advisory.severity.is_some_and(|s| s >= min));
    }
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub checked: usize,
    /// Packages with at least one open or patched advisory.
    pub packages: Vec<PackageReport>,
    /// HEAD installs: their versions cannot be placed in a release range.
    pub skipped: Vec<String>,
}

impl ScanReport {
    pub fn vulnerable(&self) -> impl Iterator<Item = &PackageReport> {
        self.packages.iter().filter(|p| !p.open.is_empty())
    }

    pub fn open_count(&self) -> usize {
        self.packages.iter().map(|p| p.open.len()).sum()
    }
}

fn severity_rank(severity: Option<Severity>) -> std::cmp::Reverse<Option<Severity>> {
    std::cmp::Reverse(severity)
}

pub fn scan<'a>(
    db: &AdvisoryDb,
    installed: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<ScanReport> {
    let mut report = ScanReport::default();
    for (name, version) in installed {
        if version.starts_with("HEAD") {
            report.skipped.push(name.to_string());
            continue;
        }
        report.checked += 1;
        let records = db.records_for(name)?;
        if records.is_empty() {
            continue;
        }

        let mut open = Vec::new();
        let mut patched = Vec::new();
        for advisory in records {
            match advisory.range_status(version) {
                None => open.push(OpenAdvisory {
                    advisory,
                    fixed_in: None,
                }),
                Some(RangeState::Affected { fixed_in }) => {
                    open.push(OpenAdvisory { advisory, fixed_in })
                }
                Some(RangeState::Fixed { .. }) if advisory.fix.as_deref() == Some("patch") => {
                    patched.push(advisory)
                }
                Some(RangeState::Fixed { .. } | RangeState::NotApplicable) => {}
            }
        }
        if open.is_empty() && patched.is_empty() {
            continue;
        }
        open.sort_by(|a, b| {
            severity_rank(a.advisory.severity)
                .cmp(&severity_rank(b.advisory.severity))
                .then_with(|| a.advisory.id.cmp(&b.advisory.id))
        });
        report.packages.push(PackageReport {
            name: name.to_string(),
            version: version.to_string(),
            open,
            patched,
        });
    }
    report.packages.sort_by(|a, b| {
        severity_rank(a.max_severity())
            .cmp(&severity_rank(b.max_severity()))
            .then_with(|| a.name.cmp(&b.name))
    });
    report.skipped.sort();
    Ok(report)
}

// --- Cache store ---

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AdvisoryMeta {
    fetched_at: i64,
    etag: Option<String>,
    last_modified: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Use the cached index unless it is older than a day.
    IfStale,
    /// Always revalidate with the server (a 304 is cheap).
    Revalidate,
}

enum Fetched {
    NotModified,
    Updated {
        payload: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

pub struct AdvisoryStore {
    dir: PathBuf,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

impl AdvisoryStore {
    pub fn new(cache: &Cache) -> Self {
        Self::for_dir(cache.cache_dir_path())
    }

    pub fn for_dir(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    fn bin_path(&self) -> PathBuf {
        self.dir.join("advisories.bin")
    }

    fn meta_path(&self) -> PathBuf {
        self.dir.join("advisories.meta.json")
    }

    /// Whether advisories have been fetched before (so `wax update` only
    /// revalidates for users who scan).
    pub fn is_present(&self) -> bool {
        self.bin_path().exists()
    }

    async fn read_cached(&self) -> (Option<AdvisoryDb>, Option<AdvisoryMeta>) {
        let (bin, meta) = tokio::join!(fs::read(self.bin_path()), fs::read(self.meta_path()));
        let db = bin.ok().and_then(|p| AdvisoryDb::from_bytes(p).ok());
        let meta = meta
            .ok()
            .and_then(|m| serde_json::from_slice::<AdvisoryMeta>(&m).ok());
        (db, meta)
    }

    async fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        fs::write(&tmp, bytes).await?;
        fs::rename(&tmp, path).await?;
        Ok(())
    }

    async fn save_meta(&self, meta: &AdvisoryMeta) -> Result<()> {
        self.write_atomic(&self.meta_path(), &serde_json::to_vec(meta)?)
            .await
    }

    /// Load the advisory index, fetching it when missing or stale. A failed
    /// refresh falls back to the cached copy with a warning.
    #[instrument(skip(self))]
    pub async fn load(&self, freshness: Freshness, show_progress: bool) -> Result<AdvisoryDb> {
        let (cached, meta) = self.read_cached().await;
        // Without a usable index, revalidation headers would only earn a 304.
        let meta = cached.as_ref().and(meta);
        let fresh = freshness == Freshness::IfStale
            && meta
                .as_ref()
                .is_some_and(|m| now_unix() - m.fetched_at < ADVISORY_STALE_SECS);
        let cached = match cached {
            Some(db) if fresh => return Ok(db),
            other => other,
        };

        let spinner = show_progress.then(|| {
            crate::ui::create_spinner(if cached.is_some() {
                "Refreshing security advisories..."
            } else {
                "Fetching security advisories..."
            })
        });
        let fetched = Self::fetch(
            meta.as_ref().and_then(|m| m.etag.as_deref()),
            meta.as_ref().and_then(|m| m.last_modified.as_deref()),
        )
        .await;
        if let Some(spinner) = spinner {
            spinner.finish_and_clear();
        }

        match (fetched, cached) {
            (Ok(Fetched::NotModified), Some(db)) => {
                let meta = AdvisoryMeta {
                    fetched_at: now_unix(),
                    ..meta.unwrap_or_default()
                };
                if let Err(e) = self.save_meta(&meta).await {
                    warn!("failed to save advisory metadata: {e}");
                }
                Ok(db)
            }
            (Ok(Fetched::NotModified), None) => Err(WaxError::CacheError(
                "advisory server returned 304 without a cached index".to_string(),
            )),
            (
                Ok(Fetched::Updated {
                    payload,
                    etag,
                    last_modified,
                }),
                _,
            ) => {
                fs::create_dir_all(&self.dir).await?;
                self.write_atomic(&self.bin_path(), &payload).await?;
                self.save_meta(&AdvisoryMeta {
                    fetched_at: now_unix(),
                    etag,
                    last_modified,
                })
                .await?;
                AdvisoryDb::from_bytes(payload)
            }
            (Err(e), Some(db)) => {
                warn!("advisory refresh failed: {e}");
                eprintln!(
                    "  {} could not refresh security advisories ({e}); using cached data",
                    style("!").yellow()
                );
                Ok(db)
            }
            (Err(e), None) => Err(e),
        }
    }

    async fn fetch(etag: Option<&str>, last_modified: Option<&str>) -> Result<Fetched> {
        info!("Fetching advisories from {ADVISORY_API_URL}");
        let mut request = crate::http_client::default_client().get(ADVISORY_API_URL);
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        if let Some(last_modified) = last_modified {
            request = request.header("If-Modified-Since", last_modified);
        }

        let response = request.send().await?;
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            info!("Advisories not modified (304)");
            return Ok(Fetched::NotModified);
        }
        let response = response.error_for_status()?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(String::from)
        };
        let etag = header("etag");
        let last_modified = header("last-modified");
        let body = response.bytes().await?;

        let payload = tokio::task::spawn_blocking(move || encode(&build_index(&body)?))
            .await
            .map_err(|e| WaxError::CacheError(format!("advisory index build failed: {e}")))??;
        info!("Built advisory index ({} bytes)", payload.len());

        Ok(Fetched::Updated {
            payload,
            etag,
            last_modified,
        })
    }
}

#[cfg(test)]
const TEST_FIXTURE: &str = include_str!("../tests/fixtures/advisories_sample.json");

/// The fixture index (abcde, abi3audit, hermes-agent, lrzsz) for other modules' tests.
#[cfg(test)]
pub(crate) fn test_db() -> AdvisoryDb {
    let index = build_index(TEST_FIXTURE.as_bytes()).expect("fixture parses");
    AdvisoryDb::from_bytes(encode(&index).unwrap()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_index() -> BTreeMap<String, Vec<Advisory>> {
        build_index(TEST_FIXTURE.as_bytes()).expect("fixture parses")
    }

    fn fixture_db() -> AdvisoryDb {
        test_db()
    }

    fn advisory_with(events: &[(&str, &str)]) -> Advisory {
        let events: Vec<RawEvent> = events
            .iter()
            .map(|(kind, v)| {
                let v = Some(v.to_string());
                match *kind {
                    "introduced" => RawEvent {
                        introduced: v,
                        fixed: None,
                        last_affected: None,
                        limit: None,
                    },
                    "fixed" => RawEvent {
                        introduced: None,
                        fixed: v,
                        last_affected: None,
                        limit: None,
                    },
                    "last_affected" => RawEvent {
                        introduced: None,
                        fixed: None,
                        last_affected: v,
                        limit: None,
                    },
                    _ => RawEvent {
                        introduced: None,
                        fixed: None,
                        last_affected: None,
                        limit: v,
                    },
                }
            })
            .collect();
        let mut intervals = Vec::new();
        push_intervals(&events, &mut intervals);
        Advisory {
            id: "BREW-wax-test-CVE-0000-0000".to_string(),
            upstream: vec!["CVE-0000-0000".to_string()],
            summary: None,
            severity: None,
            fix: None,
            resource: None,
            confidence: None,
            intervals,
            versions: Vec::new(),
        }
    }

    fn affected(fixed_in: Option<&str>) -> Option<RangeState> {
        Some(RangeState::Affected {
            fixed_in: fixed_in.map(String::from),
        })
    }

    fn fixed(fixed_in: Option<&str>) -> Option<RangeState> {
        Some(RangeState::Fixed {
            fixed_in: fixed_in.map(String::from),
        })
    }

    #[test]
    fn introduced_zero_to_fixed_respects_revisions() {
        let adv = advisory_with(&[("introduced", "0"), ("fixed", "0.12.20_1")]);
        assert_eq!(adv.range_status("0.12.20"), affected(Some("0.12.20_1")));
        assert_eq!(adv.range_status("0.12.20_1"), fixed(Some("0.12.20_1")));
        assert_eq!(adv.range_status("0.12.21"), fixed(Some("0.12.20_1")));
    }

    #[test]
    fn introduced_only_is_open_ended() {
        let adv = advisory_with(&[("introduced", "2.9.3_1")]);
        assert_eq!(adv.range_status("2.9.3"), Some(RangeState::NotApplicable));
        assert_eq!(adv.range_status("2.9.3_1"), affected(None));
        assert_eq!(adv.range_status("3.0"), affected(None));
    }

    #[test]
    fn reopened_range_after_fix() {
        let adv = advisory_with(&[
            ("introduced", "1.7.3"),
            ("fixed", "1.7.25"),
            ("introduced", "1.7.27"),
        ]);
        assert_eq!(adv.range_status("1.7.2"), Some(RangeState::NotApplicable));
        assert_eq!(adv.range_status("1.7.10"), affected(Some("1.7.25")));
        assert_eq!(adv.range_status("1.7.26"), fixed(Some("1.7.25")));
        assert_eq!(adv.range_status("1.7.27"), affected(None));
    }

    #[test]
    fn last_affected_is_inclusive_and_never_a_fix() {
        let adv = advisory_with(&[("introduced", "2026.4.30"), ("last_affected", "2026.9.24")]);
        assert_eq!(adv.range_status("2026.9.24"), affected(None));
        assert_eq!(adv.range_status("2026.9.25"), fixed(None));
    }

    #[test]
    fn limit_is_exclusive_and_star_is_unbounded() {
        let adv = advisory_with(&[("introduced", "1.0"), ("limit", "2.0")]);
        assert_eq!(adv.range_status("1.5"), affected(None));
        assert_eq!(adv.range_status("2.0"), fixed(None));
        let adv = advisory_with(&[("introduced", "1.0"), ("limit", "*")]);
        assert_eq!(adv.range_status("99"), affected(None));
    }

    #[test]
    fn leading_v_is_stripped() {
        let adv = advisory_with(&[("introduced", "0"), ("fixed", "v1.2.0")]);
        assert_eq!(adv.range_status("v1.1.9"), affected(Some("1.2.0")));
        assert_eq!(adv.range_status("1.2.0"), fixed(Some("1.2.0")));
    }

    #[test]
    fn no_comparable_range_is_unknown() {
        let mut adv = advisory_with(&[]);
        adv.intervals.clear();
        assert_eq!(adv.range_status("1.0"), None);
    }

    #[test]
    fn explicit_versions_list_matches() {
        let mut adv = advisory_with(&[]);
        adv.intervals.clear();
        adv.versions = vec!["1.0".to_string()];
        assert_eq!(adv.range_status("1.0"), affected(None));
        assert_eq!(adv.range_status("1.1"), Some(RangeState::NotApplicable));
    }

    #[test]
    fn cvss_v3_base_scores() {
        let cases = [
            ("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H", 9.8),
            ("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N", 7.5),
            ("CVSS:3.1/AV:L/AC:L/PR:L/UI:N/S:U/C:H/I:N/A:H", 7.1),
            ("CVSS:3.1/AV:N/AC:H/PR:N/UI:N/S:U/C:L/I:H/A:N", 6.5),
            ("CVSS:3.0/AV:N/AC:L/PR:N/UI:R/S:C/C:L/I:L/A:N", 6.1),
            ("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:N", 0.0),
        ];
        for (vector, expected) in cases {
            assert_eq!(cvss::base_score(vector), Some(expected), "{vector}");
        }
        assert_eq!(
            cvss::severity("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"),
            Some(Severity::Critical)
        );
        assert_eq!(
            cvss::severity("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:N"),
            None
        );
    }

    #[test]
    fn cvss_rejects_v4_and_malformed_vectors() {
        assert_eq!(
            cvss::base_score("CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N"),
            None
        );
        assert_eq!(cvss::base_score("CVSS:3.1/AV:N/AC:L"), None);
        assert_eq!(
            cvss::base_score("CVSS:3.1/AV:X/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"),
            None
        );
    }

    #[test]
    fn fixture_parses_into_compact_records() {
        let index = fixture_index();
        assert_eq!(
            index.keys().collect::<Vec<_>>(),
            ["abcde", "abi3audit", "hermes-agent", "lrzsz"]
        );

        let lrzsz = &index["lrzsz"][0];
        assert_eq!(lrzsz.id, "BREW-lrzsz-CVE-2018-10195");
        assert_eq!(lrzsz.display_id(), "CVE-2018-10195");
        assert_eq!(lrzsz.fix.as_deref(), Some("patch"));
        assert_eq!(lrzsz.severity, Some(Severity::High));

        let abi3audit = &index["abi3audit"][0];
        assert_eq!(abi3audit.display_id(), "CVE-2024-35195");
        assert_eq!(abi3audit.resource.as_deref(), Some("requests"));
        assert_eq!(abi3audit.confidence.as_deref(), Some("high"));
        assert_eq!(abi3audit.severity, Some(Severity::Medium));

        let hermes = &index["hermes-agent"][0];
        assert_eq!(hermes.intervals.len(), 2);
        assert_eq!(hermes.range_status("0.22.0"), affected(Some("0.22.4")));
        assert_eq!(hermes.range_status("2026.9.1"), affected(None));
    }

    #[test]
    fn withdrawn_and_foreign_entries_are_handled() {
        let json = r#"{"advisories":{"wax-test-a":[
            {"id":"BREW-wax-test-a-1","withdrawn":"2026-01-01T00:00:00Z","affected":[]},
            {"id":"BREW-wax-test-a-2","upstream":[],"aliases":["CVE-0000-0002"],
             "affected":[{"package":{"ecosystem":"Homebrew","name":"other"},
               "ranges":[{"type":"ECOSYSTEM","events":[{"introduced":"0"},{"fixed":"1.0"}]}]}]}
        ]}}"#;
        let index = build_index(json.as_bytes()).unwrap();
        let records = &index["wax-test-a"];
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].display_id(), "CVE-0000-0002");
        // The only entry names another package, so nothing is comparable.
        assert_eq!(records[0].range_status("2.0"), None);
    }

    #[test]
    fn summary_falls_back_to_first_prose_line_of_details() {
        assert_eq!(
            summary_from_details("### Summary\n\nPyJWT accepts inconsistent keys.\nMore."),
            Some("PyJWT accepts inconsistent keys.".to_string())
        );
        assert_eq!(summary_from_details("## Only headers\n"), None);
        let long = "x".repeat(SUMMARY_MAX_CHARS + 5);
        assert_eq!(
            summary_from_details(&long).unwrap().chars().count(),
            SUMMARY_MAX_CHARS + 1
        );
    }

    #[test]
    fn bincode_round_trip_and_lazy_lookup() {
        let index = fixture_index();
        let db = fixture_db();
        assert_eq!(db.advisory_count(), 4);
        for (name, advisories) in &index {
            assert_eq!(&db.records_for(name).unwrap(), advisories);
        }
        assert!(db.records_for("not-a-formula").unwrap().is_empty());
    }

    #[test]
    fn corrupt_cache_is_rejected() {
        let mut payload = encode(&fixture_index()).unwrap();
        assert!(AdvisoryDb::from_bytes(b"WAXBIN1\0".to_vec()).is_err());
        payload.truncate(payload.len() - 10);
        assert!(AdvisoryDb::from_bytes(payload).is_err());
    }

    #[test]
    fn scan_classifies_installed_versions() {
        let db = fixture_db();
        let installed = [
            ("abcde", "2.9.3_1"),
            ("abi3audit", "0.0.26_5"),
            ("hermes-agent", "0.22.0"),
            ("lrzsz", "0.12.20_1"),
            ("wget", "1.25.0"),
            ("neovim", "HEAD-1a2b3c4"),
        ];
        let report = scan(&db, installed).unwrap();
        assert_eq!(report.checked, 5);
        assert_eq!(report.skipped, ["neovim"]);
        assert_eq!(report.open_count(), 2);

        let vulnerable: Vec<_> = report.vulnerable().map(|p| p.name.as_str()).collect();
        assert_eq!(vulnerable, ["abcde", "hermes-agent"]);

        let hermes = report
            .packages
            .iter()
            .find(|p| p.name == "hermes-agent")
            .unwrap();
        assert_eq!(hermes.upgrade_target(), Some("0.22.4"));
        let abcde = report.packages.iter().find(|p| p.name == "abcde").unwrap();
        assert_eq!(abcde.upgrade_target(), None);

        let lrzsz = report.packages.iter().find(|p| p.name == "lrzsz").unwrap();
        assert!(lrzsz.open.is_empty());
        assert_eq!(lrzsz.patched.len(), 1);
    }

    #[test]
    fn severity_filter_drops_lower_and_unrated() {
        let db = fixture_db();
        let mut report = scan(&db, [("abcde", "2.9.3_1"), ("hermes-agent", "0.22.0")]).unwrap();
        for pkg in &mut report.packages {
            pkg.retain_min_severity(Severity::High);
        }
        let vulnerable: Vec<_> = report.vulnerable().map(|p| p.name.as_str()).collect();
        assert_eq!(vulnerable, ["abcde"]);
    }

    #[tokio::test]
    async fn store_uses_fresh_cache_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let store = AdvisoryStore::for_dir(dir.path());
        assert!(!store.is_present());

        store
            .write_atomic(&store.bin_path(), &encode(&fixture_index()).unwrap())
            .await
            .unwrap();
        store
            .save_meta(&AdvisoryMeta {
                fetched_at: now_unix(),
                etag: Some("\"test\"".to_string()),
                last_modified: None,
            })
            .await
            .unwrap();

        assert!(store.is_present());
        let db = store.load(Freshness::IfStale, false).await.unwrap();
        assert_eq!(db.advisory_count(), 4);
    }
}
