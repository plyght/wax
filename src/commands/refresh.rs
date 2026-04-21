use crate::cache::Cache;
use crate::cask::CaskState;
use crate::discovery::{discover_linux_system_packages, discover_manually_installed_casks};
use crate::error::Result;
use crate::install::InstallState;
use crate::lockfile::{Lockfile, LockfileCask, LockfilePackage};
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn refresh(cache: &Cache) -> Result<()> {
    cache.ensure_fresh().await?;

    let formulae = cache.load_all_formulae().await?;
    let casks = cache.load_casks().await?;

    let state = InstallState::new()?;
    state.sync_from_cellar().await?;

    let cask_state = CaskState::new()?;
    let mut installed_casks = cask_state.load().await?;

    if cfg!(target_os = "macos") {
        for (name, cask) in discover_manually_installed_casks(&casks).await? {
            installed_casks.entry(name).or_insert(cask);
        }
        cask_state.save(&installed_casks).await?;
    }

    let mut lockfile = Lockfile::new();

    let installed_packages = state.load().await?;
    for (name, pkg) in installed_packages {
        lockfile.packages.insert(
            name,
            LockfilePackage {
                version: pkg.version,
                bottle: pkg.platform,
            },
        );
    }

    for (name, cask) in installed_casks {
        lockfile.casks.insert(
            name,
            LockfileCask {
                version: cask.version,
            },
        );
    }

    if cfg!(target_os = "linux") {
        for (name, package) in discover_linux_system_packages(&formulae).await? {
            lockfile.packages.entry(name).or_insert(LockfilePackage {
                version: package.version,
                bottle: package.platform,
            });
        }
    }

    let lockfile_path = Lockfile::default_path();
    lockfile.save(&lockfile_path).await?;

    Ok(())
}
