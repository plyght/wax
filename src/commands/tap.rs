use crate::error::Result;
use crate::tap::TapManager;
use crate::ui::print_success;
use console::style;

pub async fn tap(action: Option<crate::TapAction>) -> Result<()> {
    let mut manager = TapManager::new()?;
    manager.load().await?;

    match action {
        Some(crate::TapAction::Add { tap }) => {
            let parts: Vec<&str> = tap.split('/').collect();
            if parts.len() != 2 {
                eprintln!(
                    "{} Invalid tap format. Use: user/repo",
                    style("✗").red().bold()
                );
                return Ok(());
            }

            let (user, repo) = (parts[0], parts[1]);

            println!("{} Adding tap: {}", style("→").cyan().bold(), tap);
            manager.add_tap(user, repo).await?;
            print_success(&format!("Added tap {}", tap));
        }
        Some(crate::TapAction::Remove { tap }) => {
            let parts: Vec<&str> = tap.split('/').collect();
            if parts.len() != 2 {
                eprintln!(
                    "{} Invalid tap format. Use: user/repo",
                    style("✗").red().bold()
                );
                return Ok(());
            }

            let (user, repo) = (parts[0], parts[1]);

            println!("{} Removing tap: {}", style("→").cyan().bold(), tap);
            manager.remove_tap(user, repo).await?;
            print_success(&format!("Removed tap {}", tap));
        }
        Some(crate::TapAction::Update { tap }) => {
            let parts: Vec<&str> = tap.split('/').collect();
            if parts.len() != 2 {
                eprintln!(
                    "{} Invalid tap format. Use: user/repo",
                    style("✗").red().bold()
                );
                return Ok(());
            }

            let (user, repo) = (parts[0], parts[1]);

            println!("{} Updating tap: {}", style("→").cyan().bold(), tap);
            manager.update_tap(user, repo).await?;
            print_success(&format!("Updated tap {}", tap));
        }
        Some(crate::TapAction::List) | None => {
            let taps = manager.list_taps();

            if taps.is_empty() {
                println!("{} No custom taps installed", style("ℹ").blue().bold());
            } else {
                println!("{} Installed taps:", style("📦").cyan().bold());
                for tap in taps {
                    println!("  {} ({})", style(&tap.full_name).green(), tap.url);
                }
            }
        }
    }

    Ok(())
}
