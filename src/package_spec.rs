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
    const PAIRS: &[(&str, Ecosystem)] = &[
        ("chocolatey", Ecosystem::Chocolatey),
        ("choco", Ecosystem::Chocolatey),
        ("scoop", Ecosystem::Scoop),
        ("winget", Ecosystem::Winget),
        ("brew", Ecosystem::Brew),
        ("homebrew", Ecosystem::Brew),
    ];
    if let Some((head, rest)) = raw.split_once('/') {
        for (prefix, eco) in PAIRS {
            if head.eq_ignore_ascii_case(prefix) {
                return PackageSpec {
                    force: Some(*eco),
                    name: rest.to_string(),
                };
            }
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
    fn prefix_match_is_ascii_case_insensitive_only() {
        for raw in ["SCOOP/ripgrep", "ScOoP/ripgrep", "scoop/ripgrep"] {
            let spec = parse_package_spec(raw);
            assert_eq!(spec.force, Some(Ecosystem::Scoop), "{raw}");
            assert_eq!(spec.name, "ripgrep", "{raw}");
        }
    }

    #[test]
    fn non_ascii_input_does_not_panic_or_match() {
        for raw in ["İscoop/ripgrep", "ſcoop/ripgrep", "grüße/paket", "Ω"] {
            let spec = parse_package_spec(raw);
            assert_eq!(spec.force, None, "{raw}");
            assert_eq!(spec.name, raw, "{raw}");
        }
    }

    #[test]
    fn name_case_and_dots_are_preserved() {
        let spec = parse_package_spec("WINGET/JesseDuffield.lazygit");
        assert_eq!(spec.force, Some(Ecosystem::Winget));
        assert_eq!(spec.name, "JesseDuffield.lazygit");
    }

    #[test]
    fn only_the_first_segment_is_treated_as_a_prefix() {
        let spec = parse_package_spec("scoop/extras/foo");
        assert_eq!(spec.force, Some(Ecosystem::Scoop));
        assert_eq!(spec.name, "extras/foo");

        let spec = parse_package_spec("user/scoop/foo");
        assert_eq!(spec.force, None);
        assert_eq!(spec.name, "user/scoop/foo");
    }

    #[test]
    fn homebrew_tap_style_names_stay_unqualified() {
        let spec = parse_package_spec("homebrew-core/git");
        assert_eq!(spec.force, None);
        assert_eq!(spec.name, "homebrew-core/git");
    }

    #[test]
    fn empty_input_is_a_plain_empty_name() {
        let spec = parse_package_spec("");
        assert_eq!(spec.force, None);
        assert_eq!(spec.name, "");

        let spec = parse_package_spec("/foo");
        assert_eq!(spec.force, None);
        assert_eq!(spec.name, "/foo");
    }

    #[test]
    fn parse_search_query_leaves_unqualified_queries_alone() {
        let (f, q) = parse_search_query("lazygit");
        assert_eq!(f, None);
        assert_eq!(q, "lazygit");
    }

    #[test]
    fn labels_round_trip_through_parsing() {
        for eco in [
            Ecosystem::Brew,
            Ecosystem::Scoop,
            Ecosystem::Winget,
            Ecosystem::Chocolatey,
        ] {
            let spec = parse_package_spec(&format!("{}/pkg", eco.label()));
            assert_eq!(spec.force, Some(eco), "{}", eco.label());
            assert_eq!(spec.name, "pkg");
        }
    }

    #[test]
    fn speed_rank_orders_fastest_first() {
        assert!(Ecosystem::Brew.speed_rank() < Ecosystem::Scoop.speed_rank());
        assert!(Ecosystem::Scoop.speed_rank() < Ecosystem::Winget.speed_rank());
        assert!(Ecosystem::Winget.speed_rank() < Ecosystem::Chocolatey.speed_rank());
    }
}
