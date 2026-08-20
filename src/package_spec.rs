//! Qualified package names: `scoop/ripgrep`, `choco/git`, `winget/JesseDuffield.lazygit`,
//! `brew/openssl` (force Homebrew), or plain `ripgrep` for automatic source selection.

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Ecosystem {
    /// Local Homebrew-style index (fastest: cached JSON).
    Brew,
    /// Scoop Main-style JSON manifest + zip/tar.gz portable.
    Scoop,
    /// winget-pkgs YAML portable zip installers.
    Winget,
    /// Chocolatey community `.nupkg` (portable `tools/*.exe` only).
    Chocolatey,
}

impl Ecosystem {
    /// Lower is faster / preferred when the same logical package exists in multiple ecosystems.
    pub fn speed_rank(self) -> u8 {
        match self {
            Ecosystem::Brew => 0,
            Ecosystem::Scoop => 1,
            Ecosystem::Winget => 2,
            Ecosystem::Chocolatey => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Ecosystem::Brew => "brew",
            Ecosystem::Scoop => "scoop",
            Ecosystem::Winget => "winget",
            Ecosystem::Chocolatey => "choco",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PackageSpec {
    /// When set, install/search only this ecosystem.
    pub force: Option<Ecosystem>,
    /// Unqualified package id (no bang prefix).
    pub name: String,
}

/// Parse `chocolatey/foo`, `choco/foo`, `scoop/foo`, `winget/foo`, `brew/foo`, `homebrew/foo`.
pub fn parse_package_spec(raw: &str) -> PackageSpec {
    let lower = raw.to_lowercase();
    const PAIRS: &[(&str, Ecosystem)] = &[
        ("chocolatey/", Ecosystem::Chocolatey),
        ("choco/", Ecosystem::Chocolatey),
        ("scoop/", Ecosystem::Scoop),
        ("winget/", Ecosystem::Winget),
        ("brew/", Ecosystem::Brew),
        ("homebrew/", Ecosystem::Brew),
    ];
    for (prefix, eco) in PAIRS {
        if lower.starts_with(prefix) {
            return PackageSpec {
                force: Some(*eco),
                name: raw[prefix.len()..].to_string(),
            };
        }
    }
    PackageSpec {
        force: None,
        name: raw.to_string(),
    }
}

/// Strip a search query bang for remote search (same rules as install).
pub fn parse_search_query(raw: &str) -> (Option<Ecosystem>, String) {
    let spec = parse_package_spec(raw);
    (spec.force, spec.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bangs_case_insensitive_prefix() {
        let s = parse_package_spec("Scoop/RipGrep");
        assert_eq!(s.force, Some(Ecosystem::Scoop));
        assert_eq!(s.name, "RipGrep");
    }

    #[test]
    fn plain_name_is_auto() {
        let s = parse_package_spec("ripgrep");
        assert!(s.force.is_none());
        assert_eq!(s.name, "ripgrep");
    }

    #[test]
    fn parses_all_known_ecosystems() {
        let cases = vec![
            ("chocolatey/foo", Ecosystem::Chocolatey),
            ("choco/foo", Ecosystem::Chocolatey),
            ("scoop/foo", Ecosystem::Scoop),
            ("winget/foo", Ecosystem::Winget),
            ("brew/foo", Ecosystem::Brew),
            ("homebrew/foo", Ecosystem::Brew),
        ];

        for (input, expected_eco) in cases {
            let spec = parse_package_spec(input);
            assert_eq!(spec.force, Some(expected_eco));
            assert_eq!(spec.name, "foo");
        }
    }

    #[test]
    fn unrecognized_prefixes_are_plain_names() {
        let spec = parse_package_spec("apt/foo");
        assert_eq!(spec.force, None);
        assert_eq!(spec.name, "apt/foo");

        let spec = parse_package_spec("npm/bar");
        assert_eq!(spec.force, None);
        assert_eq!(spec.name, "npm/bar");
    }

    #[test]
    fn prefix_without_slash_is_plain_name() {
        let spec = parse_package_spec("brew-foo");
        assert_eq!(spec.force, None);
        assert_eq!(spec.name, "brew-foo");
    }

    #[test]
    fn empty_name_after_prefix() {
        let spec = parse_package_spec("brew/");
        assert_eq!(spec.force, Some(Ecosystem::Brew));
        assert_eq!(spec.name, "");
    }

    #[test]
    fn parse_search_query_strips_known_prefixes() {
        let (f, q) = parse_search_query("choco/git");
        assert_eq!(f, Some(Ecosystem::Chocolatey));
        assert_eq!(q, "git");
        let (f, q) = parse_search_query("winget/Microsoft.WindowsTerminal");
        assert_eq!(f, Some(Ecosystem::Winget));
        assert_eq!(q, "Microsoft.WindowsTerminal");
    }

    #[test]
    fn uppercase_prefixes_are_recognised() {
        for raw in ["SCOOP/foo", "Choco/foo", "WINGET/foo", "HomeBrew/foo"] {
            assert!(
                parse_package_spec(raw).force.is_some(),
                "prefix not matched: {raw}"
            );
            assert_eq!(parse_package_spec(raw).name, "foo");
        }
    }

    #[test]
    fn name_case_is_preserved_for_winget_ids() {
        let spec = parse_package_spec("winget/JesseDuffield.lazygit");
        assert_eq!(spec.force, Some(Ecosystem::Winget));
        assert_eq!(spec.name, "JesseDuffield.lazygit");
    }

    #[test]
    fn chocolatey_prefix_wins_over_choco_prefix() {
        let spec = parse_package_spec("chocolatey/git");
        assert_eq!(spec.force, Some(Ecosystem::Chocolatey));
        assert_eq!(spec.name, "git");
    }

    #[test]
    fn extra_slashes_stay_in_the_name() {
        let spec = parse_package_spec("scoop/extras/foo");
        assert_eq!(spec.force, Some(Ecosystem::Scoop));
        assert_eq!(spec.name, "extras/foo");
    }

    #[test]
    fn homebrew_tap_names_are_not_treated_as_prefixes() {
        let spec = parse_package_spec("homebrew/cask/firefox");
        assert_eq!(spec.force, Some(Ecosystem::Brew));
        assert_eq!(spec.name, "cask/firefox");

        let spec = parse_package_spec("plyght/tap/wax");
        assert_eq!(spec.force, None);
        assert_eq!(spec.name, "plyght/tap/wax");
    }

    #[test]
    fn label_round_trips_through_the_parser() {
        for eco in [
            Ecosystem::Brew,
            Ecosystem::Scoop,
            Ecosystem::Winget,
            Ecosystem::Chocolatey,
        ] {
            let raw = format!("{}/pkg", eco.label());
            let spec = parse_package_spec(&raw);
            assert_eq!(spec.force, Some(eco), "round trip failed for {raw}");
            assert_eq!(spec.name, "pkg");
        }
    }

    #[test]
    fn parse_search_query_leaves_plain_queries_untouched() {
        let (f, q) = parse_search_query("ripgrep");
        assert_eq!(f, None);
        assert_eq!(q, "ripgrep");
    }

    #[test]
    fn speed_rank_orders_fastest_first() {
        assert!(Ecosystem::Brew.speed_rank() < Ecosystem::Scoop.speed_rank());
        assert!(Ecosystem::Scoop.speed_rank() < Ecosystem::Winget.speed_rank());
        assert!(Ecosystem::Winget.speed_rank() < Ecosystem::Chocolatey.speed_rank());
    }
}
