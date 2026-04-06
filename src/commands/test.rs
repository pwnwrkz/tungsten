use anyhow::Result;
use std::sync::Arc;

#[allow(unused_imports)]
use crate::api::roblox::{Creator, GroupCreator, UserCreator};
use crate::api::upload::{RobloxClient, UploadParams};
use crate::commands::sync::{collect_paths, make_creator};
use crate::core::asset::{AssetKind, ImageFormat};
use crate::log;
use crate::utils::config::Config;
use crate::utils::env::resolve_api_key;

/// A minimal 1×1 transparent PNG, used as a smoke-test asset if the user
/// hasn't specified a test image. Generated with Python:
/// ```
/// import struct, zlib
/// ... (standard 67-byte minimal PNG)
/// ```
const FALLBACK_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // PNG signature
    0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, // IHDR length + type
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
    0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, // 8-bit RGBA
    0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, // IDAT length + type
    0x54, 0x08, 0xd7, 0x63, 0x60, 0x60, 0x60, 0x60, // IDAT data
    0x00, 0x00, 0x00, 0x05, 0x00, 0x01, 0xa5, 0xf6, // IDAT crc
    0x45, 0x40, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, // IEND length + type
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82, // IEND crc
];

pub async fn run(config: Config, api_key: Option<String>) -> Result<()> {
    let api_key = resolve_api_key(api_key);
    let mut warnings: u32 = 0;
    let mut passed: u32 = 0;

    log!(section, "Testing Tungsten configuration");

    // Creator
    match config.creator.creator_type.as_str() {
        "user" | "group" => {
            log!(
                success,
                "Creator: {} (ID: {})",
                config.creator.creator_type,
                config.creator.id
            );
            passed += 1;
        }
        other => {
            log!(
                error,
                "Invalid creator type \"{}\" — must be \"user\" or \"group\"",
                other
            );
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

        let paths = collect_paths(&input.path)
            .map_err(|e| anyhow::anyhow!("Invalid glob for \"{}\": {}", name, e))?;

        if paths.is_empty() {
            log!(warn, "No supported files matched \"{}\"", input.path);
            warnings += 1;
        } else {
            log!(success, "Found {} file(s) for \"{}\"", paths.len(), name);
            passed += 1;
        }
    }

    // API key
    let key = match api_key.as_deref() {
        Some("") | None => {
            log!(warn, "No API key provided — skipping upload smoke test");
            warnings += 1;
            None
        }
        Some(k) => {
            log!(success, "API key present");
            passed += 1;
            Some(k.to_string())
        }
    };

    // Upload smoke test
    if let Some(key) = key {
        log!(section, "Upload smoke test");

        let creator = match make_creator(&config) {
            Ok(c) => c,
            Err(e) => {
                log!(warn, "Could not build creator for smoke test: {}", e);
                warnings += 1;
                finalize(passed, warnings);
                return Ok(());
            }
        };

        // Use the embedded fallback PNG — small, fast, no disk I/O needed.
        let test_bytes = FALLBACK_PNG.to_vec();

        log!(info, "Uploading test asset (1×1 transparent PNG)...");

        let client = Arc::new(RobloxClient::new(key));

        match client
            .upload(UploadParams {
                file_name: "tungsten_smoke_test.png".to_string(),
                display_name: "Tungsten Smoke Test".to_string(),
                description:
                    "Temporary smoke test asset uploaded by Tungsten after running [tungsten test]"
                        .to_string(),
                data: test_bytes,
                kind: AssetKind::Image(ImageFormat::Png),
                creator,
            })
            .await
        {
            Ok(id) => {
                log!(success, "Upload succeeded! (rbxassetid://{})", id);
                log!(
                    info,
                    "Note: This asset will remain in your Roblox inventory"
                );
                passed += 1;
            }
            Err(e) => {
                log!(error, "Upload failed ({:#})", e);
                warnings += 1;
            }
        }
    }

    // Summary
    log!(section, "Summary");
    finalize(passed, warnings);

    Ok(())
}

fn finalize(passed: u32, warnings: u32) {
    if warnings == 0 {
        log!(success, "All checks passed ({})", passed);
    } else {
        log!(warn, "{} check(s) passed, {} warning(s)", passed, warnings);
    }
}
