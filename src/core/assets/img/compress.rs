//! Image compression via libcaesium.
//!
//! caesium operates on file paths, so in-memory buffers are written to a
//! temporary file, compressed to a second temp file, then read back.
//! Both temp files are cleaned up automatically on drop.

use anyhow::{Context, Result};
use caesium::parameters::CSParameters;

/// Quality settings per format. All fields are optional — `None` keeps the
/// caesium default for that format.
#[derive(Debug, Clone)]
pub struct CompressOptions {
    /// JPEG quality 1–100. Defaults to 80.
    pub jpeg_quality: u32,
    /// PNG optimization level 1–6. Defaults to 3.
    pub png_quality: u32,
    /// WebP quality 1–100. Defaults to 80.
    pub webp_quality: u32,
    /// GIF optimization. Defaults to true.
    pub optimize_gif: bool,
    /// Preserve EXIF/XMP/ICC metadata. Defaults to false.
    pub keep_metadata: bool,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            jpeg_quality: 80,
            png_quality: 3,
            webp_quality: 80,
            optimize_gif: true,
            keep_metadata: true,
        }
    }
}

/// Compress `data` (already in a caesium-compatible format) and return the
/// compressed bytes.
///
/// `ext` must be one of: `png`, `jpg`/`jpeg`, `gif`, `tiff`/`tif`, `webp`.
/// For unsupported formats call `convert::normalize_for_compression` first.
pub fn compress_image(data: &[u8], ext: &str, options: &CompressOptions) -> Result<Vec<u8>> {
    use std::io::Write;

    // Write input to a named temp file with the correct extension so caesium
    // can detect the format.
    let mut input_tmp = tempfile::Builder::new()
        .suffix(&format!(".{}", ext.to_ascii_lowercase()))
        .tempfile()
        .context("Failed to create input temp file for compression")?;

    input_tmp
        .write_all(data)
        .context("Failed to write image data to temp file")?;

    input_tmp
        .flush()
        .context("Failed to flush input temp file")?;

    // Output temp file — same extension, caesium writes to it.
    let output_tmp = tempfile::Builder::new()
        .suffix(&format!(".{}", ext.to_ascii_lowercase()))
        .tempfile()
        .context("Failed to create output temp file for compression")?;

    let input_path = input_tmp
        .path()
        .to_str()
        .context("Input temp path is not valid UTF-8")?
        .to_string();

    let output_path = output_tmp
        .path()
        .to_str()
        .context("Output temp path is not valid UTF-8")?
        .to_string();

    let params = build_params(options);

    caesium::compress(input_path, output_path.clone(), &params)
        .map_err(|e| anyhow::anyhow!("Compression failed: {:?}", e))?;

    let compressed = std::fs::read(&output_path).context("Failed to read compressed output")?;

    // Only return the compressed result if it's actually smaller.
    // caesium can sometimes produce a larger file for already-optimized inputs.
    if compressed.len() < data.len() {
        Ok(compressed)
    } else {
        Ok(data.to_vec())
    }
}

fn build_params(options: &CompressOptions) -> CSParameters {
    let mut params = CSParameters::new();

    params.keep_metadata = options.keep_metadata;
    params.jpeg.quality = options.jpeg_quality;
    params.png.quality = options.png_quality;
    params.webp.quality = options.webp_quality;
    params.gif.quality = options.optimize_gif as u32;

    params
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_png(w: u32, h: u32) -> Vec<u8> {
        use image::{ImageEncoder, RgbaImage};
        let img = RgbaImage::from_pixel(w, h, image::Rgba([128, 64, 32, 255]));
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut buf))
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .unwrap();
        buf
    }

    #[test]
    fn test_compress_png_returns_valid_image() {
        let png = solid_png(64, 64);
        let opts = CompressOptions::default();
        let result = compress_image(&png, "png", &opts).unwrap();
        assert!(!result.is_empty());
        let decoded = image::load_from_memory(&result).unwrap();
        assert_eq!(decoded.width(), 64);
        assert_eq!(decoded.height(), 64);
    }

    #[test]
    fn test_compress_never_returns_empty() {
        let png = solid_png(8, 8);
        let opts = CompressOptions::default();
        let result = compress_image(&png, "png", &opts).unwrap();
        assert!(!result.is_empty());
    }

    #[test]
    fn test_compress_does_not_enlarge() {
        let png = solid_png(64, 64);
        let original_len = png.len();
        let opts = CompressOptions::default();
        let result = compress_image(&png, "png", &opts).unwrap();
        assert!(
            result.len() <= original_len,
            "compressed ({}) should not exceed original ({})",
            result.len(),
            original_len
        );
    }
}
