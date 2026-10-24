//! DPI-variant grouping: splits `@Nx`-suffixed names from plain 1x assets.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::core::assets::img::pack;
use crate::core::postsync::codegen::{parse_dpi_suffix, strip_dpi_suffix};

/// Maps a base asset name to its DPI variants: `(scale_factor, image)` pairs.
pub type DpiGroups = HashMap<String, Vec<(u8, pack::InputImage)>>;

/// DPI groups keyed by base name, values being `(scale, path)` pairs.
pub type DpiPathGroups = HashMap<String, Vec<(u8, PathBuf)>>;

/// Split `name` into `(directory prefix including the trailing '/', file stem)`
/// using string slices — no intermediate allocations.
fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('/') {
        Some(slash) => (&name[..=slash], &name[slash + 1..]),
        None => ("", name),
    }
}

/// Generic DPI grouping over `(name, item)` pairs.
///
/// `groups` maps base name → sorted `(scale, item)` for every name that has at
/// least one `@Nx` sibling (the 1x original joins its variants under the base
/// key); `plain` holds items with no `@Nx` variants at all. The 1x original and
/// its `@2x`/`@3x` siblings share a base key, so a sheet/icon that ships at
/// several scales collapses into a single codegen entry.
#[allow(clippy::type_complexity)]
fn group_by_dpi<T>(items: Vec<(String, T)>) -> (HashMap<String, Vec<(u8, T)>>, Vec<(String, T)>) {
    let mut has_variants: std::collections::HashSet<String> = std::collections::HashSet::new();
    // One pass to precompute `(base_key, scale)` per item so we don't re-split
    // and re-allocate the name string on the second pass.
    let precomputed: Vec<(String, Option<u8>)> = items
        .iter()
        .map(|(name, _)| {
            let (prefix, stem) = split_name(name);
            let base_key = format!("{}{}", prefix, strip_dpi_suffix(stem));
            let scale = parse_dpi_suffix(stem);
            if scale.is_some() {
                has_variants.insert(base_key.clone());
            }
            (base_key, scale)
        })
        .collect();

    let mut groups: HashMap<String, Vec<(u8, T)>> = HashMap::new();
    let mut plain: Vec<(String, T)> = Vec::new();

    for ((name, item), (base_key, scale)) in items.into_iter().zip(precomputed) {
        match scale {
            Some(scale) => groups.entry(base_key).or_default().push((scale, item)),
            None if has_variants.contains(&base_key) => {
                groups.entry(base_key).or_default().push((1, item))
            }
            None => plain.push((name, item)),
        }
    }

    for variants in groups.values_mut() {
        variants.sort_by_key(|(s, _)| *s);
    }

    (groups, plain)
}

/// Group [`pack::InputImage`]s by DPI base name, separating `@Nx` variants from
/// 1x originals. Returns `(base → Vec<(scale, image)>, non-DPI images)`.
pub fn group_dpi_variants(images: Vec<pack::InputImage>) -> (DpiGroups, Vec<pack::InputImage>) {
    let (groups, plain) = group_by_dpi(
        images
            .into_iter()
            .map(|img| (img.name.clone(), img))
            .collect(),
    );
    (groups, plain.into_iter().map(|(_, img)| img).collect())
}

/// Group `(path, name)` pairs by DPI base name — the on-disk mirror of
/// [`group_dpi_variants`] for files decoded lazily during processing. Returns
/// `(base → Vec<(scale, path)>, non-DPI (path, name) pairs)`.
pub fn group_paths_by_dpi(
    named: Vec<(PathBuf, String)>,
) -> (DpiPathGroups, Vec<(PathBuf, String)>) {
    let (groups, plain) = group_by_dpi(named.into_iter().map(|(p, n)| (n, p)).collect());
    (
        groups,
        plain.into_iter().map(|(name, path)| (path, name)).collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    fn img(name: &str) -> pack::InputImage {
        pack::InputImage {
            name: name.to_string(),
            image: RgbaImage::new(4, 4),
            source_hash: String::new(),
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
