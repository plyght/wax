use crate::cache::Cache;
use crate::cask::CaskState;
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

    let mut installed_packages = state.load().await?;
    let cask_state = CaskState::new()?;
    let mut installed_casks = cask_state.load().await?;

    if cfg!(target_os = "linux") {
        for (name, package) in discover_linux_system_packages(&formulae).await? {
            installed_packages.entry(name).or_insert(package);
        }
    }

    if cfg!(target_os = "macos") {
        for (name, cask) in discover_manually_installed_casks(&casks).await? {
            installed_casks.entry(name).or_insert(cask);
        }
    }

    let lockfile = Lockfile {
        packages: installed_packages
            .into_iter()
            .map(|(name, pkg)| {
                (
                    name,
                    LockfilePackage {
                        version: pkg.version,
                        bottle: pkg.platform,
                    },
                )
            })
            .collect(),
        casks: installed_casks
            .into_iter()
            .map(|(name, cask)| {
                (
                    name,
                    LockfileCask {
                        version: cask.version,
                    },
                )
            })
            .collect(),
    };

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
