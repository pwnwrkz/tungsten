use crate::log;
use anyhow::{Result, bail};

const DEFAULT_CONFIG: &str = r#"[creator]
type = "user"
id = 0

[codegen]
style = "flat"
strip_extension = true

[inputs.assets]
path = "assets/**/*.png"
output_path = "src/assets.luau"
packable = false
"#;
const GITIGNORE_ENTRY: &str = "\n\n# Tungsten API key\ntungsten_api_key.env\n";

pub fn run() -> Result<()> {
    // tungsten.toml creation
    if std::path::Path::new("tungsten.toml").exists() {
        bail!(
            "tungsten.toml already exists in this directory\n  \
             Hint: Delete it first if you want to reinitialize"
        );
    }

    std::fs::write("tungsten.toml", DEFAULT_CONFIG)
        .map_err(|e| anyhow::anyhow!("Failed to create tungsten.toml: {}", e))?;

    log!(success, "Created tungsten.toml");
    log!(
        info,
        "Edit it to set your creator ID and input paths, then run \"tungsten sync --target roblox\""
    );
    log!(
        info,
        "API key: use --api-key, a tungsten_api_key.env file, or set TUNGSTEN_GLOBAL_APIKEY"
    );

    // .gitignore update
    let gitignore = std::path::Path::new(".gitignore");
    let existing = std::fs::read_to_string(gitignore).unwrap_or_default();

    if !existing.contains("tungsten_api_key.env") {
        std::fs::write(gitignore, format!("{}{}", existing, GITIGNORE_ENTRY))
            .map_err(|e| anyhow::anyhow!("Failed to update .gitignore: {}", e))?;
        log!(success, "Added tungsten_api_key.env to .gitignore");
    }

    Ok(())
}
