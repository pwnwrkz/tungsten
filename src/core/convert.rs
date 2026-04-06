use anyhow::{Context, Result, bail};
use image::RgbaImage;
use resvg;
use std::path::Path;
use tiny_skia;
use usvg;

use super::asset::ImageFormat;

// Rule parsing

/// A single parsed conversion rule from the config array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertRule {
    ExtWide { from: String, to: String },
    FileSpecific { from: String, to: String },
}

impl ConvertRule {
    /// Parse a single rule string like `"jpg -> png"` or `"ui/sword.png -> ui/sword.jpg"`.
    pub fn parse(s: &str) -> Result<Self> {
        let (lhs, rhs) = s.split_once("->").with_context(|| {
            format!(
                "Invalid convert rule \"{}\"\n  Hint: Format is \"from -> to\", \
                 e.g. \"jpg -> png\" or \"sword.png -> sword.jpg\"",
                s
            )
        })?;

        let from = lhs.trim().to_string();
        let to = rhs.trim().to_string();

        if from.is_empty() || to.is_empty() {
            bail!(
                "Invalid convert rule \"{}\": both sides must be non-empty",
                s
            );
        }

        // A rule is file-specific if the lhs contains a slash (path) or has a
        // dot in it (filename like "sword.png") but is NOT a bare extension.
        let is_file_specific =
            from.contains('/') || (from.contains('.') && !is_bare_extension(&from));

        if is_file_specific {
            Ok(ConvertRule::FileSpecific { from, to })
        } else {
            Ok(ConvertRule::ExtWide {
                from: from.to_ascii_lowercase(),
                to: to.to_ascii_lowercase(),
            })
        }
    }
}

/// Returns `true` if `s` is a bare extension with no dots — e.g. "jpg", "png", "mp3".
fn is_bare_extension(s: &str) -> bool {
    !s.contains('/') && !s.contains('.')
}

/// A resolved set of conversion rules for a single input.
#[derive(Debug, Default, Clone)]
pub struct ConvertRules {
    pub rules: Vec<ConvertRule>,
}

impl ConvertRules {
    /// Parse from the raw TOML array of strings.
    pub fn parse_all(raw: &[String]) -> Result<Self> {
        let rules = raw
            .iter()
            .map(|s| ConvertRule::parse(s))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { rules })
    }

    /// Resolve the target extension for a given file.
    pub fn resolve<'a>(&'a self, relative_path: &str, src_ext: &str) -> Option<&'a str> {
        let file_name = Path::new(relative_path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(relative_path);

        let src_ext_lower = src_ext.to_ascii_lowercase();

        for rule in &self.rules {
            if let ConvertRule::FileSpecific { from, to } = rule {
                if from == relative_path {
                    return Some(to.as_str());
                }
            }
        }

        for rule in &self.rules {
            if let ConvertRule::FileSpecific { from, to } = rule {
                let rule_file = Path::new(from)
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or(from.as_str());
                if rule_file == file_name {
                    return Some(to.as_str());
                }
            }
        }

        for rule in &self.rules {
            if let ConvertRule::ExtWide { from, to } = rule {
                if from == &src_ext_lower {
                    return Some(to.as_str());
                }
            }
        }

        // Pass 4: SVG default → png
        if src_ext_lower == "svg" {
            return Some("png");
        }

        None
    }
}

// SVG rasterization

/// Rasterize an SVG file to a PNG byte buffer.
///
/// `scale` controls output resolution: `1.0` renders at the SVG's natural size,
/// `2.0` doubles it, etc. The result is always lossless PNG.
pub fn svg_to_png(data: &[u8], scale: f32) -> Result<Vec<u8>> {
    let scale = scale.max(0.01);

    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_data(data, &opt).context("Failed to parse SVG")?;

    let size = tree.size();
    let width = ((size.width() * scale) as u32).max(1);
    let height = ((size.height() * scale) as u32).max(1);

    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .context("Failed to allocate pixmap for SVG rasterization")?;

    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    pixmap
        .encode_png()
        .context("Failed to encode rasterized SVG as PNG")
}

// Image format conversion
pub fn convert_image(image: &RgbaImage, target: ImageFormat) -> Result<Vec<u8>> {
    use image::ImageEncoder;

    let capacity = (image.width() * image.height() * 4) as usize;
    let mut buf: Vec<u8> = Vec::with_capacity(capacity);

    match target {
        ImageFormat::Png => {
            image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut buf))
                .write_image(
                    image.as_raw(),
                    image.width(),
                    image.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .context("Failed to encode PNG")?;
        }
        ImageFormat::Jpg => {
            // JPEG has no alpha channel, flatten onto white first.
            let rgb = flatten_alpha(image);
            image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::Cursor::new(&mut buf), 95)
                .write_image(
                    rgb.as_raw(),
                    rgb.width(),
                    rgb.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .context("Failed to encode JPEG")?;
        }
        ImageFormat::Bmp => {
            image::DynamicImage::ImageRgba8(image.clone())
                .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Bmp)
                .context("Failed to encode BMP")?;
        }
        ImageFormat::Tga => {
            image::DynamicImage::ImageRgba8(image.clone())
                .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Tga)
                .context("Failed to encode TGA")?;
        }
    }

    Ok(buf)
}

/// Load raw bytes as an image and re-encode to `target` format.
pub fn transcode_image(data: &[u8], target: ImageFormat) -> Result<Vec<u8>> {
    let image = image::load_from_memory(data)
        .context("Failed to decode source image")?
        .into_rgba8();
    convert_image(&image, target)
}

/// Composite RGBA onto white, producing an RGB image (for JPEG output).
fn flatten_alpha(src: &RgbaImage) -> image::RgbImage {
    let (w, h) = src.dimensions();
    let mut out = image::RgbImage::new(w, h);
    for (x, y, px) in src.enumerate_pixels() {
        let a = px[3] as f32 / 255.0;
        let r = (px[0] as f32 * a + 255.0 * (1.0 - a)) as u8;
        let g = (px[1] as f32 * a + 255.0 * (1.0 - a)) as u8;
        let b = (px[2] as f32 * a + 255.0 * (1.0 - a)) as u8;
        out.put_pixel(x, y, image::Rgb([r, g, b]));
    }
    out
}

// ── Format string helpers ─────────────────────────────────────────────────────

pub fn image_format_from_str(s: &str) -> Result<ImageFormat> {
    match s.to_ascii_lowercase().as_str() {
        "png" => Ok(ImageFormat::Png),
        "jpg" | "jpeg" => Ok(ImageFormat::Jpg),
        "bmp" => Ok(ImageFormat::Bmp),
        "tga" => Ok(ImageFormat::Tga),
        other => bail!(
            "Unsupported image format \"{}\"\n  Hint: Supported formats: png, jpg, bmp, tga",
            other
        ),
    }
}

pub fn unsupported_audio_message(from: &str, to: &str) -> String {
    format!(
        "Audio conversion from {} → {} is not supported\n  \
         Hint: Convert your audio files manually before running tungsten sync",
        from, to
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(raw: &[&str]) -> ConvertRules {
        ConvertRules::parse_all(&raw.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn test_parse_ext_wide() {
        let r = ConvertRule::parse("jpg -> png").unwrap();
        assert_eq!(
            r,
            ConvertRule::ExtWide {
                from: "jpg".into(),
                to: "png".into()
            }
        );
    }

    #[test]
    fn test_parse_file_specific_filename() {
        let r = ConvertRule::parse("sword.png -> sword.jpg").unwrap();
        assert_eq!(
            r,
            ConvertRule::FileSpecific {
                from: "sword.png".into(),
                to: "sword.jpg".into()
            }
        );
    }

    #[test]
    fn test_parse_file_specific_path() {
        let r = ConvertRule::parse("ui/icons/sword.png -> ui/icons/sword.jpg").unwrap();
        assert!(matches!(r, ConvertRule::FileSpecific { .. }));
    }

    #[test]
    fn test_parse_invalid_no_arrow() {
        assert!(ConvertRule::parse("jpg png").is_err());
    }

    #[test]
    fn test_parse_invalid_empty_sides() {
        assert!(ConvertRule::parse("-> png").is_err());
        assert!(ConvertRule::parse("jpg ->").is_err());
    }

    #[test]
    fn test_resolve_ext_wide() {
        let r = rules(&["jpg -> png"]);
        assert_eq!(r.resolve("icons/arrow.jpg", "jpg"), Some("png"));
        assert_eq!(r.resolve("arrow.jpg", "jpg"), Some("png"));
        assert_eq!(r.resolve("arrow.png", "png"), None);
    }

    #[test]
    fn test_resolve_full_path_beats_ext_wide() {
        let r = rules(&["jpg -> png", "icons/sword.jpg -> icons/sword.bmp"]);
        assert_eq!(r.resolve("icons/sword.jpg", "jpg"), Some("icons/sword.bmp"));
        assert_eq!(r.resolve("icons/arrow.jpg", "jpg"), Some("png"));
    }

    #[test]
    fn test_resolve_filename_applies_to_all_folders() {
        let r = rules(&["sword.png -> sword.jpg"]);
        assert_eq!(r.resolve("ui/sword.png", "png"), Some("sword.jpg"));
        assert_eq!(r.resolve("weapons/sword.png", "png"), Some("sword.jpg"));
        assert_eq!(r.resolve("shield.png", "png"), None);
    }

    #[test]
    fn test_resolve_svg_default() {
        let r = rules(&[]);
        assert_eq!(r.resolve("icon.svg", "svg"), Some("png"));
    }

    #[test]
    fn test_resolve_svg_override_ext_wide() {
        let r = rules(&["svg -> jpg"]);
        assert_eq!(r.resolve("icon.svg", "svg"), Some("jpg"));
    }

    #[test]
    fn test_resolve_svg_override_file_specific() {
        let r = rules(&["svg -> bmp", "logo.svg -> logo.tga"]);
        assert_eq!(r.resolve("any/logo.svg", "svg"), Some("logo.tga"));
        assert_eq!(r.resolve("icon.svg", "svg"), Some("bmp"));
    }

    #[test]
    fn test_resolve_full_priority_chain() {
        let r = rules(&[
            "svg -> bmp",
            "icon.svg -> icon.tga",
            "ui/icon.svg -> ui/icon.jpg",
        ]);
        assert_eq!(r.resolve("ui/icon.svg", "svg"), Some("ui/icon.jpg"));
        assert_eq!(r.resolve("other/icon.svg", "svg"), Some("icon.tga"));
        assert_eq!(r.resolve("logo.svg", "svg"), Some("bmp"));
    }

    #[test]
    fn test_convert_png_roundtrip() {
        let img = RgbaImage::new(4, 4);
        let bytes = convert_image(&img, ImageFormat::Png).unwrap();
        assert!(!bytes.is_empty());
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width(), 4);
    }

    #[test]
    fn test_convert_jpg_flattens_alpha() {
        let img = RgbaImage::new(4, 4);
        let bytes = convert_image(&img, ImageFormat::Jpg).unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_image_format_from_str() {
        assert!(matches!(image_format_from_str("png"), Ok(ImageFormat::Png)));
        assert!(matches!(image_format_from_str("JPG"), Ok(ImageFormat::Jpg)));
        assert!(image_format_from_str("xyz").is_err());
    }
}
