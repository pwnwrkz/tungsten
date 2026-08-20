use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use rayon::prelude::*;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::api::sync::debug::DebugSync;
use crate::api::sync::roblox::Creator;
use crate::api::sync::studio::StudioSync;
use crate::api::upload::{RobloxClient, UploadParams};
use crate::core::assets::asset::{AssetKind, ImageFormat};
use crate::core::assets::img::alpha_bleed::alpha_bleed;
use crate::core::assets::img::compress::{CompressOptions, maybe_compress_png};
use crate::core::postsync::codegen::CodegenEntry;
use crate::core::postsync::lockfile::{Lockfile, hash_image};
use crate::log;
use crate::utils::logger::{clear_progress_line, progress};

use super::Target;
use super::dispatch::{copy_to_debug, copy_to_studio};
use super::encode::{DpiGroups, encode_png};

/// Process a set of DPI groups end-to-end: encode/bleed/compress/hash each
/// variant in parallel, then upload/copy per target, and push exactly one
/// `CodegenEntry::dpi_group(base, variants)` per base name.
///
/// Shared by `individual.rs` and `packed.rs` so both code paths emit one
/// codegen entry per base — previously cache hits emitted one entry per
/// variant, producing duplicate/nondeterministic keys.
#[allow(clippy::too_many_arguments)]
pub async fn process_dpi_groups(
    input_name: &str,
    dpi_groups: DpiGroups,
    bleed: bool,
    compress_options: Option<&CompressOptions>,
    target: Target,
    dry_run: bool,
    creator: &Creator,
    asset_type: Option<&str>,
    client: &Option<Arc<RobloxClient>>,
    studio_sync: &Option<Arc<StudioSync>>,
    debug_sync: &Option<Arc<DebugSync>>,
    lockfile: &mut Lockfile,
    studio_expected_files: &mut Option<&mut HashSet<String>>,
    max_concurrent_uploads: usize,
    codegen_entries: &mut Vec<CodegenEntry>,
) -> u32 {
    let mut errors: u32 = 0;

    if dpi_groups.is_empty() {
        return errors;
    }

    // 1. Pre-process every variant in parallel: bleed, encode, compress, hash (offloaded to blocking pool)
    let bleed_flag = bleed;
    // Owned copy so the `'static` blocking closure doesn't capture a borrowed
    // reference into the async context.
    let compress_opts = compress_options.cloned();
    let dpi_groups_vec: Vec<_> = dpi_groups
        .into_iter()
        .flat_map(|(base_name, variants)| {
            variants
                .into_iter()
                .map(move |(scale, img)| (base_name.clone(), scale, img))
        })
        .collect();

    // `(base_name, scale, bytes, hash)` per variant.
    let tasks: Vec<(String, u8, Vec<u8>, String)> = tokio::task::spawn_blocking(move || {
        dpi_groups_vec
            .into_par_iter()
            .filter_map(|(base_name, scale, img)| {
                let mut rgba = img.image;
                if bleed_flag {
                    alpha_bleed(&mut rgba);
                }
                let bytes = match encode_png(&rgba) {
                    Ok(b) => b,
                    Err(e) => {
                        clear_progress_line();
                        log!(warn, "Failed to encode {}@{}x: {}", base_name, scale, e);
                        return None;
                    }
                };
                let bytes = maybe_compress_png(bytes, compress_opts.as_ref());
                let hash = hash_image(&bytes);
                Some((base_name, scale, bytes, hash))
            })
            .collect()
    })
    .await
    .expect("spawn_blocking panicked");

    // 2. Group variants by base name so each base aggregates all scales.
    // Moving `base_name` out of the tuple avoids cloning it per variant.
    let mut by_base: HashMap<String, Vec<(u8, Vec<u8>, String)>> = HashMap::new();
    for (base_name, scale, bytes, hash) in tasks {
        by_base
            .entry(base_name)
            .or_default()
            .push((scale, bytes, hash));
    }

    let semaphore = Arc::new(Semaphore::new(max_concurrent_uploads));
    let mut upload_tasks: JoinSet<Result<(String, u8, u64, String)>> = JoinSet::new();

    // Resolved (scale, id) pairs per base: cache hits and fresh uploads land
    // in the same bucket so each base emits a single codegen entry.
    let mut resolved: HashMap<String, Vec<(u8, u64)>> = HashMap::new();
    let total_variants: usize = by_base.values().map(Vec::len).sum();
    let mut dispatched = 0usize;

    for (base_name, tasks) in by_base {
        for (scale, bytes, hash) in tasks {
            if dry_run {
                dispatched += 1;
                progress("Uploading", dispatched, total_variants, &base_name);
                resolved
                    .entry(base_name.clone())
                    .or_default()
                    .push((scale, 0));
                continue;
            }

            match target {
                Target::Cloud => {
                    if let Some(cached) = lockfile.get(input_name, &hash) {
                        dispatched += 1;
                        progress("Uploading", dispatched, total_variants, &base_name);
                        resolved
                            .entry(base_name.clone())
                            .or_default()
                            .push((scale, cached));
                        continue;
                    }
                    let Some(c) = client else {
                        resolved
                            .entry(base_name.clone())
                            .or_default()
                            .push((scale, 0));
                        continue;
                    };
                    let c_arc = Arc::clone(c);
                    let file_name = format!(
                        "{}@{}x.png",
                        base_name.rsplit('/').next().unwrap_or(&base_name),
                        scale
                    );
                    let base_clone = base_name.clone();
                    let hash_clone = hash;
                    let creator_clone = creator.clone();
                    let asset_type_override = asset_type.map(|s| s.to_string());
                    let semaphore_clone = semaphore.clone();
                    upload_tasks.spawn(async move {
                        let _permit = semaphore_clone.acquire_owned().await;
                        let id = c_arc
                            .upload(UploadParams {
                                file_name,
                                display_name: format!("{}@{}x", base_clone, scale),
                                description: "Uploaded by Tungsten".to_string(),
                                data: Bytes::from(bytes),
                                kind: AssetKind::Image(ImageFormat::Png),
                                asset_type_override,
                                creator: creator_clone,
                            })
                            .await
                            .with_context(|| {
                                format!("Failed to upload \"{}\" @{}x", base_clone, scale)
                            })?;
                        Ok((base_clone, scale, id, hash_clone))
                    });
                }
                Target::Studio => {
                    let rel = format!("{}@{}x.png", base_name, scale);
                    let uri = match copy_to_studio(studio_sync, studio_expected_files, &rel, &bytes)
                    {
                        Ok(u) => u,
                        Err(e) => {
                            clear_progress_line();
                            log!(
                                warn,
                                "Studio copy failed for \"{}\" @{}x: {}",
                                base_name,
                                scale,
                                e
                            );
                            errors += 1;
                            continue;
                        }
                    };
                    lockfile.set_uri(input_name, hash.clone(), uri);
                    dispatched += 1;
                    progress("Copying", dispatched, total_variants, &base_name);
                    let fallback_id = lockfile.get(input_name, &hash).unwrap_or(0);
                    resolved
                        .entry(base_name.clone())
                        .or_default()
                        .push((scale, fallback_id));
                }
                Target::Debug => {
                    let rel = format!("{}@{}x.png", base_name, scale);
                    if let Err(e) = copy_to_debug(debug_sync, &rel, &bytes) {
                        clear_progress_line();
                        log!(
                            warn,
                            "Debug copy failed for \"{}\" @{}x: {}",
                            base_name,
                            scale,
                            e
                        );
                        errors += 1;
                        continue;
                    }
                    dispatched += 1;
                    progress("Copying", dispatched, total_variants, &base_name);
                    let fallback_id = lockfile.get(input_name, &hash).unwrap_or(0);
                    resolved
                        .entry(base_name.clone())
                        .or_default()
                        .push((scale, fallback_id));
                }
            }
        }
    }

    // Collect Cloud DPI upload results.
    while let Some(res) = upload_tasks.join_next().await {
        match res {
            Ok(Ok((base_name, scale, id, hash))) => {
                lockfile.set(input_name, hash, id);
                resolved.entry(base_name).or_default().push((scale, id));
            }
            Ok(Err(e)) => {
                clear_progress_line();
                log!(warn, "{}", e);
                errors += 1;
            }
            Err(e) => {
                clear_progress_line();
                log!(warn, "DPI upload task panicked: {}", e);
                errors += 1;
            }
        }
    }

    // 3 (cont). One dpi_group per base name, variants sorted ascending.
    for (base_name, variants) in resolved {
        let mut variants = variants;
        variants.sort_unstable_by_key(|&(s, _)| s);
        codegen_entries.push(CodegenEntry::dpi_group(base_name, variants));
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::sync::debug::DebugSync;
    use crate::api::sync::roblox::{Creator, UserCreator};
    use crate::core::assets::img::pack;
    use crate::core::postsync::codegen::CodegenKind;
    use image::RgbaImage;
    use tempfile::tempdir;

    fn creator() -> Creator {
        Creator::User(UserCreator {
            user_id: "0".to_string(),
        })
    }

    fn encoded_hash(img: &pack::InputImage) -> String {
        hash_image(&encode_png(&img.image).unwrap())
    }

    #[test]
    fn dpi_group_debug_target_copies_and_emits_one_entry_per_base() {
        let dir = tempdir().unwrap();
        let ds = DebugSync::with_path(dir.path().to_path_buf());

        let img1 = pack::InputImage {
            name: "search".to_string(),
            image: RgbaImage::new(2, 2),
        };
        let img2 = pack::InputImage {
            name: "search@2x".to_string(),
            image: RgbaImage::new(4, 4),
        };
        let mut dpi_groups = DpiGroups::new();
        dpi_groups.insert(
            "search".to_string(),
            vec![(1, img1.clone()), (2, img2.clone())],
        );

        // Simulate a previously-uploaded 2x variant: the lockfile has a cache hit.
        let mut lockfile = Lockfile::default();
        lockfile.set("icons", encoded_hash(&img2), 64);

        let client = None;
        let studio_sync = None;
        let debug_sync = Some(Arc::new(ds));
        let mut studio_expected_files = None;
        let mut codegen_entries: Vec<CodegenEntry> = Vec::new();

        let errors = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(process_dpi_groups(
                "icons",
                dpi_groups,
                false,
                None,
                Target::Debug,
                false,
                &creator(),
                None,
                &client,
                &studio_sync,
                &debug_sync,
                &mut lockfile,
                &mut studio_expected_files,
                4,
                &mut codegen_entries,
            ));

        assert_eq!(errors, 0);

        // Both variants must be copied to the debug folder.
        assert!(dir.path().join("search@1x.png").exists());
        assert!(dir.path().join("search@2x.png").exists());

        // Exactly one codegen entry for the base, with sorted variants
        // mixing the cache-hit (2x) and fresh debug copy (1x).
        assert_eq!(codegen_entries.len(), 1);
        let entry = &codegen_entries[0];
        assert_eq!(entry.name, "search");
        match &entry.kind {
            CodegenKind::DpiGroup { variants } => {
                assert_eq!(variants, &vec![(1, 0), (2, 64)]);
            }
            other => panic!("expected DpiGroup, got {:?}", other),
        }
    }

    #[test]
    fn merged_cache_and_upload_variants_sort_ascending() {
        // Mirrors a partially-cached group: the 1x was resolved from the
        // lockfile and the 2x from a fresh upload, aggregated in one list.
        let mut variants = vec![(3, 300), (1, 100), (2, 200)];
        variants.sort_unstable_by_key(|(s, _)| *s);
        assert_eq!(variants, vec![(1, 100), (2, 200), (3, 300)]);
    }
}
