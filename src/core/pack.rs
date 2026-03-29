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

/// Pack images into as many 1024 × 1024 spritesheets as needed.
pub fn pack(images: Vec<InputImage>) -> Result<Vec<Spritesheet>> {
    const SHEET_SIZE: usize = 1024;

    let mut sheets: Vec<Spritesheet> = Vec::new();
    let mut remaining = images;

    while !remaining.is_empty() {
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
            match crunch::pack(crunch::Rect::of_size(SHEET_SIZE, SHEET_SIZE), items) {
                Ok(p) => (p, true),
                Err(p) => (p, false),
            };

        // Track which names made it onto this sheet so we can find leftovers.
        let packed_names: std::collections::HashSet<&str> =
            packed.iter().map(|p| p.data.name.as_str()).collect();

        // Composite the sheet image.
        let mut sheet_image: RgbaImage =
            ImageBuffer::from_pixel(SHEET_SIZE as u32, SHEET_SIZE as u32, image::Rgba([0, 0, 0, 0]));

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
        assert_eq!(result[0].image.width(), 1024);
        assert_eq!(result[0].image.height(), 1024);
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
