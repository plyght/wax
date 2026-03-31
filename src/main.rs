mod api;
mod bottle;
mod builder;
mod cache;
mod cask;
mod commands;
mod deps;
mod discovery;
mod error;
mod formula_parser;
mod install;
mod lockfile;
mod signal;
mod sudo;
mod tap;
mod ui;
mod version;

use api::ApiClient;
use cache::Cache;
use clap::{Parser, Subcommand};
use clap_complete::Shell;
use error::Result;
use tracing::Level;
use tracing_subscriber::fmt::writer::MakeWriterExt;
use version::WAX_VERSION;

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
    List,

    #[command(about = "Install one or more formulae or casks  [alias: i, add]")]
    #[command(visible_alias = "i")]
    #[command(alias = "add")]
    Install {
        #[arg(help = "Package name(s) to install (syncs from lockfile if omitted)")]
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

    #[command(about = "Install casks  [alias: c]")]
    #[command(name = "cask")]
    #[command(visible_alias = "c")]
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
        #[arg(long, help = "Reinstall all installed formulae")]
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
        #[arg(long)]
        dry_run: bool,
    },

    #[command(about = "List packages with available updates")]
    Outdated,

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

    #[command(about = "Manage custom taps")]
    Tap {
        #[command(subcommand)]
        action: Option<TapAction>,
    },

    #[command(about = "Check system for potential problems  [alias: dr]")]
    #[command(visible_alias = "dr")]
    Doctor {
        #[arg(long, help = "Automatically fix detected issues")]
        fix: bool,
    },

    #[command(about = "Install packages from a Waxfile (formulae, casks, cargo, uv)")]
    Bundle {
        #[arg(long, help = "Path to Waxfile (default: ./Waxfile.toml)")]
        file: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[command(subcommand)]
        action: Option<BundleAction>,
    },

    #[command(about = "Manage background services")]
    #[command(alias = "svc")]
    Services {
        #[command(subcommand)]
        action: Option<ServicesAction>,
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
enum BundleAction {
    #[command(about = "Dump installed packages as a Waxfile")]
    Dump,
}

#[derive(Subcommand)]
enum ServicesAction {
    #[command(about = "List all services")]
    List,
    #[command(about = "Start a service")]
    Start {
        #[arg(help = "Formula name")]
        formula: String,
        #[arg(long, help = "Nice priority (-20 to 20)")]
        nice: Option<i32>,
    },
    #[command(about = "Stop a service")]
    Stop {
        #[arg(help = "Formula name")]
        formula: String,
    },
    #[command(about = "Restart a service")]
    Restart {
        #[arg(help = "Formula name")]
        formula: String,
        #[arg(long, help = "Nice priority (-20 to 20)")]
        nice: Option<i32>,
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    signal::install_handler();
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
        Commands::Search { query } => commands::search::search(&cache, &query).await,
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
            if packages.is_empty() && !cask {
                // No packages specified — sync from lockfile like `npm install`
                commands::sync::sync(&cache).await
            } else {
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
            formulae,
            dry_run,
            cask,
            all,
        } => commands::uninstall::uninstall(&cache, &formulae, dry_run, cask, cli.yes, all).await,
        Commands::Reinstall {
            packages,
            cask,
            all,
        } => commands::reinstall::reinstall(&cache, &packages, cask, all).await,
        Commands::Postinstall {
            formulae,
            user,
            global,
        } => commands::install::postinstall(&cache, &formulae, user, global).await,
        Commands::Upgrade { packages, dry_run } => {
            commands::upgrade::upgrade(&cache, &packages, dry_run).await
        }
        Commands::Outdated => commands::outdated::outdated(&cache).await,
        Commands::Link { packages } => commands::link::link(&packages).await,
        Commands::Unlink { packages } => commands::link::unlink(&packages).await,
        Commands::Cleanup { dry_run } => commands::cleanup::cleanup(dry_run).await,
        Commands::Leaves => commands::leaves::leaves(&cache).await,
        Commands::Uses { formula, installed } => {
            commands::uses::uses(&cache, &formula, installed).await
        }
        Commands::Deps {
            formula,
            tree,
            installed,
        } => commands::show_deps::deps(&cache, &formula, tree, installed).await,
        Commands::Pin { packages } => commands::pin::pin(&packages).await,
        Commands::Unpin { packages } => commands::pin::unpin(&packages).await,
        Commands::Lock => commands::lock::lock(&cache).await,
        Commands::Sync => commands::sync::sync(&cache).await,
        Commands::Tap { action } => commands::tap::tap(action, Some(&cache)).await,
        Commands::Doctor { fix } => commands::doctor::doctor(&cache, fix).await,
        Commands::Bundle {
            file,
            dry_run,
            action,
        } => match action {
            Some(BundleAction::Dump) => commands::bundle::bundle_dump(&cache).await,
            None => commands::bundle::bundle(&cache, file.as_deref(), dry_run).await,
        },
        Commands::Services { action } => match action {
            Some(ServicesAction::List) | None => commands::services::services_list().await,
            Some(ServicesAction::Start { formula, nice }) => {
                commands::services::services_start(&formula, nice).await
            }
            Some(ServicesAction::Stop { formula }) => {
                commands::services::services_stop(&formula).await
            }
            Some(ServicesAction::Restart { formula, nice }) => {
                commands::services::services_restart(&formula, nice).await
            }
        },
        Commands::Source { formula } => commands::source::source(&cache, &formula).await,
        Commands::Completions { shell, print } => commands::completions::completions(shell, print),
        Commands::Why { formula } => {
            commands::info::info(&api_client, &cache, &formula, false).await
        }
        Commands::Audit => commands::audit::audit(&cache).await,
    };

    if let Err(e) = result {
        use console::style;
        use error::WaxError;

        let prefix = style("error:").red().bold();
        match e {
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
            _ => {
                eprintln!("{} {}", prefix, e);
            }
        }
        std::process::exit(1);
    }

    Ok(())
}
