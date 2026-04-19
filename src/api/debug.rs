use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use tempfile::tempdir;

/// Syncs assets into a local `.tungsten_debug/` folder mirroring the original
/// relative path structure. Useful for inspecting exactly what Tungsten would
/// upload without touching Roblox at all.
///
/// Codegen uses `rbxassetid://` from the lockfile where available, or `0`
/// for assets that have never been uploaded.
pub struct DebugSync {
    sync_path: PathBuf,
}

impl DebugSync {
    /// Create (or recreate) the `.tungsten_debug/` folder in the current directory.
    pub fn new() -> Result<Self> {
        let sync_path = Path::new(".tungsten_debug").to_path_buf();

        if sync_path.exists() {
            std::fs::remove_dir_all(&sync_path)
                .context("Failed to remove existing .tungsten_debug folder")?;
        }

        std::fs::create_dir_all(&sync_path).context("Failed to create .tungsten_debug folder")?;

        Ok(Self { sync_path })
    }

    /// Copy an asset into `.tungsten_debug/`, preserving its relative path.
    pub fn copy_asset(&self, relative_path: &str, data: &[u8]) -> Result<()> {
        let rel = relative_path.replace('\\', "/");
        let target = self
            .sync_path
            .join(Path::new(&rel.replace('/', std::path::MAIN_SEPARATOR_STR)));

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create directory for \"{}\"", target.display())
            })?;
        }

        std::fs::write(&target, data)
            .with_context(|| format!("Failed to write debug asset to \"{}\"", target.display()))?;

        Ok(())
    }

    pub fn sync_path(&self) -> &Path {
        &self.sync_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_copy_asset_creates_file() {
        let dir = tempdir().unwrap();
        let sync = DebugSync {
            sync_path: dir.path().to_path_buf(),
        };
        sync.copy_asset("icons/arrow.png", b"fake-png-data")
            .unwrap();
        let written = std::fs::read(dir.path().join("icons/arrow.png")).unwrap();
        assert_eq!(written, b"fake-png-data");
    }
}
