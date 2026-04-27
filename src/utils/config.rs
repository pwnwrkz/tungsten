use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

use crate::core::assets::img::compress::CompressOptions as ResolvedCompressOptions;

const MIN_SVG_SCALE: f32 = 0.01;

#[derive(Deserialize)]
pub struct Config {
    pub creator: CreatorConfig,
    pub codegen: Option<CodegenConfig>,
    pub inputs: HashMap<String, InputConfig>,
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
/// png_quality    = 4
/// keep_metadata = false
/// ```
///
/// Or with a shared `[codegen]`-level default — not currently supported,
/// compression is always per-input.
#[derive(Deserialize, Debug, Clone)]
pub struct CompressOptions {
    /// JPEG quality 1–100. Defaults to 80.
    pub jpeg_quality: Option<u32>,
    /// PNG quality 1–100. Defaults to 80.
    pub png_quality: Option<u32>,
    /// Preserve EXIF/XMP/ICC metadata in the output. Defaults to false.
    pub keep_metadata: Option<bool>,
}

impl CompressOptions {
    /// Merge into a `compress::CompressOptions`, filling gaps with defaults.
    pub fn resolve(&self) -> ResolvedCompressOptions {
        ResolvedCompressOptions {
            jpeg_quality: self.jpeg_quality.unwrap_or(80),
            png_quality: self.png_quality.unwrap_or(3),
            keep_metadata: self.keep_metadata.unwrap_or(false),
        }
    }
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
///
/// [inputs.icons.compress_options]
/// jpeg_quality = 75
/// png_quality = 4
/// ```
#[derive(Deserialize)]
pub struct InputConfig {
    /// Glob pattern for source files.
    pub path: String,
    /// Path to the generated Luau/TypeScript file.
    pub output_path: String,
    /// Pack images into spritesheets. Only applies to image inputs.
    pub packable: Option<bool>,
    /// Scale factor applied when rasterizing SVG files (default: 1.0).
    pub svg_scale: Option<f32>,
    /// If present, compress images before upload using libcaesium.
    /// Omit the section entirely to skip compression.
    pub compress_options: Option<CompressOptions>,
}

impl InputConfig {
    /// Resolved SVG rasterization scale (defaults to 1.0).
    pub fn resolved_svg_scale(&self) -> f32 {
        self.svg_scale.unwrap_or(1.0).max(MIN_SVG_SCALE)
    }

    /// Returns resolved `compress::CompressOptions` if compression is enabled
    /// for this input, or `None` if the `compress_options` section was omitted.
    pub fn resolved_compress_options(&self) -> Option<ResolvedCompressOptions> {
        self.compress_options.as_ref().map(|o| o.resolve())
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

            [inputs.icons.compress_options]
            jpeg_quality  = 70
            png_quality   = 60
            optimize_gif  = false
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

            [inputs.icons.compress_options]
        "#,
        );
        let input = cfg.inputs.get("icons").unwrap();
        assert!(
            input.compress_options.is_some(),
            "empty compress_options section should still enable compression"
        );
    }
}
