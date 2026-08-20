use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
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
use super::codegen_write::{seed_web_assets, write_codegen};
use super::dispatch::{copy_to_debug, copy_to_studio};
use super::dpi;
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
    max_concurrent_uploads: usize,
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
    seed_web_assets(web_assets, base_path, strip_extension, &mut codegen_entries);

    // DPI groups - process as individual per-variant uploads/copies instead of
    // waitlisting them. Each base emits exactly one `dpi_group` codegen entry.
    errors += dpi::process_dpi_groups(
        input_name,
        dpi_groups,
        bleed,
        compress_options,
        target,
        dry_run,
        creator,
        asset_type,
        client,
        studio_sync,
        debug_sync,
        lockfile,
        studio_expected_files,
        max_concurrent_uploads,
        &mut codegen_entries,
    )
    .await;

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

        // Pre-process all sheets: bleed, encode, compress in parallel (offloaded to blocking pool)
        #[derive(Debug)]
        struct ProcessedSheet {
            bytes: Vec<u8>,
            hash: String,
        }

        let bleed_flag = bleed;
        // Owned copies so the `'static` blocking closure doesn't capture
        // borrowed references; the original `spritesheets` is still needed
        // afterwards for codegen.
        let compress_opts = compress_options.cloned();
        let spritesheets_vec = spritesheets.clone();

        let processed_sheets: Vec<
            Result<ProcessedSheet, Box<dyn std::error::Error + Send + Sync>>,
        > = tokio::task::spawn_blocking(move || {
            spritesheets_vec
                .par_iter()
                .map(
                    |sheet| -> Result<ProcessedSheet, Box<dyn std::error::Error + Send + Sync>> {
                        let mut sheet_image: RgbaImage = sheet.image.clone();
                        if bleed_flag {
                            alpha_bleed(&mut sheet_image);
                        }
                        let png_bytes: Vec<u8> = encode_png(&sheet_image)?;
                        let png_bytes: Vec<u8> =
                            maybe_compress_png(png_bytes, compress_opts.as_ref());
                        let hash: String = hash_image(&png_bytes);
                        Ok(ProcessedSheet {
                            bytes: png_bytes,
                            hash,
                        })
                    },
                )
                .collect()
        })
        .await
        .expect("spawn_blocking panicked");

        codegen_entries.reserve(spritesheets.len() * 2);

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
                    data: Bytes::copy_from_slice(png_bytes),
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
            let uri = copy_to_studio(studio_sync, studio_expected_files, &rel, png_bytes)
                .with_context(|| format!("Studio copy failed for \"{}\"", sheet_name))?;
            lockfile.set_uri(input_name, hash.to_string(), uri.clone());
            Ok(codegen::AssetRef::Uri(uri))
        }
        Target::Debug => {
            let rel = format!("{}.png", sheet_name);
            copy_to_debug(debug_sync, &rel, png_bytes)
                .with_context(|| format!("Debug copy failed for \"{}\"", sheet_name))?;
            Ok(codegen::AssetRef::Id(
                lockfile.get(input_name, hash).unwrap_or(0),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::sync::roblox::{Creator, UserCreator};

    #[test]
    fn packed_images_are_added_to_codegen() {
        let output_dir = tempfile::tempdir().unwrap();
        let output_path = output_dir.path().join("icons.luau");
        let images = vec![pack::InputImage {
            name: "settings".to_string(),
            image: RgbaImage::new(16, 16),
        }];
        let creator = Creator::User(UserCreator {
            user_id: "0".to_string(),
        });
        let client = None;
        let studio_sync = None;
        let debug_sync = None;
        let mut lockfile = Lockfile::default();
        let mut studio_expected_files = None;
        let web_assets = HashMap::new();

        let errors = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(process_packed(
                "icons",
                &AssetMeta::default(),
                images,
                output_path.to_str().unwrap(),
                "flat",
                true,
                false,
                None,
                false,
                Target::Debug,
                false,
                &creator,
                None,
                &client,
                &studio_sync,
                &debug_sync,
                &mut lockfile,
                &mut studio_expected_files,
                1,
                &web_assets,
                "",
            ));

        assert_eq!(errors, 0);
        let generated = std::fs::read_to_string(output_path).unwrap();
        assert!(generated.contains("settings"));
        assert!(generated.contains("ImageRectOffset"));
        assert!(generated.contains("ImageRectSize"));
    }
}
