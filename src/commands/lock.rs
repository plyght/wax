use crate::cache::Cache;
use crate::discovery::{discover_linux_system_packages, discover_manually_installed_casks};
use crate::error::Result;
use crate::install::InstallState;
use crate::lockfile::{Lockfile, LockfileCask, LockfilePackage};
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn lock(cache: &Cache) -> Result<()> {
    let formulae = cache.load_formulae().await?;
    let casks = cache.load_casks().await?;

    let state = InstallState::new()?;
    state.sync_from_cellar().await?;

    let mut lockfile = Lockfile::generate().await?;

    if cfg!(target_os = "linux") {
        for (name, package) in discover_linux_system_packages(&formulae).await? {
            lockfile.packages.entry(name).or_insert(LockfilePackage {
                version: package.version,
                bottle: package.platform,
            });
        }
    }

    if cfg!(target_os = "macos") {
        for (name, cask) in discover_manually_installed_casks(&casks).await? {
            lockfile.casks.entry(name).or_insert(LockfileCask {
                version: cask.version,
            });
        }
    }

    let package_count = lockfile.packages.len();
    let cask_count = lockfile.casks.len();

    if package_count == 0 && cask_count == 0 {
        println!("no packages or casks installed");
        return Ok(());
    }

    let lockfile_path = Lockfile::default_path();
    lockfile.save(&lockfile_path).await?;

    println!(
        "locked {} {} and {} {} in wax.lock",
        package_count,
        if package_count == 1 {
            "package"
        } else {
            "packages"
        },
        cask_count,
        if cask_count == 1 { "cask" } else { "casks" }
    );

    Ok(())
}
