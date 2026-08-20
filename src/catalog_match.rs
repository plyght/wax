//! Shared catalogue search scoring (Homebrew names, Scoop/winget/choco ids).

pub fn catalog_match_score(name: &str, query: &str) -> Option<i32> {
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
        let q = query.to_lowercase();
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
    fn exact_beats_prefix_beats_substring() {
        assert_eq!(catalog_match_score("ripgrep", "ripgrep"), Some(1000));
        assert_eq!(catalog_match_score("ripgrep", "rip"), Some(900));
        assert_eq!(catalog_match_score("libripgrep", "ripgrep"), Some(850));
        let exact = catalog_match_score("ripgrep", "ripgrep").unwrap();
        let prefix = catalog_match_score("ripgrep-bin", "ripgrep").unwrap();
        let substring = catalog_match_score("my-ripgrep-fork", "ripgrep").unwrap();
        assert!(exact > prefix && prefix > substring);
    }

    #[test]
    fn matching_is_case_insensitive_both_ways() {
        assert_eq!(
            catalog_match_score("JesseDuffield.lazygit", "jesseduffield.lazygit"),
            Some(1000)
        );
        assert_eq!(
            catalog_match_score("Microsoft.WindowsTerminal", "WINDOWSTERMINAL"),
            Some(850)
        );
    }

    #[test]
    fn word_boundary_hit_scores_as_substring() {
        // A word-exact or word-prefix hit is always also a substring hit, so the
        // 800/700 tiers are only reachable when the substring check already fired.
        assert_eq!(catalog_match_score("gnu-tar", "tar"), Some(850));
        assert_eq!(catalog_match_score("gnu.tarball", "tar"), Some(850));
    }

    #[test]
    fn no_match_returns_none() {
        assert!(catalog_match_score("ripgrep", "lazygit").is_none());
        assert!(catalog_match_score("git", "gitgit").is_none());
        assert!(catalog_match_score("", "git").is_none());
    }

    #[test]
    fn empty_query_matches_everything_as_prefix() {
        assert_eq!(catalog_match_score("anything", ""), Some(900));
        assert_eq!(catalog_match_score("", ""), Some(1000));
    }

    #[test]
    fn dotted_winget_ids_match_on_either_half() {
        assert_eq!(
            catalog_match_score("JesseDuffield.lazygit", "lazygit"),
            Some(850)
        );
        assert_eq!(
            catalog_match_score("JesseDuffield.lazygit", "JesseDuffield"),
            Some(900)
        );
    }

    #[test]
    fn non_ascii_names_do_not_panic() {
        assert!(catalog_match_score("Grüße", "grüße").is_some());
        assert!(catalog_match_score("İstanbul", "istanbul").is_none());
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn desc_never_outranks_a_name_hit() {
        // Name hits start at 700; description hits cap at 300.
        assert_eq!(
            match_score("ripgrep", Some("ripgrep is a grep"), "ripgrep"),
            Some(1000)
        );
        assert_eq!(match_score("foo", Some("nothing here"), "ripgrep"), None);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn hyphenated_query_matches_spaced_description() {
        assert_eq!(
            match_score("foo", Some("an agent browser tool"), "agent-browser"),
            Some(250)
        );
        assert_eq!(
            match_score("foo", Some("an agent-browser tool"), "agent-browser"),
            Some(300)
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn missing_desc_falls_back_to_name_score() {
        assert_eq!(match_score("ripgrep", None, "rip"), Some(900));
        assert_eq!(match_score("ripgrep", None, "lazygit"), None);
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
