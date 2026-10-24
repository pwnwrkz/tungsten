//! `tungsten.toml` parsing: config, creator, codegen, and per-input options.

use anyhow::{Context, Result};
use relative_path::RelativePathBuf;
use serde::Deserialize;
use std::collections::HashMap;

use crate::core::assets::asset::WebAsset;
use crate::core::assets::audio::encode::VorbisProfile;
use crate::core::assets::audio::loudness::ResolvedNormalizeOptions;
use crate::core::assets::audio::transcode::ResolvedAudioOptions;
use crate::core::assets::img::compress::CompressOptions as ResolvedCompressOptions;
use crate::log;

#[derive(Deserialize, Clone)]
pub struct Config {
    pub creator: CreatorConfig,
    pub codegen: Option<CodegenConfig>,
    pub inputs: HashMap<String, InputConfig>,
    #[serde(default)]
    pub studio: Option<StudioConfig>,
    #[serde(default = "default_max_concurrent_uploads")]
    pub max_concurrent_uploads: usize,
}

fn default_max_concurrent_uploads() -> usize {
    10
}

fn default_creator_type() -> String {
    "user".to_string()
}

#[derive(Deserialize, Clone)]
pub struct CreatorConfig {
    #[serde(rename = "type", default = "default_creator_type")]
    pub creator_type: String,
    pub id: u64,
}

#[derive(Deserialize, Clone)]
pub struct CodegenConfig {
    pub style: Option<String>,
    pub strip_extension: Option<bool>,
    pub ts_declaration: Option<bool>,
    pub strip_whitespace: Option<bool>,
}

impl CodegenConfig {
    pub fn resolved_style(&self) -> &str {
        self.style.as_deref().unwrap_or("flat")
    }

    pub fn resolved_strip_extension(&self) -> bool {
        self.strip_extension.unwrap_or(false)
    }

    pub fn resolved_ts_declaration(&self) -> bool {
        self.ts_declaration.unwrap_or(false)
    }

    pub fn resolved_strip_whitespace(&self) -> bool {
        self.strip_whitespace.unwrap_or(false)
    }
}

/// Per-input `[compress]` settings (the raw, optional config form). The
/// resolved runtime form with defaults filled in is
/// [`crate::core::assets::img::compress::CompressOptions`].
#[derive(Deserialize, Debug, Clone)]
pub struct CompressConfig {
    pub jpeg_quality: Option<u32>,
    pub png_quality: Option<u32>,
    pub keep_metadata: Option<bool>,
}

impl CompressConfig {
    pub fn resolve(&self) -> ResolvedCompressOptions {
        ResolvedCompressOptions {
            jpeg_quality: self.jpeg_quality.unwrap_or(80),
            png_quality: self.png_quality.unwrap_or(80),
            keep_metadata: self.keep_metadata.unwrap_or(false),
        }
    }
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[serde(untagged)]
pub enum CellSize {
    Square(u32),
    Rect([u32; 2]),
}

impl CellSize {
    pub fn dims(&self) -> (u32, u32) {
        match self {
            CellSize::Square(s) => (*s, *s),
            CellSize::Rect([w, h]) => (*w, *h),
        }
    }
}

/// Transcode settings for an input's audio files.
///
/// When this table is present, every audio file in the input is transcoded
/// to OGG Vorbis (decoded via symphonia, re-encoded via vorbis_rs). Sources
/// that are already OGG are passed through untouched.
///
/// Example:
/// ```toml
/// [inputs.sounds]
/// path = "assets/sounds/**/*"
/// output_path = "src/sounds.luau"
///
/// [inputs.sounds.transcode]
/// vorbis_quality = 5.0
/// ```
#[derive(Deserialize, Debug, Clone)]
pub struct TranscodeOptions {
    /// Vorbis VBR quality on the libvorbis `-1.0..=10.0` scale.
    /// Defaults to 5.0 (roughly 160 kbps stereo).
    pub vorbis_quality: Option<f32>,
    /// Target VBR bitrate in kbps. Takes precedence over `vorbis_quality`.
    pub bitrate: Option<u32>,
}

impl TranscodeOptions {
    fn profile(&self) -> VorbisProfile {
        match self.bitrate {
            Some(kbps) => VorbisProfile::Bitrate(kbps),
            None => VorbisProfile::Quality(self.vorbis_quality.unwrap_or(5.0)),
        }
    }
}

/// Loudness normalization settings for an input's audio files.
///
/// When present, integrated loudness is measured per EBU R128 / ITU-R BS.1770-4
/// (K-weighted, gated at -70 LUFS absolute and -10 LU relative) and gain is
/// applied to hit the target, capped by the true peak ceiling. Normalization
/// requires decoding to PCM, so enabling it also implies transcoding every
/// audio file to OGG Vorbis (OGG sources are re-encoded too).
///
/// Example:
/// ```toml
/// [inputs.sounds.normalize]
/// target_lufs = -14.0
/// true_peak_ceiling = -1.0
/// ```
#[derive(Deserialize, Debug, Clone)]
pub struct NormalizeOptions {
    /// Integrated loudness target in LUFS. Defaults to -14.0
    /// (the common streaming loudness target).
    pub target_lufs: Option<f64>,
    /// True peak ceiling in dBTP. Gain is reduced so the output never
    /// exceeds this, even if that means missing the loudness target.
    /// Defaults to -1.0.
    pub true_peak_ceiling: Option<f64>,
}

impl NormalizeOptions {
    fn resolve(&self) -> ResolvedNormalizeOptions {
        ResolvedNormalizeOptions {
            target_lufs: self.target_lufs.unwrap_or(-14.0),
            true_peak_ceiling: self.true_peak_ceiling.unwrap_or(-1.0),
        }
    }
}

impl InputConfig {
    /// Returns resolved audio processing settings when either the `transcode`
    /// or `normalize` table is present (each independently enables the audio
    /// pipeline), or `None` when audio is uploaded as-is.
    pub fn resolved_audio_options(&self) -> Option<ResolvedAudioOptions> {
        if self.transcode.is_none() && self.normalize.is_none() {
            return None;
        }
        let profile = self
            .transcode
            .as_ref()
            .map(TranscodeOptions::profile)
            .unwrap_or(VorbisProfile::Quality(5.0));
        Some(ResolvedAudioOptions {
            profile,
            normalize: self.normalize.as_ref().map(NormalizeOptions::resolve),
        })
    }
}

#[derive(Deserialize, Default, Clone)]
pub struct StudioConfig {
    #[serde(default)]
    pub studio_path: Option<String>,
    #[serde(default)]
    pub auto_route_version: bool,
}

#[derive(Deserialize, Clone)]
pub struct InputConfig {
    pub path: String,
    pub output_path: String,
    #[serde(rename = "type")]
    pub asset_type: Option<String>,
    pub packable: Option<bool>,
    pub uniform_cells: Option<bool>,
    pub cell_size: Option<CellSize>,
    pub cell_fit: Option<String>,
    pub svg_scale: Option<f32>,
    pub bleed: Option<bool>,
    pub compress_options: Option<CompressConfig>,
    /// If present, transcode audio files to OGG Vorbis before upload.
    pub transcode: Option<TranscodeOptions>,
    /// If present, normalize loudness (implies transcoding).
    pub normalize: Option<NormalizeOptions>,
    #[serde(default)]
    pub web: HashMap<RelativePathBuf, WebAsset>,
}

impl InputConfig {
    pub fn resolved_compress_options(&self) -> Option<ResolvedCompressOptions> {
        self.compress_options.as_ref().map(|o| o.resolve())
    }

    pub fn resolved_bleed(&self) -> bool {
        self.bleed.unwrap_or(true)
    }

    pub fn resolved_uniform_cells(&self) -> bool {
        self.uniform_cells.unwrap_or(true)
    }

    pub fn resolved_cell_fit(&self) -> &str {
        self.cell_fit.as_deref().unwrap_or("contain")
    }
}

pub const CONFIG_PATH: &str = "tungsten.toml";

pub fn load(path: &str) -> Result<Config> {
    let content = std::fs::read_to_string(path).with_context(|| {
        format!(
            "Could not read \"{}\" — make sure it exists in your project root",
            path
        )
    })?;

    let config: Config = toml::from_str(&content).with_context(|| {
        format!(
            "Failed to parse \"{}\" — check for missing or invalid fields",
            path
        )
    })?;

    log!(
        debug,
        "Loaded config {}: {} input(s), creator {} {}",
        path,
        config.inputs.len(),
        config.creator.creator_type,
        config.creator.id
    );

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::path::Path;

    use crate::core::assets::asset::{effective_svg_scale_for_path, new_svg_scale_cache};

    /// Effective SVG scale for an input — the free function plus the input's
    /// configured fallback, mirroring how the sync pipeline resolves it.
    fn effective_scale(cfg: &InputConfig, file: &Path, base: &str) -> f32 {
        effective_svg_scale_for_path(file, base, cfg.svg_scale, &new_svg_scale_cache())
    }
    use std::io::Write;
    use tempfile::TempDir;

    fn parse(s: &str) -> Config {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn test_basic_config() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/icons/**/*"
            output_path = "src/icons.luau"
            "#,
        );
        assert_eq!(cfg.creator.id, 1);
        assert_eq!(cfg.inputs.len(), 1);
    }

    #[test]
    fn test_svg_scale_default() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/icons/**/*"
            output_path = "src/icons.luau"
            "#,
        );
        assert_eq!(cfg.inputs["icons"].svg_scale, None);
    }

    #[test]
    fn test_svg_scale_custom() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/icons/**/*"
            output_path = "src/icons.luau"
            svg_scale = 2.0
            "#,
        );
        assert_eq!(cfg.inputs["icons"].svg_scale, Some(2.0));
    }

    #[test]
    fn test_ts_declaration_field_parses() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [codegen]
            ts_declaration = true

            [inputs.icons]
            path = "assets/icons/**/*"
            output_path = "src/icons.luau"
            "#,
        );
        assert!(cfg.codegen.unwrap().resolved_ts_declaration());
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

            [inputs.icons]
            path = "assets/icons/**/*"
            output_path = "src/icons.luau"
            "#,
        );
        assert!(!cfg.codegen.unwrap().resolved_ts_declaration());
    }

    #[test]
    fn test_compress_options_absent_means_no_compression() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/icons/**/*"
            output_path = "src/icons.luau"
            "#,
        );
        assert!(cfg.inputs["icons"].compress_options.is_none());
        assert!(cfg.inputs["icons"].resolved_compress_options().is_none());
    }

    #[test]
    fn test_compress_options_parses_all_fields() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/icons/**/*"
            output_path = "src/icons.luau"

            [inputs.icons.compress_options]
            jpeg_quality = 75
            png_quality = 50
            keep_metadata = true
            "#,
        );
        let opts = cfg.inputs["icons"].resolved_compress_options().unwrap();
        assert_eq!(opts.jpeg_quality, 75);
        assert_eq!(opts.png_quality, 50);
        assert!(opts.keep_metadata);
    }

    #[test]
    fn test_compress_options_partial_uses_defaults() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/icons/**/*"
            output_path = "src/icons.luau"

            [inputs.icons.compress_options]
            jpeg_quality = 75
            "#,
        );
        let opts = cfg.inputs["icons"].resolved_compress_options().unwrap();
        assert_eq!(opts.jpeg_quality, 75);
        assert_eq!(opts.png_quality, 80);
        assert!(!opts.keep_metadata);
    }

    #[test]
    fn test_empty_compress_options_section_enables_compression_with_defaults() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/icons/**/*"
            output_path = "src/icons.luau"

            [inputs.icons.compress_options]
            "#,
        );
        let opts = cfg.inputs["icons"].resolved_compress_options().unwrap();
        assert_eq!(opts.jpeg_quality, 80);
        assert_eq!(opts.png_quality, 80);
        assert!(!opts.keep_metadata);
    }

    #[test]
    fn test_effective_svg_scale_file_override() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("input");
        std::fs::create_dir_all(&base).unwrap();

        let file = base.join("art.png");
        std::fs::write(&file, b"x").unwrap();

        let tmeta = base.join("art.png.tmeta");
        {
            let mut f = File::create(&tmeta).unwrap();
            f.write_all(b"svg_scale = 3.0\n").unwrap();
        }

        let cfg = InputConfig {
            path: "input/**/*".to_string(),
            output_path: "out.luau".to_string(),
            asset_type: None,
            packable: None,
            uniform_cells: None,
            cell_size: None,
            cell_fit: None,
            svg_scale: Some(1.5),
            bleed: None,
            compress_options: None,
            transcode: None,
            normalize: None,
            web: HashMap::new(),
        };

        assert_eq!(effective_scale(&cfg, &file, base.to_str().unwrap()), 3.0);
    }

    #[test]
    fn test_effective_svg_scale_single_parent() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("input");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(base.join("child")).unwrap();

        // A `.tmeta` on the child directory applies to files inside it.
        let dir_tmeta = base.join("child.tmeta");
        {
            let mut f = File::create(&dir_tmeta).unwrap();
            f.write_all(b"svg_scale = 4.0\n").unwrap();
        }

        let cfg = InputConfig {
            path: "input/**/*".to_string(),
            output_path: "out.luau".to_string(),
            asset_type: None,
            packable: None,
            uniform_cells: None,
            cell_size: None,
            cell_fit: None,
            svg_scale: Some(1.0),
            bleed: None,
            compress_options: None,
            transcode: None,
            normalize: None,
            web: HashMap::new(),
        };

        let child = base.join("child").join("file.svg");
        assert_eq!(effective_scale(&cfg, &child, base.to_str().unwrap()), 4.0);
    }

    #[test]
    fn test_effective_svg_scale_multiple_nested() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("input");
        std::fs::create_dir_all(base.join("a/b/c")).unwrap();

        let root = base.join("input.tmeta");
        let mid = base.join("a.tmeta");
        let leaf = base.join("a/b.tmeta");
        for path in [&root, &mid, &leaf] {
            let mut f = File::create(path).unwrap();
            f.write_all(b"svg_scale = 1.0\n").unwrap();
        }

        let cfg = InputConfig {
            path: "input/**/*".to_string(),
            output_path: "out.luau".to_string(),
            asset_type: None,
            packable: None,
            uniform_cells: None,
            cell_size: None,
            cell_fit: None,
            svg_scale: Some(1.0),
            bleed: None,
            compress_options: None,
            transcode: None,
            normalize: None,
            web: HashMap::new(),
        };

        let file = base.join("a/b/c/art.svg");
        assert_eq!(effective_scale(&cfg, &file, base.to_str().unwrap()), 1.0);
    }

    #[test]
    fn test_audio_tables_absent_means_no_transcoding() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.sounds]
            path = "assets/sounds/**/*"
            output_path = "src/sounds.luau"
            "#,
        );
        let input = cfg.inputs.get("sounds").unwrap();
        assert!(input.transcode.is_none());
        assert!(input.normalize.is_none());
        assert!(input.resolved_audio_options().is_none());
    }

    #[test]
    fn test_empty_transcode_section_enables_transcoding_with_defaults() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.sounds]
            path = "assets/sounds/**/*"
            output_path = "src/sounds.luau"

            [inputs.sounds.transcode]
            "#,
        );
        let input = cfg.inputs.get("sounds").unwrap();
        let opts = input.resolved_audio_options().unwrap();
        assert_eq!(opts.profile, VorbisProfile::Quality(5.0));
        assert!(opts.normalize.is_none());
    }

    #[test]
    fn test_transcode_bitrate_takes_precedence_over_quality() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.sounds]
            path = "assets/sounds/**/*"
            output_path = "src/sounds.luau"

            [inputs.sounds.transcode]
            vorbis_quality = 8.0
            bitrate = 96
            "#,
        );
        let input = cfg.inputs.get("sounds").unwrap();
        let opts = input.resolved_audio_options().unwrap();
        assert_eq!(opts.profile, VorbisProfile::Bitrate(96));
    }

    #[test]
    fn test_normalize_table_alone_enables_audio_pipeline() {
        // Normalization requires decoding to PCM, so it implies transcoding
        // with default encoder settings even without a transcode table.
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.sounds]
            path = "assets/sounds/**/*"
            output_path = "src/sounds.luau"

            [inputs.sounds.normalize]
            "#,
        );
        let input = cfg.inputs.get("sounds").unwrap();
        let opts = input.resolved_audio_options().unwrap();
        assert_eq!(opts.profile, VorbisProfile::Quality(5.0));
        let normalize = opts.normalize.unwrap();
        assert_eq!(normalize.target_lufs, -14.0);
        assert_eq!(normalize.true_peak_ceiling, -1.0);
    }

    #[test]
    fn test_normalize_custom_levels_merge_with_transcode() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.sounds]
            path = "assets/sounds/**/*"
            output_path = "src/sounds.luau"

            [inputs.sounds.transcode]
            vorbis_quality = 6.0

            [inputs.sounds.normalize]
            target_lufs = -16.0
            true_peak_ceiling = -2.0
            "#,
        );
        let input = cfg.inputs.get("sounds").unwrap();
        let opts = input.resolved_audio_options().unwrap();
        assert_eq!(opts.profile, VorbisProfile::Quality(6.0));
        let normalize = opts.normalize.unwrap();
        assert_eq!(normalize.target_lufs, -16.0);
        assert_eq!(normalize.true_peak_ceiling, -2.0);
    }

    #[test]
    fn test_uniform_cells_defaults_to_true() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/**/*"
            output_path = "src/icons.luau"
            packable = true
            "#,
        );
        let input = cfg.inputs.get("icons").unwrap();
        assert!(input.resolved_uniform_cells());
        assert!(input.cell_size.is_none());
        assert_eq!(input.resolved_cell_fit(), "contain");
    }

    #[test]
    fn test_uniform_cells_can_be_disabled() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.icons]
            path = "assets/**/*"
            output_path = "src/icons.luau"
            packable = true
            uniform_cells = false
            "#,
        );
        let input = cfg.inputs.get("icons").unwrap();
        assert!(!input.resolved_uniform_cells());
    }

    #[test]
    fn test_cell_size_square_and_rect() {
        let cfg = parse(
            r#"
            [creator]
            type = "user"
            id = 1

            [inputs.square]
            path = "assets/a/**/*"
            output_path = "src/a.luau"
            packable = true
            cell_size = 128

            [inputs.rect]
            path = "assets/b/**/*"
            output_path = "src/b.luau"
            packable = true
            cell_size = [128, 64]
            cell_fit = "stretch"
            "#,
        );
        let square = cfg.inputs.get("square").unwrap();
        assert_eq!(square.cell_size.unwrap().dims(), (128, 128));
        let rect = cfg.inputs.get("rect").unwrap();
        assert_eq!(rect.cell_size.unwrap().dims(), (128, 64));
        assert_eq!(rect.resolved_cell_fit(), "stretch");
    }

    #[test]
    fn test_effective_svg_scale_input_dir() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("input");
        std::fs::create_dir_all(&base).unwrap();

        // A `.tmeta` next to the input directory applies to the whole input.
        let file = base.join("art.svg");
        let tmeta = base.parent().unwrap().join("input.tmeta");
        {
            let mut f = File::create(&tmeta).unwrap();
            f.write_all(b"svg_scale = 2.5\n").unwrap();
        }

        let cfg = InputConfig {
            path: "input".to_string(),
            output_path: "out.luau".to_string(),
            asset_type: None,
            packable: None,
            uniform_cells: None,
            cell_size: None,
            cell_fit: None,
            svg_scale: Some(1.0),
            bleed: None,
            compress_options: None,
            transcode: None,
            normalize: None,
            web: HashMap::new(),
        };

        assert_eq!(effective_scale(&cfg, &file, base.to_str().unwrap()), 2.5);
    }

    #[test]
    fn test_effective_svg_scale_fallback_to_config() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("input");
        std::fs::create_dir_all(&base).unwrap();

        let file = base.join("art.svg");

        let cfg = InputConfig {
            path: "input/**/*".to_string(),
            output_path: "out.luau".to_string(),
            asset_type: None,
            packable: None,
            uniform_cells: None,
            cell_size: None,
            cell_fit: None,
            svg_scale: Some(2.5),
            bleed: None,
            compress_options: None,
            transcode: None,
            normalize: None,
            web: HashMap::new(),
        };

        assert_eq!(effective_scale(&cfg, &file, base.to_str().unwrap()), 2.5);
    }
}
