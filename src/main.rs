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

    let result = tokio::select! {
        // Normal operation.
        res = run(cli) => res,

        // Ctrl+C / SIGINT — log cleanly and exit.
        _ = tokio::signal::ctrl_c() => {
            // Blank line so the ^C character from the terminal doesn't run
            // into our log line.
            println!();
            log!(error, "Tungsten was interrupted — operation did not complete");
            log!(warn, "Any uploads already in flight have been cancelled");
            log!(warn, "Re-run sync to resume; completed uploads are cached in tungsten.lock.toml");
            std::process::exit(130); // 128 + SIGINT(2), standard convention
        }
    };

    if let Err(e) = result {
        log!(error, "{:#}", e);
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Sync { target, api_key } => {
            let config = config::load("tungsten.toml")?;
            commands::sync::run(config, api_key, &target).await
        }
        Commands::Init => commands::init::run(),
        Commands::Test { api_key } => {
            let config = config::load("tungsten.toml")?;
            commands::test::run(config, api_key).await
        }
    }
}
