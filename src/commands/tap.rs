use crate::error::Result;
use crate::tap::TapManager;
use console::style;

pub async fn tap(action: Option<crate::TapAction>) -> Result<()> {
    let mut manager = TapManager::new()?;
    manager.load().await?;

    match action {
        Some(crate::TapAction::Add { tap }) => {
            manager.add_tap(&tap).await?;
            println!();
            println!("+ tap {}", tap);
        }
        Some(crate::TapAction::Remove { tap }) => {
            manager.remove_tap(&tap).await?;
            println!();
            println!("- tap {}", tap);
        }
        Some(crate::TapAction::Update { tap }) => {
            manager.update_tap(&tap).await?;
            println!();
            println!("updated tap {}", tap);
        }
        Some(crate::TapAction::List) | None => {
            let taps = manager.list_taps();

            if taps.is_empty() {
                println!("no custom taps");
            } else {
                println!();
                for tap in taps {
                    let url_str = tap.url().unwrap_or_else(|| "local".to_string());
                    println!("{} {}", style(&tap.full_name).magenta(), url_str);
                }
            }
        }
    }

    Ok(())
}
