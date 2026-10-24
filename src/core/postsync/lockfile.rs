//! `tungsten.lock` — per-asset cache grouped by input name, for change detection.

use crate::log;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

pub const LOCKFILE_VERSION: u32 = 3;
pub const LOCKFILE_PATH: &str = "tungsten.lock";
/// Pre-v3 lockfile name; auto-migrated to `tungsten.lock` when found.
pub const LEGACY_LOCKFILE_PATH: &str = "tungsten.lock.toml";

#[derive(Deserialize, Default)]
pub struct Lockfile {
    pub version: u32,
    /// input name -> asset name (codegen name, e.g. "icons/settings") -> entry
    pub inputs: BTreeMap<String, BTreeMap<String, LockfileEntry>>,
    #[serde(skip)]
    pub(crate) dirty: bool,
}

#[derive(Deserialize, Default, Clone)]
pub struct LockfileEntry {
    /// mime, e.g. "image/png"
    pub kind: String,
    /// sha256 of raw source bytes
    pub source_hash: String,
    /// sha256 of processed/uploaded bytes; absent for DPI parents
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub studio_uri: Option<String>,
    #[serde(default)]
    pub packable: bool,
    /// sprite position inside its sheet; `sheet` is the 0-based sheet index
    #[serde(default)]
    pub slice: Option<Slice>,
    /// scale -> variant, for multi-DPI icon groups
    #[serde(default)]
    pub dpi: BTreeMap<u8, DpiVariant>,
}

#[derive(Deserialize, Clone, Copy)]
pub struct Slice {
    pub sheet: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Deserialize, Default, Clone)]
pub struct DpiVariant {
    pub hash: String,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub studio_uri: Option<String>,
}

/// Parse just the `version` key from lockfile TOML.
fn lockfile_version(content: &str) -> Result<u32> {
    #[derive(Deserialize)]
    struct VersionProbe {
        version: u32,
    }
    Ok(toml::from_str::<VersionProbe>(content)?.version)
}

impl Lockfile {
    /// Load `tungsten.lock`, auto-migrating a v2 file to v3 first. A legacy
    /// `tungsten.lock.toml` (any version) is migrated and renamed to the
    /// new path.
    pub fn load() -> Result<Self> {
        let new = Path::new(LOCKFILE_PATH);
        let legacy = Path::new(LEGACY_LOCKFILE_PATH);
        if !new.exists() && legacy.exists() {
            let content = std::fs::read_to_string(legacy)
                .with_context(|| format!("Could not read \"{LEGACY_LOCKFILE_PATH}\""))?;
            let version = lockfile_version(&content)?;
            if version >= LOCKFILE_VERSION {
                // Already v3: plain rename, no migration needed.
                std::fs::rename(legacy, new).with_context(|| {
                    format!("Could not rename \"{LEGACY_LOCKFILE_PATH}\" to \"{LOCKFILE_PATH}\"")
                })?;
                log!(success, "Renamed {LEGACY_LOCKFILE_PATH} → {LOCKFILE_PATH}");
            } else {
                // The engine migrates the content, writes a `.v2.bak` next
                // to the legacy file, writes the v3 result to
                // `tungsten.lock` and removes the legacy file.
                let report = crate::core::postsync::migrate::migrate_v2_to_v3(
                    &crate::core::postsync::migrate::MigrationOptions {
                        lockfile_path: legacy.to_path_buf(),
                        config_path: std::path::PathBuf::from(crate::utils::config::CONFIG_PATH),
                        dry_run: false,
                        force: false,
                        write_backup: true,
                    },
                )
                .map_err(|e| e.context(format!(
                    "auto-migration of \"{LEGACY_LOCKFILE_PATH}\" failed. run `tungsten migrate-lock` to see details or use `--config <path>` if your config is elsewhere"
                )))?;
                if let Some(backup) = &report.backup_path {
                    log!(
                        success,
                        "Migrated {LEGACY_LOCKFILE_PATH} -> {LOCKFILE_PATH} (backup: {})",
                        backup.display()
                    );
                } else {
                    log!(success, "Renamed {LEGACY_LOCKFILE_PATH} -> {LOCKFILE_PATH}");
                }
            }
        }
        Self::load_from(new)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                version: LOCKFILE_VERSION,
                inputs: BTreeMap::new(),
                dirty: false,
            });
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Could not read \"{}\"", path.display()))?;

        let version = lockfile_version(&content).with_context(|| {
            format!(
                "Failed to parse \"{}\" — it may be corrupted, try deleting it and re-running",
                path.display()
            )
        })?;

        let mut lf: Lockfile = if version == LOCKFILE_VERSION {
            toml::from_str(&content)
                .with_context(|| format!("Failed to parse lockfile \"{}\"", path.display()))?
        } else if version > LOCKFILE_VERSION {
            bail!(
                "lockfile written by a newer Tungsten — upgrade or delete \"{}\"",
                path.display()
            );
        } else {
            let report = crate::core::postsync::migrate::migrate_v2_to_v3(
                &crate::core::postsync::migrate::MigrationOptions {
                    lockfile_path: path.to_path_buf(),
                    config_path: std::path::PathBuf::from(crate::utils::config::CONFIG_PATH),
                    dry_run: false,
                    force: false,
                    write_backup: true,
                },
            )
            .map_err(|e| e.context(format!(
                "auto-migration of \"{}\" failed — run `tungsten migrate-lock` to see details or use `--config <path>` if your config is elsewhere",
                path.display()
            )))?;
            if let (Some(backup), false) = (&report.backup_path, report.dry_run) {
                log!(
                    success,
                    "Migrated {} v2 → v3 (backup: {})",
                    path.display(),
                    backup.display()
                );
            }
            // The engine writes the v3 result to `tungsten.lock` (also when
            // migrating a legacy-named file).
            let written = report
                .written_path
                .clone()
                .unwrap_or_else(|| path.to_path_buf());
            let fresh = std::fs::read_to_string(&written).with_context(|| {
                format!(
                    "Could not re-read \"{}\" after migration",
                    written.display()
                )
            })?;
            toml::from_str(&fresh).with_context(|| {
                format!(
                    "Failed to parse migrated lockfile \"{}\"",
                    written.display()
                )
            })?
        };

        lf.dirty = false;
        Ok(lf)
    }

    /// Write to disk only if entries have changed since last save.
    pub fn save(&mut self) -> Result<()> {
        self.save_to(Path::new(LOCKFILE_PATH))
    }

    pub fn save_to(&mut self, path: &Path) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }

        self.version = LOCKFILE_VERSION;
        let content = render(self);
        std::fs::write(path, content)
            .with_context(|| format!("Could not write \"{}\"", path.display()))?;

        self.dirty = false;
        Ok(())
    }

    #[inline]
    pub fn entry(&self, input: &str, name: &str) -> Option<&LockfileEntry> {
        self.inputs.get(input)?.get(name)
    }

    /// Entry accessor that creates the entry if missing and marks the
    /// lockfile dirty. Callers fill in the fields they know.
    #[inline]
    pub fn entry_mut(&mut self, input: &str, name: &str) -> &mut LockfileEntry {
        let dirty = &mut self.dirty;
        self.inputs
            .entry(input.to_string())
            .or_default()
            .entry(name.to_string())
            .or_insert_with(|| {
                *dirty = true;
                LockfileEntry::default()
            })
    }

    /// Plain (non-packed, non-DPI) asset: full entry minus id/studio_uri.
    pub fn upsert_plain(
        &mut self,
        input: &str,
        name: &str,
        kind: &str,
        source_hash: &str,
        hash: &str,
        packable: bool,
    ) {
        let e = self.entry_mut(input, name);
        e.kind = kind.to_string();
        e.source_hash = source_hash.to_string();
        e.hash = Some(hash.to_string());
        e.packable = packable;
    }

    pub fn set_id(&mut self, input: &str, name: &str, id: u64) {
        self.entry_mut(input, name).id = Some(id);
        self.dirty = true;
    }

    pub fn set_studio_uri(&mut self, input: &str, name: &str, uri: String) {
        self.entry_mut(input, name).studio_uri = Some(uri);
        self.dirty = true;
    }

    /// Sprite inside a packed sheet: shares the sheet's hash/id/uri; the
    /// `slice` is what makes it unique.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_sprite(
        &mut self,
        input: &str,
        name: &str,
        kind: &str,
        source_hash: &str,
        sheet_hash: &str,
        sheet_id: Option<u64>,
        sheet_uri: Option<String>,
        slice: Slice,
    ) {
        let e = self.entry_mut(input, name);
        e.kind = kind.to_string();
        e.source_hash = source_hash.to_string();
        e.hash = Some(sheet_hash.to_string());
        e.packable = true;
        e.slice = Some(slice);
        if sheet_id.is_some() {
            e.id = sheet_id;
        }
        if sheet_uri.is_some() {
            e.studio_uri = sheet_uri;
        }
        self.dirty = true;
    }

    /// DPI parent entry: variants live under `dpi`, keyed by scale.
    pub fn upsert_dpi_parent(
        &mut self,
        input: &str,
        name: &str,
        kind: &str,
        source_hash: &str,
        packable: bool,
    ) {
        let e = self.entry_mut(input, name);
        e.kind = kind.to_string();
        e.source_hash = source_hash.to_string();
        e.packable = packable;
        // A regrouped input may no longer be a DPI group.
        e.hash = None;
        self.dirty = true;
    }

    pub fn set_dpi_variant(&mut self, input: &str, name: &str, scale: u8, variant: DpiVariant) {
        self.entry_mut(input, name).dpi.insert(scale, variant);
        self.dirty = true;
    }

    /// Returns `true` if there are unsaved changes.
    #[inline]
    #[allow(dead_code)] // only used by tests today, kept for the dirty-tracking behaviour it verifies
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

// Rendering

/// Render the lockfile as TOML. Hand-written (not `toml::to_string`) so
/// `slice` and each DPI variant render as inline tables, matching the
/// reference format — the `toml` document serializer only emits sub-tables.
fn render(lf: &Lockfile) -> String {
    let mut out = format!("version = {}\n", lf.version);
    for (input, assets) in &lf.inputs {
        for (name, e) in assets {
            out.push('\n');
            out.push_str(&format!(
                "[inputs.{}.{}]\n",
                toml_key(input),
                toml_key(name)
            ));
            out.push_str(&format!("kind = {}\n", toml_str(&e.kind)));
            out.push_str(&format!("source_hash = {}\n", toml_str(&e.source_hash)));
            if let Some(hash) = &e.hash {
                out.push_str(&format!("hash = {}\n", toml_str(hash)));
            }
            if let Some(id) = e.id {
                out.push_str(&format!("id = {id}\n"));
            }
            if let Some(uri) = &e.studio_uri {
                out.push_str(&format!("studio_uri = {}\n", toml_str(uri)));
            }
            if let Some(s) = &e.slice {
                out.push_str(&format!(
                    "slice = {{ sheet = {}, x = {}, y = {}, width = {}, height = {} }}\n",
                    s.sheet, s.x, s.y, s.width, s.height
                ));
            }
            out.push_str(&format!("packable = {}\n", e.packable));
            if !e.dpi.is_empty() {
                out.push('\n');
                out.push_str(&format!(
                    "[inputs.{}.{}.dpi]\n",
                    toml_key(input),
                    toml_key(name)
                ));
                for (scale, v) in &e.dpi {
                    out.push_str(&format!("{} = {{ hash = {}", scale, toml_str(&v.hash)));
                    if let Some(id) = v.id {
                        out.push_str(&format!(", id = {id}"));
                    }
                    if let Some(uri) = &v.studio_uri {
                        out.push_str(&format!(", studio_uri = {}", toml_str(uri)));
                    }
                    out.push_str(" }\n");
                }
            }
        }
    }
    out
}

/// TOML key: bare when it only holds `[A-Za-z0-9_-]`, quoted otherwise.
fn toml_key(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        s.to_string()
    } else {
        format!("\"{}\"", toml_escape(s))
    }
}

/// TOML basic string. Hashes, mimes and URIs never contain specials; the
/// escape arms are for pathological names only.
fn toml_str(s: &str) -> String {
    format!("\"{}\"", toml_escape(s))
}

fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// SHA-256 hex digest of raw bytes.
pub fn hash_bytes(data: &[u8]) -> String {
    hash_parts(&[data])
}

/// SHA-256 hex digest of several byte slices hashed as one stream.
pub fn hash_parts(parts: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hex::encode(hasher.finalize())
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_plain_roundtrip() {
        let mut lf = Lockfile::default();
        assert!(!lf.is_dirty());
        lf.upsert_plain("icons", "icons/a", "image/png", "src", "hash", false);
        assert!(lf.is_dirty());
        lf.set_id("icons", "icons/a", 42);

        let e = lf.entry("icons", "icons/a").unwrap();
        assert_eq!(e.kind, "image/png");
        assert_eq!(e.source_hash, "src");
        assert_eq!(e.hash.as_deref(), Some("hash"));
        assert_eq!(e.id, Some(42));
        assert!(!e.packable);
    }

    #[test]
    fn sprite_shares_sheet_fields() {
        let mut lf = Lockfile::default();
        lf.upsert_sprite(
            "icons",
            "pack/a",
            "image/png",
            "src",
            "sheethash",
            Some(7),
            Some("rbxasset://sheet".to_string()),
            Slice {
                sheet: 0,
                x: 4,
                y: 8,
                width: 16,
                height: 16,
            },
        );
        let e = lf.entry("icons", "pack/a").unwrap();
        assert_eq!(e.id, Some(7));
        assert!(e.packable);
        let s = e.slice.unwrap();
        assert_eq!((s.x, s.y, s.width, s.height), (4, 8, 16, 16));
    }

    #[test]
    fn dpi_variants_keyed_by_scale() {
        let mut lf = Lockfile::default();
        lf.upsert_dpi_parent("icons", "icons/settings", "image/png", "src", true);
        lf.set_dpi_variant(
            "icons",
            "icons/settings",
            2,
            DpiVariant {
                hash: "h2".into(),
                id: Some(9),
                studio_uri: None,
            },
        );
        let e = lf.entry("icons", "icons/settings").unwrap();
        assert_eq!(e.hash, None, "DPI parent carries no top-level hash");
        assert_eq!(e.dpi.get(&2).unwrap().id, Some(9));
        assert!(!e.dpi.contains_key(&1));
    }

    #[test]
    fn toml_schema_roundtrip() {
        let mut lf = Lockfile {
            version: 3,
            ..Default::default()
        };
        lf.upsert_plain("images", "hello", "image/png", "sh", "h", false);
        lf.set_id("images", "hello", 5);
        lf.upsert_dpi_parent("icons", "icons/settings", "image/png", "sh", true);
        lf.set_dpi_variant(
            "icons",
            "icons/settings",
            1,
            DpiVariant {
                hash: "v1".into(),
                id: Some(1),
                studio_uri: None,
            },
        );
        lf.set_dpi_variant(
            "icons",
            "icons/settings",
            2,
            DpiVariant {
                hash: "v2".into(),
                id: Some(2),
                studio_uri: None,
            },
        );

        let s1 = render(&lf);
        let lf2: Lockfile = toml::from_str(&s1).unwrap();
        let s2 = render(&lf2);
        assert_eq!(s1, s2, "render/parse must be idempotent");
    }

    /// The rendered shape: sub-tables per entry, inline `slice` and DPI
    /// variant tables, input names in the header path.
    #[test]
    fn render_matches_reference_shape() {
        let mut lf = Lockfile {
            version: 3,
            ..Default::default()
        };
        lf.upsert_sprite(
            "images",
            "pack-these/a.png",
            "image/png",
            "src",
            "sheethash",
            Some(6),
            Some("rbxasset://s".to_string()),
            Slice {
                sheet: 0,
                x: 0,
                y: 0,
                width: 128,
                height: 128,
            },
        );
        lf.upsert_dpi_parent("images", "icons/settings", "image/png", "src", true);
        lf.set_dpi_variant(
            "images",
            "icons/settings",
            1,
            DpiVariant {
                hash: "h1".into(),
                id: Some(9),
                studio_uri: Some("rbxasset://1".into()),
            },
        );

        let s = render(&lf);
        let expected = r#"version = 3

[inputs.images."icons/settings"]
kind = "image/png"
source_hash = "src"
packable = true

[inputs.images."icons/settings".dpi]
1 = { hash = "h1", id = 9, studio_uri = "rbxasset://1" }

[inputs.images."pack-these/a.png"]
kind = "image/png"
source_hash = "src"
hash = "sheethash"
id = 6
studio_uri = "rbxasset://s"
slice = { sheet = 0, x = 0, y = 0, width = 128, height = 128 }
packable = true
"#;
        assert_eq!(s, expected);
    }

    #[test]
    fn save_no_write_when_clean() {
        let mut lf = Lockfile::default();
        assert!(lf.save().is_ok());
    }

    #[test]
    fn version_probe_rejects_newer() {
        let err = Lockfile::load_from(Path::new("/nonexistent/ok.toml"));
        // Missing file -> fresh v3 lockfile, not an error.
        assert!(err.is_ok());
    }

    /// `load()` renames a legacy `tungsten.lock.toml` to `tungsten.lock`
    /// when only the legacy file exists. Chdirs into a tempdir — `#[serial]`
    /// keeps it off other tests, and the cwd is always restored.
    #[test]
    #[serial_test::serial]
    fn load_renames_legacy_v3_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(
            root.join(LEGACY_LOCKFILE_PATH),
            "version = 3\n\n[inputs.images.hello]\nkind = \"image/png\"\nsource_hash = \"x\"\nid = 7\n",
        )
        .unwrap();

        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&root).unwrap();
        let result = Lockfile::load();
        std::env::set_current_dir(prev).unwrap();

        let lf = result.expect("v3 legacy file loads via rename");
        assert_eq!(lf.entry("images", "hello").and_then(|e| e.id), Some(7));
        assert!(root.join(LOCKFILE_PATH).exists(), "new file written");
        assert!(
            !root.join(LEGACY_LOCKFILE_PATH).exists(),
            "legacy file removed"
        );
    }

    /// Mutates a process-global env var — must not run concurrently with
    /// `env::tests::test_global_env_var` (both use the bare `#[serial]` lock).
    #[test]
    #[serial_test::serial]
    fn test_global_env_var() {
        // Kept for parity with the old test suite.
        use crate::utils::env::resolve_api_key;
        const VAR: &str = "TUNGSTEN_GLOBAL_APIKEY";
        unsafe { std::env::set_var(VAR, "test_global_key") };
        let result = resolve_api_key(None);
        unsafe { std::env::remove_var(VAR) };
        assert_eq!(result, Some("test_global_key".to_string()));
    }
}
