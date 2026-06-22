use anyhow::{Context, Result};
use image::{GenericImage, ImageBuffer, RgbaImage};
use std::path::PathBuf;

use crunch::{Item, Rotation};
use rayon::prelude::*;

use crate::log;

// Types

#[derive(Clone)]
pub struct InputImage {
    pub name: String,
    pub image: RgbaImage,
}

#[allow(dead_code)]
pub struct PackedImage {
    pub name: String,
    pub sheet_index: usize,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub struct Spritesheet {
    pub image: RgbaImage,
    pub images: Vec<PackedImage>,
}

// Public API

/// Load every PNG at the given paths in parallel.
/// Names are relative to `base_path` with the extension stripped.
pub fn load_images(paths: Vec<PathBuf>, base_path: &str) -> Result<Vec<InputImage>> {
    paths
        .into_par_iter()
        .map(|path| {
            let image = image::open(&path)
                .with_context(|| format!("Failed to open image \"{}\"", path.display()))?
                .into_rgba8();

            let name = path
                .strip_prefix(base_path)
                .unwrap_or(&path)
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/");

            Ok(InputImage { name, image })
        })
        .collect()
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
        // Use maximum sheet size (1024x1024) to pack as many images as possible per sheet
        let sheet_size = 1024;

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

/// Calculate a fully dynamic spritesheet size based on the images to be packed.
///
/// This function analyzes the dimensions of the input images and returns a
/// sheet size that closely matches the actual space needed, similar to
/// Adobe Animate's approach of sizing based solely on sprite dimensions
/// without predefined size constraints.
// fn calculate_optimal_sheet_size(images: &[InputImage]) -> usize {
//     if images.is_empty() {
//         return 64; // Minimum reasonable size for a spritesheet
//     }

//     // Calculate total area needed by all images
//     let mut total_area: u64 = 0;
//     let mut max_width = 0u32;
//     let mut max_height = 0u32;

//     for img in images {
//         let width = img.image.width();
//         let height = img.image.height();
//         total_area += (width as u64) * (height as u64);
//         if width > max_width {
//             max_width = width;
//         }
//         if height > max_height {
//             max_height = height;
//         }
//     }

//     // Start with a sheet size that can fit the largest single image
//     let mut sheet_size = std::cmp::max(max_width, max_height) as usize;

//     // Ensure minimum size (64x64) to avoid excessively small sheets
//     sheet_size = sheet_size.max(64);

//     // If we have multiple images, try to size the sheet to accommodate them efficiently
//     if images.len() > 1 {
//         // Calculate approximate dimension needed based on total area
//         // We use 1.3 efficiency factor to account for packing inefficiency
//         // (slightly higher than before to better accommodate varied sizes)
//         let needed_width = ((total_area as f64 * 1.3).sqrt()).ceil() as usize;
//         sheet_size = sheet_size.max(needed_width);
//     }

//     // For very large numbers of small images, we might want to cap the size
//     // to prevent unreasonably large sheets, but keep it dynamic within bounds
//     let max_reasonable_size = 4096;
//     if sheet_size > max_reasonable_size {
//         // If we exceed the maximum, we'll use the maximum but this ideally
//         // should trigger multiple sheets in the packing algorithm
//         // For now, we'll cap it to prevent memory issues
//         sheet_size = max_reasonable_size;
//     }

//     sheet_size
// }

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pack_single_image() {
        let inputs = vec![InputImage {
            name: "test-icon".to_string(),
            image: RgbaImage::new(48, 48),
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
        }];

        let result = pack(inputs).unwrap();
        assert_eq!(result[0].images[0].width, 48);
        assert_eq!(result[0].images[0].height, 48);
    }
}
