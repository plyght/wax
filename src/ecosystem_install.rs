//! Route `wax install` to Homebrew-style formulae, Scoop, winget-pkgs portable zips,
//! or Chocolatey `.nupkg` tools, including bang prefixes and automatic source pick.

use crate::cache::Cache;
use crate::chocolatey;
use crate::error::{Result, WaxError};
use crate::package_spec::{Ecosystem, PackageSpec};
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

    if cask {
        return Ok(false);
    }

    if spec.force == Some(Ecosystem::Brew) {
        return Ok(false);
    }

    if let Some(forced) = spec.force {
        install_forced(forced, &spec.name, dry_run).await?;
        return Ok(true);
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(eco) = auto_pick_ecosystem(cache, &spec.name).await? {
            if eco == Ecosystem::Brew {
                return Ok(false);
            }
            install_forced(eco, &spec.name, dry_run).await?;
            return Ok(true);
        }
        return Err(WaxError::FormulaNotFound(format!(
            "no matching package '{}' in brew index, Scoop Main, winget-pkgs, or Chocolatey",
            spec.name
        )));
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
    if !n.chars().all(|c| c.is_alphanumeric() || "-_.+".contains(c)) {
        return Err(WaxError::InvalidInput(format!(
            "unsupported characters in package id: {n}"
        )));
    }
    Ok(())
}

async fn install_forced(eco: Ecosystem, name: &str, dry_run: bool) -> Result<()> {
    if dry_run {
        println!("dry-run: would install via {} → {}", eco.label(), name);
        return Ok(());
    }

    match eco {
        Ecosystem::Brew => Ok(()),
        Ecosystem::Scoop => scoop::install_from_bucket(name, None).await,
        Ecosystem::Winget => winget_install::install_winget_package(name).await,
        Ecosystem::Chocolatey => chocolatey::install_portable_tools(name).await,
    }
}

#[cfg(target_os = "windows")]
async fn auto_pick_ecosystem(cache: &Cache, name: &str) -> Result<Option<Ecosystem>> {
    let formulae = cache.load_all_formulae().await?;
    let brew_hit = formulae.iter().any(|f| f.name.eq_ignore_ascii_case(name));

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
    if brew_hit {
        opts.push((Ecosystem::Brew, Ecosystem::Brew.speed_rank()));
    }
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
