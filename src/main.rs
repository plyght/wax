mod api;
mod bottle;
mod builder;
mod cache;
mod cask;
mod commands;
mod deps;
mod error;
mod formula_parser;
mod install;
mod lockfile;
mod tap;
mod ui;
mod version;

use api::ApiClient;
use cache::Cache;
use clap::{Parser, Subcommand};
use error::Result;
use tracing::Level;
use tracing_subscriber::fmt::writer::MakeWriterExt;

#[derive(Parser)]
#[command(name = "wax")]
#[command(about = "Fast Homebrew-compatible package manager", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(short, long, global = true)]
    verbose: bool,

    #[arg(short, long, global = true, help = "Assume yes for all prompts")]
    yes: bool,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Update formula index or wax itself")]
    Update {
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
    },

    #[command(about = "Search formulae and casks")]
    #[command(alias = "find")]
    #[command(alias = "s")]
    Search { query: String },

    #[command(about = "Show formula details")]
    #[command(alias = "show")]
    Info {
        formula: String,
        #[arg(long)]
        cask: bool,
    },

    #[command(about = "List installed packages")]
    #[command(alias = "ls")]
    List,

    #[command(about = "Install one or more formulae or casks")]
    #[command(alias = "i")]
    #[command(alias = "add")]
    Install {
        #[arg(required = true, help = "Package name(s) to install")]
        packages: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        cask: bool,
        #[arg(long, help = "Install to ~/.local/wax (no sudo required)")]
        user: bool,
        #[arg(long, help = "Install to system directory (may need sudo)")]
        global: bool,
        #[arg(long, help = "Build from source even if bottle available")]
        build_from_source: bool,
    },

    #[command(about = "Install casks (shorthand for install --cask)")]
    #[command(name = "cask")]
    #[command(alias = "c")]
    InstallCask {
        #[arg(required = true, help = "Cask name(s) to install")]
        packages: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = "Install to ~/.local/wax (no sudo required)")]
        user: bool,
        #[arg(long, help = "Install to system directory (may need sudo)")]
        global: bool,
    },

    #[command(about = "Uninstall a formula or cask")]
    #[command(alias = "rm")]
    #[command(alias = "remove")]
    #[command(alias = "delete")]
    Uninstall {
        formula: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        cask: bool,
    },

    #[command(about = "Upgrade formulae to the latest version")]
    #[command(alias = "up")]
    Upgrade {
        #[arg(help = "Package name(s) to upgrade (upgrades all if omitted)")]
        packages: Vec<String>,
        #[arg(long)]
        dry_run: bool,
    },

    #[command(about = "List packages with available updates")]
    Outdated,

    #[command(about = "Generate lockfile from installed packages")]
    Lock,

    #[command(about = "Install packages from lockfile")]
    Sync,

    #[command(about = "Manage custom taps")]
    Tap {
        #[command(subcommand)]
        action: Option<TapAction>,
    },
}

#[derive(Subcommand)]
enum TapAction {
    #[command(about = "Add a custom tap")]
    Add {
        #[arg(help = "Tap specification: user/repo, Git URL, local directory, or .rb file path")]
        tap: String,
    },
    #[command(about = "Remove a custom tap")]
    Remove {
        #[arg(help = "Tap specification: user/repo, Git URL, local directory, or .rb file path")]
        tap: String,
    },
    #[command(about = "List installed taps")]
    List,
    #[command(about = "Update a tap")]
    Update {
        #[arg(help = "Tap specification: user/repo, Git URL, local directory, or .rb file path")]
        tap: String,
    },
}

fn init_logging(verbose: bool) -> Result<()> {
    let log_dir = if let Some(base_dirs) = directories::BaseDirs::new() {
        base_dirs.cache_dir().join("wax").join("logs")
    } else {
        ui::dirs::home_dir()?.join(".wax").join("logs")
    };

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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    init_logging(cli.verbose)?;

    let api_client = ApiClient::new();
    let cache = Cache::new()?;

    let result = match cli.command {
        Commands::Update {
            update_self,
            nightly,
            force,
        } => {
            if update_self {
                let channel = if nightly {
                    commands::self_update::Channel::Nightly
                } else {
                    commands::self_update::Channel::Stable
                };
                commands::self_update::self_update(channel, force).await
            } else {
                commands::update::update(&api_client, &cache).await
            }
        }
        Commands::Search { query } => commands::search::search(&api_client, &cache, &query).await,
        Commands::Info { formula, cask } => {
            commands::info::info(&api_client, &cache, &formula, cask).await
        }
        Commands::List => commands::list::list().await,
        Commands::Install {
            packages,
            dry_run,
            cask,
            user,
            global,
            build_from_source,
        } => {
            commands::install::install(
                &cache,
                &packages,
                dry_run,
                cask,
                user,
                global,
                build_from_source,
            )
            .await
        }
        Commands::InstallCask {
            packages,
            dry_run,
            user,
            global,
        } => {
            commands::install::install(&cache, &packages, dry_run, true, user, global, false).await
        }
        Commands::Uninstall {
            formula,
            dry_run,
            cask,
        } => commands::uninstall::uninstall(&cache, &formula, dry_run, cask, cli.yes).await,
        Commands::Upgrade { packages, dry_run } => {
            commands::upgrade::upgrade(&cache, &packages, dry_run).await
        }
        Commands::Outdated => commands::outdated::outdated(&cache).await,
        Commands::Lock => commands::lock::lock().await,
        Commands::Sync => commands::sync::sync(&cache).await,
        Commands::Tap { action } => commands::tap::tap(action).await,
    };

    if let Err(e) = result {
        use console::style;
        use error::WaxError;

        match e {
            WaxError::NotInstalled(pkg) => {
                eprintln!("\n{} is not installed", style(&pkg).magenta());
            }
            WaxError::FormulaNotFound(pkg) => {
                eprintln!("\nformula not found: {}", style(&pkg).magenta());
            }
            WaxError::CaskNotFound(pkg) => {
                eprintln!("\ncask not found: {}", style(&pkg).magenta());
            }
            _ => {
                eprintln!("\n{}", e);
            }
        }
        std::process::exit(1);
    }

    Ok(())
}
