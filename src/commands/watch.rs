use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::commands::sync::Target;
use crate::log;
use crate::utils::config::{self, Config};

/// Debounce window: changes within this period after the first event are
/// collapsed into a single sync pass.
const DEBOUNCE_MS: u64 = 300;

pub async fn run(
    config: Config,
    api_key: Option<String>,
    target: Target,
    is_syncing: Arc<AtomicBool>,
) -> Result<()> {
    log!(section, "Tungsten Watch");
    log!(info, "Watching for changes (target: {:?})...", target);

    let watch_dirs: Vec<String> = config
        .inputs
        .values()
        .map(|input| crate::commands::sync::glob_base(&input.path))
        .filter(|base| !base.is_empty())
        .collect();

    if watch_dirs.is_empty() {
        anyhow::bail!(
            "No watchable directories found in tungsten.toml inputs\n  \
             Hint: Make sure your input paths point to real directories"
        );
    }

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

    let mut watcher = RecommendedWatcher::new(tx, notify::Config::default())
        .context("Failed to create file watcher")?;

    for dir in &watch_dirs {
        let path = std::path::Path::new(dir);
        if path.exists() {
            watcher
                .watch(path, RecursiveMode::Recursive)
                .with_context(|| format!("Failed to watch directory \"{}\"", dir))?;
            log!(info, "Watching \"{}\"", dir);
        } else {
            log!(
                warn,
                "Watch directory \"{}\" does not exist — skipping",
                dir
            );
        }
    }

    log!(
        success,
        "Watching {} director(y/ies). Press Ctrl+C to stop.",
        watch_dirs.len()
    );

    // Initial sync on startup.
    do_sync(&api_key, target, &config, &is_syncing).await;

    // Event loop with debouncing.
    let mut last_event: Option<Instant> = None;
    let mut pending_change = false;

    loop {
        loop {
            match rx.try_recv() {
                Ok(Ok(event)) => {
                    if is_relevant_event(&event) {
                        last_event = Some(Instant::now());
                        pending_change = true;
                    }
                }
                Ok(Err(e)) => log!(warn, "Watch error: {}", e),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    log!(error, "File watcher disconnected");
                    return Ok(());
                }
            }
        }

        if pending_change {
            if let Some(t) = last_event {
                if t.elapsed() >= Duration::from_millis(DEBOUNCE_MS) {
                    pending_change = false;
                    last_event = None;
                    log!(section, "Change detected — re-syncing");
                    let fresh_config = match config::load("tungsten.toml") {
                        Ok(c) => c,
                        Err(e) => {
                            log!(warn, "Failed to reload tungsten.toml: {}", e);
                            continue;
                        }
                    };
                    do_sync(&api_key, target, &fresh_config, &is_syncing).await;
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[allow(unused_variables)]
async fn do_sync(
    api_key: &Option<String>,
    target: Target,
    config: &Config,
    is_syncing: &Arc<AtomicBool>,
) {
    is_syncing.store(true, Ordering::Relaxed);

    if let Err(e) = crate::commands::sync::run(
        config::load("tungsten.toml")
            .unwrap_or_else(|_| panic!("tungsten.toml disappeared during watch")),
        api_key.clone(),
        target,
        false,
    )
    .await
    {
        log!(warn, "Sync error: {:#}", e);
    }

    is_syncing.store(false, Ordering::Relaxed);
}

fn is_relevant_event(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}
