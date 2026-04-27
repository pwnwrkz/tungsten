use std::path::{Path, PathBuf};

use crate::core::assets::asset::{self, AssetMeta};
use anyhow::{Context, Result};
use glob::glob;

pub fn collect_paths(pattern: &str) -> Result<Vec<PathBuf>> {
    let paths = glob(pattern)
        .with_context(|| {
            format!(
                "Invalid glob pattern \"{}\"\n  Hint: Example: path = \"assets/**/*.png\"",
                pattern
            )
        })?
        .filter_map(|entry| match entry {
            Ok(p) => {
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                if asset::is_supported_ext(ext) {
                    Some(p)
                } else {
                    None
                }
            }
            Err(e) => {
                eprintln!("Warning: skipping unreadable path: {}", e);
                None
            }
        })
        .collect();
    Ok(paths)
}

pub fn glob_base(pattern: &str) -> String {
    pattern
        .split('*')
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string()
}

pub fn relative_path(path: &Path, base: &str) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn load_input_meta(base_path: &str) -> AssetMeta {
    let tmeta_path = Path::new(base_path).with_extension("tmeta");
    AssetMeta::load_for(&tmeta_path).unwrap_or_default()
}
