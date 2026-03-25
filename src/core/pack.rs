use anyhow::{Result, Context};
use image::{RgbaImage, ImageBuffer, GenericImage};
use std::path::PathBuf;
use crunch::{Item, Rotation};

#[derive(Clone)]
pub struct InputImage {
    pub name: String,
    pub image: RgbaImage,
}

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

pub fn load_images(paths: Vec<PathBuf>, base_path: &str) -> Result<Vec<InputImage>> {
    let mut images = Vec::new();

    for path in paths {
        let image = image::open(&path)
            .with_context(|| format!("Failed to open image \"{}\"", path.display()))?
            .into_rgba8();

        // Strip base path and extension to get relative name like "48/arrow-up"
        let name = path
            .strip_prefix(base_path)
            .unwrap_or(&path)
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/")
            .to_string();

        images.push(InputImage { name, image });
    }

    Ok(images)
}

pub fn pack(images: Vec<InputImage>) -> Result<Vec<Spritesheet>> {
    let mut sheets = Vec::new();
    let mut remaining = images;

    while !remaining.is_empty() {
        // Clone remaining so we can recover leftovers if needed
        let remaining_clone = remaining.clone();

        let items: Vec<Item<InputImage>> = std::mem::take(&mut remaining)
            .into_iter()
            .map(|img| {
                let w = img.image.width();
                let h = img.image.height();
                Item::new(img, w as usize, h as usize, Rotation::None)
            })
            .collect();

        let (packed, all_fit) = match crunch::pack(crunch::Rect::of_size(1024, 1024), items) {
            Ok(packed) => (packed, true),
            Err(packed) => (packed, false),
        };

        // Collect names of what was packed
        let packed_names: std::collections::HashSet<String> = packed
            .iter()
            .map(|p| p.data.name.clone())
            .collect();

        // Draw the spritesheet
        let mut sheet_image: RgbaImage = ImageBuffer::from_pixel(
            1024, 1024,
            image::Rgba([0, 0, 0, 0])
        );
        let mut packed_images = Vec::new();

        for crunch::PackedItem { data: img, rect } in packed {
            let x = rect.x as u32;
            let y = rect.y as u32;

            sheet_image
                .copy_from(&img.image, x, y)
                .with_context(|| format!("Failed to copy \"{}\" onto spritesheet", img.name))?;

            packed_images.push(PackedImage {
                name: img.name,
                sheet_index: sheets.len(),
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

        // If not everything fit, recover leftovers from our clone
        if !all_fit {
            remaining = remaining_clone
                .into_iter()
                .filter(|img| !packed_names.contains(&img.name))
                .collect();

            log!(warn, "{} image(s) didn't fit, packing into another spritesheet...", remaining.len());
        }
    }

    Ok(sheets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pack_single_image() {
        // Create a simple 48x48 test image in memory
        let test_image = image::RgbaImage::new(48, 48);
        
        let inputs = vec![InputImage {
            name: "test-icon".to_string(),
            image: test_image,
        }];

        let result = pack(inputs).unwrap();
        
        // Should produce exactly one spritesheet
        assert_eq!(result.len(), 1);
        
        // Should contain our image
        assert_eq!(result[0].images.len(), 1);
        assert_eq!(result[0].images[0].name, "test-icon");
        
        // Sheet should be 1024x1024
        assert_eq!(result[0].image.width(), 1024);
        assert_eq!(result[0].image.height(), 1024);
    }

    #[test]
    fn test_pack_preserves_dimensions() {
        let test_image = image::RgbaImage::new(48, 48);
        
        let inputs = vec![InputImage {
            name: "test-icon".to_string(),
            image: test_image,
        }];

        let result = pack(inputs).unwrap();
        
        assert_eq!(result[0].images[0].width, 48);
        assert_eq!(result[0].images[0].height, 48);
    }
}