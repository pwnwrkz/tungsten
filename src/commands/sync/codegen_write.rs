use relative_path::RelativePathBuf;
use crate::core::assets::asset::WebAsset;
use crate::core::postsync::codegen::CodegenEntry;

/// Seeds web assets (pre-existing Roblox assets mapped in config) into codegen entries.
/// This creates AssetRef::Id entries for assets that don't need uploading.
pub fn seed_web_assets(
    web_assets: &std::collections::HashMap<RelativePathBuf, WebAsset>,
    base_path: &str,
    strip_extension: bool,
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
        codegen_entries.push(CodegenEntry::asset_id(key, web_asset.id));
    }
}

pub fn write_codegen(
    codegen_entries: Vec<CodegenEntry>,
    input_name: &str,
    output_path: &str,
    codegen_style: &str,
    strip_extension: bool,
    ts_declaration: bool,
    errors: &mut u32,
) {
    use crate::core::postsync::codegen;
    if let Err(e) = codegen::generate(
        codegen_entries,
        input_name,
        codegen_style,
        strip_extension,
        output_path,
        ts_declaration,
    ) {
        crate::log!(warn, "Failed to write codegen for \"{}\": {}", input_name, e);
        *errors += 1;
    }
}
