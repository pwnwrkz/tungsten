pub mod codegen_write;
pub mod dispatch;
pub mod dpi;
pub mod encode;
pub mod error;
pub mod individual;
pub mod packed;
pub mod paths;
pub mod raw;

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};

use crate::api::sync::debug::DebugSync;
use crate::api::sync::roblox::{Creator, GroupCreator, UserCreator};
use crate::api::sync::studio::StudioSync;
use crate::api::upload::RobloxClient;
use crate::core::assets::asset::{self, stem_name};
use crate::core::assets::img::pack;
use crate::core::postsync::lockfile::Lockfile;
use crate::log;
use crate::utils::config::Config;
use crate::utils::env::resolve_api_key;
use crate::utils::logger::{clear_progress_line, progress};

use individual::process_individual;
use packed::process_packed;
use paths::{collect_paths, glob_base, load_input_meta};
use raw::process_raw;

// Target

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Target {
    /// Upload to Roblox Open Cloud API.
    Cloud,
    /// Copy assets into Roblox Studio's content folder for live preview.
    Studio,
    /// Copy assets into `.tungsten-debug/` for local inspection.
    Debug,
}

/// Resolves the asset type override for upload.
/// Returns `None` to infer from file kind, `Some(type)` for explicit override.
fn resolve_asset_type_override(asset_type: Option<&str>) -> Option<&str> {
    match asset_type {
        Some(s) if s.eq_ignore_ascii_case("auto") => None,
        Some(s) => Some(s),
        None => None,
    }
}

// Entry point

pub async fn run(
    config: &Config,
    api_key: Option<&str>,
    target: Target,
    dry_run: bool,
) -> Result<()> {
    let api_key = resolve_api_key(api_key);
    let mut total_errors: u32 = 0;

    log!(debug, "Sync target: {:?}, dry_run={}", target, dry_run);

    let mut lockfile = Lockfile::load().context("Failed to load lockfile")?;

    if dry_run {
        log!(info, "Dry run — no uploads or file copies will occur");
    }

    let client: Option<Arc<RobloxClient>> = if target == Target::Cloud && !dry_run {
        let key = api_key.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "No API key provided\n  \
                 Provide one via --api-key, .env (TUNGSTEN_API_KEY=...), \
                 or the TUNGSTEN_GLOBAL_APIKEY environment variable.\n  \
                 Generate a key at https://create.roblox.com/credentials \
                 with \"Assets: Read & Write\" permissions"
            )
        })?;
        Some(Arc::new(RobloxClient::new(key.to_string())))
    } else {
        None
    };

    let studio_sync: Option<Arc<StudioSync>> = if target == Target::Studio && !dry_run {
        let studio_path = config.studio.as_ref().and_then(|s| s.studio_path.clone());
        let auto_route_version = config
            .studio
            .as_ref()
            .map(|s| s.auto_route_version)
            .unwrap_or(false);
        match StudioSync::new(studio_path, auto_route_version).await {
            Ok(s) => {
                log!(info, "Studio sync folder: {}", s.sync_path().display());
                Some(Arc::new(s))
            }
            Err(e) => {
                log!(error, "Failed to initialise Studio sync: {}", e);
                return Err(e);
            }
        }
    } else {
        None
    };

    let debug_sync: Option<Arc<DebugSync>> = if target == Target::Debug && !dry_run {
        match DebugSync::new() {
            Ok(d) => {
                log!(info, "Debug sync folder: {}", d.sync_path().display());
                Some(Arc::new(d))
            }
            Err(e) => {
                log!(error, "Failed to initialize debug sync: {}", e);
                return Err(e);
            }
        }
    } else {
        None
    };

    let mut studio_expected_files_storage = HashSet::new();
    let mut studio_expected_files = if target == Target::Studio {
        Some(&mut studio_expected_files_storage)
    } else {
        None
    };

    let creator = make_creator(config)?;

    let codegen_cfg = config.codegen.as_ref();
    let codegen_style = codegen_cfg
        .map(|c| c.resolved_style().to_string())
        .unwrap_or_else(|| "flat".to_string());
    let strip_extension = codegen_cfg
        .map(|c| c.resolved_strip_extension())
        .unwrap_or(false);
    let ts_declaration = codegen_cfg
        .map(|c| c.resolved_ts_declaration())
        .unwrap_or(false);

    let max_concurrent_uploads = config.max_concurrent_uploads;

    log!(debug, "max_concurrent_uploads={}", max_concurrent_uploads);

    for (input_name, input) in &config.inputs {
        log!(section, "SYNCING \"{}\"", input_name);
        log!(debug, "Input \"{}\" path: {}", input_name, input.path);

        let paths = match collect_paths(&input.path) {
            Ok(p) if p.is_empty() => {
                log!(
                    warn,
                    "No supported files matched \"{}\" — skipping",
                    input.path
                );
                continue;
            }
            Ok(p) => p,
            Err(e) => {
                log!(warn, "Glob error for \"{}\": {}", input_name, e);
                total_errors += 1;
                continue;
            }
        };

        log!(info, "{} file(s) found", paths.len());

        let base_path = glob_base(&input.path);
        let compress_options = input.resolved_compress_options();
        let compress_opts_ref = compress_options.as_ref();
        let bleed = input.resolved_bleed();

        let (image_paths, other_paths): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            ext.eq_ignore_ascii_case("svg")
                || asset::kind_from_ext(ext)
                    .map(|k| k.is_packable())
                    .unwrap_or(false)
        });

        // Non-image assets (audio, models)
        if !other_paths.is_empty() {
            let asset_type_override = resolve_asset_type_override(input.asset_type.as_deref());
            let errs = process_raw(
                input_name,
                other_paths,
                &base_path,
                &input.output_path,
                &codegen_style,
                strip_extension,
                ts_declaration,
                compress_opts_ref,
                target,
                dry_run,
                &creator,
                asset_type_override,
                &client,
                &studio_sync,
                &debug_sync,
                &mut lockfile,
                &mut studio_expected_files,
                max_concurrent_uploads,
                &input.web,
            )
            .await;
            total_errors += errs;
        }

        // Image assets
        if !image_paths.is_empty() {
            let (svg_paths, raster_paths): (Vec<&PathBuf>, Vec<&PathBuf>) =
                image_paths.iter().partition(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.eq_ignore_ascii_case("svg"))
                        .unwrap_or(false)
                });
            let svg_paths_owned: Vec<PathBuf> = svg_paths.into_iter().cloned().collect();
            let raster_paths_owned: Vec<PathBuf> = raster_paths.into_iter().cloned().collect();

            // Rasterize SVGs in parallel (offloaded to blocking pool).
            let svg_images: Vec<pack::InputImage> = {
                let base = base_path.clone();
                let svg_paths_vec = svg_paths_owned.clone();
                let svg_scale_opt = input.svg_scale;
                let base_path_str = base_path.to_string();
                // Shared `.tmeta` read cache so each directory's metadata file
                // is parsed once instead of once per SVG.
                let svg_scale_cache =
                    std::sync::Arc::new(crate::utils::config::new_svg_scale_cache());

                tokio::task::spawn_blocking(move || {
                    use rayon::prelude::*;
                    let svg_total = svg_paths_vec.len();
                    let counter = std::sync::atomic::AtomicUsize::new(0);
                    svg_paths_vec
                        .par_iter()
                        .filter_map(|path| {
                            let data = std::fs::read(path).ok()?;
                            let rel = path.strip_prefix(&base).unwrap_or(path).to_string_lossy();
                            let name = stem_name(&rel);
                            // Compute effective SVG scale (reads .tmeta files)
                            let scale =
                                crate::utils::config::InputConfig::effective_svg_scale_for_path(
                                    path,
                                    &base_path_str,
                                    svg_scale_opt,
                                    &svg_scale_cache,
                                );
                            // Rasterize straight to RGBA, skipping a PNG roundtrip.
                            let image =
                                crate::core::assets::img::convert::svg_to_rgba(&data, scale)
                                    .map_err(|e| {
                                        clear_progress_line();
                                        log!(
                                            warn,
                                            "Failed to rasterize \"{}\": {}",
                                            path.display(),
                                            e
                                        );
                                        e
                                    })
                                    .ok()?;
                            let done =
                                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                            progress("Rasterizing", done, svg_total, &name);
                            Some(pack::InputImage {
                                name: name.to_string(),
                                image,
                            })
                        })
                        .collect()
                })
                .await
                .expect("spawn_blocking panicked")
            };

            let errs = if input.packable.unwrap_or(false) {
                // Packed: every image must be decoded up front for bin-packing
                // and atlas compositing (decode concurrency is chunk-bounded in
                // `pack::load_images`).
                let base_path_owned = base_path.to_string();
                let mut images = match tokio::task::spawn_blocking(move || {
                    pack::load_images(raster_paths_owned, &base_path_owned)
                })
                .await
                .expect("spawn_blocking panicked")
                {
                    Ok(imgs) => imgs,
                    Err(e) => {
                        log!(warn, "Failed to load images for \"{}\": {}", input_name, e);
                        total_errors += 1;
                        continue;
                    }
                };
                images.extend(svg_images);

                if images.is_empty() {
                    log!(
                        warn,
                        "No images could be loaded for \"{}\" — skipping",
                        input_name
                    );
                    continue;
                }

                let sheet_meta = load_input_meta(&base_path);
                let asset_type_override = resolve_asset_type_override(input.asset_type.as_deref());
                process_packed(
                    input_name,
                    &sheet_meta,
                    images,
                    &input.output_path,
                    &codegen_style,
                    strip_extension,
                    ts_declaration,
                    compress_opts_ref,
                    bleed,
                    target,
                    dry_run,
                    &creator,
                    asset_type_override,
                    &client,
                    &studio_sync,
                    &debug_sync,
                    &mut lockfile,
                    &mut studio_expected_files,
                    max_concurrent_uploads,
                    &input.web,
                    &base_path,
                )
                .await
            } else {
                // Individual: raster files are decoded lazily inside the
                // processing loop so memory stays bounded by processing
                // concurrency instead of holding every decoded image at once.
                if svg_images.is_empty() && raster_paths_owned.is_empty() {
                    log!(
                        warn,
                        "No images could be loaded for \"{}\" — skipping",
                        input_name
                    );
                    continue;
                }
                let asset_type_override = resolve_asset_type_override(input.asset_type.as_deref());
                process_individual(
                    input_name,
                    svg_images,
                    svg_paths_owned,
                    raster_paths_owned,
                    &base_path,
                    &input.output_path,
                    &codegen_style,
                    strip_extension,
                    ts_declaration,
                    compress_opts_ref,
                    bleed,
                    target,
                    dry_run,
                    &creator,
                    asset_type_override,
                    &client,
                    &studio_sync,
                    &debug_sync,
                    &mut lockfile,
                    &mut studio_expected_files,
                    max_concurrent_uploads,
                    &input.web,
                )
                .await
            };
            total_errors += errs;
        }
    }

    if let Err(e) = lockfile.save() {
        log!(warn, "Failed to save lockfile: {}", e);
        total_errors += 1;
    }

    // Cleanup stale files in Studio sync folder (only for Studio target)
    if target == Target::Studio
        && let Some(studio_sync) = &studio_sync
    {
        let expected_files = &studio_expected_files_storage;
        let sync_path = studio_sync.sync_path();

        // Safely collect actual files (only files, not directories)
        let actual_files: HashSet<String> = match fs::read_dir(sync_path) {
            Ok(read_dir) => read_dir
                .filter_map(|entry| {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(e) => {
                            log!(warn, "Failed to read directory entry: {}", e);
                            return None;
                        }
                    };
                    let path = entry.path();
                    if path.is_file() {
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect(),
            Err(e) => {
                log!(
                    warn,
                    "Failed to read sync directory {}: {}",
                    sync_path.display(),
                    e
                );
                HashSet::new()
            }
        };

        // Remove files that are not in our expected set
        for file in actual_files.difference(expected_files) {
            let file_path = sync_path.join(file);
            if let Err(e) = fs::remove_file(&file_path) {
                log!(
                    warn,
                    "Failed to remove stale Studio sync file '{}': {}",
                    file,
                    e
                );
            }
        }
    }

    log!(section, "SUMMARY");
    if total_errors > 0 {
        log!(
            warn,
            "{} error(s) — some assets may not have been processed",
            total_errors
        );
    } else {
        log!(success, "All assets synced successfully");
    }

    Ok(())
}

// Creator helper

pub fn make_creator(config: &Config) -> Result<Creator> {
    match config.creator.creator_type.as_str() {
        "user" => Ok(Creator::User(UserCreator {
            user_id: config.creator.id.to_string(),
        })),
        "group" => Ok(Creator::Group(GroupCreator {
            group_id: config.creator.id.to_string(),
        })),
        other => bail!(
            "Invalid creator type \"{}\"\n  Hint: Must be \"user\" or \"group\"",
            other
        ),
    }
}
