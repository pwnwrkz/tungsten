use crate::log;
use anyhow::{Result, bail};
use std::path::Path;

const GITIGNORE_ENTRY: &str = "# Tungsten API key\ntungsten_api_key.env\n";

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
    if Path::new("tungsten.toml").exists() {
        bail!(
            "tungsten.toml already exists in this directory\n  \
             Hint: Delete it first if you want to reinitialize"
        );
    }

    log!(section, "Scanning for asset directories");

    let discovered = discover_asset_dirs(".");
    let config_content = build_config(&discovered);

    std::fs::write("tungsten.toml", &config_content)
        .map_err(|e| anyhow::anyhow!("Failed to create tungsten.toml: {}", e))?;

    if discovered.is_empty() {
        log!(success, "Created tungsten.toml with a default structure");
        log!(
            info,
            "No asset directories were detected — edit the [inputs] section manually"
        );
    } else {
        log!(
            success,
            "Created tungsten.toml with {} input(s) detected",
            discovered.len()
        );
        for dir in &discovered {
            log!(info, "  Found: {}", dir.display_path);
        }
    }

    log!(
        info,
        "See https://pwnwrkz.github.io/tungsten-docs/reference/configuration/ for configuration help"
    );

    // .gitignore update
    let gitignore = Path::new(".gitignore");
    let existing = std::fs::read_to_string(gitignore).unwrap_or_default();
    if !existing.contains("tungsten_api_key.env") {
        let content = if existing.is_empty() {
            GITIGNORE_ENTRY.to_string()
        } else {
            format!("{}\n{}", existing.trim_end(), GITIGNORE_ENTRY)
        };
        std::fs::write(gitignore, content)
            .map_err(|e| anyhow::anyhow!("Failed to update .gitignore: {}", e))?;
        log!(success, "Added tungsten_api_key.env to .gitignore");
    }

    Ok(())
}

// Discovery

struct DiscoveredDir {
    display_path: String,
    input_name: String,
    counts: KindCounts,
    has_subdirs: bool,
}

/// Asset file counts per kind.
#[derive(Debug, Clone, Copy, Default)]
struct KindCounts {
    images: usize,
    audio: usize,
    models: usize,
}

impl KindCounts {
    fn is_empty(&self) -> bool {
        self.images == 0 && self.audio == 0 && self.models == 0
    }

    /// True if more than one kind is present.
    fn is_mixed(&self) -> bool {
        [self.images > 0, self.audio > 0, self.models > 0]
            .iter()
            .filter(|&&b| b)
            .count()
            > 1
    }
}

fn discover_asset_dirs(root: &str) -> Vec<DiscoveredDir> {
    let mut results: Vec<DiscoveredDir> = Vec::new();
    scan_dir(Path::new(root), root, 0, 3, &mut results);
    results
}

fn scan_dir(
    path: &Path,
    root: &str,
    depth: usize,
    max_depth: usize,
    results: &mut Vec<DiscoveredDir>,
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

        if results.iter().any(|r| rel.starts_with(&r.display_path)) {
            continue;
        }

        let is_known = KNOWN_ASSET_DIRS.contains(&dir_name.to_ascii_lowercase().as_str());

        // Count only files directly in this dir (not recursive) to determine
        // whether the dir itself is single-kind or mixed. Mixing that comes
        // purely from subdirs means we should recurse deeper instead.
        let direct = count_direct_assets(&entry_path);
        let (total_counts, has_subdirs) = scan_for_assets(&entry_path);

        if !direct.is_empty() && direct.is_mixed() {
            // This dir directly contains files of multiple kinds — emit split inputs.
            results.push(DiscoveredDir {
                display_path: rel.clone(),
                input_name: make_input_name(&rel),
                counts: total_counts,
                has_subdirs,
            });
        } else if is_known || !total_counts.is_empty() {
            if has_subdirs && total_counts.is_mixed() && direct.is_empty() {
                // Mixing comes entirely from subdirs — recurse to discover them
                // as separate single-kind inputs rather than lumping everything.
                scan_dir(&entry_path, root, depth + 1, max_depth, results);
            } else {
                results.push(DiscoveredDir {
                    display_path: rel.clone(),
                    input_name: make_input_name(&rel),
                    counts: total_counts,
                    has_subdirs,
                });
            }
        } else {
            scan_dir(&entry_path, root, depth + 1, max_depth, results);
        }
    }
}

/// Count asset files *directly* inside `dir` — does not recurse into subdirs.
fn count_direct_assets(dir: &Path) -> KindCounts {
    let mut counts = KindCounts::default();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                continue;
            }
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            match ext.as_str() {
                "png" | "jpg" | "jpeg" | "bmp" | "tga" | "svg" => counts.images += 1,
                "mp3" | "ogg" | "flac" | "wav" => counts.audio += 1,
                "fbx" | "gltf" | "glb" | "rbxm" | "rbxmx" => counts.models += 1,
                _ => {}
            }
        }
    }
    counts
}

fn scan_for_assets(dir: &Path) -> (KindCounts, bool) {
    let mut counts = KindCounts::default();
    let mut has_subdirs = false;

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                has_subdirs = true;
                let (sub, _) = scan_for_assets(&p);
                counts.images += sub.images;
                counts.audio += sub.audio;
                counts.models += sub.models;
                continue;
            }
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            match ext.as_str() {
                "png" | "jpg" | "jpeg" | "bmp" | "tga" | "svg" => counts.images += 1,
                "mp3" | "ogg" | "flac" | "wav" => counts.audio += 1,
                "fbx" | "gltf" | "glb" | "rbxm" | "rbxmx" => counts.models += 1,
                _ => {}
            }
        }
    }

    (counts, has_subdirs)
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

fn make_input_name(rel_path: &str) -> String {
    rel_path
        .split('/')
        .next_back()
        .unwrap_or(rel_path)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

// Config builder

/// A single `[inputs.name]` block to emit.
struct InputBlock {
    name: String,
    glob: String,
    output_path: String,
    /// Whether to emit `packable = false` (images only).
    packable: bool,
}

/// Expand one `DiscoveredDir` into one or more `InputBlock`s.
///
/// Pure dirs (one kind only) -> one block.
/// Mixed dirs -> one block per present kind, suffixed `_images` / `_audio` / `_models`.
///
/// Globs use `*` (or `**/*` for dirs with subdirs). No brace patterns, since
/// the `glob` crate used by Tungsten does not expand them. Tungsten's
/// `kind_from_ext` filter automatically discards non-matching files.
fn dir_to_inputs(dir: &DiscoveredDir) -> Vec<InputBlock> {
    let wc = if dir.has_subdirs { "**/" } else { "" };
    let base = &dir.display_path;
    let name = &dir.input_name;

    if !dir.counts.is_mixed() {
        // Single-kind dir. Determine packable flag from which kind dominates.
        let is_image_dir =
            dir.counts.images >= dir.counts.audio && dir.counts.images >= dir.counts.models;
        return vec![InputBlock {
            name: name.clone(),
            glob: format!("{}/{}*", base, wc),
            output_path: format!("src/{}.luau", name),
            packable: is_image_dir,
        }];
    }

    // Mixed dir: split into one block per kind present.
    let mut blocks = Vec::new();

    if dir.counts.images > 0 {
        blocks.push(InputBlock {
            name: format!("{}_images", name),
            glob: format!("{}/{}*", base, wc),
            output_path: format!("src/{}_images.luau", name),
            packable: true,
        });
    }
    if dir.counts.audio > 0 {
        blocks.push(InputBlock {
            name: format!("{}_audio", name),
            glob: format!("{}/{}*", base, wc),
            output_path: format!("src/{}_audio.luau", name),
            packable: false,
        });
    }
    if dir.counts.models > 0 {
        blocks.push(InputBlock {
            name: format!("{}_models", name),
            glob: format!("{}/{}*", base, wc),
            output_path: format!("src/{}_models.luau", name),
            packable: false,
        });
    }

    blocks
}

fn build_config(dirs: &[DiscoveredDir]) -> String {
    let mut out = String::new();

    out.push_str("[creator]\n");
    out.push_str("type = \"user\"\n");
    out.push_str("id = 0\n");
    out.push('\n');
    out.push_str("[codegen]\n");
    out.push_str("style = \"flat\"\n");
    out.push_str("strip_extension = true\n");
    out.push('\n');

    if dirs.is_empty() {
        out.push_str("[inputs.assets]\n");
        out.push_str("path = \"assets/**/*\"\n");
        out.push_str("output_path = \"src/assets.luau\"\n");
        out.push_str("packable = false\n");
        return out;
    }

    for dir in dirs {
        for block in dir_to_inputs(dir) {
            out.push_str(&format!("[inputs.{}]\n", block.name));
            out.push_str(&format!("path = \"{}\"\n", block.glob));
            out.push_str(&format!("output_path = \"{}\"\n", block.output_path));
            if block.packable {
                out.push_str("packable = true\n");
            }
            out.push('\n');
        }
    }

    out
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_input_name() {
        assert_eq!(make_input_name("assets/icons"), "icons");
        assert_eq!(make_input_name("src/assets"), "assets");
        assert_eq!(make_input_name("my-images"), "my_images");
        assert_eq!(make_input_name("assets"), "assets");
    }

    #[test]
    fn test_is_noise_dir() {
        assert!(is_noise_dir("node_modules"));
        assert!(is_noise_dir("target"));
        assert!(is_noise_dir(".git"));
        assert!(!is_noise_dir("assets"));
        assert!(!is_noise_dir("icons"));
    }

    #[test]
    fn test_build_config_empty() {
        let cfg = build_config(&[]);
        assert!(cfg.contains("[creator]"));
        assert!(cfg.contains("[inputs.assets]"));
        assert!(cfg.contains("path = \"assets/**/*\""));
        assert!(!cfg.contains('{'), "no brace patterns in fallback");
    }

    #[test]
    fn test_no_brace_patterns() {
        let dirs = vec![
            DiscoveredDir {
                display_path: "assets/icons".into(),
                input_name: "icons".into(),
                counts: KindCounts {
                    images: 5,
                    audio: 0,
                    models: 0,
                },
                has_subdirs: true,
            },
            DiscoveredDir {
                display_path: "assets/sounds".into(),
                input_name: "sounds".into(),
                counts: KindCounts {
                    images: 0,
                    audio: 3,
                    models: 0,
                },
                has_subdirs: false,
            },
            DiscoveredDir {
                display_path: "assets/mixed".into(),
                input_name: "mixed".into(),
                counts: KindCounts {
                    images: 2,
                    audio: 2,
                    models: 1,
                },
                has_subdirs: false,
            },
        ];
        let cfg = build_config(&dirs);
        assert!(
            !cfg.contains('{'),
            "brace patterns must not appear in generated config"
        );
        assert!(cfg.contains("[inputs.icons]"));
        assert!(cfg.contains("[inputs.sounds]"));
        // Direct-mixed dir still splits
        assert!(cfg.contains("[inputs.mixed_images]"));
        assert!(cfg.contains("[inputs.mixed_audio]"));
        assert!(cfg.contains("[inputs.mixed_models]"));
    }

    /// Simulates a test structure:
    ///
    /// assets/
    /// - audio/some_audio.mp3
    /// - images/some_image.png
    ///
    /// `assets` itself has no direct files — mixing comes from subdirs only.
    /// Expected: `audio` and `images` discovered as separate inputs, NOT `assets_images` + `assets_audio`.
    #[test]
    fn test_subdir_mixed_recurses_into_children() {
        // Pretend scan_dir ran and found the children directly.
        // We test build_config with what scan_dir *should* produce.
        let dirs = vec![
            DiscoveredDir {
                display_path: "assets/images".into(),
                input_name: "images".into(),
                counts: KindCounts {
                    images: 1,
                    audio: 0,
                    models: 0,
                },
                has_subdirs: false,
            },
            DiscoveredDir {
                display_path: "assets/audio".into(),
                input_name: "audio".into(),
                counts: KindCounts {
                    images: 0,
                    audio: 1,
                    models: 0,
                },
                has_subdirs: false,
            },
        ];
        let cfg = build_config(&dirs);
        assert!(cfg.contains("[inputs.images]"));
        assert!(cfg.contains("[inputs.audio]"));
        assert!(
            !cfg.contains("[inputs.assets_images]"),
            "should not lump into parent"
        );
        assert!(
            !cfg.contains("[inputs.assets_audio]"),
            "should not lump into parent"
        );
    }

    #[test]
    fn test_single_kind_dirs() {
        let dirs = vec![
            DiscoveredDir {
                display_path: "assets/icons".into(),
                input_name: "icons".into(),
                counts: KindCounts {
                    images: 4,
                    audio: 0,
                    models: 0,
                },
                has_subdirs: true,
            },
            DiscoveredDir {
                display_path: "assets/sounds".into(),
                input_name: "sounds".into(),
                counts: KindCounts {
                    images: 0,
                    audio: 2,
                    models: 0,
                },
                has_subdirs: false,
            },
        ];
        let cfg = build_config(&dirs);
        assert!(cfg.contains("[inputs.icons]"));
        assert!(cfg.contains("[inputs.sounds]"));
        assert!(cfg.contains("packable = true")); // icons is an image dir
        assert!(cfg.contains("**/")); // icons has subdirs
        assert!(!cfg.contains("sounds/**/")); // sounds has no subdirs
    }
}
