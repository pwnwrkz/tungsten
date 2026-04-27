use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::task::JoinSet;

use crate::api::sync::debug::DebugSync;
use crate::api::sync::roblox::Creator;
use crate::api::sync::studio::StudioSync;
use crate::api::upload::{RobloxClient, UploadParams};
use crate::core::assets::asset::{self, AssetKind, AssetMeta};
use crate::core::assets::img::compress::CompressOptions;
use crate::core::assets::img::convert;
use crate::core::postsync::codegen::{self, CodegenEntry};
use crate::core::postsync::lockfile::{Lockfile, hash_image};
use crate::log;
use crate::utils::logger::{clear_progress_line, progress};

use super::Target;
use super::codegen_write::write_codegen;
use super::paths::relative_path;

pub struct RawPending {
    pub name: String,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub hash: String,
    pub kind: AssetKind,
    pub display_name: String,
    pub description: String,
}

/// Optionally compress `bytes` using the provided options.
/// Returns the (possibly compressed) bytes.
fn maybe_compress(
    bytes: Vec<u8>,
    ext: &str,
    compress_options: Option<&CompressOptions>,
) -> Vec<u8> {
    let Some(opts) = compress_options else {
        return bytes;
    };
    match convert::normalize_for_compression(bytes.clone(), ext) {
        Ok((normalized, norm_ext)) => {
            match crate::core::assets::img::compress::compress_image(&normalized, norm_ext, opts) {
                Ok(compressed) => compressed,
                Err(e) => {
                    clear_progress_line();
                    log!(warn, "Compression failed, using original: {}", e);
                    bytes
                }
            }
        }
        Err(e) => {
            clear_progress_line();
            log!(warn, "Could not normalize for compression: {}", e);
            bytes
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn process_raw(
    input_name: &str,
    paths: Vec<PathBuf>,
    base_path: &str,
    output_path: &str,
    codegen_style: &str,
    strip_extension: bool,
    ts_declaration: bool,
    compress_options: Option<&CompressOptions>,
    target: Target,
    dry_run: bool,
    creator: &Creator,
    client: &Option<Arc<RobloxClient>>,
    studio_sync: &Option<Arc<StudioSync>>,
    debug_sync: &Option<Arc<DebugSync>>,
    lockfile: &mut Lockfile,
) -> u32 {
    let mut errors: u32 = 0;
    let mut pending: Vec<RawPending> = Vec::with_capacity(paths.len());

    for path in &paths {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                clear_progress_line();
                log!(warn, "Failed to read \"{}\": {}", path.display(), e);
                errors += 1;
                continue;
            }
        };

        let src_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let kind = match asset::kind_from_ext(&src_ext) {
            Some(k) => k,
            None => {
                clear_progress_line();
                log!(warn, "Unsupported extension \"{}\" — skipping", src_ext);
                errors += 1;
                continue;
            }
        };

        let data = maybe_compress(data, &src_ext, compress_options);
        let hash = hash_image(&data);
        let meta = AssetMeta::load_for(path).unwrap_or_default();
        let name = {
            let rel = relative_path(path, base_path);
            Path::new(&rel)
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/")
        };
        let display_name = meta.resolve_name(&name).to_string();
        let description = meta.resolve_description("Uploaded by Tungsten").to_string();

        pending.push(RawPending {
            name,
            path: path.clone(),
            bytes: data,
            hash,
            kind,
            display_name,
            description,
        });
    }

    let total = pending.len();
    let mut codegen_entries: Vec<CodegenEntry> = Vec::with_capacity(total);
    let mut upload_tasks: JoinSet<Result<(String, u64, String)>> = JoinSet::new();
    let mut dispatched = 0usize;

    for p in pending {
        if dry_run {
            dispatched += 1;
            progress("Uploading", dispatched, total, &p.name);
            codegen_entries.push(CodegenEntry::asset_id(p.name, 0));
            continue;
        }

        match target {
            Target::Studio => {
                dispatched += 1;
                let rel = format!("{}.{}", p.name, p.kind.api_type().to_lowercase());
                let uri = if let Some(ss) = studio_sync {
                    match ss.copy_asset(&rel, &p.bytes) {
                        Ok(u) => u,
                        Err(e) => {
                            clear_progress_line();
                            log!(warn, "Studio copy failed for \"{}\": {}", p.name, e);
                            errors += 1;
                            continue;
                        }
                    }
                } else {
                    String::new()
                };
                lockfile.set_uri(input_name, p.hash.clone(), uri.clone());
                progress("Copying", dispatched, total, &p.name);
                codegen_entries.push(CodegenEntry::asset(p.name, codegen::AssetRef::Uri(uri)));
            }
            Target::Debug => {
                dispatched += 1;
                let rel = format!(
                    "{}.{}",
                    p.name,
                    p.path.extension().and_then(|e| e.to_str()).unwrap_or("bin")
                );
                if let Some(ds) = debug_sync
                    && let Err(e) = ds.copy_asset(&rel, &p.bytes)
                {
                    clear_progress_line();
                    log!(warn, "Debug copy failed for \"{}\": {}", p.name, e);
                    errors += 1;
                    continue;
                }
                let fallback_id = lockfile.get(input_name, &p.hash).unwrap_or(0);
                progress("Copying", dispatched, total, &p.name);
                codegen_entries.push(CodegenEntry::asset_id(p.name, fallback_id));
            }
            Target::Cloud => {
                if let Some(cached_id) = lockfile.get(input_name, &p.hash) {
                    dispatched += 1;
                    progress("Uploading", dispatched, total, &p.name);
                    codegen_entries.push(CodegenEntry::asset_id(p.name, cached_id));
                    continue;
                }
                let Some(c) = client else {
                    codegen_entries.push(CodegenEntry::asset_id(p.name, 0));
                    continue;
                };
                let c_arc = Arc::clone(c);
                let creator_own = creator.clone();
                let file_name = p
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let name_clone = p.name.clone();
                let hash_clone = p.hash.clone();
                upload_tasks.spawn(async move {
                    let id = c_arc
                        .upload(UploadParams {
                            file_name,
                            display_name: p.display_name,
                            description: p.description,
                            data: p.bytes,
                            kind: p.kind,
                            creator: creator_own,
                        })
                        .await
                        .with_context(|| format!("Failed to upload \"{}\"", name_clone))?;
                    Ok((name_clone, id, hash_clone))
                });
            }
        }
    }

    let mut completed = 0usize;
    while let Some(res) = upload_tasks.join_next().await {
        completed += 1;
        match res {
            Ok(Ok((name, id, hash))) => {
                lockfile.set(input_name, hash, id);
                progress("Uploading", dispatched + completed, total, &name);
                codegen_entries.push(CodegenEntry::asset_id(name.to_string(), id));
            }
            Ok(Err(e)) => {
                clear_progress_line();
                log!(warn, "{}", e);
                errors += 1;
            }
            Err(e) => {
                clear_progress_line();
                log!(warn, "Upload task panicked: {}", e);
                errors += 1;
            }
        }
    }

    write_codegen(
        codegen_entries,
        input_name,
        output_path,
        codegen_style,
        strip_extension,
        ts_declaration,
        &mut errors,
    );
    errors
}
