use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
pub struct Config {
    pub creator: CreatorConfig,
    pub codegen: Option<CodegenConfig>,
    pub inputs: HashMap<String, InputConfig>,
}

#[derive(Deserialize)]
pub struct CreatorConfig {
    #[serde(rename = "type")]
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
    ///
    /// The declaration file mirrors the Luau structure, with asset values
    /// typed as `string` and sprite entries as typed objects with
    /// `Image`, `ImageRectOffset`, and `ImageRectSize` fields.
    ///
    /// Example:
    /// ```toml
    /// [codegen]
    /// style = "flat"
    /// strip_extension = true
    /// ts_declaration = true
    /// ```
    pub ts_declaration: Option<bool>,
}

/// Per-input configuration block.
///
/// Example:
/// ```toml
/// [inputs.icons]
/// path = "assets/icons/**/*.{png,svg}"
/// output_path = "src/icons.luau"
/// packable = true
/// svg_scale = 2.0   # render SVGs at 2× their natural size (default: 1.0)
/// convert = [
///     "jpg -> png",               # all .jpg → .png
///     "sword.png -> sword.jpg",   # specific file (any folder)
///     "ui/logo.svg -> ui/logo.bmp", # exact path override
///     "svg -> tga",               # override SVG default (otherwise → png)
/// ]
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
    /// Use 2.0 for 2× resolution, etc.
    pub svg_scale: Option<f32>,
    /// Conversion rules as an array of `"from -> to"` strings.
    /// Supports extension-wide rules (`"jpg -> png"`), filename rules
    /// (`"sword.png -> sword.jpg"`), and full relative path rules.
    /// SVGs are always converted to PNG unless a rule overrides this.
    pub convert: Option<Vec<String>>,
}

impl InputConfig {
    /// Resolved SVG rasterization scale (defaults to 1.0).
    pub fn resolved_svg_scale(&self) -> f32 {
        self.svg_scale.unwrap_or(1.0).max(0.01)
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
    fn test_convert_array() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/**/*.png"
            output_path = "src/icons.luau"
            convert = ["jpg -> png", "sword.png -> sword.jpg", "svg -> bmp"]
        "#,
        );
        let input = cfg.inputs.get("icons").unwrap();
        let rules = input.convert.as_deref().unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0], "jpg -> png");
        assert_eq!(rules[1], "sword.png -> sword.jpg");
        assert_eq!(rules[2], "svg -> bmp");
    }

    #[test]
    fn test_no_convert_is_none() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.assets]
            path = "assets/**/*.png"
            output_path = "src/assets.luau"
        "#,
        );
        let input = cfg.inputs.get("assets").unwrap();
        assert!(input.convert.is_none());
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
}
