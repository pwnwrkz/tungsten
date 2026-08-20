use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::api::sync::debug::DebugSync;
use crate::api::sync::roblox::Creator;
use crate::api::sync::studio::StudioSync;
use crate::api::upload::{RobloxClient, UploadParams};
use crate::core::assets::asset::AssetKind;
use crate::core::postsync::codegen::{self, CodegenEntry};
use crate::core::postsync::lockfile::Lockfile;
use crate::log;
use crate::utils::logger::{clear_progress_line, progress};

use super::Target;

/// Copy an asset into the Studio sync folder, recording it as an expected
/// file for stale-file cleanup. Returns the `rbxasset://` URI.
pub fn copy_to_studio(
    studio_sync: &Option<Arc<StudioSync>>,
    studio_expected_files: &mut Option<&mut HashSet<String>>,
    rel: &str,
    bytes: &[u8],
) -> Result<String> {
    let Some(ss) = studio_sync else {
        return Ok(String::new());
    };
    let uri = ss.copy_asset(rel, bytes)?;
    if let Some(set) = studio_expected_files.as_deref_mut() {
        set.insert(rel.to_string());
    }
    Ok(uri)
}

/// Copy an asset into the debug sync folder (`.tungsten_debug/`).
pub fn copy_to_debug(debug_sync: &Option<Arc<DebugSync>>, rel: &str, bytes: &[u8]) -> Result<()> {
    if let Some(ds) = debug_sync {
        ds.copy_asset(rel, bytes)?;
    }
    Ok(())
}

/// Spawn a concurrent cloud upload task.
///
/// The task resolves to `(name, asset_id, hash)` so the caller can update the
/// lockfile and push a codegen entry once the upload completes. `bytes` is
/// moved into the task to avoid cloning potentially large image buffers.
#[allow(clippy::too_many_arguments)]
pub fn spawn_cloud_upload(
    upload_tasks: &mut JoinSet<Result<(String, u64, String)>>,
    semaphore: &Arc<Semaphore>,
    client: &Arc<RobloxClient>,
    creator: &Creator,
    file_name: String,
    display_name: String,
    description: String,
    bytes: Bytes,
    kind: AssetKind,
    asset_type_override: Option<String>,
    result_name: String,
    result_hash: String,
) {
    let client = Arc::clone(client);
    let creator = creator.clone();
    let semaphore = semaphore.clone();
    upload_tasks.spawn(async move {
        let _permit = semaphore.acquire_owned().await;
        let id = client
            .upload(UploadParams {
                file_name,
                display_name,
                description,
                data: bytes,
                kind,
                asset_type_override,
                creator,
            })
            .await
            .with_context(|| format!("Failed to upload \"{}\"", result_name))?;
        Ok((result_name, id, result_hash))
    });
}

/// Drain an upload `JoinSet`, updating the lockfile, progress bar and codegen
/// entries. Returns the number of failures via `errors`.
pub async fn collect_upload_results(
    upload_tasks: &mut JoinSet<Result<(String, u64, String)>>,
    input_name: &str,
    lockfile: &mut Lockfile,
    codegen_entries: &mut Vec<CodegenEntry>,
    total: usize,
    dispatched: usize,
    errors: &mut u32,
) {
    let mut completed = 0usize;
    while let Some(res) = upload_tasks.join_next().await {
        completed += 1;
        match res {
            Ok(Ok((name, id, hash))) => {
                lockfile.set(input_name, hash, id);
                progress("Uploading", dispatched + completed, total, &name);
                codegen_entries.push(CodegenEntry::asset_id(name, id));
            }
            Ok(Err(e)) => {
                clear_progress_line();
                log!(warn, "{}", e);
                *errors += 1;
            }
            Err(e) => {
                clear_progress_line();
                log!(warn, "Upload task panicked: {}", e);
                *errors += 1;
            }
        }
    }
}

/// A fully-prepared asset ready for the target dispatch step. Each sync mode
/// (`individual`, `raw`) is only responsible for building these; the unified
/// dispatcher below handles dry-run, cache checks, Studio/Debug copies and
/// cloud uploads identically for every mode.
pub struct PendingAsset {
    pub name: String,
    pub path: PathBuf,
    pub bytes: Bytes,
    pub hash: String,
    pub kind: AssetKind,
    pub display_name: String,
    pub description: String,
    /// Cloud asset-type override sent to the API (None lets it derive from kind).
    pub asset_type_override: Option<String>,
    /// Studio copy destination relative path (unused when `studio_needs_upload`).
    pub studio_rel: String,
    /// Debug copy destination relative path.
    pub debug_rel: String,
    /// Models/animations cannot be copied to Studio without a prior upload;
    /// dispatch resolves the cached `rbxassetid` URI instead.
    pub studio_needs_upload: bool,
}

/// Shared state for the unified dispatcher.
///
/// `'a` covers the long-lived config references (target, creator, clients);
/// `'b` covers the short-lived mutable borrows (lockfile, counters, tasks) so
/// the context can be constructed and dropped within a local block; `'c` is
/// the inner lifetime of the expected-files set.
pub struct DispatchCtx<'a, 'b, 'c> {
    pub input_name: &'a str,
    pub target: Target,
    pub dry_run: bool,
    pub creator: &'a Creator,
    pub client: &'a Option<Arc<RobloxClient>>,
    pub studio_sync: &'a Option<Arc<StudioSync>>,
    pub debug_sync: &'a Option<Arc<DebugSync>>,
    pub lockfile: &'b mut Lockfile,
    pub studio_expected_files: &'b mut Option<&'c mut HashSet<String>>,
    pub upload_tasks: &'b mut JoinSet<Result<(String, u64, String)>>,
    pub semaphore: &'b Arc<Semaphore>,
    pub total: usize,
    pub dispatched: &'b mut usize,
    pub errors: &'b mut u32,
}

/// Centralized target dispatch for a single prepared asset: handles dry-run,
/// Studio/Debug copies, lockfile cache hits and cloud upload spawning. Pushes
/// the resulting codegen entry (or, for cloud uploads, defers it until the
/// upload completes and `collect_upload_results` runs).
pub fn dispatch_asset(asset: PendingAsset, ctx: &mut DispatchCtx<'_, '_, '_>, codegen_entries: &mut Vec<CodegenEntry>) {
    if ctx.dry_run {
        *ctx.dispatched += 1;
        progress("Uploading", *ctx.dispatched, ctx.total, asset.name.as_str());
        codegen_entries.push(CodegenEntry::asset_id(asset.name, 0));
        return;
    }

    match ctx.target {
        Target::Studio => {
            *ctx.dispatched += 1;

            if asset.studio_needs_upload {
                // Models and Animations cannot be synced to Studio without
                // having been uploaded first.
                let uri = match ctx.lockfile.get(ctx.input_name, &asset.hash) {
                    Some(cached_id) => format!("rbxassetid://{cached_id}"),
                    None => {
                        clear_progress_line();
                        log!(
                            warn,
                            "Models and Animations cannot be synced to Studio without having been uploaded first: \"{}\"",
                            asset.name
                        );
                        *ctx.errors += 1;
                        return;
                    }
                };
                ctx.lockfile.set_uri(ctx.input_name, asset.hash, uri.clone());
                progress("Copying", *ctx.dispatched, ctx.total, asset.name.as_str());
                codegen_entries.push(CodegenEntry::asset(asset.name, codegen::AssetRef::Uri(uri)));
                return;
            }

            let uri = match copy_to_studio(
                ctx.studio_sync,
                ctx.studio_expected_files,
                &asset.studio_rel,
                &asset.bytes,
            ) {
                Ok(u) => u,
                Err(e) => {
                    clear_progress_line();
                    log!(warn, "Studio copy failed for \"{}\": {}", asset.name, e);
                    *ctx.errors += 1;
                    return;
                }
            };
            ctx.lockfile.set_uri(ctx.input_name, asset.hash, uri.clone());
            progress("Copying", *ctx.dispatched, ctx.total, asset.name.as_str());
            codegen_entries.push(CodegenEntry::asset(asset.name, codegen::AssetRef::Uri(uri)));
        }
        Target::Debug => {
            *ctx.dispatched += 1;

            if let Err(e) = copy_to_debug(ctx.debug_sync, &asset.debug_rel, &asset.bytes) {
                clear_progress_line();
                log!(warn, "Debug copy failed for \"{}\": {}", asset.name, e);
                *ctx.errors += 1;
                return;
            }
            let fallback = ctx.lockfile.get(ctx.input_name, &asset.hash).unwrap_or(0);
            progress("Copying", *ctx.dispatched, ctx.total, asset.name.as_str());
            codegen_entries.push(CodegenEntry::asset_id(asset.name, fallback));
        }
        Target::Cloud => {
            if let Some(cached_id) = ctx.lockfile.get(ctx.input_name, &asset.hash) {
                clear_progress_line();
                log!(
                    debug,
                    "{}: unchanged, skipping (cached asset {})",
                    asset.name,
                    cached_id
                );
                *ctx.dispatched += 1;
                progress("Uploading", *ctx.dispatched, ctx.total, asset.name.as_str());
                codegen_entries.push(CodegenEntry::asset_id(asset.name, cached_id));
                return;
            }
            let Some(c) = ctx.client else {
                codegen_entries.push(CodegenEntry::asset_id(asset.name, 0));
                return;
            };
            let file_name = asset
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            spawn_cloud_upload(
                ctx.upload_tasks,
                ctx.semaphore,
                c,
                ctx.creator,
                file_name,
                asset.display_name,
                asset.description,
                asset.bytes,
                asset.kind,
                asset.asset_type_override,
                asset.name,
                asset.hash,
            );
        }
    }
}
