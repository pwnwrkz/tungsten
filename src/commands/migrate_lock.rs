//! `tungsten migrate-lock` — explicit CLI wrapper around the v2 → v3
//! lockfile migration engine.

use anyhow::Result;
use std::path::PathBuf;

use crate::core::postsync::migrate::{self, MigrationOptions};
use crate::log;

pub async fn run(
    lockfile: Option<PathBuf>,
    config: Option<PathBuf>,
    dry_run: bool,
    force: bool,
    no_backup: bool,
) -> Result<()> {
    let opts = MigrationOptions {
        lockfile_path: lockfile.unwrap_or_else(migrate::default_lockfile_path),
        config_path: config.unwrap_or_else(|| PathBuf::from(crate::utils::config::CONFIG_PATH)),
        dry_run,
        force,
        write_backup: !no_backup,
    };

    let report = migrate::migrate_v2_to_v3(&opts)?;

    if report.dry_run {
        log!(info, "Dry run — nothing written");
    } else if let Some(path) = &report.written_path {
        log!(
            success,
            "Migrated lockfile written to \"{}\"",
            path.display()
        );
    }

    log!(
        info,
        "{}/{} entries migrated, {} unmapped (will re-upload on next sync)",
        report.migrated,
        report.total_v2_entries,
        report.unmapped.len()
    );

    if !report.unmapped.is_empty() {
        for name in &report.unmapped {
            log!(warn, "unmapped: {}", name);
        }
    }

    Ok(())
}
