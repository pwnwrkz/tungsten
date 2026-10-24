//! Codegen output: seeds web assets and writes the Luau/`.d.ts` modules.

use crate::core::assets::asset::WebAsset;
use crate::core::postsync::codegen::CodegenEntry;
use relative_path::RelativePathBuf;

use super::dispatch::SyncContext;

/// Seeds web assets into codegen entries.
pub fn seed_web_assets(
    web_assets: &std::collections::HashMap<RelativePathBuf, WebAsset>,
    base_path: &str,
    strip_extension: bool,
    strip_whitespace: bool,
    codegen_entries: &mut Vec<CodegenEntry>,
) {
    for (rel_path, web_asset) in web_assets {
        let name = rel_path.to_string().replace('\\', "/");
        let name = name
            .strip_prefix(base_path.trim_end_matches('/'))
            .unwrap_or(&name);
        let name = name.trim_start_matches('/');
        let key = if strip_extension {
            name.trim_end_matches('.').to_string()
        } else {
            name.to_string()
        };
        let key = if strip_whitespace {
            key.chars().filter(|c| !c.is_whitespace()).collect()
        } else {
            key
        };
        codegen_entries.push(CodegenEntry::asset_id(key, web_asset.id));
    }
}

/// Generates (and writes) the Luau module for this input from its codegen
/// entries, honoring the input's resolved `[codegen]` settings.
pub fn write_codegen(ctx: &mut SyncContext<'_>, mut codegen_entries: Vec<CodegenEntry>) {
    let strip_whitespace = ctx.codegen.strip_whitespace;
    if strip_whitespace {
        for entry in &mut codegen_entries {
            entry.name = entry.name.chars().filter(|c| !c.is_whitespace()).collect();
        }
    }

    if let Err(e) = crate::core::postsync::codegen::generate(
        codegen_entries,
        ctx.input_name,
        &ctx.codegen.style,
        ctx.codegen.strip_extension,
        ctx.output_path,
        ctx.codegen.ts_declaration,
    ) {
        crate::log!(
            warn,
            "Failed to write codegen for \"{}\": {}",
            ctx.input_name,
            e
        );
        ctx.errors += 1;
    }
}
