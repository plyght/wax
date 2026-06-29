use crate::cache::Cache;
use crate::error::{Result, WaxError};
use console::style;
use std::collections::HashMap;
use tracing::instrument;
fn is_safe_url(url_str: &str) -> bool {
    // Prevent command injection via shell metacharacters
    let dangerous_chars = [
        '`', '$', ';', '|', '<', '>', '"', '\'', '\\', '{', '}', '\n', '\r', '\t', ' ',
    ];
    if url_str.contains(dangerous_chars) {
        return false;
    }

    if let Ok(url) = reqwest::Url::parse(url_str) {
        matches!(url.scheme(), "http" | "https")
    } else {
        false
    }
}

#[instrument(skip(cache))]
pub async fn source(cache: &Cache, formula_name: &str) -> Result<()> {
    cache.ensure_fresh().await?;

    let formulae = cache.load_all_formulae().await?;
    let casks = cache.load_casks().await?;
    let formula_index: HashMap<_, _> = formulae
        .iter()
        .map(|f| (f.name.as_str(), f))
        .chain(formulae.iter().map(|f| (f.full_name.as_str(), f)))
        .collect();
    let cask_index: HashMap<_, _> = casks
        .iter()
        .map(|c| (c.token.as_str(), c))
        .chain(casks.iter().map(|c| (c.full_token.as_str(), c)))
        .collect();

    if let Some(formula) = formula_index.get(formula_name) {
        let homepage = &formula.homepage;
        if !is_safe_url(homepage) {
            return Err(WaxError::InvalidInput(format!(
                "Invalid or unsafe homepage URL: {}",
                homepage
            )));
        }
        println!(
            "{} → {}",
            style(formula_name).magenta(),
            style(homepage).cyan().underlined()
        );

        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(homepage).spawn();
        }

        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(homepage).spawn();
        }

        return Ok(());
    }

    if let Some(cask) = cask_index.get(formula_name) {
        let homepage = &cask.homepage;
        if !is_safe_url(homepage) {
            return Err(WaxError::InvalidInput(format!(
                "Invalid or unsafe homepage URL: {}",
                homepage
            )));
        }
        println!(
            "{} {} → {}",
            style(formula_name).magenta(),
            style("(cask)").yellow(),
            style(homepage).cyan().underlined()
        );

        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(homepage).spawn();
        }

        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(homepage).spawn();
        }

        return Ok(());
    }

    Err(WaxError::FormulaNotFound(formula_name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_url() {
        assert!(is_safe_url("http://example.com"));
        assert!(is_safe_url("https://example.com/path?query=1"));

        assert!(!is_safe_url("file:///etc/passwd"));
        assert!(!is_safe_url("ftp://example.com"));
        assert!(!is_safe_url("javascript:alert(1)"));
        assert!(!is_safe_url("-a Terminal"));
        assert!(!is_safe_url("/usr/bin/local"));
        assert!(!is_safe_url("example.com")); // Missing scheme

        // Command injection payloads
        assert!(!is_safe_url("http://example.com;rm -rf /"));
        assert!(!is_safe_url("http://example.com/$(whoami)"));
        assert!(!is_safe_url("http://example.com/`whoami`"));
        assert!(!is_safe_url("http://example.com' --no-sandbox"));
        assert!(!is_safe_url("http://example.com\" --no-sandbox"));
        assert!(!is_safe_url("http://example.com|whoami"));
    }
}
