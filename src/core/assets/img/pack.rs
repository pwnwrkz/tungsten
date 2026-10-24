//! Spritesheet packing: bin-packing, uniform cell layouts, sheet compositing.

use anyhow::{Context, Result, bail};
use image::{GenericImage, ImageBuffer, RgbaImage, imageops};
use std::path::PathBuf;

use crunch::{Item, Rotation};
use rayon::prelude::*;

use crate::core::assets::asset::stem_name;
use crate::log;

/// Maximum width/height of a single spritesheet atlas.
const MAX_SHEET_SIZE: usize = 1024;

// Types

#[derive(Clone)]
pub struct InputImage {
    pub name: String,
    pub image: RgbaImage,
    /// sha256 of the raw source file bytes, for lockfile cache keys.
    pub source_hash: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct PackedImage {
    pub name: String,
    /// sha256 of the raw source file bytes, carried through packing.
    pub source_hash: String,
    pub sheet_index: usize,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone)]
pub struct Spritesheet {
    pub image: RgbaImage,
    pub images: Vec<PackedImage>,
}

// Public API

/// Load every PNG at the given paths in parallel.
/// Names are relative to `base_path` with the extension stripped.
/// Uses chunked iteration to bound peak memory usage when decoding many images.
pub fn load_images(paths: Vec<PathBuf>, base_path: &str) -> Result<Vec<InputImage>> {
    const CHUNK_SIZE: usize = 10;
    // Decode in bounded chunks so only CHUNK_SIZE images are ever decoded
    // concurrently, bounding peak memory on large projects. Errors are
    // collected per-chunk and the first failure is propagated after the
    // parallel pass finishes.
    let chunks: Vec<Result<Vec<InputImage>>> = paths
        .into_par_iter()
        .chunks(CHUNK_SIZE)
        .map(|chunk| {
            chunk
                .into_iter()
                .map(|path| {
                    let image = image::open(&path)
                        .with_context(|| format!("Failed to open image \"{}\"", path.display()))?
                        .into_rgba8();

                    let rel = path
                        .strip_prefix(base_path)
                        .unwrap_or(&path)
                        .to_string_lossy();
                    let name = stem_name(&rel);
                    let source_hash =
                        crate::core::postsync::lockfile::hash_bytes(&std::fs::read(&path)?);

                    Ok(InputImage {
                        name,
                        image,
                        source_hash,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect();

    let mut out = Vec::new();
    for chunk in chunks {
        out.extend(chunk?);
    }
    Ok(out)
}

/// Pack images into spritesheets with automatic sizing similar to Adobe Animate.
///
/// Packs sprites into the smallest possible atlas(es) not exceeding 1024x1024,
/// automatically generating additional atlases when necessary, and trimming
/// final atlases to used space.
pub fn pack(mut images: Vec<InputImage>) -> Result<Vec<Spritesheet>> {
    // Sort by largest height first, then largest width first for better packing efficiency
    images.sort_by(|a, b| {
        b.image
            .height()
            .cmp(&a.image.height())
            .then_with(|| b.image.width().cmp(&a.image.width()))
    });

    let mut sheets: Vec<Spritesheet> = Vec::new();
    let mut remaining = images;

    while !remaining.is_empty() {
        // Use maximum sheet size to pack as many images as possible per sheet
        let sheet_size = MAX_SHEET_SIZE;

        let items: Vec<Item<InputImage>> = remaining
            .iter()
            .map(|img| {
                Item::new(
                    img.clone(),
                    img.image.width() as usize,
                    img.image.height() as usize,
                    Rotation::None,
                )
            })
            .collect();

        let (packed, all_fit) =
            match crunch::pack(crunch::Rect::of_size(sheet_size, sheet_size), items) {
                Ok(p) => (p, true),
                Err(p) => (p, false),
            };

        // Track which names made it onto this sheet so we can find leftovers.
        let packed_names: std::collections::HashSet<&str> =
            packed.iter().map(|p| p.data.name.as_str()).collect();

        // Calculate actual used space on this sheet to trim empty space
        let (used_width, used_height) = calculate_used_space(&packed);

        // Composite the sheet image with actual used dimensions (trimmed to used space)
        let mut sheet_image: RgbaImage = ImageBuffer::from_pixel(
            used_width as u32,
            used_height as u32,
            image::Rgba([0, 0, 0, 0]),
        );

        let sheet_index = sheets.len();
        let mut packed_images = Vec::with_capacity(packed.len());

        for crunch::PackedItem { data: img, rect } in &packed {
            let (x, y) = (rect.x as u32, rect.y as u32);
            sheet_image
                .copy_from(&img.image, x, y)
                .with_context(|| format!("Failed to copy \"{}\" onto spritesheet", img.name))?;

            packed_images.push(PackedImage {
                name: img.name.clone(),
                source_hash: img.source_hash.clone(),
                sheet_index,
                x,
                y,
                width: img.image.width(),
                height: img.image.height(),
            });
        }

        sheets.push(Spritesheet {
            image: sheet_image,
            images: packed_images,
        });

        if all_fit {
            break;
        }

        // Keep only the images that didn't make it onto this sheet.
        remaining.retain(|img| !packed_names.contains(img.name.as_str()));
        log!(
            warn,
            "{} image(s) didn't fit, packing into another spritesheet...",
            remaining.len()
        );
    }

    Ok(sheets)
}

/// How a sprite is resampled when its source dimensions differ from the cell size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellFit {
    /// Scale to fit inside the cell preserving aspect ratio, centered on
    /// transparent padding.
    Contain,
    /// Scale to fill the cell preserving aspect ratio, cropping overflow.
    Cover,
    /// Scale to the exact cell dimensions, ignoring aspect ratio.
    Stretch,
}

impl CellFit {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "contain" => Ok(CellFit::Contain),
            "cover" => Ok(CellFit::Cover),
            "stretch" => Ok(CellFit::Stretch),
            other => bail!(
                "Unknown cell_fit \"{}\"\n  Hint: Valid values are contain, cover, stretch",
                other
            ),
        }
    }
}

/// Options for uniform-cell packing.
pub struct UniformOptions {
    /// Exact cell dimensions. When `None`, the cell is sized to the largest
    /// sprite width and height across the input.
    pub cell_size: Option<(u32, u32)>,
    /// Resampling policy for sprites that don't already match the cell size.
    pub fit: CellFit,
}

/// Pack images into spritesheets on a uniform cell grid.
///
/// Every sprite is resampled to one exact cell size, then placed in
/// name-sorted order so sprite `k` always sits at column `k % columns`,
/// row `k / columns` of its sheet. This makes sprite positions and render
/// dimensions a tool guarantee instead of an art-pipeline convention.
pub fn pack_uniform(
    mut images: Vec<InputImage>,
    options: &UniformOptions,
) -> Result<Vec<Spritesheet>> {
    // Sort by name so grid indices are deterministic across runs.
    images.sort_by(|a, b| a.name.cmp(&b.name));

    let (cell_w, cell_h) = options.cell_size.unwrap_or_else(|| {
        let w = images.iter().map(|i| i.image.width()).max().unwrap_or(1);
        let h = images.iter().map(|i| i.image.height()).max().unwrap_or(1);
        (w, h)
    });

    if cell_w == 0 || cell_h == 0 {
        bail!("cell_size must be at least 1x1");
    }
    if cell_w as usize > MAX_SHEET_SIZE || cell_h as usize > MAX_SHEET_SIZE {
        bail!(
            "cell_size {}x{} exceeds the maximum spritesheet size of {}x{}",
            cell_w,
            cell_h,
            MAX_SHEET_SIZE,
            MAX_SHEET_SIZE
        );
    }

    let columns = MAX_SHEET_SIZE / cell_w as usize;
    let rows = MAX_SHEET_SIZE / cell_h as usize;
    let per_sheet = columns * rows;

    let resampled_count = images
        .iter()
        .filter(|i| i.image.dimensions() != (cell_w, cell_h))
        .count();
    if resampled_count > 0 {
        log!(
            info,
            "Uniform cells: resampling {} sprite(s) to {}x{}",
            resampled_count,
            cell_w,
            cell_h
        );
    }

    // Resample in parallel; collect() preserves the sorted order.
    let cells: Vec<InputImage> = images
        .into_par_iter()
        .map(|img| {
            let (w, h) = img.image.dimensions();
            if (w, h) != (cell_w, cell_h) {
                log!(
                    debug,
                    "Resampling \"{}\" from {}x{} to {}x{}",
                    img.name,
                    w,
                    h,
                    cell_w,
                    cell_h
                );
            }
            InputImage {
                image: resample_to_cell(&img.image, cell_w, cell_h, options.fit),
                name: img.name,
                source_hash: img.source_hash,
            }
        })
        .collect();

    let mut sheets: Vec<Spritesheet> = Vec::new();

    for (sheet_index, chunk) in cells.chunks(per_sheet.max(1)).enumerate() {
        let cols_used = chunk.len().min(columns);
        let rows_used = chunk.len().div_ceil(columns);
        let mut sheet_image: RgbaImage = ImageBuffer::from_pixel(
            (cols_used as u32) * cell_w,
            (rows_used as u32) * cell_h,
            image::Rgba([0, 0, 0, 0]),
        );

        let mut packed_images = Vec::with_capacity(chunk.len());
        for (k, img) in chunk.iter().enumerate() {
            let x = (k % columns) as u32 * cell_w;
            let y = (k / columns) as u32 * cell_h;
            sheet_image
                .copy_from(&img.image, x, y)
                .with_context(|| format!("Failed to copy \"{}\" onto spritesheet", img.name))?;
            packed_images.push(PackedImage {
                name: img.name.clone(),
                source_hash: img.source_hash.clone(),
                sheet_index,
                x,
                y,
                width: cell_w,
                height: cell_h,
            });
        }

        sheets.push(Spritesheet {
            image: sheet_image,
            images: packed_images,
        });
    }

    if sheets.len() > 1 {
        log!(
            warn,
            "Uniform grid exceeded one sheet, packed into {} spritesheets",
            sheets.len()
        );
    }

    Ok(sheets)
}

/// Resample `img` to exactly `cell_w`x`cell_h` using the given fit policy.
fn resample_to_cell(img: &RgbaImage, cell_w: u32, cell_h: u32, fit: CellFit) -> RgbaImage {
    let (w, h) = img.dimensions();
    if (w, h) == (cell_w, cell_h) {
        return img.clone();
    }

    match fit {
        CellFit::Stretch => imageops::resize(img, cell_w, cell_h, imageops::FilterType::Lanczos3),
        CellFit::Contain => {
            let scale = (cell_w as f64 / w as f64).min(cell_h as f64 / h as f64);
            let tw = ((w as f64 * scale).round() as u32).clamp(1, cell_w);
            let th = ((h as f64 * scale).round() as u32).clamp(1, cell_h);
            let resized = imageops::resize(img, tw, th, imageops::FilterType::Lanczos3);
            let mut cell: RgbaImage =
                ImageBuffer::from_pixel(cell_w, cell_h, image::Rgba([0, 0, 0, 0]));
            imageops::overlay(
                &mut cell,
                &resized,
                ((cell_w - tw) / 2) as i64,
                ((cell_h - th) / 2) as i64,
            );
            cell
        }
        CellFit::Cover => {
            let scale = (cell_w as f64 / w as f64).max(cell_h as f64 / h as f64);
            let tw = ((w as f64 * scale).round() as u32).max(cell_w);
            let th = ((h as f64 * scale).round() as u32).max(cell_h);
            let resized = imageops::resize(img, tw, th, imageops::FilterType::Lanczos3);
            imageops::crop_imm(
                &resized,
                (tw - cell_w) / 2,
                (th - cell_h) / 2,
                cell_w,
                cell_h,
            )
            .to_image()
        }
    }
}

/// Calculate the actual used width and height of a packed sheet.
fn calculate_used_space(packed: &[crunch::PackedItem<InputImage>]) -> (usize, usize) {
    let mut max_x = 0;
    let mut max_y = 0;

    for item in packed {
        let right = item.rect.x + item.rect.w;
        let bottom = item.rect.y + item.rect.h;
        if right > max_x {
            max_x = right;
        }
        if bottom > max_y {
            max_y = bottom;
        }
    }

    (max_x, max_y)
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pack_single_image() {
        let inputs = vec![InputImage {
            name: "test-icon".to_string(),
            image: RgbaImage::new(48, 48),
            source_hash: String::new(),
        }];

        let result = pack(inputs).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].images.len(), 1);
        assert_eq!(result[0].images[0].name, "test-icon");
        // With dynamic sizing and trimming, a single 48x48 image should fit in a 48x48 sheet
        assert_eq!(result[0].image.width(), 48);
        assert_eq!(result[0].image.height(), 48);
    }

    #[test]
    fn test_pack_preserves_dimensions() {
        let inputs = vec![InputImage {
            name: "test-icon".to_string(),
            image: RgbaImage::new(48, 48),
            source_hash: String::new(),
        }];

        let result = pack(inputs).unwrap();
        assert_eq!(result[0].images[0].width, 48);
        assert_eq!(result[0].images[0].height, 48);
    }

    // Uniform cell packing

    fn named(name: &str, w: u32, h: u32) -> InputImage {
        InputImage {
            name: name.to_string(),
            image: RgbaImage::new(w, h),
            source_hash: String::new(),
        }
    }

    #[test]
    fn test_cell_fit_parse() {
        assert_eq!(CellFit::parse("contain").unwrap(), CellFit::Contain);
        assert_eq!(CellFit::parse("COVER").unwrap(), CellFit::Cover);
        assert_eq!(CellFit::parse("stretch").unwrap(), CellFit::Stretch);
        assert!(CellFit::parse("fill").is_err());
    }

    #[test]
    fn test_pack_uniform_grid_positions_sorted_by_name() {
        let inputs = vec![named("c", 48, 48), named("a", 48, 48), named("b", 48, 48)];

        let result = pack_uniform(
            inputs,
            &UniformOptions {
                cell_size: None,
                fit: CellFit::Contain,
            },
        )
        .unwrap();

        assert_eq!(result.len(), 1);
        let sheet = &result[0];
        assert_eq!(sheet.images.len(), 3);
        // Sorted by name, sprite k sits at (k % columns, k / columns).
        assert_eq!(sheet.images[0].name, "a");
        assert_eq!((sheet.images[0].x, sheet.images[0].y), (0, 0));
        assert_eq!(sheet.images[1].name, "b");
        assert_eq!((sheet.images[1].x, sheet.images[1].y), (48, 0));
        assert_eq!(sheet.images[2].name, "c");
        assert_eq!((sheet.images[2].x, sheet.images[2].y), (96, 0));
        // Sheet trimmed to used cells.
        assert_eq!(sheet.image.width(), 144);
        assert_eq!(sheet.image.height(), 48);
    }

    #[test]
    fn test_pack_uniform_resamples_to_declared_cell() {
        // A stray 200x200 export must come out as the declared 128x128 cell.
        let inputs = vec![named("icon", 200, 200), named("small", 64, 64)];

        let result = pack_uniform(
            inputs,
            &UniformOptions {
                cell_size: Some((128, 128)),
                fit: CellFit::Contain,
            },
        )
        .unwrap();

        for img in &result[0].images {
            assert_eq!(img.width, 128);
            assert_eq!(img.height, 128);
        }
    }

    #[test]
    fn test_pack_uniform_auto_cell_uses_max_dimensions() {
        let inputs = vec![named("a", 32, 64), named("b", 64, 32)];

        let result = pack_uniform(
            inputs,
            &UniformOptions {
                cell_size: None,
                fit: CellFit::Contain,
            },
        )
        .unwrap();

        assert_eq!(result[0].images[0].width, 64);
        assert_eq!(result[0].images[0].height, 64);
    }

    #[test]
    fn test_pack_uniform_overflows_to_second_sheet() {
        // 512x512 cells: 2x2 grid per sheet, so 5 sprites need two sheets.
        let inputs = (0..5)
            .map(|i| named(&format!("sprite_{}", i), 512, 512))
            .collect();

        let result = pack_uniform(
            inputs,
            &UniformOptions {
                cell_size: Some((512, 512)),
                fit: CellFit::Contain,
            },
        )
        .unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].images.len(), 4);
        assert_eq!(result[1].images.len(), 1);
        assert_eq!((result[1].images[0].x, result[1].images[0].y), (0, 0));
        assert_eq!(result[1].images[0].sheet_index, 1);
    }

    #[test]
    fn test_pack_uniform_rejects_oversized_cell() {
        let inputs = vec![named("a", 32, 32)];
        let result = pack_uniform(
            inputs,
            &UniformOptions {
                cell_size: Some((2048, 128)),
                fit: CellFit::Contain,
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_resample_contain_pads_transparent() {
        // 100x50 into a 100x100 cell: contain keeps aspect, pads vertically.
        let mut img = RgbaImage::new(100, 50);
        for px in img.pixels_mut() {
            *px = image::Rgba([255, 0, 0, 255]);
        }
        let cell = resample_to_cell(&img, 100, 100, CellFit::Contain);
        assert_eq!(cell.dimensions(), (100, 100));
        assert_eq!(cell.get_pixel(50, 0)[3], 0, "top padding is transparent");
        assert_eq!(cell.get_pixel(50, 50)[3], 255, "center is opaque");
    }

    #[test]
    fn test_resample_cover_and_stretch_fill_cell() {
        let img = RgbaImage::new(100, 50);
        assert_eq!(
            resample_to_cell(&img, 64, 64, CellFit::Cover).dimensions(),
            (64, 64)
        );
        assert_eq!(
            resample_to_cell(&img, 64, 64, CellFit::Stretch).dimensions(),
            (64, 64)
        );
    }
}
