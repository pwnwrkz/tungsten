//! Individual (non-packed) image processing: per-file encode, optional compress, dispatch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use bytes::Bytes;
use rayon::prelude::*;
use relative_path::RelativePathBuf;

use crate::core::assets::asset::{
    AssetKind, AssetMeta, ImageFormat, SvgScaleCache, WebAsset, effective_svg_scale_for_path,
    new_svg_scale_cache, stem_name,
};
use crate::core::assets::img::alpha_bleed::alpha_bleed;
use crate::core::assets::img::compress::{CompressOptions, maybe_compress_png};
use crate::core::assets::img::pack::InputImage;
use crate::core::postsync::codegen::CodegenEntry;
use crate::core::postsync::lockfile::hash_bytes;
use crate::log;
use crate::utils::logger::{clear_progress_line, progress};

use super::codegen_write::{seed_web_assets, write_codegen};
use super::dispatch::{
    DispatchPhase, PendingAsset, SyncContext, collect_upload_results, dispatch_asset,
};
use super::dpi::process_dpi_groups;
use super::dpi_groups::{DpiGroups, group_paths_by_dpi};
use super::paths::relative_path;
use crate::core::assets::img::convert::{encode_png, svg_to_rgba};

struct Pending {
    name: String,
    path: PathBuf,
    bytes: Vec<u8>,
    /// sha256 of the raw source bytes, for lockfile cache hits.
    source_hash: String,
    hash: String,
    kind: AssetKind,
    display_name: String,
    description: String,
    /// Per-asset API asset type override from `.tmeta` (None = derive from kind).
    asset_type_override: Option<String>,
}

/// A plain (non-DPI) image source, kept on disk until processed so the
/// first asset can be written as soon as the first image is ready and memory
/// stays bounded by processing concurrency.
enum PlainSource {
    Svg(PathBuf),
    Raster(PathBuf),
}

impl PlainSource {
    fn path(&self) -> &Path {
        match self {
            PlainSource::Svg(p) | PlainSource::Raster(p) => p,
        }
    }
}

struct ProcessImageCtx<'a> {
    /// Pre-built index: image name (relative stem) → absolute PathBuf.
    /// Allows O(1) path lookup per image instead of an O(n) linear scan.
    path_index: &'a HashMap<String, PathBuf>,
    compress_options: Option<&'a CompressOptions>,
    bleed: bool,
    base_path: &'a str,
    svg_scale: Option<f32>,
    svg_cache: &'a SvgScaleCache,
}

/// Process a single image for individual asset processing (synchronous version for parallel processing)
#[inline]
fn process_single_image_sync(
    img: InputImage,
    source_hash: String,
    ctx: &ProcessImageCtx<'_>,
) -> Result<Pending> {
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
    let bytes = encode_png(&rgba)
        .map_err(|e| anyhow::anyhow!("Failed to encode \"{}\": {}", img.name, e))?;

    let bytes = maybe_compress_png(bytes, ctx.compress_options);
    let hash = hash_bytes(&bytes);
    let kind = AssetKind::Image(ImageFormat::Png);
    let meta = AssetMeta::load_for(&path).unwrap_or_default();
    let display_name = meta.resolve_name(&img.name).to_string();
    let description = meta.resolve_description("Uploaded by Tungsten").to_string();
    let asset_type_override = meta.resolve_asset_type().map(|s| s.to_string());

    Ok(Pending {
        name: img.name,
        path,
        bytes,
        source_hash,
        hash,
        kind,
        display_name,
        description,
        asset_type_override,
    })
}

/// Decode a plain source (SVG rasterized here, raster decoded here) and run
/// the full per-image pipeline (bleed, encode, compress, hash).
///
/// `Ok(None)` = source skipped, mirroring the historical behavior for bad
/// SVGs (warn or silent drop, not counted as an error); `Err` = hard failure
/// counted on the input.
fn process_plain_source(src: PlainSource, ctx: &ProcessImageCtx<'_>) -> Result<Option<Pending>> {
    let path = src.path().to_path_buf();
    // Source hash of the raw file bytes, computed before any decode so the
    // lockfile cache key never depends on encoder output.
    let source_hash = hash_bytes(
        &std::fs::read(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read \"{}\": {}", path.display(), e))?,
    );
    let img = match &src {
        PlainSource::Raster(_) => {
            let image = image::open(&path)
                .map_err(|e| anyhow::anyhow!("Failed to open image \"{}\": {}", path.display(), e))?
                .into_rgba8();
            let name = stem_name(&relative_path(&path, ctx.base_path));
            InputImage {
                name,
                image,
                source_hash: source_hash.clone(),
            }
        }
        PlainSource::Svg(_) => {
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(_) => return Ok(None),
            };
            let scale =
                effective_svg_scale_for_path(&path, ctx.base_path, ctx.svg_scale, ctx.svg_cache);
            let image = match svg_to_rgba(&data, scale) {
                Ok(img) => img,
                Err(e) => {
                    clear_progress_line();
                    log!(warn, "Failed to rasterize \"{}\": {}", path.display(), e);
                    return Ok(None);
                }
            };
            let name = stem_name(&relative_path(&path, ctx.base_path));
            InputImage {
                name,
                image,
                source_hash: source_hash.clone(),
            }
        }
    };
    Ok(Some(process_single_image_sync(img, source_hash, ctx)?))
}

/// Decode a DPI variant (SVG or raster) off-thread. Failures skip the variant
/// (warn only for SVG rasterization, matching the historical behavior).
///
/// `dpi_scale` is the `@Nx` factor: SVGs are resolution-independent, so the
/// rasterization scale is multiplied by it (the `@1x`/`@2x`/`@3x` source files
/// are otherwise identical vectors). Rasters already ship at their own pixel
/// size, so the factor is ignored there.
fn decode_dpi_variant(
    path: &Path,
    base_path: &str,
    svg_scale: Option<f32>,
    dpi_scale: u8,
    cache: &SvgScaleCache,
) -> Option<InputImage> {
    let name = stem_name(&relative_path(path, base_path));
    let image = if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
    {
        let data = std::fs::read(path).ok()?;
        let scale =
            effective_svg_scale_for_path(path, base_path, svg_scale, cache) * dpi_scale as f32;
        match svg_to_rgba(&data, scale) {
            Ok(img) => img,
            Err(e) => {
                clear_progress_line();
                log!(warn, "Failed to rasterize \"{}\": {}", path.display(), e);
                return None;
            }
        }
    } else {
        image::open(path).ok()?.into_rgba8()
    };
    let source_hash = hash_bytes(&std::fs::read(path).ok()?);
    Some(InputImage {
        name,
        image,
        source_hash,
    })
}

/// Dispatch one processed plain image (always a PNG in this mode).
fn dispatch_plain(
    p: Pending,
    asset_type_override: &Option<String>,
    phase: &mut DispatchPhase<'_, '_>,
    codegen_entries: &mut Vec<CodegenEntry>,
) {
    let name = p.name;
    let rel = format!("{}.png", name);
    dispatch_asset(
        PendingAsset {
            name,
            path: p.path,
            bytes: Bytes::from(p.bytes),
            source_hash: p.source_hash,
            hash: p.hash,
            kind: p.kind,
            display_name: p.display_name,
            description: p.description,
            asset_type_override: p
                .asset_type_override
                .clone()
                .or_else(|| asset_type_override.clone())
                .or_else(|| Some(p.kind.api_type().to_string())),
            upload_name: None,
            studio_rel: rel.clone(),
            debug_rel: rel,
            studio_needs_upload: false,
        },
        phase,
        codegen_entries,
    );
}

/// `Ok(Some(p))` processed, `Ok(None)` skipped bad SVG, `Err` counted failure.
type SourceResult = Result<Option<Pending>>;

/// Report an SVG rasterization on the shared "Rasterizing" progress bar.
fn report_rasterized(counter: &AtomicUsize, total: usize, name: &str) {
    let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
    progress("Rasterizing", done, total, name);
}

pub async fn process_individual(
    ctx: &mut SyncContext<'_>,
    svg_paths: Vec<PathBuf>,
    raster_paths: Vec<PathBuf>,
    compress_options: Option<&CompressOptions>,
    bleed: bool,
    svg_scale: Option<f32>,
    web_assets: &HashMap<RelativePathBuf, WebAsset>,
) -> u32 {
    let base_path = ctx.base_path;

    // Seed web assets into codegen entries first
    let mut codegen_entries: Vec<CodegenEntry> = Vec::new();
    seed_web_assets(
        web_assets,
        base_path,
        ctx.codegen.strip_extension,
        ctx.codegen.strip_whitespace,
        &mut codegen_entries,
    );

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

    // Split SVG and raster images into DPI groups and plain images, all by
    // path: DPI grouping is name-based, so no decoding is needed up front.
    let (mut dpi_groups_paths, raster_plain) = group_paths_by_dpi(
        raster_paths
            .into_iter()
            .map(|p| {
                let name = stem_name(&relative_path(&p, base_path));
                (p, name)
            })
            .collect(),
    );
    let (svg_dpi_groups, svg_plain) = group_paths_by_dpi(
        svg_paths
            .into_iter()
            .map(|p| {
                let name = stem_name(&relative_path(&p, base_path));
                (p, name)
            })
            .collect(),
    );
    for (base, variants) in svg_dpi_groups {
        dpi_groups_paths.entry(base).or_default().extend(variants);
    }

    // Shared `.tmeta` read cache so each directory's metadata file is parsed
    // once instead of once per SVG.
    let svg_cache = Arc::new(new_svg_scale_cache());
    let svg_dpi_count: usize = dpi_groups_paths
        .values()
        .flat_map(Vec::as_slice)
        .filter(|(_, p)| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")))
        .count();
    let rasterized = Arc::new(AtomicUsize::new(0));
    let svg_total = svg_plain.len() + svg_dpi_count;

    // Decode the DPI variants (SVGs rasterized, rasters opened) off-thread.
    let dpi_flat: Vec<(String, u8, PathBuf)> = dpi_groups_paths
        .into_iter()
        .flat_map(|(base, variants)| {
            variants
                .into_iter()
                .map(move |(scale, path)| (base.clone(), scale, path))
        })
        .collect();
    let dpi_decoded: Vec<(String, u8, InputImage)> = tokio::task::spawn_blocking({
        let base_path_owned = base_path.to_string();
        let cache = svg_cache.clone();
        let rasterized = rasterized.clone();
        move || {
            dpi_flat
                .into_par_iter()
                .filter_map(|(base, scale, path)| {
                    let is_svg = path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
                    let img =
                        decode_dpi_variant(&path, &base_path_owned, svg_scale, scale, &cache)?;
                    if is_svg {
                        report_rasterized(&rasterized, svg_total, &img.name);
                    }
                    Some((base, scale, img))
                })
                .collect()
        }
    })
    .await
    .expect("spawn_blocking panicked");

    let mut dpi_groups: DpiGroups = HashMap::new();
    for (base, scale, img) in dpi_decoded {
        dpi_groups.entry(base).or_default().push((scale, img));
    }
    for variants in dpi_groups.values_mut() {
        variants.sort_by_key(|(s, _)| *s);
    }

    // Plain sources: SVG and raster paths, decoded lazily during processing.
    let mut plain_sources: Vec<PlainSource> = svg_plain
        .into_iter()
        .map(|(path, _)| PlainSource::Svg(path))
        .chain(
            raster_plain
                .into_iter()
                .map(|(path, _)| PlainSource::Raster(path)),
        )
        .collect();

    if plain_sources.is_empty() && dpi_groups.is_empty() {
        clear_progress_line();
        log!(
            warn,
            "No images could be loaded for input \"{}\" — skipping",
            ctx.input_name
        );
        return ctx.errors;
    }

    let total = plain_sources.len() + dpi_groups.values().map(Vec::len).sum::<usize>();
    let plain_count = plain_sources.len();
    let input_name = ctx.input_name;
    let asset_type_override = ctx.asset_type_override.clone();

    // Owned copies so the `'static` blocking closures don't capture borrowed
    // references into the async context.
    let ctx_compress = compress_options.cloned();
    let base_path_owned = base_path.to_string();
    let path_index_owned = path_index;

    // Unified dispatch: dry-run, Studio/Debug copies, cache hits and cloud
    // uploads are handled identically to `raw` (see `dispatch::dispatch_asset`).
    {
        let mut phase = DispatchPhase::new(ctx, total);

        // Stream the first image: process and dispatch it immediately so the
        // first asset write doesn't wait on the whole parallel batch.
        if !plain_sources.is_empty() {
            let first = plain_sources.remove(0);
            // One-time clones: the streaming task is single-threaded, while the
            // batch task below takes ownership of the originals.
            let path_index_owned = path_index_owned.clone();
            let ctx_compress = ctx_compress.clone();
            let base_path_owned = base_path_owned.clone();
            let svg_cache = svg_cache.clone();
            let rasterized = rasterized.clone();
            let first_result = tokio::task::spawn_blocking(move || {
                let ctx_img = ProcessImageCtx {
                    path_index: &path_index_owned,
                    compress_options: ctx_compress.as_ref(),
                    bleed,
                    base_path: &base_path_owned,
                    svg_scale,
                    svg_cache: &svg_cache,
                };
                let is_svg = matches!(&first, PlainSource::Svg(_));
                let res = process_plain_source(first, &ctx_img);
                if is_svg
                    && svg_total > 0
                    && let Some(p) = res.as_ref().ok().and_then(|o| o.as_ref())
                {
                    report_rasterized(&rasterized, svg_total, &p.name);
                }
                res
            })
            .await
            .expect("spawn_blocking panicked");
            match first_result {
                Ok(Some(p)) => {
                    dispatch_plain(p, &asset_type_override, &mut phase, &mut codegen_entries)
                }
                Ok(None) => {}
                Err(e) => {
                    clear_progress_line();
                    log!(warn, "{}", e);
                    phase.ctx.errors += 1;
                }
            }
        }

        let pending_results: Vec<SourceResult> = tokio::task::spawn_blocking(move || {
            let ctx_img = ProcessImageCtx {
                path_index: &path_index_owned,
                compress_options: ctx_compress.as_ref(),
                bleed,
                base_path: &base_path_owned,
                svg_scale,
                svg_cache: &svg_cache,
            };
            plain_sources
                .into_par_iter()
                .map(|src| {
                    let svg_name = match &src {
                        PlainSource::Svg(p) => Some(stem_name(&relative_path(p, &base_path_owned))),
                        PlainSource::Raster(_) => None,
                    };
                    let res = process_plain_source(src, &ctx_img);
                    if let Some(name) = svg_name
                        && svg_total > 0
                        && matches!(&res, Ok(Some(_)))
                    {
                        report_rasterized(&rasterized, svg_total, &name);
                    }
                    res
                })
                .collect::<Vec<_>>()
        })
        .await
        .expect("spawn_blocking panicked");

        for result in pending_results {
            match result {
                Ok(Some(p)) => {
                    dispatch_plain(p, &asset_type_override, &mut phase, &mut codegen_entries)
                }
                Ok(None) => {}
                Err(e) => {
                    clear_progress_line();
                    log!(warn, "{}", e);
                    phase.ctx.errors += 1;
                }
            }
        }
        crate::utils::trace::mark(&format!(
            "images processed for input \"{}\" ({} plain)",
            input_name, plain_count
        ));
        collect_upload_results(&mut phase, &mut codegen_entries).await;
    }

    // DPI groups: encode/upload/copy + codegen via the shared processor.
    // Produces exactly one `dpi_group` entry per base name.
    ctx.errors += process_dpi_groups(
        ctx,
        dpi_groups,
        bleed,
        compress_options,
        &mut codegen_entries,
    )
    .await;

    write_codegen(ctx, codegen_entries);
    ctx.errors
}
