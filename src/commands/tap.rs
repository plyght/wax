use crate::cache::Cache;
use crate::error::Result;
use crate::tap::{TapKind, TapManager};
use console::style;

pub async fn tap(action: Option<crate::TapAction>, cache: Option<&Cache>) -> Result<()> {
    let mut manager = TapManager::new()?;
    manager.load().await?;

    match action {
        Some(crate::TapAction::Add { tap }) => {
            manager.add_tap(&tap).await?;
            if let Some(cache) = cache {
                cache.invalidate_all_tap_caches().await?;
            }
            println!("{} tap {}", style("+").green(), style(&tap).magenta());
        }
        Some(crate::TapAction::Remove { tap }) => {
            let tap_spec = crate::tap::Tap::from_spec(&tap)?;
            let full_name = tap_spec.full_name.clone();
            manager.remove_tap(&tap).await?;
            if let Some(cache) = cache {
                cache.invalidate_tap_cache(&full_name).await?;
            }
            println!("{} tap {}", style("-").red(), style(&tap).magenta());
        }
        Some(crate::TapAction::Update { tap }) => {
            let tap_spec = crate::tap::Tap::from_spec(&tap)?;
            let is_local = matches!(
                tap_spec.kind,
                TapKind::LocalDir { .. } | TapKind::LocalFile { .. }
            );

            manager.update_tap(&tap).await?;
            if let Some(cache) = cache {
                cache.invalidate_tap_cache(&tap_spec.full_name).await?;
            }
            if is_local {
                println!(
                    "{} tap {} {}",
                    style("✓").green(),
                    style(&tap).magenta(),
                    style("(local, refreshed cache)").dim()
                );
            } else {
                println!(
                    "{} updated tap {}",
                    style("✓").green(),
                    style(&tap).magenta()
                );
            }
        }
        Some(crate::TapAction::List) | None => {
            let taps = manager.list_taps();

            if taps.is_empty() {
                println!("no custom taps installed");
            } else {
                println!();
                for tap in &taps {
                    let kind_label = match &tap.kind {
                        TapKind::GitHub { .. } => style("(github)").dim(),
                        TapKind::Git { .. } => style("(git)").dim(),
                        TapKind::LocalDir { .. } => style("(local dir)").yellow(),
                        TapKind::LocalFile { .. } => style("(local file)").yellow(),
                    };
                    let url_str = tap.url().unwrap_or_default();
                    println!(
                        "{} {} {}",
                        style(&tap.full_name).magenta(),
                        kind_label,
                        style(&url_str).dim()
                    );
                }
                println!(
                    "\n{} {} installed",
                    style(taps.len()).cyan(),
                    if taps.len() == 1 { "tap" } else { "taps" }
                );
            }
        }
    }

    Ok(())
}
