//! Spritesheet packing: bin-packs images into sheets, composites, and dispatches the result.

use std::collections::HashMap;

use anyhow::{Context, Result};
use bytes::Bytes;
use rayon::prelude::*;
use relative_path::RelativePathBuf;

use super::Target;
use super::codegen_write::{seed_web_assets, write_codegen};
use super::dispatch::{SyncContext, copy_to_debug, copy_to_studio};
use super::dpi;
use super::dpi_groups::group_dpi_variants;
use crate::api::upload::UploadParams;
use crate::core::assets::asset::{AssetKind, AssetMeta, ImageFormat, WebAsset};
use crate::core::assets::img::alpha_bleed::alpha_bleed;
use crate::core::assets::img::compress::{CompressOptions, maybe_compress_png};
use crate::core::assets::img::convert::encode_png;
use crate::core::assets::img::pack;
use crate::core::postsync::codegen::{self, CodegenEntry};
use crate::core::postsync::lockfile::hash_bytes;
use crate::log;
use crate::utils::logger::{clear_progress_line, progress};

pub async fn process_packed(
    ctx: &mut SyncContext<'_>,
    sheet_meta: &AssetMeta,
    images: Vec<pack::InputImage>,
    compress_options: Option<&CompressOptions>,
    bleed: bool,
    uniform: Option<&pack::UniformOptions>,
    web_assets: &HashMap<RelativePathBuf, WebAsset>,
) -> u32 {
    let input_name = ctx.input_name;
    let base_path = ctx.base_path;

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
    seed_web_assets(
        web_assets,
        base_path,
        ctx.codegen.strip_extension,
        ctx.codegen.strip_whitespace,
        &mut codegen_entries,
    );

    // DPI groups - process as individual per-variant uploads/copies instead of
    // waitlisting them. Each base emits exactly one `dpi_group` codegen entry.
    ctx.errors += dpi::process_dpi_groups(
        ctx,
        dpi_groups,
        bleed,
        compress_options,
        &mut codegen_entries,
    )
    .await;

    // Plain images - continue with normal packing and processing
    if !plain_images.is_empty() {
        log!(info, "Packing {} image(s)...", plain_images.len());

        let pack_result = match uniform {
            Some(opts) => pack::pack_uniform(plain_images, opts),
            None => pack::pack(plain_images),
        };

        let spritesheets = match pack_result {
            Ok(s) => s,
            Err(e) => {
                clear_progress_line();
                log!(warn, "Failed to pack images for \"{}\": {}", input_name, e);
                ctx.errors += 1;
                write_codegen(ctx, codegen_entries);
                return ctx.errors;
            }
        };

        let sheet_total = spritesheets.len();

        // Pre-process all sheets: bleed, encode, compress in parallel (offloaded to blocking pool).
        // Each sheet produces its encoded bytes + hash, plus the sprite metadata needed for
        // codegen — so we don't need to keep the original `spritesheets` vector afterward.
        #[derive(Debug)]
        struct ProcessedSheet {
            bytes: Vec<u8>,
            hash: String,
            /// Sprite metadata cloned from the packed layout for codegen.
            images: Vec<pack::PackedImage>,
        }

        // Owned copy so the `'static` blocking closure doesn't capture borrowed refs.
        let compress_opts = compress_options.cloned();

        let processed_sheets: Vec<
            Result<ProcessedSheet, Box<dyn std::error::Error + Send + Sync>>,
        > = tokio::task::spawn_blocking(move || {
            spritesheets
                .into_par_iter()
                .map(
                    |sheet| -> Result<ProcessedSheet, Box<dyn std::error::Error + Send + Sync>> {
                        let mut sheet_image = sheet.image;
                        if bleed {
                            alpha_bleed(&mut sheet_image);
                        }
                        let png_bytes: Vec<u8> = encode_png(&sheet_image)?;
                        let png_bytes: Vec<u8> =
                            maybe_compress_png(png_bytes, compress_opts.as_ref());
                        let hash: String = hash_bytes(&png_bytes);
                        Ok(ProcessedSheet {
                            bytes: png_bytes,
                            hash,
                            images: sheet.images,
                        })
                    },
                )
                .collect()
        })
        .await
        .expect("spawn_blocking panicked");

        codegen_entries.reserve(sheet_total * 2);

        for (idx, result) in processed_sheets.into_iter().enumerate() {
            let processed = match result {
                Ok(v) => v,
                Err(e) => {
                    clear_progress_line();
                    log!(warn, "Failed to process sheet #{}: {}", idx + 1, e);
                    ctx.errors += 1;
                    continue;
                }
            };
            let png_bytes = processed.bytes;
            let hash = processed.hash;
            let sheet_name = format!("{}_{:03}", sheet_base, idx + 1);
            progress("Packing", idx + 1, sheet_total, &sheet_name);

            let cached = sheet_cache_hit(ctx, &processed.images, &hash);

            let (asset_ref, sheet_id, sheet_uri) = match upload_or_copy_sheet(
                ctx,
                sheet_meta,
                &png_bytes,
                &sheet_name,
                &sheet_description,
                cached,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    log!(warn, "{}", e);
                    ctx.errors += 1;
                    continue;
                }
            };

            // v3 lockfile: one entry per sprite, sharing the sheet's
            // hash/id/uri, distinguished by its slice.
            let sheet_index = idx as u32;
            for img in &processed.images {
                ctx.lockfile.upsert_sprite(
                    ctx.input_name,
                    &img.name,
                    "image/png",
                    &img.source_hash,
                    &hash,
                    sheet_id,
                    sheet_uri.clone(),
                    crate::core::postsync::lockfile::Slice {
                        sheet: sheet_index,
                        x: img.x,
                        y: img.y,
                        width: img.width,
                        height: img.height,
                    },
                );
            }

            for img in &processed.images {
                codegen_entries.push(CodegenEntry::sprite(
                    img.name.clone(),
                    asset_ref.clone(),
                    (img.x, img.y),
                    (img.width, img.height),
                ));
            }
        }
    }

    write_codegen(ctx, codegen_entries);
    ctx.errors
}

/// A sheet is cached when its first sprite's v3 entry already carries this
/// sheet's processed hash and an id/uri — sprites share the sheet's fields,
/// so one representative is enough.
fn sheet_cache_hit(
    ctx: &SyncContext<'_>,
    sprites: &[pack::PackedImage],
    sheet_hash: &str,
) -> Option<(Option<u64>, Option<String>)> {
    let first = sprites.first()?;
    let e = ctx.lockfile.entry(ctx.input_name, &first.name)?;
    (e.hash.as_deref() == Some(sheet_hash)).then(|| (e.id, e.studio_uri.clone()))
}

async fn upload_or_copy_sheet(
    ctx: &mut SyncContext<'_>,
    sheet_meta: &AssetMeta,
    png_bytes: &[u8],
    sheet_name: &str,
    sheet_description: &str,
    cached: Option<(Option<u64>, Option<String>)>,
) -> Result<(codegen::AssetRef, Option<u64>, Option<String>)> {
    if ctx.dry_run {
        return Ok((codegen::AssetRef::Id(0), None, None));
    }

    match ctx.target {
        Target::Cloud => {
            if let Some((Some(cached_id), uri)) = cached {
                clear_progress_line();
                log!(
                    debug,
                    "{}: unchanged, skipping (cached asset {})",
                    sheet_name,
                    cached_id
                );
                return Ok((codegen::AssetRef::Id(cached_id), Some(cached_id), uri));
            }
            let Some(c) = &ctx.client else {
                return Ok((codegen::AssetRef::Id(0), None, None));
            };
            let id = c
                .upload(UploadParams {
                    file_name: format!("{}.png", sheet_name),
                    display_name: sheet_name.to_string(),
                    description: sheet_description.to_string(),
                    data: Bytes::copy_from_slice(png_bytes),
                    kind: AssetKind::Image(ImageFormat::Png),
                    asset_type_override: sheet_meta
                        .resolve_asset_type()
                        .map(|s| s.to_string())
                        .or_else(|| ctx.asset_type_override.clone()),
                    creator: ctx.creator.clone(),
                })
                .await
                .with_context(|| format!("Failed to upload \"{}\"", sheet_name))?;
            Ok((codegen::AssetRef::Id(id), Some(id), None))
        }
        Target::Studio => {
            let rel = format!("{}.png", sheet_name);
            let uri = copy_to_studio(ctx, &rel, png_bytes)
                .with_context(|| format!("Studio copy failed for \"{}\"", sheet_name))?;
            let uri = Some(uri);
            Ok((
                codegen::AssetRef::Uri(uri.clone().unwrap()),
                cached.and_then(|(id, _)| id),
                uri,
            ))
        }
        Target::Debug => {
            let rel = format!("{}.png", sheet_name);
            copy_to_debug(ctx, &rel, png_bytes)
                .with_context(|| format!("Debug copy failed for \"{}\"", sheet_name))?;
            Ok((
                codegen::AssetRef::Id(0),
                cached.and_then(|(id, _)| id),
                None,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    #[test]
    fn packed_images_are_added_to_codegen() {
        let output_dir = tempfile::tempdir().unwrap();
        let output_path = output_dir.path().join("icons.luau");
        let images = vec![pack::InputImage {
            name: "settings".to_string(),
            image: RgbaImage::new(16, 16),
            source_hash: String::new(),
        }];
        let mut lockfile = crate::core::postsync::lockfile::Lockfile::default();
        let mut expected = std::collections::HashSet::new();
        let mut ctx = super::super::dispatch::test_context(
            "icons",
            Target::Debug,
            false,
            &mut lockfile,
            &mut expected,
        );
        ctx.output_path = output_path.to_str().unwrap();
        let web_assets = HashMap::new();

        let errors = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(process_packed(
                &mut ctx,
                &AssetMeta::default(),
                images,
                None,
                false,
                None,
                &web_assets,
            ));

        assert_eq!(errors, 0);
        let generated = std::fs::read_to_string(output_path).unwrap();
        assert!(generated.contains("settings"));
        assert!(generated.contains("ImageRectOffset"));
        assert!(generated.contains("ImageRectSize"));
    }
}
