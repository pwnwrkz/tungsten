use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
pub struct Config {
    pub creator: CreatorConfig,
    pub codegen: Option<CodegenConfig>,
    pub inputs: HashMap<String, InputConfig>,
}

#[derive(Deserialize)]
pub struct CreatorConfig {
    #[serde(rename = "type")]
    pub creator_type: String,
    pub id: u64,
}

#[derive(Deserialize)]
pub struct CodegenConfig {
    pub style: Option<String>,
    pub strip_extension: Option<bool>,
}

#[derive(Deserialize)]
pub struct InputConfig {
    pub path: String,
    pub output_path: String,
    pub packable: Option<bool>,
}

pub fn load(path: &str) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Could not read \"{}\" — make sure it exists in your project root", path))?;

    toml::from_str(&content)
        .with_context(|| format!("Failed to parse \"{}\" — check for missing or invalid fields", path))
}
