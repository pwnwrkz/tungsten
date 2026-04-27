use anyhow::{Context, Result};
use roblox_install::RobloxStudio;
use std::path::{Path, PathBuf};

/// A handle to the Roblox Studio content folder for this project.
/// Assets are copied into `.tungsten_{project}/` using their original
/// relative paths (not hash-named), so `rbxasset://` URIs remain stable
/// across re-syncs as long as the file path doesn't change.
pub struct StudioSync {
    /// The subfolder identifier under the Studio content path.
    /// e.g. `.tungsten-my-project`
    identifier: String,
    /// Absolute path to the subfolder we copy into.
    sync_path: PathBuf,
}

impl StudioSync {
    /// Locate the Roblox Studio installation and set up the sync folder.
    /// Wipes any previous contents so stale assets don't linger.
    pub fn new() -> Result<Self> {
        let studio =
            RobloxStudio::locate().context("Could not locate Roblox Studio installation")?;
        let content_path = studio.content_path();

        let cwd = std::env::current_dir().context("Could not get current directory")?;
        let project_name = cwd
            .file_name()
            .and_then(|s| s.to_str())
            .context("Could not determine project name from current directory")?
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-");

        let identifier = format!(".tungsten_{}", project_name);
        let sync_path = content_path.join(&identifier);

        if sync_path.exists() {
            std::fs::remove_dir_all(&sync_path).with_context(|| {
                format!(
                    "Failed to clear previous Studio sync folder \"{}\"",
                    sync_path.display()
                )
            })?;
        }

        std::fs::create_dir_all(&sync_path).with_context(|| {
            format!(
                "Failed to create Studio sync folder \"{}\"",
                sync_path.display()
            )
        })?;

        Ok(Self {
            identifier,
            sync_path,
        })
    }

    /// Copy an asset file into the Studio content folder, preserving its
    /// relative path under the sync root.
    ///
    /// Returns the `rbxasset://` URI that scripts should use to reference it.
    pub fn copy_asset(&self, relative_path: &str, data: &[u8]) -> Result<String> {
        // Normalise to forward slashes for the URI, use OS separator for the path.
        let rel_normalized = relative_path.replace('\\', "/");
        let target_path = self.sync_path.join(Path::new(
            &rel_normalized.replace('/', std::path::MAIN_SEPARATOR_STR),
        ));

        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create directory for \"{}\"",
                    target_path.display()
                )
            })?;
        }

        std::fs::write(&target_path, data)
            .with_context(|| format!("Failed to write asset to \"{}\"", target_path.display()))?;

        Ok(format!("rbxasset://{}/{}", self.identifier, rel_normalized))
    }

    /// The `rbxasset://` URI for a relative path without copying anything.
    /// Used to regenerate codegen values from already-synced assets.
    #[allow(dead_code)]
    pub fn asset_uri(&self, relative_path: &str) -> String {
        let rel_normalized = relative_path.replace('\\', "/");
        format!("rbxasset://{}/{}", self.identifier, rel_normalized)
    }

    pub fn sync_path(&self) -> &Path {
        &self.sync_path
    }

    #[allow(dead_code)]
    pub fn identifier(&self) -> &str {
        &self.identifier
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_asset_uri_format() {
        // We can test URI formatting without actually locating Studio.
        let fake = StudioSync {
            identifier: ".tungsten_my-project".to_string(),
            sync_path: PathBuf::from("/fake/content/.tungsten_my-project"),
        };

        assert_eq!(
            fake.asset_uri("icons/arrow.png"),
            "rbxasset://.tungsten_my-project/icons/arrow.png"
        );
        assert_eq!(
            fake.asset_uri("sounds/click.mp3"),
            "rbxasset://.tungsten_my-project/sounds/click.mp3"
        );
    }

    #[test]
    fn test_asset_uri_normalizes_backslashes() {
        let fake = StudioSync {
            identifier: ".tungsten_proj".to_string(),
            sync_path: PathBuf::from("/fake"),
        };
        assert_eq!(
            fake.asset_uri("icons\\arrow.png"),
            "rbxasset://.tungsten_proj/icons/arrow.png"
        );
    }
}
