use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrewVersion {
    pub base: String,
    pub revision: u32,
}

impl BrewVersion {
    pub fn parse(version: &str) -> Self {
        if let Some(idx) = version.rfind('_') {
            let (base, rev_str) = version.split_at(idx);
            let rev_str = &rev_str[1..];
            if let Ok(revision) = rev_str.parse::<u32>() {
                return BrewVersion {
                    base: base.to_string(),
                    revision,
                };
            }
        }
        BrewVersion {
            base: version.to_string(),
            revision: 0,
        }
    }

    fn parse_semver_parts(s: &str) -> Vec<u64> {
        s.split('.')
            .filter_map(|part| {
                let numeric: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
                numeric.parse().ok()
            })
            .collect()
    }
}

impl Ord for BrewVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let self_parts = Self::parse_semver_parts(&self.base);
        let other_parts = Self::parse_semver_parts(&other.base);

        let max_len = self_parts.len().max(other_parts.len());
        for i in 0..max_len {
            let a = self_parts.get(i).copied().unwrap_or(0);
            let b = other_parts.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => continue,
                ord => return ord,
            }
        }

        if self_parts.len() != other_parts.len() || self.base != other.base {
            return self.base.cmp(&other.base);
        }

        self.revision.cmp(&other.revision)
    }
}

impl PartialOrd for BrewVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn is_same_or_newer(installed: &str, latest: &str) -> bool {
    let installed_v = BrewVersion::parse(installed);
    let latest_v = BrewVersion::parse(latest);
    installed_v >= latest_v
}

pub fn sort_versions(versions: &mut [String]) {
    versions.sort_by(|a, b| {
        let va = BrewVersion::parse(a);
        let vb = BrewVersion::parse(b);
        va.cmp(&vb)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_version() {
        let v = BrewVersion::parse("2.52.0");
        assert_eq!(v.base, "2.52.0");
        assert_eq!(v.revision, 0);
    }

    #[test]
    fn test_parse_revision() {
        let v = BrewVersion::parse("2.52.0_1");
        assert_eq!(v.base, "2.52.0");
        assert_eq!(v.revision, 1);
    }

    #[test]
    fn test_revision_is_newer() {
        let v1 = BrewVersion::parse("2.52.0");
        let v2 = BrewVersion::parse("2.52.0_1");
        assert!(v2 > v1, "2.52.0_1 should be newer than 2.52.0");
    }

    #[test]
    fn test_higher_revision_is_newer() {
        let v1 = BrewVersion::parse("2.52.0_1");
        let v2 = BrewVersion::parse("2.52.0_2");
        assert!(v2 > v1, "2.52.0_2 should be newer than 2.52.0_1");
    }

    #[test]
    fn test_semver_comparison() {
        let v1 = BrewVersion::parse("2.51.0");
        let v2 = BrewVersion::parse("2.52.0");
        assert!(v2 > v1);
    }

    #[test]
    fn test_is_same_or_newer() {
        assert!(is_same_or_newer("2.52.0_1", "2.52.0"));
        assert!(is_same_or_newer("2.52.0", "2.52.0"));
        assert!(!is_same_or_newer("2.51.0", "2.52.0"));
    }

    #[test]
    fn test_sort_versions() {
        let mut versions = vec![
            "2.52.0".to_string(),
            "2.52.0_1".to_string(),
            "2.51.0".to_string(),
            "2.52.0_2".to_string(),
        ];
        sort_versions(&mut versions);
        assert_eq!(versions, vec!["2.51.0", "2.52.0", "2.52.0_1", "2.52.0_2"]);
    }
}
