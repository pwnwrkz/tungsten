mod api;
mod commands;
mod core;
mod utils;

use clap::{Parser, Subcommand};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
        /// Upload target: `cloud`, `studio`, or `debug`
        target: Option<String>,

        /// Roblox Open Cloud API key (required for cloud target)
        #[arg(long)]
        api_key: Option<String>,

        /// Dry run, show what would be uploaded without doing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Watch asset folders and re-sync automatically on changes
    Watch {
        /// Upload target: `cloud`, `studio`, or `debug`
        target: String,

        /// Roblox Open Cloud API key (required for cloud target)
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

    // Shared flag so watch::run can signal whether a sync is in progress.
    // Only meaningful for the Watch command; ignored by all others.
    let is_syncing = Arc::new(AtomicBool::new(false));
    let is_watch = matches!(cli.command, Commands::Watch { .. });

    let result = tokio::select! {
        res = run(cli, Arc::clone(&is_syncing)) => res,
        _ = tokio::signal::ctrl_c() => {
            println!();
            if is_watch {
                if is_syncing.load(Ordering::Relaxed) {
                    log!(error, "Watching was cancelled while a sync was in progress — some assets may not have been processed");
                    log!(warn, "Re-run sync to resume; completed uploads are cached in tungsten.lock.toml");
                } else {
                    log!(warn, "Watching was cancelled");
                }
            } else {
                log!(error, "Tungsten was interrupted — operation did not complete");
                log!(warn, "Re-run sync to resume; completed uploads are cached in tungsten.lock.toml");
            }
            std::process::exit(130);
        }
    };

    if let Err(e) = result {
        log!(error, "{:#}", e);
        std::process::exit(1);
    }
}

async fn run(cli: Cli, is_syncing: Arc<AtomicBool>) -> anyhow::Result<()> {
    match cli.command {
        Commands::Sync {
            target,
            api_key,
            dry_run,
        } => {
            let config = config::load("tungsten.toml")?;
            let target = match target {
                Some(t) => commands::sync::Target::parse(&t)?,
                None => {
                    if dry_run {
                        commands::sync::Target::parse("debug")?
                    } else {
                        anyhow::bail!("Target is required when not in dry run mode.");
                    }
                }
            };
            commands::sync::run(config, api_key, target, dry_run).await
        }
        Commands::Watch { target, api_key } => {
            let config = config::load("tungsten.toml")?;
            let target = commands::sync::Target::parse(&target)?;
            commands::watch::run(config, api_key, target, is_syncing).await
        }
        Commands::Init => commands::init::run(),
        Commands::Test { api_key } => {
            let config = config::load("tungsten.toml")?;
            commands::test::run(config, api_key).await
        }
    }
}
