//! Asset taxonomy: formats, kinds, extension mapping, `.tmeta` metadata, SVG scale resolution.

use anyhow::{Context, Result};
use rbx_binary::from_reader;
use rbx_xml::from_reader as xml_from_reader;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    M4a,
}

impl AudioFormat {
    /// Whether Roblox's upload API accepts this format directly.
    /// Roblox accepts MP3, OGG, WAV and FLAC; M4A/AAC sources should be
    /// transcoded to OGG before upload.
    pub fn roblox_supported(&self) -> bool {
        !matches!(self, AudioFormat::M4a)
    }
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

/// An asset that exists on Roblox (for web asset mapping).
/// Used in config to map local paths to existing Roblox asset IDs.
#[derive(Debug, Deserialize, Clone)]
pub struct WebAsset {
    /// The asset ID on Roblox.
    pub id: u64,
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
            AssetKind::Audio(AudioFormat::M4a) => "audio/mp4",

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
        "m4a" | "aac" => Some(AssetKind::Audio(AudioFormat::M4a)),

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

/// Strip the file extension from a relative path and normalise separators to
/// forward slashes. `"icons/arrow-up.png"` -> `"icons/arrow-up"`.
pub fn stem_name(rel: &str) -> String {
    Path::new(rel)
        .with_extension("")
        .to_string_lossy()
        .replace('\\', "/")
}

/// Represents the format of a Roblox model file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RobloxModelFormat {
    Binary,
    Xml,
}

/// Detects if a Roblox model file (.rbxm / .rbxmx) contains an animation
/// (KeyframeSequence or CurveAnimation).
///
/// Returns `Ok(true)` if the root object is a KeyframeSequence or CurveAnimation,
/// `Ok(false)` if it's a different class, or `Err` if parsing fails.
pub fn is_animation(data: &[u8], format: RobloxModelFormat) -> Result<bool> {
    let root_class = match format {
        RobloxModelFormat::Binary => {
            let dom = from_reader(data).context("Failed to parse binary Roblox model")?;
            let root = dom.root();
            let children = root.children();
            let first_ref = children.first().context("No children found in root")?;
            let first = dom
                .get_by_ref(*first_ref)
                .context("Failed to get first child")?;
            first.class
        }
        RobloxModelFormat::Xml => {
            let dom = xml_from_reader(data, Default::default())
                .context("Failed to parse XML Roblox model")?;
            let root = dom.root();
            let children = root.children();
            let first_ref = children.first().context("No children found in root")?;
            let first = dom
                .get_by_ref(*first_ref)
                .context("Failed to get first child")?;
            first.class
        }
    };

    Ok(root_class == "KeyframeSequence" || root_class == "CurveAnimation")
}

/// Reads a Roblox model file from disk and checks if it's an animation.
pub fn is_animation_file(path: &Path) -> Result<bool> {
    let data = std::fs::read(path).context("Failed to read file")?;
    let format = match path.extension().and_then(|e| e.to_str()) {
        Some("rbxm") => RobloxModelFormat::Binary,
        Some("rbxmx") => RobloxModelFormat::Xml,
        _ => anyhow::bail!("Not a Roblox model file (.rbxm or .rbxmx)"),
    };
    is_animation(&data, format)
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
    /// Optional override for the asset type sent to Roblox's API.
    /// Mirrors the input-level `type` config field: `"auto"` is treated as
    /// no override, and any other value takes precedence over the input-level
    /// override. For `.rbxm`/`.rbxmx` files, `"animation"` or `"model"` also
    /// pin the kind and skip the DOM parse used to auto-detect animations.
    #[serde(rename = "type")]
    pub asset_type: Option<String>,
}

impl AssetMeta {
    /// Try to load a `.tmeta` sidecar next to `asset_path`.
    /// For files, first tries `name.format.tmeta` (e.g., `image.png.tmeta`),
    /// then falls back to `name.tmeta` (e.g., `image.tmeta`).
    /// For directories, uses `name.tmeta` (e.g., `icons.tmeta`).
    /// Returns `Default::default()` (all `None`) if no sidecar exists.
    pub fn load_for(asset_path: &Path) -> Result<Self> {
        // Try the more specific format first for files: name.format.tmeta
        let mut tmeta_path = asset_path.to_path_buf();

        if asset_path.is_file() {
            // For files: try name.format.tmeta first (e.g., image.png.tmeta)
            let extension = asset_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            tmeta_path.set_extension(format!("{}.tmeta", extension));

            if tmeta_path.exists() {
                let contents = std::fs::read_to_string(&tmeta_path)
                    .with_context(|| format!("Failed to read \"{}\"", tmeta_path.display()))?;

                return toml::from_str(&contents)
                    .with_context(|| format!("Failed to parse \"{}\"", tmeta_path.display()));
            }

            // Fall back to name.tmeta (e.g., image.tmeta)
            tmeta_path = asset_path.with_extension("tmeta");
            tmeta_path.set_file_name(format!(
                "{}.tmeta",
                asset_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
            ));
        } else {
            // For directories: use name.tmeta (e.g., icons.tmeta)
            tmeta_path = asset_path.with_extension("tmeta");
        }

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

    /// Final API asset type override. `None` means derive it from the asset
    /// kind; `"auto"` is treated as `None`.
    ///
    /// `"animation"` and `"model"` are kind pins (see [`Self::pinned_model_kind`])
    /// rather than API types, so they are not forwarded as an override — the
    /// pinned kind already yields the correct `"Model"` API type.
    pub fn resolve_asset_type(&self) -> Option<&str> {
        match self.asset_type.as_deref() {
            Some(s)
                if s.eq_ignore_ascii_case("auto")
                    || s.eq_ignore_ascii_case("animation")
                    || s.eq_ignore_ascii_case("model") =>
            {
                None
            }
            Some(s) => Some(s),
            None => None,
        }
    }

    /// The pinned kind for a `.rbxm`/`.rbxmx` file when `type` is
    /// `"animation"` or `"model"`. `None` means unpinned — fall back to DOM
    /// auto-detection.
    pub fn pinned_model_kind(&self) -> Option<AssetKind> {
        match self.asset_type.as_deref() {
            Some(s) if s.eq_ignore_ascii_case("animation") => Some(AssetKind::Animation),
            Some(s) if s.eq_ignore_ascii_case("model") => {
                Some(AssetKind::Model(ModelFormat::Roblox))
            }
            _ => None,
        }
    }
}

// SVG scale resolution from `.tmeta` sidecars

/// Minimum SVG rasterization scale.
const MIN_SVG_SCALE: f32 = 0.01;

/// Memoization cache for `.tmeta` `svg_scale` lookups, so a directory's meta
/// file is read once per sync instead of once per SVG.
pub type SvgScaleCache = std::sync::Mutex<HashMap<PathBuf, Option<f32>>>;

/// Create an empty `SvgScaleCache`.
pub fn new_svg_scale_cache() -> SvgScaleCache {
    SvgScaleCache::new(HashMap::new())
}

/// The effective SVG rasterization scale for `file_path`, walking from the file
/// up to (and including) the input's `base_path`. The nearest ancestor with an
/// `svg_scale` in its `.tmeta` wins; otherwise the input's configured scale (or
/// `1.0`) is used. Clamped to a minimum of [`MIN_SVG_SCALE`].
pub fn effective_svg_scale_for_path(
    file_path: &Path,
    base_path: &str,
    configured_scale: Option<f32>,
    cache: &SvgScaleCache,
) -> f32 {
    let base = Path::new(base_path);
    for anc in file_path.ancestors() {
        if !anc.starts_with(base) {
            break;
        }
        if let Some(scale) = svg_scale_from_tmeta(anc, cache) {
            return scale.max(MIN_SVG_SCALE);
        }
        if anc == base {
            break;
        }
    }
    configured_scale.unwrap_or(1.0).max(MIN_SVG_SCALE)
}

fn svg_scale_from_tmeta(item: &Path, cache: &SvgScaleCache) -> Option<f32> {
    fn read_cached(path: &Path, cache: &SvgScaleCache) -> Option<f32> {
        let mut guard = cache.lock().unwrap();
        if let Some(&v) = guard.get(path) {
            return v;
        }
        let v = try_read_tmeta_svg_scale(path);
        guard.insert(path.to_path_buf(), v);
        v
    }

    if item.is_file() {
        if let Some(ext) = item.extension() {
            let mut path = item.to_path_buf();
            path.set_extension(format!("{}.tmeta", ext.to_string_lossy()));
            if let Some(scale) = read_cached(&path, cache) {
                return Some(scale);
            }
        }
        let mut path = item.to_path_buf();
        path.set_extension("tmeta");
        if let Some(scale) = read_cached(&path, cache) {
            return Some(scale);
        }
    } else {
        let dir_name = item.file_name()?.to_os_string();
        let mut parent = item.parent()?.to_path_buf();
        parent.push(dir_name);
        parent.set_extension("tmeta");
        if let Some(scale) = read_cached(&parent, cache) {
            return Some(scale);
        }
    }
    None
}

/// Read `svg_scale` from a single `.tmeta` file, or `None` if absent/unreadable.
fn try_read_tmeta_svg_scale(path: &Path) -> Option<f32> {
    std::fs::read_to_string(path).ok().and_then(|contents| {
        let map: toml::Value = toml::from_str(&contents).ok()?;
        match map.get("svg_scale") {
            Some(toml::Value::Float(v)) => Some(*v as f32),
            Some(toml::Value::Integer(v)) => Some(*v as f32),
            _ => None,
        }
    })
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
        assert_eq!(
            kind_from_ext("m4a"),
            Some(AssetKind::Audio(AudioFormat::M4a))
        );
        assert_eq!(
            kind_from_ext("aac"),
            Some(AssetKind::Audio(AudioFormat::M4a))
        );
    }

    #[test]
    fn test_roblox_supported_audio_formats() {
        // Roblox accepts MP3, OGG, WAV and FLAC directly; only M4A/AAC is
        // reliably rejected and should be transcoded to OGG.
        assert!(AudioFormat::Mp3.roblox_supported());
        assert!(AudioFormat::Ogg.roblox_supported());
        assert!(AudioFormat::Wav.roblox_supported());
        assert!(AudioFormat::Flac.roblox_supported());
        assert!(!AudioFormat::M4a.roblox_supported());
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
        assert_eq!(AssetKind::Audio(AudioFormat::M4a).mime(), "audio/mp4");
        assert_eq!(AssetKind::Model(ModelFormat::Fbx).mime(), "model/fbx");
    }

    #[test]
    fn test_asset_meta_defaults() {
        let meta = AssetMeta::default();
        assert_eq!(meta.resolve_name("fallback"), "fallback");
        assert_eq!(meta.resolve_description("desc"), "desc");
        assert_eq!(meta.resolve_asset_type(), None);
    }

    #[test]
    fn test_resolve_asset_type_treats_auto_as_none() {
        let meta = AssetMeta {
            asset_type: Some("auto".to_string()),
            ..Default::default()
        };
        assert_eq!(meta.resolve_asset_type(), None);

        // Casing of the sentinel does not matter.
        let meta = AssetMeta {
            asset_type: Some("Auto".to_string()),
            ..Default::default()
        };
        assert_eq!(meta.resolve_asset_type(), None);

        // Any other value is passed through verbatim.
        let meta = AssetMeta {
            asset_type: Some("Decal".to_string()),
            ..Default::default()
        };
        assert_eq!(meta.resolve_asset_type(), Some("Decal"));
    }

    #[test]
    fn test_meta_file_parses_type_field() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.png");
        File::create(&file_path).unwrap();

        let meta_path = temp_dir.path().join("test.png.tmeta");
        let mut meta_file = File::create(&meta_path).unwrap();
        writeln!(meta_file, "type = \"Decal\"").unwrap();

        let meta = AssetMeta::load_for(&file_path).unwrap();
        assert_eq!(meta.resolve_asset_type(), Some("Decal"));
    }

    #[test]
    fn test_meta_file_naming_priority() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.png");

        // Create an empty file
        File::create(&file_path).unwrap();

        // Test 1: Specific format takes priority (.format.tmeta)
        let specific_meta_path = temp_dir.path().join("test.png.tmeta");
        let mut specific_file = File::create(&specific_meta_path).unwrap();
        writeln!(specific_file, "name = \"Specific Format Name\"").unwrap();
        writeln!(
            specific_file,
            "description = \"Specific format description\""
        )
        .unwrap();

        let meta = AssetMeta::load_for(&file_path).unwrap();
        assert_eq!(meta.name.as_deref(), Some("Specific Format Name"));
        assert_eq!(
            meta.description.as_deref(),
            Some("Specific format description")
        );

        // Test 2: Fall back to general .tmeta when specific doesn't exist
        std::fs::remove_file(&specific_meta_path).unwrap();
        let general_meta_path = temp_dir.path().join("test.tmeta");
        let mut general_file = File::create(&general_meta_path).unwrap();
        writeln!(general_file, "name = \"General Meta Name\"").unwrap();
        writeln!(general_file, "description = \"General meta description\"").unwrap();

        let meta = AssetMeta::load_for(&file_path).unwrap();
        assert_eq!(meta.name.as_deref(), Some("General Meta Name"));
        assert_eq!(
            meta.description.as_deref(),
            Some("General meta description")
        );

        // Test 3: Directory meta files use name.tmeta
        let dir_path = temp_dir.path().join("assets");
        std::fs::create_dir(&dir_path).unwrap();
        let dir_meta_path = temp_dir.path().join("assets.tmeta");
        let mut dir_meta_file = File::create(&dir_meta_path).unwrap();
        writeln!(dir_meta_file, "name = \"Assets Folder\"").unwrap();
        writeln!(dir_meta_file, "description = \"Folder description\"").unwrap();

        let dir_meta = AssetMeta::load_for(&dir_path).unwrap();
        assert_eq!(dir_meta.name.as_deref(), Some("Assets Folder"));
        assert_eq!(dir_meta.description.as_deref(), Some("Folder description"));

        // Test 4: No meta files returns defaults
        std::fs::remove_file(&general_meta_path).unwrap();
        std::fs::remove_file(&dir_meta_path).unwrap();
        let default_meta = AssetMeta::load_for(&file_path).unwrap();
        assert_eq!(default_meta.name, None);
        assert_eq!(default_meta.description, None);
    }
}
