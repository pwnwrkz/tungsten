mod api;
mod commands;
mod core;
mod utils;

use clap::{Parser, Subcommand};
use utils::config;

#[derive(Parser)]
#[command(
    name = "tungsten",
    version,
    about = "A command line tool to manage Roblox assets similar to Tarmac and Asphalt."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Pack and upload assets to Roblox
    Sync {
        /// Upload target (roblox or none)
        #[arg(long)]
        target: String,

        /// Roblox Open Cloud API key (required for syncing to Roblox)
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Create a new tungsten.toml in the current directory
    Init,
    /// Test your config, API key and assets
    Test {
        /// Roblox Open Cloud API key
        #[arg(long)]
        api_key: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Sync { target, api_key } => match config::load("tungsten.toml") {
            Ok(config) => commands::sync::run(config, api_key, &target).await,
            Err(e) => Err(e),
        },
        Commands::Init => commands::init::run(),
        Commands::Test { api_key } => match config::load("tungsten.toml") {
            Ok(config) => commands::test::run(config, api_key).await,
            Err(e) => Err(e),
        },
    };

    if let Err(e) = result {
        log!(error, "{:#}", e);
        std::process::exit(1);
    }
}
