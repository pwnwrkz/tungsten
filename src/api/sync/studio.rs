use anyhow::{Context, Result};
use rbx_install::RobloxStudio;
use reqwest;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
    /// Directories already ensured to exist, so we skip the `create_dir_all`
    /// syscall for subsequent assets in the same folder.
    created_dirs: Mutex<HashSet<PathBuf>>,
}

impl StudioSync {
    /// Set up the sync folder.
    /// If `studio_path_override` is Some, it is used as the base Roblox Studio path;
    /// otherwise the installation is located via `roblox_install`.
    /// If `auto_route_version` is true, the latest version is fetched from
    /// https://setup.roblox.com/versionQTStudio and appended to the path.
    /// Does not wipe previous contents to allow incremental sync and preserve
    /// assets between Studio version updates.
    pub async fn new(
        studio_path_override: Option<String>,
        auto_route_version: bool,
    ) -> Result<Self> {
        let base_path = if let Some(path) = studio_path_override {
            PathBuf::from(path)
        } else {
            let studio =
                RobloxStudio::locate().context("Could not locate Roblox Studio installation")?;
            studio.content_path().to_path_buf()
        };

        let content_path = if auto_route_version {
            let mut path = base_path;
            // Fetch the latest version from Roblox (async)
            let version = reqwest::get("https://setup.roblox.com/versionQTStudio")
                .await
                .context("Failed to fetch latest Roblox Studio version")?
                .text()
                .await
                .context("Failed to read version response")?
                .trim()
                .to_string();

            // Append "Versions/<version>" to the base path.
            // Try capital "Versions" (Windows) first, then lowercase "versions" (Linux/Wine).
            path.push("Versions");
            path.push(&version);
            if !path.exists() {
                path.pop();
                path.pop();
                path.push("versions");
                path.push(&version);
            }
            path
        } else {
            base_path
        };

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

        std::fs::create_dir_all(&sync_path).with_context(|| {
            format!(
                "Failed to create Studio sync folder \"{}\"",
                sync_path.display()
            )
        })?;

        Ok(Self {
            identifier,
            sync_path,
            created_dirs: Mutex::new(HashSet::new()),
        })
    }

    /// Copy an asset file into the Studio content folder, preserving its
    /// relative path under the sync root.
    ///
    /// Returns the `rbxasset://` URI that scripts should use to reference it.
    pub fn copy_asset(&self, relative_path: &str, data: &[u8]) -> Result<String> {
        // Normalise to forward slashes for the URI.
        let rel_normalized = relative_path.replace('\\', "/");
        // Build the target path by pushing each component, avoiding the two
        // intermediate String allocations a separator replace would need.
        let mut target_path = self.sync_path.clone();
        for part in rel_normalized.split('/') {
            target_path.push(part);
        }

        if let Some(parent) = target_path.parent() {
            let mut created = self.created_dirs.lock().unwrap();
            if !created.contains(parent) {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create directory for \"{}\"",
                        target_path.display()
                    )
                })?;
                created.insert(parent.to_path_buf());
            }
        }

        std::fs::write(&target_path, data)
            .with_context(|| format!("Failed to write asset to \"{}\"", target_path.display()))?;

        Ok(format!("rbxasset://{}/{}", self.identifier, rel_normalized))
    }

    pub fn sync_path(&self) -> &Path {
        &self.sync_path
    }
}
