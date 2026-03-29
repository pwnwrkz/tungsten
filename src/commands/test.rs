use anyhow::Result;
use crate::utils::config::Config;
use crate::log;

pub async fn run(config: Config, api_key: Option<String>) -> Result<()> {
    let mut warnings: u32 = 0;

    log!(section, "Testing Tungsten configuration");

    // Creator
    match config.creator.creator_type.as_str() {
        "user" | "group" => {
            log!(
                success,
                "Creator type \"{}\" with ID {} is valid",
                config.creator.creator_type,
                config.creator.id
            );
        }
        other => {
            log!(error, "Invalid creator type \"{}\" — must be \"user\" or \"group\"", other);
            return Ok(());
        }
    }

    // Inputs
    if config.inputs.is_empty() {
        log!(error, "No inputs defined in tungsten.toml");
        return Ok(());
    }

    for (name, input) in &config.inputs {
        log!(info, "Checking input \"{}\"...", name);

        let paths: Vec<_> = glob::glob(&input.path)
            .map_err(|e| anyhow::anyhow!("Invalid glob pattern \"{}\": {}", input.path, e))?
            .filter_map(|e| e.ok())
            .filter(|p| p.extension().map(|e| e == "png").unwrap_or(false))
            .collect();

        if paths.is_empty() {
            log!(warn, "No PNG files matched \"{}\"", input.path);
            warnings += 1;
        } else {
            log!(success, "Found {} PNG file(s) for \"{}\"", paths.len(), name);
        }
    }

    // API key
    match api_key.as_deref() {
        Some("") | None => {
            log!(warn, "No API key provided — skipping API key check");
            warnings += 1;
        }
        Some(_) => {
            log!(success, "API key looks valid (not empty)");
            log!(info, "Note: A real upload test is not performed — run sync to verify fully");
        }
    }

    // Summary
    log!(section, "Done");

    if warnings == 0 {
        log!(success, "Configuration looks good!");
    } else {
        log!(warn, "Configuration is okay, but {} warning(s) were found.", warnings);
    }

    Ok(())
}
