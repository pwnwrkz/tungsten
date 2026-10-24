//! Tungsten CLI entry point: parses args, routes to subcommands.

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
        /// Upload target (defaults to `cloud`)
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
        /// Upload target (defaults to `cloud`)
        #[arg(value_enum)]
        target: Option<Target>,

        /// Roblox Open Cloud API key (required for cloud target)
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Create a new tungsten.toml in the current directory
    Init,
    /// Migrate a v2 lockfile to the v3 name-keyed format (tungsten.lock)
    MigrateLock {
        /// Path to the lockfile (default: tungsten.lock, or tungsten.lock.toml if only that exists)
        #[arg(long)]
        lockfile: Option<std::path::PathBuf>,

        /// Path to the config (default: tungsten.toml)
        #[arg(long)]
        config: Option<std::path::PathBuf>,

        /// Compute the migration and print the summary without writing anything
        #[arg(long)]
        dry_run: bool,

        /// Overwrite an existing v3 lockfile
        #[arg(long)]
        force: bool,

        /// Skip writing the .v2.bak backup
        #[arg(long)]
        no_backup: bool,
    },
    /// Test your config, API key and assets
    Test {
        /// Roblox Open Cloud API key
        #[arg(long)]
        api_key: Option<String>,
    },
}

fn main() {
    utils::trace::start();
    utils::trace::mark("main start");

    // Equivalent runtime to `#[tokio::main]` (multi-threaded), built
    // explicitly so startup cost can be measured.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");
    utils::trace::mark("tokio runtime built");

    let cli = Cli::parse();
    utils::trace::mark("clap parse");

    utils::logger::set_verbose(cli.verbose);

    // Shared flag so watch can signal whether a sync is in progress. Only the
    // Watch command allocates it — every other command passes `None`.
    let is_watch = matches!(cli.command, Commands::Watch { .. });
    let is_syncing = is_watch.then(|| Arc::new(AtomicBool::new(false)));

    let result = rt.block_on(async {
        tokio::select! {
            res = run(cli, is_syncing.clone()) => res,
            _ = tokio::signal::ctrl_c() => {
                println!();
                if is_watch {
                    if is_syncing
                        .as_ref()
                        .is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed))
                    {
                        log!(error, "Watching was cancelled while a sync was in progress. Some assets may not have been processed");
                        log!(warn, "Re-run sync to resume, completed uploads are cached in tungsten.lock");
                    } else {
                        log!(warn, "Watching was cancelled");
                    }
                } else {
                    log!(error, "Tungsten was interrupted. Operation did not complete");
                    log!(warn, "Re-run sync to resume, completed uploads are cached in tungsten.lock");
                }
                std::process::exit(130);
            }
        }
    });

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
            utils::trace::mark("config load");
            let target = match target {
                Some(t) => t,
                None => Target::Cloud,
            };
            commands::sync::run(&config, api_key.as_deref(), target, dry_run).await
        }
        Commands::Watch { target, api_key } => {
            let config = config::load("tungsten.toml")?;
            let target = match target {
                Some(t) => t,
                None => Target::Cloud,
            };
            let is_syncing = is_syncing.expect("is_syncing must be provided for the Watch command");
            commands::watch::run(config, api_key, target, is_syncing).await
        }
        Commands::Init => commands::init::run(),
        Commands::MigrateLock {
            lockfile,
            config,
            dry_run,
            force,
            no_backup,
        } => commands::migrate_lock::run(lockfile, config, dry_run, force, no_backup).await,
        Commands::Test { api_key } => {
            let config = config::load("tungsten.toml")?;
            commands::test::run(config, api_key).await
        }
    }
}
