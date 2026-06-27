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
    fn desc_boosts_score() {
        assert_eq!(
            match_score("foo", Some("agent browser tool"), "browser"),
            Some(300)
        );
    }
}
