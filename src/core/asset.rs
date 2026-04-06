use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

// Asset types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpg,
    Bmp,
    Tga,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    Ogg,
    Flac,
    Wav,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFormat {
    Fbx,
    GltfJson,
    GltfBinary,
    /// .rbxm / .rbxmx that is NOT an animation
    Roblox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum AssetKind {
    Image(ImageFormat),
    Audio(AudioFormat),
    Model(ModelFormat),
    Animation,
    /// Raw SVG, must be rasterized before upload.
    Svg,
}

impl AssetKind {
    /// The string Roblox's asset API expects for `assetType`.
    pub fn api_type(&self) -> &'static str {
        match self {
            AssetKind::Image(_) | AssetKind::Svg => "Decal",
            AssetKind::Audio(_) => "Audio",
            AssetKind::Model(_) | AssetKind::Animation => "Model",
        }
    }

    /// MIME type for the multipart upload.
    /// SVGs are always rasterized to PNG before upload, so they report image/png.
    pub fn mime(&self) -> &'static str {
        match self {
            AssetKind::Image(ImageFormat::Png) | AssetKind::Svg => "image/png",
            AssetKind::Image(ImageFormat::Jpg) => "image/jpeg",
            AssetKind::Image(ImageFormat::Bmp) => "image/bmp",
            AssetKind::Image(ImageFormat::Tga) => "image/tga",

            AssetKind::Audio(AudioFormat::Mp3) => "audio/mpeg",
            AssetKind::Audio(AudioFormat::Ogg) => "audio/ogg",
            AssetKind::Audio(AudioFormat::Flac) => "audio/flac",
            AssetKind::Audio(AudioFormat::Wav) => "audio/wav",

            AssetKind::Model(ModelFormat::Fbx) => "model/fbx",
            AssetKind::Model(ModelFormat::GltfJson) => "model/gltf+json",
            AssetKind::Model(ModelFormat::GltfBinary) => "model/gltf-binary",
            AssetKind::Model(ModelFormat::Roblox) | AssetKind::Animation => "model/x-rbxm",
        }
    }

    /// Whether this kind can be packed into a spritesheet.
    /// SVGs can be packed after rasterization.
    pub fn is_packable(&self) -> bool {
        matches!(self, AssetKind::Image(_) | AssetKind::Svg)
    }
}

// Extension to AssetKind

/// Resolve a file extension to an `AssetKind`.
/// Returns `None` for unsupported extensions.
pub fn kind_from_ext(ext: &str) -> Option<AssetKind> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some(AssetKind::Image(ImageFormat::Png)),
        "jpg" | "jpeg" => Some(AssetKind::Image(ImageFormat::Jpg)),
        "bmp" => Some(AssetKind::Image(ImageFormat::Bmp)),
        "tga" => Some(AssetKind::Image(ImageFormat::Tga)),
        // SVG is tracked separately so callers can rasterize it explicitly.
        "svg" => Some(AssetKind::Svg),

        "mp3" => Some(AssetKind::Audio(AudioFormat::Mp3)),
        "ogg" => Some(AssetKind::Audio(AudioFormat::Ogg)),
        "flac" => Some(AssetKind::Audio(AudioFormat::Flac)),
        "wav" => Some(AssetKind::Audio(AudioFormat::Wav)),

        "fbx" => Some(AssetKind::Model(ModelFormat::Fbx)),
        "gltf" => Some(AssetKind::Model(ModelFormat::GltfJson)),
        "glb" => Some(AssetKind::Model(ModelFormat::GltfBinary)),
        "rbxm" | "rbxmx" => Some(AssetKind::Model(ModelFormat::Roblox)),

        _ => None,
    }
}

/// Returns `true` if this extension is natively supported by Tungsten.
pub fn is_supported_ext(ext: &str) -> bool {
    kind_from_ext(ext).is_some()
}

// Meta files

/// Optional per-asset metadata loaded from a `.tmeta` sidecar file.
/// The file must share the same stem as the asset:
/// `sword.png` -> `sword.tmeta`
#[derive(Deserialize, Debug, Default, Clone)]
pub struct AssetMeta {
    /// Overrides the display name sent to Roblox.
    pub name: Option<String>,
    /// Overrides the description sent to Roblox.
    pub description: Option<String>,
}

impl AssetMeta {
    /// Try to load a `.tmeta` sidecar next to `asset_path`.
    /// Returns `Default::default()` (all `None`) if no sidecar exists.
    pub fn load_for(asset_path: &Path) -> Result<Self> {
        let tmeta_path = asset_path.with_extension("tmeta");

        if !tmeta_path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(&tmeta_path)
            .with_context(|| format!("Failed to read \"{}\"", tmeta_path.display()))?;

        toml::from_str(&contents)
            .with_context(|| format!("Failed to parse \"{}\"", tmeta_path.display()))
    }

    /// Final display name
    pub fn resolve_name<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.name.as_deref().unwrap_or(fallback)
    }

    /// Final description
    pub fn resolve_description<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.description.as_deref().unwrap_or(fallback)
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kind_from_ext_images() {
        assert_eq!(
            kind_from_ext("png"),
            Some(AssetKind::Image(ImageFormat::Png))
        );
        assert_eq!(
            kind_from_ext("jpg"),
            Some(AssetKind::Image(ImageFormat::Jpg))
        );
        assert_eq!(
            kind_from_ext("jpeg"),
            Some(AssetKind::Image(ImageFormat::Jpg))
        );
        assert_eq!(
            kind_from_ext("PNG"),
            Some(AssetKind::Image(ImageFormat::Png))
        );
    }

    #[test]
    fn test_kind_from_ext_svg() {
        assert_eq!(kind_from_ext("svg"), Some(AssetKind::Svg));
        assert_eq!(kind_from_ext("SVG"), Some(AssetKind::Svg));
    }

    #[test]
    fn test_kind_from_ext_audio() {
        assert_eq!(
            kind_from_ext("mp3"),
            Some(AssetKind::Audio(AudioFormat::Mp3))
        );
        assert_eq!(
            kind_from_ext("wav"),
            Some(AssetKind::Audio(AudioFormat::Wav))
        );
    }

    #[test]
    fn test_kind_from_ext_unknown() {
        assert_eq!(kind_from_ext("txt"), None);
        assert_eq!(kind_from_ext("exe"), None);
    }

    #[test]
    fn test_svg_is_packable() {
        assert!(AssetKind::Svg.is_packable());
    }

    #[test]
    fn test_svg_api_type_and_mime() {
        assert_eq!(AssetKind::Svg.api_type(), "Decal");
        assert_eq!(AssetKind::Svg.mime(), "image/png");
    }

    #[test]
    fn test_mime_types() {
        assert_eq!(AssetKind::Image(ImageFormat::Png).mime(), "image/png");
        assert_eq!(AssetKind::Audio(AudioFormat::Mp3).mime(), "audio/mpeg");
        assert_eq!(AssetKind::Model(ModelFormat::Fbx).mime(), "model/fbx");
    }

    #[test]
    fn test_asset_meta_defaults() {
        let meta = AssetMeta::default();
        assert_eq!(meta.resolve_name("fallback"), "fallback");
        assert_eq!(meta.resolve_description("desc"), "desc");
    }
}
