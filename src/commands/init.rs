use anyhow::{bail, Result};
use crate::log;

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

pub fn run() -> Result<()> {
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
        "Edit it to set your creator ID and input paths, then run \
         \"tungsten sync --target roblox --api-key YOUR_KEY\""
    );

    Ok(())
}
