use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use image::RgbaImage;

use crate::core::assets::asset::ImageFormat;
use crate::core::assets::img::{convert, pack};
use crate::core::postsync::codegen::{parse_dpi_suffix, strip_dpi_suffix};

/// Maps a base asset name to its DPI variants: `(scale_factor, image)` pairs.
pub type DpiGroups = HashMap<String, Vec<(u8, pack::InputImage)>>;

pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>> {
    // Single canonical PNG encoder lives in `convert::convert_image`.
    convert::convert_image(image, ImageFormat::Png)
}

/// Split `name` into `(directory prefix including the trailing '/', file stem)`
/// using string slices — no intermediate allocations.
fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('/') {
        Some(slash) => (&name[..=slash], &name[slash + 1..]),
        None => ("", name),
    }
}

/// Group InputImages by base name, separating @Nx variants from 1x originals.
///
/// Returns:
/// - `groups`: base_name -> sorted vec of (scale, InputImage)
/// - `non_dpi`: images with no @Nx variant at any scale
pub fn group_dpi_variants(images: Vec<pack::InputImage>) -> (DpiGroups, Vec<pack::InputImage>) {
    let mut has_variants: std::collections::HashSet<String> = std::collections::HashSet::new();

    // First pass: which base names have at least one @Nx variant?
    for img in &images {
        let (prefix, stem) = split_name(&img.name);
        if parse_dpi_suffix(stem).is_some() {
            has_variants.insert(format!("{}{}", prefix, strip_dpi_suffix(stem)));
        }
    }

    let mut groups: DpiGroups = HashMap::new();
    let mut non_dpi: Vec<pack::InputImage> = Vec::new();

    for img in images {
        let (prefix, stem) = split_name(&img.name);
        // For both @Nx variants and 1x originals the group key is the base
        // stem (a `@Nx`-less name is its own base). One allocation per image.
        let base_key = format!("{}{}", prefix, strip_dpi_suffix(stem));

        if let Some(scale) = parse_dpi_suffix(stem) {
            groups.entry(base_key).or_default().push((scale, img));
        } else if has_variants.contains(&base_key) {
            groups.entry(base_key).or_default().push((1, img));
        } else {
            non_dpi.push(img);
        }
    }

    for variants in groups.values_mut() {
        variants.sort_by_key(|(s, _)| *s);
    }

    (groups, non_dpi)
}

/// DPI groups keyed by base name, values being `(scale, path)` pairs.
pub type DpiPathGroups = HashMap<String, Vec<(u8, PathBuf)>>;

/// Group `(path, name)` pairs by DPI base name, mirroring `group_dpi_variants`
/// but for files that are still on disk (decoded lazily during processing).
/// Returns `(base -> Vec<(scale, path)>, non_dpi paths)`.
pub fn group_paths_by_dpi(
    named: Vec<(PathBuf, String)>,
) -> (DpiPathGroups, Vec<(PathBuf, String)>) {
    let mut has_variants: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (_, name) in &named {
        let (prefix, stem) = split_name(name);
        if parse_dpi_suffix(stem).is_some() {
            has_variants.insert(format!("{}{}", prefix, strip_dpi_suffix(stem)));
        }
    }

    let mut groups: HashMap<String, Vec<(u8, PathBuf)>> = HashMap::new();
    let mut non_dpi: Vec<(PathBuf, String)> = Vec::new();

    for (path, name) in named {
        let (prefix, stem) = split_name(&name);
        let base_key = format!("{}{}", prefix, strip_dpi_suffix(stem));
        if let Some(scale) = parse_dpi_suffix(stem) {
            groups.entry(base_key).or_default().push((scale, path));
        } else if has_variants.contains(&base_key) {
            groups.entry(base_key).or_default().push((1, path));
        } else {
            non_dpi.push((path, name));
        }
    }

    for variants in groups.values_mut() {
        variants.sort_by_key(|(s, _)| *s);
    }

    (groups, non_dpi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    fn img(name: &str) -> pack::InputImage {
        pack::InputImage {
            name: name.to_string(),
            image: RgbaImage::new(4, 4),
        }
    }

    #[test]
    fn groups_variants_and_keeps_1x_with_variants() {
        // Names are pipeline stems (extension already stripped).
        let images = vec![
            img("ui/search"),
            img("ui/search@2x"),
            img("ui/search@3x"),
            img("ui/settings"),
        ];

        let (groups, non_dpi) = group_dpi_variants(images);

        // 1x original stays grouped with its variants under the base key.
        let search = groups.get("ui/search").expect("search group");
        let scales: Vec<u8> = search.iter().map(|(s, _)| *s).collect();
        assert_eq!(scales, vec![1, 2, 3], "variants sorted ascending");

        // Plain image with no @Nx siblings goes to non_dpi.
        assert_eq!(non_dpi.len(), 1);
        assert_eq!(non_dpi[0].name, "ui/settings");
    }

    #[test]
    fn plain_image_with_no_variants_is_not_grouped() {
        let images = vec![img("a"), img("b")];
        let (groups, non_dpi) = group_dpi_variants(images);
        assert!(groups.is_empty());
        assert_eq!(non_dpi.len(), 2);
    }

    #[test]
    fn nested_directories_keep_prefixes() {
        let images = vec![img("icons/red@2x"), img("icons/red")];
        let (groups, non_dpi) = group_dpi_variants(images);
        assert!(non_dpi.is_empty());
        let red = groups.get("icons/red").expect("prefixed base key");
        assert_eq!(red.len(), 2);
    }
}
