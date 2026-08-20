use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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
    /// Directories already ensured to exist, to skip the `create_dir_all`
    /// syscall for subsequent assets in the same folder.
    created_dirs: Mutex<HashSet<PathBuf>>,
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

        Ok(Self {
            sync_path,
            created_dirs: Mutex::new(HashSet::new()),
        })
    }

    /// Copy an asset into `.tungsten_debug/`, preserving its relative path.
    pub fn copy_asset(&self, relative_path: &str, data: &[u8]) -> Result<()> {
        let rel = relative_path.replace('\\', "/");
        // Build the target path component-by-component, avoiding intermediate
        // String allocations from a separator replace.
        let mut target = self.sync_path.clone();
        for part in rel.split('/') {
            target.push(part);
        }

        if let Some(parent) = target.parent() {
            let mut created = self.created_dirs.lock().unwrap();
            if !created.contains(parent) {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create directory for \"{}\"", target.display())
                })?;
                created.insert(parent.to_path_buf());
            }
        }

        std::fs::write(&target, data)
            .with_context(|| format!("Failed to write debug asset to \"{}\"", target.display()))?;

        Ok(())
    }

    pub fn sync_path(&self) -> &Path {
        &self.sync_path
    }

    /// Construct a `DebugSync` rooted at an arbitrary path. Test-only: lets
    /// unit tests point a debug target at a temp dir instead of `.tungsten_debug/`.
    #[cfg(test)]
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            sync_path: path,
            created_dirs: Mutex::new(HashSet::new()),
        }
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
            created_dirs: Mutex::new(HashSet::new()),
        };
        sync.copy_asset("icons/arrow.png", b"fake-png-data")
            .unwrap();
        let written = std::fs::read(dir.path().join("icons/arrow.png")).unwrap();
        assert_eq!(written, b"fake-png-data");
    }
}
