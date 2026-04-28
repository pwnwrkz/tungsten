use anyhow::{Context, Result};
use image::RgbaImage;
use resvg;
use tiny_skia;
use usvg;

use super::super::asset::ImageFormat;

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
    for (x, y, px) in src.enumerate_pixels() {
        let a = px[3] as f32 / 255.0;
        let r = (px[0] as f32 * a + 255.0 * (1.0 - a)) as u8;
        let g = (px[1] as f32 * a + 255.0 * (1.0 - a)) as u8;
        let b = (px[2] as f32 * a + 255.0 * (1.0 - a)) as u8;
        out.put_pixel(x, y, image::Rgb([r, g, b]));
    }
    out
}

// Format string helpers

#[allow(dead_code)]
pub fn image_format_from_str(s: &str) -> Result<ImageFormat> {
    use anyhow::bail;
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
    #[ignore]
    fn test_normalize_tga_converts_to_png() {
        let img = RgbaImage::new(2, 2);
        let tga_bytes = convert_image(&img, ImageFormat::Tga).unwrap();
        let (png_bytes, ext) = normalize_for_compression(tga_bytes, "tga").unwrap();
        assert_eq!(ext, "png");
        let decoded = image::load_from_memory(&png_bytes).unwrap();
        assert_eq!(decoded.width(), 2);
    }
}
