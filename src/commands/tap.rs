use crate::error::Result;
use crate::tap::TapManager;
use console::style;

pub async fn tap(action: Option<crate::TapAction>) -> Result<()> {
    let mut manager = TapManager::new()?;
    manager.load().await?;

    match action {
        Some(crate::TapAction::Add { tap }) => {
            let parts: Vec<&str> = tap.split('/').collect();
            if parts.len() != 2 {
                eprintln!("✗ Invalid tap format. Use: user/repo");
                return Ok(());
            }

            let (user, repo) = (parts[0], parts[1]);

            manager.add_tap(user, repo).await?;
            println!();
            println!("+ tap {}", tap);
        }
        Some(crate::TapAction::Remove { tap }) => {
            let parts: Vec<&str> = tap.split('/').collect();
            if parts.len() != 2 {
                eprintln!("✗ Invalid tap format. Use: user/repo");
                return Ok(());
            }

            let (user, repo) = (parts[0], parts[1]);

            manager.remove_tap(user, repo).await?;
            println!();
            println!("- tap {}", tap);
        }
        Some(crate::TapAction::Update { tap }) => {
            let parts: Vec<&str> = tap.split('/').collect();
            if parts.len() != 2 {
                eprintln!("✗ Invalid tap format. Use: user/repo");
                return Ok(());
            }

            let (user, repo) = (parts[0], parts[1]);

            manager.update_tap(user, repo).await?;
            println!();
            println!("✓ updated tap {}", tap);
        }
        Some(crate::TapAction::List) | None => {
            let taps = manager.list_taps();

            if taps.is_empty() {
                println!("No custom taps");
            } else {
                println!();
                for tap in taps {
                    println!("{} {}", style(&tap.full_name).dim(), style(&tap.url).dim());
                }
            }
        }
    }

    Ok(())
}
