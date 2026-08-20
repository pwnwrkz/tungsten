use anyhow::{Context, Result};
use image::RgbaImage;
use resvg;
use tiny_skia;
use usvg;

use super::super::asset::ImageFormat;

// SVG rasterization

/// Rasterize an SVG file into a straight-alpha RGBA image.
///
/// `scale` controls output resolution: `1.0` renders at the SVG's natural size,
/// `2.0` doubles it, etc.
pub fn svg_to_rgba(data: &[u8], scale: f32) -> Result<RgbaImage> {
    let scale = scale.max(0.01);

    let opt = usvg::Options {
        style_sheet: Some("svg { color: white; }".to_string()),
        ..Default::default()
    };
    // Inject a CSS stylesheet to ensure currentColor defaults to white instead of black.
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

    // resvg renders into a premultiplied-alpha pixmap; convert to the
    // straight (non-premultiplied) alpha the `image` crate expects.
    // `take_demultiplied` consumes the pixmap and un-premultiplies in place,
    // avoiding a second RGBA allocation and per-pixel loop. It also skips the
    // PNG encode/decode roundtrip of every rasterized SVG.
    let data = pixmap.take_demultiplied();
    RgbaImage::from_raw(width, height, data).context("Failed to build RGBA image from pixmap")
}

// Image format conversion

/// Encode an `RgbaImage` into the given target format.
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

/// Decode raw image bytes and re-encode to `target` format.
pub fn transcode_image(data: &[u8], target: ImageFormat) -> Result<Vec<u8>> {
    let image = image::load_from_memory(data)
        .context("Failed to decode source image")?
        .into_rgba8();
    convert_image(&image, target)
}

/// Returns `true` if the given file extension is accepted directly by libcaesium
/// (gif, jpg/jpeg, png, tiff/tif, webp). BMP and TGA are not supported and must
/// be normalized to PNG before compression.
pub fn is_caesium_compatible(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "gif" | "jpg" | "jpeg" | "png" | "tiff" | "tif" | "webp"
    )
}

/// Ensure image bytes are in a format libcaesium can compress.
///
/// - If `src_ext` is already caesium-compatible, returns the original `data` unchanged.
/// - Otherwise (BMP, TGA, and anything else) decodes and re-encodes to PNG.
///
/// The returned tuple is `(bytes, effective_extension)` where `effective_extension`
/// is either `src_ext` (unchanged) or `"png"` (converted).
pub fn normalize_for_compression(data: Vec<u8>, src_ext: &str) -> Result<(Vec<u8>, &str)>
where
{
    if is_caesium_compatible(src_ext) {
        return Ok((data, src_ext));
    }

    let png_bytes = transcode_image(&data, ImageFormat::Png)
        .with_context(|| format!("Failed to convert .{} to PNG for compression", src_ext))?;

    Ok((png_bytes, "png"))
}

/// Composite RGBA onto white, producing an RGB image (for JPEG output).
fn flatten_alpha(src: &RgbaImage) -> image::RgbImage {
    let (w, h) = src.dimensions();
    let mut out = image::RgbImage::new(w, h);
    // Use integer alpha compositing to avoid per-pixel float division.
    // Formula: out = (src * a + 255 * (255 - a)) / 255
    let src_raw = src.as_raw();
    let dst_raw = out.as_mut();
    for (src_chunk, dst_chunk) in src_raw.chunks_exact(4).zip(dst_raw.chunks_exact_mut(3)) {
        let a = src_chunk[3] as u16;
        let inv_a = 255 - a;
        // Integer division by 255 is ~20-30 cycles on x86; the exact bitwise
        // approximation `(t + 1 + (t >> 8)) >> 8` is ~3 cycles and produces
        // identical results for the u16 range used here (max 255*255 = 65025).
        let blend = |c: u8| -> u8 {
            let t = c as u16 * a + 255 * inv_a;
            ((t + 1 + (t >> 8)) >> 8) as u8
        };
        dst_chunk[0] = blend(src_chunk[0]);
        dst_chunk[1] = blend(src_chunk[1]);
        dst_chunk[2] = blend(src_chunk[2]);
    }
    out
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_is_caesium_compatible() {
        assert!(is_caesium_compatible("png"));
        assert!(is_caesium_compatible("PNG"));
        assert!(is_caesium_compatible("jpg"));
        assert!(is_caesium_compatible("jpeg"));
        assert!(is_caesium_compatible("gif"));
        assert!(is_caesium_compatible("tiff"));
        assert!(is_caesium_compatible("tif"));
        assert!(is_caesium_compatible("webp"));
        assert!(!is_caesium_compatible("bmp"));
        assert!(!is_caesium_compatible("tga"));
        assert!(!is_caesium_compatible("svg"));
    }

    #[test]
    fn test_normalize_compatible_ext_passthrough() {
        let data = vec![1u8, 2, 3];
        let (out, ext) = normalize_for_compression(data.clone(), "png").unwrap();
        assert_eq!(out, data);
        assert_eq!(ext, "png");
    }

    #[test]
    fn test_normalize_bmp_converts_to_png() {
        // Encode a small BMP, then normalize it — should come back as valid PNG.
        let img = RgbaImage::new(2, 2);
        let bmp_bytes = convert_image(&img, ImageFormat::Bmp).unwrap();
        let (png_bytes, ext) = normalize_for_compression(bmp_bytes, "bmp").unwrap();
        assert_eq!(ext, "png");
        let decoded = image::load_from_memory(&png_bytes).unwrap();
        assert_eq!(decoded.width(), 2);
    }

    #[test]
    fn flatten_alpha_matches_exact_integer_division() {
        // The bitwise `(t + 1 + (t >> 8)) >> 8` approximation must equal the
        // exact `t / 255` for every (color, alpha) combination in range.
        let mut img = RgbaImage::new(1, 1);
        for c in 0..=255u8 {
            for a in 0..=255u8 {
                img.put_pixel(0, 0, image::Rgba([c, c, c, a]));
                let flat = flatten_alpha(&img);
                let exact = (c as u16 * a as u16 + 255u16 * (255 - a as u16)) / 255;
                for ch in flat.as_raw().iter() {
                    assert_eq!(*ch, exact as u8, "c={} a={}", c, a);
                }
            }
        }
    }

    #[test]
    fn test_svg_to_rgba_rasterizes_at_scale() {
        // currentColor defaults to white via the injected stylesheet.
        let svg_data = r#"
            <svg width="10" height="10" xmlns="http://www.w3.org/2000/svg">
                <circle cx="5" cy="5" r="4" fill="currentColor"/>
            </svg>
        "#
        .as_bytes();

        let img = svg_to_rgba(svg_data, 2.0).expect("Failed to rasterize SVG");
        assert_eq!(img.width(), 20);
        assert_eq!(img.height(), 20);

        // A rasterized opaque circle must have opaque pixels (straight alpha).
        assert!(img.pixels().any(|p| p[3] == 255));
    }
}
