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
use crate::core::assets::asset::{AssetKind, AssetMeta, ImageFormat, WebAsset, stem_name};
use crate::core::assets::img::alpha_bleed::alpha_bleed;
use crate::core::assets::img::compress::{CompressOptions, maybe_compress_png};
use crate::core::assets::img::pack::InputImage;
use crate::core::postsync::codegen::CodegenEntry;
use crate::core::postsync::lockfile::{Lockfile, hash_image};
use crate::log;
use crate::utils::logger::clear_progress_line;

use super::Target;
use super::codegen_write::{seed_web_assets, write_codegen};
use super::dispatch::{DispatchCtx, PendingAsset, collect_upload_results, dispatch_asset};
use super::dpi::process_dpi_groups;
use super::encode::{encode_png, group_dpi_variants, group_paths_by_dpi};
use super::error::ProcessingError;
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

/// A plain (non-DPI) image source: either already decoded (SVG rasterization)
/// or still on disk and decoded lazily inside the blocking pool so memory stays
/// bounded by processing concurrency instead of holding every image at once.
enum PlainSource {
    Decoded(InputImage),
    Path(PathBuf),
}

struct ProcessImageCtx<'a> {
    /// Pre-built index: image name (relative stem) → absolute PathBuf.
    /// Allows O(1) path lookup per image instead of an O(n) linear scan.
    path_index: &'a HashMap<String, PathBuf>,
    compress_options: Option<&'a CompressOptions>,
    bleed: bool,
    asset_type: Option<&'a str>,
}

/// Process a single image for individual asset processing (synchronous version for parallel processing)
#[inline]
fn process_single_image_sync(
    img: InputImage,
    ctx: &ProcessImageCtx<'_>,
) -> Result<Pending, ProcessingError> {
    // O(1) lookup via the pre-built name→path index.
    let path = ctx
        .path_index
        .get(&img.name)
        .cloned()
        .unwrap_or_else(|| PathBuf::from(&img.name));

    // Process the image: optionally alpha bleed, encode, compress, hash
    let mut rgba = img.image;
    if ctx.bleed {
        alpha_bleed(&mut rgba);
    }
    let bytes = encode_png(&rgba).map_err(|e| {
        ProcessingError::new(anyhow::anyhow!("Failed to encode \"{}\": {}", img.name, e))
    })?;

    let bytes = maybe_compress_png(bytes, ctx.compress_options);
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
        asset_type: ctx.asset_type.map(|s| s.to_string()),
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn process_individual(
    input_name: &str,
    svg_images: Vec<InputImage>,
    svg_paths: Vec<PathBuf>,
    raster_paths: Vec<PathBuf>,
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
    asset_type: Option<&str>,
    client: &Option<Arc<RobloxClient>>,
    studio_sync: &Option<Arc<StudioSync>>,
    debug_sync: &Option<Arc<DebugSync>>,
    lockfile: &mut Lockfile,
    studio_expected_files: &mut Option<&mut HashSet<String>>,
    max_concurrent_uploads: usize,
    web_assets: &HashMap<RelativePathBuf, WebAsset>,
) -> u32 {
    let mut errors: u32 = 0;
    let total = svg_images.len() + raster_paths.len();

    // Seed web assets into codegen entries first
    let mut codegen_entries: Vec<CodegenEntry> = Vec::new();
    seed_web_assets(web_assets, base_path, strip_extension, &mut codegen_entries);

    // Pre-build name→path index for O(1) meta lookups inside the parallel map
    // (covers SVG paths and all raster files).
    let mut path_index: HashMap<String, PathBuf> = HashMap::new();
    for p in &svg_paths {
        path_index.insert(stem_name(&relative_path(p, base_path)), p.clone());
    }
    for (path, name) in raster_paths
        .iter()
        .map(|p| (p.clone(), stem_name(&relative_path(p, base_path))))
    {
        path_index.insert(name, path);
    }

    // Split SVG (decoded) and raster (on-disk) images into DPI groups and
    // plain images. Raster DPI variants are few, so they're decoded eagerly;
    // plain raster images are decoded lazily inside the blocking pool.
    let (svg_dpi_groups, svg_plain) = group_dpi_variants(svg_images);
    let (raster_dpi_groups, raster_plain) = group_paths_by_dpi(
        raster_paths
            .into_iter()
            .map(|p| {
                let name = stem_name(&relative_path(&p, base_path));
                (p, name)
            })
            .collect(),
    );

    // Decode the (small number of) raster DPI variants off-thread and merge
    // them into the SVG DPI groups.
    let raster_dpi_flat: Vec<(String, u8, PathBuf)> = raster_dpi_groups
        .into_iter()
        .flat_map(|(base, variants)| {
            variants
                .into_iter()
                .map(move |(scale, path)| (base.clone(), scale, path))
        })
        .collect();
    let dpi_raster_decoded: Vec<(String, u8, InputImage)> =
        tokio::task::spawn_blocking(move || {
            raster_dpi_flat
                .into_par_iter()
                .filter_map(|(base, scale, path)| {
                    let image = image::open(&path).ok()?.into_rgba8();
                    Some((
                        base,
                        scale,
                        InputImage {
                            name: String::new(),
                            image,
                        },
                    ))
                })
                .collect()
        })
        .await
        .expect("spawn_blocking panicked");

    let mut dpi_groups = svg_dpi_groups;
    for (base, scale, img) in dpi_raster_decoded {
        dpi_groups.entry(base).or_default().push((scale, img));
    }
    for variants in dpi_groups.values_mut() {
        variants.sort_by_key(|(s, _)| *s);
    }

    // Plain sources: decoded SVGs plus on-disk raster paths.
    let plain_sources: Vec<PlainSource> = svg_plain
        .into_iter()
        .map(PlainSource::Decoded)
        .chain(
            raster_plain
                .into_iter()
                .map(|(path, _)| PlainSource::Path(path)),
        )
        .collect();

    // Process plain images in parallel for CPU-bound operations (offloaded to
    // blocking pool). Raster files are decoded right here, lazily.
    let ctx_bleed = bleed;
    // Owned copies so the `'static` blocking closure doesn't capture borrowed
    // references into the async context.
    let ctx_compress = compress_options.cloned();
    let ctx_asset_type = asset_type.map(str::to_owned);
    let base_path_owned = base_path.to_string();
    let path_index_owned = path_index;

    let pending_results: Vec<Result<Pending, ProcessingError>> =
        tokio::task::spawn_blocking(move || {
            let ctx = ProcessImageCtx {
                path_index: &path_index_owned,
                compress_options: ctx_compress.as_ref(),
                bleed: ctx_bleed,
                asset_type: ctx_asset_type.as_deref(),
            };
            plain_sources
                .into_par_iter()
                .map(|src| {
                    let img = match src {
                        PlainSource::Decoded(img) => img,
                        PlainSource::Path(path) => {
                            let image = image::open(&path)
                                .map_err(|e| {
                                    ProcessingError::new(anyhow::anyhow!(
                                        "Failed to open image \"{}\": {}",
                                        path.display(),
                                        e
                                    ))
                                })?
                                .into_rgba8();
                            let name = stem_name(&relative_path(&path, &base_path_owned));
                            InputImage { name, image }
                        }
                    };
                    process_single_image_sync(img, &ctx)
                })
                .collect::<Vec<_>>()
        })
        .await
        .expect("spawn_blocking panicked");

    // Collect results and count errors
    let mut pending: Vec<Pending> = Vec::with_capacity(total);
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

    // Configure upload concurrency limit from config
    let semaphore = Arc::new(Semaphore::new(max_concurrent_uploads));

    let mut upload_tasks: JoinSet<Result<(String, u64, String)>> = JoinSet::new();
    let mut dispatched = 0usize;

    // Unified dispatch: dry-run, Studio/Debug copies, cache hits and cloud
    // uploads are handled identically to `raw` (see `dispatch::dispatch_asset`).
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
            let name = p.name;
            let rel = format!("{}.png", name);
            dispatch_asset(
                PendingAsset {
                    name,
                    path: p.path,
                    bytes: Bytes::from(p.bytes),
                    hash: p.hash,
                    kind: p.kind,
                    display_name: p.display_name,
                    description: p.description,
                    asset_type_override: p
                        .asset_type
                        .or_else(|| Some(p.kind.api_type().to_string())),
                    studio_rel: rel.clone(),
                    debug_rel: rel,
                    studio_needs_upload: false,
                },
                &mut dispatch_ctx,
                &mut codegen_entries,
            );
        }
    }

    // DPI groups: encode/upload/copy + codegen via the shared processor.
    // Produces exactly one `dpi_group` entry per base name.
    errors += process_dpi_groups(
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
