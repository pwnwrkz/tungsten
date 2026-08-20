use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use rayon::prelude::*;
use relative_path::RelativePathBuf;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::api::sync::debug::DebugSync;
use crate::api::sync::roblox::Creator;
use crate::api::sync::studio::StudioSync;
use crate::api::upload::RobloxClient;
use crate::core::assets::asset::{
    self, AssetKind, AssetMeta, WebAsset, is_animation_file, stem_name,
};
use crate::core::assets::img::compress::{CompressOptions, compress_image};
use crate::core::assets::img::convert;
use crate::core::postsync::codegen::CodegenEntry;
use crate::core::postsync::lockfile::{Lockfile, hash_image};
use crate::log;
use crate::utils::logger::clear_progress_line;

use super::Target;
use super::codegen_write::{seed_web_assets, write_codegen};
use super::dispatch::{DispatchCtx, PendingAsset, collect_upload_results, dispatch_asset};
use super::error::ProcessingError;
use super::paths::relative_path;

pub struct RawPending {
    pub name: String,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub hash: String,
    pub kind: AssetKind,
    pub display_name: String,
    pub description: String,
}

/// Optionally compress `bytes` using the provided options.
/// Returns the (possibly compressed) bytes.
#[inline]
fn maybe_compress(
    bytes: Vec<u8>,
    ext: &str,
    compress_options: Option<&CompressOptions>,
) -> Vec<u8> {
    let Some(opts) = compress_options else {
        return bytes;
    };

    // Common case: format already caesium-compatible (PNG/JPG/...). Compress
    // in place — no clone, `bytes` stays owned for the fallback.
    let ext_lower = ext.to_ascii_lowercase();
    if convert::is_caesium_compatible(&ext_lower) {
        return match compress_image(&bytes, &ext_lower, opts) {
            Ok(Some(compressed)) => compressed,
            Ok(None) => bytes,
            Err(e) => {
                clear_progress_line();
                log!(warn, "Compression failed, using original: {}", e);
                bytes
            }
        };
    }

    // Rare path (BMP/TGA/...): transcode to PNG first. Clone so the original
    // survives if transcoding itself fails — returning empty bytes here would
    // upload a corrupt asset.
    match convert::normalize_for_compression(bytes.clone(), ext) {
        Ok((normalized, norm_ext)) => match compress_image(&normalized, norm_ext, opts) {
            Ok(Some(compressed)) => compressed,
            Ok(None) => normalized,
            Err(e) => {
                clear_progress_line();
                log!(warn, "Compression failed, using original: {}", e);
                normalized
            }
        },
        Err(e) => {
            clear_progress_line();
            log!(warn, "Could not normalize for compression: {}", e);
            bytes
        }
    }
}

/// Process a single raw file for asset processing (synchronous version for parallel processing)
#[inline]
fn process_single_raw_file(
    path: &PathBuf,
    base_path: &str,
    compress_options: Option<&CompressOptions>,
) -> Result<RawPending, ProcessingError> {
    // Read the file
    let data = std::fs::read(path).map_err(|e| {
        ProcessingError::new(anyhow::anyhow!(
            "Failed to read \"{}\": {}",
            path.display(),
            e
        ))
    })?;

    let src_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let kind = match asset::kind_from_ext(&src_ext) {
        Some(k) => {
            // Check if .rbxm/.rbxmx is actually an animation
            if (src_ext == "rbxm" || src_ext == "rbxmx")
                && k == AssetKind::Model(asset::ModelFormat::Roblox)
            {
                if is_animation_file(path).unwrap_or(false) {
                    AssetKind::Animation
                } else {
                    k
                }
            } else {
                k
            }
        }
        None => {
            return Err(ProcessingError::new(anyhow::anyhow!(
                "Unsupported extension \"{}\"",
                src_ext
            )));
        }
    };

    let data = maybe_compress(data, &src_ext, compress_options);
    let hash = hash_image(&data);
    let meta = AssetMeta::load_for(path).unwrap_or_default();
    let name = stem_name(&relative_path(path, base_path));
    let display_name = meta.resolve_name(&name).to_string();
    let description = meta.resolve_description("Uploaded by Tungsten").to_string();

    Ok(RawPending {
        name,
        path: path.clone(),
        bytes: data,
        hash,
        kind,
        display_name,
        description,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn process_raw(
    input_name: &str,
    paths: Vec<PathBuf>,
    base_path: &str,
    output_path: &str,
    codegen_style: &str,
    strip_extension: bool,
    ts_declaration: bool,
    compress_options: Option<&CompressOptions>,
    target: Target,
    dry_run: bool,
    creator: &Creator,
    asset_type: Option<&str>,
    client: &Option<Arc<RobloxClient>>,
    studio_sync: &Option<Arc<StudioSync>>,
    debug_sync: &Option<Arc<DebugSync>>,
    lockfile: &mut Lockfile,
    studio_expected_files: &mut Option<&mut HashSet<String>>,
    max_concurrent_uploads: usize,
    web_assets: &HashMap<RelativePathBuf, WebAsset>,
) -> u32 {
    // Seed web assets into codegen entries first
    let mut codegen_entries: Vec<CodegenEntry> = Vec::new();
    seed_web_assets(web_assets, base_path, strip_extension, &mut codegen_entries);

    // Process files in parallel (offloaded to blocking pool)
    let base_path_owned = base_path.to_string();
    // Owned copy so the `'static` blocking closure doesn't capture a borrowed
    // reference into the async context.
    let compress_opts = compress_options.cloned();
    let paths_vec = paths;

    let pending_results: Vec<Result<RawPending, ProcessingError>> =
        tokio::task::spawn_blocking(move || {
            paths_vec
                .into_par_iter()
                .map(|path| {
                    process_single_raw_file(&path, &base_path_owned, compress_opts.as_ref())
                })
                .collect::<Vec<_>>()
        })
        .await
        .expect("spawn_blocking panicked");

    // Collect results and count errors
    let mut pending: Vec<RawPending> = Vec::new();
    let mut errors = 0u32;
    for result in pending_results {
        match result {
            Ok(p) => pending.push(p),
            Err(e) => {
                clear_progress_line();
                log!(warn, "{}", e.error);
                errors += 1;
            }
        }
    }

    let total = pending.len();

    // Configure upload concurrency limit from config
    let semaphore = Arc::new(Semaphore::new(max_concurrent_uploads));

    let mut upload_tasks: JoinSet<Result<(String, u64, String)>> = JoinSet::new();
    let mut dispatched = 0usize;

    // Unified dispatch: dry-run, Studio/Debug copies, cache hits and cloud
    // uploads are handled identically to `individual` (see `dispatch::dispatch_asset`).
    {
        let mut dispatch_ctx = DispatchCtx {
            input_name,
            target,
            dry_run,
            creator,
            client,
            studio_sync,
            debug_sync,
            lockfile,
            studio_expected_files,
            upload_tasks: &mut upload_tasks,
            semaphore: &semaphore,
            total,
            dispatched: &mut dispatched,
            errors: &mut errors,
        };

        for p in pending {
            let ext = p.path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let studio_needs_upload = matches!(p.kind, AssetKind::Model(_) | AssetKind::Animation);
            let studio_rel = if studio_needs_upload {
                String::new()
            } else if ext.is_empty() {
                p.name.clone()
            } else {
                format!("{}.{}", p.name, ext)
            };
            let debug_rel = if ext.is_empty() {
                format!("{}.bin", p.name)
            } else {
                format!("{}.{}", p.name, ext)
            };

            dispatch_asset(
                PendingAsset {
                    name: p.name,
                    path: p.path,
                    bytes: Bytes::from(p.bytes),
                    hash: p.hash,
                    kind: p.kind,
                    display_name: p.display_name,
                    description: p.description,
                    asset_type_override: asset_type.map(|s| s.to_string()),
                    studio_rel,
                    debug_rel,
                    studio_needs_upload,
                },
                &mut dispatch_ctx,
                &mut codegen_entries,
            );
        }
    }

    // Cloud upload results
    collect_upload_results(
        &mut upload_tasks,
        input_name,
        lockfile,
        &mut codegen_entries,
        total,
        dispatched,
        &mut errors,
    )
    .await;

    write_codegen(
        codegen_entries,
        input_name,
        output_path,
        codegen_style,
        strip_extension,
        ts_declaration,
        &mut errors,
    );
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `maybe_compress` must never return empty bytes when
    /// normalization fails — that would upload a corrupt/empty asset.
    #[test]
    fn maybe_compress_preserves_bytes_when_normalize_fails() {
        let bytes = vec![1u8, 2, 3, 4, 5];
        // "svg" is not caesium-compatible and cannot be transcoded by the
        // image crate, so normalize_for_compression returns Err.
        // The original bytes must survive for the error fallback.
        let out = maybe_compress(bytes.clone(), "svg", Some(&CompressOptions::default()));
        assert_eq!(
            out, bytes,
            "original bytes must be returned on normalize failure"
        );
    }

    /// Compatible formats skip normalization entirely and keep the original
    /// bytes when compression fails or produces no saving.
    #[test]
    fn maybe_compress_compatible_ext_never_empty() {
        let bytes = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let out = maybe_compress(bytes.clone(), "png", Some(&CompressOptions::default()));
        assert_eq!(out, bytes, "garbage PNG must fall back to original bytes");
    }
}
