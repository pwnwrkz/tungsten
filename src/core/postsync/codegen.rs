use anyhow::{Context, Result};
use std::collections::BTreeMap;

// Types

/// The value an asset entry resolves to in generated code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetRef {
    /// A Roblox cloud asset ID — emitted as `"rbxassetid://<id>"`.
    Id(u64),
    /// A Studio content URI — emitted verbatim (e.g. `"rbxasset://.tungsten_proj/icon.png"`).
    Uri(String),
}

impl AssetRef {
    /// Returns the Luau string literal value (with surrounding quotes).
    pub fn luau_string(&self) -> String {
        match self {
            AssetRef::Id(id) => format!("\"rbxassetid://{}\"", id),
            AssetRef::Uri(uri) => format!("\"{}\"", uri),
        }
    }
}

/// Describes what a codegen entry represents, controlling what Luau is emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodegenKind {
    /// A packed spritesheet region.
    Sprite {
        rect_offset: (u32, u32),
        rect_size: (u32, u32),
    },
    /// A plain asset resolving to a single `rbxassetid://` string.
    Asset,
    /// A High-DPI group: multiple uploads of the same asset at different scales.
    /// `variants` is sorted ascending by scale (1, 2, 3, …).
    /// Emits a `function(dpiScale)` that returns the correct asset ID.
    DpiGroup {
        /// (scale_factor, asset_id) pairs, sorted ascending by scale.
        variants: Vec<(u8, u64)>,
    },
}

pub struct CodegenEntry {
    pub name: String,
    /// The asset reference used for codegen output.
    /// - `Asset` entries: the full ref (Id or Uri).
    /// - `Sprite` entries: the spritesheet ref (Id or Uri).
    /// - `DpiGroup` entries: not used directly (variants carry their own IDs).
    pub asset_ref: AssetRef,
    pub kind: CodegenKind,
}

impl CodegenEntry {
    pub fn sprite(
        name: String,
        asset_ref: AssetRef,
        rect_offset: (u32, u32),
        rect_size: (u32, u32),
    ) -> Self {
        Self {
            name,
            asset_ref,
            kind: CodegenKind::Sprite {
                rect_offset,
                rect_size,
            },
        }
    }

    /// Convenience constructor for a cloud-ID sprite (backwards compat).
    #[allow(dead_code)]
    pub fn sprite_id(
        name: String,
        asset_id: u64,
        rect_offset: (u32, u32),
        rect_size: (u32, u32),
    ) -> Self {
        Self::sprite(name, AssetRef::Id(asset_id), rect_offset, rect_size)
    }

    pub fn asset(name: String, asset_ref: AssetRef) -> Self {
        Self {
            name,
            asset_ref,
            kind: CodegenKind::Asset,
        }
    }

    /// Convenience constructor for a cloud-ID asset (backwards compat).
    pub fn asset_id(name: String, asset_id: u64) -> Self {
        Self::asset(name, AssetRef::Id(asset_id))
    }

    /// `variants` must be sorted ascending by scale factor.
    /// DPI groups always use cloud IDs per variant.
    pub fn dpi_group(name: String, variants: Vec<(u8, u64)>) -> Self {
        let asset_ref = AssetRef::Id(variants.first().map(|&(_, id)| id).unwrap_or(0));
        Self {
            name,
            asset_ref,
            kind: CodegenKind::DpiGroup { variants },
        }
    }
}

// Tree (nested style)

enum TreeNode<'a> {
    Leaf(&'a CodegenEntry),
    Branch(BTreeMap<String, TreeNode<'a>>),
}

// Public API

pub fn generate(
    entries: Vec<CodegenEntry>,
    table_name: &str,
    style: &str,
    strip_extension: bool,
    output_path: &str,
    ts_declaration: bool,
) -> Result<()> {
    let luau = generate_luau(&entries, table_name, style, strip_extension);

    if let Some(parent) = std::path::Path::new(output_path).parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory for \"{}\"", output_path))?;
    }

    std::fs::write(output_path, luau)
        .with_context(|| format!("Failed to write codegen to \"{}\"", output_path))?;

    if ts_declaration {
        let dts_path = dts_path_for(output_path);
        let dts = generate_dts(&entries, table_name, style, strip_extension);
        std::fs::write(&dts_path, dts).with_context(|| {
            format!("Failed to write TypeScript definition to \"{}\"", dts_path)
        })?;
    }

    Ok(())
}

/// `src/assets.luau` -> `src/assets.d.ts`
pub fn dts_path_for(luau_path: &str) -> String {
    let p = std::path::Path::new(luau_path);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("assets");
    let parent = p.parent().and_then(|p| p.to_str()).unwrap_or(".");
    if parent.is_empty() || parent == "." {
        format!("{}.d.ts", stem)
    } else {
        format!("{}/{}.d.ts", parent, stem)
    }
}

// DPI variant parsing

/// Extract the DPI scale from a file stem if it has an `@Nx` suffix.
/// `"hello@2x"` -> `Some(2)`, `"hello"` -> `None`.
pub fn parse_dpi_suffix(stem: &str) -> Option<u8> {
    let at = stem.rfind('@')?;
    let suffix = &stem[at + 1..];
    let scale_str = suffix
        .strip_suffix('x')
        .or_else(|| suffix.strip_suffix('X'))?;
    scale_str.parse::<u8>().ok().filter(|&s| s >= 2)
}

/// Strip the `@Nx` suffix from a stem, returning the base name.
/// `"hello@2x"` -> `"hello"`, `"hello"` -> `"hello"`.
pub fn strip_dpi_suffix(stem: &str) -> &str {
    if let Some(at) = stem.rfind('@') {
        let suffix = &stem[at + 1..];
        let is_dpi = suffix
            .strip_suffix('x')
            .or_else(|| suffix.strip_suffix('X'))
            .and_then(|s| s.parse::<u8>().ok())
            .filter(|&n| n >= 2)
            .is_some();
        if is_dpi { &stem[..at] } else { stem }
    } else {
        stem
    }
}

// Shared key/tree helpers

fn strip_ext(name: &str) -> &str {
    if let Some(dot) = name.rfind('.') {
        if dot > 0 { &name[..dot] } else { name }
    } else {
        name
    }
}

fn entry_key(entry: &CodegenEntry, strip_extension: bool) -> &str {
    if strip_extension {
        strip_ext(&entry.name)
    } else {
        &entry.name
    }
}

fn is_valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn luau_key(key: &str) -> String {
    if is_valid_identifier(key) {
        key.to_string()
    } else {
        format!("[\"{}\"]", key)
    }
}

fn ts_key(key: &str) -> String {
    if is_valid_identifier(key) {
        key.to_string()
    } else {
        format!("\"{}\"", key)
    }
}

fn build_tree<'a>(
    entries: &'a [CodegenEntry],
    strip_extension: bool,
) -> BTreeMap<String, TreeNode<'a>> {
    let mut root: BTreeMap<String, TreeNode<'a>> = BTreeMap::new();
    for entry in entries {
        let key = entry_key(entry, strip_extension);
        let parts: Vec<&str> = key.split('/').collect();
        let mut current = &mut root;
        for (i, &part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                current.insert(part.to_string(), TreeNode::Leaf(entry));
            } else {
                current = match current
                    .entry(part.to_string())
                    .or_insert_with(|| TreeNode::Branch(BTreeMap::new()))
                {
                    TreeNode::Branch(map) => map,
                    TreeNode::Leaf(_) => break,
                };
            }
        }
    }
    root
}

// Luau generation

fn generate_luau(
    entries: &[CodegenEntry],
    table_name: &str,
    style: &str,
    strip_extension: bool,
) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(3 + entries.len() * 6 + 2);
    lines.push("-- This file was automatically @generated by Tungsten.".into());
    lines.push("-- It is not intended for manual editing.".into());
    lines.push(String::new());

    match style {
        "nested" => luau_nested(entries, strip_extension, &mut lines, table_name),
        _ => luau_flat(entries, strip_extension, &mut lines, table_name),
    }

    lines.push(String::new());
    lines.push(format!("return {}", table_name));
    lines.join("\n")
}

/// Emit the Luau value lines for a DpiGroup entry at the given indent depth.
fn emit_dpi_function(variants: &[(u8, u64)], lines: &mut Vec<String>, depth: usize) {
    let ind = "\t".repeat(depth);
    let ind1 = "\t".repeat(depth + 1);
    let ind2 = "\t".repeat(depth + 2);

    lines.push(format!("{}function(dpiScale)", ind));

    // Emit highest scale first (>= N), then descending, with bare return last.
    let mut sorted = variants.to_vec();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.0)); // descending

    let highest = sorted[0].0;
    for (i, &(scale, id)) in sorted.iter().enumerate() {
        if i == 0 {
            lines.push(format!("{}if dpiScale >= {} then", ind1, scale));
            lines.push(format!("{}return \"rbxassetid://{}\"", ind2, id));
        } else if scale > 1 {
            lines.push(format!("{}elseif dpiScale >= {} then", ind1, scale));
            lines.push(format!("{}return \"rbxassetid://{}\"", ind2, id));
        }
    }

    // The 1x fallback (lowest scale, always the last sorted descending entry
    // unless it IS scale=1 already handled above).
    let fallback = sorted.iter().min_by_key(|&&(s, _)| s).copied();
    if let Some((_, id)) = fallback {
        lines.push(format!("{}else", ind1));
        lines.push(format!("{}return \"rbxassetid://{}\"", ind2, id));
    }

    lines.push(format!("{}end", ind1));
    lines.push(format!("{}end", ind));

    let _ = highest; // suppress unused warning
}

fn luau_flat(
    entries: &[CodegenEntry],
    strip_extension: bool,
    lines: &mut Vec<String>,
    table_name: &str,
) {
    lines.push(format!("local {} = {{", table_name));
    for entry in entries {
        let key = luau_key(entry_key(entry, strip_extension));
        match &entry.kind {
            CodegenKind::Sprite {
                rect_offset,
                rect_size,
            } => {
                lines.push(format!("\t{} = {{", key));
                lines.push(format!("\t\tImage = {},", entry.asset_ref.luau_string()));
                lines.push(format!(
                    "\t\tImageRectOffset = Vector2.new({}, {}),",
                    rect_offset.0, rect_offset.1
                ));
                lines.push(format!(
                    "\t\tImageRectSize = Vector2.new({}, {}),",
                    rect_size.0, rect_size.1
                ));
                lines.push("\t},".into());
            }
            CodegenKind::Asset => {
                lines.push(format!("\t{} = {},", key, entry.asset_ref.luau_string()));
            }
            CodegenKind::DpiGroup { variants } => {
                lines.push(format!("\t{} = ", key));
                emit_dpi_function(variants, lines, 1);
                // Replace the last line's closing `end` with `end,`
                if let Some(last) = lines.last_mut()
                    && last.trim() == "end"
                {
                    *last = format!("\t{}", "end,");
                }
            }
        }
    }
    lines.push("}".into());
}

fn write_luau_tree(tree: &BTreeMap<String, TreeNode>, lines: &mut Vec<String>, depth: usize) {
    let indent = "\t".repeat(depth);
    for (key, node) in tree {
        let qkey = luau_key(key);
        match node {
            TreeNode::Leaf(entry) => match &entry.kind {
                CodegenKind::Sprite {
                    rect_offset,
                    rect_size,
                } => {
                    let ind1 = "\t".repeat(depth + 1);
                    lines.push(format!("{}{} = {{", indent, qkey));
                    lines.push(format!(
                        "{}Image = {},",
                        ind1,
                        entry.asset_ref.luau_string()
                    ));
                    lines.push(format!(
                        "{}ImageRectOffset = Vector2.new({}, {}),",
                        ind1, rect_offset.0, rect_offset.1
                    ));
                    lines.push(format!(
                        "{}ImageRectSize = Vector2.new({}, {}),",
                        ind1, rect_size.0, rect_size.1
                    ));
                    lines.push(format!("{}}},", indent));
                }
                CodegenKind::Asset => {
                    lines.push(format!(
                        "{}{} = {}",
                        indent,
                        qkey,
                        entry.asset_ref.luau_string()
                    ));
                    let last = lines.last_mut().unwrap();
                    last.push(',');
                }
                CodegenKind::DpiGroup { variants } => {
                    // Emit:  key = function(dpiScale) ... end,
                    lines.push(format!("{}{} = function(dpiScale)", indent, qkey));
                    let ind1 = "\t".repeat(depth + 1);
                    let ind2 = "\t".repeat(depth + 2);

                    let mut sorted = variants.clone();
                    sorted.sort_by_key(|b| std::cmp::Reverse(b.0));

                    for (i, &(scale, id)) in sorted.iter().enumerate() {
                        if i == 0 {
                            lines.push(format!("{}if dpiScale >= {} then", ind1, scale));
                            lines.push(format!("{}return \"rbxassetid://{}\"", ind2, id));
                        } else if scale > 1 {
                            lines.push(format!("{}elseif dpiScale >= {} then", ind1, scale));
                            lines.push(format!("{}return \"rbxassetid://{}\"", ind2, id));
                        }
                    }

                    if let Some(&(_, id)) = sorted.iter().min_by_key(|&&(s, _)| s) {
                        lines.push(format!("{}else", ind1));
                        lines.push(format!("{}return \"rbxassetid://{}\"", ind2, id));
                    }

                    lines.push(format!("{}end", ind1));
                    lines.push(format!("{}}},", indent));
                }
            },
            TreeNode::Branch(subtree) => {
                lines.push(format!("{}{} = {{", indent, qkey));
                write_luau_tree(subtree, lines, depth + 1);
                lines.push(format!("{}}},", indent));
            }
        }
    }
}

fn luau_nested(
    entries: &[CodegenEntry],
    strip_extension: bool,
    lines: &mut Vec<String>,
    table_name: &str,
) {
    let tree = build_tree(entries, strip_extension);
    lines.push(format!("local {} = {{", table_name));
    write_luau_tree(&tree, lines, 1);
    lines.push("}".into());
}

// TypeScript declaration

fn generate_dts(
    entries: &[CodegenEntry],
    table_name: &str,
    style: &str,
    strip_extension: bool,
) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(3 + entries.len() * 5 + 4);
    lines.push("// This file was automatically @generated by Tungsten.".into());
    lines.push("// It is not intended for manual editing.".into());
    lines.push(String::new());

    match style {
        "nested" => dts_nested(entries, strip_extension, &mut lines, table_name),
        _ => dts_flat(entries, strip_extension, &mut lines, table_name),
    }

    lines.push(String::new());
    lines.push(format!("export = {}", table_name));
    lines.join("\n")
}

fn dts_flat(
    entries: &[CodegenEntry],
    strip_extension: bool,
    lines: &mut Vec<String>,
    table_name: &str,
) {
    lines.push(format!("declare const {}: {{", table_name));
    for entry in entries {
        let key = ts_key(entry_key(entry, strip_extension));
        match &entry.kind {
            CodegenKind::Sprite { .. } => {
                lines.push(format!("\t{}: {{", key));
                lines.push("\t\tImage: string".into());
                lines.push("\t\tImageRectOffset: Vector2".into());
                lines.push("\t\tImageRectSize: Vector2".into());
                lines.push("\t}".into());
            }
            CodegenKind::Asset => {
                lines.push(format!("\t{}: string", key));
            }
            CodegenKind::DpiGroup { .. } => {
                // DPI functions: (dpiScale: number) => string
                lines.push(format!("\t{}: (dpiScale: number) => string", key));
            }
        }
    }
    lines.push("}".into());
}

fn write_dts_tree(tree: &BTreeMap<String, TreeNode>, lines: &mut Vec<String>, depth: usize) {
    let indent = "\t".repeat(depth);
    for (key, node) in tree {
        let qkey = ts_key(key);
        match node {
            TreeNode::Leaf(entry) => match &entry.kind {
                CodegenKind::Sprite { .. } => {
                    let ind1 = "\t".repeat(depth + 1);
                    lines.push(format!("{}{}: {{", indent, qkey));
                    lines.push(format!("{}Image: string", ind1));
                    lines.push(format!("{}ImageRectOffset: Vector2", ind1));
                    lines.push(format!("{}ImageRectSize: Vector2", ind1));
                    lines.push(format!("{}}}", indent));
                }
                CodegenKind::Asset => {
                    lines.push(format!("{}{}: string", indent, qkey));
                }
                CodegenKind::DpiGroup { .. } => {
                    lines.push(format!("{}{}: (dpiScale: number) => string", indent, qkey));
                }
            },
            TreeNode::Branch(subtree) => {
                lines.push(format!("{}{}: {{", indent, qkey));
                write_dts_tree(subtree, lines, depth + 1);
                lines.push(format!("{}}}", indent));
            }
        }
    }
}

fn dts_nested(
    entries: &[CodegenEntry],
    strip_extension: bool,
    lines: &mut Vec<String>,
    table_name: &str,
) {
    let tree = build_tree(entries, strip_extension);
    lines.push(format!("declare const {}: {{", table_name));
    write_dts_tree(&tree, lines, 1);
    lines.push("}".into());
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn asset_entries() -> Vec<CodegenEntry> {
        vec![
            CodegenEntry::asset_id("click.mp3".into(), 999),
            CodegenEntry::asset_id("boom.ogg".into(), 888),
        ]
    }

    fn sprite_entries() -> Vec<CodegenEntry> {
        vec![
            CodegenEntry::sprite_id("icons/arrow-up.png".into(), 12345678, (0, 0), (48, 48)),
            CodegenEntry::sprite_id("icons/arrow-down.png".into(), 12345678, (48, 0), (48, 48)),
        ]
    }

    fn dpi_entries() -> Vec<CodegenEntry> {
        vec![
            CodegenEntry::dpi_group("hello.png".into(), vec![(1, 100), (2, 200), (3, 300)]),
            CodegenEntry::asset_id("other.png".into(), 999),
        ]
    }

    // DPI suffix parsing

    #[test]
    fn test_parse_dpi_suffix() {
        assert_eq!(parse_dpi_suffix("hello@2x"), Some(2));
        assert_eq!(parse_dpi_suffix("hello@3x"), Some(3));
        assert_eq!(parse_dpi_suffix("hello@2X"), Some(2));
        assert_eq!(parse_dpi_suffix("hello"), None);
        assert_eq!(parse_dpi_suffix("hello@1x"), None); // 1x not a variant
        assert_eq!(parse_dpi_suffix("hello@abc"), None);
    }

    #[test]
    fn test_strip_dpi_suffix() {
        assert_eq!(strip_dpi_suffix("hello@2x"), "hello");
        assert_eq!(strip_dpi_suffix("hello@3x"), "hello");
        assert_eq!(strip_dpi_suffix("hello"), "hello");
        assert_eq!(strip_dpi_suffix("arrow-up@2x"), "arrow-up");
    }

    // Luau: DPI group emission

    #[test]
    fn test_luau_dpi_group_flat() {
        generate(
            dpi_entries(),
            "Assets",
            "flat",
            true,
            "test_dpi_flat.luau",
            false,
        )
        .unwrap();
        let c = std::fs::read_to_string("test_dpi_flat.luau").unwrap();
        assert!(
            c.contains("function(dpiScale)"),
            "should emit dpiScale function"
        );
        assert!(c.contains("if dpiScale >= 3"), "should have 3x branch");
        assert!(c.contains("elseif dpiScale >= 2"), "should have 2x branch");
        assert!(c.contains("else"), "should have 1x fallback");
        assert!(c.contains("rbxassetid://300"), "3x id");
        assert!(c.contains("rbxassetid://200"), "2x id");
        assert!(c.contains("rbxassetid://100"), "1x id");
        assert!(c.contains("rbxassetid://999"), "other asset untouched");
        std::fs::remove_file("test_dpi_flat.luau").unwrap();
    }

    #[test]
    fn test_luau_dpi_group_nested() {
        let entries = vec![CodegenEntry::dpi_group(
            "ui/hello.png".into(),
            vec![(1, 10), (2, 20)],
        )];
        generate(
            entries,
            "Assets",
            "nested",
            true,
            "test_dpi_nested.luau",
            false,
        )
        .unwrap();
        let c = std::fs::read_to_string("test_dpi_nested.luau").unwrap();
        assert!(c.contains("function(dpiScale)"));
        assert!(c.contains("rbxassetid://20"));
        assert!(c.contains("rbxassetid://10"));
        std::fs::remove_file("test_dpi_nested.luau").unwrap();
    }

    // Luau: unchanged behaviour

    #[test]
    fn test_luau_flat_assets() {
        generate(
            asset_entries(),
            "Sounds",
            "flat",
            true,
            "test_sounds.luau",
            false,
        )
        .unwrap();
        let c = std::fs::read_to_string("test_sounds.luau").unwrap();
        assert!(c.contains("\"rbxassetid://999\""));
        assert!(c.contains("\"rbxassetid://888\""));
        assert!(!c.contains("ImageRectOffset"));
        std::fs::remove_file("test_sounds.luau").unwrap();
    }

    #[test]
    fn test_luau_sprites() {
        generate(
            sprite_entries(),
            "Icons",
            "nested",
            true,
            "test_sprites.luau",
            false,
        )
        .unwrap();
        let c = std::fs::read_to_string("test_sprites.luau").unwrap();
        assert!(c.contains("Image = \"rbxassetid://12345678\""));
        assert!(c.contains("ImageRectOffset"));
        std::fs::remove_file("test_sprites.luau").unwrap();
    }

    // TypeScript definition: DPI group

    #[test]
    fn test_dts_dpi_group_type() {
        generate(
            dpi_entries(),
            "Assets",
            "flat",
            true,
            "test_dts_dpi.luau",
            true,
        )
        .unwrap();
        let c = std::fs::read_to_string("test_dts_dpi.d.ts").unwrap();
        assert!(
            c.contains("(dpiScale: number) => string"),
            "DPI group should type as function"
        );
        assert!(!c.contains("rbxassetid"), "no asset IDs in .d.ts");
        std::fs::remove_file("test_dts_dpi.luau").unwrap();
        std::fs::remove_file("test_dts_dpi.d.ts").unwrap();
    }

    // dts_path_for

    #[test]
    fn test_dts_path_derivation() {
        assert_eq!(dts_path_for("src/assets.luau"), "src/assets.d.ts");
        assert_eq!(dts_path_for("assets.luau"), "assets.d.ts");
        assert_eq!(dts_path_for("src/assets"), "src/assets.d.ts");
    }

    // Key quoting

    #[test]
    fn test_luau_key_quoting() {
        assert_eq!(luau_key("arrow"), "arrow");
        assert_eq!(luau_key("arrow-up"), "[\"arrow-up\"]");
        assert_eq!(luau_key("48px"), "[\"48px\"]");
    }

    #[test]
    fn test_ts_key_quoting() {
        assert_eq!(ts_key("arrow"), "arrow");
        assert_eq!(ts_key("arrow-up"), "\"arrow-up\"");
    }
}
