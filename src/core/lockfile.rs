use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const LOCKFILE_PATH: &str = "tungsten.lock.toml";
const LOCKFILE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Default)]
pub struct Lockfile {
    pub version: u32,
    /// input_name -> sha256_hex -> entry
    pub inputs: HashMap<String, HashMap<String, LockfileEntry>>,
}

#[derive(Serialize, Deserialize)]
pub struct LockfileEntry {
    pub asset_id: u64,
}

impl Lockfile {
    pub fn load() -> Result<Self> {
        let path = std::path::Path::new(LOCKFILE_PATH);

        if !path.exists() {
            return Ok(Self {
                version: LOCKFILE_VERSION,
                inputs: HashMap::new(),
            });
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Could not read \"{}\"", LOCKFILE_PATH))?;

        toml::from_str(&content).with_context(|| {
            "Failed to parse lockfile — it may be corrupted, try deleting it and re-running"
        })
    }

    pub fn save(&self) -> Result<()> {
        let content = toml::to_string(self).context("Failed to serialize lockfile")?;

        std::fs::write(LOCKFILE_PATH, content)
            .with_context(|| format!("Could not write \"{}\"", LOCKFILE_PATH))
    }

    #[inline]
    pub fn get(&self, input_name: &str, hash: &str) -> Option<u64> {
        self.inputs.get(input_name)?.get(hash).map(|e| e.asset_id)
    }

    #[inline]
    pub fn set(&mut self, input_name: &str, hash: String, asset_id: u64) {
        self.inputs
            .entry(input_name.to_string())
            .or_default()
            .insert(hash, LockfileEntry { asset_id });
    }
}

/// SHA-256 hex digest of raw image bytes.
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
