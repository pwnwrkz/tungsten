use crate::log;
use crate::utils::interactive::{self, DiscoveredFolder, FolderSelection};
use anyhow::Result;
use std::path::Path;

#[allow(dead_code)]
const ASSET_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "bmp", "tga", "svg", "mp3", "ogg", "flac", "wav", "fbx", "gltf", "glb",
    "rbxm", "rbxmx",
];

const KNOWN_ASSET_DIRS: &[&str] = &[
    "assets",
    "asset",
    "images",
    "image",
    "img",
    "icons",
    "icon",
    "sounds",
    "sound",
    "audio",
    "sfx",
    "music",
    "textures",
    "texture",
    "sprites",
    "sprite",
    "models",
    "model",
    "public",
    "res",
    "resources",
    "resource",
    "media",
    "static",
    "content",
    "game",
    "games",
    "client",
    "server",
    "shared",
    "src",
];

pub fn run() -> Result<()> {
    // INITIAL CHECK: Ask to overwrite if config exists
    if Path::new("tungsten.toml").exists() {
        let overwrite = interactive::confirm("tungsten.toml already exists. Overwrite?", false)?;
        if !overwrite {
            log!(info, "Init cancelled");
            return Ok(());
        }
    }

    log!(section, "TUNGSTEN INTERACTIVE INIT");

    // 1. Creator type
    let creator_type = interactive::select("Creator type:", &["user", "group"], Some(0))?;
    let creator_type = if creator_type == 0 { "user" } else { "group" };

    // 2. Creator ID
    let creator_id_str = interactive::input("Creator ID:")?;
    let creator_id: u64 = creator_id_str
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid creator ID: must be a number"))?;

    // 3. Discover and select folders
    log!(section, "SCANNING FOR ASSET DIRECTORIES");
    let discovered = discover_asset_folders(".")?;

    let selected_folders = if discovered.is_empty() {
        log!(
            warn,
            "No asset directories detected — you can add them manually later"
        );
        Vec::new()
    } else {
        interactive::select_folders_with_types(
            "Select asset folders to include (one by one):",
            &discovered,
        )?
    };

    // 4. Codegen style
    let codegen_style = interactive::select("Codegen style:", &["flat", "nested"], Some(0))?;
    let codegen_style = if codegen_style == 0 { "flat" } else { "nested" };

    // 5. Strip extensions
    let strip_extension = interactive::confirm("Strip file extensions from asset names?", true)?;

    // 6. TypeScript declarations
    let ts_declaration =
        interactive::confirm("Generate TypeScript definition files (.d.ts)?", false)?;

    // Build config
    let config_content = build_interactive_config(
        creator_type,
        creator_id,
        &selected_folders,
        codegen_style,
        strip_extension,
        ts_declaration,
    );

    std::fs::write("tungsten.toml", &config_content)
        .map_err(|e| anyhow::anyhow!("Failed to create tungsten.toml: {}", e))?;

    log!(success, "Created tungsten.toml");
    if !selected_folders.is_empty() {
        log!(info, "Included {} input(s):", selected_folders.len());
        for f in &selected_folders {
            log!(info, "  {} (type: {})", f.display_name, f.asset_type);
        }
    } else {
        log!(
            info,
            "No inputs configured — edit tungsten.toml to add them"
        );
    }
    log!(
        info,
        "See https://pwnwrkz.github.io/tungsten-docs/reference/configuration/ for configuration help"
    );

    Ok(())
}

// Discovery: Find generic parent folders and their type-specific subfolders

fn discover_asset_folders(root: &str) -> Result<Vec<DiscoveredFolder>> {
    let mut results = Vec::new();
    scan_for_folders(Path::new(root), root, 0, 3, &mut results);
    Ok(results)
}

fn scan_for_folders(
    path: &Path,
    root: &str,
    depth: usize,
    max_depth: usize,
    results: &mut Vec<DiscoveredFolder>,
) {
    if depth > max_depth {
        return;
    }

    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let entry_path = entry.path();
        if !entry_path.is_dir() {
            continue;
        }

        let dir_name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        if dir_name.starts_with('.') || is_noise_dir(&dir_name) {
            continue;
        }

        let rel = entry_path
            .strip_prefix(root)
            .unwrap_or(&entry_path)
            .to_string_lossy()
            .replace('\\', "/");

        // Check if this is a known generic folder (assets, content, etc.)
        let dir_name_lower = dir_name.to_ascii_lowercase();
        let is_type_specific = is_type_specific_dir(&dir_name_lower);
        let is_generic = !is_type_specific && KNOWN_ASSET_DIRS.contains(&dir_name_lower.as_str());

        if is_generic {
            // Scan subdirectories for type-specific folders
            let subdirs = find_type_subdirs(&entry_path);
            for subdir in subdirs {
                let sub_rel = subdir
                    .strip_prefix(root)
                    .unwrap_or(&subdir)
                    .to_string_lossy()
                    .replace('\\', "/");

                if results.iter().any(|r| r.path == sub_rel) {
                    continue;
                }

                let counts = count_assets_in_dir(&subdir);
                let suggested = suggest_type(&counts);

                results.push(DiscoveredFolder {
                    path: sub_rel.clone(),
                    display_name: subdir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&sub_rel)
                        .to_string(),
                    suggested_type: suggested,
                    asset_counts: counts,
                });
            }

            scan_for_folders(&entry_path, root, depth + 1, max_depth, results);
        } else {
            // Direct type-specific folder
            if results.iter().any(|r| rel.starts_with(&r.path)) {
                continue;
            }

            let counts = count_assets_in_dir(&entry_path);
            if counts.total() == 0 {
                scan_for_folders(&entry_path, root, depth + 1, max_depth, results);
                continue;
            }

            let suggested = suggest_type(&counts);

            results.push(DiscoveredFolder {
                path: rel.clone(),
                display_name: dir_name,
                suggested_type: suggested,
                asset_counts: counts,
            });
        }
    }
}

fn find_type_subdirs(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut subdirs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();

                // Check if subdir name suggests a type
                if is_type_specific_dir(&name) {
                    subdirs.push(path);
                }
            }
        }
    }
    subdirs
}

fn is_type_specific_dir(name: &str) -> bool {
    matches!(
        name,
        "images"
            | "image"
            | "img"
            | "icons"
            | "icon"
            | "sprites"
            | "sprite"
            | "textures"
            | "texture"
            | "models"
            | "model"
            | "meshes"
            | "mesh"
            | "sounds"
            | "sound"
            | "audio"
            | "sfx"
            | "music"
            | "animations"
            | "animation"
    )
}

fn count_assets_in_dir(dir: &Path) -> interactive::AssetCounts {
    let mut counts = interactive::AssetCounts::default();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();

            match ext.as_str() {
                "png" | "jpg" | "jpeg" | "bmp" | "tga" | "svg" => counts.images += 1,
                "mp3" | "ogg" | "flac" | "wav" => counts.audio += 1,
                "fbx" | "gltf" | "glb" | "rbxm" | "rbxmx" => {
                    // Check if rbxm/rbxmx is animation
                    if ext == "rbxm" || ext == "rbxmx" {
                        if let Ok(true) = crate::core::assets::asset::is_animation_file(&path) {
                            counts.animations += 1;
                        } else {
                            counts.models += 1;
                        }
                    } else {
                        counts.models += 1;
                    }
                }
                _ => counts.other += 1,
            }
        }
    }
    counts
}

fn suggest_type(counts: &interactive::AssetCounts) -> String {
    if counts.images > 0
        && counts.images >= counts.audio
        && counts.images >= counts.models
        && counts.images >= counts.animations
    {
        "image"
    } else if counts.audio > 0 {
        "audio"
    } else if counts.animations > 0 {
        "animation"
    } else if counts.models > 0 {
        "model"
    } else {
        "auto"
    }
    .to_string()
}

fn is_noise_dir(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "node_modules"
            | "target"
            | "dist"
            | "build"
            | "out"
            | "__pycache__"
            | ".git"
            | ".svn"
            | ".hg"
            | "vendor"
            | "deps"
            | "packages"
            | "Packages"
            | "DevPackages"
    )
}

// Config builder

fn build_interactive_config(
    creator_type: &str,
    creator_id: u64,
    folders: &[FolderSelection],
    codegen_style: &str,
    strip_extension: bool,
    ts_declaration: bool,
) -> String {
    let mut out = String::new();

    out.push_str("[creator]\n");
    out.push_str(&format!("type = \"{}\"\n", creator_type));
    out.push_str(&format!("id = {}\n", creator_id));
    out.push('\n');

    out.push_str("[codegen]\n");
    out.push_str(&format!("style = \"{}\"\n", codegen_style));
    out.push_str(&format!("strip_extension = {}\n", strip_extension));
    out.push_str(&format!("ts_declaration = {}\n", ts_declaration));
    out.push('\n');

    if folders.is_empty() {
        out.push_str("[inputs.assets]\n");
        out.push_str("path = \"assets/**/*\"\n");
        out.push_str("output_path = \"src/assets.luau\"\n");
        out.push_str("packable = false\n");
        // type omitted = auto
        return out;
    }

    for (i, folder) in folders.iter().enumerate() {
        let name = if folder.display_name.is_empty() {
            format!("input_{}", i + 1)
        } else {
            folder.display_name.clone()
        };

        // Sanitize name
        let name = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>()
            .trim_matches('_')
            .to_string();

        out.push_str(&format!("[inputs.{}]\n", name));
        out.push_str(&format!("path = \"{}/**/*\"\n", folder.path));
        out.push_str(&format!("output_path = \"src/{}.luau\"\n", name));

        if folder.asset_type != "auto" {
            out.push_str(&format!("type = \"{}\"\n", folder.asset_type));
        }

        out.push('\n');
    }

    out
}
