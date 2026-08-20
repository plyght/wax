//! Route `wax install` to Homebrew-style formulae, Scoop, winget-pkgs portable zips,
//! or Chocolatey `.nupkg` tools, including bang prefixes and automatic source pick.

use crate::cache::Cache;
use crate::chocolatey;
use crate::error::{Result, WaxError};
use crate::package_spec::{Ecosystem, PackageSpec};

use crate::remote_search;
use crate::scoop;
use crate::winget_install;

/// Returns `true` if this package was fully handled (no Homebrew batch needed).
pub async fn install_one_qualified(
    cache: &Cache,
    raw: &str,
    dry_run: bool,
    cask: bool,
) -> Result<bool> {
    let spec = crate::package_spec::parse_package_spec(raw);
    validate_qualified_inner(&spec)?;
    crate::error::reject_brew_ecosystem(spec.force)?;

    if cask {
        return Err(WaxError::PlatformNotSupported(
            "Casks are not supported on Windows; use scoop/, winget/, or choco/".into(),
        ));
    }

    if let Some(forced) = spec.force {
        return install_forced_prefixed(cache, forced, &spec.name, dry_run).await;
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(eco) = auto_pick_ecosystem(cache, &spec.name).await? {
            install_forced(eco, &spec.name, dry_run).await?;
            return Ok(true);
        }
        let alts = catalog_alternatives(cache, &spec.name, None).await;
        Err(WaxError::FormulaNotFound(with_alternatives(
            format!(
                "'{}' is not published on any Windows catalogue (Scoop Main, winget-pkgs, Chocolatey)",
                spec.name
            ),
            &alts,
        )))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = cache;
        Ok(false)
    }
}

fn validate_qualified_inner(spec: &PackageSpec) -> Result<()> {
    let n = spec.name.trim();
    if n.is_empty() {
        return Err(WaxError::InvalidInput(
            "empty package name after prefix".into(),
        ));
    }
    if spec.force.is_some() && n.contains('/') {
        return Err(WaxError::InvalidInput(
            "names with '/' after a scoop/choco/winget/brew prefix are not supported".into(),
        ));
    }
    if n.chars().all(|c| c == '.') {
        return Err(WaxError::InvalidInput(format!(
            "unsupported package id: {n}"
        )));
    }
    if !n.chars().all(|c| c.is_alphanumeric() || "-_.+".contains(c)) {
        return Err(WaxError::InvalidInput(format!(
            "unsupported characters in package id: {n}"
        )));
    }
    Ok(())
}

fn catalog_repo_name(eco: Ecosystem) -> &'static str {
    match eco {
        Ecosystem::Scoop => "Scoop Main",
        Ecosystem::Winget => "winget-pkgs",
        Ecosystem::Chocolatey => "Chocolatey",
        Ecosystem::Brew => "Homebrew",
    }
}

fn with_alternatives(msg: String, alts: &[String]) -> String {
    if alts.is_empty() {
        return msg;
    }
    format!("{msg}\nTry: {}", alts.join(", "))
}

#[cfg(any(target_os = "windows", test))]
async fn package_in_catalog(eco: Ecosystem, name: &str) -> bool {
    match eco {
        Ecosystem::Scoop => scoop::scoop_manifest_exists(scoop::DEFAULT_BUCKET_BASE, name).await,
        Ecosystem::Chocolatey => chocolatey::package_exists(name).await,
        Ecosystem::Winget => {
            if name.contains('.') {
                winget_install::winget_package_exists(name).await
            } else {
                false
            }
        }
        Ecosystem::Brew => false,
    }
}

#[cfg(any(target_os = "windows", test))]
async fn catalog_alternatives(cache: &Cache, name: &str, skip: Option<Ecosystem>) -> Vec<String> {
    let mut out = Vec::new();
    for eco in [Ecosystem::Scoop, Ecosystem::Winget, Ecosystem::Chocolatey] {
        if skip == Some(eco) {
            continue;
        }
        if package_in_catalog(eco, name).await {
            out.push(format!("{}/{}", eco.label(), name));
        }
    }
    if !out.is_empty() {
        return out;
    }

    let include_scoop = skip != Some(Ecosystem::Scoop);
    let include_choco = skip != Some(Ecosystem::Chocolatey);
    let include_winget = skip != Some(Ecosystem::Winget);
    let Ok(hits) = remote_search::collect_remote_hits(
        cache,
        name,
        include_scoop,
        include_choco,
        include_winget,
    )
    .await
    else {
        return out;
    };
    for hit in remote_search::dedupe_remote_by_speed(hits)
        .into_iter()
        .take(5)
    {
        if skip == Some(hit.ecosystem) {
            continue;
        }
        let line = format!("{}/{}", hit.ecosystem.label(), hit.id);
        if !out.contains(&line) {
            out.push(line);
        }
    }
    out
}

#[cfg(any(target_os = "windows", test))]
async fn install_forced_prefixed(
    cache: &Cache,
    eco: Ecosystem,
    name: &str,
    dry_run: bool,
) -> Result<bool> {
    if dry_run {
        install_forced(eco, name, dry_run).await?;
        return Ok(true);
    }

    if !package_in_catalog(eco, name).await {
        let alts = catalog_alternatives(cache, name, Some(eco)).await;
        return Err(WaxError::FormulaNotFound(with_alternatives(
            format!(
                "'{name}' is not published on the {} repo",
                catalog_repo_name(eco)
            ),
            &alts,
        )));
    }

    match install_forced(eco, name, dry_run).await {
        Ok(()) => Ok(true),
        Err(err) => {
            let alts = catalog_alternatives(cache, name, Some(eco)).await;
            if alts.is_empty() {
                return Err(err);
            }
            Err(WaxError::InstallError(with_alternatives(
                err.to_string(),
                &alts,
            )))
        }
    }
}

async fn install_forced(eco: Ecosystem, name: &str, dry_run: bool) -> Result<()> {
    if dry_run {
        println!("dry-run: would install via {} → {}", eco.label(), name);
        return Ok(());
    }

    match eco {
        Ecosystem::Brew => Err(crate::error::homebrew_unavailable()),
        Ecosystem::Scoop => scoop::install_from_bucket(name, None).await,
        Ecosystem::Winget => winget_install::install_winget_package(name).await,
        Ecosystem::Chocolatey => chocolatey::install_portable_tools(name).await,
    }
}

#[cfg(any(target_os = "windows", test))]
async fn auto_pick_ecosystem(_cache: &Cache, name: &str) -> Result<Option<Ecosystem>> {
    let scoop_f = scoop::scoop_manifest_exists(scoop::DEFAULT_BUCKET_BASE, name);
    let choco_f = chocolatey::package_exists(name);
    let winget_f = async {
        if name.contains('.') {
            winget_install::winget_package_exists(name).await
        } else {
            false
        }
    };

    let (scoop_ok, choco_ok, winget_ok) = tokio::join!(scoop_f, choco_f, winget_f);

    let mut opts: Vec<(Ecosystem, u8)> = Vec::new();
    if scoop_ok {
        opts.push((Ecosystem::Scoop, Ecosystem::Scoop.speed_rank()));
    }
    if winget_ok {
        opts.push((Ecosystem::Winget, Ecosystem::Winget.speed_rank()));
    }
    if choco_ok {
        opts.push((Ecosystem::Chocolatey, Ecosystem::Chocolatey.speed_rank()));
    }

    opts.sort_by_key(|(_, r)| *r);
    Ok(opts.first().map(|(e, _)| *e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_spec::parse_package_spec;

    fn validate(raw: &str) -> Result<()> {
        validate_qualified_inner(&parse_package_spec(raw))
    }

    fn err(raw: &str) -> String {
        validate(raw).unwrap_err().to_string()
    }

    #[test]
    fn accepts_plain_and_qualified_ids() {
        for raw in [
            "ripgrep",
            "scoop/ripgrep",
            "winget/JesseDuffield.lazygit",
            "choco/git",
            "chocolatey/nodejs-lts",
            "scoop/7zip",
            "scoop/gcc_libs",
            "scoop/notepad++",
        ] {
            assert!(validate(raw).is_ok(), "{raw} should validate");
        }
    }

    #[test]
    fn rejects_empty_name_after_prefix() {
        assert!(err("scoop/").contains("empty package name"));
        assert!(err("scoop/   ").contains("empty package name"));
        assert!(err("").contains("empty package name"));
    }

    #[test]
    fn rejects_slashes_after_a_prefix() {
        let msg = err("scoop/extras/foo");
        assert!(msg.contains("not supported"), "{msg}");
    }

    #[test]
    fn rejects_shell_metacharacters_and_traversal() {
        for raw in [
            "scoop/rip;rm -rf /",
            "scoop/rip grep",
            "choco/$(whoami)",
            "winget/..",
            "scoop/rip\\grep",
            "choco/pkg&calc",
        ] {
            assert!(validate(raw).is_err(), "{raw} should be rejected");
        }
    }

    #[test]
    fn rejects_dot_only_ids_that_would_escape_the_staging_dir() {
        for raw in ["choco/..", "scoop/.", "winget/..."] {
            assert!(validate(raw).is_err(), "{raw} should be rejected");
        }
    }

    #[test]
    fn unsupported_character_error_names_the_id() {
        let msg = err("scoop/rip grep");
        assert!(msg.contains("unsupported characters"), "{msg}");
        assert!(msg.contains("rip grep"), "{msg}");
    }

    #[test]
    fn unqualified_names_with_slashes_are_rejected_as_bad_characters() {
        let msg = err("apt/foo");
        assert!(msg.contains("unsupported characters"), "{msg}");
    }

    #[test]
    fn with_alternatives_appends_only_when_present() {
        assert_eq!(with_alternatives("nope".into(), &[]), "nope");
        assert_eq!(
            with_alternatives("nope".into(), &["scoop/git".into(), "choco/git".into()]),
            "nope\nTry: scoop/git, choco/git"
        );
    }

    #[test]
    fn catalog_repo_names_are_human_readable() {
        assert_eq!(catalog_repo_name(Ecosystem::Scoop), "Scoop Main");
        assert_eq!(catalog_repo_name(Ecosystem::Winget), "winget-pkgs");
        assert_eq!(catalog_repo_name(Ecosystem::Chocolatey), "Chocolatey");
        assert_eq!(catalog_repo_name(Ecosystem::Brew), "Homebrew");
    }
}
