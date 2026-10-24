//! Raw (non-image) asset processing: audio transcode/normalize and model passthrough.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use bytes::Bytes;
use rayon::prelude::*;
use relative_path::RelativePathBuf;

use crate::core::assets::asset::{
    self, AssetKind, AssetMeta, AudioFormat, WebAsset, is_animation_file, stem_name,
};
use crate::core::assets::audio::transcode::{ResolvedAudioOptions, transcode_to_ogg};
use crate::core::assets::img::compress::{CompressOptions, compress_image};
use crate::core::assets::img::convert;
use crate::core::postsync::codegen::CodegenEntry;
use crate::core::postsync::lockfile::{hash_bytes, hash_parts};
use crate::log;
use crate::utils::logger::{clear_progress_line, progress};

use super::codegen_write::{seed_web_assets, write_codegen};
use super::dispatch::{
    DispatchPhase, PendingAsset, SyncContext, collect_upload_results, dispatch_asset,
};
use super::paths::relative_path;

/// Progress label for the decode/normalize/encode pass.
const TRANSCODE_PHASE: &str = "Transcoding";

pub struct RawPending {
    pub name: String,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    /// sha256 of the raw source bytes, for lockfile cache hits.
    pub source_hash: String,
    pub hash: String,
    pub kind: AssetKind,
    /// Output file extension. Matches the source extension unless the asset
    /// was transcoded (e.g. "wav" becomes "ogg").
    pub ext: String,
    pub display_name: String,
    pub description: String,
    /// Per-asset API asset type override from `.tmeta` (None = derive from kind).
    pub asset_type_override: Option<String>,
}

/// Optionally compress `bytes` using the provided options.
/// Returns the (possibly compressed) bytes.
#[inline]
fn maybe_compress(
    bytes: Vec<u8>,
    ext: &str,
    compress_options: Option<&CompressOptions>,
) -> Vec<u8> {
    let Some(opts) = compress_options else {
        return bytes;
    };

    // Common case: format already caesium-compatible (PNG/JPG/...). Compress
    // in place — no clone, `bytes` stays owned for the fallback.
    let ext_lower = ext.to_ascii_lowercase();
    if convert::is_caesium_compatible(&ext_lower) {
        return match compress_image(&bytes, &ext_lower, opts) {
            Ok(Some(compressed)) => compressed,
            Ok(None) => bytes,
            Err(e) => {
                clear_progress_line();
                log!(warn, "Compression failed, using original: {}", e);
                bytes
            }
        };
    }

    // Rare path (BMP/TGA/...): transcode to PNG first. Clone so the original
    // survives if transcoding itself fails — returning empty bytes here would
    // upload a corrupt asset.
    match convert::normalize_for_compression(bytes.clone(), ext) {
        Ok((normalized, norm_ext)) => match compress_image(&normalized, norm_ext, opts) {
            Ok(Some(compressed)) => compressed,
            Ok(None) => normalized,
            Err(e) => {
                clear_progress_line();
                log!(warn, "Compression failed, using original: {}", e);
                normalized
            }
        },
        Err(e) => {
            clear_progress_line();
            log!(warn, "Could not normalize for compression: {}", e);
            bytes
        }
    }
}

/// A file that has been read and classified, but not yet processed.
struct PreparedFile {
    path: PathBuf,
    bytes: Vec<u8>,
    kind: AssetKind,
    src_ext: String,
    name: String,
    /// sha256 of the raw source bytes — the v3 lockfile cache key.
    source_hash: String,
    /// Audio-processed identity: source bytes + encoder settings. Used as the
    /// lockfile `hash` field for transcoded audio so unchanged files can skip
    /// the decode/encode round-trip entirely and non-deterministic encoder
    /// output can't invalidate the cache.
    audio_cache_key: Option<String>,
    /// `.tmeta` sidecar (loaded once here, reused for kind pin and display
    /// name/description later).
    meta: AssetMeta,
}

/// Read and classify a file without doing any expensive processing.
fn prepare_raw_file(
    path: &PathBuf,
    base_path: &str,
    audio_options: Option<&ResolvedAudioOptions>,
) -> Result<PreparedFile> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("Failed to read \"{}\": {}", path.display(), e))?;

    let src_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // `.tmeta` sidecar, read once: used for the animation kind pin below and
    // for display name/description later.
    let meta = AssetMeta::load_for(path).unwrap_or_default();

    let kind = match asset::kind_from_ext(&src_ext) {
        Some(k) => {
            // Check if .rbxm/.rbxmx is actually an animation: a `.tmeta`
            // `type = "animation" | "model"` pin wins (skips the DOM parse),
            // otherwise the file is parsed to auto-detect.
            if (src_ext == "rbxm" || src_ext == "rbxmx")
                && k == AssetKind::Model(asset::ModelFormat::Roblox)
            {
                match meta.pinned_model_kind() {
                    Some(pinned) => pinned,
                    None if is_animation_file(path).unwrap_or(false) => AssetKind::Animation,
                    None => k,
                }
            } else {
                k
            }
        }
        None => {
            return Err(anyhow::anyhow!("Unsupported extension \"{}\"", src_ext));
        }
    };

    let name = stem_name(&relative_path(path, base_path));

    // Audio cache key mixes source bytes with the settings fingerprint;
    // everything else hashes its final bytes.
    let source_hash = hash_bytes(&bytes);
    let audio_cache_key = match (kind, audio_options) {
        (AssetKind::Audio(_), Some(opts)) => {
            Some(hash_parts(&[&bytes, opts.fingerprint().as_bytes()]))
        }
        _ => None,
    };

    Ok(PreparedFile {
        path: path.clone(),
        source_hash,
        bytes,
        kind,
        src_ext,
        name,
        audio_cache_key,
        meta,
    })
}

/// Finish processing a prepared file: transcode, compress and hash.
fn finish_raw_file(
    prep: PreparedFile,
    compress_options: Option<&CompressOptions>,
    audio_options: Option<&ResolvedAudioOptions>,
    dry_run: bool,
) -> Result<RawPending> {
    let PreparedFile {
        path,
        bytes,
        kind,
        src_ext,
        name,
        source_hash,
        audio_cache_key,
        meta,
    } = prep;

    // Transcode audio to OGG Vorbis when the input opts in.
    let (data, kind, out_ext) = if let (AssetKind::Audio(_), Some(opts)) = (kind, audio_options) {
        if dry_run {
            // Dry runs never upload or copy the bytes — skip the expensive
            // decode/encode round-trip.
            (Vec::new(), kind, src_ext.clone())
        } else {
            let started = std::time::Instant::now();
            match transcode_to_ogg(&bytes, &src_ext, opts) {
                Ok(Some(ogg)) => {
                    log!(
                        debug,
                        "Transcoded \"{}\" in {:.2}s ({} KB -> {} KB)",
                        name,
                        started.elapsed().as_secs_f64(),
                        bytes.len() / 1024,
                        ogg.len() / 1024
                    );
                    (ogg, AssetKind::Audio(AudioFormat::Ogg), "ogg".to_string())
                }
                // Already OGG — passed through untouched.
                Ok(None) => (bytes, kind, src_ext.clone()),
                Err(e) => {
                    return Err(e.context(format!(
                        "Failed to transcode \"{}\" to OGG Vorbis",
                        path.display()
                    )));
                }
            }
        }
    } else {
        if let AssetKind::Audio(format) = kind
            && !format.roblox_supported()
        {
            clear_progress_line();
            log!(
                warn,
                "\"{}\" is a format Roblox does not accept — add an [inputs.<name>.transcode] section to convert it to OGG",
                path.display()
            );
        }
        (bytes, kind, src_ext.clone())
    };

    // Audio transcoding may have emptied the buffer (dry run); don't try to
    // image-compress audio or an empty buffer.
    let data = if data.is_empty() {
        data
    } else {
        maybe_compress(data, &out_ext, compress_options)
    };
    // Processed audio is keyed by source bytes and settings; everything else
    // by its final bytes.
    let hash = match audio_cache_key {
        Some(key) => key,
        None => hash_bytes(&data),
    };
    let display_name = meta.resolve_name(&name).to_string();
    let description = meta.resolve_description("Uploaded by Tungsten").to_string();
    let asset_type_override = meta.resolve_asset_type().map(|s| s.to_string());

    Ok(RawPending {
        name,
        path,
        bytes: data,
        source_hash,
        hash,
        kind,
        ext: out_ext,
        display_name,
        description,
        asset_type_override,
    })
}

pub async fn process_raw(
    ctx: &mut SyncContext<'_>,
    paths: Vec<PathBuf>,
    compress_options: Option<&CompressOptions>,
    audio_options: Option<&ResolvedAudioOptions>,
    web_assets: &HashMap<RelativePathBuf, WebAsset>,
) -> u32 {
    let base_path = ctx.base_path;

    // Seed web assets into codegen entries first
    let mut codegen_entries: Vec<CodegenEntry> = Vec::new();
    seed_web_assets(
        web_assets,
        base_path,
        ctx.codegen.strip_extension,
        ctx.codegen.strip_whitespace,
        &mut codegen_entries,
    );

    // Phase 1: read and classify files in parallel (no expensive work yet).
    // Paths are read here so the lockfile lookup below can short-circuit
    // before any decode/encode round-trip.
    // Owned clones so the `'static` blocking closure doesn't capture borrowed
    // params.
    let base_path_owned = base_path.to_string();
    let audio_opts = audio_options.cloned();

    let prepared_results: Vec<Result<PreparedFile>> = tokio::task::spawn_blocking(move || {
        paths
            .into_par_iter()
            .map(|path| prepare_raw_file(&path, &base_path_owned, audio_opts.as_ref()))
            .collect::<Vec<_>>()
    })
    .await
    .expect("spawn_blocking panicked");

    let mut prepared: Vec<PreparedFile> = Vec::new();
    for result in prepared_results {
        match result {
            Ok(p) => prepared.push(p),
            Err(e) => {
                clear_progress_line();
                log!(warn, "Failed to prepare file: {}", e);
                ctx.errors += 1;
            }
        }
    }

    // Phase 2: serve already-uploaded audio straight from the lockfile so
    // unchanged files skip the decode/encode round-trip entirely.
    let mut pending: Vec<RawPending> = Vec::new();
    let mut to_process: Vec<PreparedFile> = Vec::new();
    for prep in prepared {
        let cached = prep
            .audio_cache_key
            .as_ref()
            .filter(|_| ctx.target == super::Target::Cloud && !ctx.dry_run)
            .and_then(|key| {
                let hit = ctx
                    .lockfile
                    .entry(ctx.input_name, &prep.name)
                    .is_some_and(|e| e.hash.as_deref() == Some(key.as_str()) && e.id.is_some());
                hit.then(|| (key.clone(), 0u64))
            });
        match cached {
            // The lockfile hit is reported once, later, by dispatch.
            Some((key, id)) => {
                log!(
                    debug,
                    "{}: unchanged, skipping (cached asset {})",
                    prep.name,
                    id
                );
                let display_name = prep.meta.resolve_name(&prep.name).to_string();
                let description = prep
                    .meta
                    .resolve_description("Uploaded by Tungsten")
                    .to_string();
                let asset_type_override = prep.meta.resolve_asset_type().map(|s| s.to_string());
                pending.push(RawPending {
                    display_name,
                    description,
                    name: prep.name,
                    path: prep.path,
                    // Never uploaded or copied — the cached ID is reused.
                    bytes: Vec::new(),
                    source_hash: prep.source_hash,
                    hash: key,
                    kind: prep.kind,
                    ext: prep.src_ext,
                    asset_type_override,
                });
            }
            None => to_process.push(prep),
        }
    }

    // Phase 3: transcode, compress and hash the remainder in parallel.
    // Dry runs skip transcoding entirely, so there is nothing to report.
    let transcode_total = if ctx.dry_run {
        0
    } else {
        to_process
            .iter()
            .filter(|p| p.audio_cache_key.is_some())
            .count()
    };
    if transcode_total > 0 {
        match audio_options.and_then(|o| o.normalize) {
            Some(n) => log!(
                info,
                "Transcoding {} audio file(s) to OGG Vorbis, normalizing to {} LUFS...",
                transcode_total,
                n.target_lufs
            ),
            None => log!(
                info,
                "Transcoding {} audio file(s) to OGG Vorbis...",
                transcode_total
            ),
        }
    }
    let transcoded = std::sync::atomic::AtomicUsize::new(0);

    // Owned clones so the `'static` blocking closure doesn't capture the
    // function's borrowed params.
    let compress_opts = compress_options.cloned();
    let audio_opts = audio_options.cloned();
    let dry_run = ctx.dry_run;
    let finished_results: Vec<Result<RawPending>> = tokio::task::spawn_blocking(move || {
        to_process
            .into_par_iter()
            .map(|prep| {
                // Only audio carries real processing cost worth reporting;
                // other raw assets are just a file read.
                let reporting = (transcode_total > 0 && prep.audio_cache_key.is_some())
                    .then(|| prep.name.clone());

                // Show the bar as soon as work starts, so a single long
                // file isn't silent while it decodes and re-encodes.
                if let Some(name) = &reporting {
                    progress(
                        TRANSCODE_PHASE,
                        transcoded.load(std::sync::atomic::Ordering::Relaxed),
                        transcode_total,
                        name,
                    );
                }

                let result =
                    finish_raw_file(prep, compress_opts.as_ref(), audio_opts.as_ref(), dry_run);

                if let Some(name) = &reporting {
                    let done = transcoded.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    progress(TRANSCODE_PHASE, done, transcode_total, name);
                }

                result
            })
            .collect::<Vec<_>>()
    })
    .await
    .expect("spawn_blocking panicked");

    for result in finished_results {
        match result {
            Ok(p) => pending.push(p),
            Err(e) => {
                clear_progress_line();
                log!(warn, "Failed to process file: {}", e);
                ctx.errors += 1;
            }
        }
    }

    let total = pending.len();

    // Unified dispatch: dry-run, Studio/Debug copies, cache hits and cloud
    // uploads are handled identically to `individual` (see `dispatch::dispatch_asset`).
    {
        let asset_type_override = ctx.asset_type_override.clone();
        let mut phase = DispatchPhase::new(ctx, total);
        for p in pending {
            let studio_needs_upload = matches!(p.kind, AssetKind::Model(_) | AssetKind::Animation);
            // Audio assets transcoded to OGG upload a derived artifact, so the
            // cloud file name must carry the output extension even though the
            // source on disk is still e.g. `.wav`. This also applies to
            // passthrough OGG sources (extension unchanged) — harmless.
            let is_audio = matches!(p.kind, AssetKind::Audio(_));
            let stem = p
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.name.clone());
            let upload_name = is_audio.then(|| format!("{}.{}", stem, p.ext));

            let studio_rel = if studio_needs_upload {
                String::new()
            } else if p.ext.is_empty() {
                p.name.clone()
            } else {
                format!("{}.{}", p.name, p.ext)
            };
            let debug_rel = if p.ext.is_empty() {
                format!("{}.bin", p.name)
            } else {
                format!("{}.{}", p.name, p.ext)
            };

            dispatch_asset(
                PendingAsset {
                    name: p.name,
                    path: p.path,
                    bytes: Bytes::from(p.bytes),
                    source_hash: p.source_hash,
                    hash: p.hash,
                    kind: p.kind,
                    display_name: p.display_name,
                    description: p.description,
                    asset_type_override: p
                        .asset_type_override
                        .clone()
                        .or_else(|| asset_type_override.clone()),
                    upload_name,
                    studio_rel,
                    debug_rel,
                    studio_needs_upload,
                },
                &mut phase,
                &mut codegen_entries,
            );
        }
        collect_upload_results(&mut phase, &mut codegen_entries).await;
    }

    write_codegen(ctx, codegen_entries);
    ctx.errors
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `maybe_compress` must never return empty bytes when
    /// normalization fails — that would upload a corrupt/empty asset.
    #[test]
    fn maybe_compress_preserves_bytes_when_normalize_fails() {
        let bytes = vec![1u8, 2, 3, 4, 5];
        // "svg" is not caesium-compatible and cannot be transcoded by the
        // image crate, so normalize_for_compression returns Err.
        // The original bytes must survive for the error fallback.
        let out = maybe_compress(bytes.clone(), "svg", Some(&CompressOptions::default()));
        assert_eq!(
            out, bytes,
            "original bytes must be returned on normalize failure"
        );
    }

    /// Compatible formats skip normalization entirely and keep the original
    /// bytes when compression fails or produces no saving.
    #[test]
    fn maybe_compress_compatible_ext_never_empty() {
        let bytes = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let out = maybe_compress(bytes.clone(), "png", Some(&CompressOptions::default()));
        assert_eq!(out, bytes, "garbage PNG must fall back to original bytes");
    }

    /// Audio cache keys must mix source bytes with the settings fingerprint so
    /// changing encoder settings or normalization re-processes the file.
    #[test]
    fn audio_cache_key_mixes_settings_fingerprint() {
        use crate::core::assets::audio::encode::VorbisProfile;

        let bytes = b"fake-wav-bytes";
        let a = ResolvedAudioOptions {
            profile: VorbisProfile::Quality(5.0),
            normalize: None,
        };
        let b = ResolvedAudioOptions {
            profile: VorbisProfile::Bitrate(96),
            normalize: None,
        };
        let key_a = hash_parts(&[bytes, a.fingerprint().as_bytes()]);
        let key_b = hash_parts(&[bytes, b.fingerprint().as_bytes()]);
        assert_ne!(key_a, key_b, "settings must invalidate the audio cache key");
    }

    /// A `.tmeta` `type` pin decides the animation/model classification without
    /// parsing the (garbage) model DOM.
    #[test]
    fn tmeta_type_pin_skips_dom_parse() {
        use crate::core::assets::asset::{AssetKind, ModelFormat};
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("anim.rbxm");
        // Garbage bytes — a real parse would fail, so only the pin can classify.
        std::fs::write(&model, b"not a real rbxm").unwrap();
        let mut tmeta = std::fs::File::create(dir.path().join("anim.tmeta")).unwrap();
        writeln!(tmeta, "type = \"animation\"").unwrap();

        let base = dir.path().to_string_lossy().into_owned();
        let prep = prepare_raw_file(&model, &base, None).unwrap();
        assert_eq!(prep.kind, AssetKind::Animation);
        // The pin is not forwarded as a raw API type override.
        assert_eq!(prep.meta.resolve_asset_type(), None);

        // `type = "model"` pins the other way.
        let mut tmeta = std::fs::File::create(dir.path().join("anim.tmeta")).unwrap();
        writeln!(tmeta, "type = \"model\"").unwrap();
        let prep = prepare_raw_file(&model, &base, None).unwrap();
        assert_eq!(prep.kind, AssetKind::Model(ModelFormat::Roblox));
        assert_eq!(prep.meta.resolve_asset_type(), None);
    }
}
