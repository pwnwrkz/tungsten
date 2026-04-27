use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::api::sync::debug::DebugSync;
use crate::api::sync::roblox::Creator;
use crate::api::sync::studio::StudioSync;
use crate::api::upload::{RobloxClient, UploadParams};
use crate::core::assets::asset::{AssetKind, AssetMeta, ImageFormat};
use crate::core::assets::img::alpha_bleed::alpha_bleed;
use crate::core::assets::img::compress::CompressOptions;
use crate::core::assets::img::pack;
use crate::core::postsync::codegen::{self, CodegenEntry};
use crate::core::postsync::lockfile::{Lockfile, hash_image};
use crate::log;
use crate::utils::logger::{clear_progress_line, progress};

use super::Target;
use super::codegen_write::write_codegen;
use super::encode::{encode_png, group_dpi_variants};

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
    target: Target,
    dry_run: bool,
    creator: &Creator,
    client: &Option<Arc<RobloxClient>>,
    studio_sync: &Option<Arc<StudioSync>>,
    debug_sync: &Option<Arc<DebugSync>>,
    lockfile: &mut Lockfile,
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

    // DPI groups
    if !dpi_groups.is_empty() {
        let mut all_scales: std::collections::BTreeSet<u8> = std::collections::BTreeSet::new();
        for variants in dpi_groups.values() {
            for &(scale, _) in variants {
                all_scales.insert(scale);
            }
        }

        let mut dpi_ids: HashMap<String, Vec<(u8, u64)>> = HashMap::new();

        for scale in all_scales {
            let scale_images: Vec<pack::InputImage> = dpi_groups
                .iter()
                .filter_map(|(base, variants)| {
                    variants
                        .iter()
                        .find(|(s, _)| *s == scale)
                        .map(|(_, img)| pack::InputImage {
                            name: base.clone(),
                            image: img.image.clone(),
                        })
                })
                .collect();

            if scale_images.is_empty() {
                continue;
            }

            log!(
                info,
                "Packing {}x ({} image(s))...",
                scale,
                scale_images.len()
            );

            let spritesheets = match pack::pack(scale_images) {
                Ok(s) => s,
                Err(e) => {
                    clear_progress_line();
                    log!(warn, "Failed to pack {}x images: {}", scale, e);
                    errors += 1;
                    continue;
                }
            };

            for (idx, sheet) in spritesheets.iter().enumerate() {
                let mut sheet_image = sheet.image.clone();
                alpha_bleed(&mut sheet_image);

                let png_bytes = match encode_png(&sheet_image) {
                    Ok(b) => b,
                    Err(e) => {
                        clear_progress_line();
                        log!(
                            warn,
                            "Failed to encode {}x sheet #{}: {}",
                            scale,
                            idx + 1,
                            e
                        );
                        errors += 1;
                        continue;
                    }
                };

                let png_bytes = maybe_compress_png(png_bytes, compress_options);
                let hash = hash_image(&png_bytes);
                let sheet_name = format!("{}_{}x_{:03}", sheet_base, scale, idx + 1);

                let asset_ref = upload_or_copy_sheet(
                    &png_bytes,
                    &hash,
                    &sheet_name,
                    &sheet_description,
                    input_name,
                    target,
                    dry_run,
                    creator,
                    client,
                    studio_sync,
                    debug_sync,
                    lockfile,
                )
                .await;

                let asset_ref = match asset_ref {
                    Ok(r) => r,
                    Err(e) => {
                        log!(warn, "{}", e);
                        errors += 1;
                        continue;
                    }
                };

                let asset_id = match &asset_ref {
                    codegen::AssetRef::Id(id) => *id,
                    codegen::AssetRef::Uri(_) => lockfile.get(input_name, &hash).unwrap_or(0),
                };

                for img in &sheet.images {
                    dpi_ids
                        .entry(img.name.clone())
                        .or_default()
                        .push((scale, asset_id));
                }
            }
        }

        for (name, mut variants) in dpi_ids {
            variants.sort_by_key(|(s, _)| *s);
            codegen_entries.push(CodegenEntry::dpi_group(name, variants));
        }
    }

    // Plain images
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
        for (idx, sheet) in spritesheets.iter().enumerate() {
            let mut sheet_image = sheet.image.clone();
            alpha_bleed(&mut sheet_image);

            let png_bytes = match encode_png(&sheet_image) {
                Ok(b) => b,
                Err(e) => {
                    clear_progress_line();
                    log!(warn, "Failed to encode sheet #{}: {}", idx + 1, e);
                    errors += 1;
                    continue;
                }
            };

            let png_bytes = maybe_compress_png(png_bytes, compress_options);
            let hash = hash_image(&png_bytes);
            let sheet_name = format!("{}_{:03}", sheet_base, idx + 1);
            progress("Packing", idx + 1, sheet_total, &sheet_name);

            let asset_ref = upload_or_copy_sheet(
                &png_bytes,
                &hash,
                &sheet_name,
                &sheet_description,
                input_name,
                target,
                dry_run,
                creator,
                client,
                studio_sync,
                debug_sync,
                lockfile,
            )
            .await;

            let asset_ref = match asset_ref {
                Ok(r) => r,
                Err(e) => {
                    log!(warn, "{}", e);
                    errors += 1;
                    continue;
                }
            };

            for img in &sheet.images {
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
    client: &Option<Arc<RobloxClient>>,
    studio_sync: &Option<Arc<StudioSync>>,
    debug_sync: &Option<Arc<DebugSync>>,
    lockfile: &mut Lockfile,
) -> Result<codegen::AssetRef> {
    if dry_run {
        return Ok(codegen::AssetRef::Id(0));
    }

    match target {
        Target::Cloud => {
            if let Some(cached) = lockfile.get(input_name, hash) {
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
                ss.copy_asset(&rel, png_bytes)
                    .with_context(|| format!("Studio copy failed for \"{}\"", sheet_name))?
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
