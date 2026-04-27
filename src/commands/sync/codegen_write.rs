use std::path::Path;

use crate::core::postsync::codegen::{self, CodegenEntry};
use crate::log;

pub fn write_codegen(
    entries: Vec<CodegenEntry>,
    input_name: &str,
    output_path: &str,
    style: &str,
    strip_extension: bool,
    ts_declaration: bool,
    errors: &mut u32,
) {
    let table_name = match Path::new(output_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
    {
        Some(n) => n,
        None => {
            log!(warn, "Invalid output path \"{}\"", output_path);
            *errors += 1;
            return;
        }
    };

    log!(info, "Writing codegen → \"{}\"", output_path);
    match codegen::generate(
        entries,
        &table_name,
        style,
        strip_extension,
        output_path,
        ts_declaration,
    ) {
        Ok(()) => {}
        Err(e) => {
            log!(
                warn,
                "Failed to write codegen for \"{}\": {}",
                input_name,
                e
            );
            *errors += 1;
        }
    }
}
