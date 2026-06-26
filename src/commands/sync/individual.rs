use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rayon::prelude::*;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::api::sync::debug::DebugSync;
use crate::api::sync::roblox::Creator;
use crate::api::sync::studio::StudioSync;
use crate::api::upload::{RobloxClient, UploadParams};
use crate::core::assets::asset::{AssetKind, AssetMeta, ImageFormat};
use crate::core::assets::img::alpha_bleed::alpha_bleed;
use crate::core::assets::img::compress::CompressOptions;
use crate::core::postsync::codegen::{self, CodegenEntry};
use crate::core::postsync::lockfile::{Lockfile, hash_image};
use crate::log;
use crate::utils::logger::{clear_progress_line, progress};

use super::Target;
use super::codegen_write::write_codegen;
use super::encode::{encode_png, group_dpi_variants};
use super::paths::relative_path;

struct Pending {
    name: String,
    path: PathBuf,
    bytes: Vec<u8>,
    hash: String,
    kind: AssetKind,
    display_name: String,
    description: String,
    asset_type: Option<String>,
}

/// Error type for asset processing failures
#[derive(Debug)]
struct ProcessingError {
    error: anyhow::Error,
}

/// Optionally compress PNG bytes before upload.
fn maybe_compress_png(bytes: Vec<u8>, compress_options: Option<&CompressOptions>) -> Vec<u8> {
    let Some(opts) = compress_options else {
        return bytes;
    };
    match crate::core::assets::img::compress::compress_image(&bytes, "png", opts) {
        Ok(compressed) => compressed,
        Err(e) => {
            clear_progress_line();
            log!(warn, "Compression failed, using original: {}", e);
            bytes
        }
    }
}

/// Process a single image for individual asset processing (synchronous version for parallel processing)
fn process_single_image_sync(
    img: crate::core::assets::img::pack::InputImage,
    paths: &[PathBuf],
    base_path: &str,
    compress_options: Option<&CompressOptions>,
    bleed: bool,
    _creator: &Creator,
    asset_type: &str,
) -> Result<Pending, ProcessingError> {
    // Find the actual file path for this image
    let path = paths
        .iter()
        .find(|p| {
            let rel = relative_path(p, base_path);
            let rel_stem = Path::new(&rel)
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/");
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            rel_stem == img.name || stem == img.name.rsplit('/').next().unwrap_or(&img.name)
        })
        .cloned()
        .unwrap_or_else(|| PathBuf::from(&img.name));

    // Process the image: optionally alpha bleed, encode, compress, hash
    let mut rgba = img.image.clone();
    if bleed {
        alpha_bleed(&mut rgba);
    }
    let bytes = encode_png(&rgba).map_err(|e| ProcessingError {
        error: anyhow::anyhow!("Failed to encode \"{}\": {}", img.name, e),
    })?;

    let bytes = maybe_compress_png(bytes, compress_options);
    let hash = hash_image(&bytes);
    let kind = AssetKind::Image(ImageFormat::Png);
    let meta = AssetMeta::load_for(&path).unwrap_or_default();
    let display_name = meta.resolve_name(&img.name).to_string();
    let description = meta.resolve_description("Uploaded by Tungsten").to_string();

    Ok(Pending {
        name: img.name,
        path,
        bytes,
        hash,
        kind,
        display_name,
        description,
        asset_type: Some(asset_type.to_string()),
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn process_individual(
    input_name: &str,
    images: Vec<crate::core::assets::img::pack::InputImage>,
    image_paths: Vec<PathBuf>,
    svg_scale: f32,
    base_path: &str,
    output_path: &str,
    codegen_style: &str,
    strip_extension: bool,
    ts_declaration: bool,
    compress_options: Option<&CompressOptions>,
    bleed: bool,
    target: Target,
    dry_run: bool,
    creator: &Creator,
    asset_type: &str,
    client: &Option<Arc<RobloxClient>>,
    studio_sync: &Option<Arc<StudioSync>>,
    debug_sync: &Option<Arc<DebugSync>>,
    lockfile: &mut Lockfile,
) -> u32 {
    let mut errors: u32 = 0;
    let total = images.len();
    let _ = svg_scale;

    let (dpi_groups, plain_images) = group_dpi_variants(images);

    // Process plain images in parallel for CPU-bound operations
    let mut pending: Vec<Pending> = Vec::with_capacity(total);
    let pending_results: Vec<Result<Pending, ProcessingError>> = plain_images
        .into_par_iter()
        .map(|img| {
            process_single_image_sync(
                img,
                &image_paths,
                base_path,
                compress_options,
                bleed,
                creator,
                asset_type,
            )
        })
        .collect::<Vec<_>>();

    // Collect results and count errors
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

    let mut codegen_entries: Vec<CodegenEntry> = Vec::with_capacity(total);

    // Configure upload concurrency limit (can be made configurable later)
    const MAX_CONCURRENT_UPLOADS: usize = 10;
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_UPLOADS));

    let mut upload_tasks: JoinSet<Result<(String, u64, String)>> = JoinSet::new();
    let mut dispatched = 0usize;

    // Plain images
    for p in &pending {
        if dry_run {
            dispatched += 1;
            progress("Uploading", dispatched, total, p.name.as_str());
            codegen_entries.push(CodegenEntry::asset_id(p.name.clone(), 0));
            continue;
        }

        match target {
            Target::Studio => {
                dispatched += 1;
                let rel = format!("{}.png", p.name);
                let uri = if let Some(ss) = studio_sync {
                    match ss.copy_asset(&rel, &p.bytes) {
                        Ok(u) => u,
                        Err(e) => {
                            clear_progress_line();
                            log!(warn, "Studio copy failed for \"{}\": {}", p.name, e);
                            errors += 1;
                            continue;
                        }
                    }
                } else {
                    String::new()
                };
                lockfile.set_uri(input_name, p.hash.clone(), uri.clone());
                progress("Copying", dispatched, total, p.name.as_str());
                codegen_entries.push(CodegenEntry::asset(
                    p.name.clone(),
                    codegen::AssetRef::Uri(uri),
                ));
            }
            Target::Debug => {
                dispatched += 1;
                let rel = format!("{}.png", p.name);
                if let Some(ds) = debug_sync
                    && let Err(e) = ds.copy_asset(&rel, &p.bytes)
                {
                    clear_progress_line();
                    log!(warn, "Debug copy failed for \"{}\": {}", p.name, e);
                    errors += 1;
                    continue;
                }
                let fallback = lockfile.get(input_name, &p.hash).unwrap_or(0);
                progress("Copying", dispatched, total, p.name.as_str());
                codegen_entries.push(CodegenEntry::asset_id(p.name.clone(), fallback));
            }
            Target::Cloud => {
                if let Some(cached_id) = lockfile.get(input_name, &p.hash) {
                    dispatched += 1;
                    progress("Uploading", dispatched, total, p.name.as_str());
                    codegen_entries.push(CodegenEntry::asset_id(p.name.clone(), cached_id));
                    continue;
                }
                let Some(c) = client else {
                    codegen_entries.push(CodegenEntry::asset_id(p.name.clone(), 0));
                    continue;
                };
                let c_arc = Arc::clone(c);
                let file_name = p
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let name_clone = p.name.clone();
                let hash_clone = p.hash.clone();
                let display_name_clone = p.display_name.clone();
                let description_clone = p.description.clone();
                let bytes_clone = p.bytes.clone();
                let kind_clone = p.kind;
                let asset_type_clone = p.asset_type.clone();
                let semaphore_clone = semaphore.clone();
                let creator_own = creator.clone();
                upload_tasks.spawn(async move {
                    let _permit = semaphore_clone.acquire_owned().await;
                    let id = c_arc
                        .upload(UploadParams {
                            file_name: file_name.clone(),
                            display_name: display_name_clone.clone(),
                            description: description_clone.clone(),
                            data: bytes_clone.clone(),
                            kind: kind_clone,
                            asset_type_override: asset_type_clone
                                .clone()
                                .or_else(|| Some(kind_clone.api_type().to_string())),
                            creator: creator_own,
                        })
                        .await
                        .with_context(|| format!("Failed to upload \"{}\"", name_clone))?;
                    Ok((name_clone, id, hash_clone))
                });
            }
        }
    }

    // DPI group variants - keep sequential for now to avoid complexity
    // TODO: Parallelize DPI variant processing in a future iteration
    for (base_name, variants) in dpi_groups {
        if dry_run {
            dispatched += 1;
            progress("Uploading", dispatched, total, base_name.as_str());
            let fake: Vec<(u8, u64)> = variants.iter().map(|(s, _)| (*s, 0)).collect();
            codegen_entries.push(CodegenEntry::dpi_group(base_name.to_string(), fake));
            continue;
        }

        let mut resolved_variants: Vec<(u8, u64)> = Vec::with_capacity(variants.len());

        for (scale, img) in variants {
            let mut rgba = img.image.clone();
            if bleed {
                alpha_bleed(&mut rgba);
            }
            let bytes = match encode_png(&rgba) {
                Ok(b) => b,
                Err(e) => {
                    clear_progress_line();
                    log!(warn, "Failed to encode {}@{}x: {}", base_name, scale, e);
                    errors += 1;
                    continue;
                }
            };

            let bytes = maybe_compress_png(bytes, compress_options);
            let hash = hash_image(&bytes);

            match target {
                Target::Cloud => {
                    if let Some(cached) = lockfile.get(input_name, &hash) {
                        dispatched += 1;
                        progress("Uploading", dispatched, total, base_name.as_str());
                        resolved_variants.push((scale, cached));
                        continue;
                    }
                    let Some(c) = client else {
                        resolved_variants.push((scale, 0));
                        continue;
                    };
                    let file_name = format!(
                        "{}@{}x.png",
                        base_name.rsplit('/').next().unwrap_or(&base_name),
                        scale
                    );
                    match c
                        .upload(UploadParams {
                            file_name,
                            display_name: format!("{}@{}x", base_name, scale),
                            description: "Uploaded by Tungsten".to_string(),
                            data: bytes,
                            kind: AssetKind::Image(ImageFormat::Png),
                            asset_type_override: Some(asset_type.to_string()),
                            creator: creator.clone(),
                        })
                        .await
                    {
                        Ok(id) => {
                            lockfile.set(input_name, hash, id);
                            dispatched += 1;
                            progress("Uploading", dispatched, total, base_name.as_str());
                            resolved_variants.push((scale, id));
                        }
                        Err(e) => {
                            clear_progress_line();
                            log!(
                                warn,
                                "Upload failed for \"{}\" @{}x: {}",
                                base_name,
                                scale,
                                e
                            );
                            errors += 1;
                        }
                    }
                }
                Target::Studio => {
                    let rel = format!("{}@{}x.png", base_name, scale);
                    let uri = if let Some(ss) = studio_sync {
                        match ss.copy_asset(&rel, &bytes) {
                            Ok(u) => u,
                            Err(e) => {
                                clear_progress_line();
                                log!(warn, "Studio copy failed: {}", e);
                                errors += 1;
                                continue;
                            }
                        }
                    } else {
                        String::new()
                    };
                    lockfile.set_uri(input_name, hash.clone(), uri);
                    dispatched += 1;
                    progress("Copying", dispatched, total, &base_name);
                    resolved_variants.push((scale, lockfile.get(input_name, &hash).unwrap_or(0)));
                }
                Target::Debug => {
                    let rel = format!("{}@{}x.png", base_name, scale);
                    if let Some(ds) = debug_sync
                        && let Err(e) = ds.copy_asset(&rel, &bytes)
                    {
                        clear_progress_line();
                        log!(warn, "Debug copy failed: {}", e);
                        errors += 1;
                        continue;
                    }
                    dispatched += 1;
                    progress("Copying", dispatched, total, &base_name);
                    resolved_variants.push((scale, lockfile.get(input_name, &hash).unwrap_or(0)));
                }
            }
        }

        if !resolved_variants.is_empty() {
            resolved_variants.sort_by_key(|(s, _)| *s);
            codegen_entries.push(CodegenEntry::dpi_group(
                base_name.to_string(),
                resolved_variants,
            ));
        }
    }

    // Cloud upload results
    let mut completed = 0usize;
    while let Some(res) = upload_tasks.join_next().await {
        completed += 1;
        match res {
            Ok(Ok((name, id, hash))) => {
                lockfile.set(input_name, hash, id);
                progress("Uploading", dispatched + completed, total, &name);
                codegen_entries.push(CodegenEntry::asset_id(name.to_string(), id));
            }
            Ok(Err(e)) => {
                clear_progress_line();
                log!(warn, "{}", e);
                errors += 1;
            }
            Err(e) => {
                clear_progress_line();
                log!(warn, "Upload task panicked: {}", e);
                errors += 1;
            }
        }
    }

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
