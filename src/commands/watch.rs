use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::Instant;

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
    log!(section, "WATCH MODE");
    log!(info, "Watching for changes (target: {:?})...", target);

    let watch_dirs: Vec<String> = config
        .inputs
        .values()
        .map(|input| crate::commands::sync::paths::glob_base(&input.path))
        .filter(|base| !base.is_empty())
        .collect();

    if watch_dirs.is_empty() {
        anyhow::bail!(
            "No watchable directories found in tungsten.toml inputs\n  \
             Hint: Make sure your input paths point to real directories"
        );
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<notify::Result<Event>>();

    let mut watcher = RecommendedWatcher::new(
        move |event| {
            // Non-blocking send; errors are ignored (channel closed = watcher shutting down).
            let _ = tx.send(event);
        },
        notify::Config::default(),
    )
    .context("Failed to create file watcher")?;

    for dir in &watch_dirs {
        let path = std::path::Path::new(dir);
        if path.exists() {
            watcher
                .watch(path, RecursiveMode::Recursive)
                .with_context(|| format!("Failed to watch directory \"{}\"", dir))?;
            log!(info, "Watching \"{}\"", dir);
        } else {
            log!(warn, "Watch directory \"{}\" does not exist. Skipping", dir);
        }
    }

    log!(
        success,
        "Watching {} director(y/ies). Press Ctrl+C to stop.",
        watch_dirs.len()
    );

    // Initial sync on startup.
    do_sync(&api_key, target, &config, &is_syncing).await;

    // Event loop with debouncing. When idle we simply await the next file
    // event (no busy-polling); once a change is pending we race the debounce
    // deadline against any further events so bursts collapse into one sync.
    let mut last_event: Option<Instant> = None;
    let mut pending_change = false;

    loop {
        let debounce_deadline = last_event
            .map(|t| t + Duration::from_millis(DEBOUNCE_MS))
            .unwrap_or_else(|| Instant::now() + Duration::from_millis(DEBOUNCE_MS));

        tokio::select! {
            event = rx.recv() => {
                match event {
                    Some(Ok(event)) => {
                        if is_relevant_event(&event) {
                            log!(debug, "Change detected: {:?}", event.kind);
                            last_event = Some(Instant::now());
                            pending_change = true;
                        }
                    }
                    Some(Err(e)) => log!(warn, "Watch error: {}", e),
                    None => {
                        log!(error, "File watcher disconnected");
                        return Ok(());
                    }
                }
            }
            _ = tokio::time::sleep_until(debounce_deadline), if pending_change => {
                log!(debug, "Debounce window elapsed, triggering re-sync");
                pending_change = false;
                last_event = None;
                log!(section, "RE-SYNCING");
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
}

async fn do_sync(
    api_key: &Option<String>,
    target: Target,
    config: &Config,
    is_syncing: &Arc<AtomicBool>,
) {
    is_syncing.store(true, Ordering::Relaxed);

    if let Err(e) = crate::commands::sync::run(config, api_key.as_deref(), target, false).await {
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
