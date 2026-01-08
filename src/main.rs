mod api;
mod cache;
mod commands;
mod error;
mod ui;

use api::ApiClient;
use cache::Cache;
use clap::{Parser, Subcommand};
use error::Result;
use std::path::PathBuf;
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
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Update formula index")]
    Update,

    #[command(about = "Search formulae and casks")]
    Search { query: String },

    #[command(about = "Show formula details")]
    Info { formula: String },

    #[command(about = "List installed packages")]
    List,
}

fn init_logging(verbose: bool) -> Result<()> {
    let log_dir = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".wax")
        .join("logs");

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

    match cli.command {
        Commands::Update => {
            commands::update::update(&api_client, &cache).await?;
        }
        Commands::Search { query } => {
            commands::search::search(&cache, &query).await?;
        }
        Commands::Info { formula } => {
            commands::info::info(&cache, &formula).await?;
        }
        Commands::List => {
            commands::list::list().await?;
        }
    }

    Ok(())
}
