mod api;
mod commands;
mod core;
mod utils;

use clap::{Parser, Subcommand};
use commands::sync::Target;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use utils::config;

#[derive(Parser)]
#[command(
    name = "Tungsten",
    version,
    about = "A command line tool to manage Roblox assets similar to Tarmac and Asphalt."
)]
struct Cli {
    /// Enable verbose logging for troubleshooting
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Pack and upload assets to Roblox
    Sync {
        /// Upload target: `cloud`, `studio`, or `debug`
        #[arg(value_enum)]
        target: Option<Target>,

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
        #[arg(value_enum)]
        target: Target,

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

    utils::logger::set_verbose(cli.verbose);

    // Shared flag so watch can signal whether a sync is in progress. Only the
    // Watch command allocates it — every other command passes `None`.
    let is_watch = matches!(cli.command, Commands::Watch { .. });
    let is_syncing = is_watch.then(|| Arc::new(AtomicBool::new(false)));

    let result = tokio::select! {
        res = run(cli, is_syncing.clone()) => res,
        _ = tokio::signal::ctrl_c() => {
            println!();
            if is_watch {
                if is_syncing
                    .as_ref()
                    .is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed))
                {
                    log!(error, "Watching was cancelled while a sync was in progress. Some assets may not have been processed");
                    log!(warn, "Re-run sync to resume, completed uploads are cached in tungsten.lock.toml");
                } else {
                    log!(warn, "Watching was cancelled");
                }
            } else {
                log!(error, "Tungsten was interrupted. Operation did not complete");
                log!(warn, "Re-run sync to resume, completed uploads are cached in tungsten.lock.toml");
            }
            std::process::exit(130);
        }
    };

    if let Err(e) = result {
        log!(error, "{:#}", e);
        std::process::exit(1);
    }
}

async fn run(cli: Cli, is_syncing: Option<Arc<AtomicBool>>) -> anyhow::Result<()> {
    match cli.command {
        Commands::Sync {
            target,
            api_key,
            dry_run,
        } => {
            let config = config::load("tungsten.toml")?;
            let target = match target {
                Some(t) => t,
                None => {
                    if dry_run {
                        Target::Debug
                    } else {
                        anyhow::bail!("Target is required when not in dry run mode.")
                    }
                }
            };
            commands::sync::run(&config, api_key.as_deref(), target, dry_run).await
        }
        Commands::Watch { target, api_key } => {
            let config = config::load("tungsten.toml")?;
            let is_syncing = is_syncing.expect("is_syncing must be provided for the Watch command");
            commands::watch::run(config, api_key, target, is_syncing).await
        }
        Commands::Init => commands::init::run(),
        Commands::Test { api_key } => {
            let config = config::load("tungsten.toml")?;
            commands::test::run(config, api_key).await
        }
    }
}
