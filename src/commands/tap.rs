use crate::cache::Cache;
use crate::error::Result;
use crate::tap::{TapKind, TapManager};
use console::style;

pub async fn tap(
    action: Option<crate::TapAction>,
    repair: bool,
    cache: Option<&Cache>,
) -> Result<()> {
    let mut manager = TapManager::new()?;
    manager.load().await?;

    if repair {
        let repaired = manager.repair_all().await?;
        if repaired.is_empty() {
            println!("{} all taps OK", style("✓").green());
        } else {
            for name in &repaired {
                println!("{} repaired {}", style("✓").green(), style(name).magenta());
            }
        }
        return Ok(());
    }

    match action {
        Some(crate::TapAction::Add { tap }) => {
            manager.add_tap(&tap).await?;
            if let Some(cache) = cache {
                cache.invalidate_all_tap_caches().await?;
            }
            println!("{} tap {}", style("+").green(), style(&tap).magenta());
        }
        Some(crate::TapAction::External(args)) => {
            // `wax tap user/repo` without the `add` subcommand — treat as add.
            let tap = args.into_iter().next().unwrap_or_default();
            if tap.is_empty() {
                return Err(crate::error::WaxError::InvalidInput(
                    "No tap specified".to_string(),
                ));
            }
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
        Some(crate::TapAction::List {
            tap: Some(tap_spec),
        }) => {
            list_tap_packages(&manager, &tap_spec).await?;
        }
        Some(crate::TapAction::List { tap: None }) => list_installed_taps(&manager),
        None => {
            list_installed_taps(&manager);
        }
    }

    Ok(())
}

fn list_installed_taps(manager: &TapManager) {
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

async fn list_tap_packages(manager: &TapManager, tap_spec: &str) -> Result<()> {
    let tap = manager.get_tap(tap_spec)?;
    let formulae = manager.load_formulae_from_tap(&tap).await?;
    let casks = manager.load_casks_from_tap(&tap).await?;

    if formulae.is_empty() && casks.is_empty() {
        println!("tap {} has no packages", style(&tap.full_name).magenta());
        return Ok(());
    }

    println!();
    println!("{}", style(&tap.full_name).magenta().bold());

    if !formulae.is_empty() {
        println!();
        println!("{}", style("Formulae").cyan().bold());
        for formula in &formulae {
            println!("  {}", formula.name);
        }
    }

    if !casks.is_empty() {
        println!();
        println!("{}", style("Casks").cyan().bold());
        for cask in &casks {
            println!("  {}", cask.token);
        }
    }

    let total = formulae.len() + casks.len();
    println!(
        "\n{} {} in tap",
        style(total).cyan(),
        if total == 1 { "package" } else { "packages" }
    );

    Ok(())
}
