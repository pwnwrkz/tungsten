use anyhow::{Context, Result, bail};
use glob::glob;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::task::JoinSet;

use crate::api::roblox::{Creator, GroupCreator, UserCreator};
use crate::api::upload::{RobloxClient, UploadParams};
use crate::core::asset::{self, AssetKind, AssetMeta, ImageFormat};
use crate::core::codegen::{self, CodegenEntry};
use crate::core::convert::{self, ConvertRules};
use crate::core::lockfile::{Lockfile, hash_image};
use crate::core::pack;
use crate::log;
use crate::utils::config::{Config, InputConfig};
use crate::utils::env::resolve_api_key;
use crate::utils::logger::progress;

// Entry point
pub async fn run(config: Config, api_key: Option<String>, target: &str) -> Result<()> {
    let api_key = resolve_api_key(api_key);
    let mut total_errors: u32 = 0;

    let mut lockfile = Lockfile::load().context("Failed to load lockfile")?;

    let client: Option<Arc<RobloxClient>> = if target == "roblox" {
        let key = api_key.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "No API key provided\n  \
                 You can provide an API key for Tungsten in multiple ways:\n  \
                 1. Pass it as a flag: tungsten sync --target roblox --api-key YOUR_KEY\n  \
                 2. Store it in a file named tungsten_api_key.env with the line API_KEY=...\n  \
                 3. Set it as a system environment variable: TUNGSTEN_GLOBAL_APIKEY\n  \
                 If you don't have one, generate a key at https://create.roblox.com/credentials \
                 with \"Assets: Read & Write\" permissions"
            )
        })?;
        Some(Arc::new(RobloxClient::new(key.to_string())))
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

    for (input_name, input) in &config.inputs {
        log!(section, "Processing \"{}\"", input_name);

        let convert_rules = match validate_convert_rules(input) {
            Ok(r) => r,
            Err(e) => {
                log!(
                    warn,
                    "Invalid conversion rules for \"{}\": {}",
                    input_name,
                    e
                );
                total_errors += 1;
                continue;
            }
        };

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

        log!(info, "Found {} file(s)", paths.len());

        let base_path = glob_base(&input.path);
        let svg_scale = input.resolved_svg_scale();

        // Split: images (including SVGs that will rasterize to an image) vs everything else.
        let (image_paths, other_paths): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| {
            let src_ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            // SVGs always produce an image regardless of convert rules.
            if src_ext.eq_ignore_ascii_case("svg") {
                return true;
            }
            let rel = relative_path(p, &base_path);
            let target_ext = convert_rules.resolve(&rel, src_ext).unwrap_or(src_ext);
            asset::kind_from_ext(target_ext)
                .map(|k| k.is_packable())
                .unwrap_or(false)
        });

        // Non-image assets (audio, models, animations)
        if !other_paths.is_empty() {
            let errs = process_raw(
                input_name,
                other_paths,
                &convert_rules,
                &base_path,
                svg_scale,
                &input.output_path,
                &codegen_style,
                strip_extension,
                &creator,
                &client,
                &mut lockfile,
            )
            .await;
            total_errors += errs;
        }

        // Image assets (raster + SVG)
        if !image_paths.is_empty() {
            log!(info, "Loading images...");

            // SVGs must be rasterized before the image crate can decode them.
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
                svg_paths
                    .par_iter()
                    .filter_map(|path| {
                        let data = match std::fs::read(path) {
                            Ok(d) => d,
                            Err(e) => {
                                log!(warn, "Failed to read SVG \"{}\": {}", path.display(), e);
                                return None;
                            }
                        };
                        let png_bytes = match convert::svg_to_png(&data, svg_scale) {
                            Ok(b) => b,
                            Err(e) => {
                                log!(warn, "Failed to rasterize \"{}\": {}", path.display(), e);
                                return None;
                            }
                        };
                        let image = match image::load_from_memory(&png_bytes) {
                            Ok(img) => img.into_rgba8(),
                            Err(e) => {
                                log!(
                                    warn,
                                    "Failed to decode rasterized SVG \"{}\": {}",
                                    path.display(),
                                    e
                                );
                                return None;
                            }
                        };
                        let name = path
                            .strip_prefix(&base)
                            .unwrap_or(path)
                            .with_extension("")
                            .to_string_lossy()
                            .replace('\\', "/");
                        Some(pack::InputImage { name, image })
                    })
                    .collect()
            };

            // Load raster images.
            let raster: Vec<PathBuf> = raster_paths.into_iter().cloned().collect();
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
                    &creator,
                    &client,
                    &mut lockfile,
                )
                .await
            } else {
                process_individual(
                    input_name,
                    images,
                    image_paths,
                    &convert_rules,
                    svg_scale,
                    &base_path,
                    &input.output_path,
                    &codegen_style,
                    strip_extension,
                    &creator,
                    &client,
                    &mut lockfile,
                )
                .await
            };
            total_errors += errs;
        }
    }

    // Flush lockfile once at the end — far cheaper than writing after every upload.
    if let Err(e) = lockfile.save() {
        log!(warn, "Failed to save lockfile: {}", e);
        total_errors += 1;
    }

    log!(section, "Done");

    if total_errors > 0 {
        log!(
            warn,
            "Sync completed with {} error(s) — some assets may not have been uploaded",
            total_errors
        );
    } else {
        log!(success, "Tungsten sync complete!");
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

// Path collection
/// Shared glob resolution used by both sync and test commands.
pub fn collect_paths(pattern: &str) -> Result<Vec<PathBuf>> {
    let paths = glob(pattern)
        .with_context(|| {
            format!(
                "Invalid glob pattern \"{}\"\n  Hint: Example: path = \"assets/**/*.png\"",
                pattern
            )
        })?
        .filter_map(|entry| match entry {
            Ok(p) => {
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                if asset::is_supported_ext(ext) {
                    Some(p)
                } else {
                    None
                }
            }
            Err(e) => {
                eprintln!("Warning: skipping unreadable path: {}", e);
                None
            }
        })
        .collect();
    Ok(paths)
}

/// Extract the non-glob prefix of a pattern to use as the name base.
pub fn glob_base(pattern: &str) -> String {
    pattern
        .split('*')
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string()
}

/// Get the path of a file relative to the glob base, normalised to forward slashes.
pub fn relative_path(path: &Path, base: &str) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// .tmeta helpers

/// Load the .tmeta sidecar for a packable input folder.
/// Silently returns a default (all-None) meta if the file doesn't exist.
fn load_input_meta(base_path: &str) -> AssetMeta {
    let tmeta_path = Path::new(base_path).with_extension("tmeta");
    AssetMeta::load_for(&tmeta_path).unwrap_or_default()
}

// Conversion
fn validate_convert_rules(input: &InputConfig) -> Result<ConvertRules> {
    let Some(raw) = &input.convert else {
        return Ok(ConvertRules::default());
    };
    let rules = ConvertRules::parse_all(raw)?;

    for rule in &rules.rules {
        let to = match rule {
            convert::ConvertRule::ExtWide { to, .. } => to.as_str(),
            convert::ConvertRule::FileSpecific { to, .. } => Path::new(to)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or(to.as_str()),
        };
        if matches!(to, "mp3" | "ogg" | "flac" | "wav") {
            let from = match rule {
                convert::ConvertRule::ExtWide { from, .. } => from.as_str(),
                convert::ConvertRule::FileSpecific { from, .. } => from.as_str(),
            };
            bail!("{}", convert::unsupported_audio_message(from, to));
        }
    }

    Ok(rules)
}

/// Apply conversion to raw bytes (for non-image raw assets).
/// For SVGs this rasterizes to PNG (or the resolved target format) via resvg.
fn apply_conversion(
    data: Vec<u8>,
    path: &Path,
    base_path: &str,
    rules: &ConvertRules,
    svg_scale: f32,
) -> Result<(Vec<u8>, AssetKind)> {
    let rel = relative_path(path, base_path);
    let src_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let resolved = rules.resolve(&rel, &src_ext);

    // Rasterize, then optionally re-encode to the target format.
    if src_ext == "svg" {
        let target = resolved.unwrap_or("png");
        let target_bare: String = if target.contains('.') {
            Path::new(target)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or(target)
                .to_string()
        } else {
            target.to_string()
        };
        let fmt = convert::image_format_from_str(&target_bare).unwrap_or(ImageFormat::Png);
        let png = convert::svg_to_png(&data, svg_scale).context("Failed to rasterize SVG")?;
        let bytes = if fmt == ImageFormat::Png {
            png
        } else {
            let rgba = image::load_from_memory(&png)
                .context("Failed to decode rasterized SVG")?
                .into_rgba8();
            convert::convert_image(&rgba, fmt)
                .with_context(|| format!("Conversion to {} failed", target_bare))?
        };
        return Ok((bytes, AssetKind::Image(fmt)));
    }

    let Some(target_ext) = resolved else {
        let kind = asset::kind_from_ext(&src_ext)
            .with_context(|| format!("Unsupported extension \"{}\"", src_ext))?;
        return Ok((data, kind));
    };

    let target_bare: String = if target_ext.contains('.') {
        Path::new(target_ext)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or(target_ext)
            .to_string()
    } else {
        target_ext.to_string()
    };

    let target_kind = asset::kind_from_ext(&target_bare)
        .with_context(|| format!("Unknown target extension \"{}\"", target_bare))?;

    let converted = match target_kind {
        AssetKind::Image(fmt) => convert::transcode_image(&data, fmt)
            .with_context(|| format!("Conversion to {} failed", target_bare))?,
        _ => bail!(
            "{}",
            convert::unsupported_audio_message(&src_ext, &target_bare)
        ),
    };

    Ok((converted, target_kind))
}

/// Resolve the target image format for an already-loaded `RgbaImage` and encode it.
/// Used in `process_individual` for raster images.
fn encode_with_conversion(
    img: &image::RgbaImage,
    path: &Path,
    base_path: &str,
    rules: &ConvertRules,
) -> Result<(Vec<u8>, ImageFormat)> {
    let rel = relative_path(path, base_path);
    let src_ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png");
    let target_raw = rules.resolve(&rel, src_ext).unwrap_or("png");

    let target_bare: String = if target_raw.contains('.') {
        Path::new(target_raw)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or(target_raw)
            .to_string()
    } else {
        target_raw.to_string()
    };

    let fmt = convert::image_format_from_str(&target_bare).unwrap_or(ImageFormat::Png);
    let bytes = convert::convert_image(img, fmt)?;
    Ok((bytes, fmt))
}

// Raw (non-image) asset processing

struct RawPending {
    name: String,
    path: PathBuf,
    bytes: Vec<u8>,
    hash: String,
    kind: AssetKind,
    display_name: String,
    description: String,
}

#[allow(clippy::too_many_arguments)]
async fn process_raw(
    input_name: &str,
    paths: Vec<PathBuf>,
    convert_rules: &ConvertRules,
    base_path: &str,
    svg_scale: f32,
    output_path: &str,
    codegen_style: &str,
    strip_extension: bool,
    creator: &Creator,
    client: &Option<Arc<RobloxClient>>,
    lockfile: &mut Lockfile,
) -> u32 {
    let mut errors: u32 = 0;
    let mut pending: Vec<RawPending> = Vec::with_capacity(paths.len());

    for path in &paths {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                log!(warn, "Failed to read \"{}\": {}", path.display(), e);
                errors += 1;
                continue;
            }
        };

        let (data, kind) = match apply_conversion(data, path, base_path, convert_rules, svg_scale) {
            Ok(r) => r,
            Err(e) => {
                log!(warn, "{}", e);
                errors += 1;
                continue;
            }
        };

        let hash = hash_image(&data);

        let meta = match AssetMeta::load_for(path) {
            Ok(m) => m,
            Err(e) => {
                log!(
                    warn,
                    "Failed to load .tmeta for \"{}\": {}",
                    path.display(),
                    e
                );
                AssetMeta::default()
            }
        };

        let name = {
            let rel = relative_path(path, base_path);
            Path::new(&rel)
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/")
        };
        let display_name = meta.resolve_name(&name).to_string();
        let description = meta.resolve_description("Uploaded by Tungsten").to_string();

        pending.push(RawPending {
            name,
            path: path.clone(),
            bytes: data,
            hash,
            kind,
            display_name,
            description,
        });
    }

    let total = pending.len();
    let mut codegen_entries: Vec<CodegenEntry> = Vec::with_capacity(total);
    let mut upload_tasks: JoinSet<Result<(String, u32, u32, u64, String)>> = JoinSet::new();

    for p in pending {
        if let Some(cached_id) = lockfile.get(input_name, &p.hash) {
            log!(
                info,
                "\"{}\" unchanged, skipping (rbxassetid://{})",
                p.name,
                cached_id
            );
            codegen_entries.push(CodegenEntry::asset(p.name, cached_id));
            continue;
        }

        let Some(c) = client else {
            codegen_entries.push(CodegenEntry::asset(p.name, 0));
            continue;
        };

        let c_arc = Arc::clone(c);
        let creator_own = creator.clone();
        let file_name = p
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        upload_tasks.spawn(async move {
            let id = c_arc
                .upload(UploadParams {
                    file_name,
                    display_name: p.display_name,
                    description: p.description,
                    data: p.bytes,
                    kind: p.kind,
                    creator: creator_own,
                })
                .await
                .with_context(|| format!("Failed to upload \"{}\"", p.name))?;
            Ok((p.name, 0u32, 0u32, id, p.hash))
        });
    }

    let upload_total = upload_tasks.len();
    let mut completed = 0usize;

    while let Some(res) = upload_tasks.join_next().await {
        completed += 1;
        match res {
            #[allow(unused_variables)]
            Ok(Ok((name, w, h, id, hash))) => {
                lockfile.set(input_name, hash, id);
                progress(completed, upload_total, &name);
                codegen_entries.push(CodegenEntry::asset(name, id));
            }
            Ok(Err(e)) => {
                log!(warn, "{}", e);
                errors += 1;
            }
            Err(e) => {
                log!(warn, "Upload task panicked: {}", e);
                errors += 1;
            }
        }
    }

    if upload_total > 0 {
        progress(total, total, "done");
        println!();
    }

    write_codegen(
        codegen_entries,
        input_name,
        output_path,
        codegen_style,
        strip_extension,
        &mut errors,
    );
    errors
}

// Packed image path
#[allow(clippy::too_many_arguments)]
async fn process_packed(
    input_name: &str,
    sheet_meta: &AssetMeta,
    images: Vec<pack::InputImage>,
    output_path: &str,
    codegen_style: &str,
    strip_extension: bool,
    creator: &Creator,
    client: &Option<Arc<RobloxClient>>,
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

    log!(info, "Packing into spritesheets...");
    let spritesheets = match pack::pack(images) {
        Ok(s) => s,
        Err(e) => {
            log!(warn, "Failed to pack images for \"{}\": {}", input_name, e);
            return 1;
        }
    };

    log!(success, "Packed into {} spritesheet(s)", spritesheets.len());

    let mut codegen_entries: Vec<CodegenEntry> = Vec::new();

    for (idx, sheet) in spritesheets.iter().enumerate() {
        let png_bytes = match encode_png(&sheet.image) {
            Ok(b) => b,
            Err(e) => {
                log!(warn, "Failed to encode spritesheet #{}: {}", idx + 1, e);
                errors += 1;
                continue;
            }
        };

        let hash = hash_image(&png_bytes);

        // Zero-padded index so names sort correctly: _001, _002, ...
        let sheet_display_name = format!("{}_{:03}", sheet_base, idx + 1);

        let asset_id = match client {
            Some(c) => {
                if let Some(cached) = lockfile.get(input_name, &hash) {
                    log!(
                        info,
                        "Spritesheet #{} unchanged, skipping (rbxassetid://{})",
                        idx + 1,
                        cached
                    );
                    cached
                } else {
                    log!(info, "Uploading \"{}\"...", sheet_display_name);
                    match c
                        .upload(UploadParams {
                            file_name: format!("{}.png", sheet_display_name),
                            display_name: sheet_display_name.clone(),
                            description: sheet_description.clone(),
                            data: png_bytes,
                            kind: AssetKind::Image(ImageFormat::Png),
                            creator: creator.clone(),
                        })
                        .await
                    {
                        Ok(id) => {
                            lockfile.set(input_name, hash, id);
                            log!(
                                success,
                                "\"{}\" uploaded -> rbxassetid://{}",
                                sheet_display_name,
                                id
                            );
                            id
                        }
                        Err(e) => {
                            log!(warn, "Failed to upload \"{}\": {}", sheet_display_name, e);
                            errors += 1;
                            continue;
                        }
                    }
                }
            }
            None => {
                log!(
                    info,
                    "Dry run: skipping upload for \"{}\"",
                    sheet_display_name
                );
                0
            }
        };

        for img in &sheet.images {
            codegen_entries.push(CodegenEntry::sprite(
                img.name.clone(),
                asset_id,
                (img.x, img.y),
                (img.width, img.height),
            ));
        }
    }

    write_codegen(
        codegen_entries,
        input_name,
        output_path,
        codegen_style,
        strip_extension,
        &mut errors,
    );
    errors
}

// Individual image path
#[allow(clippy::too_many_arguments)]
async fn process_individual(
    input_name: &str,
    images: Vec<pack::InputImage>,
    paths: Vec<PathBuf>,
    convert_rules: &ConvertRules,
    svg_scale: f32,
    base_path: &str,
    output_path: &str,
    codegen_style: &str,
    strip_extension: bool,
    creator: &Creator,
    client: &Option<Arc<RobloxClient>>,
    lockfile: &mut Lockfile,
) -> u32 {
    let mut errors: u32 = 0;
    let total = images.len();

    struct Pending {
        name: String,
        path: PathBuf,
        width: u32,
        height: u32,
        bytes: Vec<u8>,
        hash: String,
        kind: AssetKind,
        display_name: String,
        description: String,
    }

    let mut pending: Vec<Pending> = Vec::with_capacity(total);

    for (img, path) in images.into_iter().zip(paths.iter()) {
        let src_ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png");
        let is_svg = src_ext.eq_ignore_ascii_case("svg");

        // SVGs are already rasterized into `img.image` by the time we get here.
        // We only need to apply any requested format conversion on top (e.g. svg -> jpg).
        let (bytes, fmt) = if is_svg {
            let rel = relative_path(path, base_path);
            let target_raw = convert_rules.resolve(&rel, src_ext).unwrap_or("png");
            let target_bare: String = if target_raw.contains('.') {
                Path::new(target_raw)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or(target_raw)
                    .to_string()
            } else {
                target_raw.to_string()
            };
            let fmt = convert::image_format_from_str(&target_bare).unwrap_or(ImageFormat::Png);
            match convert::convert_image(&img.image, fmt) {
                Ok(b) => (b, fmt),
                Err(e) => {
                    log!(warn, "Failed to encode SVG \"{}\": {}", img.name, e);
                    errors += 1;
                    continue;
                }
            }
        } else {
            match encode_with_conversion(&img.image, path, base_path, convert_rules) {
                Ok(r) => r,
                Err(e) => {
                    log!(warn, "Failed to encode \"{}\": {}", img.name, e);
                    errors += 1;
                    continue;
                }
            }
        };

        let _ = svg_scale; // already consumed during rasterization in the load phase
        let hash = hash_image(&bytes);
        let kind = AssetKind::Image(fmt);

        let meta = match AssetMeta::load_for(path) {
            Ok(m) => m,
            Err(e) => {
                log!(
                    warn,
                    "Failed to load .tmeta for \"{}\": {}",
                    path.display(),
                    e
                );
                AssetMeta::default()
            }
        };

        let display_name = meta.resolve_name(&img.name).to_string();
        let description = meta.resolve_description("Uploaded by Tungsten").to_string();

        pending.push(Pending {
            name: img.name,
            path: path.clone(),
            width: img.image.width(),
            height: img.image.height(),
            bytes,
            hash,
            kind,
            display_name,
            description,
        });
    }

    let mut codegen_entries: Vec<CodegenEntry> = Vec::with_capacity(pending.len());
    let mut upload_tasks: JoinSet<Result<(String, u32, u32, u64, String)>> = JoinSet::new();

    for p in pending {
        if let Some(cached_id) = lockfile.get(input_name, &p.hash) {
            codegen_entries.push(CodegenEntry::asset(p.name, cached_id));
            continue;
        }

        let Some(c) = client else {
            codegen_entries.push(CodegenEntry::asset(p.name, 0));
            continue;
        };

        let c_arc = Arc::clone(c);
        let creator_own = creator.clone();
        let file_name = p
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        upload_tasks.spawn(async move {
            let id = c_arc
                .upload(UploadParams {
                    file_name,
                    display_name: p.display_name,
                    description: p.description,
                    data: p.bytes,
                    kind: p.kind,
                    creator: creator_own,
                })
                .await
                .with_context(|| format!("Failed to upload \"{}\"", p.name))?;
            Ok((p.name, p.width, p.height, id, p.hash))
        });
    }

    let upload_total = upload_tasks.len();
    let mut completed = 0usize;

    while let Some(res) = upload_tasks.join_next().await {
        completed += 1;
        match res {
            #[allow(unused_variables)]
            Ok(Ok((name, width, height, id, hash))) => {
                lockfile.set(input_name, hash, id);
                progress(completed, upload_total, &name);
                codegen_entries.push(CodegenEntry::asset(name, id));
            }
            Ok(Err(e)) => {
                log!(warn, "{}", e);
                errors += 1;
            }
            Err(e) => {
                log!(warn, "Upload task panicked: {}", e);
                errors += 1;
            }
        }
    }

    if upload_total > 0 {
        progress(total, total, "done");
        println!();
    }

    write_codegen(
        codegen_entries,
        input_name,
        output_path,
        codegen_style,
        strip_extension,
        &mut errors,
    );
    errors
}

// Shared helpers
pub fn encode_png(image: &image::RgbaImage) -> Result<Vec<u8>> {
    use image::ImageEncoder;
    let capacity = (image.width() * image.height() * 4) as usize;
    let mut bytes: Vec<u8> = Vec::with_capacity(capacity);
    image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut bytes))
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .context("Failed to encode PNG")?;
    Ok(bytes)
}

fn write_codegen(
    entries: Vec<CodegenEntry>,
    input_name: &str,
    output_path: &str,
    style: &str,
    strip_extension: bool,
    errors: &mut u32,
) {
    let table_name = match Path::new(output_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
    {
        Some(n) => n,
        None => {
            log!(warn, "Invalid output path \"{}\"", output_path);
            *errors += 1;
            return;
        }
    };

    log!(info, "Writing codegen to \"{}\"...", output_path);
    match codegen::generate(entries, &table_name, style, strip_extension, output_path) {
        Ok(()) => log!(success, "Codegen written to \"{}\"", output_path),
        Err(e) => {
            log!(
                warn,
                "Failed to write codegen for \"{}\": {}",
                input_name,
                e
            );
            *errors += 1;
        }
    }
}
