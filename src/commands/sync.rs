use anyhow::{Context, Result, bail};
use glob::glob;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::task::JoinSet;

use crate::api::debug::DebugSync;
use crate::api::roblox::{Creator, GroupCreator, UserCreator};
use crate::api::studio::StudioSync;
use crate::api::upload::{RobloxClient, UploadParams};
use crate::core::alpha_bleed::alpha_bleed;
use crate::core::asset::{self, AssetKind, AssetMeta, ImageFormat};
use crate::core::codegen::{self, CodegenEntry, parse_dpi_suffix, strip_dpi_suffix};
use crate::core::convert::{self, ConvertRules};
use crate::core::lockfile::{Lockfile, hash_image};
use crate::core::pack;
use crate::log;
use crate::utils::config::{Config, InputConfig};
use crate::utils::env::resolve_api_key;
use crate::utils::logger::progress;

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

    // Set up upload client / studio / debug handles.
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

        // Partition: images (including SVG) vs non-image assets.
        let (image_paths, other_paths): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| {
            let src_ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if src_ext.eq_ignore_ascii_case("svg") {
                return true;
            }
            let rel = relative_path(p, &base_path);
            let target_ext = convert_rules.resolve(&rel, src_ext).unwrap_or(src_ext);
            asset::kind_from_ext(target_ext)
                .map(|k| k.is_packable())
                .unwrap_or(false)
        });

        // Non-image assets (audio, models, animations).
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
                ts_declaration,
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

        // Image assets (raster + SVG).
        if !image_paths.is_empty() {
            log!(info, "Loading images...");

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
                        let data = std::fs::read(path).ok()?;
                        let png_bytes = convert::svg_to_png(&data, svg_scale)
                            .map_err(|e| {
                                log!(warn, "Failed to rasterize \"{}\": {}", path.display(), e);
                                e
                            })
                            .ok()?;
                        let image = image::load_from_memory(&png_bytes)
                            .map_err(|e| {
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
                    ts_declaration,
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
                    &convert_rules,
                    svg_scale,
                    &base_path,
                    &input.output_path,
                    &codegen_style,
                    strip_extension,
                    ts_declaration,
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
            "Sync completed with {} error(s) — some assets may not have been processed",
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

// Path helpers

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

pub fn glob_base(pattern: &str) -> String {
    pattern
        .split('*')
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string()
}

pub fn relative_path(path: &Path, base: &str) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// Meta file helpers

fn load_input_meta(base_path: &str) -> AssetMeta {
    let tmeta_path = Path::new(base_path).with_extension("tmeta");
    AssetMeta::load_for(&tmeta_path).unwrap_or_default()
}

// Conversion validation

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

// DPI grouping

/// Group InputImages by base name, separating @Nx variants from 1x originals.
/// Returns:
/// - `groups`: base_name -> sorted vec of (scale, InputImage)
/// - `non_dpi`: images with no variants at any scale (upload as plain assets)
fn group_dpi_variants(
    images: Vec<pack::InputImage>,
) -> (
    HashMap<String, Vec<(u8, pack::InputImage)>>,
    Vec<pack::InputImage>,
) {
    // Identify which base names have any @Nx variant.
    let mut has_variants: std::collections::HashSet<String> = std::collections::HashSet::new();
    for img in &images {
        let stem = img.name.rsplit('/').next().unwrap_or(&img.name);
        if parse_dpi_suffix(stem).is_some() {
            // strip the @Nx to get the base key
            let base_stem = strip_dpi_suffix(stem);
            let prefix = if let Some(slash) = img.name.rfind('/') {
                &img.name[..=slash]
            } else {
                ""
            };
            has_variants.insert(format!("{}{}", prefix, base_stem));
        }
    }

    let mut groups: HashMap<String, Vec<(u8, pack::InputImage)>> = HashMap::new();
    let mut non_dpi: Vec<pack::InputImage> = Vec::new();

    for img in images {
        let stem = img.name.rsplit('/').next().unwrap_or(&img.name).to_string();
        let prefix = if let Some(slash) = img.name.rfind('/') {
            img.name[..=slash].to_string()
        } else {
            String::new()
        };

        if let Some(scale) = parse_dpi_suffix(&stem) {
            // This is a @Nx variant — add to its group.
            let base_stem = strip_dpi_suffix(&stem);
            let base_key = format!("{}{}", prefix, base_stem);
            groups.entry(base_key).or_default().push((scale, img));
        } else {
            let base_key = format!("{}{}", prefix, stem);
            if has_variants.contains(&base_key) {
                // The 1x version — add to group at scale 1.
                groups.entry(base_key).or_default().push((1, img));
            } else {
                non_dpi.push(img);
            }
        }
    }

    // Sort each group ascending by scale.
    for variants in groups.values_mut() {
        variants.sort_by_key(|(s, _)| *s);
    }

    (groups, non_dpi)
}

// Upload / copy dispatch

/// Upload bytes to Roblox Cloud and return the asset ID.
#[allow(dead_code)]
async fn cloud_upload(client: &Arc<RobloxClient>, params: UploadParams) -> Result<u64> {
    client.upload(params).await
}

/// Resolve an asset's ID: check lockfile first, then upload/copy depending on target.
/// Returns `Some(id)` for cloud/lockfile, `Some(0)` for studio/debug/dry-run codegen placeholders.
/// For studio the returned "id" is meaningless — codegen for studio uses URIs separately.
#[allow(dead_code)]
async fn resolve_asset_id(
    hash: &str,
    input_name: &str,
    lockfile: &Lockfile,
    target: Target,
    _dry_run: bool,
) -> Option<u64> {
    if let Some(cached) = lockfile.get(input_name, hash) {
        return Some(cached);
    }
    match target {
        Target::Cloud => None, // caller must upload
        Target::Studio | Target::Debug => Some(0),
    }
}

// Encode helpers

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

// Codegen writer

fn write_codegen(
    entries: Vec<CodegenEntry>,
    input_name: &str,
    output_path: &str,
    style: &str,
    strip_extension: bool,
    ts_declaration: bool,
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
    match codegen::generate(
        entries,
        &table_name,
        style,
        strip_extension,
        output_path,
        ts_declaration,
    ) {
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
    ts_declaration: bool,
    target: Target,
    dry_run: bool,
    creator: &Creator,
    client: &Option<Arc<RobloxClient>>,
    studio_sync: &Option<Arc<StudioSync>>,
    debug_sync: &Option<Arc<DebugSync>>,
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
        let meta = AssetMeta::load_for(path).unwrap_or_default();
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
    let mut upload_tasks: JoinSet<Result<(String, u64, String)>> = JoinSet::new();

    for p in pending {
        if dry_run {
            log!(info, "Dry run: would process \"{}\"", p.name);
            codegen_entries.push(CodegenEntry::asset_id(p.name, 0));
            continue;
        }

        match target {
            Target::Studio => {
                let rel = format!("{}.{}", p.name, p.kind.api_type().to_lowercase());
                let uri = if let Some(ss) = studio_sync {
                    match ss.copy_asset(&rel, &p.bytes) {
                        Ok(u) => u,
                        Err(e) => {
                            log!(warn, "Studio copy failed for \"{}\": {}", p.name, e);
                            errors += 1;
                            continue;
                        }
                    }
                } else {
                    String::new()
                };
                lockfile.set_uri(input_name, p.hash.clone(), uri.clone());
                log!(success, "\"{}\" -> {}", p.name, uri);
                codegen_entries.push(CodegenEntry::asset(p.name, codegen::AssetRef::Uri(uri)));
            }
            Target::Debug => {
                let rel = format!(
                    "{}.{}",
                    p.name,
                    p.path.extension().and_then(|e| e.to_str()).unwrap_or("bin")
                );
                if let Some(ds) = debug_sync {
                    if let Err(e) = ds.copy_asset(&rel, &p.bytes) {
                        log!(warn, "Debug copy failed for \"{}\": {}", p.name, e);
                        errors += 1;
                        continue;
                    }
                }
                let fallback_id = lockfile.get(input_name, &p.hash).unwrap_or(0);
                codegen_entries.push(CodegenEntry::asset_id(p.name, fallback_id));
            }
            Target::Cloud => {
                if let Some(cached_id) = lockfile.get(input_name, &p.hash) {
                    log!(
                        info,
                        "\"{}\" unchanged, skipping (rbxassetid://{})",
                        p.name,
                        cached_id
                    );
                    codegen_entries.push(CodegenEntry::asset_id(p.name, cached_id));
                    continue;
                }

                let Some(c) = client else {
                    codegen_entries.push(CodegenEntry::asset_id(p.name, 0));
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
                let name_clone = p.name.clone();
                let hash_clone = p.hash.clone();

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
                        .with_context(|| format!("Failed to upload \"{}\"", name_clone))?;
                    Ok((name_clone, id, hash_clone))
                });
            }
        }
    }

    let upload_total = upload_tasks.len();
    let mut completed = 0usize;

    while let Some(res) = upload_tasks.join_next().await {
        completed += 1;
        match res {
            Ok(Ok((name, id, hash))) => {
                lockfile.set(input_name, hash, id);
                progress(completed, upload_total, &name);
                codegen_entries.push(CodegenEntry::asset_id(name, id));
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
        ts_declaration,
        &mut errors,
    );
    errors
}

// Packed spritesheet processing

#[allow(clippy::too_many_arguments)]
async fn process_packed(
    input_name: &str,
    sheet_meta: &AssetMeta,
    images: Vec<pack::InputImage>,
    output_path: &str,
    codegen_style: &str,
    strip_extension: bool,
    ts_declaration: bool,
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

    // Separate DPI groups from regular images before packing.
    let (dpi_groups, plain_images) = group_dpi_variants(images);

    let mut codegen_entries: Vec<CodegenEntry> = Vec::new();

    // --- Process DPI groups ---
    // Each scale level of a DPI group is packed into its own spritesheet pass.
    // We upload each scale separately and emit a single DpiGroup codegen entry.
    if !dpi_groups.is_empty() {
        // Collect all unique scale values across all groups.
        let mut all_scales: std::collections::BTreeSet<u8> = std::collections::BTreeSet::new();
        for variants in dpi_groups.values() {
            for &(scale, _) in variants {
                all_scales.insert(scale);
            }
        }

        // Per-scale: gather images, pack, upload, collect (base_name, scale, asset_id).
        // Map: base_name -> Vec<(scale, asset_id)>
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
                "Packing {}x spritesheet ({} image(s))...",
                scale,
                scale_images.len()
            );

            let spritesheets = match pack::pack(scale_images) {
                Ok(s) => s,
                Err(e) => {
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

                // DPI group variants always store a u64 ID.
                // For Studio, fall back to the cached cloud ID (0 if none).
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

        // Emit one DpiGroup entry per base name.
        for (name, mut variants) in dpi_ids {
            variants.sort_by_key(|(s, _)| *s);
            codegen_entries.push(CodegenEntry::dpi_group(name, variants));
        }
    }

    // --- Process plain (non-DPI) images ---
    if !plain_images.is_empty() {
        log!(
            info,
            "Packing {} plain image(s) into spritesheet(s)...",
            plain_images.len()
        );

        let spritesheets = match pack::pack(plain_images) {
            Ok(s) => s,
            Err(e) => {
                log!(
                    warn,
                    "Failed to pack plain images for \"{}\": {}",
                    input_name,
                    e
                );
                errors += 1;
                // Fall through to write whatever codegen_entries we have.
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

        log!(success, "Packed into {} spritesheet(s)", spritesheets.len());

        for (idx, sheet) in spritesheets.iter().enumerate() {
            let mut sheet_image = sheet.image.clone();
            alpha_bleed(&mut sheet_image);

            let png_bytes = match encode_png(&sheet_image) {
                Ok(b) => b,
                Err(e) => {
                    log!(warn, "Failed to encode sheet #{}: {}", idx + 1, e);
                    errors += 1;
                    continue;
                }
            };

            let hash = hash_image(&png_bytes);
            let sheet_name = format!("{}_{:03}", sheet_base, idx + 1);

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

/// Upload or copy a single spritesheet PNG, returning an `AssetRef`.
/// - `Cloud` → `AssetRef::Id(id)` (uploaded or cached)
/// - `Studio` → `AssetRef::Uri(uri)` (copied into Studio content folder)
/// - `Debug`  → `AssetRef::Id(cached_or_0)` (copied locally, ID for reference)
#[allow(clippy::too_many_arguments)]
async fn upload_or_copy_sheet(
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
        log!(info, "Dry run: would upload/copy \"{}\"", sheet_name);
        return Ok(codegen::AssetRef::Id(0));
    }

    match target {
        Target::Cloud => {
            if let Some(cached) = lockfile.get(input_name, hash) {
                log!(
                    info,
                    "Spritesheet \"{}\" unchanged, skipping (rbxassetid://{})",
                    sheet_name,
                    cached
                );
                return Ok(codegen::AssetRef::Id(cached));
            }
            let Some(c) = client else {
                return Ok(codegen::AssetRef::Id(0));
            };
            log!(info, "Uploading \"{}\"...", sheet_name);
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
            log!(
                success,
                "\"{}\" uploaded -> rbxassetid://{}",
                sheet_name,
                id
            );
            Ok(codegen::AssetRef::Id(id))
        }
        Target::Studio => {
            let rel = format!("{}.png", sheet_name);
            let uri = if let Some(ss) = studio_sync {
                let uri = ss
                    .copy_asset(&rel, png_bytes)
                    .with_context(|| format!("Studio copy failed for \"{}\"", sheet_name))?;
                log!(success, "\"{}\" copied -> {}", sheet_name, uri);
                uri
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
                log!(success, "\"{}\" copied to debug folder", sheet_name);
            }
            Ok(codegen::AssetRef::Id(
                lockfile.get(input_name, hash).unwrap_or(0),
            ))
        }
    }
}

// Individual image processing

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
    ts_declaration: bool,
    target: Target,
    dry_run: bool,
    creator: &Creator,
    client: &Option<Arc<RobloxClient>>,
    studio_sync: &Option<Arc<StudioSync>>,
    debug_sync: &Option<Arc<DebugSync>>,
    lockfile: &mut Lockfile,
) -> u32 {
    let mut errors: u32 = 0;
    let total = images.len();

    // Group DPI variants from the loaded images.
    let (dpi_groups, plain_images) = group_dpi_variants(images);

    struct Pending {
        name: String,
        path: PathBuf,
        bytes: Vec<u8>,
        hash: String,
        kind: AssetKind,
        display_name: String,
        description: String,
    }

    let mut pending: Vec<Pending> = Vec::with_capacity(total);

    // Plain (non-DPI) images.
    for img in plain_images {
        // Find the corresponding path by matching name.
        let path = paths
            .iter()
            .find(|p| {
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let rel_no_ext = relative_path(p, base_path)
                    .trim_end_matches(|c| c != '/')
                    .to_string();
                let _ = rel_no_ext;
                // Match by stripping extension from relative path.
                let rel = relative_path(p, base_path);
                let rel_stem = Path::new(&rel)
                    .with_extension("")
                    .to_string_lossy()
                    .replace('\\', "/");
                rel_stem == img.name || stem == img.name.rsplit('/').next().unwrap_or(&img.name)
            })
            .cloned()
            .unwrap_or_else(|| PathBuf::from(&img.name));

        let src_ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png");
        let is_svg = src_ext.eq_ignore_ascii_case("svg");

        let (mut bytes, fmt) = if is_svg {
            let rel = relative_path(&path, base_path);
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
            match encode_with_conversion(&img.image, &path, base_path, convert_rules) {
                Ok(r) => r,
                Err(e) => {
                    log!(warn, "Failed to encode \"{}\": {}", img.name, e);
                    errors += 1;
                    continue;
                }
            }
        };

        // Alpha bleed the individual image before upload.
        {
            let mut rgba = match image::load_from_memory(&bytes) {
                Ok(i) => i.into_rgba8(),
                Err(_) => img.image.clone(),
            };
            alpha_bleed(&mut rgba);
            if let Ok(bled) = encode_png(&rgba) {
                bytes = bled;
            }
        }

        let _ = svg_scale;
        let hash = hash_image(&bytes);
        let kind = AssetKind::Image(fmt);
        let meta = AssetMeta::load_for(&path).unwrap_or_default();
        let display_name = meta.resolve_name(&img.name).to_string();
        let description = meta.resolve_description("Uploaded by Tungsten").to_string();

        pending.push(Pending {
            name: img.name,
            path,
            bytes,
            hash,
            kind,
            display_name,
            description,
        });
    }

    let mut codegen_entries: Vec<CodegenEntry> = Vec::with_capacity(total);
    let mut upload_tasks: JoinSet<Result<(String, u64, String)>> = JoinSet::new();

    // Dispatch plain images
    for p in pending {
        if dry_run {
            log!(info, "Dry run: would process \"{}\"", p.name);
            codegen_entries.push(CodegenEntry::asset_id(p.name, 0));
            continue;
        }

        match target {
            Target::Studio => {
                let rel = format!("{}.png", p.name);
                let uri = if let Some(ss) = studio_sync {
                    match ss.copy_asset(&rel, &p.bytes) {
                        Ok(u) => {
                            log!(success, "\"{}\" -> {}", p.name, u);
                            u
                        }
                        Err(e) => {
                            log!(warn, "Studio copy failed for \"{}\": {}", p.name, e);
                            errors += 1;
                            continue;
                        }
                    }
                } else {
                    String::new()
                };
                lockfile.set_uri(input_name, p.hash.clone(), uri.clone());
                codegen_entries.push(CodegenEntry::asset(p.name, codegen::AssetRef::Uri(uri)));
            }
            Target::Debug => {
                let rel = format!("{}.png", p.name);
                if let Some(ds) = debug_sync {
                    if let Err(e) = ds.copy_asset(&rel, &p.bytes) {
                        log!(warn, "Debug copy failed for \"{}\": {}", p.name, e);
                        errors += 1;
                        continue;
                    }
                }
                let fallback = lockfile.get(input_name, &p.hash).unwrap_or(0);
                codegen_entries.push(CodegenEntry::asset_id(p.name, fallback));
            }
            Target::Cloud => {
                if let Some(cached_id) = lockfile.get(input_name, &p.hash) {
                    codegen_entries.push(CodegenEntry::asset_id(p.name, cached_id));
                    continue;
                }
                let Some(c) = client else {
                    codegen_entries.push(CodegenEntry::asset_id(p.name, 0));
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
                let name_clone = p.name.clone();
                let hash_clone = p.hash.clone();
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
                        .with_context(|| format!("Failed to upload \"{}\"", name_clone))?;
                    Ok((name_clone, id, hash_clone))
                });
            }
        };
    }

    // Dispatch DPI group variants
    // For each base name, upload/copy each scale and accumulate (scale, id).
    // We do this synchronously per group to keep lockfile writes consistent.
    for (base_name, variants) in dpi_groups {
        if dry_run {
            log!(
                info,
                "Dry run: would process DPI group \"{}\" ({} variant(s))",
                base_name,
                variants.len()
            );
            let fake: Vec<(u8, u64)> = variants.iter().map(|(s, _)| (*s, 0)).collect();
            codegen_entries.push(CodegenEntry::dpi_group(base_name, fake));
            continue;
        }

        let mut resolved_variants: Vec<(u8, u64)> = Vec::with_capacity(variants.len());

        for (scale, img) in variants {
            let (mut bytes, fmt) = match convert::convert_image(&img.image, ImageFormat::Png) {
                Ok(b) => (b, ImageFormat::Png),
                Err(e) => {
                    log!(warn, "Failed to encode {}@{}x: {}", base_name, scale, e);
                    errors += 1;
                    continue;
                }
            };

            // Alpha bleed each DPI variant.
            {
                let mut rgba = img.image.clone();
                alpha_bleed(&mut rgba);
                if let Ok(bled) = encode_png(&rgba) {
                    bytes = bled;
                }
            }

            let hash = hash_image(&bytes);
            let lockfile_key = format!("{}@{}x", base_name, scale);

            match target {
                Target::Cloud => {
                    if let Some(cached) = lockfile.get(input_name, &hash) {
                        resolved_variants.push((scale, cached));
                        continue;
                    }
                    let Some(c) = client else {
                        resolved_variants.push((scale, 0));
                        continue;
                    };
                    log!(info, "Uploading \"{}\" @{}x...", base_name, scale);
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
                            kind: AssetKind::Image(fmt),
                            creator: creator.clone(),
                        })
                        .await
                    {
                        Ok(id) => {
                            lockfile.set(input_name, hash, id);
                            log!(
                                success,
                                "\"{}\" @{}x -> rbxassetid://{}",
                                base_name,
                                scale,
                                id
                            );
                            resolved_variants.push((scale, id));
                        }
                        Err(e) => {
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
                                log!(warn, "Studio copy failed: {}", e);
                                errors += 1;
                                continue;
                            }
                        }
                    } else {
                        String::new()
                    };
                    log!(success, "\"{}\" @{}x -> {}", base_name, scale, uri);
                    lockfile.set_uri(input_name, hash.clone(), uri);
                    // DpiGroup variants carry u64 IDs; use cached cloud ID if present.
                    resolved_variants.push((scale, lockfile.get(input_name, &hash).unwrap_or(0)));
                }
                Target::Debug => {
                    let rel = format!("{}@{}x.png", base_name, scale);
                    if let Some(ds) = debug_sync {
                        if let Err(e) = ds.copy_asset(&rel, &bytes) {
                            log!(warn, "Debug copy failed: {}", e);
                            errors += 1;
                            continue;
                        }
                    }
                    resolved_variants.push((scale, lockfile.get(input_name, &hash).unwrap_or(0)));
                }
            }

            let _ = lockfile_key;
        }

        if !resolved_variants.is_empty() {
            resolved_variants.sort_by_key(|(s, _)| *s);
            codegen_entries.push(CodegenEntry::dpi_group(base_name, resolved_variants));
        }
    }

    // Collect cloud upload results.
    let upload_total = upload_tasks.len();
    let mut completed = 0usize;
    while let Some(res) = upload_tasks.join_next().await {
        completed += 1;
        match res {
            Ok(Ok((name, id, hash))) => {
                lockfile.set(input_name, hash, id);
                progress(completed, upload_total, &name);
                codegen_entries.push(CodegenEntry::asset_id(name, id));
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
        ts_declaration,
        &mut errors,
    );
    errors
}
