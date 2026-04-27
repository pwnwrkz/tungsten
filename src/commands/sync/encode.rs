use std::collections::HashMap;

use anyhow::{Context, Result};
use image::RgbaImage;

use crate::core::assets::img::pack;
use crate::core::postsync::codegen::{parse_dpi_suffix, strip_dpi_suffix};

/// Maps a base asset name to its DPI variants: `(scale_factor, image)` pairs.
pub type DpiGroups = HashMap<String, Vec<(u8, pack::InputImage)>>;

pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>> {
    use image::ImageEncoder;
    let capacity = (image.width() * image.height() * 4) as usize;
    let mut bytes: Vec<u8> = Vec::with_capacity(capacity);
    image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut bytes))
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .context("Failed to encode PNG")?;
    Ok(bytes)
}

/// Group InputImages by base name, separating @Nx variants from 1x originals.
///
/// Returns:
/// - `groups`: base_name -> sorted vec of (scale, InputImage)
/// - `non_dpi`: images with no @Nx variant at any scale
pub fn group_dpi_variants(images: Vec<pack::InputImage>) -> (DpiGroups, Vec<pack::InputImage>) {
    let mut has_variants: std::collections::HashSet<String> = std::collections::HashSet::new();

    for img in &images {
        let stem = img.name.rsplit('/').next().unwrap_or(&img.name);
        if parse_dpi_suffix(stem).is_some() {
            let base_stem = strip_dpi_suffix(stem);
            let prefix = if let Some(slash) = img.name.rfind('/') {
                &img.name[..=slash]
            } else {
                ""
            };
            has_variants.insert(format!("{}{}", prefix, base_stem));
        }
    }

    let mut groups: DpiGroups = HashMap::new();
    let mut non_dpi: Vec<pack::InputImage> = Vec::new();

    for img in images {
        let stem = img.name.rsplit('/').next().unwrap_or(&img.name).to_string();
        let prefix = if let Some(slash) = img.name.rfind('/') {
            img.name[..=slash].to_string()
        } else {
            String::new()
        };

        if let Some(scale) = parse_dpi_suffix(&stem) {
            let base_stem = strip_dpi_suffix(&stem);
            let base_key = format!("{}{}", prefix, base_stem);
            groups.entry(base_key).or_default().push((scale, img));
        } else {
            let base_key = format!("{}{}", prefix, stem);
            if has_variants.contains(&base_key) {
                groups.entry(base_key).or_default().push((1, img));
            } else {
                non_dpi.push(img);
            }
        }
    }

    for variants in groups.values_mut() {
        variants.sort_by_key(|(s, _)| *s);
    }

    (groups, non_dpi)
}
