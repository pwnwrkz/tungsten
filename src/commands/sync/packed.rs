use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use rayon::prelude::*;
use relative_path::RelativePathBuf;

use crate::api::sync::debug::DebugSync;
use crate::api::sync::roblox::Creator;
use crate::api::sync::studio::StudioSync;
use crate::api::upload::{RobloxClient, UploadParams};
use crate::core::assets::asset::{AssetKind, AssetMeta, ImageFormat, WebAsset};
use crate::core::assets::img::alpha_bleed::alpha_bleed;
use crate::core::assets::img::compress::{CompressOptions, maybe_compress_png};
use crate::core::assets::img::pack;
use crate::core::postsync::codegen::{self, CodegenEntry};
use crate::core::postsync::lockfile::{Lockfile, hash_image};
use crate::log;
use crate::utils::logger::{clear_progress_line, progress};
use image::RgbaImage;

use super::Target;
use super::codegen_write::write_codegen;
use super::encode::{encode_png, group_dpi_variants};

#[allow(clippy::too_many_arguments)]
pub async fn process_packed(
    input_name: &str,
    sheet_meta: &AssetMeta,
    images: Vec<pack::InputImage>,
    output_path: &str,
    codegen_style: &str,
    strip_extension: bool,
    ts_declaration: bool,
    compress_options: Option<&CompressOptions>,
    bleed: bool,
    target: Target,
    dry_run: bool,
    creator: &Creator,
    asset_type: Option<&str>,
    client: &Option<Arc<RobloxClient>>,
    studio_sync: &Option<Arc<StudioSync>>,
    debug_sync: &Option<Arc<DebugSync>>,
    lockfile: &mut Lockfile,
    studio_expected_files: &mut Option<&mut HashSet<String>>,
    _max_concurrent_uploads: usize,
    web_assets: &HashMap<RelativePathBuf, WebAsset>,
    base_path: &str,
) -> u32 {
    let mut errors: u32 = 0;

    let sheet_base = sheet_meta
        .name
        .as_deref()
        .map(|n| n.to_string())
        .unwrap_or_else(|| format!("tungsten_{}", input_name));
    let sheet_description = sheet_meta
        .description
        .as_deref()
        .unwrap_or("Uploaded by Tungsten")
        .to_string();

    let (dpi_groups, plain_images) = group_dpi_variants(images);
    let mut codegen_entries: Vec<CodegenEntry> = Vec::new();

    // Seed web assets into codegen entries
    for (rel_path, web_asset) in web_assets {
        let name = rel_path.to_string().replace('\\', "/");
        let name = name
            .strip_prefix(base_path.trim_end_matches('/'))
            .unwrap_or(&name);
        let name = name.trim_start_matches('/');
        let key = if strip_extension {
            name.trim_end_matches('.').to_string()
        } else {
            name.to_string()
        };
        codegen_entries.push(CodegenEntry::asset(
            key,
            codegen::AssetRef::Id(web_asset.id),
        ));
    }

    // DPI groups - skip packing and uploading, create codegen entries with placeholder IDs
    // These go on a waitlist for manual upload later
    if !dpi_groups.is_empty() {
        for (base_name, variants) in dpi_groups {
            // Extract unique scales and create placeholder variant data for codegen
            let mut scales: std::collections::HashSet<u8> = std::collections::HashSet::new();
            for &(scale, _) in variants.iter() {
                scales.insert(scale);
            }

            // Convert to sorted vector for consistent codegen
            let mut scale_vec: Vec<u8> = scales.into_iter().collect();
            scale_vec.sort();

            // Create placeholder variants with ID 0 (will be updated during manual upload)
            let placeholder_variants: Vec<(u8, u64)> =
                scale_vec.iter().map(|&scale| (scale, 0)).collect();

            // Create DPI group codegen entry
            codegen_entries.push(CodegenEntry::dpi_group(
                base_name.to_string(),
                placeholder_variants,
            ));
        }
    }

    // Plain images - continue with normal packing and processing
    if !plain_images.is_empty() {
        log!(info, "Packing {} image(s)...", plain_images.len());

        let spritesheets = match pack::pack(plain_images) {
            Ok(s) => s,
            Err(e) => {
                clear_progress_line();
                log!(warn, "Failed to pack images for \"{}\": {}", input_name, e);
                errors += 1;
                write_codegen(
                    codegen_entries,
                    input_name,
                    output_path,
                    codegen_style,
                    strip_extension,
                    ts_declaration,
                    &mut errors,
                );
                return errors;
            }
        };

        let sheet_total = spritesheets.len();

        // Pre-process all sheets: bleed, encode, compress in parallel
        #[derive(Debug)]
        struct ProcessedSheet {
            _image: RgbaImage,
            bytes: Vec<u8>,
            hash: String,
        }

        let processed_sheets: Vec<
            Result<ProcessedSheet, Box<dyn std::error::Error + Send + Sync>>,
        > = spritesheets
            .par_iter()
            .map(
                |sheet| -> Result<ProcessedSheet, Box<dyn std::error::Error + Send + Sync>> {
                    let mut sheet_image: RgbaImage = sheet.image.clone();
                    if bleed {
                        alpha_bleed(&mut sheet_image);
                    }
                    let png_bytes: Vec<u8> = encode_png(&sheet_image)?;
                    let png_bytes: Vec<u8> = maybe_compress_png(png_bytes, compress_options);
                    let hash: String = hash_image(&png_bytes);
                    Ok(ProcessedSheet {
                        _image: sheet_image,
                        bytes: png_bytes,
                        hash,
                    })
                },
            )
            .collect();

        let mut codegen_entries = Vec::with_capacity(spritesheets.len() * 2);

        for (idx, result) in processed_sheets.into_iter().enumerate() {
            let processed = match result {
                Ok(v) => v,
                Err(e) => {
                    clear_progress_line();
                    log!(warn, "Failed to process sheet #{}: {}", idx + 1, e);
                    errors += 1;
                    continue;
                }
            };
            let png_bytes = processed.bytes;
            let hash = processed.hash;
            let sheet_name = format!("{}_{:03}", sheet_base, idx + 1);
            progress("Packing", idx + 1, sheet_total, &sheet_name);

            let asset_ref = match upload_or_copy_sheet(
                &png_bytes,
                &hash,
                &sheet_name,
                &sheet_description,
                input_name,
                target,
                dry_run,
                creator,
                asset_type,
                client,
                studio_sync,
                debug_sync,
                lockfile,
                studio_expected_files,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    log!(warn, "{}", e);
                    errors += 1;
                    continue;
                }
            };

            for img in &spritesheets[idx].images {
                codegen_entries.push(CodegenEntry::sprite(
                    img.name.clone(),
                    asset_ref.clone(),
                    (img.x, img.y),
                    (img.width, img.height),
                ));
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

#[allow(clippy::too_many_arguments)]
pub async fn upload_or_copy_sheet(
    png_bytes: &[u8],
    hash: &str,
    sheet_name: &str,
    sheet_description: &str,
    input_name: &str,
    target: Target,
    dry_run: bool,
    creator: &Creator,
    asset_type: Option<&str>,
    client: &Option<Arc<RobloxClient>>,
    studio_sync: &Option<Arc<StudioSync>>,
    debug_sync: &Option<Arc<DebugSync>>,
    lockfile: &mut Lockfile,
    studio_expected_files: &mut Option<&mut HashSet<String>>,
) -> Result<codegen::AssetRef> {
    if dry_run {
        return Ok(codegen::AssetRef::Id(0));
    }

    match target {
        Target::Cloud => {
            if let Some(cached) = lockfile.get(input_name, hash) {
                clear_progress_line();
                log!(
                    debug,
                    "{}: unchanged, skipping (cached asset {})",
                    sheet_name,
                    cached
                );
                return Ok(codegen::AssetRef::Id(cached));
            }
            let Some(c) = client else {
                return Ok(codegen::AssetRef::Id(0));
            };
            let id = c
                .upload(UploadParams {
                    file_name: format!("{}.png", sheet_name),
                    display_name: sheet_name.to_string(),
                    description: sheet_description.to_string(),
                    data: png_bytes.to_vec(),
                    kind: AssetKind::Image(ImageFormat::Png),
                    asset_type_override: asset_type.map(|s| s.to_string()),
                    creator: creator.clone(),
                })
                .await
                .with_context(|| format!("Failed to upload \"{}\"", sheet_name))?;
            lockfile.set(input_name, hash.to_string(), id);
            Ok(codegen::AssetRef::Id(id))
        }
        Target::Studio => {
            let rel = format!("{}.png", sheet_name);
            let uri = if let Some(ss) = studio_sync {
                match ss.copy_asset(&rel, png_bytes) {
                    Ok(u) => {
                        // Track expected file for Studio sync cleanup
                        if let Some(ref mut set) = *studio_expected_files {
                            set.insert(rel.clone());
                        }
                        u
                    }
                    Err(e) => {
                        return Err(e)
                            .with_context(|| format!("Studio copy failed for \"{}\"", sheet_name));
                    }
                }
            } else {
                String::new()
            };
            lockfile.set_uri(input_name, hash.to_string(), uri.clone());
            Ok(codegen::AssetRef::Uri(uri))
        }
        Target::Debug => {
            let rel = format!("{}.png", sheet_name);
            if let Some(ds) = debug_sync {
                ds.copy_asset(&rel, png_bytes)
                    .with_context(|| format!("Debug copy failed for \"{}\"", sheet_name))?;
            }
            Ok(codegen::AssetRef::Id(
                lockfile.get(input_name, hash).unwrap_or(0),
            ))
        }
    }
}
