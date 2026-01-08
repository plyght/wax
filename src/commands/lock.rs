use crate::error::Result;
use crate::lockfile::Lockfile;
use crate::ui::print_success;
use console::style;
use tracing::instrument;

#[instrument]
pub async fn lock() -> Result<()> {
    println!(
        "{} Generating lockfile from installed packages...",
        style("→").cyan().bold()
    );

    let lockfile = Lockfile::generate().await?;
    let package_count = lockfile.packages.len();

    if package_count == 0 {
        println!(
            "{} No packages installed. Lockfile not created.",
            style("ℹ").blue().bold()
        );
        return Ok(());
    }

    let lockfile_path = Lockfile::default_path();
    lockfile.save(&lockfile_path).await?;

    print_success(&format!(
        "Locked {} {} in wax.lock",
        package_count,
        if package_count == 1 {
            "package"
        } else {
            "packages"
        }
    ));

    Ok(())
}
