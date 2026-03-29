use anyhow::{bail, Context, Result};
use glob::glob;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task::JoinSet;

use crate::api::roblox::{Creator, GroupCreator, UserCreator};
use crate::api::upload::RobloxClient;
use crate::core::codegen::{self, CodegenEntry};
use crate::core::lockfile::{hash_image, Lockfile};
use crate::core::pack;
use crate::utils::config::Config;
use crate::utils::logger::progress;
use crate::log;

// Entry point

pub async fn run(config: Config, api_key: Option<String>, target: &str) -> Result<()> {
    let mut errors: u32 = 0;

    let mut lockfile = Lockfile::load().context("Failed to load lockfile")?;

    let client: Option<Arc<RobloxClient>> = if target == "roblox" {
        let key = api_key.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "Missing --api-key flag\n  \
                 Hint: Generate an API key at https://create.roblox.com/credentials \
                 with \"Assets: Read & Write\" permissions"
            )
        })?;
        Some(Arc::new(RobloxClient::new(key.to_string())))
    } else {
        None
    };

    let creator = match config.creator.creator_type.as_str() {
        "user" => Creator::User(UserCreator {
            user_id: config.creator.id.to_string(),
        }),
        "group" => Creator::Group(GroupCreator {
            group_id: config.creator.id.to_string(),
        }),
        other => bail!(
            "Invalid creator type \"{}\"\n  Hint: Must be \"user\" or \"group\"",
            other
        ),
    };

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

        // Resolve glob
        let paths: Vec<PathBuf> = glob(&input.path)
            .with_context(|| format!(
                "Invalid glob pattern \"{}\"\n  Hint: Example: path = \"assets/**/*.png\"",
                input.path
            ))?
            .filter_map(|entry| match entry {
                Ok(p) if p.extension().map(|e| e == "png").unwrap_or(false) => Some(p),
                Ok(_) => None,
                Err(e) => {
                    log!(warn, "Skipping unreadable path: {}", e);
                    None
                }
            })
            .collect();

        if paths.is_empty() {
            log!(warn, "No PNG files matched \"{}\" — skipping", input.path);
            continue;
        }

        log!(info, "Found {} PNG files", paths.len());

        // Load images
        let base_path = input.path
            .split('*')
            .next()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_string();

        log!(info, "Loading images...");

        let images = match pack::load_images(paths, &base_path) {
            Ok(imgs) => imgs,
            Err(e) => {
                log!(warn, "Failed to load images for \"{}\": {}", input_name, e);
                errors += 1;
                continue;
            }
        };

        // Dispatch to pack or individual upload path
        errors += if input.packable.unwrap_or(false) {
            process_packed(
                input_name,
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
                &input.output_path,
                &codegen_style,
                strip_extension,
                &creator,
                &client,
                &mut lockfile,
            )
            .await
        };
    }

    log!(section, "Done");

    if errors > 0 {
        log!(
            warn,
            "Sync completed with {} error(s) — some assets may not have been uploaded",
            errors
        );
    } else {
        log!(success, "Tungsten sync complete!");
    }

    Ok(())
}

// Packed path

async fn process_packed(
    input_name: &str,
    images: Vec<pack::InputImage>,
    output_path: &str,
    codegen_style: &str,
    strip_extension: bool,
    creator: &Creator,
    client: &Option<Arc<RobloxClient>>,
    lockfile: &mut Lockfile,
) -> u32 {
    let mut errors: u32 = 0;

    log!(info, "Packing into spritesheets...");
    let spritesheets = match pack::pack(images) {
        Ok(s)  => s,
        Err(e) => {
            log!(warn, "Failed to pack images for \"{}\": {}", input_name, e);
            return 1;
        }
    };

    log!(success, "Packed into {} spritesheet(s)", spritesheets.len());

    let mut codegen_entries: Vec<CodegenEntry> = Vec::new();

    for (idx, sheet) in spritesheets.iter().enumerate() {
        let png_bytes = match encode_png(&sheet.image) {
            Ok(b)  => b,
            Err(e) => {
                log!(warn, "Failed to encode spritesheet #{}: {}", idx + 1, e);
                errors += 1;
                continue;
            }
        };

        let hash = hash_image(&png_bytes);

        let asset_id = match client {
            Some(c) => {
                if let Some(cached) = lockfile.get(input_name, &hash) {
                    log!(info, "Spritesheet #{} unchanged, skipping (rbxassetid://{})", idx + 1, cached);
                    cached
                } else {
                    log!(info, "Uploading spritesheet #{}...", idx + 1);
                    match c.upload(
                        &format!("tungsten_{}_{}", input_name, idx),
                        png_bytes,
                        creator.clone(),
                    )
                    .await
                    {
                        Ok(id) => {
                            lockfile.set(input_name, hash, id);
                            if let Err(e) = lockfile.save() {
                                log!(warn, "Failed to save lockfile: {}", e);
                                errors += 1;
                            }
                            log!(success, "Spritesheet #{} uploaded → rbxassetid://{}", idx + 1, id);
                            id
                        }
                        Err(e) => {
                            log!(warn, "Failed to upload spritesheet #{}: {}", idx + 1, e);
                            errors += 1;
                            continue;
                        }
                    }
                }
            }
            None => {
                log!(info, "Dry run: skipping upload for spritesheet #{}", idx + 1);
                0
            }
        };

        for img in &sheet.images {
            codegen_entries.push(CodegenEntry {
                name: img.name.clone(),
                asset_id,
                rect_offset: (img.x, img.y),
                rect_size: (img.width, img.height),
            });
        }
    }

    write_codegen(codegen_entries, input_name, output_path, codegen_style, strip_extension, &mut errors);
    errors
}

// Individual path

async fn process_individual(
    input_name: &str,
    images: Vec<pack::InputImage>,
    output_path: &str,
    codegen_style: &str,
    strip_extension: bool,
    creator: &Creator,
    client: &Option<Arc<RobloxClient>>,
    lockfile: &mut Lockfile,
) -> u32 {
    let mut errors: u32 = 0;
    let total = images.len();

    // ── Encode (CPU-bound, serial here but fast in practice) ─────────────────
    struct Pending {
        name:   String,
        width:  u32,
        height: u32,
        bytes:  Vec<u8>,
        hash:   String,
    }

    let mut pending: Vec<Pending> = Vec::with_capacity(total);

    for img in images {
        match encode_png(&img.image) {
            Ok(bytes) => {
                let hash = hash_image(&bytes);
                pending.push(Pending {
                    name:   img.name,
                    width:  img.image.width(),
                    height: img.image.height(),
                    bytes,
                    hash,
                });
            }
            Err(e) => {
                log!(warn, "Failed to encode \"{}\": {}", img.name, e);
                errors += 1;
            }
        }
    }

    // Cache hits vs. uploads
    let mut codegen_entries: Vec<CodegenEntry>  = Vec::with_capacity(pending.len());
    let mut upload_tasks: JoinSet<Result<(String, u32, u32, u64, String)>> = JoinSet::new();

    for p in pending {
        // Cache hit — no network needed.
        if let Some(cached_id) = lockfile.get(input_name, &p.hash) {
            codegen_entries.push(CodegenEntry {
                name:        p.name,
                asset_id:    cached_id,
                rect_offset: (0, 0),
                rect_size:   (p.width, p.height),
            });
            continue;
        }

        // Dry run — no client.
        let Some(c) = client else {
            codegen_entries.push(CodegenEntry {
                name:        p.name,
                asset_id:    0,
                rect_offset: (0, 0),
                rect_size:   (p.width, p.height),
            });
            continue;
        };

        // Spawn upload task.
        let c_arc        = Arc::clone(c);
        let creator_own  = creator.clone();

        upload_tasks.spawn(async move {
            let id = c_arc
                .upload(&p.name, p.bytes, creator_own)
                .await
                .with_context(|| format!("Failed to upload \"{}\"", p.name))?;
            Ok((p.name, p.width, p.height, id, p.hash))
        });
    }

    // Collect results
    let upload_total  = upload_tasks.len();
    let mut completed = 0usize;

    while let Some(res) = upload_tasks.join_next().await {
        completed += 1;

        match res {
            Ok(Ok((name, width, height, id, hash))) => {
                lockfile.set(input_name, hash, id);
                if let Err(e) = lockfile.save() {
                    log!(warn, "Failed to save lockfile: {}", e);
                    errors += 1;
                }
                progress(completed, upload_total, &name);
                codegen_entries.push(CodegenEntry {
                    name,
                    asset_id:    id,
                    rect_offset: (0, 0),
                    rect_size:   (width, height),
                });
            }
            Ok(Err(e)) => {
                log!(warn, "{}", e);
                errors += 1;
            }
            Err(e) => {
                // JoinError = task panicked.
                log!(warn, "Upload task panicked: {}", e);
                errors += 1;
            }
        }
    }

    if upload_total > 0 {
        // Flush the progress bar to its own line.
        progress(total, total, "done");
        println!();
    }

    write_codegen(codegen_entries, input_name, output_path, codegen_style, strip_extension, &mut errors);
    errors
}

// Helpers

fn encode_png(image: &image::RgbaImage) -> Result<Vec<u8>> {
    let mut bytes: Vec<u8> = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut bytes)),
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
    let table_name = match std::path::Path::new(output_path)
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
            log!(warn, "Failed to write codegen for \"{}\": {}", input_name, e);
            *errors += 1;
        }
    }
}
