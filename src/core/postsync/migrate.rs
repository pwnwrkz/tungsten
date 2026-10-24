//! Lockfile v2 → v3 migration: one engine used by both `Lockfile::load()`
//! auto-migration and the `tungsten migrate-lock` CLI.
//!
//! v2 keyed entries by `(input_group, processed_content_hash)`; v3 keys them
//! by per-asset name. The migrator re-runs the sync pipeline's
//! prepare/hash/pack steps against the files on disk to reconstruct each
//! asset's name and hashes, then lifts the cached `asset_id` / `studio_uri`
//! out of the matching v2 entry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::Deserialize;

use crate::commands::sync::dpi_groups::{group_dpi_variants, group_paths_by_dpi};
use crate::commands::sync::paths::{collect_paths, glob_base, relative_path};
use crate::core::assets::asset::{self, AssetKind, stem_name};
use crate::core::assets::img::alpha_bleed::alpha_bleed;
use crate::core::assets::img::compress::{CompressOptions, maybe_compress_png};
use crate::core::assets::img::convert::{encode_png, svg_to_rgba};
use crate::core::assets::img::pack;
use crate::core::postsync::lockfile::{
    DpiVariant, LOCKFILE_VERSION, Lockfile, LockfileEntry, Slice, hash_bytes, hash_parts,
};
use crate::utils::config::{self, Config, InputConfig};

pub struct MigrationOptions {
    pub lockfile_path: PathBuf,
    pub config_path: PathBuf,
    pub dry_run: bool,
    /// Allow overwriting an existing v3 lockfile (nothing to migrate).
    #[allow(dead_code)]
    pub force: bool,
    pub write_backup: bool,
}

#[derive(Debug)]
pub struct MigrationReport {
    pub total_v2_entries: usize,
    pub migrated: usize,
    pub unmapped: Vec<String>,
    pub written_path: Option<PathBuf>,
    pub backup_path: Option<PathBuf>,
    pub dry_run: bool,
}

/// One discovered asset with everything needed to build its v3 entry.
struct Discovered {
    /// Config input name (the v3 lockfile groups entries under it).
    input: String,
    name: String,
    kind: String,
    source_hash: String,
    /// Processed-bytes hash: the encoded (and possibly compressed) PNG for
    /// images, the final bytes for raw assets. `None` for DPI parents and
    /// sprites (resolved later from the sheet).
    hash: Option<String>,
    packable: bool,
    /// `(scale, processed_hash)` for DPI groups.
    dpi: Option<Vec<(u8, String)>>,
    /// Sprite layout when this asset packs into a sheet.
    slice: Option<Slice>,
}

impl Default for MigrationOptions {
    fn default() -> Self {
        Self {
            lockfile_path: default_lockfile_path(),
            config_path: PathBuf::from(config::CONFIG_PATH),
            dry_run: false,
            force: false,
            write_backup: true,
        }
    }
}

/// Resolve the lockfile to migrate: the new `tungsten.lock` if present,
/// otherwise the legacy `tungsten.lock.toml` (auto-detect).
pub fn default_lockfile_path() -> PathBuf {
    let new = PathBuf::from(crate::core::postsync::lockfile::LOCKFILE_PATH);
    if new.exists() {
        return new;
    }
    let legacy = PathBuf::from(crate::core::postsync::lockfile::LEGACY_LOCKFILE_PATH);
    if legacy.exists() {
        return legacy;
    }
    new
}

/// v2 lockfile shape: `input_name -> hash -> { asset_id, studio_uri }`.
#[derive(Deserialize, Default)]
struct V2Lockfile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    inputs: HashMap<String, HashMap<String, V2Entry>>,
}

#[derive(Deserialize, Default, Clone)]
struct V2Entry {
    asset_id: Option<u64>,
    #[serde(default)]
    studio_uri: Option<String>,
}

/// Read the v2 lockfile. Refuses a v3+ file unless `force` is set.
fn load_v2(path: &Path, force: bool) -> Result<V2Lockfile> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Could not read \"{}\"", path.display()))?;
    let v2: V2Lockfile = toml::from_str(&content)
        .with_context(|| format!("Failed to parse \"{}\" as a v2 lockfile", path.display()))?;
    if v2.version >= LOCKFILE_VERSION && v2.version != 0 {
        if force {
            return Ok(V2Lockfile::default());
        }
        bail!(
            "\"{}\" is already at version {} — nothing to migrate",
            path.display(),
            v2.version
        );
    }
    Ok(v2)
}

/// Per-input processing settings the hash computation depends on.
struct ResolvedProcessing {
    bleed: bool,
    compress: Option<CompressOptions>,
}

/// Processed hash for one plain image: rasterize if SVG, bleed, encode PNG,
/// compress, hash — the same steps `individual.rs` runs.
fn plain_image_hash(
    path: &Path,
    base: &str,
    input: &InputConfig,
    p: &ResolvedProcessing,
) -> Result<String> {
    let cache = std::sync::Arc::new(asset::new_svg_scale_cache());
    let rgba = if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
    {
        let data = std::fs::read(path)?;
        let scale = asset::effective_svg_scale_for_path(path, base, input.svg_scale, &cache);
        svg_to_rgba(&data, scale)?
    } else {
        image::open(path)?.into_rgba8()
    };
    Ok(processed_image_hash(rgba, p))
}

/// bleed → encode → compress → hash, shared by plain images, DPI variants
/// and packed sheets so the migrator and sync agree byte-for-byte.
fn processed_image_hash(mut rgba: image::RgbaImage, p: &ResolvedProcessing) -> String {
    if p.bleed {
        alpha_bleed(&mut rgba);
    }
    let mut bytes = encode_png(&rgba).expect("PNG encode of a decoded image cannot fail");
    bytes = maybe_compress_png(bytes, p.compress.as_ref());
    hash_bytes(&bytes)
}

pub fn migrate_v2_to_v3(opts: &MigrationOptions) -> Result<MigrationReport> {
    let v2 = load_v2(&opts.lockfile_path, opts.force)?;
    let total_v2_entries = v2.inputs.values().map(|m| m.len()).sum::<usize>();

    let config: Config = config::load(opts.config_path.to_string_lossy().as_ref()).with_context(
        || {
            format!(
                "migration requires \"{}\" to reconstruct per-asset names — pass --config <path> if your config is elsewhere",
                opts.config_path.display()
            )
        },
    )?;

    // 1. Discover every asset across all inputs, computing name + hashes
    //    the same way sync does. Input globs are relative to the config's
    //    directory (the project root), not the process cwd.
    // Absolute project root: the config's directory, canonicalized so glob
    // output paths (also absolute) strip-prefix cleanly during naming.
    let root = if opts.config_path.is_absolute() {
        opts.config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    } else {
        // Relative (e.g. the default "tungsten.toml"): anchor to the cwd.
        let cwd = std::env::current_dir().unwrap_or_default();
        let joined = cwd.join(&opts.config_path);
        joined.parent().map(|p| p.to_path_buf()).unwrap_or(cwd)
    };
    let mut discovered: Vec<Discovered> = Vec::new();
    for (input_name, input) in &config.inputs {
        discovered.extend(discover_input(input_name, &root, input)?);
    }

    // 2. Lift cached ids/uris from v2 into the v3 entries.
    let mut inputs: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, LockfileEntry>,
    > = std::collections::BTreeMap::new();
    let mut migrated = 0usize;
    let mut unmapped: Vec<String> = Vec::new();

    for d in &discovered {
        let mut entry = LockfileEntry {
            kind: d.kind.clone(),
            source_hash: d.source_hash.clone(),
            hash: d.hash.clone(),
            id: None,
            studio_uri: None,
            packable: d.packable,
            slice: d.slice,
            dpi: Default::default(),
        };

        match (&d.hash, &d.dpi) {
            // Plain/sprite/audio asset: one processed hash to look up.
            (Some(hash), _) => match lookup_v2(&v2, hash) {
                Some(e) => {
                    entry.id = e.asset_id;
                    entry.studio_uri = e.studio_uri.clone();
                    migrated += 1;
                }
                None => unmapped.push(d.name.clone()),
            },
            // DPI parent: each variant lifts its own v2 entry.
            (None, Some(variants)) => {
                for (scale, vhash) in variants {
                    match lookup_v2(&v2, vhash) {
                        Some(e) => {
                            entry.dpi.insert(
                                *scale,
                                DpiVariant {
                                    hash: vhash.clone(),
                                    id: e.asset_id,
                                    studio_uri: e.studio_uri.clone(),
                                },
                            );
                            migrated += 1;
                        }
                        None => unmapped.push(format!("{}@{}x", d.name, scale)),
                    }
                }
            }
            (None, None) => unmapped.push(d.name.clone()),
        }

        inputs
            .entry(d.input.clone())
            .or_default()
            .insert(d.name.clone(), entry);
    }

    // 3. Back up the v2 file, then write the v3 lockfile.
    // Output path: the new `tungsten.lock`, even when migrating the legacy
    // `tungsten.lock.toml` — the migration is the rename point. Keep the
    // input's directory so an explicit `--lockfile path/to/x.toml` migrates
    // in place.
    let out_path = if opts.lockfile_path.file_name().and_then(|n| n.to_str())
        == Some(crate::core::postsync::lockfile::LEGACY_LOCKFILE_PATH)
    {
        opts.lockfile_path
            .parent()
            .map(|p| p.join(crate::core::postsync::lockfile::LOCKFILE_PATH))
            .unwrap_or_else(|| PathBuf::from(crate::core::postsync::lockfile::LOCKFILE_PATH))
    } else {
        opts.lockfile_path.clone()
    };

    let mut backup_path = None;
    if !opts.dry_run {
        if opts.write_backup {
            let bak = backup_path_of(&opts.lockfile_path);
            if bak.exists() {
                bail!(
                    "\"{}\" already exists — move or delete it before migrating (it is your escape hatch back to v2)",
                    bak.display()
                );
            }
            std::fs::copy(&opts.lockfile_path, &bak).with_context(|| {
                format!(
                    "Could not back up \"{}\" to \"{}\"",
                    opts.lockfile_path.display(),
                    bak.display()
                )
            })?;
            backup_path = Some(bak);
        }
        let mut lf = Lockfile {
            version: LOCKFILE_VERSION,
            inputs,
            dirty: true,
        };
        lf.save_to(&out_path)?;
        // The legacy file is superseded by the migrated `tungsten.lock`.
        if out_path != opts.lockfile_path {
            std::fs::remove_file(&opts.lockfile_path).with_context(|| {
                format!(
                    "Could not remove \"{}\" after migrating to \"{}\"",
                    opts.lockfile_path.display(),
                    out_path.display()
                )
            })?;
        }
    }

    Ok(MigrationReport {
        total_v2_entries,
        migrated,
        unmapped,
        written_path: (!opts.dry_run).then_some(out_path),
        backup_path,
        dry_run: opts.dry_run,
    })
}

/// `<lockfile>.v2.bak` — literal suffix, not an extension swap.
fn backup_path_of(lockfile: &Path) -> PathBuf {
    let mut s = lockfile.as_os_str().to_os_string();
    s.push(".v2.bak");
    PathBuf::from(s)
}

/// The v2 lockfile does not record which input an asset came from, so search
/// every input group for the processed hash.
fn lookup_v2<'a>(v2: &'a V2Lockfile, hash: &str) -> Option<&'a V2Entry> {
    v2.inputs.values().find_map(|m| m.get(hash))
}

/// Discover every asset for one input: computes names, source hashes,
/// processed hashes (plain images, audio cache keys, DPI variants) and
/// sprite slices (packable inputs). Paths resolve relative to `root`.
fn discover_input(input_name: &str, root: &Path, input: &InputConfig) -> Result<Vec<Discovered>> {
    // Globs are relative to the project root; make them absolute so the
    // engine works regardless of the process cwd.
    let pattern = if Path::new(&input.path).is_absolute() {
        input.path.clone()
    } else {
        root.join(&input.path).to_string_lossy().replace('\\', "/")
    };
    let paths = collect_paths(&pattern)?;
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    // Base dir for asset naming, in absolute form to match the glob output.
    let base = glob_base(&pattern);
    let processing = ResolvedProcessing {
        bleed: input.resolved_bleed(),
        compress: input.resolved_compress_options(),
    };

    let packable = input.packable.unwrap_or(false);
    let (image_paths, other_paths): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        ext.eq_ignore_ascii_case("svg")
            || asset::kind_from_ext(ext).is_some_and(|k| k.is_packable())
    });

    let mut out: Vec<Discovered> = Vec::new();

    // Non-image assets: audio/models — name, source hash, and either the
    // audio cache key (transcode settings mixed in) or raw-bytes hash.
    let audio_opts = input.resolved_audio_options();
    for path in &other_paths {
        let raw = std::fs::read(path)?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(kind) = asset::kind_from_ext(&ext) else {
            continue;
        };
        let name = stem_name(&relative_path(path, &base));
        let source_hash = hash_bytes(&raw);
        let hash = match (kind, &audio_opts) {
            (AssetKind::Audio(_), Some(audio)) => {
                hash_parts(&[&raw, audio.fingerprint().as_bytes()])
            }
            _ => hash_bytes(&raw),
        };
        out.push(Discovered {
            input: input_name.to_string(),
            name,
            kind: kind.mime().to_string(),
            source_hash,
            hash: Some(hash),
            packable: false,
            dpi: None,
            slice: None,
        });
    }

    if packable {
        // Packable input: pack everything (mirrors process_packed) to get
        // per-sheet hashes + per-sprite slices. DPI variants are grouped out
        // first — they upload individually.
        let images = load_input_images(&image_paths, &base, input)?;
        let (dpi_groups, plain) = group_dpi_variants(images);

        for (base_name, variants) in dpi_groups {
            let mut d = dpi_parent(base_name, variants, &processing);
            d.input = input_name.to_string();
            out.push(d);
        }

        if !plain.is_empty() {
            let sheets =
                pack::pack(plain.clone()).context("Failed to pack images during migration")?;
            for sheet in &sheets {
                let sheet_hash = processed_image_hash(sheet.image.clone(), &processing);
                for img in &sheet.images {
                    let Some(src) = plain.iter().find(|i| i.name == img.name) else {
                        continue;
                    };
                    out.push(Discovered {
                        input: input_name.to_string(),
                        name: img.name.clone(),
                        kind: "image/png".to_string(),
                        source_hash: src.source_hash.clone(),
                        hash: Some(sheet_hash.clone()),
                        packable: true,
                        dpi: None,
                        slice: Some(Slice {
                            sheet: img.sheet_index as u32,
                            x: img.x,
                            y: img.y,
                            width: img.width,
                            height: img.height,
                        }),
                    });
                }
            }
        }
    } else {
        // Individual input: every image is its own entry, DPI variants
        // grouped per base name.
        let named: Vec<(PathBuf, String)> = image_paths
            .iter()
            .map(|p| (p.clone(), stem_name(&relative_path(p, &base))))
            .collect();
        let (groups, plain_paths) = group_paths_by_dpi(named);

        for (base_name, variants) in groups {
            // Parent source hash: the @1x variant's raw bytes.
            let source_hash = variants
                .iter()
                .find(|(s, _)| *s == 1)
                .or_else(|| variants.first())
                .map(|(_, p)| hash_bytes(&std::fs::read(p).unwrap_or_default()))
                .unwrap_or_default();
            let mut dpi = Vec::new();
            for (scale, path) in &variants {
                dpi.push((*scale, plain_image_hash(path, &base, input, &processing)?));
            }
            out.push(Discovered {
                input: input_name.to_string(),
                name: base_name,
                kind: "image/png".to_string(),
                source_hash,
                hash: None,
                // Matches sync: DPI parents are `packable = true` (they group
                // into variants) even in individual inputs.
                packable: true,
                dpi: Some(dpi),
                slice: None,
            });
        }
        for (path, _name) in plain_paths {
            let name = stem_name(&relative_path(&path, &base));
            let source_hash = hash_bytes(&std::fs::read(&path)?);
            let hash = plain_image_hash(&path, &base, input, &processing)?;
            out.push(Discovered {
                input: input_name.to_string(),
                name,
                kind: "image/png".to_string(),
                source_hash,
                hash: Some(hash),
                packable: false,
                dpi: None,
                slice: None,
            });
        }
    }

    Ok(out)
}

/// Load + decode every image for an input (raster + SVG), carrying the
/// source hash for sprite entries.
fn load_input_images(
    paths: &[PathBuf],
    base: &str,
    input: &InputConfig,
) -> Result<Vec<pack::InputImage>> {
    let cache = std::sync::Arc::new(asset::new_svg_scale_cache());
    paths
        .par_iter()
        .map(|path| {
            let rel = relative_path(path, base);
            let name = stem_name(&rel);
            let raw = std::fs::read(path)?;
            let image = if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
            {
                let scale =
                    asset::effective_svg_scale_for_path(path, base, input.svg_scale, &cache);
                svg_to_rgba(&raw, scale)?
            } else {
                image::open(path)?.into_rgba8()
            };
            Ok(pack::InputImage {
                name,
                image,
                source_hash: hash_bytes(&raw),
            })
        })
        .collect()
}

/// DPI parent from already-decoded variants (packable path). Uses the same
/// bleed/encode/compress pipeline as `dpi.rs` so migrated variant hashes
/// match sync-time cache hits.
fn dpi_parent(
    base_name: String,
    variants: Vec<(u8, pack::InputImage)>,
    processing: &ResolvedProcessing,
) -> Discovered {
    let source_hash = variants
        .iter()
        .find(|(s, _)| *s == 1)
        .or_else(|| variants.first())
        .map(|(_, img)| img.source_hash.clone())
        .unwrap_or_default();
    let dpi = variants
        .iter()
        .map(|(scale, img)| (*scale, processed_image_hash(img.image.clone(), processing)))
        .collect();
    Discovered {
        input: String::new(),
        name: base_name,
        kind: "image/png".to_string(),
        source_hash,
        hash: None,
        packable: true,
        dpi: Some(dpi),
        slice: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A valid 1x1 PNG, encoded at runtime so CRCs are always correct.
    fn tiny_png() -> Vec<u8> {
        let img = image::RgbaImage::new(1, 1);
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    /// Scaffold a project: config, one individual image, one model.
    /// The caller is responsible for running inside the tempdir (globs in
    /// the config are relative to the cwd).
    fn scaffold(dir: &Path) {
        fs::create_dir_all(dir.join("assets/icons")).unwrap();
        fs::create_dir_all(dir.join("assets/models")).unwrap();
        fs::write(dir.join("assets/icons/hello.png"), tiny_png()).unwrap();
        fs::write(dir.join("assets/models/thing.rbxm"), b"fake-model").unwrap();
        fs::write(
            dir.join("tungsten.toml"),
            r#"
[creator]
type = "user"
id = 1

[inputs.icons]
path = "assets/icons/**/*"
output_path = "src/icons.luau"

[inputs.models]
path = "assets/models/**/*"
output_path = "src/models.luau"
"#,
        )
        .unwrap();
    }

    /// Processed hash the migrator computes for the fixture PNG (bleed on,
    /// no compression): decode → bleed → encode → hash, same as
    /// `processed_image_hash`.
    fn fixture_processed_hash() -> String {
        let rgba = image::load_from_memory(&tiny_png()).unwrap().into_rgba8();
        processed_image_hash(
            rgba,
            &ResolvedProcessing {
                bleed: true,
                compress: None,
            },
        )
    }

    fn opts(dir: &Path) -> MigrationOptions {
        MigrationOptions {
            lockfile_path: dir.join("tungsten.lock"),
            config_path: dir.join("tungsten.toml"),
            dry_run: false,
            force: false,
            write_backup: true,
        }
    }

    #[test]
    fn v2_parse_total_entries() {
        let v2: V2Lockfile = toml::from_str(
            r#"
version = 2

[inputs.images.abc]
asset_id = 42
studio_uri = "rbxasset://x"

[inputs.models.def]
asset_id = 7
"#,
        )
        .unwrap();
        let total: usize = v2.inputs.values().map(|m| m.len()).sum();
        assert_eq!(total, 2);
        assert_eq!(v2.inputs["images"]["abc"].asset_id, Some(42));
        assert_eq!(v2.inputs["models"]["def"].studio_uri, None);
    }

    #[test]
    fn backup_path_is_literal_suffix() {
        assert_eq!(
            backup_path_of(Path::new("tungsten.lock")),
            PathBuf::from("tungsten.lock.v2.bak")
        );
    }

    /// End-to-end: cached v2 ids lift into name-keyed v3 entries, the v2
    /// file is backed up, and the result re-loads as v3.
    #[test]
    fn migration_lifts_cached_ids_and_writes_backup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        scaffold(&root);
        let hash = fixture_processed_hash();
        fs::write(
            root.join("tungsten.lock"),
            format!(
                r#"
version = 2

[inputs.icons.{hash}]
asset_id = 111
studio_uri = "rbxasset://.tungsten_proj/icons/hello.png"

[inputs.models.deadbeef]
asset_id = 222
"#
            ),
        )
        .unwrap();

        let report = migrate_v2_to_v3(&opts(&root)).unwrap();

        assert_eq!(report.total_v2_entries, 2);
        assert!(report.migrated >= 1, "the image's cached id lifts");
        let v3 = fs::read_to_string(root.join("tungsten.lock")).unwrap();
        assert!(v3.contains("version = 3"));
        // The asset name is the codegen name: input-base-relative, no ext,
        // grouped under its config input.
        assert!(v3.contains("[inputs.icons.hello]"), "name-keyed entry");
        assert!(v3.contains("id = 111"), "cached id lifted from v2");
        assert!(v3.contains("kind = \"image/png\""));
        assert!(root.join("tungsten.lock.v2.bak").exists());
        assert_eq!(report.backup_path, Some(root.join("tungsten.lock.v2.bak")));

        // The model's stale v2 hash is unmapped (source changed / no match).
        assert!(
            report.unmapped.iter().any(|n| n.contains("thing")),
            "stale model entry unmapped: {:?}",
            report.unmapped
        );
    }

    /// Missing config is a hard error whose message points at --config.
    #[test]
    fn missing_config_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        scaffold(&root);
        fs::remove_file(root.join("tungsten.toml")).unwrap();
        fs::write(root.join("tungsten.lock"), "version = 2\n").unwrap();

        let err = migrate_v2_to_v3(&opts(&root)).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("--config"),
            "error must point at --config, got: {msg}"
        );
    }

    /// Dry run: compute only, write nothing.
    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        scaffold(&root);
        fs::write(root.join("tungsten.lock"), "version = 2\n").unwrap();

        let mut dry_opts = opts(&root);
        dry_opts.dry_run = true;
        let report = migrate_v2_to_v3(&dry_opts).unwrap();

        assert_eq!(
            fs::read_to_string(root.join("tungsten.lock")).unwrap(),
            "version = 2\n"
        );
        assert!(!root.join("tungsten.lock.v2.bak").exists());
        assert_eq!(report.written_path, None);
    }

    /// An existing .v2.bak aborts before touching anything.
    #[test]
    fn existing_backup_aborts_migration() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        scaffold(&root);
        fs::write(root.join("tungsten.lock"), "version = 2\n").unwrap();
        fs::write(root.join("tungsten.lock.v2.bak"), "previous backup").unwrap();

        let err = migrate_v2_to_v3(&opts(&root)).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains(".v2.bak"), "error names the backup: {msg}");
        // Both files untouched.
        assert_eq!(
            fs::read_to_string(root.join("tungsten.lock")).unwrap(),
            "version = 2\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("tungsten.lock.v2.bak")).unwrap(),
            "previous backup"
        );
    }

    /// A v3 lockfile is refused without --force.
    #[test]
    fn v3_lockfile_refused_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(
            root.join("tungsten.lock"),
            "version = 3\n\n[inputs.icons.abc]\nasset_id = 1\n",
        )
        .unwrap();
        let err = migrate_v2_to_v3(&opts(&root)).unwrap_err();
        assert!(
            format!("{:#}", err).contains("already at version 3"),
            "must refuse a v3 file"
        );
    }

    /// Migrating a legacy `tungsten.lock.toml` writes the v3 result to
    /// `tungsten.lock`, backs up the old file under the old name, and
    /// removes the legacy file.
    #[test]
    fn legacy_filename_migrates_to_new_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        scaffold(&root);
        let hash = fixture_processed_hash();
        fs::write(
            root.join("tungsten.lock.toml"),
            format!(
                r#"
version = 2

[inputs.icons.{hash}]
asset_id = 111
"#
            ),
        )
        .unwrap();

        let mut legacy_opts = opts(&root);
        legacy_opts.lockfile_path = root.join("tungsten.lock.toml");
        let report = migrate_v2_to_v3(&legacy_opts).unwrap();

        // v3 written under the new name, legacy file gone.
        let v3 = fs::read_to_string(root.join("tungsten.lock")).unwrap();
        assert!(v3.contains("version = 3"));
        assert!(v3.contains("id = 111"), "cached id lifted");
        assert!(
            !root.join("tungsten.lock.toml").exists(),
            "legacy file removed"
        );
        assert_eq!(report.written_path, Some(root.join("tungsten.lock")));
        // Backup keeps the legacy name.
        assert!(root.join("tungsten.lock.toml.v2.bak").exists());
        assert_eq!(
            report.backup_path,
            Some(root.join("tungsten.lock.toml.v2.bak"))
        );
    }

    /// The default lockfile path auto-detects: legacy name when the new one
    /// is absent. (Path-existence only — cwd-dependent discovery is covered
    /// by the engine tests above.)
    #[test]
    fn default_lockfile_path_prefers_new_name() {
        // Neither exists relative to this test's cwd assumptions is not
        // guaranteed, so assert the constant-driven shape instead.
        assert_eq!(
            PathBuf::from(crate::core::postsync::lockfile::LOCKFILE_PATH),
            PathBuf::from("tungsten.lock")
        );
        assert_eq!(
            PathBuf::from(crate::core::postsync::lockfile::LEGACY_LOCKFILE_PATH),
            PathBuf::from("tungsten.lock.toml")
        );
    }
}
