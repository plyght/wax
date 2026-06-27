mod api;
mod bottle;
mod builder;
mod cache;
mod cask;
mod catalog_match;
mod commands;
mod deps;
mod digest;
mod discovery;
mod error;
mod formula_parser;
mod http_client;
mod install;
mod lockfile;
mod signal;
mod sudo;
mod system_pm;
mod tap;
mod ui;
mod version;

// Windows package manager support (scoop, winget, chocolatey)
#[cfg(target_os = "windows")]
mod chocolatey;
#[cfg(target_os = "windows")]
mod ecosystem_install;
#[cfg(target_os = "windows")]
mod package_spec;
#[cfg(target_os = "windows")]
mod remote_search;
#[cfg(target_os = "windows")]
mod scoop;
#[cfg(target_os = "windows")]
mod windows_state;
#[cfg(target_os = "windows")]
mod winget_install;

use cache::Cache;
use clap::{Parser, Subcommand};
use clap_complete::Shell;
use error::Result;
use std::time::Instant;
use tracing::Level;
use tracing_subscriber::fmt::writer::MakeWriterExt;
use version::WAX_VERSION;

fn command_prints_timing(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Update { .. }
            | Commands::Install { .. }
            | Commands::InstallCask { .. }
            | Commands::Uninstall { .. }
            | Commands::Reinstall { .. }
            | Commands::Upgrade { .. }
            | Commands::Outdated { .. }
            | Commands::Sync
    )
}

async fn run_self_update(nightly: bool, force: bool, clean: bool, no_clean: bool) -> Result<()> {
    if clean && no_clean {
        return Err(error::WaxError::InvalidInput(
            "Cannot specify both --clean and --no-clean".to_string(),
        ));
    }

    let channel = if nightly {
        commands::self_update::Channel::Nightly
    } else {
        commands::self_update::Channel::Stable
    };
    let nightly_cleanup = if nightly {
        if clean {
            Some(true)
        } else if no_clean {
            Some(false)
        } else {
            None
        }
    } else {
        None
    };

    commands::self_update::self_update(channel, force, nightly_cleanup).await
}

#[derive(Parser)]
#[command(name = "wax")]
#[command(version = WAX_VERSION)]
#[command(about = format!("wax v{} - the fast homebrew-compat package manager", WAX_VERSION), long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(short, long, global = true)]
    verbose: bool,

    #[arg(short, long, global = true, help = "Assume yes for all prompts")]
    yes: bool,

    #[arg(
        long,
        alias = "tta",
        alias = "time",
        global = true,
        help = "Show command duration in result output"
    )]
    time_to_action: bool,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Update formula index or wax itself")]
    Update {
        #[arg(
            help = "Optional shorthand: s/self for stable self-update, sn/self-nightly for GitHub HEAD"
        )]
        action: Option<String>,
        #[arg(
            short = 's',
            long = "self",
            help = "Update wax itself instead of formula index"
        )]
        update_self: bool,
        #[arg(short, long, help = "Use nightly build from GitHub (with --self)")]
        nightly: bool,
        #[arg(
            short,
            long,
            help = "Force reinstall even if on latest version (with --self)"
        )]
        force: bool,
        #[arg(
            long,
            help = "After nightly self-update, clean Cargo git cache for wax"
        )]
        clean: bool,
        #[arg(long, help = "After nightly self-update, keep Cargo git cache")]
        no_clean: bool,
    },

    #[command(about = "Update wax itself  [alias: self-up]")]
    #[command(name = "self-update")]
    #[command(visible_alias = "self-up")]
    SelfUpdate {
        #[arg(short, long, help = "Use nightly build from GitHub")]
        nightly: bool,
        #[arg(short, long, help = "Force reinstall even if on latest version")]
        force: bool,
        #[arg(
            long,
            help = "After nightly self-update, clean Cargo git cache for wax"
        )]
        clean: bool,
        #[arg(long, help = "After nightly self-update, keep Cargo git cache")]
        no_clean: bool,
    },

    #[command(about = "Search formulae and casks  [alias: s, find]")]
    #[command(visible_alias = "s")]
    #[command(alias = "find")]
    Search { query: String },

    #[command(about = "Show formula details  [alias: show]")]
    #[command(visible_alias = "show")]
    Info {
        formula: String,
        #[arg(long)]
        cask: bool,
    },

    #[command(about = "List installed packages  [alias: ls]")]
    #[command(visible_alias = "ls")]
    List {
        #[arg(help = "Filter: pre-fills the interactive search (TTY), or limits printed output")]
        query: Option<String>,
        #[arg(long, conflicts_with = "global")]
        user: bool,
        #[arg(long, conflicts_with = "user")]
        global: bool,
    },

    #[command(about = "Install one or more formulae or casks  [alias: i, add]")]
    #[command(visible_alias = "i")]
    #[command(alias = "add")]
    Install {
        #[arg(help = "Package name(s) to install (syncs from lockfile if omitted)")]
        packages: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = "Show the install plan and ask before making changes")]
        ask: bool,
        #[arg(long)]
        cask: bool,
        #[arg(long, help = "Install to ~/.local/wax (no sudo required)")]
        user: bool,
        #[arg(long, help = "Install to system directory (may need sudo)")]
        global: bool,
        #[arg(long, help = "Build from source even if bottle available")]
        build_from_source: bool,
        #[arg(
            long,
            help = "Install the HEAD version (clones git repo, builds from source)"
        )]
        head: bool,
        #[arg(long = "no-script", help = "Skip automatic post-install scripts")]
        no_script: bool,
    },

    #[command(about = "Install casks  [alias: c]")]
    #[command(name = "cask")]
    #[command(visible_alias = "c")]
    InstallCask {
        #[arg(required = true, help = "Cask name(s) to install")]
        packages: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = "Show the install plan and ask before making changes")]
        ask: bool,
        #[arg(long, help = "Install to ~/.local/wax (no sudo required)")]
        user: bool,
        #[arg(long, help = "Install to system directory (may need sudo)")]
        global: bool,
        #[arg(long = "no-script", help = "Skip automatic post-install scripts")]
        no_script: bool,
    },

    #[command(about = "Uninstall a formula or cask  [alias: ui, rm, remove]")]
    #[command(visible_alias = "ui")]
    #[command(alias = "rm")]
    #[command(alias = "remove")]
    #[command(alias = "delete")]
    Uninstall {
        #[arg(conflicts_with = "all", required_unless_present = "all", num_args = 1..)]
        formulae: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        cask: bool,
        #[arg(long, help = "Uninstall all installed formulae")]
        all: bool,
    },

    #[command(about = "Reinstall a formula or cask  [alias: ri]")]
    #[command(visible_alias = "ri")]
    Reinstall {
        #[arg(conflicts_with = "all", required_unless_present = "all")]
        packages: Vec<String>,
        #[arg(long)]
        cask: bool,
        #[arg(long, help = "Reinstall all installed formulae and casks")]
        all: bool,
    },

    #[command(about = "Run post-installation steps for a package")]
    Postinstall {
        #[arg(help = "Formula name(s) to run post-install for")]
        formulae: Vec<String>,
        #[arg(long, help = "Install to ~/.local/wax")]
        user: bool,
        #[arg(long, help = "Install to system directory")]
        global: bool,
    },

    #[command(about = "Upgrade formulae to the latest version  [alias: up]")]
    #[command(visible_alias = "up")]
    Upgrade {
        #[arg(help = "Package name(s) to upgrade (upgrades all if omitted)")]
        packages: Vec<String>,
        #[arg(short = 's', long = "self", help = "Upgrade wax itself")]
        upgrade_self: bool,
        #[arg(short, long, help = "Use nightly build from GitHub (with --self)")]
        nightly: bool,
        #[arg(
            long,
            help = "After nightly self-update, clean Cargo git cache for wax"
        )]
        clean: bool,
        #[arg(long, help = "After nightly self-update, keep Cargo git cache")]
        no_clean: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = "Show the upgrade plan and ask before making changes")]
        ask: bool,
        #[arg(
            long,
            help = "Also upgrade OS packages via the native package manager (apt/dnf/pacman/apk/…)"
        )]
        system: bool,
        #[arg(long, conflicts_with = "global")]
        user: bool,
        #[arg(long, conflicts_with = "user")]
        global: bool,
    },

    #[command(about = "Manage OS-level packages via the native package manager")]
    System {
        #[command(subcommand)]
        action: SystemAction,
    },

    #[command(about = "List packages with available updates")]
    Outdated {
        #[arg(long, conflicts_with = "global")]
        user: bool,
        #[arg(long, conflicts_with = "user")]
        global: bool,
    },

    #[command(about = "Re-create symlinks for installed packages  [alias: ln]")]
    #[command(visible_alias = "ln")]
    Link {
        #[arg(required = true)]
        packages: Vec<String>,
    },

    #[command(about = "Remove symlinks for a package (keeps Cellar)")]
    Unlink {
        #[arg(required = true)]
        packages: Vec<String>,
    },

    #[command(about = "Remove old versions from the Cellar")]
    Cleanup {
        #[arg(long)]
        dry_run: bool,
    },

    #[command(about = "Show installed packages not required by any other package")]
    Leaves,

    #[command(about = "Show formulae that depend on a given formula")]
    Uses {
        formula: String,
        #[arg(long, help = "Only show installed dependents")]
        installed: bool,
    },

    #[command(about = "Show dependencies for a formula")]
    Deps {
        formula: String,
        #[arg(long, help = "Show as dependency tree")]
        tree: bool,
        #[arg(long, help = "Only show installed dependencies")]
        installed: bool,
    },

    #[command(about = "Pin a formula to its current version")]
    Pin {
        #[arg(required = true)]
        packages: Vec<String>,
    },

    #[command(about = "Unpin a formula to allow upgrades")]
    Unpin {
        #[arg(required = true)]
        packages: Vec<String>,
    },

    #[command(about = "Generate lockfile from installed packages")]
    Lock,

    #[command(about = "Install packages from lockfile")]
    Sync,

    #[command(about = "Manage custom taps  [alias: untap]")]
    Tap {
        #[arg(long, help = "Re-clone missing or broken taps")]
        repair: bool,
        #[command(subcommand)]
        action: Option<TapAction>,
    },

    #[command(about = "Check system for potential problems  [alias: dr]")]
    #[command(visible_alias = "dr")]
    Doctor {
        #[arg(long, help = "Automatically fix detected issues")]
        fix: bool,
        #[arg(
            long,
            alias = "deep",
            help = "Run full diagnostics, including slower network, bottle, and code-signature scans"
        )]
        full: bool,
    },

    #[command(about = "Open a formula's source repository")]
    #[command(alias = "src")]
    Source {
        #[arg(help = "Formula or cask name")]
        formula: String,
    },

    #[command(about = "Install shell completions (auto-detects shell)")]
    Completions {
        #[arg(
            value_enum,
            help = "Shell to generate completions for (auto-detected if omitted)"
        )]
        shell: Option<Shell>,
        #[arg(long, help = "Print completions to stdout instead of installing")]
        print: bool,
    },

    #[command(about = "Show why a package is installed  [alias: explain]")]
    #[command(alias = "explain")]
    Why {
        #[arg(help = "Package name")]
        formula: String,
    },

    #[command(about = "Check installed packages for issues (deprecated, disabled, outdated)")]
    Audit,
}

#[derive(Subcommand)]
enum SystemAction {
    #[command(about = "Upgrade all OS packages via the native package manager")]
    Upgrade,
    #[command(about = "Install packages via the native package manager")]
    Install {
        #[arg(required = true, help = "Package name(s) to install")]
        packages: Vec<String>,
    },
}

#[derive(Subcommand)]
enum TapAction {
    #[command(about = "Add a custom tap")]
    Add {
        #[arg(help = "Tap specification: user/repo, Git URL, local directory, or .rb file path")]
        tap: String,
        #[arg(long, help = "Trust this tap for formula discovery and installs")]
        trust: bool,
    },
    #[command(
        about = "Remove a custom tap",
        visible_alias = "rm",
        alias = "uninstall",
        alias = "delete"
    )]
    Remove {
        #[arg(help = "Tap specification: user/repo, Git URL, local directory, or .rb file path")]
        tap: String,
    },
    #[command(about = "List installed taps", visible_alias = "ls")]
    List,
    #[command(about = "Update a tap", visible_alias = "up")]
    Update {
        #[arg(help = "Tap specification: user/repo, Git URL, local directory, or .rb file path")]
        tap: String,
    },
    #[command(about = "Trust a tap for formula discovery and installs")]
    Trust {
        #[arg(help = "Tap specification: user/repo, Git URL, local directory, or .rb file path")]
        tap: String,
    },
    #[command(about = "Remove trust from a tap")]
    Untrust {
        #[arg(help = "Tap specification: user/repo, Git URL, local directory, or .rb file path")]
        tap: String,
    },
    /// Bare `wax tap user/repo` — treated as an add.
    #[command(external_subcommand)]
    External(Vec<String>),
}

fn init_logging(verbose: bool) -> Result<()> {
    let log_dir = ui::dirs::wax_logs_dir()?;

    std::fs::create_dir_all(&log_dir)?;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("wax.log"))?;

    let level = if verbose { Level::DEBUG } else { Level::INFO };

    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(log_file.with_max_level(Level::TRACE))
        .with_ansi(false)
        .init();

    Ok(())
}

fn install_scope(user: bool, global: bool) -> Result<Option<install::InstallMode>> {
    install::InstallMode::from_flags(user, global)
}

async fn handle_system_upgrade() -> Result<()> {
    use crate::system_pm::SystemPm;
    match SystemPm::detect().await {
        Some(pm) => {
            println!(
                "\n{} upgrading OS packages via {}",
                console::style("→").cyan(),
                pm.name()
            );
            pm.upgrade_all().await
        }
        None => {
            println!(
                "  {} no supported system package manager found",
                console::style("!").yellow()
            );
            Ok(())
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        print_error_and_exit(err);
    }
}

fn print_error_and_exit(err: error::WaxError) -> ! {
    use console::style;
    use error::WaxError;

    let prefix = style("error:").red().bold();
    match err {
        WaxError::Interrupted => {
            eprintln!("\n{} interrupted", style("✗").red());
            std::process::exit(130);
        }
        WaxError::NotInstalled(pkg) => {
            eprintln!("{} {} is not installed", prefix, style(&pkg).magenta());
        }
        WaxError::FormulaNotFound(pkg) => {
            eprintln!("{} formula not found: {}", prefix, style(&pkg).magenta());
        }
        WaxError::CaskNotFound(pkg) => {
            eprintln!("{} cask not found: {}", prefix, style(&pkg).magenta());
        }
        WaxError::InstallError(message) => {
            eprintln!("{} {}", prefix, message);
        }
        other => {
            eprintln!("{} {}", prefix, other);
        }
    }
    std::process::exit(1);
}

async fn run() -> Result<()> {
    let action_timer = Instant::now();
    let cli = Cli::parse();

    signal::install_handler();
    init_logging(cli.verbose)?;

    let command = cli.command;
    let command_prints_own_timing = command_prints_timing(&command);
    let cache = Cache::new()?;
    ui::set_timing_enabled(cli.time_to_action);

    let result = match command {
        Commands::Update {
            action,
            mut update_self,
            mut nightly,
            force,
            clean,
            no_clean,
        } => {
            if let Some(action) = action {
                match action.as_str() {
                    "s" | "self" => update_self = true,
                    "sn" | "self-nightly" => {
                        update_self = true;
                        nightly = true;
                    }
                    other => {
                        return Err(error::WaxError::InvalidInput(format!(
                            "Unknown update shorthand '{other}' (use s/self or sn/self-nightly)"
                        )));
                    }
                }
            }

            if update_self {
                run_self_update(nightly, force, clean, no_clean).await
            } else {
                #[cfg(target_os = "windows")]
                crate::error::reject_homebrew_cli("update")?;
                commands::update::update(&cache).await
            }
        }
        Commands::SelfUpdate {
            nightly,
            force,
            clean,
            no_clean,
        } => run_self_update(nightly, force, clean, no_clean).await,
        Commands::Search { query } => commands::search::search(&cache, &query).await,
        Commands::Info { formula, cask } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("info")?;
            commands::info::info(&cache, &formula, cask).await
        }
        Commands::List {
            query,
            user,
            global,
        } => commands::list::list(&cache, query, install_scope(user, global)?).await,
        Commands::Install {
            packages,
            dry_run,
            ask,
            cask,
            user,
            global,
            build_from_source,
            head,
            no_script,
        } => {
            if packages.is_empty() && !cask {
                #[cfg(target_os = "windows")]
                crate::error::reject_homebrew_cli("install")?;
                // No packages specified — sync from lockfile like `npm install`
                commands::sync::sync(&cache).await
            } else {
                commands::install::install(
                    &cache,
                    &packages,
                    dry_run,
                    ask && !cli.yes,
                    cask,
                    user,
                    global,
                    build_from_source,
                    head,
                    !no_script,
                )
                .await
            }
        }
        Commands::InstallCask {
            packages,
            dry_run,
            ask,
            user,
            global,
            no_script,
        } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("install --cask")?;
            commands::install::install(
                &cache,
                &packages,
                dry_run,
                ask && !cli.yes,
                true,
                user,
                global,
                false,
                false,
                !no_script,
            )
            .await
        }
        Commands::Uninstall {
            formulae,
            dry_run,
            cask,
            all,
        } => commands::uninstall::uninstall(&cache, &formulae, dry_run, cask, cli.yes, all).await,
        Commands::Reinstall {
            packages,
            cask,
            all,
        } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("reinstall")?;
            commands::reinstall::reinstall(&cache, &packages, cask, all).await
        }
        Commands::Postinstall {
            formulae,
            user,
            global,
        } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("postinstall")?;
            commands::install::postinstall(&cache, &formulae, user, global).await
        }
        Commands::Upgrade {
            packages,
            upgrade_self,
            nightly,
            clean,
            no_clean,
            dry_run,
            ask,
            system,
            user,
            global,
        } => {
            if upgrade_self {
                run_self_update(nightly, false, clean, no_clean).await?;
                return Ok(());
            }

            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("upgrade")?;

            let explicit_packages_requested = !packages.is_empty();

            commands::upgrade::upgrade(
                &cache,
                &packages,
                dry_run,
                ask && !cli.yes,
                install_scope(user, global)?,
            )
            .await?;
            if system {
                handle_system_upgrade().await?;
            }

            // Only check for wax self-update after a full upgrade run.
            // For explicit package upgrades (e.g. `wax up codex`), skip this
            // to avoid unrelated self-update output in command results.
            if !explicit_packages_requested {
                commands::self_update::self_update(
                    commands::self_update::Channel::Stable,
                    false,
                    None,
                )
                .await?;
            }

            Ok(())
        }
        Commands::System { action } => match action {
            SystemAction::Upgrade => handle_system_upgrade().await,
            SystemAction::Install { packages } => {
                use crate::system_pm::SystemPm;
                match SystemPm::detect().await {
                    Some(pm) => {
                        println!("installing via {}", pm.name());
                        pm.install(&packages).await
                    }
                    None => Err(crate::error::WaxError::PlatformNotSupported(
                        "No supported system package manager found".to_string(),
                    )),
                }
            }
        },
        Commands::Outdated { user, global } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("outdated")?;
            commands::outdated::outdated(&cache, install_scope(user, global)?).await
        }
        Commands::Link { packages } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("link")?;
            commands::link::link(&packages).await
        }
        Commands::Unlink { packages } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("unlink")?;
            commands::link::unlink(&packages).await
        }
        Commands::Cleanup { dry_run } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("cleanup")?;
            commands::cleanup::cleanup(dry_run).await
        }
        Commands::Leaves => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("leaves")?;
            commands::leaves::leaves(&cache).await
        }
        Commands::Uses { formula, installed } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("uses")?;
            commands::uses::uses(&cache, &formula, installed).await
        }
        Commands::Deps {
            formula,
            tree,
            installed,
        } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("deps")?;
            commands::show_deps::deps(&cache, &formula, tree, installed).await
        }
        Commands::Pin { packages } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("pin")?;
            commands::pin::pin(&packages).await
        }
        Commands::Unpin { packages } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("unpin")?;
            commands::pin::unpin(&packages).await
        }
        Commands::Lock => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("lock")?;
            commands::lock::lock(&cache).await
        }
        Commands::Sync => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("sync")?;
            commands::sync::sync(&cache).await
        }
        Commands::Tap { action, repair } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("tap")?;
            commands::tap::tap(action, repair, Some(&cache)).await
        }
        Commands::Doctor { fix, full } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("doctor")?;
            commands::doctor::doctor(&cache, fix, full).await
        }
        Commands::Source { formula } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("source")?;
            commands::source::source(&cache, &formula).await
        }
        Commands::Completions { shell, print } => commands::completions::completions(shell, print),
        Commands::Why { formula } => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("why")?;
            commands::info::info(&cache, &formula, false).await
        }
        Commands::Audit => {
            #[cfg(target_os = "windows")]
            crate::error::reject_homebrew_cli("audit")?;
            commands::audit::audit(&cache).await
        }
    };

    result?;

    if cli.time_to_action && !command_prints_own_timing {
        println!("[{}ms]", action_timer.elapsed().as_millis());
    }

    Ok(())
}
