use crate::error::Result;
use crate::lockfile::Lockfile;
use tracing::instrument;

#[instrument]
pub async fn lock() -> Result<()> {
    let lockfile = Lockfile::generate().await?;
    let package_count = lockfile.packages.len();

    if package_count == 0 {
        println!("no packages installed");
        return Ok(());
    }

    let lockfile_path = Lockfile::default_path();
    lockfile.save(&lockfile_path).await?;

    println!();
    println!(
        "locked {} {} in wax.lock",
        package_count,
        if package_count == 1 {
            "package"
        } else {
            "packages"
        }
    );

    Ok(())
}
