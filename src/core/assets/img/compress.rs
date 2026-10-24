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
            // Matches the config-level default in `config::CompressConfig::resolve`.
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
    // `caesium::compress_in_memory` consumes an owned `Vec<u8>`, so we clone
    // here. The clone is unavoidable because callers need the original buffer
    // back when compression offers no size improvement (the `Ok(None)` fallback
    // path). See OPTIMIZATION.md S6 for the trade-off analysis.
    let compressed = caesium::compress_in_memory(data.to_vec(), &params)
        .map_err(|e| anyhow::anyhow!("Compression failed: {:?}", e))?;

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

    // --- Engine benchmark: caesium vs oxipng (PNG).
    // JPEG (caesium/mozjpeg vs turbojpeg) can't be built here (no cmake), and
    // it only runs for raw JPEG passthroughs — PNG is the live pipeline path.
    // Run with: cargo test bench_compress_engines -- --nocapture

    fn make_test_pngs() -> Vec<(&'static str, Vec<u8>)> {
        use image::{ImageEncoder, Rgba, RgbaImage};
        let grad = RgbaImage::from_fn(512, 512, |x, y| {
            Rgba([(x * 255 / 511) as u8, (y * 255 / 511) as u8, 128, 255])
        });
        let noise = RgbaImage::from_fn(512, 512, |x, y| {
            let h = x
                .wrapping_mul(1664525)
                .wrapping_add(y)
                .wrapping_mul(1013904223)
                .wrapping_add(12345);
            Rgba([h as u8, (h >> 8) as u8, (h >> 16) as u8, 255])
        });
        let mixed = RgbaImage::from_fn(256, 256, |x, _y| {
            let base = (x * 255 / 255) as u8;
            let n = ((x.wrapping_mul(2654435761) >> 13) % 40) as u8;
            Rgba([base.wrapping_add(n), base, n, 255])
        });

        fn to_png(img: &RgbaImage) -> Vec<u8> {
            let mut buf = Vec::new();
            image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut buf))
                .write_image(
                    img.as_raw(),
                    img.width(),
                    img.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .unwrap();
            buf
        }

        vec![
            ("gradient-512", to_png(&grad)),
            ("noise-512", to_png(&noise)),
            ("mixed-256", to_png(&mixed)),
        ]
    }

    fn median(v: &mut [u128]) -> f64 {
        v.sort();
        let mid = v.len() / 2;
        v[mid] as f64 / 1_000_000.0 // ns -> ms
    }

    fn walk_pngs(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p.extension().is_some_and(|x| x == "png") {
                        out.push(p);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn bench_compress_engines() {
        let pngs = make_test_pngs();
        let opts = CompressOptions::default(); // q80
        let rounds = 5;

        println!("\n=== PNG (synthetic): caesium(q80) vs direct oxipng (same input) ===");
        for (name, data) in &pngs {
            let o3 = oxipng::Options::from_preset(3);
            let o6 = oxipng::Options::from_preset(6);
            compress_image(data, "png", &opts).unwrap(); // warmup
            let c_size = compress_image(data, "png", &opts)
                .unwrap()
                .map(|v| v.len())
                .unwrap_or(data.len());
            let s3 = oxipng::optimize_from_memory(data, &o3)
                .map(|v| v.len())
                .unwrap_or(data.len());
            let s6 = oxipng::optimize_from_memory(data, &o6)
                .map(|v| v.len())
                .unwrap_or(data.len());
            let (mut ct, mut t3, mut t6) = (vec![], vec![], vec![]);
            for _ in 0..rounds {
                let t = std::time::Instant::now();
                compress_image(data, "png", &opts).unwrap();
                ct.push(t.elapsed().as_nanos());
                let t = std::time::Instant::now();
                oxipng::optimize_from_memory(data, &o3).unwrap();
                t3.push(t.elapsed().as_nanos());
                let t = std::time::Instant::now();
                oxipng::optimize_from_memory(data, &o6).unwrap();
                t6.push(t.elapsed().as_nanos());
            }
            println!(
                "  {name}: in {}B | caesium {:7.2}ms ({c_size}B) | oxipng p3 {:7.2}ms ({s3}B) | oxipng p6 {:7.2}ms ({s6}B)",
                data.len(),
                median(&mut ct),
                median(&mut t3),
                median(&mut t6)
            );
        }

        // Real workload: the pipeline's own PNG output (what maybe_compress_png
        // actually sees) — all .png under tungsten-bench/.tungsten_debug.
        let bench_dir = std::env::var("TUNGSTEN_BENCH_DIR")
            .ok()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(
                    "/mnt/c/Users/magni/AppData/Local/Temp/opencode/tungsten-bench",
                )
            });
        let debug_dir = bench_dir.join(".tungsten_debug");
        if debug_dir.is_dir() {
            let mut files = walk_pngs(&debug_dir);
            files.sort();
            let o3 = oxipng::Options::from_preset(3);
            println!(
                "\n=== PNG (real workload, {} files): caesium(q80) vs oxipng p3 ===",
                files.len()
            );
            let mut c_total = 0.0f64;
            let mut o_total = 0.0f64;
            let mut c_size = 0usize;
            let mut o_size = 0usize;
            let mut in_size = 0usize;
            let mut improved = 0usize;
            for path in &files {
                let data = std::fs::read(path).expect("read png");
                let t = std::time::Instant::now();
                let r = compress_image(&data, "png", &opts).unwrap();
                c_total += t.elapsed().as_secs_f64() * 1000.0;
                c_size += r.as_ref().map(|v| v.len()).unwrap_or(data.len());
                let t = std::time::Instant::now();
                let r2 = oxipng::optimize_from_memory(&data, &o3);
                o_total += t.elapsed().as_secs_f64() * 1000.0;
                o_size += r2.as_ref().map(|v| v.len()).unwrap_or(data.len());
                in_size += data.len();
                if r.map(|v| v.len()).unwrap_or(data.len()) < data.len() {
                    improved += 1;
                }
            }
            println!(
                "  total in {}B -> caesium {:7.1}ms ({c_size}B) | oxipng p3 {:7.1}ms ({o_size}B) | improved-over-input: {improved}/{}",
                in_size,
                c_total,
                o_total,
                files.len()
            );

            // Full preset sweep on the same files (size vs time curve).
            println!("\n=== oxipng preset sweep (real workload) ===");
            let mut all: Vec<Vec<u8>> = Vec::new();
            for path in &files {
                all.push(std::fs::read(path).expect("read png"));
            }
            for level in 0..=6u8 {
                let o = oxipng::Options::from_preset(level);
                let mut total = 0.0f64;
                let mut size = 0usize;
                for data in &all {
                    let t = std::time::Instant::now();
                    size += oxipng::optimize_from_memory(data, &o)
                        .map(|v| v.len())
                        .unwrap_or(data.len());
                    total += t.elapsed().as_secs_f64() * 1000.0;
                }
                println!("  p{level}: {:7.1}ms  {}B", total, size);
            }
        }
        println!();
    }
}
