use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const LOCKFILE_PATH: &str = "tungsten.lock.toml";
const LOCKFILE_VERSION: u32 = 2;

#[derive(Serialize, Deserialize, Default)]
pub struct Lockfile {
    pub version: u32,
    /// input_name -> sha256_hex -> entry
    pub inputs: HashMap<String, HashMap<String, LockfileEntry>>,

    /// Whether any entry has been mutated since the last `save()`.
    /// Skipped during serialization — purely in-memory state.
    #[serde(skip)]
    dirty: bool,
}

#[derive(Serialize, Deserialize)]
pub struct LockfileEntry {
    /// Cloud asset ID (set after a successful `tungsten sync cloud` upload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<u64>,
    /// Studio content URI (set after a successful `tungsten sync studio` copy).
    /// Format: `rbxasset://.tungsten_<project>/<relative_path>`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio_uri: Option<String>,
}

#[allow(dead_code)]
impl Lockfile {
    pub fn load() -> Result<Self> {
        let path = std::path::Path::new(LOCKFILE_PATH);

        if !path.exists() {
            return Ok(Self {
                version: LOCKFILE_VERSION,
                inputs: HashMap::new(),
                dirty: false,
            });
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Could not read \"{}\"", LOCKFILE_PATH))?;

        let mut lf: Lockfile = toml::from_str(&content).with_context(
            || "Failed to parse lockfile — it may be corrupted, try deleting it and re-running",
        )?;

        // v1 → v2: asset_id was a bare u64, now Option<u64>.
        // serde(default) handles field-level coercion automatically on read,
        // so no entry migration is needed — just stamp the new version and
        // mark dirty so the upgraded format is flushed on the next save().
        if lf.version < LOCKFILE_VERSION {
            lf.version = LOCKFILE_VERSION;
            lf.dirty = true;
        } else {
            lf.dirty = false;
        }

        Ok(lf)
    }

    /// Write to disk only if entries have changed since last save.
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }

        let content = toml::to_string(self).context("Failed to serialize lockfile")?;
        std::fs::write(LOCKFILE_PATH, content)
            .with_context(|| format!("Could not write \"{}\"", LOCKFILE_PATH))?;

        self.dirty = false;
        Ok(())
    }

    /// Force a save regardless of dirty state. Useful for explicit flush points.
    pub fn force_save(&mut self) -> Result<()> {
        self.dirty = true;
        self.save()
    }

    /// Look up a cached cloud asset ID.
    #[inline]
    pub fn get(&self, input_name: &str, hash: &str) -> Option<u64> {
        self.inputs.get(input_name)?.get(hash)?.asset_id
    }

    /// Look up a cached Studio content URI.
    #[inline]
    pub fn get_uri(&self, input_name: &str, hash: &str) -> Option<&str> {
        self.inputs
            .get(input_name)?
            .get(hash)?
            .studio_uri
            .as_deref()
    }

    /// Store a cloud asset ID, preserving any existing studio_uri for the same hash.
    #[inline]
    pub fn set(&mut self, input_name: &str, hash: String, asset_id: u64) {
        let entry = self
            .inputs
            .entry(input_name.to_string())
            .or_default()
            .entry(hash)
            .or_insert_with(|| LockfileEntry {
                asset_id: None,
                studio_uri: None,
            });
        entry.asset_id = Some(asset_id);
        self.dirty = true;
    }

    /// Store a Studio content URI, preserving any existing asset_id for the same hash.
    #[inline]
    pub fn set_uri(&mut self, input_name: &str, hash: String, uri: String) {
        let entry = self
            .inputs
            .entry(input_name.to_string())
            .or_default()
            .entry(hash)
            .or_insert_with(|| LockfileEntry {
                asset_id: None,
                studio_uri: None,
            });
        entry.studio_uri = Some(uri);
        self.dirty = true;
    }

    /// Returns `true` if there are unsaved changes.
    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

/// SHA-256 hex digest of raw bytes.
/// Used as the cache key in the lockfile.
pub fn hash_image(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(data);
    digest.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
        s
    })
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dirty_flag_on_set() {
        let mut lf = Lockfile::default();
        assert!(!lf.is_dirty());
        lf.set("icons", "abc123".to_string(), 999);
        assert!(lf.is_dirty());
    }

    #[test]
    fn test_get_after_set() {
        let mut lf = Lockfile::default();
        lf.set("icons", "hashval".to_string(), 42);
        assert_eq!(lf.get("icons", "hashval"), Some(42));
        assert_eq!(lf.get("icons", "nope"), None);
        assert_eq!(lf.get("other", "hashval"), None);
    }

    #[test]
    fn test_save_no_write_when_clean() {
        // save() on an unmodified lockfile should be a no-op (no I/O).
        // We can't easily assert no I/O, but we can at least assert it returns Ok.
        let mut lf = Lockfile::default();
        assert!(lf.save().is_ok());
    }

    #[test]
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
