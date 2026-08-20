//! Image compression via libcaesium.
//!
//! Compression runs fully in memory (`caesium::compress_in_memory`) — no
//! temporary files are written, avoiding the disk I/O bottleneck that
//! path-based compression would impose per image.

use crate::log;
use anyhow::Result;
use caesium::parameters::CSParameters;

/// Quality settings per format. All fields are optional — `None` keeps the
/// caesium default for that format.
#[derive(Debug, Clone)]
pub struct CompressOptions {
    /// JPEG quality 1–100. Defaults to 80.
    pub jpeg_quality: u32,
    /// PNG quality 1-100. Defaults to 80.
    pub png_quality: u32,
    /// Preserve EXIF/XMP/ICC metadata. Defaults to false.
    pub keep_metadata: bool,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            jpeg_quality: 80,
            png_quality: 80,
            // Matches the config-level default in `config::CompressOptions::resolve`
            // so there is a single source of truth for the defaults.
            keep_metadata: false,
        }
    }
}

/// Compress `data` (already in a caesium-compatible format) and return the
/// compressed bytes, or `None` if the compressed result would not be smaller.
///
/// `ext` must be one of: `png`, `jpg`/`jpeg`, `gif`, `tiff`/`tif`, `webp`.
/// For unsupported formats call `convert::normalize_for_compression` first.
///
/// Returns `Err` only on I/O or compression failure; `Ok(None)` means
/// compression succeeded but offered no space saving.
pub fn compress_image(
    data: &[u8],
    ext: &str,
    options: &CompressOptions,
) -> Result<Option<Vec<u8>>> {
    // `ext` is only used for format hints; the format itself is detected from
    // the buffer contents by `compress_in_memory`, so no temp file (with its
    // suffix) is needed.
    let _ = ext;

    let params = build_params(options);

    // Fully in-memory pipeline: no temp files, no extra disk I/O per image.
    let compressed = caesium::compress_in_memory(data.to_vec(), &params)
        .map_err(|e| anyhow::anyhow!("Compression failed: {:?}", e))?;

    // Only return the compressed result if it's actually smaller.
    // caesium can sometimes produce a larger file for already-optimized inputs.
    if compressed.len() < data.len() {
        Ok(Some(compressed))
    } else {
        Ok(None)
    }
}

/// Optionally compress PNG bytes before upload.
/// Returns the compressed bytes, or the original `bytes` if compression
/// fails or produces a larger output.
pub fn maybe_compress_png(bytes: Vec<u8>, compress_options: Option<&CompressOptions>) -> Vec<u8> {
    let Some(opts) = compress_options else {
        return bytes;
    };
    match compress_image(&bytes, "png", opts) {
        Ok(Some(compressed)) => compressed,
        Ok(None) => bytes,
        Err(e) => {
            log!(warn, "Compression failed, using original: {}", e);
            bytes
        }
    }
}

fn build_params(options: &CompressOptions) -> CSParameters {
    let mut params = CSParameters::new();

    params.keep_metadata = options.keep_metadata;
    params.jpeg.quality = options.jpeg_quality;
    params.png.quality = options.png_quality;

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
        let result = result.unwrap_or(png);
        assert!(!result.is_empty());
        let decoded = image::load_from_memory(&result).unwrap();
        assert_eq!(decoded.width(), 64);
        assert_eq!(decoded.height(), 64);
    }

    #[test]
    fn test_compress_never_returns_empty() {
        let png = solid_png(8, 8);
        let opts = CompressOptions::default();
        let result = compress_image(&png, "png", &opts).unwrap().unwrap_or(png);
        assert!(!result.is_empty());
    }

    #[test]
    fn test_compress_does_not_enlarge() {
        let png = solid_png(64, 64);
        let original_len = png.len();
        let opts = CompressOptions::default();
        // compress_image returns None when compressed >= original
        let result = compress_image(&png, "png", &opts)
            .unwrap()
            .unwrap_or_else(|| png.clone());
        assert!(
            result.len() <= original_len,
            "compressed ({}) should not exceed original ({})",
            result.len(),
            original_len
        );
    }
}
