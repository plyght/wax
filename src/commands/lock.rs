use crate::adopt::{self, AdoptOptions};
use crate::cache::Cache;
use crate::error::Result;
use crate::lockfile::{Lockfile, LockfileCask, LockfilePackage};
use tracing::instrument;

#[instrument(skip(cache))]
pub async fn lock(cache: &Cache) -> Result<()> {
    let snapshot = adopt::sync_installed_state(cache, AdoptOptions::default()).await?;

    let mut lockfile = Lockfile::new();

    for (name, pkg) in snapshot.formulae {
        lockfile.packages.insert(
            name,
            LockfilePackage {
                version: pkg.version,
                bottle: pkg.platform,
            },
        );
    }

    for (name, cask) in snapshot.casks {
        lockfile.casks.insert(
            name,
            LockfileCask {
                version: cask.version,
            },
        );
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
