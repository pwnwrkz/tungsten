use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use toml;

use crate::core::assets::img::compress::CompressOptions as ResolvedCompressOptions;

const MIN_SVG_SCALE: f32 = 0.01;

#[derive(Deserialize)]
pub struct Config {
    pub creator: CreatorConfig,
    pub codegen: Option<CodegenConfig>,
    pub inputs: HashMap<String, InputConfig>,
    /// Studio configuration (optional).
    #[serde(default)]
    pub studio: Option<StudioConfig>,
    /// Maximum concurrent uploads to Roblox API (default: 10).
    /// Increase for faster uploads if you have bandwidth, decrease if hitting rate limits.
    #[serde(default = "default_max_concurrent_uploads")]
    pub max_concurrent_uploads: usize,
}

fn default_max_concurrent_uploads() -> usize {
    10
}

fn default_creator_type() -> String {
    "user".to_string()
}

#[derive(Deserialize)]
pub struct CreatorConfig {
    #[serde(rename = "type", default = "default_creator_type")]
    /// Creator type to use: `"user"` or `"group"`. Defaults to `"user"`.
    pub creator_type: String,
    /// Creator ID to use.
    pub id: u64,
}

#[derive(Deserialize)]
pub struct CodegenConfig {
    /// Codegen style: `"flat"` or `"nested"`. Defaults to `"flat"`.
    pub style: Option<String>,
    /// Whether to strip the file extension from asset names in the output.
    /// Defaults to `false`.
    pub strip_extension: Option<bool>,
    /// Whether to generate a sibling `.d.ts` TypeScript definition file
    /// alongside the Luau output. Defaults to `false`.
    pub ts_declaration: Option<bool>,
}

impl CodegenConfig {
    /// Returns the configured codegen style, defaulting to `"flat"` when omitted.
    #[allow(dead_code)]
    pub fn resolved_style(&self) -> &str {
        self.style.as_deref().unwrap_or("flat")
    }
}

/// Compression settings applied to images before upload.
///
/// All fields are optional — omitting a field keeps the built-in default.
///
/// Example:
/// ```toml
/// [inputs.icons]
/// path = "assets/icons/**/*"
/// output_path = "src/icons.luau"
///
/// [inputs.icons.compress_options]
/// jpeg_quality = 75
/// png_quality    = 75
/// keep_metadata = false
/// ```
///
/// Or with a shared `[codegen]`-level default — not currently supported,
/// compression is always per-input.
#[derive(Deserialize, Debug, Clone)]
pub struct CompressOptions {
    /// JPEG quality 1–100. Defaults to 80.
    pub jpeg_quality: Option<u32>,
    /// PNG quality percentage 1–100 (higher = better quality, larger files).
    /// This is a user-facing percentage scale, not a raw PNG compression level (0–9).
    /// Defaults to 80.
    pub png_quality: Option<u32>,
    /// Preserve EXIF/XMP/ICC metadata in the output. Defaults to false.
    pub keep_metadata: Option<bool>,
}

impl CompressOptions {
    /// Merge into a `compress::CompressOptions`, filling gaps with defaults.
    pub fn resolve(&self) -> ResolvedCompressOptions {
        ResolvedCompressOptions {
            jpeg_quality: self.jpeg_quality.unwrap_or(80),
            png_quality: self.png_quality.unwrap_or(80),
            keep_metadata: self.keep_metadata.unwrap_or(false),
        }
    }
}

/// Studio-specific configuration.
#[derive(Deserialize, Default)]
pub struct StudioConfig {
    /// Base path to Roblox installation (where Versions folder lives).
    /// If set, overrides auto-detection.
    #[serde(default)]
    pub studio_path: Option<String>,
    /// If true, automatically fetch latest Studio version from
    /// https://setup.roblox.com/versionQTStudio and append Versions/<version> to studio_path.
    /// Only applies if studio_path is set.
    #[serde(default)]
    pub auto_route_version: bool,
}

/// Per-input configuration block.
///
/// Example:
/// ```toml
/// [inputs.icons]
/// path = "assets/icons/**/*"
/// output_path = "src/icons.luau"
/// packable = true
/// svg_scale = 2.0
/// type = "decal"
///
/// [inputs.icons.compress_options]
/// jpeg_quality = 75
/// png_quality = 75
/// ```
#[derive(Deserialize)]
pub struct InputConfig {
    /// Glob pattern for source files.
    pub path: String,
    /// Path to the generated Luau/TypeScript file.
    pub output_path: String,
    /// The Roblox asset type (e.g., "decal", "audio", "model"). Overrides the type inferred from file kind.
    #[serde(rename = "type")]
    pub asset_type: String,
    /// Pack images into spritesheets. Only applies to image inputs.
    pub packable: Option<bool>,
    /// Scale factor applied when rasterizing SVG files (default: 1.0).
    pub svg_scale: Option<f32>,
    /// Whether to apply alpha bleeding to images (default: true).
    /// When false, skips the alpha bleeding step to preserve original transparent borders.
    pub bleed: Option<bool>,
    /// If present, compress images before upload using libcaesium.
    /// Omit the section entirely to skip compression.
    pub compress_options: Option<CompressOptions>,
}

impl InputConfig {
    /// Resolved SVG rasterization scale (defaults to 1.0).
    pub fn resolved_svg_scale(&self) -> f32 {
        self.svg_scale.unwrap_or(1.0).max(MIN_SVG_SCALE)
    }

    /// Returns the effective SVG scale for a given file, checking
    /// `.tmeta` files in the file's directory and parent directories up to
    /// the input directory, falling back to the configured value.
    pub fn effective_svg_scale(&self, file_path: &Path, base_path: &str) -> f32 {
        let base = Path::new(base_path);
        // Walk from the file's directory up to and including the base directory.
        for anc in file_path.ancestors() {
            // Stop if we have gone above the base directory.
            if !anc.starts_with(base) {
                break;
            }
            if let Some(scale) = Self::svg_scale_from_tmeta(anc) {
                return scale.max(MIN_SVG_SCALE);
            }
            // Stop after checking the base directory itself.
            if anc == base {
                break;
            }
        }
        // Fallback to the config‑provided scale (or default 1.0).
        self.resolved_svg_scale()
    }

    /// Attempts to read an `svg_scale` field from a `.tmeta` file associated
    /// with `item` (which may be a file or directory). Returns `None` if the
    /// file does not exist, cannot be parsed, or does not contain the field.
    fn svg_scale_from_tmeta(item: &Path) -> Option<f32> {
        // Determine candidate .tmeta paths following the same precedence as
        // `AssetMeta::load_for`.
        if item.is_file() {
            // Try <file>.<ext>.tmeta first.
            if let Some(ext) = item.extension() {
                let mut path = item.to_path_buf();
                path.set_extension(format!("{}.tmeta", ext.to_string_lossy()));
                if let Some(scale) = Self::try_read_tmeta(&path) {
                    return Some(scale);
                }
            }
            // Fall back to <file>.tmeta.
            let mut path = item.to_path_buf();
            path.set_extension("tmeta");
            if let Some(scale) = Self::try_read_tmeta(&path) {
                return Some(scale);
            }
        } else {
            // Directory: <dir>.tmeta (i.e., parent directory contains <dir_name>.tmeta)
            let dir_name = {
                let n = item.file_name()?;
                n.to_os_string()
            };
            let mut parent = {
                let p = item.parent()?;
                p.to_path_buf()
            };
            parent.push(dir_name);
            parent.set_extension("tmeta");
            if let Some(scale) = Self::try_read_tmeta(&parent) {
                return Some(scale);
            }
        }
        None
    }

    /// Reads a `.tmeta` file and extracts the `svg_scale` field if present.
    fn try_read_tmeta(path: &Path) -> Option<f32> {
        std::fs::read_to_string(path).ok().and_then(|contents| {
            // Parse the TOML and look for a numeric `svg_scale` value.
            let map: toml::Value = toml::from_str(&contents).ok()?;
            match map.get("svg_scale") {
                Some(toml::Value::Float(v)) => Some(*v as f32),
                Some(toml::Value::Integer(v)) => Some(*v as f32),
                _ => None,
            }
        })
    }

    /// Returns resolved `compress::CompressOptions` if compression is enabled
    /// for this input, or `None` if the `compress_options` section was omitted.
    pub fn resolved_compress_options(&self) -> Option<ResolvedCompressOptions> {
        self.compress_options.as_ref().map(|o| o.resolve())
    }

    /// Returns whether alpha bleeding should be applied (defaults to true).
    pub fn resolved_bleed(&self) -> bool {
        self.bleed.unwrap_or(true)
    }
}

pub fn load(path: &str) -> Result<Config> {
    let content = std::fs::read_to_string(path).with_context(|| {
        format!(
            "Could not read \"{}\" — make sure it exists in your project root",
            path
        )
    })?;

    toml::from_str(&content).with_context(|| {
        format!(
            "Failed to parse \"{}\" — check for missing or invalid fields",
            path
        )
    })
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    fn parse(s: &str) -> Config {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn resolved_style_defaults_to_flat_when_none() {
        let cfg = CodegenConfig {
            style: None,
            strip_extension: None,
            ts_declaration: None,
        };
        assert_eq!(cfg.resolved_style(), "flat");
    }

    #[test]
    fn resolved_style_returns_configured_style() {
        let cfg = CodegenConfig {
            style: Some("nested".to_string()),
            strip_extension: None,
            ts_declaration: None,
        };
        assert_eq!(cfg.resolved_style(), "nested");
    }

    #[test]
    fn test_basic_config() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 12345

            [inputs.assets]
            path = "assets/**/*.png"
            output_path = "src/assets.luau"
            type = "image"
        "#,
        );
        assert_eq!(cfg.creator.id, 12345);
        assert!(cfg.inputs.contains_key("assets"));
    }

    #[test]
    fn test_svg_scale_default() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/**/*.svg"
            output_path = "src/icons.luau"
            type = "decal"
        "#,
        );
        let input = cfg.inputs.get("icons").unwrap();
        assert_eq!(input.resolved_svg_scale(), 1.0);
    }

    #[test]
    fn test_svg_scale_custom() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/**/*.svg"
            output_path = "src/icons.luau"
            svg_scale = 2.0
            type = "decal"
        "#,
        );
        let input = cfg.inputs.get("icons").unwrap();
        assert_eq!(input.resolved_svg_scale(), 2.0);
    }

    #[test]
    fn test_ts_declaration_field_parses() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [codegen]
            style = "flat"
            strip_extension = true
            ts_declaration = true

            [inputs.assets]
            path = "assets/**/*.png"
            output_path = "src/assets.luau"
            type = "image"
        "#,
        );
        let ts_def = cfg
            .codegen
            .as_ref()
            .and_then(|c| c.ts_declaration)
            .unwrap_or(false);
        assert!(ts_def);
    }

    #[test]
    fn test_ts_declaration_defaults_to_none() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [codegen]
            style = "flat"

            [inputs.assets]
            path = "assets/**/*.png"
            output_path = "src/assets.luau"
            type = "image"
        "#,
        );
        let ts_def = cfg.codegen.as_ref().and_then(|c| c.ts_declaration);
        assert!(ts_def.is_none());
    }

    #[test]
    fn test_compress_options_absent_means_no_compression() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/**/*"
            output_path = "src/icons.luau"
            type = "image"
        "#,
        );
        let input = cfg.inputs.get("icons").unwrap();
        assert!(input.compress_options.is_none());
    }

    #[test]
    fn test_compress_options_parses_all_fields() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/**/*"
            output_path = "src/icons.luau"
            type = "image"

            [inputs.icons.compress_options]
            jpeg_quality  = 70
            png_quality   = 60
            keep_metadata = true
        "#,
        );
        let opts = cfg
            .inputs
            .get("icons")
            .unwrap()
            .compress_options
            .as_ref()
            .unwrap();
        assert_eq!(opts.jpeg_quality, Some(70));
        assert_eq!(opts.png_quality, Some(60));
        assert_eq!(opts.keep_metadata, Some(true));
    }

    #[test]
    fn test_compress_options_partial_uses_defaults() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/**/*"
            output_path = "src/icons.luau"
            type = "image"

            [inputs.icons.compress_options]
            jpeg_quality = 60
        "#,
        );
        let input = cfg.inputs.get("icons").unwrap();
        let opts = input.compress_options.as_ref().unwrap();
        assert_eq!(opts.jpeg_quality, Some(60));
        assert!(opts.png_quality.is_none()); // filled by resolve()
        assert!(opts.keep_metadata.is_none());
    }

    #[test]
    fn test_empty_compress_options_section_enables_compression_with_defaults() {
        // An empty [inputs.x.compress_options] table opts in with all defaults.
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/**/*"
            output_path = "src/icons.luau"
            type = "image"

            [inputs.icons.compress_options]
        "#,
        );
        let input = cfg.inputs.get("icons").unwrap();
        assert!(
            input.compress_options.is_some(),
            "empty compress_options section should still enable compression"
        );
    }

    #[test]
    fn test_effective_svg_scale_file_override() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("input");
        std::fs::create_dir_all(&base).unwrap();

        // Create a fake file inside input
        let file = base.join("test.svg");
        File::create(&file).unwrap();

        // Parent directory .tmeta with scale 1.5 -> creates base.tmeta inside parent of base
        let parent_tmeta = base.parent().unwrap().join("input.tmeta");
        let mut f = File::create(&parent_tmeta).unwrap();
        writeln!(f, "svg_scale = 1.5").unwrap();

        // File-specific .tmeta (test.svg.tmeta) with scale 2.5
        let file_tmeta = file.with_extension("svg.tmeta");
        let mut f2 = File::create(&file_tmeta).unwrap();
        writeln!(f2, "svg_scale = 2.5").unwrap();

        let config_str = format!(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.test]
            path = "{}"
            output_path = "src/out.luau"
            type = "decal"
            "#,
            base.to_string_lossy().replace('\\', "/")
        );
        let cfg: Config = toml::from_str(&config_str).unwrap();
        let input = cfg.inputs.get("test").unwrap();
        let scale =
            input.effective_svg_scale(&file, base.to_str().unwrap().replace('\\', "/").as_str());
        // Should pick file's own .tmeta (2.5)
        assert_eq!(scale, 2.5);
    }

    #[test]
    fn test_effective_svg_scale_single_parent() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("input");
        std::fs::create_dir_all(&base).unwrap();
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let file = sub.join("test.svg");
        File::create(&file).unwrap();

        // Parent directory (sub) .tmeta with scale 3.0 -> creates sub.tmeta inside base
        let parent_tmeta = base.join("sub.tmeta");
        let mut f = File::create(&parent_tmeta).unwrap();
        writeln!(f, "svg_scale = 3.0").unwrap();
        // No .tmeta on file

        let config_str = format!(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.test]
            path = "{}"
            output_path = "src/out.luau"
            type = "decal"
            "#,
            base.to_string_lossy().replace('\\', "/")
        );
        let cfg: Config = toml::from_str(&config_str).unwrap();
        let input = cfg.inputs.get("test").unwrap();
        let scale =
            input.effective_svg_scale(&file, base.to_str().unwrap().replace('\\', "/").as_str());
        // Should pick parent's .tmeta (3.0)
        assert_eq!(scale, 3.0);
    }

    #[test]
    fn test_effective_svg_scale_multiple_nested() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("input");
        std::fs::create_dir_all(&base).unwrap();
        let l1 = base.join("l1");
        std::fs::create_dir_all(&l1).unwrap();
        let l2 = l1.join("l2");
        std::fs::create_dir_all(&l2).unwrap();

        let file = l2.join("test.svg");
        File::create(&file).unwrap();

        // grandparent (l1) .tmeta with scale 4.0 -> creates l1.tmeta inside base
        let l1_tmeta = base.join("l1.tmeta");
        let mut f = File::create(&l1_tmeta).unwrap();
        writeln!(f, "svg_scale = 4.0").unwrap();
        // No .tmedia in l2 or file

        let config_str = format!(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.test]
            path = "{}"
            output_path = "src/out.luau"
            type = "decal"
            "#,
            base.to_string_lossy().replace('\\', "/")
        );
        let cfg: Config = toml::from_str(&config_str).unwrap();
        let input = cfg.inputs.get("test").unwrap();
        let scale =
            input.effective_svg_scale(&file, base.to_str().unwrap().replace('\\', "/").as_str());
        // Should pick l1's .tmeta (4.0)
        assert_eq!(scale, 4.0);
    }

    #[test]
    fn test_effective_svg_scale_input_dir() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("input");
        std::fs::create_dir_all(&base).unwrap();
        let sub = base.join("deep");
        std::fs::create_dir_all(&sub).unwrap();

        let file = sub.join("test.svg");
        File::create(&file).unwrap();

        // Input directory (base) .tmeta with scale 5.0 -> creates base.tmeta inside parent of base
        let parent_tmeta = base.parent().unwrap().join("input.tmeta");
        let mut f = File::create(&parent_tmeta).unwrap();
        writeln!(f, "svg_scale = 5.0").unwrap();
        // No other .tmeta

        let config_str = format!(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.test]
            path = "{}"
            output_path = "src/out.luau"
            type = "decal"
            "#,
            base.to_string_lossy().replace('\\', "/")
        );
        let cfg: Config = toml::from_str(&config_str).unwrap();
        let input = cfg.inputs.get("test").unwrap();
        let scale =
            input.effective_svg_scale(&file, base.to_str().unwrap().replace('\\', "/").as_str());
        // Should pick input dir's .tmeta (5.0)
        assert_eq!(scale, 5.0);
    }

    #[test]
    fn test_effective_svg_scale_fallback_to_config() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("input");
        std::fs::create_dir_all(&base).unwrap();
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let file = sub.join("test.svg");
        File::create(&file).unwrap();
        // No .tmeta anywhere

        let config_str = format!(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.test]
            path = "{}"
            output_path = "src/out.luau"
            svg_scale = 6.0
            type = "decal"
            "#,
            base.to_string_lossy().replace('\\', "/")
        );
        let cfg: Config = toml::from_str(&config_str).unwrap();
        let input = cfg.inputs.get("test").unwrap();
        let scale =
            input.effective_svg_scale(&file, base.to_str().unwrap().replace('\\', "/").as_str());
        // Should fall back to config svg_scale (6.0)
        assert_eq!(scale, 6.0);
    }

    #[test]
    fn test_effective_svg_scale_fallback_to_default_when_no_config() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("input");
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("test.svg");
        File::create(&file).unwrap();
        // No .tmeta anywhere

        let config_str = format!(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.test]
            path = "{}"
            output_path = "src/out.luau"
            type = "decal"
            "#,
            base.to_string_lossy().replace('\\', "/")
        );
        let cfg: Config = toml::from_str(&config_str).unwrap();
        let input = cfg.inputs.get("test").unwrap();
        let scale =
            input.effective_svg_scale(&file, base.to_str().unwrap().replace('\\', "/").as_str());
        // Should fall back to default 1.0
        assert_eq!(scale, 1.0);
    }

    #[test]
    fn test_effective_svg_scale_does_not_escape_input_root() {
        let tmp = TempDir::new().unwrap();
        // Create an external directory outside the input root
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        // Place a .tmeta with a huge value outside
        let outside_tmeta = outside.join("outside.tmeta");
        let mut f = File::create(&outside_tmeta).unwrap();
        writeln!(f, "svg_scale = 999.0").unwrap();

        // Input root
        let base = tmp.path().join("input");
        std::fs::create_dir_all(&base).unwrap();
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let file = sub.join("test.svg");
        File::create(&file).unwrap();
        // No .tmeta inside input tree

        let config_str = format!(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.test]
            path = "{}"
            output_path = "src/out.luau"
            svg_scale = 7.0
            type = "decal"
            "#,
            base.to_string_lossy().replace('\\', "/")
        );
        let cfg: Config = toml::from_str(&config_str).unwrap();
        let input = cfg.inputs.get("test").unwrap();
        let scale =
            input.effective_svg_scale(&file, base.to_str().unwrap().replace('\\', "/").as_str());
        // Should NOT pick the outside .tmeta (999.0); should fall back to config (7.0)
        assert_eq!(scale, 7.0);
    }
}
