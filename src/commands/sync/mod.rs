pub mod codegen_write;
pub mod encode;
pub mod individual;
pub mod packed;
pub mod paths;
pub mod raw;

use std::sync::Arc;

use anyhow::{Context, Result, bail};

use crate::api::sync::debug::DebugSync;
use crate::api::sync::roblox::{Creator, GroupCreator, UserCreator};
use crate::api::sync::studio::StudioSync;
use crate::api::upload::RobloxClient;
use crate::core::assets::asset;
use crate::core::assets::img::{convert, pack};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Upload to Roblox Open Cloud API.
    Cloud,
    /// Copy assets into Roblox Studio's content folder for live preview.
    Studio,
    /// Copy assets into `.tungsten-debug/` for local inspection.
    Debug,
}

impl Target {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "cloud" => Ok(Target::Cloud),
            "studio" => Ok(Target::Studio),
            "debug" => Ok(Target::Debug),
            other => bail!(
                "Unknown target \"{}\"\n  Hint: Valid targets are cloud, studio, debug",
                other
            ),
        }
    }
}

// Entry point

pub async fn run(
    config: Config,
    api_key: Option<String>,
    target: Target,
    dry_run: bool,
) -> Result<()> {
    let api_key = resolve_api_key(api_key);
    let mut total_errors: u32 = 0;

    let mut lockfile = Lockfile::load().context("Failed to load lockfile")?;

    if dry_run {
        log!(info, "Dry run — no uploads or file copies will occur");
    }

    let client: Option<Arc<RobloxClient>> = if target == Target::Cloud && !dry_run {
        let key = api_key.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "No API key provided\n  \
                 Provide one via --api-key, tungsten_api_key.env (API_KEY=...), \
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
        match StudioSync::new() {
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
                log!(error, "Failed to initialise debug sync: {}", e);
                return Err(e);
            }
        }
    } else {
        None
    };

    let creator = make_creator(&config)?;

    let codegen_style = config
        .codegen
        .as_ref()
        .and_then(|c| c.style.as_deref())
        .unwrap_or("flat")
        .to_string();

    let strip_extension = config
        .codegen
        .as_ref()
        .and_then(|c| c.strip_extension)
        .unwrap_or(false);

    let ts_declaration = config
        .codegen
        .as_ref()
        .and_then(|c| c.ts_declaration)
        .unwrap_or(false);

    for (input_name, input) in &config.inputs {
        log!(section, "Syncing \"{}\"", input_name);

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
        let svg_scale = input.resolved_svg_scale();
        let compress_options = input.resolved_compress_options();
        let compress_opts_ref = compress_options.as_ref();

        let (image_paths, other_paths): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            ext.eq_ignore_ascii_case("svg")
                || asset::kind_from_ext(ext)
                    .map(|k| k.is_packable())
                    .unwrap_or(false)
        });

        // Non-image assets (audio, models)
        if !other_paths.is_empty() {
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
                &client,
                &studio_sync,
                &debug_sync,
                &mut lockfile,
            )
            .await;
            total_errors += errs;
        }

        // Image assets
        if !image_paths.is_empty() {
            let (svg_paths, raster_paths): (Vec<_>, Vec<_>) = image_paths.iter().partition(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("svg"))
                    .unwrap_or(false)
            });

            // Rasterize SVGs in parallel.
            let svg_images: Vec<pack::InputImage> = {
                use rayon::prelude::*;
                let base = base_path.clone();
                let svg_total = svg_paths.len();
                let counter = std::sync::atomic::AtomicUsize::new(0);
                svg_paths
                    .par_iter()
                    .filter_map(|path| {
                        let data = std::fs::read(path).ok()?;
                        let name = path
                            .strip_prefix(&base)
                            .unwrap_or(path)
                            .with_extension("")
                            .to_string_lossy()
                            .replace('\\', "/");
                        let png_bytes = convert::svg_to_png(&data, svg_scale)
                            .map_err(|e| {
                                clear_progress_line();
                                log!(warn, "Failed to rasterize \"{}\": {}", path.display(), e);
                                e
                            })
                            .ok()?;
                        let image = image::load_from_memory(&png_bytes)
                            .map_err(|e| {
                                clear_progress_line();
                                log!(
                                    warn,
                                    "Failed to decode rasterized SVG \"{}\": {}",
                                    path.display(),
                                    e
                                );
                                e
                            })
                            .ok()?
                            .into_rgba8();
                        let done = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        progress("Rasterizing", done, svg_total, &name);
                        Some(pack::InputImage {
                            name: name.to_string(),
                            image,
                        })
                    })
                    .collect()
            };

            let raster: Vec<_> = raster_paths.into_iter().cloned().collect();
            let mut images = match pack::load_images(raster, &base_path) {
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

            let errs = if input.packable.unwrap_or(false) {
                let sheet_meta = load_input_meta(&base_path);
                process_packed(
                    input_name,
                    &sheet_meta,
                    images,
                    &input.output_path,
                    &codegen_style,
                    strip_extension,
                    ts_declaration,
                    compress_opts_ref,
                    target,
                    dry_run,
                    &creator,
                    &client,
                    &studio_sync,
                    &debug_sync,
                    &mut lockfile,
                )
                .await
            } else {
                process_individual(
                    input_name,
                    images,
                    image_paths,
                    svg_scale,
                    &base_path,
                    &input.output_path,
                    &codegen_style,
                    strip_extension,
                    ts_declaration,
                    compress_opts_ref,
                    target,
                    dry_run,
                    &creator,
                    &client,
                    &studio_sync,
                    &debug_sync,
                    &mut lockfile,
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

    log!(section, "Done");
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
