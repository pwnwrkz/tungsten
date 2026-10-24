//! Shared sync context and the cloud/studio/debug dispatch + upload-collection machinery
//! used by every `process_*` routine.

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

/// Resolved `[codegen]` settings, computed once per run so the per-input
/// processors don't repeat the Option-unwrapping boilerplate.
#[derive(Clone)]
pub struct ResolvedCodegen {
    pub style: String,
    pub strip_extension: bool,
    pub ts_declaration: bool,
    pub strip_whitespace: bool,
}

/// Shared mutable state for one input's sync pass.
///
/// Constructed once per input in `sync::run` and threaded through the
/// processors as `&mut`, so they don't repeat a dozen shared values in their
/// signatures. `errors` accumulates per-asset failures; `run` folds it into
/// the run-level total after each input.
pub struct SyncContext<'a> {
    pub input_name: &'a str,
    /// Glob base directory for this input; asset names are relative to it.
    pub base_path: &'a str,
    /// Output path for this input's generated Luau module.
    pub output_path: &'a str,
    pub target: Target,
    pub dry_run: bool,
    pub creator: Creator,
    pub client: Option<Arc<RobloxClient>>,
    pub studio_sync: Option<Arc<StudioSync>>,
    pub debug_sync: Option<Arc<DebugSync>>,
    pub lockfile: &'a mut Lockfile,
    /// Files copied into the Studio folder, shared across all inputs so
    /// stale-file cleanup after the run keeps the union of every input's
    /// output (not just the last input's).
    pub studio_expected_files: &'a mut HashSet<String>,
    pub semaphore: Arc<Semaphore>,
    pub codegen: ResolvedCodegen,
    /// Explicit asset-type override for this input (None = derive from kind).
    pub asset_type_override: Option<String>,
    /// Per-asset failures accumulated during this input's pass.
    pub errors: u32,
}

impl<'a> SyncContext<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input_name: &'a str,
        base_path: &'a str,
        output_path: &'a str,
        target: Target,
        dry_run: bool,
        creator: &Creator,
        client: &Option<Arc<RobloxClient>>,
        studio_sync: &Option<Arc<StudioSync>>,
        debug_sync: &Option<Arc<DebugSync>>,
        lockfile: &'a mut Lockfile,
        studio_expected_files: &'a mut HashSet<String>,
        max_concurrent_uploads: usize,
        codegen: ResolvedCodegen,
        asset_type_override: Option<String>,
    ) -> Self {
        Self {
            input_name,
            base_path,
            output_path,
            target,
            dry_run,
            creator: creator.clone(),
            client: client.clone(),
            studio_sync: studio_sync.clone(),
            debug_sync: debug_sync.clone(),
            lockfile,
            studio_expected_files,
            semaphore: Arc::new(Semaphore::new(max_concurrent_uploads.max(1))),
            codegen,
            asset_type_override,
            errors: 0,
        }
    }
}

/// Transient state for a dispatch phase: one upload `JoinSet` plus progress
/// counters over `total` assets. Created by each processor for the assets it
/// dispatches; shared values live on [`SyncContext`].
pub struct DispatchPhase<'a, 'ctx> {
    pub ctx: &'a mut SyncContext<'ctx>,
    pub upload_tasks: JoinSet<Result<(String, u64, String)>>,
    pub total: usize,
    pub dispatched: usize,
}

impl<'a, 'ctx> DispatchPhase<'a, 'ctx> {
    pub fn new(ctx: &'a mut SyncContext<'ctx>, total: usize) -> Self {
        Self {
            ctx,
            upload_tasks: JoinSet::new(),
            total,
            dispatched: 0,
        }
    }
}

/// Copy an asset into the Studio sync folder, recording it as an expected
/// file for stale-file cleanup. Returns the `rbxasset://` URI.
pub fn copy_to_studio(ctx: &mut SyncContext, rel: &str, bytes: &[u8]) -> Result<String> {
    let Some(ss) = &ctx.studio_sync else {
        return Ok(String::new());
    };
    let uri = ss.copy_asset(rel, bytes)?;
    crate::utils::trace::mark_first_write();
    ctx.studio_expected_files.insert(rel.to_string());
    Ok(uri)
}

/// Copy an asset into the debug sync folder (`.tungsten_debug/`).
pub fn copy_to_debug(ctx: &mut SyncContext, rel: &str, bytes: &[u8]) -> Result<()> {
    if let Some(ds) = &ctx.debug_sync {
        ds.copy_asset(rel, bytes)?;
        crate::utils::trace::mark_first_write();
    }
    Ok(())
}

/// A fully-prepared asset ready for the target dispatch step. Each sync mode
/// (`individual`, `raw`) is only responsible for building these; the unified
/// dispatcher below handles dry-run, cache checks, Studio/Debug copies and
/// cloud uploads identically for every mode.
pub struct PendingAsset {
    pub name: String,
    pub path: PathBuf,
    pub bytes: Bytes,
    /// sha256 of the raw source bytes (pre-processing).
    pub source_hash: String,
    /// sha256 of the processed/uploaded bytes.
    pub hash: String,
    pub kind: AssetKind,
    pub display_name: String,
    pub description: String,
    /// Cloud asset-type override sent to the API (None lets it derive from kind).
    pub asset_type_override: Option<String>,
    /// Cloud upload file name. Defaults to `path.file_name()` — set this when
    /// the uploaded bytes are a derived artifact (e.g. an audio file transcoded
    /// from WAV to OGG) whose name differs from the source on disk.
    pub upload_name: Option<String>,
    /// Studio copy destination relative path (unused when `studio_needs_upload`).
    pub studio_rel: String,
    /// Debug copy destination relative path.
    pub debug_rel: String,
    /// Models/animations cannot be copied to Studio without a prior upload;
    /// dispatch resolves the cached `rbxassetid` URI instead.
    pub studio_needs_upload: bool,
}

/// Spawn a concurrent cloud upload task for a prepared asset.
///
/// The task resolves to `(name, asset_id, hash)` so the caller can update the
/// lockfile and push a codegen entry once the upload completes. `Bytes` clone
/// is a reference-count bump, so the payload is not copied for the task.
pub fn spawn_cloud_upload(
    phase: &mut DispatchPhase<'_, '_>,
    asset: &PendingAsset,
    client: &Arc<RobloxClient>,
) {
    let file_name = match &asset.upload_name {
        Some(name) => name.clone(),
        None => asset
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    };
    let client = client.clone();
    let creator = phase.ctx.creator.clone();
    let semaphore = phase.ctx.semaphore.clone();
    let display_name = asset.display_name.clone();
    let description = asset.description.clone();
    let bytes = asset.bytes.clone();
    let kind = asset.kind;
    let asset_type_override = asset.asset_type_override.clone();
    let name = asset.name.clone();
    let hash = asset.hash.clone();
    phase.upload_tasks.spawn(async move {
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
            .with_context(|| format!("Failed to upload \"{}\"", name))?;
        Ok((name, id, hash))
    });
}

/// Drain an upload `JoinSet`, updating the lockfile, progress bar and codegen
/// entries. Failures are counted on `phase.ctx.errors`.
pub async fn collect_upload_results(
    phase: &mut DispatchPhase<'_, '_>,
    codegen_entries: &mut Vec<CodegenEntry>,
) {
    let mut completed = 0usize;
    while let Some(res) = phase.upload_tasks.join_next().await {
        completed += 1;
        match res {
            Ok(Ok((name, id, _hash))) => {
                let ctx = &mut phase.ctx;
                let input = ctx.input_name;
                ctx.lockfile.set_id(input, &name, id);
                progress(
                    "Uploading",
                    phase.dispatched + completed,
                    phase.total,
                    &name,
                );
                codegen_entries.push(CodegenEntry::asset_id(name, id));
            }
            Ok(Err(e)) => {
                clear_progress_line();
                log!(warn, "{}", e);
                phase.ctx.errors += 1;
            }
            Err(e) => {
                clear_progress_line();
                log!(warn, "Upload task panicked: {}", e);
                phase.ctx.errors += 1;
            }
        }
    }
}

/// Centralized target dispatch for a single prepared asset: handles dry-run,
/// Studio/Debug copies, lockfile cache hits and cloud upload spawning. Pushes
/// the resulting codegen entry (or, for cloud uploads, defers it until the
/// upload completes and `collect_upload_results` runs).
///
/// The lockfile entry (kind/source_hash/hash/packable) is upserted first so
/// every target branch — including cache hits — sees a current entry.
pub fn dispatch_asset(
    asset: PendingAsset,
    phase: &mut DispatchPhase<'_, '_>,
    codegen_entries: &mut Vec<CodegenEntry>,
) {
    if !phase.ctx.dry_run {
        let input = phase.ctx.input_name;
        phase.ctx.lockfile.upsert_plain(
            input,
            &asset.name,
            asset.kind.mime(),
            &asset.source_hash,
            &asset.hash,
            false,
        );
    }

    if phase.ctx.dry_run {
        phase.dispatched += 1;
        progress(
            "Uploading",
            phase.dispatched,
            phase.total,
            asset.name.as_str(),
        );
        codegen_entries.push(CodegenEntry::asset_id(asset.name, 0));
        return;
    }

    match phase.ctx.target {
        Target::Studio => {
            phase.dispatched += 1;

            if asset.studio_needs_upload {
                // Models and Animations cannot be synced to Studio without
                // having been uploaded first.
                let uri = match phase
                    .ctx
                    .lockfile
                    .entry(phase.ctx.input_name, &asset.name)
                    .and_then(|e| e.id)
                    .map(|id| format!("rbxassetid://{id}"))
                {
                    Some(uri) => uri,
                    None => {
                        clear_progress_line();
                        log!(
                            warn,
                            "Models and Animations cannot be synced to Studio without having been uploaded first: \"{}\"",
                            asset.name
                        );
                        phase.ctx.errors += 1;
                        return;
                    }
                };
                phase
                    .ctx
                    .lockfile
                    .set_studio_uri(phase.ctx.input_name, &asset.name, uri.clone());
                progress(
                    "Copying",
                    phase.dispatched,
                    phase.total,
                    asset.name.as_str(),
                );
                codegen_entries.push(CodegenEntry::asset(asset.name, codegen::AssetRef::Uri(uri)));
                return;
            }

            let uri = match copy_to_studio(phase.ctx, &asset.studio_rel, &asset.bytes) {
                Ok(u) => u,
                Err(e) => {
                    clear_progress_line();
                    log!(warn, "Studio copy failed for \"{}\": {}", asset.name, e);
                    phase.ctx.errors += 1;
                    return;
                }
            };
            phase
                .ctx
                .lockfile
                .set_studio_uri(phase.ctx.input_name, &asset.name, uri.clone());
            progress(
                "Copying",
                phase.dispatched,
                phase.total,
                asset.name.as_str(),
            );
            codegen_entries.push(CodegenEntry::asset(asset.name, codegen::AssetRef::Uri(uri)));
        }
        Target::Debug => {
            phase.dispatched += 1;

            if let Err(e) = copy_to_debug(phase.ctx, &asset.debug_rel, &asset.bytes) {
                clear_progress_line();
                log!(warn, "Debug copy failed for \"{}\": {}", asset.name, e);
                phase.ctx.errors += 1;
                return;
            }
            let fallback = phase
                .ctx
                .lockfile
                .entry(phase.ctx.input_name, &asset.name)
                .and_then(|e| e.id)
                .unwrap_or(0);
            progress(
                "Copying",
                phase.dispatched,
                phase.total,
                asset.name.as_str(),
            );
            codegen_entries.push(CodegenEntry::asset_id(asset.name, fallback));
        }
        Target::Cloud => {
            let cache_hit = phase
                .ctx
                .lockfile
                .entry(phase.ctx.input_name, &asset.name)
                .is_some_and(|e| e.source_hash == asset.source_hash && e.id.is_some());
            if cache_hit {
                let cached_id = phase
                    .ctx
                    .lockfile
                    .entry(phase.ctx.input_name, &asset.name)
                    .and_then(|e| e.id)
                    .unwrap_or(0);
                clear_progress_line();
                log!(
                    debug,
                    "{}: unchanged, skipping (cached asset {})",
                    asset.name,
                    cached_id
                );
                phase.dispatched += 1;
                progress(
                    "Uploading",
                    phase.dispatched,
                    phase.total,
                    asset.name.as_str(),
                );
                codegen_entries.push(CodegenEntry::asset_id(asset.name, cached_id));
                return;
            }
            let client = match &phase.ctx.client {
                Some(c) => c.clone(),
                None => {
                    codegen_entries.push(CodegenEntry::asset_id(asset.name, 0));
                    return;
                }
            };
            spawn_cloud_upload(phase, &asset, &client);
        }
    }
}

// Tests

#[cfg(test)]
/// Build a `SyncContext` with the minimal setup for unit tests: no clients,
/// a debug-sized semaphore, and default "flat" codegen settings. Tests can
/// override `debug_sync`/`studio_sync`/`client` after construction.
pub fn test_context<'a>(
    input_name: &'a str,
    target: Target,
    dry_run: bool,
    lockfile: &'a mut Lockfile,
    studio_expected_files: &'a mut HashSet<String>,
) -> SyncContext<'a> {
    use crate::api::sync::roblox::UserCreator;
    SyncContext {
        input_name,
        base_path: "",
        output_path: "",
        target,
        dry_run,
        creator: Creator::User(UserCreator {
            user_id: "0".to_string(),
        }),
        client: None,
        studio_sync: None,
        debug_sync: None,
        lockfile,
        studio_expected_files,
        semaphore: Arc::new(Semaphore::new(4)),
        codegen: ResolvedCodegen {
            style: "flat".to_string(),
            strip_extension: true,
            ts_declaration: false,
            strip_whitespace: false,
        },
        asset_type_override: None,
        errors: 0,
    }
}
