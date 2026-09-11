//! Shared catalogue search scoring (Homebrew names, Scoop/winget/choco ids).

pub fn catalog_match_score(name: &str, query: &str) -> Option<i32> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
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
    let words: Vec<&str> = n.split(|c: char| !c.is_alphanumeric()).collect();
    for word in &words {
        if *word == q {
            return Some(800);
        }
    }
    for word in &words {
        if word.starts_with(&q) {
            return Some(700);
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
pub fn match_score(name: &str, desc: Option<&str>, query: &str) -> Option<i32> {
    let mut best = catalog_match_score(name, query);
    if let Some(desc) = desc {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return best;
        }
        let desc_lower = desc.to_lowercase();
        if desc_lower.contains(&q) {
            best = Some(best.map_or(300, |s| s.max(300)));
        } else if q.contains('-') {
            let q_spaces = q.replace('-', " ");
            if desc_lower.contains(&q_spaces) {
                best = Some(best.map_or(250, |s| s.max(250)));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_loose_matches() {
        assert!(catalog_match_score("antigravity", "agent-browser").is_none());
        assert_eq!(
            catalog_match_score("agent-browser", "agent-browser"),
            Some(1000)
        );
    }

    #[test]
    fn score_ladder_is_ordered_by_match_quality() {
        assert_eq!(catalog_match_score("ripgrep", "ripgrep"), Some(1000));
        assert_eq!(catalog_match_score("ripgrep-all", "ripgrep"), Some(900));
        assert_eq!(catalog_match_score("go-ripgrep", "ripgrep"), Some(850));
        assert_eq!(catalog_match_score("nothing", "ripgrep"), None);
    }

    #[test]
    fn substring_tier_absorbs_word_tiers() {
        // Any whole-word or word-prefix hit is also a substring hit, so the 800/700
        // tiers below `contains` are never reached.
        assert_eq!(catalog_match_score("go ripgrep tool", "ripgrep"), Some(850));
        assert_eq!(catalog_match_score("go ripgrepall", "ripgrep"), Some(850));
    }

    #[test]
    fn scoring_is_case_insensitive_both_ways() {
        assert_eq!(
            catalog_match_score("JesseDuffield.lazygit", "jesseduffield.lazygit"),
            Some(1000)
        );
        assert_eq!(
            catalog_match_score("microsoft.windowsterminal", "Microsoft.WindowsTerminal"),
            Some(1000)
        );
    }

    #[test]
    fn dotted_winget_ids_match_on_segment() {
        assert_eq!(
            catalog_match_score("JesseDuffield.lazygit", "lazygit"),
            Some(850)
        );
        assert_eq!(
            catalog_match_score("Microsoft.VisualStudioCode", "visualstudio"),
            Some(850)
        );
    }

    #[test]
    fn empty_query_matches_nothing() {
        assert_eq!(catalog_match_score("ripgrep", ""), None);
        assert_eq!(catalog_match_score("ripgrep", "   "), None);
        assert_eq!(catalog_match_score("", ""), None);
    }

    #[test]
    fn query_is_trimmed_before_matching() {
        assert_eq!(catalog_match_score("ripgrep", "  ripgrep "), Some(1000));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn empty_query_does_not_match_via_description() {
        assert_eq!(match_score("ripgrep", Some("search tool"), ""), None);
        assert_eq!(match_score("ripgrep", Some("search tool"), "  "), None);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn name_match_outranks_description_match() {
        assert_eq!(
            match_score("ripgrep", Some("ripgrep is a search tool"), "ripgrep"),
            Some(1000)
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn hyphenated_query_falls_back_to_spaced_description() {
        assert_eq!(
            match_score("rg", Some("recursive search tool"), "recursive-search"),
            Some(250)
        );
        assert_eq!(
            match_score("rg", Some("recursive search"), "no-match"),
            None
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn desc_boosts_score() {
        assert_eq!(
            match_score("foo", Some("agent browser tool"), "browser"),
            Some(300)
        );
    }
}
